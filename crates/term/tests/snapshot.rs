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
