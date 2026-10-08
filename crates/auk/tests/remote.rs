//! The `remote` suite (GOAL.md; spec §10.2, §12.5, §15.6, M12, AC16): local headless windows
//! attach over SSH (`ssh … auk attach --stdio`) to a daemon on the Lima VM `auk-linux`, which
//! runs the Linux build of `auk` that each run builds in the VM. So this suite is also the Linux
//! check (D7).
//!
//! The VM is reached with Lima's generated SSH config; the suite starts it if it's stopped and
//! fails, saying what to do, if Lima or the VM is missing (`auk_conformance::remote`).

use std::io::{self, BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};

use auk_conformance::harness::{self, Headless, wait_for};
use auk_conformance::remote::RemoteEnv;
use auk_mirror::{
    Client, ClientFacts, ClientKind, ClientMsg, DaemonMsg, HandshakeError, Hello, Versions,
};
use auk_replica::{Replica, Role};
use serde_json::Value;

fn env(name: &str) -> RemoteEnv {
    RemoteEnv::new(env!("CARGO_BIN_EXE_auk"), name)
}

/// The test shell's prompt (`PS1='%~ %# '`).
const PROMPT: &str = "~ %";

fn status(window: &Headless) -> Value {
    window.request("remote")
}

fn label(window: &Headless) -> String {
    status(window)["label"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

fn wait_label(window: &Headless, want: &str) -> Value {
    wait_for(&format!("the link to be {want}"), || {
        let st = status(window);
        (st["label"] == want).then_some(st)
    })
}

/// Types a line and Enter into a pane through the window; returns the two replies.
fn type_line(window: &Headless, pane: u64, line: &str) -> (Value, Value) {
    let text = window.request(&format!("send {pane} text {}", harness::quote(line)));
    let enter = window.request(&format!("send {pane} keys Enter"));
    (text, enter)
}

fn signal(pid: u64, sig: &str) {
    let ok = Command::new("/bin/kill")
        .args([sig, &pid.to_string()])
        .status()
        .is_ok_and(|s| s.success());
    assert!(ok, "kill {sig} {pid}");
}

/// A window's text, with trailing blanks dropped, for comparing with the daemon's.
fn normalized(text: &str) -> String {
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    lines.join("\n").trim_end().to_owned()
}

#[test]
fn ac16_remote_pane_is_owned_by_the_remote_daemon_and_survives_the_local_window() {
    let r = env("ac16");
    let pane = r.new_pane(&[]);
    let w = r.window("w1");
    r.window_shows(&w, pane, PROMPT);
    type_line(&w, pane, "echo before-$((40+2))");
    r.window_shows(&w, pane, "\nbefore-42");

    // The remote daemon owns the pane: its process runs on the VM, and nothing local serves it.
    let info = r.pane_info(pane);
    let pid = info["pid"].as_u64().expect("the pane's pid");
    assert_eq!(r.vm.ssh_ok(&format!("cat /proc/{pid}/comm")), "zsh");
    assert!(
        !harness::ping(&r.local.control_path()),
        "a local daemon is running; the window should need none"
    );

    // Kill the local window. Its link ends, and the bridge on the VM with it.
    r.local.kill9(w.pid);
    let bridge = format!("{}/auk attach", r.root);
    wait_for("the remote bridge to end", || {
        (!r.remote_running(&bridge)).then_some(())
    });

    // The pane lives on, the same process, and takes more work.
    let info = r.pane_info(pane);
    assert_eq!(info["pid"].as_u64(), Some(pid));
    assert!(info["exit"].is_null(), "the pane exited: {info}");
    r.vm.ssh_ok(&format!("test -d /proc/{pid}"));
    r.run(pane, "echo after-$((50+5))");
    let text = r.capture(pane);
    assert!(
        text.contains("\nbefore-42") && text.contains("\nafter-55"),
        "{text}"
    );

    // A new window shows it all, scrollback included.
    let w2 = r.window("w2");
    let shown = r.window_shows(&w2, pane, "\nafter-55");
    assert!(shown.contains("\nbefore-42"), "{shown}");
    assert!(w2.errors().is_empty(), "{:?}", w2.errors());
}

#[test]
fn attach_mirrors_the_remote_panes_and_typing_reaches_the_remote_shell() {
    let r = env("mirror");
    let pane = r.new_pane(&[]);
    r.run(pane, "echo from-the-vm-$((6*7))");
    let w = r.window("w");
    r.window_shows(&w, pane, "\nfrom-the-vm-42");
    wait_for("the window to match the remote daemon", || {
        (normalized(&w.capture(pane, true)) == normalized(&r.capture(pane))).then_some(())
    });

    // Typed input reaches the remote shell, a Linux one.
    let (text, enter) = type_line(&w, pane, "uname -s; echo typed-$((40+2))");
    assert!(
        text.is_null() && enter.is_null(),
        "input was held: {text} {enter}"
    );
    let shown = r.window_shows(&w, pane, "\ntyped-42");
    assert!(shown.contains("\nLinux\n"), "{shown}");
    r.wait(pane, "prompt");
    let remote = r.capture(pane);
    assert!(
        remote.contains("\nLinux\n") && remote.contains("\ntyped-42"),
        "{remote}"
    );

    // A pane the host opens later appears in the window.
    let home = format!("{}/home", r.root);
    let tab: u64 = r
        .auk_ok(&["new", "tab", "--cwd", &home])
        .parse()
        .expect("a pane id");
    r.wait(tab, "prompt");
    r.window_shows(&w, tab, PROMPT);
    assert!(w.errors().is_empty(), "{:?}", w.errors());
    assert_eq!(label(&w), "connected");
}

#[test]
fn reconnects_and_resyncs_after_the_link_drops() {
    let r = env("reconnect");
    let pane = r.new_pane(&[]);
    let w = r.window("w");
    r.window_shows(&w, pane, PROMPT);
    let before = wait_label(&w, "connected");
    let ssh = before["ssh_pid"].as_u64().expect("the ssh pid");

    // Drop the link, and hold the next connection at the gate so the outage lasts.
    r.vm.ssh_ok(&format!("touch {}", r.gate()));
    signal(ssh, "-KILL");
    wait_label(&w, "reconnecting(1)");

    // The pane goes on without the window.
    r.run(pane, "echo during-outage-$((3*3))");

    // Typing waits for the resync.
    let (text, enter) = type_line(&w, pane, "echo typed-while-down");
    assert_eq!(text["held"], true, "{text}");
    assert_eq!(enter["held"], true, "{enter}");
    let st = status(&w);
    assert_eq!(st["held"], 2, "{st}");
    assert_eq!(st["label"], "reconnecting(1)", "{st}");
    assert!(!r.capture(pane).contains("typed-while-down"));

    // Open the gate: the link reconnects, resyncs from a fresh snapshot, then sends the typing.
    r.vm.ssh_ok(&format!("rm {}", r.gate()));
    let after = wait_label(&w, "connected");
    let shown = r.window_shows(&w, pane, "\ntyped-while-down");
    assert!(shown.contains("\nduring-outage-9"), "{shown}");
    assert_eq!(after["reconnects"], 1, "{after}");
    assert_eq!(after["held"], 0, "{after}");
    assert_ne!(after["ssh_pid"].as_u64(), Some(ssh));
    let history: Vec<&str> = after["history"]
        .as_array()
        .expect("history")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let at = history
        .iter()
        .position(|s| *s == "reconnecting(1)")
        .unwrap_or_else(|| panic!("no reconnecting(1) in {history:?}"));
    assert_eq!(history.last(), Some(&"connected"), "{history:?}");
    assert!(at < history.len() - 1);
    r.wait(pane, "prompt");
    assert!(r.capture(pane).contains("\ntyped-while-down"));
    wait_for("the window to match the remote daemon", || {
        (normalized(&w.capture(pane, true)) == normalized(&r.capture(pane))).then_some(())
    });
    assert!(w.errors().is_empty(), "{:?}", w.errors());
}

/// Continues a stopped process when the test ends, however it ends.
struct Continue(u64);

impl Drop for Continue {
    fn drop(&mut self) {
        let _ = Command::new("/bin/kill")
            .args(["-CONT", &self.0.to_string()])
            .status();
    }
}

#[test]
fn a_silent_link_shows_stalled_until_it_answers() {
    let r = env("stall");
    let pane = r.new_pane(&[]);
    let w = r.window("w");
    r.window_shows(&w, pane, PROMPT);
    let ssh = wait_label(&w, "connected")["ssh_pid"]
        .as_u64()
        .expect("the ssh pid");

    signal(ssh, "-STOP");
    let resume = Continue(ssh);
    let (text, _) = type_line(&w, pane, "echo after-the-stall");
    assert!(text.is_null(), "input was held on a live link: {text}");
    wait_label(&w, "stalled");

    drop(resume);
    wait_label(&w, "connected");
    r.window_shows(&w, pane, "\nafter-the-stall");
    assert_eq!(status(&w)["reconnects"], 0);
}

#[test]
fn identity_chain_shows_the_ssh_key_hop() {
    let r = env("identity");
    // No remote daemon runs yet: the bridge starts one.
    let w = r.window("w");
    let st = wait_label(&w, "connected");
    let host = &st["host"];
    assert_eq!(host["started_daemon"], true, "{st}");
    assert_eq!(host["os"], "linux", "{st}");
    assert_eq!(host["arch"], "aarch64", "{st}");
    assert!(r.remote_running(&format!("{} daemon", r.auk)));

    let chain = st["identity"].as_array().expect("an identity chain");
    assert_eq!(chain.len(), 1, "{st}");
    let hop = &chain[0];
    assert_eq!(hop["method"], "ssh-key", "{hop}");

    // The key is the one ssh's own configuration names, as ssh-keygen reads it.
    let config = Command::new("ssh")
        .arg("-G")
        .arg("-F")
        .arg(&r.vm.ssh_config)
        .arg(&r.vm.host)
        .output()
        .expect("ssh -G");
    let config = String::from_utf8_lossy(&config.stdout);
    let identity_file = config
        .lines()
        .find_map(|l| l.strip_prefix("identityfile "))
        .expect("an identity file in the VM's ssh config")
        .trim()
        .replacen('~', &std::env::var("HOME").expect("HOME"), 1);
    let keygen = Command::new("ssh-keygen")
        .arg("-lf")
        .arg(format!("{identity_file}.pub"))
        .output()
        .expect("ssh-keygen");
    let keygen = String::from_utf8_lossy(&keygen.stdout).trim().to_owned();
    // `256 SHA256:… comment (ED25519)`
    let words: Vec<&str> = keygen.split_whitespace().collect();
    let kind = words.last().expect("a key type").trim_matches(['(', ')']);
    assert_eq!(
        hop["key"],
        format!("{kind} {}", words[1]),
        "{hop} vs {keygen}"
    );
    assert_eq!(
        hop["principal"],
        words[2..words.len() - 1].join(" "),
        "{hop}"
    );

    // From this machine, as the VM's user on the VM.
    let hostname = Command::new("hostname").output().expect("hostname");
    assert_eq!(
        hop["origin"],
        String::from_utf8_lossy(&hostname.stdout).trim()
    );
    assert_eq!(hop["acting"], r.vm.ssh_ok("echo \"$(id -un)@$(uname -n)\""));
}

#[test]
fn presence_shows_every_window_of_the_host_with_its_ssh_hop() {
    let r = env("presence");
    r.new_pane(&[]);
    let w1 = r.window("w1");
    let w2 = r.window("w2");
    wait_label(&w1, "connected");
    wait_label(&w2, "connected");
    // Each window hears who else is attached to the host, with their identity (§10.1).
    let seen = wait_for("both windows in the presence list", || {
        let p = w1.request("presence");
        (p.as_array()?.len() == 2).then_some(p)
    });
    let acting = r.vm.ssh_ok("echo \"$(id -un)@$(uname -n)\"");
    let hostname = Command::new("hostname").output().expect("hostname");
    let origin = String::from_utf8_lossy(&hostname.stdout).trim().to_owned();
    for client in seen.as_array().unwrap() {
        assert_eq!(client["kind"], "headless", "{client}");
        assert_eq!(client["window_key"], "main", "{client}");
        let hop = &client["identity"][0];
        assert_eq!(hop["method"], "ssh-key", "{client}");
        assert_eq!(hop["acting"], acting, "{client}");
        assert_eq!(hop["origin"], origin, "{client}");
    }
    // The chain each window shows for the host is the same hop.
    let own = status(&w1)["identity"][0].clone();
    assert_eq!(seen[0]["identity"][0]["key"], own["key"], "{seen} vs {own}");
    // A window that goes away leaves the list.
    r.local.kill9(w2.pid);
    wait_for("one window left in the presence list", || {
        (w1.request("presence").as_array()?.len() == 1).then_some(())
    });
}

#[test]
fn the_link_starts_connecting_and_fails_when_the_host_stops_serving_auk() {
    let r = env("failed");
    let pane = r.new_pane(&[]);
    let w = r.window("w");
    r.window_shows(&w, pane, PROMPT);
    let st = wait_label(&w, "connected");
    let history: Vec<&str> = st["history"]
        .as_array()
        .expect("history")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(history.first(), Some(&"connecting"), "{history:?}");
    let ssh = st["ssh_pid"].as_u64().expect("the ssh pid");

    // Drop the link while the host's `auk` is gone: the reconnect finds no bridge to run.
    r.vm.ssh_ok(&format!("touch {}", r.gate()));
    signal(ssh, "-KILL");
    wait_label(&w, "reconnecting(1)");
    r.vm.ssh_ok(&format!("mv {0} {0}.gone && rm {1}", r.auk, r.gate()));
    let failed = wait_for("the link to fail", || {
        let st = status(&w);
        st["label"]
            .as_str()
            .is_some_and(|l| l.starts_with("failed"))
            .then_some(st)
    });
    let label = failed["label"].as_str().unwrap();
    assert!(label.contains("wasn't found"), "{failed}");
    // A failed link stays failed: no more attempts, and typing isn't held for a resync.
    std::thread::sleep(std::time::Duration::from_secs(1));
    assert_eq!(status(&w)["label"], failed["label"]);
    assert_eq!(status(&w)["reconnects"], 0);
}

/// `ssh`'s stdio as one stream, after the bridge's preamble.
#[derive(Debug)]
struct Duplex {
    reader: BufReader<std::process::ChildStdout>,
    writer: ChildStdin,
}

impl Read for Duplex {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.reader.read(buf)
    }
}

impl Write for Duplex {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.writer.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

/// Runs the bridge over plain `ssh` and reads past its preamble.
fn raw_bridge(r: &RemoteEnv) -> (Child, Duplex) {
    let mut child =
        r.vm.ssh_command()
            .arg(r.remote_command())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("run ssh");
    let mut reader = BufReader::new(child.stdout.take().expect("stdout"));
    let mut line = String::new();
    reader.read_line(&mut line).expect("the preamble");
    assert!(line.starts_with("AUK-BRIDGE 1 {"), "{line:?}");
    let writer = child.stdin.take().expect("stdin");
    (child, Duplex { reader, writer })
}

#[test]
fn the_bridge_carries_the_replication_protocol_unchanged() {
    let r = env("bridge");
    let pane = r.new_pane(&[]);
    r.run(pane, "echo over-the-bridge");

    // A plain mirror client, speaking §15.6 over ssh's stdio.
    let (mut child, stream) = raw_bridge(&r);
    let hello = Hello::new(ClientKind::Remote, ClientFacts::default());
    let (mut client, welcome) = Client::handshake(stream, &hello).expect("handshake");
    assert_eq!(welcome.versions, Versions::current());
    client
        .send(&ClientMsg::Attach {
            window_key: auk_daemon::server::DEFAULT_WINDOW_KEY.to_owned(),
        })
        .expect("attach");
    let replica = loop {
        match client.recv().expect("a message") {
            Some(DaemonMsg::Snapshot { pane: p, snapshot }) if p == pane => {
                break Replica::from_snapshot(&snapshot, Role::Follower, None);
            }
            Some(_) => {}
            None => panic!("the bridge closed before the snapshot"),
        }
    };
    assert!(
        replica.text().contains("\nover-the-bridge"),
        "{}",
        replica.text()
    );
    client.send(&ClientMsg::Detach).expect("detach");
    drop(client);
    let _ = child.wait();

    // A client of another version is refused through the bridge too (X9).
    let (mut child, stream) = raw_bridge(&r);
    let mut hello = Hello::new(ClientKind::Remote, ClientFacts::default());
    hello.versions.protocol += 1;
    match Client::handshake(stream, &hello) {
        Err(HandshakeError::Refused(refusal)) => {
            assert_eq!(refusal.daemon, Versions::current());
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn attaching_to_a_host_without_auk_fails_clearly() {
    let r = env("noauk");
    let control = r.local.root.join("w.ctl");
    let config = r.vm.ssh_config.to_string_lossy().into_owned();
    let out = r
        .local
        .command()
        .arg("headless")
        .arg("--control")
        .arg(&control)
        .args(["--host", &r.vm.host, "--ssh-config", &config])
        .args(["--remote-command", "/nonexistent/auk attach --stdio"])
        .output()
        .expect("run auk headless");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(
        stderr.contains("/nonexistent/auk attach --stdio") && stderr.contains("wasn't found"),
        "{stderr}"
    );
}

/// How many times the remote `deep` plugin's host half has loaded (its kv count).
fn loads(r: &RemoteEnv) -> u64 {
    let kv =
        r.vm.ssh(&format!("cat {}/state/plugin-data/deep/kv.json", r.root));
    serde_json::from_slice::<Value>(&kv.stdout)
        .ok()
        .and_then(|v| v["loads"].as_u64())
        .unwrap_or(0)
}

/// §9.3 "Watching" on Linux: the daemon watches the plugins folder with inotify, 8 levels deep.
/// Editing a file 8 levels below the folder reloads the plugins; editing one 9 levels down
/// doesn't.
#[test]
fn on_linux_inotify_reloads_plugins_on_a_change_8_levels_deep() {
    let r = env("inotify");
    let pkg = format!("{}/state/plugins/deep", r.root);
    // `deep` is level 1; the file in `l7` is level 8, the one in `l8` level 9.
    let l7 = format!("{pkg}/l2/l3/l4/l5/l6/l7");
    let eight = format!("{l7}/eight.txt");
    let nine = format!("{l7}/l8/nine.txt");
    r.write_file(
        &format!("{pkg}/plugin.toml"),
        "schema = 1\nid = \"deep\"\nname = \"Deep\"\nversion = \"1\"\nhost = \"host.luau\"\n",
    );
    r.write_file(
        &format!("{pkg}/host.luau"),
        "tern.kv.set(\"loads\", (tern.kv.get(\"loads\") or 0) + 1)\n",
    );
    r.write_file(&eight, "eight\n");
    r.write_file(&nine, "nine\n");

    // Starting the daemon loads the plugin once.
    let _pane = r.new_pane(&[]);
    wait_for("the plugin's first load", || (loads(&r) == 1).then_some(()));

    // The daemon holds an inotify instance, and it watches the level-8 file's directory.
    let pid = r.ls()["daemon_pid"].as_u64().expect("the daemon's pid");
    let inode = r.vm.ssh_ok(&format!("printf '%x' \"$(stat -c %i {l7})\""));
    let watched = r.vm.ssh_ok(&format!(
        "for fd in /proc/{pid}/fd/*; do \
           if [ \"$(readlink $fd)\" = anon_inode:inotify ]; then cat /proc/{pid}/fdinfo/${{fd##*/}}; fi; \
         done"
    ));
    assert!(
        watched.contains("inotify wd:"),
        "the daemon has no inotify watches:\n{watched}"
    );
    assert!(
        watched
            .lines()
            .any(|l| l.split_whitespace().any(|w| w == format!("ino:{inode}"))),
        "{l7} (inode {inode}) isn't watched:\n{watched}"
    );

    // Level 9: past the depth, no reload (a native reload lands in about 300 ms).
    r.vm.ssh_ok(&format!("echo more >> {nine}"));
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert_eq!(loads(&r), 1, "an edit 9 levels deep reloaded the plugins");

    // Level 8: reloads.
    r.vm.ssh_ok(&format!("echo more >> {eight}"));
    wait_for("the reload after an edit 8 levels deep", || {
        (loads(&r) == 2).then_some(())
    });
    std::thread::sleep(std::time::Duration::from_millis(700));
    assert_eq!(loads(&r), 2, "one edit, one reload");
}

/// The mode gate's counts for a remote pane: (written, dropped).
fn gate_counts(r: &RemoteEnv, pane: u64) -> (u64, u64) {
    let st = r.daemon(&format!("tsp {pane}"))["gate_stats"].clone();
    match (st["written"].as_u64(), st["dropped"].as_u64()) {
        (Some(w), Some(d)) => (w, d),
        _ => panic!("no gate counts: {st}"),
    }
}

/// §12.1 on Linux, where the gate reads the tty's modes from the PTY master: while the program
/// has the tty canonical or echoing, the daemon drops its TSP replies; with both off, the reply
/// reaches the program.
#[test]
fn on_linux_the_mode_gate_drops_tsp_replies_while_the_tty_is_canonical_or_echoing() {
    let r = env("lingate");
    let pane = r.new_pane(&[]);
    let hello = r#"printf '\033_tsp;q;{"q":"hello","v":[1],"app":"sh","ver":"1"}\033\\'"#;
    for (stty, canonical, echo) in [
        ("icanon -echo", true, false),
        ("-icanon echo", false, true),
        ("icanon echo", true, true),
    ] {
        let (written, dropped) = gate_counts(&r, pane);
        r.auk_ok(&[
            "run",
            &pane.to_string(),
            &format!("stty {stty}; {hello}; sleep 3; stty sane"),
        ]);
        wait_for(
            &format!("the gate to drop the reply under `stty {stty}`"),
            || (gate_counts(&r, pane).1 > dropped).then_some(()),
        );
        // The gate's reading while the program runs.
        let tty = r.daemon(&format!("tty-mode {pane}"));
        assert_eq!(
            (&tty["canonical"], &tty["echo"], &tty["tsp_input"]),
            (
                &Value::Bool(canonical),
                &Value::Bool(echo),
                &Value::Bool(false)
            ),
            "{stty}: {tty}"
        );
        assert_eq!(
            gate_counts(&r, pane).0,
            written,
            "{stty}: a reply was written"
        );
        r.wait(pane, "prompt");
    }

    // Non-canonical with echo off: the reply is written, and the program reads it.
    let (written, _) = gate_counts(&r, pane);
    let reply = format!("{}/reply", r.root);
    r.run(
        pane,
        &format!("stty -icanon -echo min 0 time 20; {hello}; head -c 4096 > {reply}; stty sane"),
    );
    assert!(gate_counts(&r, pane).0 > written, "no reply was written");
    let got = r.vm.ssh_ok(&format!("cat {reply}"));
    assert!(
        got.contains("\x1b_tsp;") && got.contains(r#""r":"hello""#),
        "the program read {got:?}"
    );
}
