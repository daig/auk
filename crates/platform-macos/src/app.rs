//! The application: `NSApplication` setup, its delegate and the main menu.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSMenuItem,
};
use objc2_foundation::{NSNotification, NSObject};

use crate::{Clipboard, Menu, Window, WindowAttributes, WindowHandler, menu};

/// What the app's startup closure and later main-thread code create windows with.
#[derive(Clone, Copy, Debug)]
pub struct AppContext {
    mtm: MainThreadMarker,
}

impl AppContext {
    /// The context, on the main thread (`None` elsewhere).
    pub fn current() -> Option<AppContext> {
        MainThreadMarker::new().map(|mtm| AppContext { mtm })
    }

    pub fn create_window(
        &self,
        attrs: WindowAttributes,
        handler: Box<dyn WindowHandler>,
    ) -> Window {
        Window::create(self.mtm, &attrs, handler)
    }

    pub fn clipboard(&self) -> Clipboard {
        Clipboard::new(self.mtm)
    }

    /// Replaces the menu bar.
    pub fn set_main_menu(&self, description: &Menu) {
        menu::install(self.mtm, description);
    }

    /// Receives menu commands ([`crate::MenuItem::Command`]) that no window took: those chosen
    /// while no window is key.
    pub fn set_command_handler(&self, handler: impl FnMut(&AppContext, u32) + 'static) {
        menu::set_app_handler(Box::new(handler));
    }

    /// The menu bar's menus and their item titles.
    pub fn menu_titles(&self) -> Vec<(String, Vec<String>)> {
        menu::dump(self.mtm)
    }

    /// A menu item's key equivalent and modifier mask, by its path.
    pub fn menu_key(&self, path: &[&str]) -> Option<(String, usize)> {
        menu::key_equivalent(self.mtm, path)
    }

    /// Chooses a menu item as a click would: `path` is a menu's title, then item titles.
    /// Returns false when no enabled item has that path.
    pub fn perform_menu(&self, path: &[&str]) -> bool {
        menu::perform(self.mtm, path)
    }

    /// Whether the app is the active (frontmost) app.
    pub fn is_active(&self) -> bool {
        NSApplication::sharedApplication(self.mtm).isActive()
    }

    /// Brings the app forward.
    pub fn activate(&self) {
        #[allow(deprecated)]
        NSApplication::sharedApplication(self.mtm).activateIgnoringOtherApps(true);
    }

    /// Quits the app.
    pub fn quit(&self) {
        NSApplication::sharedApplication(self.mtm).terminate(None);
    }
}

/// Runs `f` on the main thread, from the event loop, soon. Callable from any thread.
/// The system's Reduce Motion setting (System Settings › Accessibility › Display).
pub fn system_reduce_motion() -> bool {
    objc2_app_kit::NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
}

pub fn run_on_main(f: impl FnOnce() + Send + 'static) {
    dispatch2::DispatchQueue::main().exec_async(f);
}

type Init = Box<dyn FnOnce(&AppContext)>;

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "AukAppDelegate"]
    #[ivars = RefCell<Option<Init>>]
    struct AppDelegate;

    unsafe impl NSObjectProtocol for AppDelegate {}

    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn application_did_finish_launching(&self, _notification: &NSNotification) {
            let mtm = self.mtm();
            // `activate` is cooperative and is refused for an app launched from a terminal; the
            // older call still brings the app forward there.
            #[allow(deprecated)]
            NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
            if let Some(init) = self.ivars().borrow_mut().take() {
                init(&AppContext { mtm });
            }
        }

        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn should_terminate_after_last_window_closed(&self, _app: &NSApplication) -> bool {
            true
        }
    }

    impl AppDelegate {
        /// A menu command no window took.
        #[unsafe(method(aukCommand:))]
        fn auk_command(&self, sender: Option<&AnyObject>) {
            let tag = sender
                .and_then(|s| s.downcast_ref::<NSMenuItem>())
                .map_or(0, |item| item.tag());
            menu::app_command(&AppContext { mtm: self.mtm() }, tag as u32);
        }
    }
);

/// Runs the app: sets up `NSApplication` and the main menu, calls `init` once the app has
/// launched, and runs the event loop until the app quits. Must be called on the main thread.
pub fn run(init: impl FnOnce(&AppContext) + 'static) {
    let mtm =
        MainThreadMarker::new().expect("auk_platform_macos::run must be called on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    // The menu bar is ours: AppKit doesn't add its own full-screen item to the View menu.
    objc2_foundation::NSUserDefaults::standardUserDefaults().setBool_forKey(
        false,
        &objc2_foundation::NSString::from_str("NSFullScreenMenuItemEverywhere"),
    );
    let delegate: Retained<AppDelegate> = {
        let this = AppDelegate::alloc(mtm).set_ivars(RefCell::new(Some(Box::new(init) as Init)));
        // SAFETY: NSObject's initializer.
        unsafe { msg_send![super(this), init] }
    };
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    menu::install(mtm, &menu::default_menu());
    app.run();
}
