//! A snapshot may be taken at any byte position: a late-attaching client's replica
//! rebuilt from it and fed the rest of the stream must reach the live replica's state, even when
//! the cut falls inside an escape sequence, an OSC, DCS or TSP string, a UTF-8 character or a TSP
//! chunk sequence.

// proptest doesn't build for wasm32.
#![cfg(not(target_family = "wasm"))]

use auk_replica::{Item, Record, Replica, Role, Settings};
use proptest::prelude::*;

fn tsp(verb: &str, params: &str, body: &str) -> Vec<u8> {
    format!("\x1b_tsp;{verb};{params}{body}\x1b\\").into_bytes()
}

/// Pieces of pane output that leave the parser or the TSP engine in the middle of something.
fn pieces() -> Vec<Vec<u8>> {
    let mut p: Vec<Vec<u8>> = [
        &b"\x1b[38;2;1;2;3m"[..],
        b"\x1b[4:3m",
        b"\x1b[1;31;48;5;200m",
        b"\x1b[?1049h",
        b"\x1b[?1049l",
        b"\x1b[3;5r",
        b"\x1b[2J",
        b"\x1b[5;7H",
        b"\x1b[!p",
        b"\x1b[1 q",
        b"\x1b[>1u",
        b"\x1b[;;;:1:2;99999999;?h",
        b"\x1b(0",
        b"\x1b#8",
        b"\x1b \x21\x22F",
        b"\x1b7\x1b8",
        b"\x1bc",
        b"\x1b]0;title\x07",
        b"\x1b]2;other title\x1b\\",
        b"\x1b]7;file://host/tmp/x\x1b\\",
        b"\x1b]8;id=1;http://x\x1b\\link\x1b]8;;\x1b\\",
        b"\x1b]1337;SetUserVar=v=aGk=\x07",
        b"\x1b]133;A\x07$ ",
        b"\x1b]133;C\x07",
        b"\x1bP1;2$qm\x1b\\",
        b"\x1bXsos\x1b\\",
        b"\x1b^pm\x1b\\",
        b"\x1b_other apc\x07",
        b"\x1b_ts\x1bx",
        "\u{4e2d}\u{6587}e\u{301}\u{1f600}".as_bytes(),
        b"\xe4\xb8\xad\xf0\x9f\x98\x80\xc2\x9c",
        b"\xe4\xb8\x1b[m\xf0\x9f\x98\xff\xc2",
        b"hello world",
        b"\r\n",
        b"\x08\t",
        b"\x1b[6n\x1b[c",
    ]
    .iter()
    .map(|b| b.to_vec())
    .collect();
    p.extend([
        tsp("q", "", r#"{"q":"hello","v":[1],"app":"cut"}"#),
        tsp("o", "", r#"{"id":"s1","mode":"flow","title":"T"}"#),
        tsp("o", "", r#"{"id":"s2","mode":"inline"}"#),
        tsp(
            "f",
            "",
            r#"{"sf":"s1","s":1,"ops":[["add","main","s1",null,{"id":"main","k":"col","c":[{"id":"t","k":"text","p":{"text":"hi"}}]}]]}"#,
        ),
        tsp(
            "f",
            "",
            r#"{"sf":"s2","s":2,"ops":[["add","main","s2",null,{"id":"main","k":"col"}]]}"#,
        ),
        tsp("x", "", r#"{"id":"s1","keep":true}"#),
        tsp("x", "", r#"{"id":"s2"}"#),
        // A chunk sequence, its pieces arriving as separate strings.
        tsp("o", "c=k1;m=1;", r#"{"id":"s3","#),
        tsp("o", "c=k1;m=1;", r#""mode":"#),
        tsp("o", "c=k1;", r#""flow"}"#),
        tsp("o", "c=k2;m=1;", r#"{"id":"s4","mode""#),
        tsp("o", "c=k2;", r#":"inline"}"#),
    ]);
    p
}

#[derive(Clone, Debug)]
enum Step {
    Output(Vec<u8>),
    Record(Record),
}

fn output() -> impl Strategy<Value = Vec<u8>> {
    let piece = prop_oneof![
        6 => prop::sample::select(pieces()),
        // Part of a piece, so strings are cut and abandoned too.
        2 => (prop::sample::select(pieces()), any::<prop::sample::Index>())
            .prop_map(|(p, i)| p[..i.index(p.len() + 1)].to_vec()),
        2 => prop::collection::vec(
            prop::sample::select(b"\x1b[]_P^X;:?0123456789mHJ\\\x07\x18\x9c\xc2\xe4\xb8".to_vec()),
            1..6,
        ),
        1 => prop::collection::vec(any::<u8>(), 1..4),
    ];
    prop::collection::vec(piece, 1..24).prop_map(|v| v.concat())
}

fn record() -> impl Strategy<Value = Record> {
    prop_oneof![
        (10u16..60, 3u16..20).prop_map(|(cols, rows)| Record::Resize { cols, rows }),
        Just(Record::ClearScrollback),
        Just(Record::Reset),
        Just(Record::ChunkTimeout),
        Just(Record::Exit { status: 1 }),
        (0usize..50, any::<bool>()).prop_map(|(scrollback, native_surfaces)| Record::Settings {
            settings: Settings {
                scrollback,
                native_surfaces,
                ..Settings::default()
            },
        }),
    ]
}

fn steps() -> impl Strategy<Value = Vec<Step>> {
    prop::collection::vec(
        prop_oneof![
            8 => output().prop_map(Step::Output),
            1 => record().prop_map(Step::Record),
        ],
        1..12,
    )
}

fn apply(r: &mut Replica, step: &Step) {
    match step {
        Step::Output(b) => r.push(Item::Output(b)),
        Step::Record(rec) => r.push(Item::Record(rec)),
    };
}

/// Feeds `steps` to a live replica, cutting the output step `at` at byte `cut`: a snapshot taken
/// there and replayed, then fed the rest, must agree with the live replica after every step.
fn check_cut(cols: u16, rows: u16, steps: &[Step], at: usize, cut: usize) {
    let mut live = Replica::new(cols, rows, Settings::default(), Role::Authoritative);
    let mut late: Option<Replica> = None;
    for (k, step) in steps.iter().enumerate() {
        match (k == at, step) {
            (true, Step::Output(b)) => {
                let cut = cut.min(b.len());
                live.push(Item::Output(&b[..cut]));
                let snapshot = live.snapshot();
                let rebuilt = Replica::from_snapshot(&snapshot, Role::Follower, None);
                late = Some(rebuilt);
                live.push(Item::Output(&b[cut..]));
                let r = late.as_mut().expect("just built");
                r.push(Item::Output(&b[cut..]));
            }
            _ => {
                apply(&mut live, step);
                if let Some(r) = late.as_mut() {
                    apply(r, step);
                }
            }
        }
        if let Some(r) = &late {
            assert_eq!(
                r.state_hash(),
                live.state_hash(),
                "cut at step {at} byte {cut}, diverged after step {k}: {steps:?}"
            );
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 2000, failure_persistence: None, ..ProptestConfig::default() })]

    /// A snapshot at a random byte position of a random step.
    #[test]
    fn snapshot_at_any_byte_continues_like_the_live_replica(
        cols in 10u16..50,
        rows in 3u16..12,
        steps in steps(),
        at in any::<prop::sample::Index>(),
        cut in any::<prop::sample::Index>(),
    ) {
        let at = at.index(steps.len());
        let len = match &steps[at] { Step::Output(b) => b.len(), Step::Record(_) => 0 };
        check_cut(cols, rows, &steps, at, cut.index(len + 1));
    }
}

/// Every byte position of a stream that holds each kind of piece.
#[test]
fn snapshot_at_every_byte_of_every_piece() {
    let steps: Vec<Step> = pieces().into_iter().map(Step::Output).collect();
    for (at, step) in steps.iter().enumerate() {
        let Step::Output(b) = step else { continue };
        for cut in 0..=b.len() {
            check_cut(40, 8, &steps, at, cut);
        }
    }
}

/// Feeds `steps` to a live replica, taking a snapshot at each `(step, byte)` cut (in order) and
/// feeding each rebuilt replica the rest; all must end in the live replica's state.
fn check_cuts(cols: u16, rows: u16, steps: &[Step], cuts: &[(usize, usize)]) {
    let mut live = Replica::new(cols, rows, Settings::default(), Role::Authoritative);
    let mut late: Vec<((usize, usize), Replica)> = Vec::new();
    for (k, step) in steps.iter().enumerate() {
        let Step::Output(b) = step else {
            apply(&mut live, step);
            late.iter_mut().for_each(|(_, r)| apply(r, step));
            continue;
        };
        let mut done = 0;
        for &(at, cut) in cuts.iter().filter(|(at, _)| *at == k) {
            let cut = cut.clamp(done, b.len());
            let piece = &b[done..cut];
            live.push(Item::Output(piece));
            late.iter_mut().for_each(|(_, r)| {
                r.push(Item::Output(piece));
            });
            done = cut;
            late.push((
                (at, cut),
                Replica::from_snapshot(&live.snapshot(), Role::Follower, None),
            ));
        }
        let piece = &b[done..];
        live.push(Item::Output(piece));
        late.iter_mut().for_each(|(_, r)| {
            r.push(Item::Output(piece));
        });
    }
    for ((at, cut), r) in &late {
        assert_eq!(
            r.state_hash(),
            live.state_hash(),
            "cut at step {at} byte {cut}"
        );
    }
}

/// A chunk sequence dropped for its size, its remaining chunks swallowed: snapshots inside the
/// chunk that overflows, while swallowing, inside the swallowed chunk and after it.
#[test]
fn snapshot_while_joining_and_swallowing_an_oversize_sequence() {
    let piece = "y".repeat(250_000);
    let chunk = |more: bool| tsp("f", if more { "c=big;m=1;" } else { "c=big;" }, &piece);
    let mut steps: Vec<Step> = vec![Step::Output(tsp("o", "", r#"{"id":"s1","mode":"flow"}"#))];
    // 102 chunks of 250 000 bytes pass the 24 MiB join limit at the 101st.
    for _ in 0..101 {
        steps.push(Step::Output(chunk(true)));
    }
    steps.push(Step::Output(chunk(false)));
    steps.push(Step::Output(b"after\r\n".to_vec()));
    steps.push(Step::Output(tsp(
        "f",
        "",
        r#"{"sf":"s1","s":1,"ops":[["add","main","s1",null,{"id":"main","k":"text","p":{"text":"hi"}}]]}"#,
    )));
    let len = |at: usize| match &steps[at] {
        Step::Output(b) => b.len(),
        Step::Record(_) => 0,
    };
    // Joining is covered above; these are around the drop and while swallowing.
    // A cut in step 101 replays the 24 MiB joined so far, so only two there.
    let mut cuts = vec![(101, 3), (101, len(101) - 1)];
    for at in [102, 103] {
        cuts.extend([0, 3, 12, len(at) / 2, len(at) - 1].map(|cut| (at, cut)));
    }
    check_cuts(40, 8, &steps, &cuts);
}
