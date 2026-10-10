//! Lens claims and command blocks in replicas (§9.6, §15.1, §15.2): every replica claims from the
//! sequenced catalog and captures the same bytes, followers built from a snapshot mid-command
//! included.

use auk_plugins::catalog::{LensRule, PluginEntry};
use auk_plugins::{Catalog, Status};
use auk_replica::{Item, LensNote, Record, Replica, Role, Settings, claim_catalog};

fn catalog() -> Catalog {
    Catalog {
        plugins: vec![PluginEntry {
            id: "tf".into(),
            name: "TF".into(),
            version: "1".into(),
            description: "d".into(),
            icon: "stack".into(),
            css: ".x{}".into(),
            blocks: Vec::new(),
            lenses: vec![LensRule {
                id: "plan".into(),
                patterns: vec!["terraform plan".into(), "terraform plan *".into()],
            }],
            host: true,
            window: false,
            status: Status::Ready,
        }],
        problems: Vec::new(),
        jit: true,
    }
}

const START: &[u8] =
    b"$ terraform plan\r\n\x1b]133;C;cmdline_url=terraform%20plan;typed_url=tp\x07";

fn stream() -> Vec<Vec<u8>> {
    let mut items = vec![START.to_vec()];
    for i in 0..30 {
        items.push(format!("  # aws_thing.r{i} will be created\r\n").into_bytes());
    }
    items.push(
        b"Plan: 30 to add\r\n\x1b]133;D;0\x07\x1b]7;file://h/tmp\x07\x1b]133;A\x07$ ".to_vec(),
    );
    items
}

fn hex(r: &Replica) -> [u8; 32] {
    r.state_hash()
}

#[test]
fn every_replica_claims_and_captures_alike() {
    let settings = Settings::default();
    let mut daemon = Replica::new(80, 24, settings.clone(), Role::Authoritative);
    let mut follower = Replica::new(80, 24, settings, Role::Follower);
    let record = Record::Catalog {
        catalog: claim_catalog(&catalog()),
    };
    let (seq, _) = daemon.push(Item::Record(&record));
    follower.apply(seq, Item::Record(&record)).unwrap();
    assert!(daemon.lens_catalog().unwrap().plugins[0].css.is_empty());

    let items = stream();
    let mut late = None;
    for (n, bytes) in items.iter().enumerate() {
        let (seq, _) = daemon.push(Item::Output(bytes));
        follower.apply(seq, Item::Output(bytes)).unwrap();
        if let Some(l) = &mut late {
            Replica::apply(l, seq, Item::Output(bytes)).unwrap();
        }
        if n == 10 {
            assert_eq!(daemon.lens_running(), Some(0));
            // A snapshot mid-command, through JSON as the wire carries it.
            let snap = daemon.snapshot();
            let header = serde_json::to_string(&snap).unwrap();
            let mut back: auk_replica::Snapshot = serde_json::from_str(&header).unwrap();
            back.bytes = snap.bytes.clone();
            late = Some(Replica::from_snapshot(&back, Role::Follower, None));
            assert_eq!(hex(late.as_ref().unwrap()), hex(&daemon));
        }
        assert_eq!(hex(&daemon), hex(&follower), "item {n}");
    }
    let late = late.unwrap();
    assert_eq!(hex(&late), hex(&daemon));

    // The output went to the block, not the grid; the prompt after `D` is in the grid.
    let text = daemon.text();
    assert!(!text.contains("aws_thing"), "{text}");
    assert!(daemon.screen_text().contains("$ "));
    let blocks = daemon.lens_blocks();
    assert_eq!(blocks.len(), 1);
    let b = &blocks[0];
    assert_eq!((b.plugin.as_str(), b.lens.as_str()), ("tf", "plan"));
    assert_eq!(
        (b.line.as_str(), b.typed.as_str()),
        ("terraform plan", "tp")
    );
    assert_eq!((b.done, b.status), (true, Some(0)));
    assert!(String::from_utf8_lossy(&b.raw).contains("aws_thing.r29"));
    assert_eq!(late.lens_blocks()[0].raw, b.raw);

    // Notes: open, lines, finish; all 31 output lines reached the host.
    let notes = daemon.take_lens_notes();
    assert!(
        matches!(&notes[0], LensNote::Open { block: 0, run, .. } if run.program == "terraform")
    );
    let lines: Vec<String> = notes
        .iter()
        .filter_map(|n| match n {
            LensNote::Lines { lines, .. } => Some(lines.iter().map(|l| l.text.clone())),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(lines.len(), 31, "{lines:?}");
    assert_eq!(lines[30], "Plan: 30 to add");
    assert!(matches!(
        notes.last(),
        Some(LensNote::Finish {
            block: 0,
            status: Some(0)
        })
    ));
    assert!(
        follower.take_lens_notes().is_empty(),
        "followers tell nobody"
    );

    // The duration and the view: the first is stream state, the second a side message.
    let took = Record::LensTook {
        block: 0,
        took_ms: 1234,
    };
    let (seq, _) = daemon.push(Item::Record(&took));
    follower.apply(seq, Item::Record(&took)).unwrap();
    assert_eq!(follower.lens_blocks()[0].took_ms, Some(1234));
    let before = hex(&follower);
    assert!(follower.set_lens_view(
        0,
        Some(serde_json::json!({"k": "text", "p": {"text": "hi"}}))
    ));
    assert_eq!(hex(&follower), before, "views aren't hashed");
    assert!(
        follower.snapshot().lens.blocks[0].view.is_some(),
        "but they travel in snapshots"
    );
    assert!(!follower.set_lens_view(9, None));
}

#[test]
fn unclaimed_and_interactive_commands_stay_in_the_grid() {
    let mut r = Replica::new(80, 24, Settings::default(), Role::Authoritative);
    r.push(Item::Record(&Record::Catalog {
        catalog: claim_catalog(&catalog()),
    }));
    r.push(Item::Output(
        b"\x1b]133;C;cmdline_url=ls\x07file.txt\r\n\x1b]133;D;0\x07",
    ));
    assert!(r.lens_blocks().is_empty());
    assert!(r.text().contains("file.txt"));
    // A pipeline is never lensed.
    r.push(Item::Output(
        b"\x1b]133;C;cmdline_url=terraform%20plan%20%7C%20cat\x07piped\r\n\x1b]133;D;0\x07",
    ));
    assert!(r.lens_blocks().is_empty());
    // A claimed command that turns interactive: everything captured is replayed into the grid.
    r.push(Item::Output(
        b"\x1b]133;C;cmdline_url=terraform%20plan\x07before\r\n",
    ));
    assert_eq!(r.lens_running(), Some(0));
    r.push(Item::Output(b"\x1b[6n"));
    assert_eq!(r.lens_running(), None);
    assert!(r.lens_blocks().is_empty());
    assert!(r.text().contains("before"));
    let replies = r.take_replies();
    assert!(
        replies.iter().any(|a| a.bytes().ends_with(b"R")),
        "the replayed query is answered by the grid: {replies:?}"
    );
    let notes = r.take_lens_notes();
    assert!(matches!(notes.last(), Some(LensNote::Drop { block: 0 })));
    // Lenses off: no claim.
    let off = Settings {
        command_lenses: false,
        ..Settings::default()
    };
    r.push(Item::Record(&Record::Settings { settings: off }));
    r.push(Item::Output(
        b"\x1b]133;C;cmdline_url=terraform%20plan\x07x\r\n\x1b]133;D;0\x07",
    ));
    assert!(r.lens_blocks().is_empty());
}

#[test]
fn an_exit_ends_a_running_capture_and_cleared_scrollback_drops_blocks() {
    let mut r = Replica::new(40, 5, Settings::default(), Role::Authoritative);
    r.push(Item::Record(&Record::Catalog {
        catalog: claim_catalog(&catalog()),
    }));
    r.push(Item::Output(
        b"\x1b]133;C;cmdline_url=terraform%20plan\x07partial\r\n",
    ));
    r.push(Item::Record(&Record::Exit { status: 1 }));
    let b = &r.lens_blocks()[0];
    assert!(b.done && b.status.is_none());
    assert_eq!(b.raw, b"partial\r\n");
    for _ in 0..10 {
        r.push(Item::Output(b"line\r\n"));
    }
    assert_eq!(r.lens_blocks().len(), 1, "scrolled into the scrollback");
    r.push(Item::Record(&Record::ClearScrollback));
    assert!(r.lens_blocks().is_empty());
}

/// One claimed `terraform plan` that prints `n` lines and finishes.
fn claimed_command(n: usize) -> Vec<u8> {
    let mut out = START.to_vec();
    for i in 0..n {
        out.extend(
            format!("  # aws_thing.r{i:02} will be created, with a long tail of text\r\n").bytes(),
        );
    }
    out.extend_from_slice(b"\x1b]133;D;0\x07\x1b]133;A\x07$ ");
    out
}

/// A flow surface with a little text, closed (it stays, read-only, at its anchor row).
fn closed_surface(id: &str) -> Vec<u8> {
    let tsp = |verb: &str, body: String| format!("\x1b_tsp;{verb};{body}\x1b\\").into_bytes();
    let mut out = b"prog\r\n".to_vec();
    out.extend(tsp("o", format!(r#"{{"id":"{id}","mode":"flow"}}"#)));
    out.extend(tsp(
        "f",
        format!(
            r#"{{"sf":"{id}","s":1,"ops":[["add","main","{id}",null,{{"id":"main","k":"col","c":[{{"id":"t","k":"text","p":{{"text":"{}"}}}}]}}]]}}"#,
            "x".repeat(300)
        ),
    ));
    out.extend(tsp("x", format!(r#"{{"id":"{id}","keep":true}}"#)));
    out.extend_from_slice(b"\x1b]133;A\x07$ ");
    out
}

#[test]
fn finished_command_blocks_share_the_retention_budget_and_go_oldest_first() {
    // Measure what a block and a closed surface cost, on a replica with room for everything.
    let mut probe = Replica::new(100, 30, Settings::default(), Role::Authoritative);
    probe.push(Item::Record(&Record::Catalog {
        catalog: claim_catalog(&catalog()),
    }));
    let docs = probe.tsp().retention_cost();
    probe.push(Item::Output(&claimed_command(10)));
    let block = probe.tsp().retention_cost() - docs;
    assert_eq!(
        block,
        probe.lens_blocks()[0].cost(),
        "finished blocks count"
    );
    probe.push(Item::Output(&closed_surface("s1")));
    let surface = probe.tsp().retention_cost() - docs - block;
    assert!(block > 500 && surface > 300, "{block} {surface}");

    // Room for two blocks, or a block and the surface, but not three things.
    let settings = Settings {
        retention: 2 * block.max(surface) + block.min(surface) / 2,
        ..Settings::default()
    };
    let mut daemon = Replica::new(100, 30, settings.clone(), Role::Authoritative);
    let mut client = Replica::new(100, 30, settings, Role::Follower);
    fn feed(daemon: &mut Replica, client: &mut Replica, item: Item<'_>) {
        let (seq, _) = daemon.push(item);
        client.apply(seq, item).unwrap();
        assert_eq!(hex(daemon), hex(client));
    }
    let (d, c) = (&mut daemon, &mut client);
    feed(
        d,
        c,
        Item::Record(&Record::Catalog {
            catalog: claim_catalog(&catalog()),
        }),
    );
    feed(d, c, Item::Output(&claimed_command(10))); // block 0
    feed(d, c, Item::Output(&closed_surface("s1")));
    assert_eq!(daemon.lens_blocks().len(), 1);
    assert_eq!(daemon.tsp().surfaces().len(), 1);

    // A third item: the oldest of the finished blocks and closed surfaces goes, whole.
    feed(&mut daemon, &mut client, Item::Output(&claimed_command(10))); // block 1
    let ids: Vec<u64> = daemon.lens_blocks().iter().map(|b| b.id).collect();
    assert_eq!(ids, [1], "block 0 was the oldest");
    assert_eq!(
        daemon.tsp().surfaces().len(),
        1,
        "the surface is younger than block 0"
    );
    assert!(daemon.lens_raw(0).is_none(), "an evicted block is gone");

    // A replica built from a snapshot here weighs the same blocks from the start.
    let mut late = Replica::from_snapshot(&daemon.snapshot(), Role::Follower, None);
    let block2 = claimed_command(10);
    let (seq, _) = daemon.push(Item::Output(&block2));
    client.apply(seq, Item::Output(&block2)).unwrap();
    late.apply(seq, Item::Output(&block2)).unwrap();
    assert_eq!(hex(&daemon), hex(&client));
    assert_eq!(hex(&late), hex(&daemon));
    assert_eq!(late.lens_blocks(), client.lens_blocks());
    let ids: Vec<u64> = daemon.lens_blocks().iter().map(|b| b.id).collect();
    assert_eq!(ids, [1, 2], "then the closed surface, older than block 1");
    assert!(daemon.tsp().surfaces().is_empty());
    assert!(daemon.tsp().retention_cost() <= 2 * block.max(surface) + block.min(surface) / 2);

    // A running capture isn't finished: it is never evicted while it runs.
    let (seq, _) = daemon.push(Item::Output(START));
    client.apply(seq, Item::Output(START)).unwrap();
    assert_eq!(daemon.lens_running(), Some(3));
    assert_eq!(hex(&daemon), hex(&client));
    let late = Replica::from_snapshot(&daemon.snapshot(), Role::Follower, None);
    assert_eq!(hex(&late), hex(&daemon));
}
