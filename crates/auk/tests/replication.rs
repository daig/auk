//! Replication: replicas agree, clients mirror each other, late attaches get the live state,
//! and mismatched versions are refused.

use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use auk_conformance::harness::{Headless, TestEnv, wait_for};
use auk_conformance::probe::{Probe, events, frame, hello as tsp_hello, open_with};
use auk_mirror::{
    Client, ClientFacts, ClientKind, ClientMsg, DaemonMsg, HandshakeError, Hello, ThemeColors,
    Versions, read_msg, write_msg,
};
use auk_replica::{Item, Record, Replica, Role, Settings};
use serde_json::{Value, json};

fn env(name: &str) -> TestEnv {
    TestEnv::new(env!("CARGO_BIN_EXE_auk"), name)
}

/// Waits until a window's replica of `pane` equals the daemon's, at the same stream position.
fn converge(env: &TestEnv, window: &Headless, pane: u64) -> (String, u64) {
    wait_for("the window's replica to match the daemon's", || {
        let daemon = env.daemon_hash(pane);
        (window.hash(pane) == Some(daemon.clone())).then_some(daemon)
    })
}

/// Shell output exercising colors, wide characters, scrolling and cursor motion.
const NOISY: &str = "for i in $(seq 1 120); do printf '\\e[3%dm%03d \\u4e2d\\u6587 \\e[0m\\e[2K\\r%s\\n' $((i % 8)) $i $i; done; printf '\\e[5;10Hpos\\e[?25l\\e[?25h'";

#[test]
fn two_clients_agree_with_the_daemon_after_shell_output() {
    let env = env("twoclients");
    env.start_daemon();
    let a = env.start_headless("a", &[]);
    let b = env.start_headless("b", &[]);
    let pane = env.new_pane(&[]);
    env.wait(pane, "prompt");
    env.run(pane, NOISY);
    env.run(pane, "ls -la /usr/bin | head -40");
    let at = converge(&env, &a, pane);
    let bt = converge(&env, &b, pane);
    assert_eq!(at.0, bt.0);
    assert!(at.1 > 2, "{at:?}");
    assert_eq!(a.capture(pane, true), b.capture(pane, true));
    assert!(a.capture(pane, true).contains("120"));
    assert!(a.errors().is_empty() && b.errors().is_empty());
}

#[test]
fn late_attach_gets_a_snapshot_equal_to_the_live_state() {
    let env = env("lateattach");
    env.start_daemon();
    let early = env.start_headless("early", &[]);
    let pane = env.new_pane(&[]);
    env.wait(pane, "prompt");
    env.run(pane, NOISY);
    env.daemon(&format!("resize {pane} 70 20"));
    env.run(pane, "echo after-resize-$((2*21))");
    // The alternate screen and modes must come through the snapshot too.
    env.daemon(&format!(
        "run {pane} 'printf \"\\e[?1049h\\e[?2004h\\e[3;3Halt screen\"; sleep 30'"
    ));
    wait_for("the alternate screen", || {
        env.daemon(&format!("capture {pane}"))["text"]
            .as_str()?
            .contains("alt screen")
            .then_some(())
    });

    let late = env.start_headless("late", &[]);
    let live = converge(&env, &late, pane);
    assert_eq!(converge(&env, &early, pane), live);
    assert_eq!(late.capture(pane, false), early.capture(pane, false));

    // Live output after the snapshot keeps them equal.
    env.daemon(&format!("send {pane} keys C-c"));
    env.wait(pane, "prompt");
    env.run(pane, "echo tail-$((3*3))");
    let after = converge(&env, &late, pane);
    assert_eq!(converge(&env, &early, pane), after);
    assert!(late.capture(pane, true).contains("tail-9"));
    assert!(late.errors().is_empty());
}

#[test]
fn mismatched_versions_are_refused() {
    let env = env("refuse");
    env.start_daemon();
    let mut hello = Hello::new(ClientKind::Headless, ClientFacts::default());
    hello.versions = Versions {
        parser: format!("{}-other", hello.versions.parser),
        ..hello.versions
    };
    let stream = UnixStream::connect(&env.socket).unwrap();
    match Client::handshake(stream, &hello) {
        Err(HandshakeError::Refused(refusal)) => {
            assert_eq!(refusal.daemon, Versions::current());
            assert!(refusal.reason.contains("parser"), "{}", refusal.reason);
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    // The daemon closes the connection after refusing.
    let mut hello = Hello::new(ClientKind::Web, ClientFacts::default());
    hello.versions.protocol += 1;
    let stream = UnixStream::connect(&env.socket).unwrap();
    let Err(HandshakeError::Refused(_)) = Client::handshake(stream.try_clone().unwrap(), &hello)
    else {
        panic!("expected a refusal");
    };
    let mut reader = stream;
    assert!(
        read_msg::<DaemonMsg>(&mut reader, &mut Vec::new())
            .unwrap()
            .is_none()
    );

    // The same versions are welcomed.
    let stream = UnixStream::connect(&env.socket).unwrap();
    let hello = Hello::new(ClientKind::Headless, ClientFacts::default());
    let (_, welcome) = Client::handshake(stream, &hello).unwrap();
    assert_eq!(welcome.versions, Versions::current());
}

#[test]
fn ac08_key_typed_in_one_client_appears_in_the_other() {
    let env = env("ac08");
    env.start_daemon();
    let a = env.start_headless("a", &[]);
    let b = env.start_headless("b", &[]);
    let pane = env.new_pane(&[]);
    env.wait(pane, "prompt");
    converge(&env, &a, pane);

    // Typed in A (encoded from A's replica), shown in B without a refresh.
    a.request(&format!("send {pane} text 'echo typed-$((40+2))'"));
    wait_for("B to show the keys typed in A", || {
        b.capture(pane, false)
            .contains("echo typed-$((40+2))")
            .then_some(())
    });
    a.request(&format!("send {pane} keys Enter"));
    wait_for("B to show the command's output", || {
        b.capture(pane, false).contains("typed-42").then_some(())
    });
    env.wait(pane, "prompt");
    assert_eq!(converge(&env, &a, pane), converge(&env, &b, pane));

    // The client that typed is the pane's primary; typing in B moves it.
    let primary = |w: &Headless| {
        w.request("panes")["panes"].as_array().and_then(|p| {
            p.iter()
                .find(|p| p["id"] == pane)
                .map(|p| p["primary"] == true)
        })
    };
    wait_for("A to be primary", || {
        (primary(&a) == Some(true)).then_some(())
    });
    b.request(&format!("send {pane} keys Space BSpace"));
    wait_for("B to be primary", || {
        (primary(&b) == Some(true) && primary(&a) == Some(false)).then_some(())
    });
}

#[test]
fn pane_size_follows_the_primary_client() {
    let env = env("primarysize");
    env.start_daemon();
    let a = env.start_headless("a", &["--size", "100x30"]);
    let b = env.start_headless("b", &["--size", "60x20"]);
    let pane = env.new_pane(&[]);
    env.wait(pane, "prompt");
    let size = || {
        let info = env.pane_info(pane);
        (
            info["cols"].as_u64().unwrap(),
            info["rows"].as_u64().unwrap(),
        )
    };
    a.request(&format!("focus {pane}"));
    wait_for("A's size", || (size() == (100, 30)).then_some(()));
    b.request(&format!("send {pane} keys Space BSpace"));
    wait_for("B's size", || (size() == (60, 20)).then_some(()));
    env.run(pane, "stty size");
    let screen = env.daemon(&format!("capture {pane}"))["text"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(screen.contains("20 60"), "{screen}");
    // When B leaves, A is primary again and its size applies.
    env.kill9(b.pid);
    wait_for("A's size again", || (size() == (100, 30)).then_some(()));
    // The shell redraws after the resize on its own schedule: wait for it to settle.
    wait_for("A to match the daemon at rest", || {
        (converge(&env, &a, pane).1 == env.daemon_hash(pane).1).then_some(())
    });
}

/// A small deterministic generator, so failures reproduce from the seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Byte soup biased toward the sequences terminals parse: CSI, OSC, SGR, wide text, controls.
fn fuzz_bytes(rng: &mut Rng, len: usize) -> Vec<u8> {
    const PIECES: &[&[u8]] = &[
        b"\x1b[",
        b"\x1b]",
        b"\x07",
        b"\x1b\\",
        b"\r\n",
        b"\n",
        b"\r",
        b"\t",
        b"\x08",
        b";",
        b"m",
        b"H",
        b"J",
        b"K",
        b"?1049h",
        b"?1049l",
        b"?2004h",
        b"r",
        b"S",
        b"T",
        b"L",
        b"M",
        b"@",
        b"P",
        b"X",
        b"\x1bc",
        b"\x1b7",
        b"\x1b8",
        b"\x1bD",
        b"\x1bM",
        b"133;A",
        b"7;file:///x",
        "\u{4e2d}".as_bytes(),
        "\u{1f600}".as_bytes(),
        "e\u{301}".as_bytes(),
        b"\xff",
        b"\xc3",
        b"0",
        b"1",
        b"2",
        b"38;5;",
        b"48;2;1;2;3",
        b"hello",
        b"x",
    ];
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        if rng.below(3) == 0 {
            out.push(rng.below(256) as u8);
        } else {
            out.extend_from_slice(PIECES[rng.below(PIECES.len() as u64) as usize]);
        }
    }
    out
}

#[test]
fn ac02_replicas_fed_the_same_stream_and_records_agree_under_fuzzing() {
    for seed in 1..=40u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        let mut daemon = Replica::new(80, 24, Settings::default(), Role::Authoritative);
        let mut followers = vec![
            Replica::new(80, 24, Settings::default(), Role::Follower),
            Replica::new(80, 24, Settings::default(), Role::Follower),
        ];
        let mut late: Option<Replica> = None;
        for step in 0..60 {
            let record = match rng.below(12) {
                0 => Some(Record::Resize {
                    cols: 10 + rng.below(120) as u16,
                    rows: 3 + rng.below(50) as u16,
                }),
                1 => Some(Record::ClearScrollback),
                2 if rng.below(4) == 0 => Some(Record::Reset),
                3 => Some(Record::Settings {
                    settings: Settings {
                        scrollback: rng.below(200) as usize,
                        ..Settings::default()
                    },
                }),
                _ => None,
            };
            let len = 1 + rng.below(400) as usize;
            let bytes = fuzz_bytes(&mut rng, len);
            let item = match &record {
                Some(r) => Item::Record(r),
                None => Item::Output(&bytes),
            };
            let (seq, _) = daemon.push(item);
            let _ = daemon.take_replies();
            for f in &mut followers {
                f.apply(seq, item).unwrap();
            }
            if let Some(l) = &mut late {
                l.apply(seq, item).unwrap();
            }
            if step == 30 {
                late = Some(Replica::from_snapshot(
                    &daemon.snapshot(),
                    Role::Follower,
                    None,
                ));
            }
        }
        let want = daemon.state_hash();
        for f in &followers {
            assert_eq!(f.state_hash(), want, "seed {seed}");
        }
        assert_eq!(
            late.unwrap().state_hash(),
            want,
            "seed {seed}: snapshot replica"
        );
    }
}

/// A client on the daemon's wire, attached to `main`: it sends what the test says (focus, acks)
/// and keeps every message the daemon sends it.
struct RawClient {
    id: u64,
    stream: UnixStream,
    got: Arc<Mutex<Vec<DaemonMsg>>>,
}

impl RawClient {
    fn attach(env: &TestEnv, facts: ClientFacts) -> RawClient {
        let stream = UnixStream::connect(&env.socket).unwrap();
        let hello = Hello::new(ClientKind::Headless, facts);
        let (client, welcome) = Client::handshake(stream, &hello).unwrap();
        let stream = client.into_inner();
        let got = Arc::new(Mutex::new(Vec::new()));
        let mut reader = stream.try_clone().unwrap();
        let sink = got.clone();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            while let Ok(Some(msg)) = read_msg::<DaemonMsg>(&mut reader, &mut buf) {
                sink.lock().unwrap().push(msg);
            }
        });
        let raw = RawClient {
            id: welcome.client,
            stream,
            got,
        };
        raw.send(&ClientMsg::Attach {
            window_key: "main".into(),
        });
        raw
    }

    fn send(&self, msg: &ClientMsg) {
        write_msg(&mut &self.stream, msg).unwrap();
    }

    /// The last presence message, once one satisfies `done`.
    fn presence(
        &self,
        done: impl Fn(&[auk_mirror::ClientInfo]) -> bool,
    ) -> Vec<auk_mirror::ClientInfo> {
        wait_for("a presence message", || {
            self.got
                .lock()
                .unwrap()
                .iter()
                .rev()
                .find_map(|m| match m {
                    DaemonMsg::Presence { clients } => Some(clients.clone()),
                    _ => None,
                })
                .filter(|c| done(c))
        })
    }
}

impl Drop for RawClient {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

/// Waits until the daemon names `client` as the pane's primary.
fn wait_primary(env: &TestEnv, pane: u64, client: u64) {
    wait_for(
        &format!("client {client} to be pane {pane}'s primary"),
        || {
            env.daemon("clients")
                .as_array()?
                .iter()
                .any(|c| {
                    c["id"] == client
                        && c["primary_for"]
                            .as_array()
                            .is_some_and(|p| p.contains(&json!(pane)))
                })
                .then_some(())
        },
    );
}

fn acks(bodies: &[Value]) -> Vec<u64> {
    events(bodies, "ack")
        .iter()
        .filter_map(|a| a["s"].as_u64())
        .collect()
}

#[test]
fn only_the_primary_clients_acks_count_and_client_facts_follow_the_primary() {
    let env = env("primaryacks");
    env.start_daemon();
    let probe = Probe::start(&env, &[]);
    let pane = probe.pane;
    let x = RawClient::attach(&env, ClientFacts::default());
    let y = RawClient::attach(
        &env,
        ClientFacts {
            cell_px: [10, 20],
            dark: true,
            ..ClientFacts::default()
        },
    );
    x.send(&ClientMsg::Focus { pane });
    wait_primary(&env, pane, x.id);
    probe.write(&tsp_hello(&[]));
    probe.write(&open_with(
        "s1",
        "flow",
        json!([{"id": "t", "k": "text", "p": {"text": "hi"}}]),
    ));
    // The hello's reply, then nothing more: neither client has drawn frame 1.
    let bodies = probe.fence_tsp();
    assert!(acks(&bodies).is_empty(), "{bodies:?}");

    // A client that isn't the pane's primary acks: the program hears nothing.
    y.send(&ClientMsg::Ack {
        pane,
        sf: "s1".into(),
        s: 1,
    });
    std::thread::sleep(Duration::from_millis(500));
    let bodies = probe.fence_tsp();
    assert!(
        acks(&bodies).is_empty(),
        "a non-primary ack reached the program: {bodies:?}"
    );
    // The primary's ack does.
    x.send(&ClientMsg::Ack {
        pane,
        sf: "s1".into(),
        s: 1,
    });
    let input = probe.read_until("the primary's ack", |b| {
        let items = auk_conformance::probe::split_input(b);
        !acks(&auk_conformance::probe::tsp_bodies(&items)).is_empty()
    });
    let bodies = auk_conformance::probe::tsp_bodies(&auk_conformance::probe::split_input(&input));
    assert_eq!(acks(&bodies), [1]);

    // Y becomes the primary: the program now sees Y's facts (dark, its cell size)...
    y.send(&ClientMsg::Focus { pane });
    wait_primary(&env, pane, y.id);
    let input = probe.read_until("Y's theme", |b| {
        let bodies = auk_conformance::probe::tsp_bodies(&auk_conformance::probe::split_input(b));
        events(&bodies, "theme").iter().any(|t| t["dark"] == true)
    });
    let bodies = auk_conformance::probe::tsp_bodies(&auk_conformance::probe::split_input(&input));
    assert!(
        events(&bodies, "theme").contains(&&json!({"ev": "theme", "dark": true})),
        "{bodies:?}"
    );
    // ...and only Y's acks count.
    probe.write(&frame("s1", 2, json!([["set", "t", {"text": "two"}]])));
    x.send(&ClientMsg::Ack {
        pane,
        sf: "s1".into(),
        s: 2,
    });
    std::thread::sleep(Duration::from_millis(500));
    let bodies = probe.fence_tsp();
    assert!(
        acks(&bodies).is_empty(),
        "the old primary's ack reached the program: {bodies:?}"
    );
    y.send(&ClientMsg::Ack {
        pane,
        sf: "s1".into(),
        s: 2,
    });
    probe.read_until("the new primary's ack", |b| {
        let bodies = auk_conformance::probe::tsp_bodies(&auk_conformance::probe::split_input(b));
        acks(&bodies) == [2]
    });

    // X again: its facts come back.
    x.send(&ClientMsg::Focus { pane });
    wait_primary(&env, pane, x.id);
    probe.read_until("X's theme", |b| {
        let bodies = auk_conformance::probe::tsp_bodies(&auk_conformance::probe::split_input(b));
        events(&bodies, "theme").iter().any(|t| t["dark"] == false)
    });
}

/// The TSP bodies the terminal wrote to the program, once `done` holds for them.
fn program_reads(probe: &Probe, what: &str, done: impl Fn(&[Value]) -> bool) -> Vec<Value> {
    let input = probe.read_until(what, |b| {
        done(&auk_conformance::probe::tsp_bodies(
            &auk_conformance::probe::split_input(b),
        ))
    });
    auk_conformance::probe::tsp_bodies(&auk_conformance::probe::split_input(&input))
}

#[test]
fn cell_size_reduced_motion_and_the_12_hour_clock_follow_the_primary_client() {
    let env = env("primaryfacts");
    env.start_daemon();
    let probe = Probe::start(&env, &[]);
    let pane = probe.pane;
    let x = RawClient::attach(&env, ClientFacts::default());
    let y = RawClient::attach(
        &env,
        ClientFacts {
            cell_px: [10, 20],
            dark: false,
            reduce_motion: true,
            hour12: true,
            colors: None,
        },
    );
    x.send(&ClientMsg::Focus { pane });
    wait_primary(&env, pane, x.id);
    // X's facts: the hello reply, and the first-draw resize when X acks the first frame.
    probe.write(&tsp_hello(&[]));
    let bodies = program_reads(&probe, "the hello reply", |b| {
        b.iter().any(|v| v["r"] == "hello")
    });
    let reply = bodies.iter().find(|v| v["r"] == "hello").unwrap();
    assert_eq!(
        (&reply["cell"], &reply["reduceMotion"], &reply["hour12"]),
        (&json!({"w": 8, "h": 16}), &json!(false), &json!(false)),
        "{reply}"
    );
    probe.write(&open_with(
        "s1",
        "flow",
        json!([{"id": "t", "k": "text", "p": {"text": "hi"}}]),
    ));
    x.send(&ClientMsg::Ack {
        pane,
        sf: "s1".into(),
        s: 1,
    });
    program_reads(&probe, "X's first-draw resize", |b| {
        events(b, "resize")
            .iter()
            .any(|r| r["cell"] == json!({"w": 8, "h": 16}))
    });

    // Y becomes the primary: the program is told of its cell size and reduced motion...
    y.send(&ClientMsg::Focus { pane });
    wait_primary(&env, pane, y.id);
    let bodies = program_reads(&probe, "Y's cell size and motion", |b| {
        events(b, "resize")
            .iter()
            .any(|r| r["cell"] == json!({"w": 10, "h": 20}))
            && !events(b, "motion").is_empty()
    });
    assert!(
        events(&bodies, "motion").contains(&&json!({"ev": "motion", "reduce": true})),
        "{bodies:?}"
    );
    // ...asking again gets all of Y's facts, the 12-hour clock included...
    probe.write(&tsp_hello(&[]));
    let bodies = program_reads(&probe, "the second hello reply", |b| {
        b.iter().any(|v| v["r"] == "hello")
    });
    let reply = bodies.iter().find(|v| v["r"] == "hello").unwrap();
    assert_eq!(
        (&reply["cell"], &reply["reduceMotion"], &reply["hour12"]),
        (&json!({"w": 10, "h": 20}), &json!(true), &json!(true)),
        "{reply}"
    );
    // ...and the terminal's own cell-size report (CSI 16 t) is Y's.
    probe.write(b"\x1b[16t");
    let input = probe.read_until("the cell-size report", |b| {
        b.windows(10).any(|w| w == b"\x1b[6;20;10t")
    });
    assert!(!input.is_empty());
    let tsp = env.daemon(&format!("tsp {pane}"));
    assert_eq!(
        (&tsp["facts"]["reduce_motion"], &tsp["facts"]["hour12"]),
        (&json!(true), &json!(true)),
        "{tsp}"
    );

    // X again: its facts come back.
    x.send(&ClientMsg::Focus { pane });
    wait_primary(&env, pane, x.id);
    let bodies = program_reads(&probe, "X's cell size and motion", |b| {
        events(b, "resize")
            .iter()
            .any(|r| r["cell"] == json!({"w": 8, "h": 16}))
            && !events(b, "motion").is_empty()
    });
    assert!(
        events(&bodies, "motion").contains(&&json!({"ev": "motion", "reduce": false})),
        "{bodies:?}"
    );
    probe.write(b"\x1b[16t");
    probe.read_until("X's cell-size report", |b| {
        b.windows(9).any(|w| w == b"\x1b[6;16;8t")
    });
    let tsp = env.daemon(&format!("tsp {pane}"));
    assert_eq!(tsp["facts"]["hour12"], false, "{tsp}");
}

/// What the terminal answered an OSC color query (`11`, `10`, `4;1`): its `rgb:` value.
fn osc_color(probe: &Probe, query: &str) -> String {
    probe.write(format!("\x1b]{query};?\x07").as_bytes());
    let head = format!("\x1b]{query};rgb:");
    let input = probe.read_until(&format!("the OSC {query} answer"), |b| {
        let text = String::from_utf8_lossy(b);
        text.find(&head)
            .is_some_and(|at| text[at..].contains('\x07'))
    });
    let text = String::from_utf8_lossy(&input);
    let at = text.find(&head).unwrap() + head.len() - "rgb:".len();
    text[at..].split('\x07').next().unwrap().to_owned()
}

/// Whether an `rgb:rrrr/gggg/bbbb` answer is a dark color, by the same luminance rule as the
/// daemon's `Facts::is_dark`.
fn reads_dark(rgb: &str) -> bool {
    let parts: Vec<u8> = rgb
        .trim_start_matches("rgb:")
        .split('/')
        .map(|c| u8::from_str_radix(&c[..2], 16).unwrap())
        .collect();
    let facts = auk_term::Facts {
        background: auk_term::Rgb(parts[0], parts[1], parts[2]),
        ..auk_term::Facts::default()
    };
    facts.is_dark()
}

fn themed(dark: bool) -> ClientFacts {
    ClientFacts {
        dark,
        colors: Some(ThemeColors::from(&auk_term::Facts::for_appearance(dark))),
        ..ClientFacts::default()
    }
}

#[test]
fn osc11_answers_the_primary_clients_background() {
    let env = env("osccolors");
    env.start_daemon();
    // X, dark, is attached before the pane opens: the program's first query gets its colors,
    // with no focus or input from X. (The probe asks only once the pane has X as a viewer, so
    // this doesn't tell the pane's seed facts from X's attach.)
    let x = RawClient::attach(&env, themed(true));
    x.presence(|c| c.iter().any(|c| c.id == x.id && c.window_key.is_some()));
    let probe = Probe::start(&env, &[]);
    let pane = probe.pane;
    assert_eq!(osc_color(&probe, "11"), "rgb:1616/1818/1a1a");
    assert_eq!(osc_color(&probe, "10"), "rgb:d8d8/dada/dbdb");
    assert_eq!(osc_color(&probe, "4;1"), "rgb:cccc/6666/6666");
    // The hello's `dark` agrees with OSC 11.
    probe.write(&tsp_hello(&[]));
    let bodies = program_reads(&probe, "the hello reply", |b| {
        b.iter().any(|v| v["r"] == "hello")
    });
    let reply = bodies.iter().find(|v| v["r"] == "hello").unwrap();
    assert_eq!(reply["dark"], json!(true), "{reply}");
    assert!(reads_dark(&osc_color(&probe, "11")));

    // X turns light: the answers follow, and so does the hello's appearance (a theme event, to
    // a program with a surface open).
    probe.write(&open_with(
        "s1",
        "flow",
        json!([{"id": "t", "k": "text", "p": {"text": "hi"}}]),
    ));
    x.send(&ClientMsg::Facts(themed(false)));
    program_reads(&probe, "the light theme event", |b| {
        events(b, "theme").iter().any(|t| t["dark"] == false)
    });
    assert_eq!(osc_color(&probe, "11"), "rgb:fbfb/fbfb/fafa");
    assert_eq!(osc_color(&probe, "10"), "rgb:1f1f/2323/2828");
    assert_eq!(osc_color(&probe, "4;1"), "rgb:c9c9/1b1b/0000");
    assert!(!reads_dark(&osc_color(&probe, "11")));

    // Y names only its appearance (an older client): the answers are Auk's dark colors. Z's
    // theme has its own red, which OSC 4 then answers.
    let y = RawClient::attach(
        &env,
        ClientFacts {
            dark: true,
            ..ClientFacts::default()
        },
    );
    y.send(&ClientMsg::Focus { pane });
    wait_primary(&env, pane, y.id);
    assert_eq!(osc_color(&probe, "11"), "rgb:1616/1818/1a1a");
    let mut own = themed(true);
    own.colors.as_mut().unwrap().palette[1] = [0x12, 0x34, 0x56];
    let z = RawClient::attach(&env, own);
    z.send(&ClientMsg::Focus { pane });
    wait_primary(&env, pane, z.id);
    assert_eq!(osc_color(&probe, "4;1"), "rgb:1212/3434/5656");
}

#[test]
fn a_new_pane_gets_its_clients_appearance_in_colorfgbg() {
    let env = env("colorfgbg");
    env.start_daemon();
    let script = "printf '<%s>' \"${COLORFGBG-unset}\" > fgbg.txt";
    let file = env.home.join("fgbg.txt");
    let read = |what: &str| {
        let _ = std::fs::remove_file(&file);
        env.new_pane(&["--", script]);
        wait_for(what, || {
            std::fs::read_to_string(&file)
                .ok()
                .filter(|t| t.ends_with('>'))
        })
    };
    // No client: none is set.
    assert_eq!(read("COLORFGBG with no client"), "<unset>");
    // Nor for a client's facts without colors (the app's defaults before its window draws).
    let w = RawClient::attach(
        &env,
        ClientFacts {
            dark: true,
            ..ClientFacts::default()
        },
    );
    w.presence(|c| c.iter().any(|c| c.id == w.id && c.window_key.is_some()));
    assert_eq!(read("COLORFGBG for facts without colors"), "<unset>");
    let w_id = w.id;
    drop(w);
    let x = RawClient::attach(&env, themed(true));
    x.presence(|c| {
        c.iter().any(|c| c.id == x.id && c.window_key.is_some()) && !c.iter().any(|c| c.id == w_id)
    });
    assert_eq!(read("COLORFGBG for a dark client"), "<15;0>");
    x.send(&ClientMsg::Facts(themed(false)));
    wait_for("the light facts", || {
        (read("COLORFGBG for a light client") == "<0;15>").then_some(())
    });
}

#[test]
fn clear_scrollback_from_a_window_reaches_every_replica_and_ends_its_surfaces() {
    let env = env("clearsb");
    env.start_daemon();
    let a = env.start_headless("a", &[]);
    let b = env.start_headless("b", &[]);
    let probe = Probe::start(&env, &[]);
    let pane = probe.pane;
    probe.write(&tsp_hello(&[]));
    probe.fence_tsp();
    // A flow surface, then enough lines to push it and the first lines into scrollback.
    probe.write(&open_with(
        "c",
        "flow",
        json!([{"id": "t", "k": "text", "p": {"text": "anchored"}}]),
    ));
    let mut lines = Vec::new();
    for i in 0..60 {
        lines.extend(format!("old-line-{i:02}.\r\n").into_bytes());
    }
    probe.write(&lines);
    // The daemon reads the pty on its own schedule: the fence has it parse all of it first.
    probe.fence();
    converge(&env, &a, pane);
    converge(&env, &b, pane);
    assert!(a.capture(pane, true).contains("old-line-00."));
    assert!(
        b.request(&format!("tsp {pane}"))["surfaces"]
            .to_string()
            .contains("\"c\"")
    );

    // Window A clears the pane's scrollback.
    a.request(&format!("clear-scrollback {pane}"));
    let bodies = probe.fence_tsp();
    assert!(
        bodies.contains(&json!({"ev": "gone", "sf": "c", "ids": ["c"]})),
        "the anchored surface didn't end: {bodies:?}"
    );
    let at = converge(&env, &a, pane);
    assert_eq!(converge(&env, &b, pane), at);
    let daemon_text = env.daemon(&format!("capture {pane} --scrollback"))["text"]
        .as_str()
        .unwrap()
        .to_owned();
    for (who, text, tsp) in [
        (
            "the daemon",
            daemon_text,
            env.daemon(&format!("tsp {pane}")),
        ),
        (
            "window a",
            a.capture(pane, true),
            a.request(&format!("tsp {pane}")),
        ),
        (
            "window b",
            b.capture(pane, true),
            b.request(&format!("tsp {pane}")),
        ),
    ] {
        assert!(
            !text.contains("old-line-00."),
            "{who} kept the scrollback: {text}"
        );
        // The screen stays.
        assert!(text.contains("old-line-59."), "{who}: {text}");
        assert_eq!(tsp["surfaces"], json!([]), "{who}: {tsp}");
    }
    assert!(a.errors().is_empty() && b.errors().is_empty());
}

#[test]
fn an_older_client_is_refused_and_offered_an_upgrade() {
    let env = env("older");
    env.start_daemon();
    let mut hello = Hello::new(ClientKind::App, ClientFacts::default());
    hello.versions.auk = "0.0.0-older".into();
    // An older client never upgrades the daemon to itself.
    hello.exe = None;
    let stream = UnixStream::connect(&env.socket).unwrap();
    let err = match Client::handshake(stream, &hello) {
        Err(err @ HandshakeError::Refused(_)) => err,
        other => panic!("expected a refusal, got {other:?}"),
    };
    let HandshakeError::Refused(refusal) = &err else {
        unreachable!()
    };
    assert!(!refusal.upgrading, "{refusal:?}");
    assert!(refusal.reason.contains("0.0.0-older"), "{}", refusal.reason);
    // What the client shows: the refusal, and the release to upgrade to.
    let shown = err.to_string();
    let offer = format!("upgrade to Auk {}", Versions::current().auk);
    assert!(shown.contains(&offer), "{shown}");
    assert!(
        shown.starts_with("the daemon refused this client"),
        "{shown}"
    );
    // The daemon is unchanged and keeps serving current clients.
    let stream = UnixStream::connect(&env.socket).unwrap();
    let hello = Hello::new(ClientKind::Headless, ClientFacts::default());
    assert!(Client::handshake(stream, &hello).is_ok());
}

#[test]
fn presence_lists_the_attached_clients_with_their_identity() {
    let env = env("presence");
    env.start_daemon();
    let x = RawClient::attach(&env, ClientFacts::default());
    let y = RawClient::attach(&env, ClientFacts::default());
    // Every client hears who is attached to the host, itself included.
    let seen = x.presence(|c| c.len() == 2 && c.iter().all(|c| c.window_key.is_some()));
    let ids: Vec<u64> = seen.iter().map(|c| c.id).collect();
    assert_eq!(ids, [x.id, y.id]);
    let user = std::process::Command::new("id")
        .arg("-un")
        .output()
        .unwrap();
    let user = String::from_utf8_lossy(&user.stdout).trim().to_owned();
    for c in &seen {
        assert_eq!(c.kind, ClientKind::Headless);
        assert_eq!(c.window_key.as_deref(), Some("main"));
        let [hop] = c.identity.as_slice() else {
            panic!("one local hop: {c:?}")
        };
        assert_eq!(hop.method, "local");
        assert!(hop.acting.starts_with(&format!("{user}@")), "{hop:?}");
        assert_eq!(hop.principal, hop.acting);
    }
    // A headless window shows the same, and the control endpoint lists identities too.
    let w = env.start_headless("w", &[]);
    let shown = wait_for("the window's presence", || {
        let p = w.request("presence");
        (p.as_array()?.len() == 3).then_some(p)
    });
    assert_eq!(shown[2]["identity"][0]["method"], "local", "{shown}");
    let clients = env.daemon("clients");
    assert_eq!(clients[0]["identity"][0]["method"], "local", "{clients}");
    // A client that leaves is gone from the next presence message.
    drop(y);
    let seen = x.presence(|c| c.len() == 2 && !c.iter().any(|c| c.id == ids[1]));
    assert!(seen.iter().any(|c| c.id == x.id));
}
