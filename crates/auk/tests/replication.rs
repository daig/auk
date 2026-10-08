//! Replication (GOAL.md, §15): replicas agree (AC2), clients mirror each other (AC8), late
//! attaches get the live state, and mismatched versions are refused (X9).

use std::os::unix::net::UnixStream;

use auk_conformance::harness::{Headless, TestEnv, wait_for};
use auk_mirror::{
    Client, ClientFacts, ClientKind, DaemonMsg, HandshakeError, Hello, Versions, read_msg,
};
use auk_replica::{Item, Record, Replica, Role, Settings};

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

    // The client that typed is the pane's primary (§15.5); typing in B moves it.
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
    assert_eq!(converge(&env, &a, pane).1, env.daemon_hash(pane).1);
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
