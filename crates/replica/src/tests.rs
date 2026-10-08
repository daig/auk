use super::*;

/// A shell-like session: prompts with marks, colored output, a cwd, a title, a user variable,
/// wide characters and enough lines to scroll.
fn session() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"\x1b]7;file://host/tmp/work\x1b\\\x1b]2;work\x07");
    out.extend_from_slice(b"\x1b]1337;SetUserVar=resume=b21wIC0tcmVzdW1l\x07");
    for i in 0..60 {
        out.extend_from_slice(b"\x1b]133;A\x07% \x1b]133;B\x07");
        out.extend_from_slice(format!("echo {i}\r\n").as_bytes());
        out.extend_from_slice(b"\x1b]133;C\x07");
        out.extend_from_slice(
            format!("\x1b[1;3{}mline {i} \u{4e2d}\u{6587}\x1b[0m\r\n", i % 8).as_bytes(),
        );
        out.extend_from_slice(b"\x1b]133;D;0\x07");
    }
    out
}

/// Splits `bytes` into chunks whose sizes cycle through `sizes`.
fn chunks<'a>(bytes: &'a [u8], sizes: &[usize]) -> Vec<&'a [u8]> {
    let mut out = Vec::new();
    let mut rest = bytes;
    for size in sizes.iter().cycle() {
        if rest.is_empty() {
            break;
        }
        let n = (*size).clamp(1, rest.len());
        out.push(&rest[..n]);
        rest = &rest[n..];
    }
    out
}

fn follower() -> Replica {
    Replica::new(80, 24, Settings::default(), Role::Follower)
}

#[test]
fn out_of_order_items_are_refused() {
    let mut r = follower();
    assert_eq!(
        r.apply(2, Item::Output(b"x")),
        Err(SeqError {
            expected: 1,
            got: 2
        })
    );
    r.apply(1, Item::Output(b"x")).unwrap();
    assert_eq!(
        r.apply(1, Item::Output(b"y")),
        Err(SeqError {
            expected: 2,
            got: 1
        })
    );
    assert_eq!(r.seq(), 1);
}

#[test]
fn chunking_does_not_change_state() {
    let bytes = session();
    let mut whole = follower();
    whole.push(Item::Output(&bytes));
    let mut split = follower();
    // Same items, the bytes of the first one split differently inside one output item each.
    let mut joined = Vec::new();
    for chunk in chunks(&bytes, &[1, 7, 3, 64, 2]) {
        joined.extend_from_slice(chunk);
    }
    split.push(Item::Output(&joined));
    assert_eq!(whole.state_hash(), split.state_hash());
    assert_eq!(whole.text(), split.text());
}

#[test]
fn replicas_fed_the_same_items_agree() {
    let bytes = session();
    let resize = Record::Resize { cols: 50, rows: 10 };
    let clear = Record::ClearScrollback;
    let mut daemon = Replica::new(80, 24, Settings::default(), Role::Authoritative);
    let mut clients = [follower(), follower()];

    let mut stream: Vec<Item<'_>> = Vec::new();
    for (i, chunk) in chunks(&bytes, &[5, 300, 17]).into_iter().enumerate() {
        stream.push(Item::Output(chunk));
        if i == 20 {
            stream.push(Item::Record(&resize));
        }
        if i == 40 {
            stream.push(Item::Record(&clear));
        }
    }
    for item in &stream {
        let (seq, _) = daemon.push(*item);
        for client in &mut clients {
            client.apply(seq, *item).unwrap();
        }
    }
    for client in &clients {
        assert_eq!(client.state_hash(), daemon.state_hash());
        assert_eq!(client.text(), daemon.text());
    }
    assert_eq!(daemon.seq(), stream.len() as u64);
}

#[test]
fn snapshot_replay_matches_live_state_and_continues() {
    let bytes = session();
    let (first, second) = bytes.split_at(bytes.len() / 2);
    let mut live = Replica::new(80, 24, Settings::default(), Role::Authoritative);
    live.push(Item::Output(first));
    live.push(Item::Record(&Record::Resize { cols: 72, rows: 20 }));

    let snapshot = live.snapshot();
    assert_eq!(snapshot.seq, 2);
    assert_eq!(
        (snapshot.cols, snapshot.rows),
        (live.terminal().cols(), live.terminal().rows())
    );
    let mut late = Replica::from_snapshot(&snapshot, Role::Follower, None);
    assert_eq!(late.seq(), 2);
    assert_eq!(late.state_hash(), live.state_hash());

    let (seq, _) = live.push(Item::Output(second));
    late.apply(seq, Item::Output(second)).unwrap();
    assert_eq!(late.state_hash(), live.state_hash());
    assert_eq!(late.text(), live.text());
}

#[test]
fn cwd_and_user_vars_are_replicated_state() {
    let mut r = follower();
    let events = r.push(Item::Output(&session())).1;
    assert_eq!(r.cwd(), Some("file://host/tmp/work"));
    assert_eq!(
        r.user_vars().get("resume").map(Vec::as_slice),
        Some(&b"omp --resume"[..])
    );
    assert!(events.contains(&Event::Cwd("file://host/tmp/work".into())));
    assert!(events.contains(&Event::Mark(auk_term::Mark::CommandEnd { status: Some(0) })));

    let copy = Replica::from_snapshot(&r.snapshot(), Role::Follower, None);
    assert_eq!(copy.cwd(), r.cwd());
    assert_eq!(copy.user_vars(), r.user_vars());

    let mut other = follower();
    other.push(Item::Output(b"\x1b]7;file://host/elsewhere\x07"));
    let mut same = follower();
    same.push(Item::Output(b"\x1b]7;file://host/tmp/work\x07"));
    assert_ne!(other.state_hash(), same.state_hash());
}

#[test]
fn only_the_authoritative_replica_answers() {
    let mut daemon = Replica::new(80, 24, Settings::default(), Role::Authoritative);
    let mut client = follower();
    daemon.push(Item::Output(b"hi\x1b[6n"));
    client.push(Item::Output(b"hi\x1b[6n"));
    assert_eq!(daemon.take_replies(), b"\x1b[1;3R");
    assert!(daemon.take_replies().is_empty());
    assert!(client.take_replies().is_empty());
}

#[test]
fn snapshot_replay_answers_nothing() {
    let mut live = follower();
    live.push(Item::Output(b"\x1b[c\x1b[6n"));
    let replayed = Replica::from_snapshot(&live.snapshot(), Role::Authoritative, None);
    let mut replayed = replayed;
    assert!(replayed.take_replies().is_empty());
}

#[test]
fn records_apply_in_stream_order() {
    let mut r = follower();
    r.push(Item::Output(b"one\r\ntwo"));
    r.push(Item::Record(&Record::Resize { cols: 40, rows: 5 }));
    assert_eq!((r.terminal().cols(), r.terminal().rows()), (40, 5));
    let settings = Settings {
        scrollback: 3,
        native_surfaces: false,
        ..Settings::default()
    };
    r.push(Item::Record(&Record::Settings {
        settings: settings.clone(),
    }));
    assert_eq!(r.settings(), &settings);
    r.push(Item::Record(&Record::Exit { status: 3 }));
    assert_eq!(r.exit(), Some(3));
    let copy = Replica::from_snapshot(&r.snapshot(), Role::Follower, None);
    assert_eq!((copy.exit(), copy.state_hash()), (Some(3), r.state_hash()));
    let (_, events) = r.push(Item::Record(&Record::Reset));
    assert_eq!(events, vec![Event::Reset]);
    assert!(!r.text().contains("one"));
}

#[test]
fn settings_are_part_of_the_hash() {
    let mut a = follower();
    let mut b = follower();
    a.push(Item::Record(&Record::Settings {
        settings: Settings::default(),
    }));
    b.push(Item::Record(&Record::Settings {
        settings: Settings {
            command_lenses: false,
            ..Settings::default()
        },
    }));
    assert_ne!(a.state_hash(), b.state_hash());
}

/// A TSP handler that answers every string with its length, to check ordering and replay rules.
#[derive(Debug, Default)]
struct Echo {
    seen: Vec<Vec<u8>>,
}

impl TspHandler for Echo {
    fn on_tsp(&mut self, payload: &[u8], ctx: TspContext<'_>) {
        self.seen.push(payload.to_vec());
        ctx.replies
            .extend_from_slice(format!("<{}>", payload.len()).as_bytes());
    }
}

#[test]
fn tsp_strings_reach_the_handler_in_order_with_answers() {
    let mut r = Replica::new(80, 24, Settings::default(), Role::Authoritative);
    r.set_tsp_handler(Box::new(Echo::default()));
    r.push(Item::Output(b"\x1b_tsp;q;{}\x1b\\\x1b[c"));
    let replies = r.take_replies();
    assert!(
        replies.starts_with(b"<8>"),
        "{:?}",
        String::from_utf8_lossy(&replies)
    );
    assert!(replies.len() > 3, "DA1 follows the TSP answer");
}
