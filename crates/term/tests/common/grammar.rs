//! A generator of realistic VT streams: text (with wide and combining characters), SGR, cursor
//! motion, erasing, scrolling, modes, charsets, tabs, links and titles, plus resizes.

use proptest::prelude::*;

/// One step of a generated session.
#[derive(Clone, Debug)]
pub enum Step {
    Bytes(Vec<u8>),
    Resize(u16, u16),
}

fn csi(body: &str) -> Vec<u8> {
    format!("\x1b[{body}").into_bytes()
}

fn text_token() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        8 => "[a-zA-Z0-9 .,:;!?_-]{1,24}".prop_map(String::into_bytes),
        2 => proptest::collection::vec(
            prop::sample::select(vec!["中", "文", "字", "한", "あ", "😀", "🎉", "é", "e\u{301}", "\u{301}", "ß", "─", "│", "ⅷ", "\u{200b}", "\u{7f}"]),
            1..8
        ).prop_map(|v| v.concat().into_bytes()),
    ]
}

fn color_param() -> impl Strategy<Value = String> {
    prop_oneof![
        (30u8..38).prop_map(|n| n.to_string()),
        (40u8..48).prop_map(|n| n.to_string()),
        (90u8..98).prop_map(|n| n.to_string()),
        (100u8..108).prop_map(|n| n.to_string()),
        any::<u8>().prop_map(|n| format!("38;5;{n}")),
        any::<u8>().prop_map(|n| format!("48;5;{n}")),
        (any::<u8>(), any::<u8>(), any::<u8>()).prop_map(|(r, g, b)| format!("38;2;{r};{g};{b}")),
        (any::<u8>(), any::<u8>(), any::<u8>()).prop_map(|(r, g, b)| format!("48:2::{r}:{g}:{b}")),
        any::<u8>().prop_map(|n| format!("58;5;{n}")),
        (any::<u8>(), any::<u8>(), any::<u8>()).prop_map(|(r, g, b)| format!("58:2:{r}:{g}:{b}")),
        Just("39".into()),
        Just("49".into()),
        Just("59".into()),
    ]
}

fn sgr_token() -> impl Strategy<Value = Vec<u8>> {
    let attr = prop_oneof![
        prop::sample::select(vec!["0", "1", "2", "3", "4", "4:0", "4:2", "4:3", "4:4", "4:5", "5", "7", "8", "9", "22", "23", "24", "25", "27", "28", "29", "53", "55", ""]).prop_map(str::to_owned),
        color_param(),
    ];
    proptest::collection::vec(attr, 1..4).prop_map(|v| csi(&format!("{}m", v.join(";"))))
}

fn n() -> impl Strategy<Value = u16> {
    prop_oneof![3 => 0u16..4, 2 => 0u16..30, 1 => Just(1000u16)]
}

fn control_token() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        prop::sample::select(vec![
            "\r", "\n", "\r\n", "\x08", "\t", "\x0b", "\x0c", "\x07", "\x0e", "\x0f",
            "\x1b7", "\x1b8", "\x1bD", "\x1bE", "\x1bH", "\x1bM", "\x1b(0", "\x1b(B", "\x1b)0",
            "\x1b[s", "\x1b[u", "\x1b=", "\x1b>", "\x1b#8", "\x1bc", "\x1b[g", "\x1b[3g",
            "\x1b[?7l", "\x1b[?7h", "\x1b[4h", "\x1b[4l", "\x1b[?6h", "\x1b[?6l", "\x1b[?1049h",
            "\x1b[?1049l", "\x1b[?25l", "\x1b[?25h", "\x1b[?1h", "\x1b[?1l", "\x1b[?2004h",
            "\x1b[?1006h", "\x1b[r", "\x1b[?3h",
            "\x1b]8;;https://example.com/a\x1b\\", "\x1b]8;id=x;https://example.com/b\x07",
            "\x1b]8;;\x1b\\", "\x1b]2;title\x07", "\x1b]0;both\x1b\\",
        ])
        .prop_map(|s| s.as_bytes().to_vec()),
        (n(), n()).prop_map(|(a, b)| csi(&format!("{a};{b}H"))),
        (n(), n()).prop_map(|(a, b)| csi(&format!("{a};{b}f"))),
        (n(), prop::sample::select(vec!['A', 'B', 'C', 'D', 'E', 'F', 'G', '`', 'a', 'd', 'e', '@', 'P', 'X', 'L', 'M', 'S', 'T', 'b', 'I', 'Z']))
            .prop_map(|(a, f)| csi(&format!("{a}{f}"))),
        (0u16..4, prop::sample::select(vec!['J', 'K'])).prop_map(|(a, f)| csi(&format!("{a}{f}"))),
        (n(), n()).prop_map(|(a, b)| csi(&format!("{a};{b}r"))),
    ]
}

pub fn token() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![5 => text_token(), 2 => sgr_token(), 5 => control_token()]
}

pub fn steps() -> impl Strategy<Value = Vec<Step>> {
    let step = prop_oneof![
        30 => token().prop_map(Step::Bytes),
        1 => (2u16..24, 1u16..10).prop_map(|(c, r)| Step::Resize(c, r)),
    ];
    proptest::collection::vec(step, 1..80)
}

