//! The content view: a `CAMetalLayer`-backed `NSView` that implements `NSTextInputClient` and
//! turns AppKit's events into [`Event`]s for the window's handler.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ops::Range;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, Sel};
use objc2::{
    AllocAnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class,
    msg_send, sel,
};
use objc2_app_kit::{
    NSAppearanceCustomization, NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSEvent,
    NSEventModifierFlags, NSEventPhase, NSResponder, NSTextInputClient, NSTrackingArea,
    NSTrackingAreaOptions, NSView, NSViewLayerContentsRedrawPolicy,
};
use objc2_foundation::{
    NSArray, NSAttributedString, NSAttributedStringKey, NSNotFound, NSPoint, NSRange, NSRect,
    NSRunLoop, NSRunLoopCommonModes, NSSize, NSString, NSUInteger,
};
use objc2_quartz_core::{CADisplayLink, CALayer, CAMetalLayer};

use crate::keys::{self, OptionAsAlt};
use crate::window::{Waker, Window, WindowDelegate, WindowId, registry};
use crate::{
    Appearance, Command, Event, KeyInput, MouseButton, MouseEvent, MouseKind, Preedit, ScrollEvent,
    ScrollPhase, Size, WindowHandler,
};
use auk_term::input::{Key, KeyEvent, Mods};

/// What `interpretKeyEvents:` did with the key being handled.
#[derive(Default)]
struct KeyDown {
    /// Text the input system inserted.
    inserted: Option<String>,
    /// The input system mapped the key to an editing command (`insertNewline:`, `moveUp:`…).
    command: bool,
}

pub(crate) struct ViewState {
    pub id: WindowId,
    handler: RefCell<Option<Box<dyn WindowHandler>>>,
    /// Events raised while the handler was busy (re-entrant AppKit callbacks), delivered after.
    pending: RefCell<VecDeque<Event>>,
    /// The input method's marked text; empty when not composing.
    marked: RefCell<String>,
    /// Set while `keyDown:` runs `interpretKeyEvents:`.
    key_down: RefCell<Option<KeyDown>>,
    pub layer: Retained<CAMetalLayer>,
    display_link: RefCell<Option<Retained<CADisplayLink>>>,
    frame_requested: Cell<bool>,
    pub option_as_alt: Cell<OptionAsAlt>,
    /// Where the IME's candidate window goes: the cursor's cell, in view points.
    pub ime_rect: Cell<NSRect>,
    pub delegate: RefCell<Option<Retained<WindowDelegate>>>,
    pub waker: Waker,
    closed: Cell<bool>,
    /// AccessKit's adapter, once the app supplies a tree.
    pub a11y: RefCell<Option<crate::accessibility::Accessibility>>,
}

define_class!(
    #[unsafe(super(NSView, NSResponder, objc2_foundation::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "AukView"]
    #[ivars = ViewState]
    pub(crate) struct AukView;

    unsafe impl NSObjectProtocol for AukView {}

    impl AukView {
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(wantsUpdateLayer))]
        fn wants_update_layer(&self) -> bool {
            true
        }

        #[unsafe(method_id(makeBackingLayer))]
        fn make_backing_layer(&self) -> Retained<CALayer> {
            Retained::into_super(self.ivars().layer.clone())
        }

        #[unsafe(method(viewDidMoveToWindow))]
        fn view_did_move_to_window(&self) {
            self.update_scale();
            if self.window().is_some() && self.ivars().display_link.borrow().is_none() {
                self.start_display_link();
            }
        }

        #[unsafe(method(viewDidChangeBackingProperties))]
        fn view_did_change_backing_properties(&self) {
            self.update_scale();
            self.emit(Event::Resized(self.size()));
        }

        #[unsafe(method(setFrameSize:))]
        fn set_frame_size(&self, size: NSSize) {
            // SAFETY: forwarding to NSView's implementation with the same argument.
            let _: () = unsafe { msg_send![super(self), setFrameSize: size] };
            self.emit(Event::Resized(self.size()));
        }

        #[unsafe(method(viewDidChangeEffectiveAppearance))]
        fn view_did_change_effective_appearance(&self) {
            self.emit(Event::Appearance(self.appearance()));
        }

        #[unsafe(method(displayLinkFired:))]
        fn display_link_fired(&self, _link: &CADisplayLink) {
            self.ivars().frame_requested.set(false);
            self.emit(Event::Frame);
            if !self.ivars().frame_requested.get()
                && let Some(link) = self.ivars().display_link.borrow().as_ref()
            {
                link.setPaused(true);
            }
        }

        // Keyboard.

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            self.handle_key_down(event);
        }

        #[unsafe(method(keyUp:))]
        fn key_up(&self, _event: &NSEvent) {}

        #[unsafe(method(flagsChanged:))]
        fn flags_changed(&self, event: &NSEvent) {
            self.emit(Event::ModifiersChanged(self.mods_of(event)));
        }

        // Mouse.

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            self.mouse(event, MouseKind::Down(MouseButton::Left));
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            self.mouse(event, MouseKind::Up(MouseButton::Left));
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            self.mouse(event, MouseKind::Dragged(MouseButton::Left));
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, event: &NSEvent) {
            self.mouse(event, MouseKind::Down(MouseButton::Right));
        }

        #[unsafe(method(rightMouseUp:))]
        fn right_mouse_up(&self, event: &NSEvent) {
            self.mouse(event, MouseKind::Up(MouseButton::Right));
        }

        #[unsafe(method(rightMouseDragged:))]
        fn right_mouse_dragged(&self, event: &NSEvent) {
            self.mouse(event, MouseKind::Dragged(MouseButton::Right));
        }

        #[unsafe(method(otherMouseDown:))]
        fn other_mouse_down(&self, event: &NSEvent) {
            self.mouse(event, MouseKind::Down(other_button(event)));
        }

        #[unsafe(method(otherMouseUp:))]
        fn other_mouse_up(&self, event: &NSEvent) {
            self.mouse(event, MouseKind::Up(other_button(event)));
        }

        #[unsafe(method(otherMouseDragged:))]
        fn other_mouse_dragged(&self, event: &NSEvent) {
            self.mouse(event, MouseKind::Dragged(other_button(event)));
        }

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            self.mouse(event, MouseKind::Moved);
        }

        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, event: &NSEvent) {
            self.mouse(event, MouseKind::Entered);
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, event: &NSEvent) {
            self.mouse(event, MouseKind::Exited);
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            let (x, y) = self.location(event);
            let phase = event.phase();
            let momentum = event.momentumPhase();
            let phase = if momentum.contains(NSEventPhase::Ended) {
                ScrollPhase::MomentumEnded
            } else if momentum != NSEventPhase::None {
                ScrollPhase::Momentum
            } else if phase.contains(NSEventPhase::Began) {
                ScrollPhase::Began
            } else if phase.contains(NSEventPhase::Changed) {
                ScrollPhase::Changed
            } else if phase.contains(NSEventPhase::Ended) || phase.contains(NSEventPhase::Cancelled) {
                ScrollPhase::Ended
            } else {
                ScrollPhase::None
            };
            self.emit(Event::Scroll(ScrollEvent {
                dx: event.scrollingDeltaX(),
                dy: event.scrollingDeltaY(),
                precise: event.hasPreciseScrollingDeltas(),
                phase,
                x,
                y,
                mods: self.mods_of(event),
            }));
        }

        // Menu commands sent down the responder chain.

        #[unsafe(method(copy:))]
        fn copy(&self, _sender: Option<&AnyObject>) {
            self.emit(Event::Command(Command::Copy));
        }

        #[unsafe(method(paste:))]
        fn paste(&self, _sender: Option<&AnyObject>) {
            self.emit(Event::Command(Command::Paste));
        }

        #[unsafe(method(selectAll:))]
        fn select_all(&self, _sender: Option<&AnyObject>) {
            self.emit(Event::Command(Command::SelectAll));
        }

        /// A registry command from the menu (`MenuItem::Command`), by its tag.
        #[unsafe(method(aukCommand:))]
        fn auk_command(&self, sender: Option<&AnyObject>) {
            let tag = sender
                .and_then(|s| s.downcast_ref::<objc2_app_kit::NSMenuItem>())
                .map_or(0, |item| item.tag());
            self.emit(Event::Menu(tag as u32));
        }

        // Accessibility: AccessKit's adapter answers once the app supplies a tree.

        #[unsafe(method(accessibilityChildren))]
        fn accessibility_children(&self) -> *mut AnyObject {
            let mut a11y = self.ivars().a11y.borrow_mut();
            match a11y.as_mut() {
                Some(a) => a.adapter.view_children(&mut a.activation).cast(),
                // SAFETY: NSView's implementation, returning an autoreleased array or nil.
                None => unsafe { msg_send![super(self), accessibilityChildren] },
            }
        }

        #[unsafe(method(accessibilityFocusedUIElement))]
        fn accessibility_focused_ui_element(&self) -> *mut AnyObject {
            let mut a11y = self.ivars().a11y.borrow_mut();
            match a11y.as_mut() {
                Some(a) => a.adapter.focus(&mut a.activation).cast(),
                // SAFETY: NSView's implementation.
                None => unsafe { msg_send![super(self), accessibilityFocusedUIElement] },
            }
        }

        #[unsafe(method(accessibilityHitTest:))]
        fn accessibility_hit_test(&self, point: NSPoint) -> *mut AnyObject {
            let mut a11y = self.ivars().a11y.borrow_mut();
            match a11y.as_mut() {
                Some(a) => a
                    .adapter
                    .hit_test(accesskit_macos::NSPoint::new(point.x, point.y), &mut a.activation)
                    .cast(),
                // SAFETY: NSView's implementation.
                None => unsafe { msg_send![super(self), accessibilityHitTest: point] },
            }
        }
    }

    unsafe impl NSTextInputClient for AukView {
        #[unsafe(method(hasMarkedText))]
        fn has_marked_text(&self) -> bool {
            !self.ivars().marked.borrow().is_empty()
        }

        #[unsafe(method(markedRange))]
        fn marked_range(&self) -> NSRange {
            let len = self.ivars().marked.borrow().encode_utf16().count();
            if len == 0 { not_found() } else { NSRange::new(0, len) }
        }

        #[unsafe(method(selectedRange))]
        fn selected_range(&self) -> NSRange {
            // The terminal has no editable selection; the caret is at the end of any composition.
            let len = self.ivars().marked.borrow().encode_utf16().count();
            if len == 0 { not_found() } else { NSRange::new(len, 0) }
        }

        #[unsafe(method(setMarkedText:selectedRange:replacementRange:))]
        fn set_marked_text(&self, string: &AnyObject, selected: NSRange, _replacement: NSRange) {
            let text = string_of(string);
            if text.is_empty() {
                self.unmark();
                return;
            }
            let selection = utf16_range_to_bytes(&text, selected.location, selected.length);
            *self.ivars().marked.borrow_mut() = text.clone();
            self.emit(Event::Preedit(Some(Preedit { text, selection })));
        }

        #[unsafe(method(unmarkText))]
        fn unmark_text(&self) {
            self.unmark();
        }

        #[unsafe(method_id(validAttributesForMarkedText))]
        fn valid_attributes_for_marked_text(&self) -> Retained<NSArray<NSAttributedStringKey>> {
            NSArray::new()
        }

        #[unsafe(method_id(attributedSubstringForProposedRange:actualRange:))]
        fn attributed_substring_for_proposed_range(
            &self,
            _range: NSRange,
            _actual: *mut NSRange,
        ) -> Option<Retained<NSAttributedString>> {
            None
        }

        #[unsafe(method(insertText:replacementRange:))]
        fn insert_text(&self, string: &AnyObject, _replacement: NSRange) {
            let text = string_of(string);
            self.unmark();
            let mut key_down = self.ivars().key_down.borrow_mut();
            if let Some(state) = key_down.as_mut() {
                state.inserted.get_or_insert_with(String::new).push_str(&text);
            } else {
                drop(key_down);
                if !text.is_empty() {
                    self.emit(Event::Text(text));
                }
            }
        }

        #[unsafe(method(characterIndexForPoint:))]
        fn character_index_for_point(&self, _point: NSPoint) -> NSUInteger {
            NSNotFound as NSUInteger
        }

        #[unsafe(method(firstRectForCharacterRange:actualRange:))]
        fn first_rect_for_character_range(&self, _range: NSRange, _actual: *mut NSRange) -> NSRect {
            let rect = self.convertRect_toView(self.ivars().ime_rect.get(), None);
            match self.window() {
                Some(window) => window.convertRectToScreen(rect),
                None => rect,
            }
        }

        #[unsafe(method(doCommandBySelector:))]
        fn do_command_by_selector(&self, _selector: Sel) {
            // Keys that map to editing commands (Return, arrows, ⌃A…) are sent as keys by
            // `keyDown:` once `interpretKeyEvents:` returns.
            if let Some(state) = self.ivars().key_down.borrow_mut().as_mut() {
                state.command = true;
            }
        }
    }
);

impl AukView {
    pub fn new(
        mtm: MainThreadMarker,
        frame: NSRect,
        id: WindowId,
        handler: Box<dyn WindowHandler>,
        option_as_alt: OptionAsAlt,
    ) -> Retained<AukView> {
        let layer = CAMetalLayer::new();
        layer.setOpaque(true);
        let state = ViewState {
            id,
            handler: RefCell::new(Some(handler)),
            pending: RefCell::new(VecDeque::new()),
            marked: RefCell::new(String::new()),
            key_down: RefCell::new(None),
            layer,
            display_link: RefCell::new(None),
            frame_requested: Cell::new(false),
            option_as_alt: Cell::new(option_as_alt),
            ime_rect: Cell::new(NSRect::ZERO),
            delegate: RefCell::new(None),
            waker: Waker::new(id),
            closed: Cell::new(false),
            a11y: RefCell::new(None),
        };
        let this = Self::alloc(mtm).set_ivars(state);
        // SAFETY: NSView's designated initializer.
        let this: Retained<AukView> = unsafe { msg_send![super(this), initWithFrame: frame] };
        this.setWantsLayer(true);
        this.setLayerContentsRedrawPolicy(NSViewLayerContentsRedrawPolicy::DuringViewResize);
        let options = NSTrackingAreaOptions::MouseMoved
            | NSTrackingAreaOptions::MouseEnteredAndExited
            | NSTrackingAreaOptions::ActiveInKeyWindow
            | NSTrackingAreaOptions::InVisibleRect;
        // SAFETY: the view owns the tracking area, which is removed with it.
        let area = unsafe {
            NSTrackingArea::initWithRect_options_owner_userInfo(
                NSTrackingArea::alloc(),
                NSRect::ZERO,
                options,
                Some(&this),
                None,
            )
        };
        this.addTrackingArea(&area);
        this
    }

    /// Delivers `event` to the handler, after any events raised while it was busy.
    pub(crate) fn emit(&self, event: Event) {
        self.ivars().pending.borrow_mut().push_back(event);
        let Ok(mut handler) = self.ivars().handler.try_borrow_mut() else {
            // The handler is running further up the stack; it gets this when it returns.
            return;
        };
        let window = Window::from_view(self.retain());
        loop {
            let next = self.ivars().pending.borrow_mut().pop_front();
            let Some(event) = next else { break };
            if let Some(handler) = handler.as_mut() {
                handler.event(&window, event);
            }
        }
        drop(handler);
        if self.ivars().closed.get() {
            self.ivars().handler.borrow_mut().take();
        }
    }

    pub(crate) fn size(&self) -> Size {
        let frame = self.frame();
        Size {
            width: frame.size.width,
            height: frame.size.height,
            scale: self.scale(),
        }
    }

    pub(crate) fn scale(&self) -> f64 {
        self.window().map_or(1.0, |w| w.backingScaleFactor())
    }

    pub(crate) fn appearance(&self) -> Appearance {
        // SAFETY: the appearance names are constants.
        let names =
            unsafe { NSArray::from_slice(&[NSAppearanceNameAqua, NSAppearanceNameDarkAqua]) };
        let best = self
            .effectiveAppearance()
            .bestMatchFromAppearancesWithNames(&names);
        // SAFETY: a constant.
        let dark = unsafe { NSAppearanceNameDarkAqua };
        if best.is_some_and(|name| &*name == dark) {
            Appearance::Dark
        } else {
            Appearance::Light
        }
    }

    pub(crate) fn request_frame(&self) {
        self.ivars().frame_requested.set(true);
        if let Some(link) = self.ivars().display_link.borrow().as_ref() {
            link.setPaused(false);
        }
    }

    /// Tears down per-window state as the window closes; the handler gets `Closed` and is then
    /// dropped.
    pub(crate) fn closed(&self) {
        if self.ivars().closed.replace(true) {
            return;
        }
        if let Some(link) = self.ivars().display_link.borrow_mut().take() {
            link.invalidate();
        }
        registry::remove(self.ivars().id);
        self.emit(Event::Closed);
    }

    /// Tells AccessKit whether the window is key.
    pub(crate) fn accessibility_focus(&self, focused: bool) {
        let events = self
            .ivars()
            .a11y
            .borrow_mut()
            .as_mut()
            .and_then(|a| a.adapter.update_view_focus_state(focused));
        if let Some(events) = events {
            events.raise();
        }
    }

    fn update_scale(&self) {
        let scale = self.scale();
        self.ivars().layer.setContentsScale(scale);
    }

    fn start_display_link(&self) {
        // SAFETY: the selector names a method of this class taking a CADisplayLink.
        let link = unsafe { self.displayLinkWithTarget_selector(self, sel!(displayLinkFired:)) };
        link.setPaused(!self.ivars().frame_requested.get());
        // SAFETY: the main run loop and a constant mode; common modes keep frames coming during
        // live resize and menu tracking.
        unsafe { link.addToRunLoop_forMode(&NSRunLoop::mainRunLoop(), NSRunLoopCommonModes) };
        *self.ivars().display_link.borrow_mut() = Some(link);
    }

    fn is_composing(&self) -> bool {
        !self.ivars().marked.borrow().is_empty()
    }

    fn unmark(&self) {
        let was = std::mem::take(&mut *self.ivars().marked.borrow_mut());
        if !was.is_empty() {
            self.emit(Event::Preedit(None));
        }
    }

    fn option_alt(&self, event: &NSEvent) -> bool {
        keys::option_is_alt(
            event.modifierFlags().0 as u64,
            self.ivars().option_as_alt.get(),
        )
    }

    fn mods_of(&self, event: &NSEvent) -> Mods {
        keys::mods(event.modifierFlags().0 as u64, self.option_alt(event))
    }

    fn location(&self, event: &NSEvent) -> (f64, f64) {
        let p = self.convertPoint_fromView(event.locationInWindow(), None);
        (p.x, p.y)
    }

    fn mouse(&self, event: &NSEvent, kind: MouseKind) {
        let (x, y) = self.location(event);
        let clicks = match kind {
            MouseKind::Down(_) | MouseKind::Up(_) => event.clickCount().max(0) as u32,
            _ => 0,
        };
        self.emit(Event::Mouse(MouseEvent {
            kind,
            x,
            y,
            mods: self.mods_of(event),
            clicks,
        }));
    }

    fn handle_key_down(&self, event: &NSEvent) {
        let flags = event.modifierFlags();
        let option_alt = self.option_alt(event);
        let mods = keys::mods(flags.0 as u64, option_alt);
        let keycode = event.keyCode();
        let repeat = event.isARepeat();
        let base = event.charactersByApplyingModifiers(NSEventModifierFlags::empty());
        let key = keys::key(keycode, base.map(|s| s.to_string()).as_deref());
        let composing = self.is_composing();

        if !composing && (option_alt || mods.ctrl || mods.meta) {
            // A binding, not text: bypass the input system so AppKit's key bindings (⌃A as
            // moveToBeginningOfLine:, Option's dead keys) don't intercept it.
            let text = if mods.ctrl || mods.meta {
                None
            } else {
                event
                    .charactersByApplyingModifiers(flags & !NSEventModifierFlags::Option)
                    .map(|s| s.to_string())
                    .filter(|t| keys::is_text(t))
            };
            if let Some(key) = key {
                self.emit_key(key, mods, text, repeat, keycode);
            }
            return;
        }

        *self.ivars().key_down.borrow_mut() = Some(KeyDown::default());
        let events = NSArray::from_retained_slice(&[event.retain()]);
        self.interpretKeyEvents(&events);
        let state = self
            .ivars()
            .key_down
            .borrow_mut()
            .take()
            .unwrap_or_default();
        let composing_after = self.is_composing();

        match state.inserted {
            // An ordinary key: the text it typed.
            Some(text) if !composing && key.is_some() => {
                let text = keys::is_text(&text).then_some(text);
                self.emit_key(key.expect("checked"), mods, text, repeat, keycode);
            }
            // A composition's commit (dead key, input method).
            Some(text) => {
                if !text.is_empty() {
                    self.emit(Event::Text(text));
                }
            }
            // The input method took the key to edit its composition.
            None if composing || composing_after => {}
            // A key the input system typed nothing for. With an inactive input context (the
            // app in the background) that is every key, so fall back to the event's own text.
            None => {
                if let Some(key) = key {
                    let text = event
                        .characters()
                        .map(|s| s.to_string())
                        .filter(|t| keys::is_text(t));
                    let text = if matches!(key, Key::Char(_) | Key::Keypad(_)) {
                        text
                    } else {
                        None
                    };
                    // A character key with no text and no command is a dead key the layout holds
                    // until the next key, which arrives composed.
                    if matches!(key, Key::Char(_)) && text.is_none() && !state.command {
                        return;
                    }
                    self.emit_key(key, mods, text, repeat, keycode);
                }
            }
        }
    }

    fn emit_key(&self, key: Key, mods: Mods, text: Option<String>, repeat: bool, keycode: u16) {
        self.emit(Event::Key(KeyInput {
            event: KeyEvent { key, mods, text },
            repeat,
            keycode,
        }));
    }
}

fn not_found() -> NSRange {
    NSRange::new(NSNotFound as usize, 0)
}

fn other_button(event: &NSEvent) -> MouseButton {
    match event.buttonNumber() {
        2 => MouseButton::Middle,
        n => MouseButton::Other(n.clamp(0, 255) as u8),
    }
}

/// The text of an `NSString` or `NSAttributedString`.
fn string_of(object: &AnyObject) -> String {
    if let Some(s) = object.downcast_ref::<NSAttributedString>() {
        s.string().to_string()
    } else if let Some(s) = object.downcast_ref::<NSString>() {
        s.to_string()
    } else {
        String::new()
    }
}

/// A UTF-16 range in `text` as a byte range, clamped to the text.
pub(crate) fn utf16_range_to_bytes(text: &str, location: usize, length: usize) -> Range<usize> {
    let byte_at = |units: usize| {
        let mut count = 0;
        for (i, c) in text.char_indices() {
            if count >= units {
                return i;
            }
            count += c.len_utf16();
        }
        text.len()
    };
    if location == NSNotFound as usize {
        return text.len()..text.len();
    }
    let start = byte_at(location);
    let end = byte_at(location.saturating_add(length)).max(start);
    start..end
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_ranges() {
        assert_eq!(utf16_range_to_bytes("abc", 1, 1), 1..2);
        assert_eq!(utf16_range_to_bytes("にほん", 1, 2), 3..9);
        assert_eq!(utf16_range_to_bytes("a😀b", 1, 2), 1..5);
        assert_eq!(utf16_range_to_bytes("a😀b", 3, 0), 5..5);
        assert_eq!(utf16_range_to_bytes("ab", 9, 9), 2..2);
        assert_eq!(utf16_range_to_bytes("ab", NSNotFound as usize, 0), 2..2);
    }
}
