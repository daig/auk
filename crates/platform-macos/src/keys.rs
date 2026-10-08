//! Mapping macOS key events to the terminal's key model (`auk_term::input`).
//!
//! The functions here are pure: they take the parts of an `NSEvent` the view extracts (virtual
//! key code, modifier flags, the characters the layout produced) so they can be tested without
//! AppKit.

use auk_term::input::{Key, Mods};

/// Which Option keys act as Alt (sending ESC-prefixed or kitty-encoded Alt) instead of composing
/// characters through the keyboard layout.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum OptionAsAlt {
    /// Both Option keys compose characters (å, ∑, dead keys).
    #[default]
    None,
    Left,
    Right,
    Both,
}

/// `NSEventModifierFlags` bits, plus the device-dependent bits that tell left from right.
pub mod flags {
    pub const CAPS_LOCK: u64 = 1 << 16;
    pub const SHIFT: u64 = 1 << 17;
    pub const CONTROL: u64 = 1 << 18;
    pub const OPTION: u64 = 1 << 19;
    pub const COMMAND: u64 = 1 << 20;
    pub const NUMERIC_PAD: u64 = 1 << 21;
    pub const FUNCTION: u64 = 1 << 23;
    /// `NX_DEVICELALTKEYMASK`.
    pub const LEFT_OPTION: u64 = 0x20;
    /// `NX_DEVICERALTKEYMASK`.
    pub const RIGHT_OPTION: u64 = 0x40;
}

/// Whether the Option keys held in `raw_flags` act as Alt under `setting`.
pub fn option_is_alt(raw_flags: u64, setting: OptionAsAlt) -> bool {
    if raw_flags & flags::OPTION == 0 {
        return false;
    }
    let left = raw_flags & flags::LEFT_OPTION != 0;
    let right = raw_flags & flags::RIGHT_OPTION != 0;
    // Synthetic events may carry no device bits; treat them as the left key.
    let left = left || !right;
    match setting {
        OptionAsAlt::None => false,
        OptionAsAlt::Both => true,
        OptionAsAlt::Left => left,
        OptionAsAlt::Right => right,
    }
}

/// The terminal modifiers for `raw_flags`. Option counts as Alt only when it acts as Alt;
/// otherwise the layout consumed it.
pub fn mods(raw_flags: u64, option_alt: bool) -> Mods {
    Mods {
        shift: raw_flags & flags::SHIFT != 0,
        ctrl: raw_flags & flags::CONTROL != 0,
        alt: option_alt,
        meta: raw_flags & flags::COMMAND != 0,
    }
}

/// Virtual key codes (`kVK_*` in HIToolbox's Events.h) of keys that don't produce characters,
/// and of the keypad.
pub fn special_key(keycode: u16) -> Option<Key> {
    Some(match keycode {
        0x24 => Key::Enter,
        0x30 => Key::Tab,
        0x33 => Key::Backspace,
        0x35 => Key::Escape,
        0x75 => Key::Delete,
        0x72 => Key::Insert, // Help, where an Insert key would be
        0x73 => Key::Home,
        0x77 => Key::End,
        0x74 => Key::PageUp,
        0x79 => Key::PageDown,
        0x7B => Key::Left,
        0x7C => Key::Right,
        0x7D => Key::Down,
        0x7E => Key::Up,
        0x7A => Key::F(1),
        0x78 => Key::F(2),
        0x63 => Key::F(3),
        0x76 => Key::F(4),
        0x60 => Key::F(5),
        0x61 => Key::F(6),
        0x62 => Key::F(7),
        0x64 => Key::F(8),
        0x65 => Key::F(9),
        0x6D => Key::F(10),
        0x67 => Key::F(11),
        0x6F => Key::F(12),
        0x69 => Key::F(13),
        0x6B => Key::F(14),
        0x71 => Key::F(15),
        0x6A => Key::F(16),
        0x40 => Key::F(17),
        0x4F => Key::F(18),
        0x50 => Key::F(19),
        0x5A => Key::F(20),
        0x52 => Key::Keypad('0'),
        0x53 => Key::Keypad('1'),
        0x54 => Key::Keypad('2'),
        0x55 => Key::Keypad('3'),
        0x56 => Key::Keypad('4'),
        0x57 => Key::Keypad('5'),
        0x58 => Key::Keypad('6'),
        0x59 => Key::Keypad('7'),
        0x5B => Key::Keypad('8'),
        0x5C => Key::Keypad('9'),
        0x41 => Key::Keypad('.'),
        0x43 => Key::Keypad('*'),
        0x45 => Key::Keypad('+'),
        0x4B => Key::Keypad('/'),
        0x4E => Key::Keypad('-'),
        0x51 => Key::Keypad('='),
        0x4C => Key::Keypad('\r'),
        _ => return None,
    })
}

/// Virtual key codes of modifier keys (they arrive as `flagsChanged:`, never as key presses).
pub fn is_modifier_key(keycode: u16) -> bool {
    matches!(keycode, 0x36..=0x3F)
}

/// The key for an event: a special key by its code, or the character the key produces with no
/// modifiers on the current layout (`base`, from `charactersByApplyingModifiers:` with no
/// flags). `None` for keys the model has no name for.
pub fn key(keycode: u16, base: Option<&str>) -> Option<Key> {
    if let Some(key) = special_key(keycode) {
        return Some(key);
    }
    let mut chars = base?.chars();
    let c = chars.next()?;
    if chars.next().is_some() || c.is_control() {
        return None;
    }
    // AppKit reports the function keys' private-use characters (U+F700…) for some keys that
    // have no `kVK_` entry above; they name nothing the model knows.
    if ('\u{F700}'..='\u{F8FF}').contains(&c) {
        return None;
    }
    Some(Key::Char(c.to_lowercase().next().unwrap_or(c)))
}

/// Whether `text` is something to send as typed text: printable, not a control character or a
/// function key's private-use character.
pub fn is_text(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|c| !c.is_control() && !('\u{F700}'..='\u{F8FF}').contains(&c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn option_sides() {
        let left = flags::OPTION | flags::LEFT_OPTION;
        let right = flags::OPTION | flags::RIGHT_OPTION;
        assert!(!option_is_alt(left, OptionAsAlt::None));
        assert!(option_is_alt(left, OptionAsAlt::Both));
        assert!(option_is_alt(left, OptionAsAlt::Left));
        assert!(!option_is_alt(left, OptionAsAlt::Right));
        assert!(option_is_alt(right, OptionAsAlt::Right));
        assert!(!option_is_alt(right, OptionAsAlt::Left));
        assert!(!option_is_alt(flags::SHIFT, OptionAsAlt::Both));
        // No device bits: the left key.
        assert!(option_is_alt(flags::OPTION, OptionAsAlt::Left));
    }

    #[test]
    fn modifiers() {
        let m = mods(
            flags::SHIFT | flags::CONTROL | flags::OPTION | flags::COMMAND,
            false,
        );
        assert_eq!(
            m,
            Mods {
                shift: true,
                ctrl: true,
                alt: false,
                meta: true
            }
        );
        assert!(mods(flags::OPTION, true).alt);
    }

    #[test]
    fn keys_by_code_and_layout() {
        assert_eq!(key(0x24, Some("\r")), Some(Key::Enter));
        assert_eq!(key(0x7E, Some("\u{F700}")), Some(Key::Up));
        assert_eq!(key(0x7A, Some("\u{F704}")), Some(Key::F(1)));
        assert_eq!(key(0x4C, Some("\u{3}")), Some(Key::Keypad('\r')));
        assert_eq!(key(0x53, Some("1")), Some(Key::Keypad('1')));
        // Character keys are named by the layout's unshifted character (kitty's key code).
        assert_eq!(key(0x00, Some("a")), Some(Key::Char('a')));
        assert_eq!(key(0x00, Some("q")), Some(Key::Char('q'))); // AZERTY's A position
        assert_eq!(key(0x31, Some(" ")), Some(Key::Char(' ')));
        assert_eq!(key(0x00, Some("A")), Some(Key::Char('a')));
        assert_eq!(key(0x00, None), None);
        assert_eq!(key(0x0A, Some("\u{F72C}")), None);
        assert!(is_modifier_key(0x38) && is_modifier_key(0x3A) && !is_modifier_key(0x00));
    }

    #[test]
    fn text_filter() {
        assert!(is_text("a") && is_text("é") && is_text("漢字"));
        assert!(!is_text("") && !is_text("\r") && !is_text("\u{1b}") && !is_text("\u{F700}"));
    }
}
