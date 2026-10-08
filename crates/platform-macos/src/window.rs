//! Windows: creation, the handle the handler drives them through, the window delegate, and
//! wakers for other threads.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use objc2::rc::{Retained, Weak};
use objc2::runtime::{NSObjectProtocol, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSBackingStoreType, NSWindow, NSWindowDelegate, NSWindowStyleMask};
use objc2_foundation::{NSNotification, NSObject, NSPoint, NSRect, NSSize, NSString};
use raw_window_handle::{
    AppKitDisplayHandle, AppKitWindowHandle, DisplayHandle, HandleError, HasDisplayHandle,
    HasWindowHandle, RawDisplayHandle, RawWindowHandle, WindowHandle,
};

use crate::view::AukView;
use crate::{Appearance, Event, OptionAsAlt, Size, WindowAttributes, WindowHandler};

/// Identifies a window for the life of the process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WindowId(pub u64);

/// A window. Main thread only; cheap to clone.
#[derive(Clone)]
pub struct Window {
    view: Retained<AukView>,
}

impl std::fmt::Debug for Window {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Window").field("id", &self.id()).finish()
    }
}

impl Window {
    pub(crate) fn create(
        mtm: MainThreadMarker,
        attrs: &WindowAttributes,
        handler: Box<dyn WindowHandler>,
    ) -> Window {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = WindowId(NEXT.fetch_add(1, Ordering::Relaxed));
        let rect = NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(attrs.width, attrs.height),
        );
        let style = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Miniaturizable
            | NSWindowStyleMask::Resizable;
        // SAFETY: NSWindow's designated initializer with valid arguments.
        let ns_window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                rect,
                style,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // SAFETY: the window is owned by this crate's handles, not released on close.
        unsafe { ns_window.setReleasedWhenClosed(false) };
        ns_window.setTitle(&NSString::from_str(&attrs.title));
        ns_window.setAcceptsMouseMovedEvents(true);

        let view = AukView::new(mtm, rect, id, handler, attrs.option_as_alt);
        let delegate = WindowDelegate::new(mtm, &view);
        ns_window.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        *view.ivars().delegate.borrow_mut() = Some(delegate);
        ns_window.setContentView(Some(&view));
        ns_window.makeFirstResponder(Some(&view));
        registry::insert(id, &view);
        ns_window.center();
        ns_window.makeKeyAndOrderFront(None);
        let window = Window { view };
        // The handler's first events: its size and look.
        window.view.emit(Event::Resized(window.view.size()));
        window
            .view
            .emit(Event::Appearance(window.view.appearance()));
        window
    }

    pub(crate) fn from_view(view: Retained<AukView>) -> Window {
        Window { view }
    }

    pub fn id(&self) -> WindowId {
        self.view.ivars().id
    }

    fn ns_window(&self) -> Option<Retained<NSWindow>> {
        self.view.window()
    }

    /// The content area's size in points, and the display scale.
    pub fn size(&self) -> Size {
        self.view.size()
    }

    pub fn appearance(&self) -> Appearance {
        self.view.appearance()
    }

    pub fn is_focused(&self) -> bool {
        self.ns_window().is_some_and(|w| w.isKeyWindow())
    }

    /// Asks for an [`Event::Frame`] on the display's next refresh. Frames stop when a `Frame`
    /// event passes without a request.
    pub fn request_frame(&self) {
        self.view.request_frame();
    }

    pub fn set_title(&self, title: &str) {
        if let Some(w) = self.ns_window() {
            w.setTitle(&NSString::from_str(title));
        }
    }

    pub fn set_option_as_alt(&self, setting: OptionAsAlt) {
        self.view.ivars().option_as_alt.set(setting);
    }

    /// Where input methods put their candidate window: the text cursor's cell, in points from
    /// the view's top-left corner.
    pub fn set_ime_cursor_area(&self, x: f64, y: f64, width: f64, height: f64) {
        self.view
            .ivars()
            .ime_rect
            .set(NSRect::new(NSPoint::new(x, y), NSSize::new(width, height)));
    }

    /// Closes the window. The handler gets [`Event::Closed`] and is then dropped.
    pub fn close(&self) {
        match self.ns_window() {
            Some(w) => w.close(),
            None => self.view.closed(),
        }
    }

    /// A handle other threads use to wake this window ([`Event::Wake`]).
    pub fn waker(&self) -> Waker {
        self.view.ivars().waker.clone()
    }

    /// The `CAMetalLayer` backing the content view, for a wgpu surface. It lives as long as the
    /// window.
    pub fn metal_layer(&self) -> NonNull<c_void> {
        let layer: *const objc2_quartz_core::CAMetalLayer = &*self.view.ivars().layer;
        NonNull::new(layer as *mut c_void).expect("a reference is never null")
    }

    /// Delivers an `NSEvent` to the window as if it came from the system: keys go to the view
    /// through the whole input path (`keyDown:`, the input method, `NSTextInputClient`).
    pub fn send_event(&self, event: &objc2_app_kit::NSEvent) {
        if let Some(w) = self.ns_window() {
            w.sendEvent(event);
        }
    }
}

impl HasWindowHandle for Window {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let view: *const AukView = &*self.view;
        let handle = AppKitWindowHandle::new(NonNull::new(view as *mut c_void).expect("non-null"));
        // SAFETY: the view lives as long as this handle borrows the window.
        Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::AppKit(handle)) })
    }
}

impl HasDisplayHandle for Window {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        // SAFETY: AppKit's display handle carries no data.
        Ok(unsafe {
            DisplayHandle::borrow_raw(RawDisplayHandle::AppKit(AppKitDisplayHandle::new()))
        })
    }
}

/// Wakes a window from any thread: the window's handler gets [`Event::Wake`] on the main
/// thread. Wakes before the handler has run coalesce into one.
#[derive(Clone, Debug)]
pub struct Waker {
    id: WindowId,
    pending: Arc<AtomicBool>,
}

impl Waker {
    pub(crate) fn new(id: WindowId) -> Waker {
        Waker {
            id,
            pending: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn wake(&self) {
        if self.pending.swap(true, Ordering::AcqRel) {
            return;
        }
        let (id, pending) = (self.id, Arc::clone(&self.pending));
        dispatch2::DispatchQueue::main().exec_async(move || {
            pending.store(false, Ordering::Release);
            if let Some(view) = registry::get(id) {
                view.emit(Event::Wake);
            }
        });
    }
}

/// Live windows by id, for wakers. Main thread only.
pub(crate) mod registry {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use objc2::rc::{Retained, Weak};

    use super::WindowId;
    use crate::view::AukView;

    thread_local! {
        static VIEWS: RefCell<HashMap<WindowId, Weak<AukView>>> = RefCell::new(HashMap::new());
    }

    pub fn insert(id: WindowId, view: &AukView) {
        VIEWS.with(|v| v.borrow_mut().insert(id, Weak::from(view)));
    }

    pub fn remove(id: WindowId) {
        VIEWS.with(|v| v.borrow_mut().remove(&id));
    }

    pub fn get(id: WindowId) -> Option<Retained<AukView>> {
        VIEWS.with(|v| v.borrow().get(&id).and_then(Weak::load))
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "AukWindowDelegate"]
    #[ivars = Weak<AukView>]
    pub(crate) struct WindowDelegate;

    unsafe impl NSObjectProtocol for WindowDelegate {}

    unsafe impl NSWindowDelegate for WindowDelegate {
        #[unsafe(method(windowShouldClose:))]
        fn window_should_close(&self, _sender: &NSWindow) -> bool {
            if let Some(view) = self.ivars().load() {
                view.emit(Event::CloseRequested);
            }
            false
        }

        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            if let Some(view) = self.ivars().load() {
                view.closed();
            }
        }

        #[unsafe(method(windowDidBecomeKey:))]
        fn window_did_become_key(&self, _notification: &NSNotification) {
            if let Some(view) = self.ivars().load() {
                view.emit(Event::Focus(true));
            }
        }

        #[unsafe(method(windowDidResignKey:))]
        fn window_did_resign_key(&self, _notification: &NSNotification) {
            if let Some(view) = self.ivars().load() {
                view.emit(Event::Focus(false));
            }
        }
    }
);

impl WindowDelegate {
    fn new(mtm: MainThreadMarker, view: &AukView) -> Retained<WindowDelegate> {
        let this = Self::alloc(mtm).set_ivars(Weak::from(view));
        // SAFETY: NSObject's initializer.
        unsafe { msg_send![super(this), init] }
    }
}
