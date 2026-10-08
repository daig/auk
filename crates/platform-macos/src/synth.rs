//! Synthetic key events, for the S1 input checks (spec §23): the same physical keystrokes
//! posted to an Auk window ([`Window::type_stroke`]) and to a reference terminal's process
//! ([`post_to_pid`]), so the bytes each sends to its program can be compared.
//!
//! Strokes are built as `CGEvent`s from a virtual key code, so the current keyboard layout and
//! input method interpret them exactly as they would a real key: dead keys compose, and IME
//! keystrokes go through `NSTextInputClient`.

use objc2::rc::Retained;
use objc2_app_kit::NSEvent;
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::{CGEvent, CGEventFlags};

use crate::Window;
use crate::keys::flags;

/// One keystroke: a virtual key code (`kVK_*`) and modifier flags ([`crate::keys::flags`]
/// bits, device-dependent side bits included).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Stroke {
    pub keycode: u16,
    pub flags: u64,
}

/// Device-dependent bits (`NX_DEVICE*KEYMASK`) that tell which side a modifier is on.
mod side {
    pub const LEFT_CONTROL: u64 = 0x1;
    pub const LEFT_SHIFT: u64 = 0x2;
    pub const LEFT_COMMAND: u64 = 0x8;
}

impl Stroke {
    pub fn new(keycode: u16, flags: u64) -> Stroke {
        Stroke { keycode, flags }
    }

    /// Parses `mod+mod+key`: modifiers `shift`, `ctrl`, `opt` (left Option), `ropt` (right
    /// Option), `cmd`; keys by their US-layout position (`a`, `1`, `[`, `space`) or by name
    /// (`return`, `tab`, `backspace`, `esc`, `up`, `f1`, `kp1`, `kpenter`…).
    pub fn parse(spec: &str) -> Option<Stroke> {
        let mut parts: Vec<&str> = spec.split('+').collect();
        // `+` itself as the key: "shift++".
        if spec.ends_with("++") {
            parts.pop();
            *parts.last_mut()? = "+";
        }
        let key = parts.pop()?;
        let mut bits = 0;
        for m in parts {
            bits |= match m {
                "shift" => flags::SHIFT | side::LEFT_SHIFT,
                "ctrl" => flags::CONTROL | side::LEFT_CONTROL,
                "opt" | "alt" => flags::OPTION | flags::LEFT_OPTION,
                "ropt" => flags::OPTION | flags::RIGHT_OPTION,
                "cmd" => flags::COMMAND | side::LEFT_COMMAND,
                _ => return None,
            };
        }
        Some(Stroke {
            keycode: keycode(key)?,
            flags: bits,
        })
    }

    /// The key-down and key-up `CGEvent`s.
    pub fn cg_events(&self) -> Option<[CFRetained<CGEvent>; 2]> {
        let make = |down| {
            let event = CGEvent::new_keyboard_event(None, self.keycode, down)?;
            CGEvent::set_flags(Some(&event), CGEventFlags(self.flags));
            Some(event)
        };
        Some([make(true)?, make(false)?])
    }

    /// The key-down and key-up `NSEvent`s.
    pub fn ns_events(&self) -> Option<[Retained<NSEvent>; 2]> {
        let [down, up] = self.cg_events()?;
        Some([
            NSEvent::eventWithCGEvent(&down)?,
            NSEvent::eventWithCGEvent(&up)?,
        ])
    }
}

impl Window {
    /// Types `stroke` into this window, through the same path as a real keystroke.
    pub fn type_stroke(&self, stroke: Stroke) -> bool {
        let Some([down, up]) = stroke.ns_events() else {
            return false;
        };
        // A synthesized event isn't routed to a window, so hand it to this one directly.
        self.send_event(&down);
        self.send_event(&up);
        true
    }
}

/// A synthetic mouse event for [`Window::send_mouse`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseAction {
    Down,
    Up,
    Dragged,
    Moved,
}

impl Window {
    /// Delivers a left- or right-button mouse event at (`x`, `y`) points from the view's
    /// top-left corner, as an `NSEvent` through the window (`sendEvent:`), as AppKit delivers a
    /// real one. `flags` are modifier flags ([`crate::keys::flags`]).
    pub fn send_mouse(
        &self,
        action: MouseAction,
        right: bool,
        x: f64,
        y: f64,
        clicks: u32,
        flags: u64,
    ) -> bool {
        use objc2_app_kit::{NSEventModifierFlags, NSEventType};
        let Some(window) = self.ns_window() else {
            return false;
        };
        let kind = match (action, right) {
            (MouseAction::Down, false) => NSEventType::LeftMouseDown,
            (MouseAction::Up, false) => NSEventType::LeftMouseUp,
            (MouseAction::Dragged, false) => NSEventType::LeftMouseDragged,
            (MouseAction::Down, true) => NSEventType::RightMouseDown,
            (MouseAction::Up, true) => NSEventType::RightMouseUp,
            (MouseAction::Dragged, true) => NSEventType::RightMouseDragged,
            (MouseAction::Moved, _) => NSEventType::MouseMoved,
        };
        let point = self
            .view
            .convertPoint_toView(objc2_foundation::NSPoint::new(x, y), None);
        let time = objc2_foundation::NSProcessInfo::processInfo().systemUptime();
        let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            kind,
            point,
            NSEventModifierFlags(flags as usize),
            time,
            window.windowNumber(),
            None,
            0,
            clicks as isize,
            if matches!(action, MouseAction::Up | MouseAction::Moved) { 0.0 } else { 1.0 },
        );
        match event {
            Some(event) => {
                window.sendEvent(&event);
                true
            }
            None => false,
        }
    }

    /// Delivers a scroll-wheel event of `lines` lines (positive scrolls toward the top) at
    /// (`x`, `y`) points from the view's top-left corner, through the window.
    pub fn send_scroll(&self, lines: i32, x: f64, y: f64) -> bool {
        use objc2::MainThreadOnly;
        use objc2_core_foundation::CGPoint;
        use objc2_core_graphics::CGScrollEventUnit;
        let Some(window) = self.ns_window() else {
            return false;
        };
        let Some(event) =
            CGEvent::new_scroll_wheel_event2(None, CGScrollEventUnit::Line, 1, lines, 0, 0)
        else {
            return false;
        };
        // CGEvent locations are global, from the top-left of the main display.
        let in_window = self
            .view
            .convertPoint_toView(objc2_foundation::NSPoint::new(x, y), None);
        let on_screen = window.convertPointToScreen(in_window);
        let main_height = objc2_app_kit::NSScreen::screens(window.mtm())
            .firstObject()
            .map_or(0.0, |s| s.frame().size.height);
        CGEvent::set_location(
            Some(&event),
            CGPoint {
                x: on_screen.x,
                y: main_height - on_screen.y,
            },
        );
        match NSEvent::eventWithCGEvent(&event) {
            Some(ns) => {
                window.sendEvent(&ns);
                true
            }
            None => false,
        }
    }
}

/// Runs the main run loop for `time`, so asynchronous input methods (which answer over mach
/// messages) finish handling the keys typed so far. Main thread only.
pub fn settle(time: std::time::Duration) {
    let until = objc2_foundation::NSDate::dateWithTimeIntervalSinceNow(time.as_secs_f64());
    objc2_foundation::NSRunLoop::currentRunLoop().runUntilDate(&until);
}

/// Whether this process may post events to other processes ([`post_to_pid`]): the
/// Accessibility permission for the app this runs under. Without it, posted events are dropped
/// silently.
pub fn can_post_events() -> bool {
    objc2_core_graphics::CGPreflightPostEventAccess()
}

/// Posts `stroke` to another process (a reference terminal). Needs the Accessibility permission
/// for the posting process ([`can_post_events`]).
pub fn post_to_pid(pid: i32, stroke: Stroke) -> bool {
    let Some([down, up]) = stroke.cg_events() else {
        return false;
    };
    CGEvent::post_to_pid(pid, Some(&down));
    CGEvent::post_to_pid(pid, Some(&up));
    true
}

/// Keyboard input sources (layouts and input methods), for driving IME checks: select the
/// Japanese input method, type romaji strokes, restore the user's source.
pub mod input_source {
    use std::ffi::c_void;
    use std::ptr::NonNull;

    use objc2_core_foundation::{CFArray, CFDictionary, CFRetained, CFString, CFType};

    #[repr(C)]
    struct OpaqueSource([u8; 0]);

    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        static kTISPropertyInputSourceID: &'static CFString;
        fn TISCopyCurrentKeyboardInputSource() -> Option<NonNull<OpaqueSource>>;
        fn TISCreateInputSourceList(
            properties: Option<&CFDictionary>,
            include_all_installed: bool,
        ) -> Option<NonNull<CFArray>>;
        fn TISGetInputSourceProperty(
            source: NonNull<OpaqueSource>,
            key: &CFString,
        ) -> *const c_void;
        fn TISEnableInputSource(source: NonNull<OpaqueSource>) -> i32;
        fn TISSelectInputSource(source: NonNull<OpaqueSource>) -> i32;
        fn CFRelease(object: *const c_void);
    }

    /// The input source currently selected, by ID (e.g. `com.apple.keylayout.US`).
    pub fn current() -> Option<String> {
        // SAFETY: TIS returns a retained source (released below) whose ID property is a CFString
        // owned by the source.
        unsafe {
            let source = TISCopyCurrentKeyboardInputSource()?;
            let id = id_of(source);
            CFRelease(source.as_ptr().cast());
            id
        }
    }

    /// Enables (if needed) and selects the installed input source with `id`, e.g.
    /// `com.apple.inputmethod.Kotoeri.RomajiTyping.Japanese`. An input mode's input method
    /// (`…Kotoeri.RomajiTyping`) is enabled first. Enabling adds the source to the user's input
    /// menu. Returns whether it was selected.
    pub fn select(id: &str) -> bool {
        let mut parents: Vec<&str> = id.match_indices('.').map(|(i, _)| &id[..i]).collect();
        parents.retain(|p| p.matches('.').count() >= 3);
        for parent in parents {
            with_source(parent, |source| {
                // SAFETY: a valid source.
                unsafe { TISEnableInputSource(source) };
            });
        }
        with_source(id, |source| {
            // SAFETY: a valid source.
            unsafe {
                TISEnableInputSource(source);
                TISSelectInputSource(source) == 0
            }
        })
        .unwrap_or(false)
    }

    /// Calls `f` with the installed source `id`, if there is one.
    fn with_source<R>(id: &str, f: impl FnOnce(NonNull<OpaqueSource>) -> R) -> Option<R> {
        let key: &CFString = unsafe { kTISPropertyInputSourceID };
        let value = CFString::from_str(id);
        let filter = CFDictionary::<CFString, CFType>::from_slices(&[key], &[&value]);
        // SAFETY: a valid filter dictionary; the returned array is retained and keeps its
        // sources alive while `f` runs.
        unsafe {
            let list = TISCreateInputSourceList(Some(filter.as_opaque()), true)?;
            let list: CFRetained<CFArray<CFType>> = CFRetained::from_raw(list.cast());
            let first = list.get(0)?;
            Some(f(NonNull::from(&*first).cast::<OpaqueSource>()))
        }
    }

    unsafe fn id_of(source: NonNull<OpaqueSource>) -> Option<String> {
        // SAFETY: the caller passes a valid source; the property is a CFString it owns.
        unsafe {
            let id = TISGetInputSourceProperty(source, kTISPropertyInputSourceID);
            (!id.is_null()).then(|| (*id.cast::<CFString>()).to_string())
        }
    }
}

/// The virtual key code for a key name: US-layout positions (`kVK_ANSI_*`) and named keys.
pub fn keycode(name: &str) -> Option<u16> {
    const ANSI: &[(&str, u16)] = &[
        ("a", 0x00),
        ("s", 0x01),
        ("d", 0x02),
        ("f", 0x03),
        ("h", 0x04),
        ("g", 0x05),
        ("z", 0x06),
        ("x", 0x07),
        ("c", 0x08),
        ("v", 0x09),
        ("b", 0x0B),
        ("q", 0x0C),
        ("w", 0x0D),
        ("e", 0x0E),
        ("r", 0x0F),
        ("y", 0x10),
        ("t", 0x11),
        ("1", 0x12),
        ("2", 0x13),
        ("3", 0x14),
        ("4", 0x15),
        ("6", 0x16),
        ("5", 0x17),
        ("=", 0x18),
        ("9", 0x19),
        ("7", 0x1A),
        ("-", 0x1B),
        ("8", 0x1C),
        ("0", 0x1D),
        ("]", 0x1E),
        ("o", 0x1F),
        ("u", 0x20),
        ("[", 0x21),
        ("i", 0x22),
        ("p", 0x23),
        ("l", 0x25),
        ("j", 0x26),
        ("'", 0x27),
        ("k", 0x28),
        (";", 0x29),
        ("\\", 0x2A),
        (",", 0x2B),
        ("/", 0x2C),
        ("n", 0x2D),
        ("m", 0x2E),
        (".", 0x2F),
        ("`", 0x32),
    ];
    const NAMED: &[(&str, u16)] = &[
        ("return", 0x24),
        ("enter", 0x24),
        ("tab", 0x30),
        ("space", 0x31),
        ("backspace", 0x33),
        ("esc", 0x35),
        ("escape", 0x35),
        ("delete", 0x75),
        ("help", 0x72),
        ("insert", 0x72),
        ("home", 0x73),
        ("end", 0x77),
        ("pageup", 0x74),
        ("pagedown", 0x79),
        ("left", 0x7B),
        ("right", 0x7C),
        ("down", 0x7D),
        ("up", 0x7E),
        ("f1", 0x7A),
        ("f2", 0x78),
        ("f3", 0x63),
        ("f4", 0x76),
        ("f5", 0x60),
        ("f6", 0x61),
        ("f7", 0x62),
        ("f8", 0x64),
        ("f9", 0x65),
        ("f10", 0x6D),
        ("f11", 0x67),
        ("f12", 0x6F),
        ("f13", 0x69),
        ("f14", 0x6B),
        ("f15", 0x71),
        ("f16", 0x6A),
        ("f17", 0x40),
        ("f18", 0x4F),
        ("f19", 0x50),
        ("f20", 0x5A),
        ("kp0", 0x52),
        ("kp1", 0x53),
        ("kp2", 0x54),
        ("kp3", 0x55),
        ("kp4", 0x56),
        ("kp5", 0x57),
        ("kp6", 0x58),
        ("kp7", 0x59),
        ("kp8", 0x5B),
        ("kp9", 0x5C),
        ("kp.", 0x41),
        ("kp*", 0x43),
        ("kp+", 0x45),
        ("kp/", 0x4B),
        ("kp-", 0x4E),
        ("kp=", 0x51),
        ("kpenter", 0x4C),
    ];
    let name = name.to_ascii_lowercase();
    ANSI.iter()
        .chain(NAMED)
        .find(|(n, _)| *n == name)
        .map(|&(_, code)| code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys;

    #[test]
    fn parses_strokes() {
        assert_eq!(Stroke::parse("a"), Some(Stroke::new(0x00, 0)));
        let s = Stroke::parse("ctrl+shift+up").unwrap();
        assert_eq!(s.keycode, 0x7E);
        assert!(s.flags & flags::CONTROL != 0 && s.flags & flags::SHIFT != 0);
        let o = Stroke::parse("opt+e").unwrap();
        assert!(keys::option_is_alt(o.flags, keys::OptionAsAlt::Left));
        assert!(!keys::option_is_alt(o.flags, keys::OptionAsAlt::Right));
        let r = Stroke::parse("ropt+e").unwrap();
        assert!(keys::option_is_alt(r.flags, keys::OptionAsAlt::Right));
        assert_eq!(Stroke::parse("kpenter").unwrap().keycode, 0x4C);
        assert!(Stroke::parse("hyper+a").is_none());
        assert!(Stroke::parse("nosuchkey").is_none());
    }

    #[test]
    fn every_named_key_maps_to_the_key_model() {
        // The synthesizer's names and the view's key table agree on the special keys.
        for (name, key) in [
            ("return", auk_term::input::Key::Enter),
            ("tab", auk_term::input::Key::Tab),
            ("backspace", auk_term::input::Key::Backspace),
            ("esc", auk_term::input::Key::Escape),
            ("f12", auk_term::input::Key::F(12)),
            ("kp5", auk_term::input::Key::Keypad('5')),
            ("pagedown", auk_term::input::Key::PageDown),
        ] {
            assert_eq!(
                keys::special_key(keycode(name).unwrap()),
                Some(key),
                "{name}"
            );
        }
    }

    #[test]
    fn current_input_source_has_an_id() {
        let id = input_source::current().expect("a selected input source");
        assert!(id.starts_with("com.") || id.contains('.'), "{id}");
    }

    #[test]
    fn events_carry_code_and_flags() {
        let stroke = Stroke::parse("shift+ctrl+k").unwrap();
        let [down, up] = stroke.ns_events().expect("NSEvent from CGEvent");
        assert_eq!(down.keyCode(), 0x28);
        assert_eq!(up.keyCode(), 0x28);
        let bits = down.modifierFlags().0 as u64;
        assert!(bits & flags::SHIFT != 0 && bits & flags::CONTROL != 0);
        assert_eq!(down.r#type(), objc2_app_kit::NSEventType::KeyDown);
        assert_eq!(up.r#type(), objc2_app_kit::NSEventType::KeyUp);
    }
}
