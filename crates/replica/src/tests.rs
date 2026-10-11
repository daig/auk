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

    // `SetUserVar=<name>` with no value (omp clearing its session) unsets the variable.
    let mut unset = Replica::from_snapshot(&r.snapshot(), Role::Follower, None);
    unset.push(Item::Output(b"\x1b]1337;SetUserVar=resume\x07"));
    assert_eq!(unset.user_vars().get("resume"), None);

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
    assert_eq!(daemon.take_replies(), [Answer::Term(b"\x1b[1;3R".to_vec())]);
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

fn tsp(verb: &str, body: &str) -> Vec<u8> {
    format!("\x1b_tsp;{verb};{body}\x1b\\").into_bytes()
}

#[test]
fn tsp_answers_keep_stream_order_with_terminal_answers() {
    let mut r = Replica::new(80, 24, Settings::default(), Role::Authoritative);
    let mut bytes = b"\x1b[6n".to_vec();
    bytes.extend(tsp("q", r#"{"q":"hello","v":[1]}"#));
    bytes.extend_from_slice(b"\x1b[c");
    r.push(Item::Output(&bytes));
    let replies = r.take_replies();
    assert_eq!(replies.len(), 3, "{replies:?}");
    assert_eq!(replies[0], Answer::Term(b"\x1b[1;1R".to_vec()));
    let Answer::Tsp(hello) = &replies[1] else {
        panic!("{replies:?}")
    };
    assert!(hello.starts_with(b"\x1b_tsp;r;{"));
    assert!(matches!(&replies[2], Answer::Term(da1) if da1.starts_with(b"\x1b[?")));
}

/// A flow surface with a document, a prompt, more output.
fn surface_session() -> Vec<u8> {
    let mut out = b"$ prog\r\n".to_vec();
    out.extend(tsp("q", r#"{"q":"hello","v":[1],"app":"t"}"#));
    out.extend(tsp("o", r#"{"id":"s1","mode":"flow","title":"T"}"#));
    out.extend(tsp(
        "f",
        r#"{"sf":"s1","s":1,"ops":[["add","main","s1",null,{"id":"main","k":"col","c":[{"id":"t","k":"text","p":{"text":"hi"}}]}]]}"#,
    ));
    out.extend(tsp("x", r#"{"id":"s1"}"#));
    out.extend_from_slice(b"\x1b]133;A\x07$ ");
    out.extend(tsp("o", r#"{"id":"s2","mode":"inline"}"#));
    out.extend(tsp(
        "f",
        r#"{"sf":"s2","s":7,"ops":[["add","main","s2",null,{"id":"main","k":"col"}]]}"#,
    ));
    out.extend(tsp("f", r#"{"sf":"s2","s":8,"ops":[["add","p","main",null,{"id":"p","k":"progress","p":{"value":0.5}}]]}"#));
    out
}

#[test]
fn tsp_state_replicates_and_snapshots() {
    let bytes = surface_session();
    let mut daemon = Replica::new(80, 24, Settings::default(), Role::Authoritative);
    let mut client = follower();
    for (i, chunk) in chunks(&bytes, &[3, 50, 1, 9]).into_iter().enumerate() {
        let (seq, _) = daemon.push(Item::Output(chunk));
        client.apply(seq, Item::Output(chunk)).unwrap();
        assert_eq!(daemon.state_hash(), client.state_hash(), "item {i}");
    }
    assert_eq!(daemon.tsp().surfaces().len(), 2);
    assert!(client.take_replies().is_empty());
    assert!(!daemon.take_replies().is_empty());

    let late = Replica::from_snapshot(&daemon.snapshot(), Role::Follower, None);
    assert_eq!(late.state_hash(), daemon.state_hash());
    assert_eq!(late.tsp().surfaces().len(), 2);

    // The snapshot answers nothing, even to the authoritative role.
    let mut replayed = Replica::from_snapshot(&daemon.snapshot(), Role::Authoritative, None);
    assert!(replayed.take_replies().is_empty());
    assert_eq!(replayed.state_hash(), daemon.state_hash());
}

#[test]
fn tsp_records_apply_on_every_replica() {
    let mut a = Replica::new(80, 24, Settings::default(), Role::Authoritative);
    let mut b = follower();
    let mut items: Vec<Vec<u8>> = vec![surface_session()];
    items.push(b"\x1b_tsp;f;c=k;m=1;{\"sf\":\x1b\\".to_vec());
    for item in &items {
        let (seq, _) = a.push(Item::Output(item));
        b.apply(seq, Item::Output(item)).unwrap();
    }
    assert!(a.tsp().chunk_pending().is_some());
    for record in [
        Record::ChunkTimeout,
        Record::Resize { cols: 60, rows: 20 },
        Record::Exit { status: 0 },
    ] {
        let (seq, _) = a.push(Item::Record(&record));
        b.apply(seq, Item::Record(&record)).unwrap();
        assert_eq!(a.state_hash(), b.state_hash(), "{record:?}");
    }
    assert!(a.tsp().chunk_pending().is_none());
    assert!(a.tsp().surfaces().iter().all(|s| !s.is_live()));
    let off = Settings {
        native_surfaces: false,
        ..Settings::default()
    };
    let (seq, _) = a.push(Item::Record(&Record::Settings {
        settings: off.clone(),
    }));
    b.apply(seq, Item::Record(&Record::Settings { settings: off }))
        .unwrap();
    assert!(!a.tsp().config().enabled);
    assert_eq!(a.state_hash(), b.state_hash());
}

#[test]
fn restored_surfaces_are_closed_read_only_and_survive_snapshots_and_prompts() {
    // A daemon restart rebuilds the replica from its saved snapshot, then sequences `Restore`.
    let mut before = Replica::new(80, 24, Settings::default(), Role::Authoritative);
    before.push(Item::Output(&surface_session()));
    let mut a = Replica::from_snapshot(&before.snapshot(), Role::Authoritative, None);
    let (seq, _) = a.push(Item::Record(&Record::Restore));
    assert!(
        a.tsp()
            .surfaces()
            .iter()
            .all(|s| !s.is_live() && s.restored())
    );
    let mut b = Replica::from_snapshot(&before.snapshot(), Role::Follower, None);
    b.apply(seq, Item::Record(&Record::Restore)).unwrap();
    assert_eq!(a.state_hash(), b.state_hash());
    // A window attaching after the restore gets the same state from a snapshot.
    let late = Replica::from_snapshot(&a.snapshot(), Role::Follower, None);
    assert_eq!(late.state_hash(), a.state_hash());
    assert!(late.tsp().surfaces().iter().all(|s| s.restored()));
    // The new shell's prompt keeps the restored inline surface (Auk's own behavior).
    let (seq, _) = a.push(Item::Output(b"\x1b]133;A\x07$ "));
    b.apply(seq, Item::Output(b"\x1b]133;A\x07$ ")).unwrap();
    assert_eq!(a.tsp().surfaces().len(), 2);
    assert_eq!(a.state_hash(), b.state_hash());
}

#[test]
fn programs_cannot_forge_anchor_rows() {
    let mut r = follower();
    r.push(Item::Output(b"\x1b]6464;anchor;5\x1b\\x"));
    let hash = r.state_hash();
    let mut plain = follower();
    plain.push(Item::Output(b"x"));
    assert_eq!(hash, plain.state_hash());
}

#[test]
fn erasing_an_anchor_row_ends_its_surface_with_gone() {
    let mut r = Replica::new(80, 24, Settings::default(), Role::Authoritative);
    r.push(Item::Output(&tsp("q", r#"{"q":"hello","v":[1]}"#)));
    r.take_replies();
    r.push(Item::Output(&tsp("o", r#"{"id":"e","mode":"flow"}"#)));
    assert!(r.take_replies().is_empty());
    r.push(Item::Output(b"\x1b[2J"));
    let replies = r.take_replies();
    assert!(
        matches!(&replies[..], [Answer::Tsp(b)] if b.windows(4).any(|w| w == b"gone")),
        "{replies:?}"
    );
    assert!(r.tsp().surfaces().is_empty());
}

#[test]
fn ris_and_reset_clear_surfaces_silently() {
    for reset in [true, false] {
        let mut r = Replica::new(80, 24, Settings::default(), Role::Authoritative);
        r.push(Item::Output(&tsp("o", r#"{"id":"f","mode":"flow"}"#)));
        r.push(Item::Output(&tsp("o", r#"{"id":"i","mode":"inline"}"#)));
        r.take_replies();
        if reset {
            r.push(Item::Record(&Record::Reset));
        } else {
            r.push(Item::Output(b"\x1bc"));
        }
        assert!(r.take_replies().is_empty());
        assert!(r.tsp().surfaces().is_empty());
    }
}
