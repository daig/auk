//! Snapshots (§15.3) and determinism (§15.2): a snapshot replayed on a fresh terminal of the same
//! size and configuration gives the same state hash, anchors included, and the two stay equal as
//! the same input continues. Streams come from raw bytes biased toward escape sequences (so the
//! parser is often caught mid-sequence) and from the realistic grammar, mixed with records and
//! anchor operations.

// Uses dev-dependencies that don't build for wasm32 (see Cargo.toml).
#![cfg(not(target_family = "wasm"))]

use auk_term::{AnchorKey, Config, Terminal};
use proptest::prelude::*;

mod common;
use common::grammar;

/// Something that happens to a pane, in stream order.
#[derive(Clone, Debug)]
enum Op {
    Bytes(Vec<u8>),
    Resize(u16, u16),
    ClearScrollback,
    Reset,
    Scrollback(usize),
    OpenAnchor(u64),
    UndoAnchor(u64),
    DropAnchor(u64),
}

fn apply(t: &mut Terminal, op: &Op) {
    match op {
        Op::Bytes(b) => {
            t.feed(b);
        }
        Op::Resize(c, r) => t.resize(*c, *r),
        Op::ClearScrollback => t.clear_scrollback(),
        Op::Reset => t.reset(),
        Op::Scrollback(n) => t.set_config(Config { scrollback: *n }),
        Op::OpenAnchor(k) => {
            let _ = t.open_anchor(AnchorKey(*k));
        }
        Op::UndoAnchor(k) => {
            t.undo_anchor(AnchorKey(*k));
        }
        Op::DropAnchor(k) => t.drop_anchor(AnchorKey(*k)),
    }
    t.take_events();
}

/// Bytes biased toward the parser's interesting paths.
fn soup() -> impl Strategy<Value = Vec<u8>> {
    let byte = prop_oneof![
        4 => prop::sample::select(b"\x1b[]_P^X;:?>=<!$ \"'0123456789mHJKhlrstuqcbn@ABCDGLMPSTXZ\\".to_vec()),
        2 => prop::sample::select(vec![0x07u8, 0x08, 0x09, 0x0a, 0x0d, 0x0e, 0x0f, 0x18, 0x1a, 0x7f, 0x9c, 0x9b, 0xc2, 0xe4, 0xb8, 0xad, 0xf0, 0x9f, 0x98, 0x80, 0xff]),
        2 => b'a'..=b'z',
        1 => any::<u8>(),
    ];
    prop::collection::vec(byte, 0..120)
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        10 => soup().prop_map(Op::Bytes),
        10 => grammar::token().prop_map(Op::Bytes),
        2 => (2u16..16, 1u16..8).prop_map(|(c, r)| Op::Resize(c, r)),
        1 => Just(Op::ClearScrollback),
        1 => Just(Op::Reset),
        1 => (0usize..12).prop_map(Op::Scrollback),
        3 => (0u64..4).prop_map(Op::OpenAnchor),
        1 => (0u64..4).prop_map(Op::UndoAnchor),
        1 => (0u64..4).prop_map(Op::DropAnchor),
        1 => Just(Op::Bytes(b"\x1b_tsp;hello\x1b\\\x1b_other\x07".to_vec())),
        1 => Just(Op::Bytes(b"\x1b_tsp;partial".to_vec())),
        1 => Just(Op::Bytes(b"\x1b]6464;anchor;9\x1b\\".to_vec())),
    ]
}

fn check(
    cols: u16,
    rows: u16,
    scrollback: usize,
    ops: &[Op],
    suffix: &[Op],
) -> Result<(), TestCaseError> {
    let config = Config { scrollback };
    let mut t = Terminal::new(cols, rows, config.clone());
    for op in ops {
        apply(&mut t, op);
    }
    let snapshot = t.snapshot();
    let mut u = Terminal::new(
        t.cols(),
        t.rows(),
        Config {
            scrollback: t_config_scrollback(ops, scrollback),
        },
    );
    u.feed(&snapshot);
    u.take_events();
    prop_assert_eq!(
        t.state_hash(),
        u.state_hash(),
        "snapshot {:?}",
        String::from_utf8_lossy(&snapshot)
    );
    for (i, op) in suffix.iter().enumerate() {
        apply(&mut t, op);
        apply(&mut u, op);
        prop_assert_eq!(
            t.state_hash(),
            u.state_hash(),
            "after suffix op {} {:?}",
            i,
            op
        );
    }
    Ok(())
}

/// The scrollback setting in force after `ops` (records change it).
fn t_config_scrollback(ops: &[Op], initial: usize) -> usize {
    ops.iter().fold(
        initial,
        |n, op| if let Op::Scrollback(s) = op { *s } else { n },
    )
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 10000, failure_persistence: None, ..ProptestConfig::default() })]

    #[test]
    fn snapshot_rebuilds_equal_state(
        cols in 2u16..16,
        rows in 1u16..8,
        scrollback in 0usize..12,
        ops in prop::collection::vec(op(), 0..40),
        suffix in prop::collection::vec(op(), 0..12),
    ) {
        check(cols, rows, scrollback, &ops, &suffix)?;
    }

    #[test]
    fn same_input_same_hash(ops in prop::collection::vec(op(), 0..40)) {
        let mut a = Terminal::new(10, 4, Config { scrollback: 5 });
        let mut b = Terminal::new(10, 4, Config { scrollback: 5 });
        for op in &ops {
            apply(&mut a, op);
        }
        // The same bytes split differently.
        for op in &ops {
            match op {
                Op::Bytes(bytes) => {
                    for chunk in bytes.chunks(3) {
                        b.feed(chunk);
                    }
                    b.take_events();
                }
                other => apply(&mut b, other),
            }
        }
        prop_assert_eq!(a.state_hash(), b.state_hash());
    }
}

/// Streams of whole and cut sequences: CSI with parameters and subparameters, OSC (BEL and ST
/// ended), DCS, SOS/PM, TSP and other APC strings, UTF-8 split anywhere.
fn stream() -> impl Strategy<Value = Vec<u8>> {
    let pieces: Vec<Vec<u8>> = [
        &b"\x1b[38;2;1;2;3m"[..],
        b"\x1b[4:3;58:2::9:8:7m",
        b"\x1b[?1049h",
        b"\x1b[?1049l",
        b"\x1b[2;4r",
        b"\x1b[3;5H",
        b"\x1b[1 q",
        b"\x1b[>1u",
        b"\x1b[;;;:1:2;99999999;?h",
        b"\x1b(0q\x1b(B",
        b"\x1b \x21\x22F",
        b"\x1b]2;a title\x07",
        b"\x1b]8;id=1;http://x\x1b\\link\x1b]8;;\x1b\\",
        b"\x1b]4;1;rgb:12/34/56\x1b\\",
        b"\x1b]133;A\x07",
        b"\x1bP1;2$qm\x1b\\",
        b"\x1bXsos\x1b\\",
        b"\x1b^pm\x1b\\",
        b"\x1b_tsp;o;{\"id\":\"s\"}\x1b\\",
        b"\x1b_tsp;o;c=1;m=1;{\"id\"\x07",
        b"\x1b_other\x07",
        b"\x1b_ts\x1bx",
        "\u{4e2d}e\u{301}\u{1f600}x\u{fe0f}".as_bytes(),
        b"\xe4\xb8\xad\xf0\x9f\x98\x80\xc2\x9c\xff",
        b"plain text\r\n",
        b"\x08\t\x0e\x0f",
        b"\x1b7\x1b8\x1bD\x1bM",
    ]
    .iter()
    .map(|b| b.to_vec())
    .collect();
    let piece = prop_oneof![
        4 => prop::sample::select(pieces.clone()),
        2 => (prop::sample::select(pieces), any::<prop::sample::Index>())
            .prop_map(|(p, i)| p[..i.index(p.len() + 1)].to_vec()),
        2 => soup(),
    ];
    prop::collection::vec(piece, 1..16).prop_map(|v| v.concat())
}

/// Cuts `stream` at `cut`: a terminal rebuilt from a snapshot there, with other sequences between
/// the terminal's state and the parser's (as a replica appends its own, §15.3), continues like
/// the original.
fn check_cut(cols: u16, rows: u16, stream: &[u8], cut: usize) -> Result<(), TestCaseError> {
    let config = Config { scrollback: 6 };
    let mut t = Terminal::new(cols, rows, config.clone());
    t.feed(&stream[..cut]);
    t.take_events();
    let mut whole = t.snapshot_state();
    t.snapshot_parser(&mut whole);
    prop_assert_eq!(&whole, &t.snapshot());

    let mut snapshot = t.snapshot_state();
    // Sequences that leave the terminal's state alone.
    snapshot.extend_from_slice(b"\x1b]7;file://host/x\x1b\\\x1b_tsp;q;{}\x1b\\");
    t.snapshot_parser(&mut snapshot);
    let mut u = Terminal::new(cols, rows, config);
    u.feed(&snapshot);
    u.take_events();
    prop_assert_eq!(t.state_hash(), u.state_hash(), "rebuilt at {}", cut);
    let rest = &stream[cut..];
    let (te, ue) = (t.feed(rest), u.feed(rest));
    prop_assert_eq!(t.state_hash(), u.state_hash(), "continued from {}", cut);
    prop_assert_eq!(te, ue, "events after {}", cut);
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 400, failure_persistence: None, ..ProptestConfig::default() })]

    /// A snapshot at every byte position of a stream.
    #[test]
    fn snapshot_at_every_byte_continues_like_the_original(
        cols in 2u16..16,
        rows in 1u16..8,
        stream in stream(),
    ) {
        for cut in 0..=stream.len() {
            check_cut(cols, rows, &stream, cut)?;
        }
    }
}

/// An APC string abandoned for its length: snapshots on either side of the limit.
#[test]
fn snapshot_around_an_abandoned_apc_string() {
    for prefix in [&b"tsp;o;"[..], b"other;", b"t"] {
        let mut stream = b"\x1b_".to_vec();
        stream.extend_from_slice(prefix);
        stream.resize(2 + 262_144, b'y');
        stream.extend_from_slice(b"zz\x1b[31mred\x1b\\");
        let end = 2 + 262_144;
        for cut in [0, 1, 2, 3, end - 1, end, end + 1, end + 2, end + 3] {
            check_cut(10, 3, &stream, cut).unwrap();
        }
    }
}

#[test]
fn snapshot_keeps_anchors_and_scrollback() {
    let mut t = Terminal::new(20, 5, Config { scrollback: 100 });
    for i in 0..30 {
        t.feed(format!("line {i}\r\n").as_bytes());
    }
    t.feed(b"$ run");
    let line = t.open_anchor(AnchorKey(42)).expect("main screen");
    t.feed(b"output\r\n");
    let snapshot = t.snapshot();
    let mut u = Terminal::new(20, 5, Config { scrollback: 100 });
    u.feed(&snapshot);
    assert_eq!(u.anchor_line(AnchorKey(42)), Some(line));
    assert_eq!(u.history_len(), t.history_len());
    assert_eq!(
        u.text(0..u.history_len() + 5),
        t.text(0..t.history_len() + 5)
    );
    assert_eq!(u.state_hash(), t.state_hash());
}

#[test]
fn hash_covers_state() {
    let base = || {
        let mut t = Terminal::new(10, 3, Config::default());
        t.feed(b"abc");
        t
    };
    let reference = base().state_hash();
    let variants: [&[u8]; 8] = [
        b"\x1b[1m",
        b"\x1b[?2004h",
        b"\x1b]2;x\x07",
        b"\x1b[2;3r",
        b"\x1bH",
        b"\x1b(0",
        b"\x1b[>1u",
        b"\x1b[",
    ];
    for v in variants {
        let mut t = base();
        t.feed(v);
        assert_ne!(t.state_hash(), reference, "{v:?} changes the hash");
    }
}

/// Prompt rows (OSC 133 `A`, §5.3) are replicated state: they survive a snapshot and scrolling
/// into the scrollback, and erasing the row clears them. The alternate screen has none.
#[test]
fn prompt_rows_survive_snapshots_and_go_with_their_row() {
    let mut t = Terminal::new(20, 4, Config { scrollback: 50 });
    t.feed(b"\x1b]133;A\x07$ one\r\nout\r\n\x1b]133;A\x07$ two\r\nout\r\nout\r\n\x1b]133;A\x07$ ");
    assert_eq!(t.prompt_lines(), vec![0, 2, 5]);
    let mut copy = Terminal::new(20, 4, Config { scrollback: 50 });
    copy.feed(&t.snapshot());
    assert_eq!(copy.prompt_lines(), t.prompt_lines());
    assert_eq!(copy.state_hash(), t.state_hash());
    // Erasing the screen moves its rows into the scrollback, prompts and all; erasing a prompt's
    // whole row clears its mark.
    t.feed(b"\x1b[2J");
    assert_eq!(t.prompt_lines(), vec![0, 2, 5]);
    t.feed(b"\x1b]133;A\x07$ \x1b[2K");
    assert_eq!(t.prompt_lines(), vec![0, 2, 5]);
    // Prompts on the alternate screen aren't marked.
    t.feed(b"\x1b[?1049h\x1b]133;A\x07alt");
    assert!(t.prompt_lines().is_empty());
    t.feed(b"\x1b[?1049l");
    assert_eq!(t.prompt_lines(), vec![0, 2, 5]);
}
