//! Encoding input for the program (§15.4, §19): keys, paste, focus and mouse, for the pane's
//! current modes (legacy xterm, application cursor/keypad, kitty keyboard flags, bracketed paste,
//! SGR mouse). Windows and the browser client both encode with this module.
//!
//! Keys follow xterm's encodings by default and the kitty keyboard protocol
//! (<https://sw.kovidgoyal.net/kitty/keyboard-protocol/>) when the program pushed flags: flag 1
//! (disambiguate), 4 (alternate keys), 8 (all keys as escape codes) and 16 (associated text).
//! Only presses are encoded, so flag 2 (event types) changes nothing.

use crate::{Modes, MouseMode};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    /// A key that produces a character. `Char` holds the unshifted base character of the key
    /// (kitty's key code); the text the layout produced goes in `KeyEvent::text`.
    Char(char),
    Enter,
    Tab,
    Backspace,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    /// F1–F24.
    F(u8),
    /// Keypad keys, by their base character (`0`–`9`, `.`, `+`, `-`, `*`, `/`, `=`), and Enter
    /// as `'\r'`.
    Keypad(char),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub meta: bool,
}

impl Mods {
    /// The xterm and kitty modifier bits (shift 1, alt 2, ctrl 4, meta/super 8).
    fn bits(self) -> u8 {
        u8::from(self.shift)
            | u8::from(self.alt) << 1
            | u8::from(self.ctrl) << 2
            | u8::from(self.meta) << 3
    }

    fn any(self) -> bool {
        self.bits() != 0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyEvent {
    pub key: Key,
    pub mods: Mods,
    /// The text the keyboard layout produced, if any (after Option composition when Option is
    /// not Alt).
    pub text: Option<String>,
}

const DISAMBIGUATE: u8 = 1;
const ALTERNATE_KEYS: u8 = 4;
const ALL_AS_ESCAPES: u8 = 8;
const ASSOCIATED_TEXT: u8 = 16;

/// The bytes a key press sends to the program under `modes`. Empty for keys that send nothing.
pub fn encode_key(ev: &KeyEvent, modes: &Modes) -> Vec<u8> {
    if modes.kitty_flags & (DISAMBIGUATE | ALL_AS_ESCAPES) != 0 {
        kitty_key(ev, modes)
    } else {
        legacy_key(ev, modes)
    }
}

/// How a non-text key is written: `CSI 1;m <final>` (or SS3 / `CSI <final>` unmodified), or
/// `CSI <n>;m ~`.
#[derive(Clone, Copy)]
enum Functional {
    Letter(u8),
    Tilde(u16),
}

fn functional(key: Key) -> Option<Functional> {
    use Functional::{Letter, Tilde};
    Some(match key {
        Key::Up => Letter(b'A'),
        Key::Down => Letter(b'B'),
        Key::Right => Letter(b'C'),
        Key::Left => Letter(b'D'),
        Key::Home => Letter(b'H'),
        Key::End => Letter(b'F'),
        Key::PageUp => Tilde(5),
        Key::PageDown => Tilde(6),
        Key::Insert => Tilde(2),
        Key::Delete => Tilde(3),
        Key::F(1) => Letter(b'P'),
        Key::F(2) => Letter(b'Q'),
        Key::F(3) => Letter(b'R'),
        Key::F(4) => Letter(b'S'),
        Key::F(n @ 5..=20) => Tilde(
            [
                15, 17, 18, 19, 20, 21, 23, 24, 25, 26, 28, 29, 31, 32, 33, 34,
            ][usize::from(n - 5)],
        ),
        _ => return None,
    })
}

fn csi(out: &mut Vec<u8>, body: &str) {
    out.extend_from_slice(b"\x1b[");
    out.extend_from_slice(body.as_bytes());
}

/// The control character Ctrl+`c` sends in legacy mode, if any.
fn ctrl_byte(c: char) -> Option<u8> {
    Some(match c.to_ascii_lowercase() {
        c @ 'a'..='z' => c as u8 - b'a' + 1,
        '@' | ' ' | '2' => 0,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' | '~' => 0x1e,
        '_' | '7' | '/' => 0x1f,
        '8' | '?' => 0x7f,
        _ => return None,
    })
}

fn legacy_key(ev: &KeyEvent, modes: &Modes) -> Vec<u8> {
    let mut out = Vec::new();
    let mods = ev.mods;
    let param = 1 + mods.bits();
    let alt_prefix = |out: &mut Vec<u8>| {
        if mods.alt {
            out.push(0x1b);
        }
    };
    if let Some(f) = functional(ev.key) {
        match f {
            Functional::Letter(c) if mods.any() => {
                csi(&mut out, &format!("1;{param}{}", c as char))
            }
            Functional::Letter(c) => {
                let ss3 = matches!(c, b'P'..=b'S') || modes.app_cursor;
                out.extend_from_slice(if ss3 { b"\x1bO" } else { b"\x1b[" });
                out.push(c);
            }
            Functional::Tilde(n) if mods.any() => csi(&mut out, &format!("{n};{param}~")),
            Functional::Tilde(n) => csi(&mut out, &format!("{n}~")),
        }
        return out;
    }
    match ev.key {
        Key::Enter => {
            alt_prefix(&mut out);
            out.extend_from_slice(if modes.newline { b"\r\n" } else { b"\r" });
        }
        Key::Tab if mods.shift => {
            alt_prefix(&mut out);
            out.extend_from_slice(b"\x1b[Z");
        }
        Key::Tab => {
            alt_prefix(&mut out);
            out.push(b'\t');
        }
        Key::Backspace => {
            alt_prefix(&mut out);
            out.push(if mods.ctrl { 0x08 } else { 0x7f });
        }
        Key::Escape => {
            alt_prefix(&mut out);
            out.push(0x1b);
        }
        Key::Keypad(c) => {
            if modes.app_keypad {
                let code = match c {
                    '0'..='9' => b'p' + (c as u8 - b'0'),
                    '.' => b'n',
                    '+' => b'k',
                    '-' => b'm',
                    '*' => b'j',
                    '/' => b'o',
                    '=' => b'X',
                    _ => b'M',
                };
                out.extend_from_slice(b"\x1bO");
                out.push(code);
            } else if c == '\r' {
                out.extend_from_slice(if modes.newline { b"\r\n" } else { b"\r" });
            } else {
                alt_prefix(&mut out);
                out.push(c as u8);
            }
        }
        Key::Char(c) => {
            if mods.ctrl
                && let Some(b) = ctrl_byte(c)
            {
                alt_prefix(&mut out);
                out.push(b);
                return out;
            }
            let text = match &ev.text {
                Some(t) => t.clone(),
                None if mods.shift => c.to_uppercase().collect(),
                None => c.to_string(),
            };
            if text.is_empty() {
                return out;
            }
            alt_prefix(&mut out);
            out.extend_from_slice(text.as_bytes());
        }
        _ => {}
    }
    out
}

/// The kitty key code of a key that isn't written as a functional key.
fn kitty_code(key: Key) -> Option<u32> {
    Some(match key {
        Key::Char(c) => u32::from(c),
        Key::Enter => 13,
        Key::Tab => 9,
        Key::Backspace => 127,
        Key::Escape => 27,
        Key::F(n @ 13..=24) => 57376 + u32::from(n - 13),
        Key::Keypad(c) => match c {
            '0'..='9' => 57399 + (c as u32 - '0' as u32),
            '.' => 57409,
            '/' => 57410,
            '*' => 57411,
            '-' => 57412,
            '+' => 57413,
            '=' => 57415,
            _ => 57414,
        },
        _ => return None,
    })
}

fn kitty_key(ev: &KeyEvent, modes: &Modes) -> Vec<u8> {
    let flags = modes.kitty_flags;
    let mods = ev.mods;
    let param = 1 + mods.bits();
    let all = flags & ALL_AS_ESCAPES != 0;
    let mut out = Vec::new();

    // Arrows, Home/End, PageUp/Down, Insert/Delete and F1–F12 keep their CSI forms.
    if let Some(f) = functional(ev.key) {
        match f {
            Functional::Letter(b'R') => csi(
                &mut out,
                &if mods.any() {
                    format!("13;{param}~")
                } else {
                    "13~".into()
                },
            ),
            Functional::Letter(c) if mods.any() => {
                csi(&mut out, &format!("1;{param}{}", c as char))
            }
            Functional::Letter(c) => csi(&mut out, &(c as char).to_string()),
            Functional::Tilde(n) if n >= 25 => {
                // F13–F20 have their own codes in kitty's table.
                let Key::F(k) = ev.key else {
                    unreachable!("only F keys have codes ≥ 25")
                };
                return kitty_csi_u(57376 + u32::from(k - 13), None, mods, None);
            }
            Functional::Tilde(n) if mods.any() => csi(&mut out, &format!("{n};{param}~")),
            Functional::Tilde(n) => csi(&mut out, &format!("{n}~")),
        }
        return out;
    }

    let Some(code) = kitty_code(ev.key) else {
        return out;
    };
    let text = match ev.key {
        Key::Char(c) => Some(ev.text.clone().unwrap_or_else(|| {
            if mods.shift {
                c.to_uppercase().collect()
            } else {
                c.to_string()
            }
        })),
        Key::Keypad(c) if c != '\r' => Some(c.to_string()),
        _ => None,
    };
    let text = text.filter(|t| !t.is_empty() && !t.chars().any(char::is_control));

    if !all {
        // Disambiguate only: keys that type text without Ctrl, Alt or Meta send the text, and
        // unmodified Enter, Tab and Backspace keep their legacy bytes.
        let plain = !(mods.ctrl || mods.alt || mods.meta);
        match ev.key {
            Key::Enter | Key::Tab | Key::Backspace if !mods.any() => return legacy_key(ev, modes),
            Key::Char(_) | Key::Keypad(_) if plain => {
                if let (Some(t), false) = (&text, matches!(ev.key, Key::Keypad('\r'))) {
                    out.extend_from_slice(t.as_bytes());
                    return out;
                }
            }
            _ => {}
        }
    }

    let alternate = match ev.key {
        Key::Char(c) if flags & ALTERNATE_KEYS != 0 && mods.shift => {
            // The shifted key: what the layout typed, else the uppercase letter.
            let typed = text.as_deref().and_then(|t| {
                let mut chars = t.chars();
                let first = chars.next()?;
                chars.next().is_none().then_some(first)
            });
            let upper = c
                .to_uppercase()
                .next()
                .filter(|_| c.to_uppercase().count() == 1);
            typed
                .filter(|&t| t != c)
                .or(upper)
                .map(u32::from)
                .filter(|&s| s != code)
        }
        _ => None,
    };
    let associated = if all && flags & ASSOCIATED_TEXT != 0 && !(mods.ctrl || mods.alt || mods.meta)
    {
        text
    } else {
        None
    };
    kitty_csi_u(code, alternate, mods, associated.as_deref())
}

/// `CSI code[:shifted] ; mods ; text u`, leaving out what is default.
fn kitty_csi_u(code: u32, shifted: Option<u32>, mods: Mods, text: Option<&str>) -> Vec<u8> {
    let mut body = code.to_string();
    if let Some(s) = shifted {
        body.push_str(&format!(":{s}"));
    }
    let param = 1 + mods.bits();
    if let Some(text) = text {
        let cps: Vec<String> = text.chars().map(|c| u32::from(c).to_string()).collect();
        body.push_str(&format!(";{param};{}", cps.join(":")));
    } else if param != 1 {
        body.push_str(&format!(";{param}"));
    }
    body.push('u');
    let mut out = Vec::new();
    csi(&mut out, &body);
    out
}

/// The bytes a paste sends: wrapped in `CSI 200~` / `CSI 201~` under bracketed paste, with the
/// end marker stripped from the text; otherwise the text with LF turned into CR.
pub fn encode_paste(text: &str, modes: &Modes) -> Vec<u8> {
    if modes.bracketed_paste {
        let mut body = text.to_owned();
        // Removing one marker can join the text around it into another; repeat until none.
        while body.contains("\x1b[201~") {
            body = body.replace("\x1b[201~", "");
        }
        let mut out = b"\x1b[200~".to_vec();
        out.extend_from_slice(body.as_bytes());
        out.extend_from_slice(b"\x1b[201~");
        out
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

/// Focus in/out reports (`CSI I` / `CSI O`) when focus reporting is on.
pub fn encode_focus(focused: bool, modes: &Modes) -> Vec<u8> {
    if !modes.focus_reports {
        return Vec::new();
    }
    if focused {
        b"\x1b[I".to_vec()
    } else {
        b"\x1b[O".to_vec()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseAction {
    Press,
    Release,
    Motion,
}

/// A mouse report for the cell at (`col`, `row`), 0-based, under the active mouse modes. Empty
/// when the modes don't ask for this event.
///
/// Mode 9 (X10) reports presses only, without modifiers; 1000 adds releases; 1002 adds motion
/// with a button held; 1003 adds all motion. Reports use SGR encoding (1006) when it is on, and
/// the legacy `CSI M` encoding otherwise, which can't name a cell past column or row 223.
pub fn encode_mouse(
    button: MouseButton,
    action: MouseAction,
    mods: Mods,
    col: u16,
    row: u16,
    modes: &Modes,
) -> Vec<u8> {
    let wheel = matches!(button, MouseButton::WheelUp | MouseButton::WheelDown);
    let wanted = match (modes.mouse, action) {
        (MouseMode::Off, _) => false,
        (_, MouseAction::Press) => button != MouseButton::None,
        (MouseMode::Press, _) => false,
        (_, MouseAction::Release) => !wheel,
        (MouseMode::PressRelease, MouseAction::Motion) => false,
        (MouseMode::ButtonMotion, MouseAction::Motion) => button != MouseButton::None,
        (MouseMode::AnyMotion, MouseAction::Motion) => true,
    };
    if !wanted {
        return Vec::new();
    }
    let mut code: u16 = match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
        MouseButton::None => 3,
        MouseButton::WheelUp => 64,
        MouseButton::WheelDown => 65,
    };
    if action == MouseAction::Motion {
        code += 32;
    }
    if modes.mouse != MouseMode::Press {
        code += u16::from(mods.shift) * 4 + u16::from(mods.alt) * 8 + u16::from(mods.ctrl) * 16;
    }
    let (x, y) = (u32::from(col) + 1, u32::from(row) + 1);
    if modes.mouse_sgr {
        let fin = if action == MouseAction::Release {
            'm'
        } else {
            'M'
        };
        return format!("\x1b[<{code};{x};{y}{fin}").into_bytes();
    }
    if action == MouseAction::Release {
        // Legacy releases don't say which button.
        code = (code & !3) | 3;
    }
    if x > 223 || y > 223 {
        return Vec::new();
    }
    vec![
        0x1b,
        b'[',
        b'M',
        32 + code as u8,
        32 + x as u8,
        32 + y as u8,
    ]
}
