//! The `cli` suite (GOAL.md, spec §11): the CLI verbs against a real daemon, the pane socket,
//! and headless mode: the control endpoint (`auk serve`, `auk ctl`), scenario scripts with PNG
//! and layout-JSON goldens (`auk shot`), and `surface-play`.
//!
//! Goldens are in `fixtures/goldens/<scenario>/`; `AUK_UPDATE_GOLDENS=1` rewrites them (see
//! `auk_conformance::goldens` for the tolerance).

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use auk_conformance::goldens;
use auk_conformance::harness::{TestEnv, wait_for};
use auk_conformance::probe::{repo_root, require_program};
use serde_json::{Value, json};

const AUK: &str = env!("CARGO_BIN_EXE_auk");

fn ok(out: &Output, what: &str) -> String {
    assert!(
        out.status.success(),
        "{what} failed ({}):\n{}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn cli(env: &TestEnv, args: &[&str]) -> String {
    ok(&env.cli(args), &format!("auk {}", args.join(" ")))
}

fn cli_json(env: &TestEnv, args: &[&str]) -> Value {
    let mut all = vec!["--json"];
    all.extend_from_slice(args);
    let text = cli(env, &all);
    serde_json::from_str(text.trim()).unwrap_or_else(|e| panic!("auk {args:?}: {e}: {text}"))
}

fn ls(env: &TestEnv) -> Value {
    cli_json(env, &["ls"])
}

fn pane_of(ls: &Value, id: u64) -> Option<&Value> {
    ls["panes"].as_array()?.iter().find(|p| p["id"] == id)
}

/// Opens a shell pane through the CLI, in the test's home, at its prompt.
fn new_shell(env: &TestEnv, args: &[&str]) -> u64 {
    let home = env.home.to_string_lossy().into_owned();
    let mut all = vec!["new", "--cwd", home.as_str()];
    all.extend_from_slice(args);
    let id: u64 = cli(env, &all).trim().parse().expect("a pane id");
    cli(
        env,
        &[
            "wait",
            &id.to_string(),
            "--until",
            "prompt",
            "--timeout",
            "20",
        ],
    );
    id
}

fn screen(env: &TestEnv, pane: u64) -> String {
    cli(env, &["capture", &pane.to_string()])
}

fn wait_screen(env: &TestEnv, pane: u64, what: &str) -> String {
    wait_for(&format!("`{what}` in pane {pane}"), || {
        let s = screen(env, pane);
        s.contains(what).then_some(s)
    })
}

// ---------------------------------------------------------------------------------------------
// Verbs

#[test]
fn verbs_new_split_ls_close() {
    let env = TestEnv::new(AUK, "cli-layout");
    let _d = env.start_daemon();
    let first = new_shell(&env, &[]);
    let tab: u64 = cli(&env, &["new", "tab", "--cwd", &env.home.to_string_lossy()])
        .trim()
        .parse()
        .unwrap();
    let split = cli_json(&env, &["split", &first.to_string(), "right"]);
    let right = split["pane"].as_u64().expect("split prints the new pane");
    let down: u64 = cli(
        &env,
        &["split", &right.to_string(), "down", "--", "sleep", "300"],
    )
    .trim()
    .parse()
    .unwrap();
    cli(
        &env,
        &[
            "wait",
            &right.to_string(),
            "--until",
            "prompt",
            "--timeout",
            "20",
        ],
    );

    let l = ls(&env);
    let main = &l["layout"]["main"]["sessions"][0];
    assert_eq!(main["tabs"].as_array().unwrap().len(), 2, "two tabs: {l:#}");
    let root = &main["tabs"][0]["root"];
    assert_eq!(root["node"], "split");
    assert_eq!(root["dir"], "right");
    assert_eq!(root["second"]["dir"], "down", "{root:#}");
    assert_eq!(
        pane_of(&l, down).unwrap()["kind"]["line"],
        "sleep 300",
        "{l:#}"
    );
    // The split pane starts in its neighbour's directory.
    assert_eq!(pane_of(&l, right).unwrap()["cwd"], json!(env.home));
    for id in [first, tab, right, down] {
        assert!(pane_of(&l, id).is_some(), "pane {id} is listed");
    }

    // The table form names every pane.
    let table = cli(&env, &["ls"]);
    assert!(table.starts_with("PANE"), "{table}");
    for id in [first, tab, right, down] {
        assert!(
            table
                .lines()
                .any(|line| line.trim_start().starts_with(&format!("{id} ")))
        );
    }

    // --window filters; another window key gets its own sessions.
    let other: u64 = cli(
        &env,
        &[
            "--window",
            "w2",
            "new",
            "--cwd",
            &env.home.to_string_lossy(),
        ],
    )
    .trim()
    .parse()
    .unwrap();
    let only = cli_json(&env, &["--window", "w2", "ls"]);
    let ids: Vec<u64> = only["panes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_u64().unwrap())
        .collect();
    assert_eq!(ids, [other]);
    assert!(only["layout"].get("main").is_none());

    // close hangs up and takes the pane out of the layout.
    cli(&env, &["close", &down.to_string()]);
    wait_for("the closed pane to leave the layout", || {
        let l = ls(&env);
        let tab0 = &l["layout"]["main"]["sessions"][0]["tabs"][0]["root"];
        (!tab0.to_string().contains(&format!("\"pane\":{down}"))).then_some(())
    });
    let gone = cli(
        &env,
        &[
            "wait",
            &down.to_string(),
            "--until",
            "exit",
            "--timeout",
            "20",
        ],
    );
    assert!(gone.is_empty());
    // Unknown targets fail with a message.
    let bad = env.cli(&["close", "no-such-pane"]);
    assert!(!bad.status.success());
    assert!(String::from_utf8_lossy(&bad.stderr).contains("no pane matches"));
}

#[test]
fn verbs_run_send_wait_capture() {
    let env = TestEnv::new(AUK, "cli-io");
    let _d = env.start_daemon();
    let pane = new_shell(&env, &["--size", "60x12"]);
    let p = pane.to_string();

    cli(
        &env,
        &["run", &p, "printf", "'\\e[1;31mred\\e[0m plain <b>&\\n'"],
    );
    let waited = cli_json(&env, &["wait", &p, "--until", "prompt", "--timeout", "20"]);
    assert_eq!(waited["status"], 0, "{waited}");
    let text = wait_screen(&env, pane, "red plain <b>&");

    // --ansi keeps the attributes as SGR; --html as styled spans, escaped.
    let ansi = cli(&env, &["capture", &p, "--ansi"]);
    assert!(
        ansi.contains("\x1b[0;1;31mred\x1b[0m plain <b>&"),
        "{ansi:?}"
    );
    let html = cli(&env, &["capture", &p, "--html"]);
    assert!(html.starts_with("<pre class=\"auk-capture\">"), "{html}");
    assert!(
        html.contains(
            "<span style=\"color:#cd0000;font-weight:bold\">red</span> plain &lt;b&gt;&amp;"
        ),
        "{html}"
    );
    let j = cli_json(&env, &["capture", &p]);
    assert_eq!(j["pane"], pane);
    assert_eq!(
        (j["cols"].as_u64(), j["rows"].as_u64()),
        (Some(60), Some(12))
    );
    assert_eq!(j["text"].as_str().unwrap().trim_end(), text.trim_end());

    // Scrollback: 40 lines through a 12-row screen.
    cli(&env, &["run", &p, "seq", "1", "40"]);
    cli(&env, &["wait", &p, "--until", "prompt", "--timeout", "20"]);
    wait_screen(&env, pane, "40");
    let visible = screen(&env, pane);
    assert!(!visible.lines().any(|l| l == "1"), "{visible}");
    let all = cli(&env, &["capture", &p, "--scrollback"]);
    assert!(
        all.lines().any(|l| l == "1") && all.lines().any(|l| l == "40"),
        "{all}"
    );

    // send: text, then keys (Enter); paste is bracketed when the shell asks for it.
    cli(&env, &["send", &p, "text", "echo", "typed"]);
    cli(&env, &["send", &p, "keys", "Enter"]);
    wait_screen(&env, pane, "\ntyped");
    cli(&env, &["send", &p, "paste", "echo pasted"]);
    cli(&env, &["send", &p, "keys", "Enter"]);
    wait_screen(&env, pane, "\npasted");
    // C-c interrupts a running program.
    cli(&env, &["run", &p, "sleep", "100"]);
    cli(
        &env,
        &["wait", &p, "--until", "idle=300", "--timeout", "20"],
    );
    cli(&env, &["send", &p, "keys", "C-c"]);
    cli(&env, &["wait", &p, "--until", "prompt", "--timeout", "20"]);

    // A wait that times out exits 124.
    let start = Instant::now();
    let out = env.cli(&["wait", &p, "--until", "exit", "--timeout", "0.5"]);
    assert_eq!(out.status.code(), Some(124), "{out:?}");
    assert!(start.elapsed() < Duration::from_secs(10));
    cli(&env, &["run", &p, "exit", "3"]);
    let exited = cli_json(&env, &["wait", &p, "--until", "exit", "--timeout", "20"]);
    assert_eq!(exited["exit"], 3);
}

#[test]
fn verbs_targets_resolve_and_ties_go_to_the_active_tab() {
    let env = TestEnv::new(AUK, "cli-targets");
    let _d = env.start_daemon();
    let a = new_shell(&env, &[]);
    let b = new_shell(&env, &[]);
    let c: u64 = cli(&env, &["split", &b.to_string(), "right"])
        .trim()
        .parse()
        .unwrap();
    cli(
        &env,
        &[
            "wait",
            &c.to_string(),
            "--until",
            "prompt",
            "--timeout",
            "20",
        ],
    );
    // Titles a running program set (the shell's prompt would set its own).
    let title = |pane: u64, t: &str| {
        cli(
            &env,
            &[
                "run",
                &pane.to_string(),
                &format!("printf '\\e]2;{t}\\a'; cat"),
            ],
        );
    };
    title(a, "Builds");
    title(b, "builds");
    title(c, "Logs");
    for (pane, t) in [(a, "Builds"), (b, "builds"), (c, "Logs")] {
        wait_for("the title", || {
            (pane_of(&ls(&env), pane).unwrap()["title"] == t).then_some(())
        });
    }
    let target_of = |word: &str| cli_json(&env, &["capture", word])["pane"].as_u64().unwrap();
    // Exact title, then any case, then a program name.
    assert_eq!(target_of("Builds"), a);
    assert_eq!(target_of("builds"), b);
    assert_eq!(target_of("logs"), c);
    assert_eq!(target_of(&c.to_string()), c);
    let d: u64 = cli(&env, &["new", "--", "sleep", "300"])
        .trim()
        .parse()
        .unwrap();
    assert_eq!(target_of("sleep"), d);

    // @focused and ties: a window focuses pane c; "BUILDS" matches a and b in any case, and b
    // shares c's tab.
    let none = env.cli(&["capture", "@focused"]);
    assert!(!none.status.success(), "no pane is focused yet");
    let window = env.start_headless("win", &[]);
    wait_for("the window's replicas", || window.hash(c));
    window.request(&format!("focus {c}"));
    wait_for("the focus", || (ls(&env)["focused"] == c).then_some(()));
    assert_eq!(target_of("@focused"), c);
    assert_eq!(target_of("BUILDS"), b, "the active tab wins a tie");
    window.request(&format!("focus {a}"));
    wait_for("the focus", || (ls(&env)["focused"] == a).then_some(()));
    assert_eq!(target_of("BUILDS"), a, "the active tab wins a tie");
}

/// `auk events` as a child process, its lines read on a thread.
struct Events {
    child: Child,
    lines: mpsc::Receiver<Value>,
}

impl Events {
    fn start(env: &TestEnv, filter: Option<&str>) -> Events {
        let before = ls(env)["event_subscribers"].as_u64().unwrap();
        let mut cmd = env.command();
        cmd.arg("events");
        if let Some(f) = filter {
            cmd.args(["--filter", f]);
        }
        let mut child = cmd.stdout(Stdio::piped()).spawn().expect("auk events");
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let v: Value = serde_json::from_str(&line)
                    .unwrap_or_else(|e| panic!("an event line isn't JSON: {line:?}: {e}"));
                if tx.send(v).is_err() {
                    break;
                }
            }
        });
        wait_for("the event stream to subscribe", || {
            (ls(env)["event_subscribers"].as_u64().unwrap() > before).then_some(())
        });
        Events { child, lines }
    }

    /// The next event matching `pred` within 20 s; the ones before it are returned too.
    fn until(&self, what: &str, pred: impl Fn(&Value) -> bool) -> (Value, Vec<Value>) {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut seen = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(v) if pred(&v) => return (v, seen),
                Ok(v) => seen.push(v),
                Err(_) => panic!("no {what} event; saw {seen:#?}"),
            }
        }
    }
}

impl Drop for Events {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn verbs_events_stream_json_lines() {
    let env = TestEnv::new(AUK, "cli-events");
    let _d = env.start_daemon();
    let filtered = Events::start(&env, Some("pane_spawned,layout_changed,pane_exited"));
    let all = Events::start(&env, None);

    let pane = new_shell(&env, &[]);
    let (spawned, _) = filtered.until("pane_spawned", |e| e["event"] == "pane_spawned");
    assert_eq!(spawned["pane"], pane);
    assert_eq!(spawned["window"], "main");
    assert_eq!(spawned["by"], "cli", "{spawned}");
    let (layout, _) = filtered.until("layout_changed", |e| e["event"] == "layout_changed");
    assert_eq!(layout["by"], "cli");

    // Shell integration reaches the stream: a command's start and end, and the title.
    cli(
        &env,
        &["run", &pane.to_string(), "printf '\\e]2;evt\\a'; false"],
    );
    let (finished, before) = all.until("command_finished", |e| e["event"] == "command_finished");
    assert_eq!(
        (finished["pane"].as_u64(), finished["status"].as_i64()),
        (Some(pane), Some(1))
    );
    let started = before
        .iter()
        .find(|e| e["event"] == "command_started")
        .unwrap_or_else(|| panic!("command_started before command_finished: {before:#?}"));
    assert!(
        started["cmdline"].as_str().unwrap().contains("false"),
        "{started}"
    );
    assert!(
        before
            .iter()
            .any(|e| e["event"] == "title_changed" && e["title"] == "evt" && e["pane"] == pane),
        "{before:#?}"
    );

    // A window's client id is `by` for what it does.
    let window = env.start_headless("win", &[]);
    let (connected, _) = all.until("client_connected", |e| e["event"] == "client_connected");
    let client = connected["client"].clone();
    assert_eq!(connected["kind"], "headless");
    all.until("client_attached", |e| {
        e["event"] == "client_attached" && e["client"] == client
    });
    wait_for("the window's replica", || window.hash(pane));
    window.request(&format!("focus {pane}"));
    let (focus, _) = all.until("focus_changed", |e| e["event"] == "focus_changed");
    assert_eq!(
        (focus["pane"].as_u64(), &focus["by"]),
        (Some(pane), &client)
    );

    // Exits arrive without a cause; the filter kept other kinds out.
    cli(&env, &["run", &pane.to_string(), "exit"]);
    let (exited, before) = filtered.until("pane_exited", |e| e["event"] == "pane_exited");
    assert_eq!(
        (exited["pane"].as_u64(), &exited["by"]),
        (Some(pane), &Value::Null)
    );
    for e in &before {
        let kind = e["event"].as_str().unwrap();
        assert!(
            ["pane_spawned", "layout_changed"].contains(&kind),
            "filtered out: {e}"
        );
    }
    // The window leaving is an event too.
    let _ = std::process::Command::new("kill")
        .arg(window.pid.to_string())
        .status();
    all.until("client_left", |e| {
        e["event"] == "client_left" && e["client"] == client
    });
}

#[test]
fn verbs_open_routes_to_a_window_then_the_system_opener() {
    let env = TestEnv::new(AUK, "cli-open");
    let _d = env.start_daemon();
    let file = env.home.join("notes.md");
    std::fs::write(&file, "# notes\n").unwrap();
    // A stand-in system opener that records what it was asked to open.
    let opened = env.root.join("opened.txt");
    let opener = env.root.join("opener.sh");
    std::fs::write(
        &opener,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{}'\n",
            opened.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&opener, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let open = |args: &[&str]| -> Value {
        let mut cmd = env.command();
        cmd.env("AUK_OPENER", &opener).arg("--json").args(args);
        let out = cmd.output().unwrap();
        serde_json::from_str(ok(&out, "auk open").trim()).unwrap()
    };

    // No window: the system opener.
    let v = open(&["open", &file.to_string_lossy()]);
    assert_eq!(v["routed"], "system", "{v}");
    assert_eq!(
        std::fs::read_to_string(&opened).unwrap(),
        format!("{}\n", file.display())
    );

    // A window showing the key takes it (its route.open, then its opener).
    let pane = new_shell(&env, &[]);
    let window = env.start_headless("win", &[]);
    wait_for("the window's replica", || window.hash(pane));
    let v = open(&["open", &file.to_string_lossy()]);
    assert_eq!(
        (v["routed"].as_str(), v["windows"].as_u64()),
        (Some("window"), Some(1)),
        "{v}"
    );
    let effects = wait_for("the open effect", || {
        let e = window.request("effects");
        (!e.as_array().unwrap().is_empty()).then_some(e)
    });
    assert_eq!(effects[0], json!({"effect": "open", "target": file}));
    assert_eq!(
        std::fs::read_to_string(&opened).unwrap().lines().count(),
        1,
        "not opened twice"
    );
    // A window key no window shows falls back to the system opener.
    let v = open(&["--window", "elsewhere", "open", "notes.md"]);
    assert_eq!(v["routed"], "system");
}

#[test]
fn verbs_plugin_lists_and_links() {
    let env = TestEnv::new(AUK, "cli-plugin");
    let _d = env.start_daemon();
    let empty = cli_json(&env, &["plugin", "list"]);
    assert_eq!(
        empty["plugins"].as_array().map(Vec::len),
        Some(0),
        "{empty}"
    );
    let dirvars = repo_root().join("third_party/tern-sdk/plugins/examples/dirvars");
    cli(&env, &["plugin", "link", &dirvars.to_string_lossy()]);
    let listed = cli_json(&env, &["plugin", "list"]);
    assert!(listed.to_string().contains("dirvars"), "{listed}");
    let dir = cli(&env, &["plugin", "dir"]);
    assert!(Path::new(dir.trim()).starts_with(&env.state), "{dir}");
}

#[test]
fn verbs_surface_play_and_capture_surfaces() {
    let env = TestEnv::new(AUK, "cli-play");
    let _d = env.start_daemon();
    let pane = new_shell(&env, &[]);
    let rec = repo_root().join("fixtures/tsp/docs-regions.jsonl");
    let played = cli(
        &env,
        &[
            "surface-play",
            &rec.to_string_lossy(),
            "--pane",
            &pane.to_string(),
        ],
    );
    let played: Value = serde_json::from_str(played.trim()).unwrap();
    assert_eq!(played["frames"], 1, "{played}");
    let text = cli(&env, &["capture", &pane.to_string(), "--surfaces"]);
    for line in [
        "Checked 214 crates.",
        "Run cargo update -p serde?",
        "Ask anything",
        "main ready",
    ] {
        assert!(text.contains(line), "{line} in {text}");
    }
    let plain = screen(&env, pane);
    assert!(
        !plain.contains("Checked 214 crates."),
        "surfaces only with --surfaces"
    );
    let html = cli(
        &env,
        &["capture", &pane.to_string(), "--surfaces", "--html"],
    );
    assert!(
        html.contains("<div class=\"auk-surface\" data-sf=\"s1\" data-mode=\"inline\">"),
        "{html}"
    );
}

#[test]
fn pane_socket_greets_and_answers_by_id() {
    let env = TestEnv::new(AUK, "cli-sock");
    let _d = env.start_daemon();
    let pane = new_shell(&env, &[]);
    let socket = cli(
        &env,
        &["run", &pane.to_string(), "echo SOCK=$AUK_PANE_SOCKET"],
    );
    assert!(socket.is_empty());
    let text = wait_screen(&env, pane, "\nSOCK=/");
    let path = text
        .lines()
        .find_map(|l| l.strip_prefix("SOCK="))
        .expect("the socket path")
        .trim()
        .to_owned();
    let mut s = UnixStream::connect(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    assert_eq!(
        socket_frame(&mut s, &json!({"hello": {}})),
        json!({"welcome": {"ops": []}})
    );
    // v1 offers no ops; unknown members are skipped and the request still gets its answer.
    let reply = socket_frame(
        &mut s,
        &json!({"id": 7, "fork": {"block": 1, "dir": "right"}, "extra": true}),
    );
    assert_eq!(reply["id"], 7);
    assert_eq!(reply["fork"]["error"]["kind"], "unsupported", "{reply}");
    // The answer is keyed by the request's kind, even behind an unknown member that sorts first.
    let reply = socket_frame(
        &mut s,
        &json!({"aaa": [1], "id": 8, "browser": {"op": "open"}}),
    );
    assert_eq!(reply["id"], 8, "{reply}");
    assert_eq!(reply["browser"]["error"]["kind"], "unsupported", "{reply}");
    assert!(reply.get("aaa").is_none(), "{reply}");
    // An unknown kind is skipped: no answer, so the next answer is the next request's.
    socket_send(&mut s, &json!({"id": 9, "teleport": {}}));
    assert_eq!(
        socket_frame(&mut s, &json!({"id": 10, "fork": {}}))["id"],
        10
    );
    // Frames are capped at 256 MiB: a longer one closes the connection.
    s.write_all(&((256u32 << 20) + 1).to_le_bytes()).unwrap();
    let mut rest = Vec::new();
    assert_eq!(
        s.read_to_end(&mut rest).unwrap_or(0),
        0,
        "the connection closes"
    );
    // A fresh connection is served as before.
    let mut s = UnixStream::connect(&path).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    assert_eq!(
        socket_frame(&mut s, &json!({"hello": {}})),
        json!({"welcome": {"ops": []}})
    );
}

fn socket_send(s: &mut UnixStream, msg: &Value) {
    let bytes = serde_json::to_vec(msg).unwrap();
    s.write_all(&(bytes.len() as u32).to_le_bytes()).unwrap();
    s.write_all(&bytes).unwrap();
}

/// Sends one pane-socket frame and reads the next answer.
fn socket_frame(s: &mut UnixStream, msg: &Value) -> Value {
    socket_send(s, msg);
    let mut len = [0; 4];
    s.read_exact(&mut len).unwrap();
    let mut buf = vec![0; u32::from_le_bytes(len) as usize];
    s.read_exact(&mut buf).unwrap();
    serde_json::from_slice(&buf).unwrap()
}

#[test]
fn agent_panes_run_agent_command_through_the_login_shell_with_an_agent_badge() {
    let env = TestEnv::new(AUK, "cli-agent");
    // `agent_command` from the settings file (`$AUK_CONFIG_DIR/settings.json`).
    std::fs::write(
        env.state.join("settings.json"),
        json!({"agent_command": "echo agent-$((6*7)); sleep 300; echo end", "theme": "System"})
            .to_string(),
    )
    .unwrap();
    let _d = env.start_daemon();
    let first = new_shell(&env, &[]);
    let agent = cli_json(&env, &["split", &first.to_string(), "right", "--agent"])["pane"]
        .as_u64()
        .expect("split prints the new pane");
    // The login shell expands the command: it ran through `$SHELL -l -c`.
    wait_screen(&env, agent, "agent-42");
    let l = ls(&env);
    let info = pane_of(&l, agent).unwrap();
    assert_eq!(
        info["kind"],
        json!({"kind": "agent", "line": "echo agent-$((6*7)); sleep 300; echo end"}),
        "{l:#}"
    );
    let pid = info["pid"].as_u64().unwrap();
    let args = Command::new("ps")
        .args(["-o", "args=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    let args = String::from_utf8_lossy(&args.stdout);
    assert!(args.contains("zsh -l -c echo agent-"), "{args}");
    // Every window sees the kind in the layout, for the agent icon (§5.1).
    assert_eq!(
        l["layout"]["main"]["kinds"],
        json!({agent.to_string(): "agent"}),
        "{l:#}"
    );

    // Without the setting, the default is `omp`; changing it doesn't touch running agents.
    std::fs::remove_file(env.state.join("settings.json")).unwrap();
    let out = env.cli(&[
        "--json",
        "new",
        "tab",
        "--agent",
        "--cwd",
        &env.home.to_string_lossy(),
    ]);
    let omp: Value = serde_json::from_slice(&out.stdout).expect("new prints JSON");
    let omp = omp["pane"].as_u64().unwrap();
    let l = ls(&env);
    assert_eq!(pane_of(&l, omp).unwrap()["kind"]["line"], "omp", "{l:#}");
    assert_eq!(
        pane_of(&l, agent).unwrap()["kind"]["line"],
        info["kind"]["line"]
    );
    assert_eq!(
        l["layout"]["main"]["kinds"][omp.to_string()],
        "agent",
        "{l:#}"
    );

    // A window opens one with a layout edit: split right of its focused pane.
    use auk_mirror::{Client, ClientFacts, ClientKind, ClientMsg, Hello, LayoutEdit};
    let stream = UnixStream::connect(&env.socket).unwrap();
    let hello = Hello::new(ClientKind::Headless, ClientFacts::default());
    let (mut client, _) = Client::handshake(stream, &hello).unwrap();
    client
        .send(&ClientMsg::LayoutEdit(LayoutEdit::Agent {
            pane: Some(first),
        }))
        .unwrap();
    let third = wait_for("the window's agent pane", || {
        let l = ls(&env);
        let kinds = l["layout"]["main"]["kinds"].as_object()?.clone();
        kinds
            .iter()
            .filter(|(_, k)| *k == "agent")
            .filter_map(|(p, _)| p.parse::<u64>().ok())
            .find(|p| *p != agent && *p != omp)
    });
    let l = ls(&env);
    let root = &l["layout"]["main"]["sessions"][0]["tabs"][0]["root"];
    assert_eq!(root["node"], "split", "{root:#}");
    assert_eq!(root["first"]["dir"], "right", "{root:#}");
    assert_eq!(root["first"]["second"]["pane"], third, "{root:#}");
}

#[test]
fn a_locked_session_refuses_closing_its_panes_with_a_toast() {
    let env = TestEnv::new(AUK, "cli-lock");
    let _d = env.start_daemon();
    let window = env.start_headless("w", &[]);
    let a = new_shell(&env, &[]);
    let b = cli_json(&env, &["split", &a.to_string(), "down"])["pane"]
        .as_u64()
        .unwrap();
    let session = ls(&env)["layout"]["main"]["sessions"][0]["id"]
        .as_u64()
        .unwrap();
    assert_eq!(
        ls(&env)["layout"]["main"]["sessions"][0]["locked"],
        false,
        "new sessions start unlocked"
    );

    cli(&env, &["lock", &session.to_string()]);
    assert_eq!(ls(&env)["layout"]["main"]["sessions"][0]["locked"], true);
    // Closing is refused, without failing the script, and the windows say why.
    let out = env.cli(&["close", &a.to_string()]);
    assert!(out.status.success(), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("locked"),
        "{out:?}"
    );
    let refused = cli_json(&env, &["close", &b.to_string()]);
    assert_eq!(refused["closed"], false, "{refused}");
    assert_eq!(refused["locked"], session, "{refused}");
    let toast = wait_for("the lock's toast", || {
        window
            .request("effects")
            .as_array()?
            .iter()
            .find(|e| e["effect"] == "toast")
            .cloned()
    });
    // New sessions get a generated two-word name (amendment A11).
    let name = ls(&env)["layout"]["main"]["sessions"][0]["name"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        toast["text"],
        format!("Session {name} is locked"),
        "{toast}"
    );
    // A window's close is refused too.
    use auk_mirror::{Client, ClientFacts, ClientKind, ClientMsg, Hello, LayoutEdit};
    let stream = UnixStream::connect(&env.socket).unwrap();
    let hello = Hello::new(ClientKind::Headless, ClientFacts::default());
    let (mut client, _) = Client::handshake(stream, &hello).unwrap();
    client
        .send(&ClientMsg::Attach {
            window_key: "main".into(),
        })
        .unwrap();
    client
        .send(&ClientMsg::LayoutEdit(LayoutEdit::Close { pane: a }))
        .unwrap();
    std::thread::sleep(Duration::from_millis(500));
    let l = ls(&env);
    for p in [a, b] {
        assert!(
            pane_of(&l, p).unwrap()["exit"].is_null(),
            "pane {p} closed: {l:#}"
        );
    }
    // The window's lock edit unlocks it (the layout reaches every window); now closing works.
    client
        .send(&ClientMsg::LayoutEdit(LayoutEdit::LockSession {
            session,
            locked: false,
        }))
        .unwrap();
    wait_for("the session unlocked", || {
        (window.request("layout")["sessions"][0]["locked"] == false).then_some(())
    });
    cli(&env, &["close", &b.to_string()]);
    wait_for("the closed pane to leave the layout", || {
        let l = ls(&env);
        (!l["layout"]["main"]
            .to_string()
            .contains(&format!("\"pane\":{b}}}")))
        .then_some(())
    });
    // Locking again reaches the window with the layout.
    cli(&env, &["lock", &session.to_string()]);
    wait_for("the window to see the lock", || {
        (window.request("layout")["sessions"][0]["locked"] == true).then_some(())
    });
}

// ---------------------------------------------------------------------------------------------
// Headless mode, the control endpoint and scenarios

/// A fixture HOME for scenarios: the SDK checkout, a Python 3, and a directory to list.
fn scenario_home(env: &TestEnv) {
    std::os::unix::fs::symlink(
        repo_root().join("third_party/tern-sdk"),
        env.home.join("sdk"),
    )
    .unwrap();
    std::os::unix::fs::symlink(require_program("python3"), env.home.join("python3")).unwrap();
    let w = env.home.join("w");
    std::fs::create_dir_all(w.join("src")).unwrap();
    std::fs::write(w.join("notes.md"), "# Notes\n\nSome notes.\n").unwrap();
    std::fs::write(w.join("Cargo.toml"), "[package]\nname = \"w\"\n").unwrap();
    std::fs::write(w.join("data.bin"), vec![7u8; 3000]).unwrap();
}

fn shot_command(env: &TestEnv, out: &Path, args: &[&str]) -> Command {
    let mut cmd = env.command();
    cmd.env("AUK_FIXTURE_ROOT", repo_root())
        .current_dir(&env.root)
        .arg("shot")
        .arg("--out")
        .arg(out)
        .args(args);
    cmd
}

/// Runs a scenario from `fixtures/scenarios` and checks every shot against its golden.
fn scenario(name: &str, shots: &[&str]) {
    let env = TestEnv::new(AUK, &format!("sc-{name}"));
    scenario_home(&env);
    let out = env.root.join("shots");
    let script = repo_root().join(format!("fixtures/scenarios/{name}.auk"));
    let printed = ok(
        &shot_command(&env, &out, &["--json"])
            .arg(&script)
            .output()
            .unwrap(),
        &format!("auk shot {name}"),
    );
    let report: Value = serde_json::from_str(printed.trim()).unwrap();
    assert_eq!(report["scenario"], name);
    let written: Vec<String> = report["shots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["png"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(written.len(), shots.len(), "{report:#}");
    let dir = out.join(name);
    for shot in shots {
        goldens::check_shot(name, &dir, shot).unwrap_or_else(|e| panic!("{e}"));
    }
}

#[test]
fn scenario_shell_session_matches_goldens() {
    scenario("shell", &["shell"]);
}

#[test]
fn scenario_sdk_example_surface_matches_goldens() {
    scenario("sdk-table", &["table"]);
}

#[test]
fn scenario_split_layout_matches_goldens() {
    scenario("split", &["split", "split-focus-left"]);
}

#[test]
fn scenario_surface_play_regions_matches_goldens() {
    scenario("play-regions", &["regions"]);
}

#[test]
fn scenario_surface_play_progress_matches_goldens() {
    scenario("play-progress", &["progress"]);
}

#[test]
fn shots_are_deterministic() {
    let env = TestEnv::new(AUK, "cli-determ");
    scenario_home(&env);
    let script = repo_root().join("fixtures/scenarios/play-regions.auk");
    let mut runs = Vec::new();
    for i in 0..2 {
        let out = env.root.join(format!("run{i}"));
        ok(
            &shot_command(&env, &out, &[]).arg(&script).output().unwrap(),
            "auk shot",
        );
        runs.push(out.join("play-regions"));
    }
    let json = |d: &PathBuf| std::fs::read_to_string(d.join("regions.json")).unwrap();
    assert_eq!(json(&runs[0]), json(&runs[1]), "the layout is identical");
    let a = goldens::read_png(&runs[0].join("regions.png")).unwrap();
    let b = goldens::read_png(&runs[1].join("regions.png")).unwrap();
    let d = goldens::diff_images(&a, &b).unwrap();
    assert_eq!(d.differing, 0, "{d:?}");
}

#[test]
fn shot_reports_the_failing_line() {
    let env = TestEnv::new(AUK, "cli-shotfail");
    let script = env.root.join("bad.auk");
    std::fs::write(&script, "ready\nexpect \"never printed\"\n").unwrap();
    let start = Instant::now();
    let out = shot_command(&env, &env.root.join("o"), &[])
        .arg(&script)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("bad.auk:2") && err.contains("never printed"),
        "{err}"
    );
    // Real-work waits give up after 20 s.
    assert!(
        start.elapsed() < Duration::from_secs(40),
        "{:?}",
        start.elapsed()
    );
}

/// `auk serve` on a control endpoint, driven with `auk ctl`.
struct Serve {
    child: Child,
    control: PathBuf,
}

impl Serve {
    fn start(env: &TestEnv, out: &Path) -> Serve {
        let control = env.root.join("serve.ctl");
        let log = std::fs::File::create(env.root.join("serve.log")).unwrap();
        let mut cmd = env.command();
        cmd.env("AUK_FIXTURE_ROOT", repo_root())
            .current_dir(&env.root)
            .args(["serve", "--size", "640x400", "--control"])
            .arg(&control)
            .arg("--out")
            .arg(out)
            .stdout(log.try_clone().unwrap())
            .stderr(log);
        let child = cmd.spawn().unwrap();
        wait_for("auk serve's endpoint", || {
            auk_conformance::harness::ping(&control).then_some(())
        });
        Serve { child, control }
    }

    fn ctl(&self, env: &TestEnv, words: &[&str]) -> Value {
        let mut args = vec!["ctl", "--control"];
        let control = self.control.to_string_lossy().into_owned();
        args.push(&control);
        args.extend_from_slice(words);
        let text = cli(env, &args);
        let reply: Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(reply["ok"], true, "{words:?}: {reply}");
        reply.get("value").cloned().unwrap_or(Value::Null)
    }
}

impl Drop for Serve {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn serve_control_endpoint_takes_harness_control_and_terminal_commands() {
    let env = TestEnv::new(AUK, "cli-serve");
    let out = env.root.join("shots");
    let mut serve = Serve::start(&env, &out);

    // Deterministic: a pinned clock, no plugins, no restored sessions.
    let state = serve.ctl(&env, &["state"]);
    assert_eq!(state["window"]["clock"], 0.0);
    assert_eq!(state["panes"], json!([]), "nothing restored");
    let plugins = serve.ctl(&env, &["plugins", "list", "--json"]);
    assert_eq!(
        plugins["plugins"].as_array().map(Vec::len),
        Some(0),
        "{plugins}"
    );

    let ready = serve.ctl(&env, &["ready"]);
    let pane = ready["pane"].as_u64().unwrap();
    serve.ctl(&env, &["run", "echo", "served"]);
    serve.ctl(&env, &["expect", "served"]);
    serve.ctl(&env, &["wait", "250"]);
    let state = serve.ctl(&env, &["state"]);
    assert_eq!(
        state["window"]["clock"], 250.0,
        "wait moves the pinned clock"
    );
    assert_eq!(state["focused"], pane);
    // The window sizes its panes: 640x400 points less padding, in 8x16 cells... whatever the
    // font gives, the pane fills the window.
    let p = &state["panes"][0];
    let (cols, rows) = (p["cols"].as_u64().unwrap(), p["rows"].as_u64().unwrap());
    serve.ctl(&env, &["size", "960x400"]);
    let wider = serve.ctl(&env, &["state"]);
    assert!(
        wider["panes"][0]["cols"].as_u64().unwrap() > cols,
        "{wider}"
    );
    assert_eq!(wider["panes"][0]["rows"].as_u64().unwrap(), rows);

    // Harness input.
    serve.ctl(&env, &["type", "echo", "keyed"]);
    serve.ctl(&env, &["key", "Enter"]);
    serve.ctl(&env, &["expect", "\"\\nkeyed\""]);

    // feed puts program output on the screen, through surface-play's stand-in.
    serve.ctl(&env, &["feed", "\"fed\\nline\""]);
    serve.ctl(&env, &["expect", "fed"]);

    // surface-play, then the control commands that read the drawn surfaces.
    let rec = repo_root().join("fixtures/tsp/docs-regions.jsonl");
    let played = serve.ctl(&env, &["surface-play", &rec.to_string_lossy()]);
    assert_eq!(played["frames"], 1);
    let tree = serve.ctl(&env, &["tree", ".sf-text"]);
    let texts: Vec<&Value> = tree
        .as_array()
        .unwrap()
        .iter()
        .filter(|b| b["class"].as_str().unwrap().contains("sf-text"))
        .collect();
    assert!(texts.len() >= 5, "{tree:#}");
    assert!(texts.iter().all(|b| b["box"][3].as_f64().unwrap() > 0.0));
    let css = serve.ctl(&env, &["css", ".sf-editor", "display", "font-size"]);
    assert_eq!(css["style"]["display"], "Flex", "{css}");
    assert!(css["box"][2].as_f64().unwrap() > 100.0, "{css}");
    let dump = serve.ctl(&env, &["dump"]);
    assert!(
        dump["text"][pane.to_string()]
            .as_str()
            .unwrap()
            .contains("Run cargo update -p serde?"),
        "{dump:#}"
    );

    // Shots, in both appearances; perf counts them.
    serve.ctl(&env, &["appearance", "light"]);
    let light = serve.ctl(&env, &["shot", "light"]);
    serve.ctl(&env, &["appearance", "dark"]);
    let dark = serve.ctl(&env, &["shot", "dark"]);
    for s in [&light, &dark] {
        assert_eq!(s["size"], json!([960, 400]));
        assert!(Path::new(s["png"].as_str().unwrap()).is_file());
    }
    let (l, d) = (
        goldens::read_png(Path::new(light["png"].as_str().unwrap())).unwrap(),
        goldens::read_png(Path::new(dark["png"].as_str().unwrap())).unwrap(),
    );
    let bg = |img: &goldens::Rgba| img.data[img.data.len() - 4..].to_vec();
    assert!(
        bg(&l)[0] > 200 && bg(&d)[0] < 60,
        "light and dark backgrounds: {:?} {:?}",
        bg(&l),
        bg(&d)
    );
    let layout: Value =
        serde_json::from_str(&std::fs::read_to_string(dark["json"].as_str().unwrap()).unwrap())
            .unwrap();
    assert_eq!(layout["appearance"], "dark");
    assert_eq!(layout["panes"][0]["blocks"][0]["sf"], "s1", "{layout:#}");
    assert!(layout["panes"][0]["dock"]["height"].as_f64().unwrap() > 20.0);
    let perf = serve.ctl(&env, &["perf"]);
    assert!(perf["shots"].as_u64().unwrap() >= 2, "{perf}");

    // Motion: frozen clocks don't move on wait; motion-at sets the offset.
    serve.ctl(&env, &["freeze-anims"]);
    serve.ctl(&env, &["wait", "100"]);
    assert_eq!(serve.ctl(&env, &["state"])["window"]["clock"], 250.0);
    serve.ctl(&env, &["motion-at", "40"]);
    assert_eq!(serve.ctl(&env, &["state"])["window"]["clock"], 290.0);
    serve.ctl(&env, &["thaw"]);

    // Layout commands, from a script sent with ctl --file.
    let script = env.root.join("layout.auk");
    std::fs::write(
        &script,
        "# split, focus and close\nsplit right\nrun \"echo two\"\nfocus left\nnew tab\nclose\nstate\n",
    )
    .unwrap();
    let text = cli(
        &env,
        &[
            "ctl",
            "--control",
            &serve.control.to_string_lossy(),
            "--file",
            &script.to_string_lossy(),
        ],
    );
    let replies: Vec<Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(replies.len(), 6);
    assert!(replies.iter().all(|r| r["ok"] == true), "{replies:#?}");
    let right = replies[0]["value"]["pane"].as_u64().unwrap();
    assert_eq!(replies[2]["value"]["pane"], pane, "focus left goes back");
    let last = &replies[5]["value"];
    assert_eq!(
        last["focused"], pane,
        "closing the new tab's pane focuses what's left"
    );
    assert!(
        last["layout"]
            .to_string()
            .contains(&format!("\"pane\":{right}"))
    );

    // A failing command fails ctl; quit stops the window and its private daemon.
    let bad = env.cli(&[
        "ctl",
        "--control",
        &serve.control.to_string_lossy(),
        "focus",
        "up",
    ]);
    assert!(!bad.status.success());
    let _ = env.cli(&["ctl", "--control", &serve.control.to_string_lossy(), "quit"]);
    let status = wait_for("auk serve to exit", || {
        serve.child.try_wait().ok().flatten()
    });
    assert!(status.success(), "{status}");
    assert!(!serve.control.exists());
}

#[test]
fn css_lists_sheets_in_cascade_order_with_program_sheets_as_tsp_scope_name() {
    let env = TestEnv::new(AUK, "cli-css");
    let out = env.root.join("shots");
    let serve = Serve::start(&env, &out);
    serve.ctl(&env, &["ready"]);
    let rec = repo_root().join("fixtures/tsp/docs-ask.jsonl");
    serve.ctl(&env, &["surface-play", &rec.to_string_lossy()]);
    let listed = serve.ctl(&env, &["css"]);
    let sheets = listed["sheets"].as_array().expect("a sheets list");
    let names: Vec<&str> = sheets.iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert_eq!(
        names.first(),
        Some(&"ua"),
        "the user-agent sheet first: {names:?}"
    );
    // The recording's sheet `main`, under its surface's scope token, after the built-in sheets.
    let (at, program) = sheets
        .iter()
        .enumerate()
        .find(|(_, s)| s["name"].as_str().unwrap().starts_with("tsp:"))
        .unwrap_or_else(|| panic!("no program sheet in {names:?}"));
    let name = program["name"].as_str().unwrap();
    let scope = name
        .strip_prefix("tsp:")
        .and_then(|r| r.strip_suffix(":main"))
        .unwrap_or_else(|| panic!("{name} is not tsp:<scope>:main"));
    assert!(scope.starts_with("sf"), "{name}");
    assert!(program["rules"].as_u64().unwrap() > 0, "{program}");
    assert_eq!(
        at,
        sheets.len() - 1,
        "program sheets cascade last: {names:?}"
    );
}

#[test]
fn serve_plugins_fixtures_loads_the_sdk_examples_into_the_private_daemon() {
    let env = TestEnv::new(AUK, "cli-fixtures");
    let out = env.root.join("shots");
    let serve = Serve::start(&env, &out);
    // No plugins unless fixtures load them (§11.3).
    let none = serve.ctl(&env, &["plugins", "list", "--json"]);
    assert_eq!(none["plugins"].as_array().map(Vec::len), Some(0), "{none}");
    let loaded = serve.ctl(&env, &["plugins", "fixtures"]);
    let examples = repo_root().join("third_party/tern-sdk/plugins/examples");
    let linked: Vec<String> = loaded["linked"]
        .as_array()
        .expect("linked")
        .iter()
        .filter_map(|d| d.as_str().map(str::to_owned))
        .collect();
    for name in ["dirvars", "review-queue", "terraform", "jsonx"] {
        assert!(
            linked.contains(&examples.join(name).to_string_lossy().into_owned()),
            "{name} not linked: {linked:?}"
        );
    }
    let list = &loaded["list"]["plugins"];
    let status = |id: &str| {
        list.as_array()
            .and_then(|p| p.iter().find(|p| p["id"] == id))
            .map(|p| p["status"].clone())
            .unwrap_or(Value::Null)
    };
    for id in ["dirvars", "review-queue", "terraform"] {
        assert_eq!(status(id), "ready", "{id}: {list:#}");
    }
    // The window holds the new catalog: block types with their titles for the palette's
    // "New <title> block" rows, and the lens rules.
    let catalog = serve.ctl(&env, &["catalog"]);
    let blocks: Vec<(String, String)> = catalog["plugins"]
        .as_array()
        .expect("plugins")
        .iter()
        .flat_map(|p| {
            let id = p["id"].as_str().unwrap_or_default().to_owned();
            p["blocks"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(move |b| {
                    (
                        format!("{id}.{}", b["id"].as_str().unwrap_or_default()),
                        b["title"].as_str().unwrap_or_default().to_owned(),
                    )
                })
        })
        .collect();
    assert!(
        blocks
            .iter()
            .any(|(kind, title)| kind == "review-queue.queue" && !title.is_empty()),
        "{blocks:?}"
    );
    // A fixture plugin runs in the private daemon: the terraform lens claims a plan.
    let plan = repo_root().join("crates/plugins/tests/fixtures/terraform-plan.txt");
    let tf = env.root.join("bin/terraform");
    std::fs::create_dir_all(tf.parent().unwrap()).unwrap();
    std::fs::write(&tf, format!("#!/bin/sh\ncat '{}'\n", plan.display())).unwrap();
    std::fs::set_permissions(&tf, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let pane = serve.ctl(&env, &["ready"])["pane"].as_u64().unwrap();
    serve.ctl(&env, &["run", &tf.to_string_lossy(), "plan"]);
    let lens = wait_for("the terraform lens's view", || {
        let l = serve.ctl(&env, &["lens", &pane.to_string()]);
        (!l["blocks"][0]["view"].is_null()).then_some(l)
    });
    assert_eq!(lens["blocks"][0]["plugin"], "terraform", "{lens:#}");
}

#[test]
fn remote_loopback_attaches_a_second_daemon_through_the_bridge() {
    let env = TestEnv::new(AUK, "cli-loop");
    let out = env.root.join("shots");
    let serve = Serve::start(&env, &out);
    let first = serve.ctl(&env, &["ready"])["pane"].as_u64().unwrap();
    serve.ctl(&env, &["run", "echo", "on the first host"]);
    // It takes no address (§11.3): the host is always `loopback`.
    let control = serve.control.to_string_lossy().into_owned();
    let refused = env.cli(&[
        "ctl",
        "--control",
        &control,
        "remote",
        "loopback",
        "example.com",
    ]);
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&refused.stdout),
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(said.contains("takes no address"), "{said}");
    let status = serve.ctl(&env, &["remote", "loopback"]);
    assert_eq!(status["state"]["state"], "connected", "{status:#}");
    // The window now shows the loopback host: nothing yet, then its own session.
    let state = serve.ctl(&env, &["state"]);
    assert_eq!(state["panes"], json!([]), "{state:#}");
    let pane = serve.ctl(&env, &["ready"])["pane"].as_u64().unwrap();
    serve.ctl(&env, &["run", "echo", "over the bridge"]);
    serve.ctl(&env, &["expect", "over the bridge"]);
    let dump = serve.ctl(&env, &["dump"]);
    let text = dump["text"][pane.to_string()].as_str().unwrap();
    assert!(!text.contains("on the first host"), "{text}");
    let shot = serve.ctl(&env, &["shot", "loopback"]);
    let layout: Value =
        serde_json::from_str(&std::fs::read_to_string(shot["json"].as_str().unwrap()).unwrap())
            .unwrap();
    assert_eq!(layout["panes"][0]["pane"], pane);
    let _ = first;
    let status = serve.ctl(&env, &["remote"]);
    assert_eq!(status["state"]["state"], "connected", "{status:#}");
}

/// Every file under `dir` with its bytes and mtime.
fn snapshot_tree(dir: &Path) -> Vec<(PathBuf, Vec<u8>, std::time::SystemTime)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let meta = std::fs::symlink_metadata(&path).unwrap();
            if meta.is_dir() {
                stack.push(path);
            } else {
                let bytes = std::fs::read(&path).unwrap_or_default();
                out.push((path, bytes, meta.modified().unwrap()));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[test]
fn headless_mode_ignores_the_users_settings_and_never_writes_them() {
    let env = TestEnv::new(AUK, "cli-usersettings");
    // The user's config dir (the harness points AUK_CONFIG_DIR and AUK_STATE_DIR at it): settings
    // that turn plugins off and disable terraform, and a plugin of their own.
    let config = env.state.clone();
    let settings = config.join("settings.json");
    std::fs::write(
        &settings,
        json!({"plugins": false, "plugins_disabled": ["terraform"], "agent_command": "false"})
            .to_string(),
    )
    .unwrap();
    let mine = config.join("plugins/mine");
    std::fs::create_dir_all(&mine).unwrap();
    std::fs::write(
        mine.join("plugin.toml"),
        "schema = 1\nid = \"mine\"\nname = \"Mine\"\nversion = \"1\"\nhost = \"host.luau\"\n",
    )
    .unwrap();
    std::fs::write(mine.join("host.luau"), "").unwrap();
    let before = snapshot_tree(&config);

    let out = env.root.join("shots");
    let serve = Serve::start(&env, &out);
    // Default settings: the user's plugin isn't loaded, and fixtures load with every plugin on,
    // terraform included.
    let none = serve.ctl(&env, &["plugins", "list", "--json"]);
    assert_eq!(none["plugins"], json!([]), "{none}");
    let loaded = serve.ctl(&env, &["plugins", "fixtures"]);
    let terraform = loaded["list"]["plugins"]
        .as_array()
        .and_then(|p| p.iter().find(|p| p["id"] == "terraform"))
        .cloned()
        .unwrap_or(Value::Null);
    assert_eq!(terraform["status"], "ready", "{loaded:#}");
    // Changes a window could save: none reach the user's files.
    serve.ctl(&env, &["ready"]);
    serve.ctl(&env, &["appearance", "dark"]);
    serve.ctl(&env, &["size", "800x500"]);
    serve.ctl(&env, &["run", "echo", "hi"]);
    serve.ctl(&env, &["expect", "hi"]);
    serve.ctl(&env, &["shot", "s"]);
    drop(serve);
    assert_eq!(
        snapshot_tree(&config),
        before,
        "headless mode changed the user's config dir"
    );
}

#[test]
fn a_real_work_wait_gives_up_after_20_s() {
    let env = TestEnv::new(AUK, "cli-realwork");
    let out = env.root.join("shots");
    let serve = Serve::start(&env, &out);
    serve.ctl(&env, &["ready"]);
    let control = serve.control.to_string_lossy().into_owned();
    let start = Instant::now();
    let reply = env.cli(&["ctl", "--control", &control, "expect", "never-printed"]);
    let took = start.elapsed();
    let text = String::from_utf8_lossy(&reply.stdout).into_owned();
    let reply: Value = serde_json::from_str(text.trim()).unwrap_or_else(|e| panic!("{e}: {text}"));
    assert_eq!(reply["ok"], false, "{reply}");
    assert!(
        reply["error"]
            .as_str()
            .is_some_and(|e| e.contains("never-printed")),
        "{reply}"
    );
    assert!(
        took >= Duration::from_secs(20) && took < Duration::from_secs(23),
        "the wait took {took:?}, not 20 s"
    );
    // The window still works after the timeout.
    serve.ctl(&env, &["run", "echo", "after"]);
    serve.ctl(&env, &["expect", "after"]);
}
