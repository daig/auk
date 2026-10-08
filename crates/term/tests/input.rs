//! Input encoding (§15.4, §19), checked by decoding with the Tern SDK's key decoder: what a
//! program built on the SDK reads must be the key that was pressed.

// Uses dev-dependencies that don't build for wasm32 (see Cargo.toml).
#![cfg(not(target_family = "wasm"))]

use auk_term::input::{
    Key, KeyEvent, Mods, MouseAction, MouseButton, encode_focus, encode_key, encode_mouse,
    encode_paste,
};
use auk_term::{Modes, MouseMode};
use tern_sdk::keys::{Decoder, Key as SdkKey};

fn decode(bytes: &[u8]) -> Vec<SdkKey> {
    let mut d = Decoder::new();
    let mut keys = d.feed(bytes);
    keys.extend(d.flush());
    keys
}

/// What a US keyboard types for `c` with Shift.
fn shifted(c: char) -> char {
    let (plain, shifted) = ("`1234567890-=[]\\;',./", "~!@#$%^&*()_+{}|:\"<>?");
    match plain.chars().position(|p| p == c) {
        Some(i) => shifted.chars().nth(i).expect("same length"),
        None => c.to_ascii_uppercase(),
    }
}

/// A key event as a window on a US layout reports it.
fn ev(key: Key, mods: Mods) -> KeyEvent {
    let text = match key {
        Key::Char(c) if !(mods.ctrl || mods.alt || mods.meta) => Some(if mods.shift {
            shifted(c).to_string()
        } else {
            c.to_string()
        }),
        Key::Char(c) => Some(c.to_string()),
        _ => None,
    };
    KeyEvent { key, mods, text }
}

/// Whether the key is sent as its text under kitty flag 1 (so the SDK reads the text).
fn sent_as_text(key: Key, m: Mods) -> bool {
    !(m.ctrl || m.alt || m.meta)
        && matches!(
            key,
            Key::Char(_) | Key::Keypad('0'..='9' | '.' | '+' | '-' | '*' | '/' | '=')
        )
}

fn mods(bits: u8) -> Mods {
    Mods {
        shift: bits & 1 != 0,
        alt: bits & 2 != 0,
        ctrl: bits & 4 != 0,
        meta: bits & 8 != 0,
    }
}

/// The SDK's name for a key.
fn name(key: Key) -> String {
    match key {
        Key::Char(' ') => "space".into(),
        Key::Char(c) => c.to_lowercase().collect(),
        Key::Enter | Key::Keypad('\r') => "enter".into(),
        Key::Tab => "tab".into(),
        Key::Backspace => "backspace".into(),
        Key::Escape => "escape".into(),
        Key::Up => "up".into(),
        Key::Down => "down".into(),
        Key::Left => "left".into(),
        Key::Right => "right".into(),
        Key::Home => "home".into(),
        Key::End => "end".into(),
        Key::PageUp => "page_up".into(),
        Key::PageDown => "page_down".into(),
        Key::Insert => "insert".into(),
        Key::Delete => "delete".into(),
        Key::F(n) => format!("f{n}"),
        Key::Keypad(c) => c.to_string(),
    }
}

fn all_keys() -> Vec<Key> {
    let mut keys = vec![
        Key::Enter,
        Key::Tab,
        Key::Backspace,
        Key::Escape,
        Key::Up,
        Key::Down,
        Key::Left,
        Key::Right,
        Key::Home,
        Key::End,
        Key::PageUp,
        Key::PageDown,
        Key::Insert,
        Key::Delete,
    ];
    keys.extend((1..=20).map(Key::F));
    keys.extend("az09`-=[];',./\\ ".chars().map(Key::Char));
    keys.extend("0123456789.+-*/=\r".chars().map(Key::Keypad));
    keys
}

fn kitty(flags: u8) -> Modes {
    Modes {
        kitty_flags: flags,
        ..Modes::default()
    }
}

#[test]
fn kitty_flag_1_round_trips_every_key_and_modifier() {
    for key in all_keys() {
        for bits in 0..16 {
            let m = mods(bits);
            let bytes = encode_key(&ev(key, m), &kitty(1));
            let keys = decode(&bytes);
            assert_eq!(keys.len(), 1, "{key:?} {m:?} -> {bytes:?} -> {keys:?}");
            let got = &keys[0];
            let (expect_name, expect_shift) = match (key, sent_as_text(key, m)) {
                // The SDK names a typed character by its lowercase form, with Shift if that
                // changed it.
                (Key::Char(c), true) if m.shift => {
                    let s = shifted(c);
                    (
                        if s == ' ' {
                            "space".into()
                        } else {
                            s.to_lowercase().collect()
                        },
                        s != s.to_ascii_lowercase(),
                    )
                }
                (_, true) => (name(key), false),
                _ => (name(key), m.shift),
            };
            assert_eq!(got.name, expect_name, "{key:?} {m:?} -> {bytes:?}");
            assert_eq!(
                (got.shift, got.alt, got.ctrl, got.meta),
                (expect_shift, m.alt, m.ctrl, m.meta),
                "{key:?} {m:?} -> {bytes:?} -> {got:?}"
            );
        }
    }
}

#[test]
fn kitty_flag_1_sends_text_for_plain_keys_and_csi_u_otherwise() {
    let m = kitty(1);
    assert_eq!(encode_key(&ev(Key::Char('a'), Mods::default()), &m), b"a");
    assert_eq!(encode_key(&ev(Key::Char('a'), mods(1)), &m), b"A");
    assert_eq!(encode_key(&ev(Key::Char('c'), mods(4)), &m), b"\x1b[99;5u");
    assert_eq!(encode_key(&ev(Key::Char('a'), mods(2)), &m), b"\x1b[97;3u");
    assert_eq!(
        encode_key(&ev(Key::Escape, Mods::default()), &m),
        b"\x1b[27u"
    );
    // Enter, Tab and Backspace keep their legacy bytes when unmodified.
    assert_eq!(encode_key(&ev(Key::Enter, Mods::default()), &m), b"\r");
    assert_eq!(encode_key(&ev(Key::Tab, Mods::default()), &m), b"\t");
    assert_eq!(
        encode_key(&ev(Key::Backspace, Mods::default()), &m),
        b"\x7f"
    );
    assert_eq!(encode_key(&ev(Key::Enter, mods(1)), &m), b"\x1b[13;2u");
    assert_eq!(encode_key(&ev(Key::Up, Mods::default()), &m), b"\x1b[A");
    assert_eq!(encode_key(&ev(Key::Up, mods(4)), &m), b"\x1b[1;5A");
    assert_eq!(encode_key(&ev(Key::F(3), Mods::default()), &m), b"\x1b[13~");
    assert_eq!(
        encode_key(&ev(Key::F(13), Mods::default()), &m),
        b"\x1b[57376u"
    );
    assert_eq!(
        encode_key(&ev(Key::Keypad('\r'), Mods::default()), &m),
        b"\x1b[57414u"
    );
}

#[test]
fn kitty_other_flags() {
    // 8: every key as an escape code; 16 adds the text.
    assert_eq!(
        encode_key(&ev(Key::Char('a'), Mods::default()), &kitty(8)),
        b"\x1b[97u"
    );
    assert_eq!(
        encode_key(&ev(Key::Enter, Mods::default()), &kitty(8)),
        b"\x1b[13u"
    );
    assert_eq!(
        encode_key(&ev(Key::Char('a'), mods(1)), &kitty(8 | 16)),
        b"\x1b[97;2;65u"
    );
    // 4: the shifted key as an alternate.
    assert_eq!(
        encode_key(&ev(Key::Char('a'), mods(1 | 4)), &kitty(1 | 4)),
        b"\x1b[97:65;6u"
    );
    for flags in [8, 8 | 16, 1 | 4, 1 | 2, 31] {
        for key in [
            Key::Char('x'),
            Key::Enter,
            Key::Escape,
            Key::F(5),
            Key::Keypad('7'),
        ] {
            for bits in 0..16 {
                let m = mods(bits);
                let bytes = encode_key(&ev(key, m), &kitty(flags));
                let keys = decode(&bytes);
                assert_eq!(keys.len(), 1, "{flags} {key:?} {m:?} -> {bytes:?}");
                assert_eq!(
                    keys[0].name,
                    name(key),
                    "{flags} {key:?} {m:?} -> {bytes:?}"
                );
                assert_eq!(
                    (keys[0].ctrl, keys[0].alt, keys[0].meta),
                    (m.ctrl, m.alt, m.meta)
                );
            }
        }
    }
}

#[test]
fn legacy_keys_decode() {
    let legacy = Modes::default();
    let cases: Vec<(Key, Mods, &str)> = vec![
        (Key::Char('a'), Mods::default(), "a"),
        (Key::Char('a'), mods(1), "shift+a"),
        (Key::Char('a'), mods(4), "ctrl+a"),
        (Key::Char('a'), mods(2), "alt+a"),
        (Key::Char('a'), mods(6), "ctrl+alt+a"),
        (Key::Char(' '), mods(4), "ctrl+space"),
        (Key::Char('['), mods(4), "escape"),
        (Key::Enter, Mods::default(), "enter"),
        (Key::Enter, mods(2), "alt+enter"),
        (Key::Tab, mods(1), "shift+tab"),
        (Key::Backspace, Mods::default(), "backspace"),
        (Key::Backspace, mods(4), "ctrl+backspace"),
        (Key::Backspace, mods(2), "alt+backspace"),
        (Key::Escape, Mods::default(), "escape"),
        (Key::Up, Mods::default(), "up"),
        (Key::Left, mods(4), "ctrl+left"),
        (Key::Right, mods(1 | 2), "alt+shift+right"),
        (Key::Home, Mods::default(), "home"),
        (Key::End, mods(4), "ctrl+end"),
        (Key::PageUp, Mods::default(), "page_up"),
        (Key::Delete, mods(1), "shift+delete"),
        (Key::Insert, Mods::default(), "insert"),
        (Key::F(1), Mods::default(), "f1"),
        (Key::F(4), mods(4), "ctrl+f4"),
        (Key::F(5), Mods::default(), "f5"),
        (Key::F(12), mods(1), "shift+f12"),
        (Key::Keypad('5'), Mods::default(), "5"),
    ];
    for (key, m, chord) in cases {
        let bytes = encode_key(&ev(key, m), &legacy);
        let keys = decode(&bytes);
        assert_eq!(keys.len(), 1, "{key:?} {m:?} -> {bytes:?}");
        assert!(
            keys[0].is(chord),
            "{key:?} {m:?} -> {bytes:?} -> {} (want {chord})",
            keys[0]
        );
    }
}

#[test]
fn application_cursor_and_keypad() {
    let m = Modes {
        app_cursor: true,
        app_keypad: true,
        ..Modes::default()
    };
    assert_eq!(encode_key(&ev(Key::Up, Mods::default()), &m), b"\x1bOA");
    assert_eq!(encode_key(&ev(Key::Home, Mods::default()), &m), b"\x1bOH");
    assert_eq!(encode_key(&ev(Key::Up, mods(4)), &m), b"\x1b[1;5A");
    assert_eq!(
        encode_key(&ev(Key::Keypad('1'), Mods::default()), &m),
        b"\x1bOq"
    );
    assert_eq!(
        encode_key(&ev(Key::Keypad('\r'), Mods::default()), &m),
        b"\x1bOM"
    );
    assert!(decode(b"\x1bOM")[0].is("enter"));
    let plain = Modes::default();
    assert_eq!(encode_key(&ev(Key::Up, Mods::default()), &plain), b"\x1b[A");
    let lnm = Modes {
        newline: true,
        ..Modes::default()
    };
    assert_eq!(encode_key(&ev(Key::Enter, Mods::default()), &lnm), b"\r\n");
}

#[test]
fn composed_text_is_sent_as_is() {
    let e = KeyEvent {
        key: Key::Char('e'),
        mods: Mods::default(),
        text: Some("é".into()),
    };
    assert_eq!(encode_key(&e, &Modes::default()), "é".as_bytes());
    assert_eq!(encode_key(&e, &kitty(1)), "é".as_bytes());
    let keys = decode("é".as_bytes());
    assert_eq!(keys[0].text.as_deref(), Some("é"));
}

#[test]
fn paste() {
    let bracketed = Modes {
        bracketed_paste: true,
        ..Modes::default()
    };
    let bytes = encode_paste("one\ntwo \x1b[20\x1b[201~1~ three", &bracketed);
    let keys = decode(&bytes);
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].name, "paste");
    assert_eq!(keys[0].text.as_deref(), Some("one\ntwo  three"));
    assert_eq!(encode_paste("a\nb\r\nc", &Modes::default()), b"a\rb\rc");
}

#[test]
fn focus() {
    let on = Modes {
        focus_reports: true,
        ..Modes::default()
    };
    assert_eq!(encode_focus(true, &on), b"\x1b[I");
    assert_eq!(encode_focus(false, &on), b"\x1b[O");
    assert!(encode_focus(true, &Modes::default()).is_empty());
}

#[test]
fn mouse_reports_follow_modes() {
    use MouseAction::{Motion, Press, Release};
    use MouseButton::{Left, None as NoButton, Right, WheelUp};
    let modes = |mouse, sgr| Modes {
        mouse,
        mouse_sgr: sgr,
        ..Modes::default()
    };
    let shift_ctrl = Mods {
        shift: true,
        ctrl: true,
        ..Mods::default()
    };

    assert!(
        encode_mouse(
            Left,
            Press,
            Mods::default(),
            0,
            0,
            &modes(MouseMode::Off, true)
        )
        .is_empty()
    );

    let sgr = modes(MouseMode::PressRelease, true);
    assert_eq!(
        encode_mouse(Left, Press, Mods::default(), 4, 9, &sgr),
        b"\x1b[<0;5;10M"
    );
    assert_eq!(
        encode_mouse(Right, Release, Mods::default(), 4, 9, &sgr),
        b"\x1b[<2;5;10m"
    );
    assert_eq!(
        encode_mouse(WheelUp, Press, shift_ctrl, 0, 0, &sgr),
        b"\x1b[<84;1;1M"
    );
    assert!(encode_mouse(Left, Motion, Mods::default(), 1, 1, &sgr).is_empty());
    assert!(encode_mouse(WheelUp, Release, Mods::default(), 1, 1, &sgr).is_empty());

    let drag = modes(MouseMode::ButtonMotion, true);
    assert_eq!(
        encode_mouse(Left, Motion, Mods::default(), 2, 3, &drag),
        b"\x1b[<32;3;4M"
    );
    assert!(encode_mouse(NoButton, Motion, Mods::default(), 2, 3, &drag).is_empty());

    let any = modes(MouseMode::AnyMotion, true);
    assert_eq!(
        encode_mouse(NoButton, Motion, Mods::default(), 2, 3, &any),
        b"\x1b[<35;3;4M"
    );

    let legacy = modes(MouseMode::PressRelease, false);
    assert_eq!(
        encode_mouse(Left, Press, Mods::default(), 0, 0, &legacy),
        b"\x1b[M !!"
    );
    assert_eq!(
        encode_mouse(Left, Release, Mods::default(), 0, 0, &legacy),
        b"\x1b[M#!!"
    );
    assert_eq!(
        encode_mouse(Left, Press, Mods::default(), 222, 0, &legacy),
        b"\x1b[M \xff!"
    );
    assert!(encode_mouse(Left, Press, Mods::default(), 223, 0, &legacy).is_empty());

    let x10 = modes(MouseMode::Press, false);
    assert_eq!(
        encode_mouse(Left, Press, shift_ctrl, 0, 0, &x10),
        b"\x1b[M !!",
        "X10 drops modifiers"
    );
    assert!(encode_mouse(Left, Release, Mods::default(), 0, 0, &x10).is_empty());
}

#[test]
fn sdk_ignores_mouse_reports_as_keys() {
    // A program that didn't ask for mouse reports never sees them as keys.
    let sgr = Modes {
        mouse: MouseMode::PressRelease,
        mouse_sgr: true,
        ..Modes::default()
    };
    let bytes = encode_mouse(
        MouseButton::Left,
        MouseAction::Press,
        Mods::default(),
        1,
        1,
        &sgr,
    );
    assert!(decode(&bytes).is_empty());
}
