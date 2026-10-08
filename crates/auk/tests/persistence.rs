//! Persistence (GOAL.md): survival of window exit (AC1), restore after a daemon restart (AC10,
//! §5.4, §16), the resume hook (D19), in-place upgrade (§15.7) and zsh integration (M2), against
//! the real `auk` binary with a hermetic daemon per test.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use auk_conformance::harness::{TestEnv, alive, quote, wait_for};
use auk_conformance::probe::{Probe, hello, open_with, tsp_json};
use base64::Engine as _;
use serde_json::{Value, json};

fn env(name: &str) -> TestEnv {
    TestEnv::new(env!("CARGO_BIN_EXE_auk"), name)
}

/// The physical form of a path, for comparing cwds the shell reports.
fn real(path: &Path) -> std::path::PathBuf {
    path.canonicalize().unwrap()
}

#[test]
fn ac01_shell_survives_window_exit_and_new_window_shows_scrollback() {
    let env = env("ac01");
    env.start_daemon();
    let work = env.home.join("work dir");
    fs::create_dir_all(&work).unwrap();

    let first = env.start_headless("first", &[]);
    let pane = env.new_pane(&["--cwd", &work.to_string_lossy()]);
    env.wait(pane, "prompt");
    // The marker appears only in the output, not in the typed line.
    env.run(pane, "echo marker-$((6*7))");
    wait_for("the first window to show the marker", || {
        first
            .capture(pane, true)
            .contains("marker-42")
            .then_some(())
    });
    let shell = env.pane_info(pane)["pid"].as_u64().unwrap() as u32;

    // The window process exits.
    env.kill9(first.pid);
    assert!(!alive(first.pid));

    // The shell keeps running, in the same cwd.
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(alive(shell), "the shell died with the window");
    let info = env.pane_info(pane);
    assert!(info["exit"].is_null(), "{info}");
    assert_eq!(real(Path::new(info["cwd"].as_str().unwrap())), real(&work));

    // A new window shows the scrollback through snapshot replay, equal to the daemon's state.
    let second = env.start_headless("second", &[]);
    let (hash, seq) = wait_for("the second window's replica", || second.hash(pane));
    assert_eq!((hash, seq), env.daemon_hash(pane));
    assert!(second.capture(pane, true).contains("marker-42"));

    // And the shell still answers, in its cwd.
    env.run(pane, "pwd -P");
    let cwd = real(&work).to_string_lossy().into_owned();
    wait_for("pwd in the second window", || {
        second.capture(pane, true).contains(&cwd).then_some(())
    });
    assert!(second.errors().is_empty(), "{:?}", second.errors());
}

#[test]
fn zsh_integration_reports_prompts_commands_and_status() {
    let env = env("zshmarks");
    env.start_daemon();
    let pane = env.new_pane(&[]);
    let first = env.wait(pane, "prompt");
    assert!(first["prompts"].as_u64().unwrap() >= 1, "{first}");

    // The user's own .zshrc ran (through ZDOTDIR injection).
    env.run(pane, "echo rc-$AUK_TEST_RC");
    let screen = env.daemon(&format!("capture {pane}"))["text"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(screen.contains("rc-sourced"), "{screen}");

    let done = env.run(pane, "sh -c 'exit 3'");
    assert_eq!(done["status"], 3, "{done}");
    let marks = env.daemon(&format!("marks {pane}"));
    let last = marks.as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["cmdline"], "sh -c 'exit 3'", "{marks}");
    assert_eq!(last["status"], 3);
    assert!(last["line"].as_u64().is_some());
    let info = env.pane_info(pane);
    assert_eq!(
        (info["at_prompt"].as_bool(), info["running"].as_bool()),
        (Some(true), Some(false))
    );
}

#[test]
fn zsh_integration_reports_cwd_and_titles() {
    let env = env("zshcwd");
    env.start_daemon();
    let pane = env.new_pane(&[]);
    env.wait(pane, "prompt");
    let dir = env.home.join("a b");
    fs::create_dir_all(&dir).unwrap();

    env.run(pane, "cd 'a b'");
    let info = env.pane_info(pane);
    assert_eq!(
        real(Path::new(info["cwd"].as_str().unwrap())),
        real(&dir),
        "{info}"
    );
    // At the prompt the title is the directory (%~).
    assert_eq!(info["title"], "~/a b", "{info}");
    let marks = env.daemon(&format!("marks {pane}"));
    assert_eq!(
        marks.as_array().unwrap().last().unwrap()["cmdline"],
        "cd 'a b'"
    );

    // While a command runs the title is its line.
    env.daemon(&format!("run {pane} 'sleep 2'"));
    wait_for("the running command's title", || {
        (env.pane_info(pane)["title"] == "sleep 2").then_some(())
    });
    assert_eq!(env.pane_info(pane)["running"], true);
    env.wait(pane, "prompt");
    // The next mark records the new cwd.
    let marks = env.daemon(&format!("marks {pane}"));
    let cwd = marks.as_array().unwrap().last().unwrap()["cwd"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        cwd.starts_with("file://") && cwd.ends_with("/a%20b"),
        "{cwd}"
    );
}

#[test]
fn zsh_integration_command_durations_are_measured_by_the_daemon() {
    let env = env("zshdur");
    env.start_daemon();
    let pane = env.new_pane(&[]);
    env.wait(pane, "prompt");
    env.run(pane, "sleep 0.4");
    env.run(pane, "true");
    let marks = env.daemon(&format!("marks {pane}"));
    let marks = marks.as_array().unwrap();
    let sleep = &marks[marks.len() - 2];
    let quick = &marks[marks.len() - 1];
    assert_eq!(sleep["cmdline"], "sleep 0.4");
    let slow_ms = sleep["duration_ms"].as_u64().unwrap();
    assert!((400..5000).contains(&slow_ms), "{sleep}");
    assert!(quick["duration_ms"].as_u64().unwrap() < slow_ms, "{quick}");
    assert!(sleep["started_ms"].as_u64().unwrap() <= quick["started_ms"].as_u64().unwrap());
}

#[test]
fn panes_get_a_login_environment_and_their_identity() {
    let env = env("paneenv");
    env.start_daemon();
    let pane = env.new_pane(&[]);
    env.wait(pane, "prompt");
    env.run(
        pane,
        "print -r -- \"[$TERM|$COLORTERM|$TERM_PROGRAM|$AUK_PANE|$AUK_WINDOW_KEY|${+AUK_WINDOW_SOCKET}|$ZDOTDIR|${AUK_PANE_SOCKET:t}]\"",
    );
    let screen = env.daemon(&format!("capture {pane}"))["text"]
        .as_str()
        .unwrap()
        .to_owned();
    let expected = format!(
        "[xterm-256color|truecolor|auk|{pane}|main|1|{}|{pane}.sock]",
        env.zdotdir.display()
    );
    assert!(screen.contains(&expected), "{expected} not in\n{screen}");
    // A login shell: zsh's login option is set.
    env.run(pane, "[[ -o login ]] && echo login-$((1+1))");
    let screen = env.daemon(&format!("capture {pane}"))["text"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(screen.contains("login-2"), "{screen}");
}

#[test]
fn exited_panes_report_their_status() {
    let env = env("exit");
    env.start_daemon();
    let pane = env.new_pane(&["--", "exit 7"]);
    let done = env.wait(pane, "exit");
    assert_eq!(done["exit"], 7, "{done}");
    // The exit is a record in the stream: the replica's hash covers it.
    let (_, seq) = env.daemon_hash(pane);
    assert!(seq >= 1);
    // The pane left the layout but stays listed for capture.
    let ls = env.daemon("ls");
    assert!(
        ls["layout"]["main"]["sessions"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{ls}"
    );
    assert_eq!(env.pane_info(pane)["exit"], 7);

    let shell = env.new_pane(&[]);
    env.wait(shell, "prompt");
    let pid = env.pane_info(shell)["pid"].as_u64().unwrap() as u32;
    env.daemon(&format!("kill {shell}"));
    let done = env.wait(shell, "exit");
    // zsh exits with a failure status when hung up.
    assert!(done["exit"].as_i64().is_some_and(|s| s != 0), "{done}");
    assert!(!alive(pid));
}

#[test]
fn first_client_starts_a_detached_daemon_and_cli_verbs_drive_it() {
    let env = env("autostart");
    // No daemon yet: verbs that only talk fail clearly.
    let ls = env.cli(&["ls"]);
    assert!(!ls.status.success());
    assert!(
        String::from_utf8_lossy(&ls.stderr).contains("no daemon"),
        "{ls:?}"
    );

    // `new` starts one, detached, logging to the state dir.
    let new = env.cli(&["new", "--cwd", &env.home.to_string_lossy()]);
    assert!(new.status.success(), "{new:?}");
    let pane: u64 = String::from_utf8_lossy(&new.stdout).trim().parse().unwrap();
    assert!(env.state.join("daemon.log").exists());
    let daemon_pid = env.daemon("ls")["daemon_pid"].as_u64().unwrap() as u32;
    // Its own session: not in this test's process group.
    let sid = std::process::Command::new("/bin/ps")
        .args(["-o", "sess=,pgid=", "-p", &daemon_pid.to_string()])
        .output()
        .unwrap();
    let ids = String::from_utf8_lossy(&sid.stdout)
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert_eq!(
        ids.get(1).map(String::as_str),
        Some(daemon_pid.to_string().as_str()),
        "{ids:?}"
    );

    let ok = |args: &[&str]| {
        let out = env.cli(args);
        assert!(out.status.success(), "{args:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let p = pane.to_string();
    ok(&["wait", &p, "--until", "prompt", "--timeout", "20"]);
    ok(&["run", &p, "echo", "verb-$((5*5))"]);
    ok(&["wait", &p, "--until", "prompt", "--timeout", "20"]);
    assert!(ok(&["capture", &p]).contains("verb-25"));
    ok(&["send", &p, "text", "echo sent-$((3+4))"]);
    ok(&["send", &p, "keys", "Enter"]);
    ok(&["wait", &p, "--until", "prompt", "--timeout", "20"]);
    ok(&["wait", &p, "--until", "idle=100", "--timeout", "20"]);
    assert!(ok(&["capture", &p, "--scrollback"]).contains("sent-7"));
    let listing = ok(&["ls"]);
    assert!(
        listing
            .lines()
            .any(|l| l.trim_start().starts_with(&p) && l.contains("prompt")),
        "{listing}"
    );
    let json: serde_json::Value = serde_json::from_str(&ok(&["ls", "--json"])).unwrap();
    assert_eq!(json["panes"][0]["id"], pane);

    // A wait that times out exits 124.
    let out = env.cli(&["wait", &p, "--until", "exit", "--timeout", "0.2"]);
    assert_eq!(out.status.code(), Some(124), "{out:?}");

    // A second `new` reuses the running daemon.
    let tab = env.cli(&["new", "tab", "--", "sleep 5"]);
    assert!(tab.status.success(), "{tab:?}");
    let ls = env.daemon("ls");
    assert_eq!(
        ls["layout"]["main"]["sessions"][0]["tabs"]
            .as_array()
            .unwrap()
            .len(),
        2,
        "{ls}"
    );
    assert!(alive(daemon_pid));
}

// ---------------------------------------------------------------------------------------------
// Restore (AC10, §5.4, §16)

/// The daemon's text of a pane, scrollback included.
fn scrollback(env: &TestEnv, pane: u64) -> String {
    env.daemon(&format!("capture {pane} --scrollback"))["text"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// Waits until a checkpoint holds every given pane at (at least) its current stream position:
/// the idle checkpoint, with nothing forcing it.
fn wait_checkpoint(env: &TestEnv, panes: &[u64]) -> Value {
    let wanted: Vec<(u64, u64)> = panes.iter().map(|&p| (p, env.daemon_hash(p).1)).collect();
    wait_for("a checkpoint with the panes' latest state", || {
        let state = env.daemon("state");
        wanted
            .iter()
            .all(|(p, seq)| {
                state["last"]["panes"][p.to_string()]
                    .as_u64()
                    .is_some_and(|s| s >= *seq)
            })
            .then_some(state)
    })
}

/// A process and all its descendants (a pane's shell and what it runs).
fn tree(pid: u32) -> Vec<u32> {
    let mut all = vec![pid];
    let mut i = 0;
    while i < all.len() {
        let out = Command::new("/usr/bin/pgrep")
            .args(["-P", &all[i].to_string()])
            .output()
            .unwrap();
        all.extend(
            String::from_utf8_lossy(&out.stdout)
                .split_whitespace()
                .filter_map(|p| p.parse::<u32>().ok()),
        );
        i += 1;
    }
    all
}

fn kill9(pid: u32) {
    let _ = Command::new("/bin/kill")
        .args(["-9", &pid.to_string()])
        .status();
}

fn surface<'a>(tsp: &'a Value, id: &str) -> Option<&'a Value> {
    tsp["surfaces"].as_array()?.iter().find(|s| s["id"] == id)
}

/// Writes as the program and returns the TSP the terminal answered with.
fn exchange(probe: &Probe, bytes: &[u8]) -> Vec<Value> {
    probe.write(bytes);
    probe.fence_tsp()
}

fn text_doc(text: &str) -> Value {
    json!([{"id": "t", "k": "text", "p": {"text": text}}])
}

#[test]
fn ac10_after_kill9_of_the_daemon_panes_come_back_over_their_scrollback_with_past_surfaces() {
    let env = env("ac10k");
    let daemon = env.start_daemon();
    let work = env.home.join("work dir");
    fs::create_dir_all(work.join("later")).unwrap();
    let pane = env.new_pane(&["--cwd", &work.to_string_lossy()]);
    env.wait(pane, "prompt");
    env.run(pane, "cd later");
    env.run(pane, "echo marker-$((6*7))");

    // A program leaves a kept flow surface in scrollback, then ends.
    // Each write waits for the terminal's answers (`fence_tsp`), so none reaches the shell.
    let probe = Probe::run_in_shell(&env, pane);
    exchange(&probe, &hello(&[]));
    exchange(&probe, &open_with("f", "flow", text_doc("flow kept")));
    exchange(&probe, &tsp_json("x", &json!({"id": "f", "keep": true})));
    probe.tty("cooked");
    probe.exit(0);
    env.wait(pane, "prompt");
    // Another one is still running at the crash, with a live inline surface and, on the
    // alternate screen, a screen surface.
    let probe = Probe::run_in_shell(&env, pane);
    exchange(&probe, &hello(&[]));
    exchange(&probe, &open_with("i", "inline", text_doc("inline live")));
    probe.write(b"\x1b[?1049h");
    exchange(&probe, &open_with("s", "screen", text_doc("screen live")));
    let before = env.daemon(&format!("tsp {pane}"));
    for (id, state) in [("f", "closed"), ("i", "live"), ("s", "live")] {
        assert_eq!(surface(&before, id).unwrap()["state"], state, "{before}");
    }
    let shell = env.pane_info(pane)["pid"].as_u64().unwrap() as u32;
    let state = wait_checkpoint(&env, &[pane]);
    assert!(env.state.join("daemon.state").is_file(), "{state}");

    env.kill9(daemon);
    wait_for("the pane's processes to end with the daemon", || {
        (!alive(shell) && !alive(probe.pid)).then_some(())
    });

    env.start_daemon();
    let info = env.pane_info(pane);
    assert_eq!(info["kind"]["kind"], "shell", "{info}");
    let ls = env.daemon("ls");
    assert_eq!(
        ls["layout"]["main"]["sessions"][0]["tabs"][0]["root"]["pane"], pane,
        "the layout came back: {ls}"
    );
    env.wait(pane, "prompt");
    // A new login shell, in the saved cwd.
    let new_shell = env.pane_info(pane)["pid"].as_u64().unwrap() as u32;
    assert_ne!(new_shell, shell);
    assert!(alive(new_shell));
    assert_eq!(
        real(Path::new(env.pane_info(pane)["cwd"].as_str().unwrap())),
        real(&work.join("later"))
    );
    env.run(pane, "[[ -o login ]] && echo login-$((1+1)); pwd -P");
    let text = scrollback(&env, pane);
    assert!(text.contains("login-2"), "{text}");
    assert!(
        text.contains(&real(&work.join("later")).to_string_lossy().into_owned()),
        "{text}"
    );
    // Over its old scrollback.
    assert!(text.contains("marker-42"), "{text}");

    // Past surfaces are closed and read-only at their anchors; the screen surface is gone, and
    // the restored inline surface outlives the new shell's prompts.
    let after = env.daemon(&format!("tsp {pane}"));
    for id in ["f", "i"] {
        let s = surface(&after, id).unwrap_or_else(|| panic!("{id} missing: {after}"));
        assert_eq!(
            (&s["state"], &s["restored"], &s["interactive"]),
            (&json!("closed"), &json!(true), &json!(false)),
            "{s}"
        );
        assert!(s["anchor"]["line"].is_u64(), "{s}");
    }
    assert!(surface(&after, "s").is_none(), "{after}");
    // The alternate screen the old program used is left: the shell runs on the main screen.
    let screen = env.daemon(&format!("capture {pane}"))["text"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(screen.contains("login-2"), "{screen}");

    // A window attaching now gets the restored state.
    let window = env.start_headless("w", &[]);
    let (hash, seq) = wait_for("the window's replica", || window.hash(pane));
    assert_eq!((hash, seq), env.daemon_hash(pane));
    assert!(window.errors().is_empty(), "{:?}", window.errors());
}

#[test]
fn ac10_after_a_simulated_reboot_every_pane_and_the_layout_come_back() {
    let env = env("ac10r");
    let daemon = env.start_daemon();
    let a_dir = env.home.join("a");
    let b_dir = env.home.join("b");
    fs::create_dir_all(&a_dir).unwrap();
    fs::create_dir_all(&b_dir).unwrap();
    let a = env.new_pane(&["--cwd", &a_dir.to_string_lossy()]);
    env.wait(a, "prompt");
    env.run(a, "echo first-$((2*3))");
    let b = env.daemon(&format!(
        "split {a} right --cwd {}",
        quote(&b_dir.to_string_lossy())
    ));
    let b = b
        .as_u64()
        .or_else(|| b["pane"].as_u64())
        .expect("the split's pane");
    env.wait(b, "prompt");
    env.run(b, "echo second-$((3*3))");
    // A long-running program in the first pane.
    env.daemon(&format!("run {a} 'sleep 600'"));
    wait_for("the sleep to run", || {
        (env.pane_info(a)["running"] == true).then_some(())
    });
    let layout = env.daemon("ls")["layout"].clone();
    let pids: Vec<u32> = [a, b]
        .iter()
        .flat_map(|p| tree(env.pane_info(*p)["pid"].as_u64().unwrap() as u32))
        .collect();
    assert!(pids.len() >= 3, "{pids:?}");
    wait_checkpoint(&env, &[a, b]);

    // The machine goes down: the daemon and every pane process die at once.
    env.kill9(daemon);
    for pid in &pids {
        kill9(*pid);
    }
    wait_for("every pane process gone", || {
        pids.iter().all(|p| !alive(*p)).then_some(())
    });

    env.start_daemon();
    assert_eq!(env.daemon("ls")["layout"], layout);
    for (pane, dir, marker) in [(a, &a_dir, "first-6"), (b, &b_dir, "second-9")] {
        env.wait(pane, "prompt");
        let info = env.pane_info(pane);
        assert!(
            !pids.contains(&(info["pid"].as_u64().unwrap() as u32)),
            "{info}"
        );
        assert_eq!(real(Path::new(info["cwd"].as_str().unwrap())), real(dir));
        assert!(scrollback(&env, pane).contains(marker));
        env.run(pane, "echo again-$((1+1))");
        assert!(scrollback(&env, pane).contains("again-2"));
    }
    // The command marks survive with the scrollback.
    let marks = env.daemon(&format!("marks {b}"));
    assert!(
        marks
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["cmdline"] == "echo second-$((3*3))"),
        "{marks}"
    );
    // New panes don't reuse restored ids.
    let c = env.new_pane(&[]);
    assert!(c > a.max(b));
}

/// The SDK's review-queue example with a stand-in `gh` (PR fixture from the plugins suite).
const PRS: &str = r#"[
  {"number": 12, "title": "Fix the parser", "url": "https://github.com/acme/app/pull/12",
   "repository": {"nameWithOwner": "acme/app"}, "author": {"login": "ada"},
   "createdAt": "2026-09-30T14:03:11Z", "isDraft": false},
  {"number": 7, "title": "WIP: new renderer", "url": "https://github.com/acme/gfx/pull/7",
   "repository": {"nameWithOwner": "acme/gfx"}, "author": {"login": "bob"},
   "createdAt": "2026-09-29T09:00:00Z", "isDraft": true}
]"#;

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
        }
    }
}

fn executable(path: &Path, script: &str) {
    use std::os::unix::fs::PermissionsExt;
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, script).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn ac10_a_plugin_block_resumes_from_its_last_save() {
    let env = env("ac10b");
    let examples = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../third_party/tern-sdk/plugins/examples/review-queue");
    assert!(examples.is_dir(), "the SDK example review-queue is missing");
    // The harness points AUK_CONFIG_DIR at the state dir.
    copy_dir(&examples, &env.state.join("plugins/review-queue"));
    let bin = env.root.join("bin");
    executable(
        &bin.join("gh"),
        &format!("#!/bin/sh\ncat <<'EOF'\n{PRS}\nEOF\n"),
    );
    let path = format!("{}:/usr/bin:/bin", bin.display());
    let daemon = env.start_daemon_with(|c| {
        c.env("PATH", &path);
    });

    let pane = env.daemon("block review-queue.queue")["pane"]
        .as_u64()
        .expect("a pane");
    let title = |env: &TestEnv| {
        env.pane_info(pane)["title"]
            .as_str()
            .unwrap_or("")
            .to_owned()
    };
    wait_for("the queue without drafts", || {
        (title(&env) == "Review Queue (1)").then_some(())
    });
    // `d` shows drafts, and that's what `save` keeps.
    env.daemon(&format!("send {pane} text d"));
    wait_for("drafts shown and saved", || {
        (title(&env) == "Review Queue (2)"
            && env.pane_info(pane)["kind"]["saved"] == r#"{"drafts":true}"#)
            .then_some(())
    });
    wait_checkpoint(&env, &[pane]);

    env.kill9(daemon);
    env.start_daemon_with(|c| {
        c.env("PATH", &path);
    });
    let kind = env.pane_info(pane)["kind"].clone();
    assert_eq!(
        (&kind["kind"], &kind["block"], &kind["saved"]),
        (
            &json!("block"),
            &json!("review-queue.queue"),
            &json!(r#"{"drafts":true}"#)
        ),
    );
    // Restarted from the save: drafts still shown, without any key.
    wait_for("the restored queue with drafts", || {
        (title(&env) == "Review Queue (2)").then_some(())
    });
    let ls = env.daemon("ls");
    assert_eq!(
        ls["layout"]["main"]["sessions"][0]["tabs"][0]["root"]["pane"], pane,
        "{ls}"
    );
}

// ---------------------------------------------------------------------------------------------
// The resume hook (D19, Q13)

fn b64(text: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(text)
}

#[test]
fn resume_user_variable_relaunches_the_program_in_the_new_shell() {
    let env = env("resume");
    let daemon = env.start_daemon();
    // Still running at the crash: it set `resume`.
    let running = env.new_pane(&[]);
    env.wait(running, "prompt");
    let line = format!(
        "printf '\\033]1337;SetUserVar=resume=%s\\007' {}; sleep 600",
        b64("echo resumed-$((40+2))")
    );
    env.daemon(&format!("run {running} {}", quote(&line)));
    // Ended before the crash: its `resume` is stale once the prompt came back.
    let ended = env.new_pane(&[]);
    env.wait(ended, "prompt");
    env.run(
        ended,
        &format!(
            "printf '\\033]1337;SetUserVar=resume=%s\\007' {}",
            b64("echo stale-$((1+1))")
        ),
    );
    wait_for("the sleep to run", || {
        (env.pane_info(running)["running"] == true).then_some(())
    });
    wait_checkpoint(&env, &[running, ended]);

    env.kill9(daemon);
    env.start_daemon();
    wait_for("the resume command to run in the new shell", || {
        scrollback(&env, running)
            .contains("resumed-42")
            .then_some(())
    });
    let marks = env.daemon(&format!("marks {running}"));
    assert_eq!(
        marks.as_array().unwrap().last().unwrap()["cmdline"],
        "echo resumed-$((40+2))",
        "run inside the login shell, as a command: {marks}"
    );
    env.wait(ended, "prompt");
    env.run(ended, "echo settled");
    assert!(!scrollback(&env, ended).contains("stale-2"));
}

#[test]
fn resume_hook_relaunches_omp_with_its_session_file() {
    let env = env("ompres");
    // A stand-in `omp` on the shell's PATH says how it was started. Q13 (live omp 18.8.4, in
    // PROGRESS.md): omp sets `omp_session_file` only when TERM_PROGRAM is `tern`, so here a
    // stand-in program sets it as omp would there.
    let bin = env.root.join("bin");
    executable(
        &bin.join("omp"),
        "#!/bin/sh\necho \"omp-stand-in $*\"\nexec sleep 600\n",
    );
    let mut rc = fs::read_to_string(env.zdotdir.join(".zshrc")).unwrap();
    rc.push_str(&format!(
        "export PATH={}:$PATH\n",
        quote(&bin.to_string_lossy())
    ));
    fs::write(env.zdotdir.join(".zshrc"), rc).unwrap();
    let session: PathBuf = env.home.join(".omp/sessions/a session.jsonl");
    fs::create_dir_all(session.parent().unwrap()).unwrap();
    fs::write(&session, "{}\n").unwrap();

    let daemon = env.start_daemon();
    let pane = env.new_pane(&[]);
    env.wait(pane, "prompt");
    let line = format!(
        "printf '\\033]1337;SetUserVar=omp_session_file=%s\\007' {}; sleep 600",
        b64(&session.to_string_lossy())
    );
    env.daemon(&format!("run {pane} {}", quote(&line)));
    wait_for("the program to run", || {
        (env.pane_info(pane)["running"] == true).then_some(())
    });
    wait_checkpoint(&env, &[pane]);

    env.kill9(daemon);
    env.start_daemon();
    let expected = format!("omp-stand-in --resume={}", session.display());
    wait_for("omp relaunched with --resume", || {
        scrollback(&env, pane).contains(&expected).then_some(())
    });
}

// ---------------------------------------------------------------------------------------------
// In-place upgrade (§15.7)

/// The highest `tick-N` in a pane's text.
fn last_tick(text: &str) -> Option<u64> {
    text.split("tick-")
        .skip(1)
        .filter_map(|rest| {
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().ok()
        })
        .max()
}

/// Starts a loop that prints a numbered tick every 50 ms and waits for a few.
fn ticking(env: &TestEnv) -> (u64, u32) {
    let pane = env.new_pane(&[]);
    env.wait(pane, "prompt");
    env.daemon(&format!(
        "run {pane} {}",
        quote("i=0; while :; do echo tick-$i; i=$((i+1)); sleep 0.05; done")
    ));
    wait_for("ticks", || {
        last_tick(&scrollback(env, pane))
            .filter(|n| *n >= 5)
            .map(drop)
    });
    let shell = env.pane_info(pane)["pid"].as_u64().unwrap() as u32;
    (pane, shell)
}

/// After an upgrade: the same shell, still ticking past `before`, with its scrollback intact;
/// then Ctrl-C reaches it and the window's replica matches the daemon's.
fn still_ticking(
    env: &TestEnv,
    pane: u64,
    shell: u32,
    before: u64,
    window: &auk_conformance::harness::Headless,
) {
    assert_eq!(env.pane_info(pane)["pid"].as_u64(), Some(u64::from(shell)));
    assert!(alive(shell));
    wait_for("ticks after the upgrade", || {
        last_tick(&scrollback(env, pane))
            .filter(|n| *n > before + 5)
            .map(drop)
    });
    let text = scrollback(env, pane);
    assert!(
        text.lines().any(|l| l.trim() == "tick-1"),
        "scrollback intact: {text}"
    );
    env.daemon(&format!("send {pane} keys C-c"));
    env.wait(pane, "prompt");
    env.run(pane, "echo after-$((4*4))");
    wait_for("the window's replica after reattaching", || {
        window
            .hash(pane)
            .filter(|h| *h == env.daemon_hash(pane))
            .map(drop)
    });
    assert!(window.capture(pane, true).contains("after-16"));
    assert!(window.errors().is_empty(), "{:?}", window.errors());
}

#[test]
fn upgrade_in_place_keeps_a_running_program_alive() {
    let env = env("upgrade");
    let daemon = env.start_daemon();
    let window = env.start_headless("w", &[]);
    let (pane, shell) = ticking(&env);
    let before = last_tick(&scrollback(&env, pane)).unwrap();

    let out = env.cli(&["daemon", "upgrade", "--exe", &env.exe.to_string_lossy()]);
    assert!(out.status.success(), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("upgraded"),
        "{out:?}"
    );

    // The same process image slot: exec keeps the pid.
    let state = env.daemon("state");
    assert_eq!(state["upgrades"], 1, "{state}");
    assert_eq!(
        env.daemon("ls")["daemon_pid"].as_u64(),
        Some(u64::from(daemon))
    );
    still_ticking(&env, pane, shell, before, &window);
    assert!(
        alive(window.pid),
        "the window reconnected rather than exiting"
    );
    // The new daemon makes panes as before.
    let fresh = env.new_pane(&[]);
    env.wait(fresh, "prompt");
}

#[test]
fn a_newer_client_attaching_upgrades_the_daemon() {
    let env = env("upgnew");
    let daemon = env.start_daemon_with(|c| {
        c.env("AUK_TEST_DAEMON_VERSION", "0.0.0-old");
    });
    assert_eq!(env.daemon("version")["auk"], "0.0.0-old");
    let (pane, shell) = ticking(&env);
    let before = last_tick(&scrollback(&env, pane)).unwrap();

    // A window of the real (newer) release attaches: the daemon becomes that release.
    let window = env.start_headless("w", &[]);
    wait_for("the upgraded daemon", || {
        (env.daemon("state")["upgrades"] == 1).then_some(())
    });
    assert_eq!(env.daemon("version")["auk"], env!("CARGO_PKG_VERSION"));
    assert_eq!(
        env.daemon("ls")["daemon_pid"].as_u64(),
        Some(u64::from(daemon))
    );
    still_ticking(&env, pane, shell, before, &window);
}
