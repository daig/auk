//! DOM keyboard events to the terminal's key model: the browser client encodes keys for
//! the pane's current modes with `auk_term::input`, exactly as the app does.
//!
//! Plain typing, Option compositions and Ctrl chords become key events. Dead keys and input
//! methods are left to the page's text field, whose committed text is sent as typed text. ⌘
//! chords stay with the browser (paste, copy, reload).

use auk_term::input::{Key, KeyEvent, Mods};

/// The parts of a DOM `KeyboardEvent` the mapping reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DomKey<'a> {
    /// `KeyboardEvent.key`: the character typed, or the key's name.
    pub key: &'a str,
    /// `KeyboardEvent.code`: the physical key.
    pub code: &'a str,
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub meta: bool,
    /// `isComposing`, or `keyCode` 229: an input method has the key.
    pub composing: bool,
}

/// What to do with a key-down.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyAction {
    /// Encode and send this key; the browser's default action is suppressed.
    Send(KeyEvent),
    /// Let the text field have it (a dead key or an input method); its committed text is sent.
    Compose,
    /// Not for the terminal: modifiers alone, and ⌘ chords the browser handles.
    Ignore,
}

/// Maps a key-down. `option_as_alt` makes Option send Alt (ESC-prefixed) instead of composing.
pub fn map_key(k: &DomKey<'_>, option_as_alt: bool) -> KeyAction {
    if k.composing || matches!(k.key, "Dead" | "Process" | "Unidentified") {
        return KeyAction::Compose;
    }
    if k.meta {
        return KeyAction::Ignore;
    }
    let mods = Mods {
        shift: k.shift,
        ctrl: k.ctrl,
        alt: k.alt,
        meta: false,
    };
    if let Some(key) = keypad(k) {
        let text = match key {
            Key::Keypad('\r') => None,
            _ => single_char(k.key).map(String::from),
        };
        return KeyAction::Send(KeyEvent { key, mods, text });
    }
    if let Some(key) = named(k.key) {
        return KeyAction::Send(KeyEvent {
            key,
            mods,
            text: None,
        });
    }
    let Some(typed) = single_char(k.key) else {
        return KeyAction::Ignore;
    };
    let base = base_char(k.code).unwrap_or_else(|| typed.to_lowercase().next().unwrap_or(typed));
    let key = Key::Char(base);
    if k.ctrl {
        return KeyAction::Send(KeyEvent {
            key,
            mods,
            text: None,
        });
    }
    if k.alt && option_as_alt {
        // The text without Option: the base character, shifted.
        let unshifted = if k.shift {
            shifted(base)
        } else {
            base.to_string()
        };
        return KeyAction::Send(KeyEvent {
            key,
            mods,
            text: Some(unshifted),
        });
    }
    // Plain typing, or an Option composition (Option isn't Alt): the typed character.
    KeyAction::Send(KeyEvent {
        key,
        mods: Mods { alt: false, ..mods },
        text: Some(typed.to_string()),
    })
}

fn single_char(s: &str) -> Option<char> {
    let mut chars = s.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if !c.is_control() => Some(c),
        _ => None,
    }
}

fn named(key: &str) -> Option<Key> {
    Some(match key {
        "Enter" => Key::Enter,
        "Tab" => Key::Tab,
        "Backspace" => Key::Backspace,
        "Escape" | "Esc" => Key::Escape,
        "ArrowUp" | "Up" => Key::Up,
        "ArrowDown" | "Down" => Key::Down,
        "ArrowLeft" | "Left" => Key::Left,
        "ArrowRight" | "Right" => Key::Right,
        "Home" => Key::Home,
        "End" => Key::End,
        "PageUp" => Key::PageUp,
        "PageDown" => Key::PageDown,
        "Insert" => Key::Insert,
        "Delete" | "Del" => Key::Delete,
        f if f.len() >= 2 && f.starts_with('F') => {
            let n: u8 = f[1..].parse().ok()?;
            if !(1..=24).contains(&n) {
                return None;
            }
            Key::F(n)
        }
        _ => return None,
    })
}

/// Numeric keypad keys, when they type (Num Lock on; navigation otherwise).
fn keypad(k: &DomKey<'_>) -> Option<Key> {
    let rest = k.code.strip_prefix("Numpad")?;
    if rest == "Enter" {
        return Some(Key::Keypad('\r'));
    }
    single_char(k.key)?;
    Some(Key::Keypad(match rest {
        "Decimal" => '.',
        "Add" => '+',
        "Subtract" => '-',
        "Multiply" => '*',
        "Divide" => '/',
        "Equal" => '=',
        d if d.len() == 1 && d.as_bytes()[0].is_ascii_digit() => d.as_bytes()[0] as char,
        _ => return None,
    }))
}

/// The unshifted character of a physical key on a US layout (kitty's base key), from
/// `KeyboardEvent.code`.
fn base_char(code: &str) -> Option<char> {
    if let Some(letter) = code.strip_prefix("Key")
        && letter.len() == 1
    {
        return Some(letter.as_bytes()[0].to_ascii_lowercase() as char);
    }
    if let Some(digit) = code.strip_prefix("Digit")
        && digit.len() == 1
    {
        return Some(digit.as_bytes()[0] as char);
    }
    Some(match code {
        "Space" => ' ',
        "Minus" => '-',
        "Equal" => '=',
        "BracketLeft" => '[',
        "BracketRight" => ']',
        "Backslash" => '\\',
        "Semicolon" => ';',
        "Quote" => '\'',
        "Backquote" => '`',
        "Comma" => ',',
        "Period" => '.',
        "Slash" => '/',
        "IntlBackslash" => '§',
        _ => return None,
    })
}

/// A US layout's shifted character for a base character.
fn shifted(c: char) -> String {
    let s = match c {
        '1' => '!',
        '2' => '@',
        '3' => '#',
        '4' => '$',
        '5' => '%',
        '6' => '^',
        '7' => '&',
        '8' => '*',
        '9' => '(',
        '0' => ')',
        '-' => '_',
        '=' => '+',
        '[' => '{',
        ']' => '}',
        '\\' => '|',
        ';' => ':',
        '\'' => '"',
        '`' => '~',
        ',' => '<',
        '.' => '>',
        '/' => '?',
        c => return c.to_uppercase().collect(),
    };
    s.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use auk_term::Modes;
    use auk_term::input::encode_key;

    fn dom<'a>(key: &'a str, code: &'a str) -> DomKey<'a> {
        DomKey {
            key,
            code,
            ..DomKey::default()
        }
    }

    fn bytes(k: DomKey<'_>, modes: &Modes) -> Vec<u8> {
        match map_key(&k, false) {
            KeyAction::Send(ev) => encode_key(&ev, modes),
            other => panic!("{k:?} gave {other:?}"),
        }
    }

    #[test]
    fn plain_and_shifted_typing_sends_the_text() {
        let m = Modes::default();
        assert_eq!(bytes(dom("a", "KeyA"), &m), b"a");
        let shifted = DomKey {
            shift: true,
            ..dom("A", "KeyA")
        };
        assert_eq!(bytes(shifted, &m), b"A");
        assert_eq!(bytes(dom(" ", "Space"), &m), b" ");
        assert_eq!(bytes(dom("é", "KeyE"), &m), "é".as_bytes());
    }

    #[test]
    fn ctrl_chords_and_named_keys() {
        let m = Modes::default();
        let ctrl_c = DomKey {
            ctrl: true,
            ..dom("c", "KeyC")
        };
        assert_eq!(bytes(ctrl_c, &m), b"\x03");
        assert_eq!(bytes(dom("Enter", "Enter"), &m), b"\r");
        assert_eq!(bytes(dom("Backspace", "Backspace"), &m), b"\x7f");
        assert_eq!(bytes(dom("ArrowUp", "ArrowUp"), &m), b"\x1b[A");
        assert_eq!(bytes(dom("F5", "F5"), &m), b"\x1b[15~");
    }

    #[test]
    fn option_composes_unless_it_is_alt() {
        let m = Modes::default();
        let opt = DomKey {
            alt: true,
            ..dom("å", "KeyA")
        };
        assert_eq!(bytes(opt, &m), "å".as_bytes());
        match map_key(&opt, true) {
            KeyAction::Send(ev) => assert_eq!(encode_key(&ev, &m), b"\x1ba"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn dead_keys_ime_and_command_chords_are_not_sent() {
        assert_eq!(map_key(&dom("Dead", "KeyE"), false), KeyAction::Compose);
        let ime = DomKey {
            composing: true,
            ..dom("k", "KeyK")
        };
        assert_eq!(map_key(&ime, false), KeyAction::Compose);
        let paste = DomKey {
            meta: true,
            ..dom("v", "KeyV")
        };
        assert_eq!(map_key(&paste, false), KeyAction::Ignore);
        assert_eq!(
            map_key(&dom("Shift", "ShiftLeft"), false),
            KeyAction::Ignore
        );
    }

    #[test]
    fn keypad_and_kitty_disambiguation() {
        let m = Modes::default();
        assert_eq!(bytes(dom("5", "Numpad5"), &m), b"5");
        let kitty = Modes {
            kitty_flags: 1,
            ..Modes::default()
        };
        assert_eq!(bytes(dom("Escape", "Escape"), &kitty), b"\x1b[27u");
    }
}
