//! Persistence: survival of window exit, restore after a daemon restart, the resume hook,
//! in-place upgrade and zsh integration, against the real `auk` binary with a hermetic daemon
//! per test.

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
    wait_for("the second window's replica to match the daemon's", || {
        second
            .hash(pane)
            .filter(|h| *h == env.daemon_hash(pane))
            .map(drop)
    });
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
    // Without an alias the typed line is the line itself.
    assert_eq!(last["typed"], "sh -c 'exit 3'", "{marks}");
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
    // The title comes in a write of its own, just before the command's 133;C mark.
    wait_for("the command running", || {
        (env.pane_info(pane)["running"] == true).then_some(())
    });
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

/// Whether `pane` is in the window key `main`'s layout.
fn in_layout(env: &TestEnv, pane: u64) -> bool {
    let ls = env.daemon("ls");
    ls["layout"]["main"]["sessions"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|s| s["tabs"].as_array().into_iter().flatten())
        .any(|t| tree_has(&t["root"], pane))
}

/// Whether a split tree's JSON names `pane`.
fn tree_has(node: &Value, pane: u64) -> bool {
    node["pane"] == pane
        || tree_has_child(node, "first", pane)
        || tree_has_child(node, "second", pane)
}

fn tree_has_child(node: &Value, side: &str, pane: u64) -> bool {
    node.get(side).is_some_and(|n| tree_has(n, pane))
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
    // A command that fails keeps its pane open, showing how it ended.
    assert!(in_layout(&env, pane), "{}", env.daemon("ls"));
    assert_eq!(env.pane_info(pane)["exit"], 7);
    // Closing it takes it out of the layout and the table.
    env.daemon(&format!("close {pane}"));
    assert!(!in_layout(&env, pane));
    assert!(env.control().send(&format!("capture {pane}"))["ok"] == false);

    let shell = env.new_pane(&[]);
    env.wait(shell, "prompt");
    let pid = env.pane_info(shell)["pid"].as_u64().unwrap() as u32;
    env.daemon(&format!("kill {shell}"));
    let done = env.wait(shell, "exit");
    // zsh exits with a failure status when hung up; a closed pane leaves the layout anyway.
    assert!(done["exit"].as_i64().is_some_and(|s| s != 0), "{done}");
    assert!(!alive(pid));
    assert!(!in_layout(&env, shell));
}

#[test]
fn a_command_pane_closes_when_its_command_exits_0_unless_kept_open() {
    let env = env("exit0");
    env.start_daemon();
    let keep = env.new_pane(&[]);
    env.wait(keep, "prompt");
    // Exit 0 closes the pane; it stays listed for `capture` and `wait`.
    let ok = env.new_pane(&["--", "echo done-$((40+2))"]);
    assert_eq!(env.wait(ok, "exit")["exit"], 0);
    wait_for("the finished pane to leave the layout", || {
        (!in_layout(&env, ok)).then_some(())
    });
    assert!(
        env.daemon(&format!("capture {ok}"))["text"]
            .as_str()
            .unwrap()
            .contains("done-42")
    );
    // Tern's CLI can keep it open (`--keep-open`), through the CLI verb too.
    let out = env.cli(&[
        "new",
        "tab",
        "--keep-open",
        "--cwd",
        &env.home.to_string_lossy(),
        "--",
        "true",
    ]);
    assert!(out.status.success(), "{out:?}");
    let kept: u64 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap();
    assert_eq!(env.wait(kept, "exit")["exit"], 0);
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(in_layout(&env, kept), "{}", env.daemon("ls"));
    // A shell that exits 0 closes like any program.
    env.daemon(&format!("run {keep} 'exit 0'"));
    assert_eq!(env.wait(keep, "exit")["exit"], 0);
    wait_for("the shell's pane to leave the layout", || {
        (!in_layout(&env, keep)).then_some(())
    });
    assert!(in_layout(&env, kept));
}

/// Kills a process the test spawned when the test ends, however it ends.
struct Reap(std::process::Child);

impl Drop for Reap {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Waits up to 20 s for a process to exit; returns its output.
fn finish(mut child: std::process::Child) -> std::process::Output {
    let deadline = std::time::Instant::now() + auk_conformance::harness::WAIT;
    while std::time::Instant::now() < deadline {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let _ = child.kill();
    panic!("the process didn't exit: {:?}", child.wait_with_output());
}

#[test]
fn a_second_daemon_on_the_same_socket_is_refused() {
    let env = env("onedaemon");
    let first = env.start_daemon();
    let second = env
        .command()
        .arg("daemon")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let out = finish(second);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(
        stderr.contains(&format!("another daemon serves {}", env.socket.display())),
        "{stderr}"
    );
    // The first one still serves, on the same socket.
    assert!(alive(first));
    assert_eq!(env.daemon("ls")["daemon_pid"], first);
    assert!(env.socket.exists());
}

#[test]
fn without_auk_daemon_socket_the_daemon_listens_in_the_settings_dir() {
    let env = env("defsock");
    // No `$AUK_DAEMON_SOCKET` and no `$XDG_RUNTIME_DIR`: `daemon.sock` in the settings dir,
    // which `$AUK_CONFIG_DIR` names.
    let mut cmd = env.command();
    cmd.env_remove("AUK_DAEMON_SOCKET").arg("daemon");
    let daemon = Reap(cmd.spawn().unwrap());
    let socket = env.state.join("daemon.sock");
    let control = env.state.join("daemon.ctl");
    wait_for("the daemon on its default socket", || {
        auk_conformance::harness::ping(&control).then_some(())
    });
    assert!(socket.exists());
    assert!(
        !env.socket.exists(),
        "nothing listens on the test's own socket"
    );
    // Clients find it the same way.
    let out = env
        .command()
        .env_remove("AUK_DAEMON_SOCKET")
        .args(["--json", "ls"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let ls: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(ls["daemon_pid"], daemon.0.id());
    // A different socket is a separate daemon with its own sessions.
    env.start_daemon();
    assert_ne!(env.daemon("ls")["daemon_pid"], daemon.0.id());
}

#[test]
fn with_xdg_runtime_dir_the_daemon_listens_in_its_auk_dir() {
    let env = env("xdgsock");
    // No `$AUK_DAEMON_SOCKET`, but `$XDG_RUNTIME_DIR` is set: `daemon.sock` in
    // `$XDG_RUNTIME_DIR/auk`, which the daemon creates, and not in the settings dir.
    let runtime = env.root.join("xdg");
    std::fs::create_dir_all(&runtime).unwrap();
    let mut cmd = env.command();
    cmd.env_remove("AUK_DAEMON_SOCKET")
        .env("XDG_RUNTIME_DIR", &runtime)
        .arg("daemon");
    let daemon = Reap(cmd.spawn().unwrap());
    let socket = runtime.join("auk/daemon.sock");
    let control = runtime.join("auk/daemon.ctl");
    wait_for("the daemon on its runtime-dir socket", || {
        auk_conformance::harness::ping(&control).then_some(())
    });
    assert!(socket.exists());
    assert!(
        !env.state.join("daemon.sock").exists(),
        "the daemon listens in the settings dir too"
    );
    assert!(
        !env.socket.exists(),
        "nothing listens on the test's own socket"
    );
    // Clients find it the same way, and its panes point `auk` back at it, with their pane
    // sockets beside it.
    let client = || {
        let mut cmd = env.command();
        cmd.env_remove("AUK_DAEMON_SOCKET")
            .env("XDG_RUNTIME_DIR", &runtime);
        cmd
    };
    let out = client().args(["--json", "ls"]).output().unwrap();
    assert!(out.status.success(), "{out:?}");
    let ls: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(ls["daemon_pid"], daemon.0.id());
    let out = client()
        .args(["new", "--cwd", &env.home.to_string_lossy()])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let pane: u64 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap();
    let waited = client()
        .args([
            "wait",
            &pane.to_string(),
            "--until",
            "prompt",
            "--timeout",
            "20",
        ])
        .output()
        .unwrap();
    assert!(waited.status.success(), "{waited:?}");
    let ran = client()
        .args([
            "run",
            &pane.to_string(),
            "print -r -- \"[$AUK_DAEMON_SOCKET|$AUK_PANE_SOCKET]\"",
        ])
        .output()
        .unwrap();
    assert!(ran.status.success(), "{ran:?}");
    let want = format!(
        "[{}|{}]",
        socket.display(),
        runtime.join(format!("auk/panes/{pane}.sock")).display()
    );
    wait_for("the pane to print its sockets", || {
        let out = client()
            .args(["capture", &pane.to_string()])
            .output()
            .ok()?;
        // Rows joined, as the paths may wrap.
        let text: String = String::from_utf8_lossy(&out.stdout).lines().collect();
        text.contains(&want).then_some(())
    });
}

/// Runs `line` in a new shell pane and returns the screen.
fn pane_prints(env: &TestEnv, line: &str) -> String {
    let pane = env.new_pane(&[]);
    env.wait(pane, "prompt");
    env.run(pane, line);
    env.daemon(&format!("capture {pane}"))["text"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn the_daemon_keeps_the_environment_it_started_with() {
    let env = env("keepenv");
    env.start_daemon_with(|c| {
        c.env("LANG", "en_GB.UTF-8");
    });
    // A later client with another environment opens a pane: the pane gets the daemon's.
    let out = env
        .command()
        .env("LANG", "fr_FR.UTF-8")
        .env("AUK_TEST_CLIENT_ONLY", "leaked")
        .env("SHELL", "/bin/sh")
        .args(["new", "--cwd", &env.home.to_string_lossy()])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let pane: u64 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap();
    env.wait(pane, "prompt");
    env.run(
        pane,
        "print -r -- \"[$LANG|${AUK_TEST_CLIENT_ONLY-unset}|$SHELL|${ZSH_NAME}|$HOME]\"",
    );
    let screen = env.daemon(&format!("capture {pane}"))["text"]
        .as_str()
        .unwrap()
        .to_owned();
    let expected = format!("[en_GB.UTF-8|unset|/bin/zsh|zsh|{}]", env.home.display());
    assert!(screen.contains(&expected), "{expected} not in\n{screen}");
}

#[test]
fn panes_start_from_a_fresh_login_environment_not_the_daemons() {
    let env = env("loginenv");
    let sentinel = env.root.join("daemon-only-bin");
    fs::create_dir_all(&sentinel).unwrap();
    env.start_daemon_with(|c| {
        c.env("AUK_TEST_DAEMON_ONLY", "leaked")
            .env("PATH", format!("{}:/usr/bin:/bin", sentinel.display()));
    });
    let screen = pane_prints(
        &env,
        "print -r -- \"[${AUK_TEST_DAEMON_ONLY-unset}|$PATH]\" | tr : ,",
    );
    let line = screen
        .lines()
        .find(|l| l.starts_with("[unset|") || l.starts_with("[leaked|"))
        .unwrap_or_else(|| panic!("no line in\n{screen}"));
    assert!(
        line.starts_with("[unset|"),
        "the daemon's variable leaked: {line}"
    );
    assert!(
        !line.contains("daemon-only-bin"),
        "the daemon's PATH leaked: {line}"
    );
    assert!(line.contains("/usr/bin"), "the login shell's PATH: {line}");

    // The pane-identity set, with the TERN_ prefix swapped for AUK_.
    let screen = pane_prints(
        &env,
        "print -r -- \"<$AUK_IDENTITY|$AUK_LENSES|$AUK_COMPLETE|$AUK_BLOB_DIR|${+AUK_WINDOW_SOCKET}${#AUK_WINDOW_SOCKET}|$TERM_PROGRAM_VERSION>\"",
    );
    let user = Command::new("id").arg("-un").output().unwrap();
    let host = Command::new("hostname").arg("-s").output().unwrap();
    let identity = format!(
        "{}@{}",
        String::from_utf8_lossy(&user.stdout).trim(),
        String::from_utf8_lossy(&host.stdout).trim()
    );
    let expected = format!(
        "<{identity}|1|0|{}|10|{}>",
        env.state.join("blobs").display(),
        env!("CARGO_PKG_VERSION")
    );
    assert!(screen.contains(&expected), "{expected} not in\n{screen}");
    // Lens claims follow `command_lenses`.
    env.daemon("settings --command-lenses off");
    let screen = pane_prints(&env, "print -r -- \"<lenses=$AUK_LENSES>\"");
    assert!(screen.contains("<lenses=0>"), "{screen}");
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
// Restore

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
    wait_for("the window's replica to match the daemon's", || {
        window
            .hash(pane)
            .filter(|h| *h == env.daemon_hash(pane))
            .map(drop)
    });
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

/// A block that shows its size and a count its keys bump, and saves the count.
fn sizer_plugin(env: &TestEnv) {
    let dir = env.state.join("plugins/sizer");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("plugin.toml"),
        "schema = 1\nid = \"sizer\"\nname = \"Sizer\"\nversion = \"1\"\nhost = \"host.luau\"\n[[blocks]]\nid = \"show\"\ntitle = \"Show\"\n",
    )
    .unwrap();
    fs::write(
        dir.join("host.luau"),
        r#"tern.block.define("show", {
            init = function(cx, args, saved)
                return { n = saved and saved.n or 0, cx = cx }
            end,
            view = function(s)
                return { main = tern.ui.text("size " .. s.cx.cols .. "x" .. s.cx.rows .. " n=" .. s.n) }
            end,
            key = function(s, key) s.n += 1 return true end,
            save = function(s) return { n = s.n } end,
        })"#,
    )
    .unwrap();
}

/// The text of a pane's live surface (a block's view), from the daemon.
fn block_text(env: &TestEnv, pane: u64) -> String {
    let tsp = env.daemon(&format!("tsp {pane}"));
    tsp["surfaces"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|s| s["state"] == "live")
        .map(|s| s["doc"].to_string())
        .collect()
}

#[test]
fn a_restored_block_restarts_at_its_last_size_on_a_blank_screen_and_needs_its_type() {
    let env = env("blocksize");
    sizer_plugin(&env);
    let daemon = env.start_daemon();
    let pane = env.daemon("block sizer.show --size 90x25")["pane"]
        .as_u64()
        .expect("a pane");
    wait_for("the block's first view", || {
        block_text(&env, pane)
            .contains("size 90x25 n=0")
            .then_some(())
    });
    for _ in 0..3 {
        env.daemon(&format!("send {pane} text k"));
    }
    wait_for("three keys counted and saved", || {
        (block_text(&env, pane).contains("n=3")
            && env.pane_info(pane)["kind"]["saved"] == r#"{"n":3}"#)
            .then_some(())
    });
    // A size a window gave it later is the one it keeps.
    env.daemon(&format!("resize {pane} 70 18"));
    wait_for("the block's new size", || {
        block_text(&env, pane)
            .contains("size 70x18 n=3")
            .then_some(())
    });
    let (_, seq_before) = env.daemon_hash(pane);
    wait_checkpoint(&env, &[pane]);

    env.kill9(daemon);
    let daemon = env.start_daemon();
    // Back from its save, at its last size, on a blank screen: a new stream with one live
    // surface, not the old screen.
    wait_for("the restored block", || {
        block_text(&env, pane)
            .contains("size 70x18 n=3")
            .then_some(())
    });
    let info = env.pane_info(pane);
    assert_eq!(
        (info["cols"].as_u64(), info["rows"].as_u64()),
        (Some(70), Some(18)),
        "{info}"
    );
    let tsp = env.daemon(&format!("tsp {pane}"));
    let surfaces = tsp["surfaces"].as_array().unwrap();
    assert_eq!(surfaces.len(), 1, "only the new run's surface: {tsp:#}");
    let (_, seq_after) = env.daemon_hash(pane);
    assert!(
        seq_after < seq_before,
        "a fresh stream: {seq_after} vs {seq_before}"
    );
    wait_checkpoint(&env, &[pane]);

    // With its plugin disabled (`plugins_disabled` in the settings file), no Ready plugin
    // defines the block type: the pane isn't restored, and leaves the layout.
    fs::write(
        env.state.join("settings.json"),
        json!({"plugins_disabled": ["sizer"]}).to_string(),
    )
    .unwrap();
    env.kill9(daemon);
    env.start_daemon();
    let catalog = env.daemon("plugin list");
    assert_eq!(catalog["plugins"][0]["status"], "disabled", "{catalog}");
    let ls = env.daemon("ls");
    assert!(
        !ls["panes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["id"] == pane),
        "{ls}"
    );
    assert!(
        ls["layout"]
            .get("main")
            .is_none_or(|w| !w.to_string().contains(&format!("\"pane\":{pane}"))),
        "{ls}"
    );
}

#[test]
fn a_block_survives_a_daemon_restart_with_the_same_args_and_its_save() {
    let env = env("blockargs");
    let dir = env.state.join("plugins/argsy");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("plugin.toml"),
        "schema = 1\nid = \"argsy\"\nname = \"Argsy\"\nversion = \"1\"\nhost = \"host.luau\"\n[[blocks]]\nid = \"b\"\ntitle = \"B\"\n",
    )
    .unwrap();
    fs::write(
        dir.join("host.luau"),
        r#"tern.block.define("b", {
            init = function(cx, args, saved)
                return { args = table.concat(args, "|"), n = #args, inits = (saved and saved.inits or 0) + 1 }
            end,
            view = function(s)
                return { main = tern.ui.text("args=[" .. s.args .. "] n=" .. s.n .. " inits=" .. s.inits) }
            end,
            save = function(s) return { inits = s.inits } end,
        })"#,
    )
    .unwrap();
    let daemon = env.start_daemon();
    // Opened through the CLI with args, one of them with a space.
    let out = env.cli(&["block", "argsy.b", "alpha", "two words", "-x"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let pane: u64 = String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .expect("a block pane id");
    wait_for("the block's first view", || {
        block_text(&env, pane)
            .contains("args=[alpha|two words|-x] n=3 inits=1")
            .then_some(())
    });
    let kind = env.pane_info(pane)["kind"].clone();
    assert_eq!(kind["args"], json!(["alpha", "two words", "-x"]), "{kind}");
    wait_for("the first save", || {
        (env.pane_info(pane)["kind"]["saved"] == r#"{"inits":1}"#).then_some(())
    });
    wait_checkpoint(&env, &[pane]);

    env.kill9(daemon);
    env.start_daemon();
    // The same pane id, restarted with the same args and its last save.
    wait_for("the restored block", || {
        block_text(&env, pane)
            .contains("args=[alpha|two words|-x] n=3 inits=2")
            .then_some(())
    });
    let kind = env.pane_info(pane)["kind"].clone();
    assert_eq!(
        (&kind["block"], &kind["args"]),
        (&json!("argsy.b"), &json!(["alpha", "two words", "-x"])),
        "{kind}"
    );
}

/// `auk block` run in a pane opens the block beside it (a split in the same tab, so the window
/// shows it), in the shell's directory: `stack.json` names the file where the command ran.
#[test]
fn a_block_opened_from_a_pane_splits_it_in_its_directory() {
    let env = env("block-beside");
    let dir = env.root.join("plugins/where");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("plugin.toml"),
        "schema = 1\nid = \"where\"\nname = \"Where\"\nversion = \"1\"\nhost = \"host.luau\"\n[[blocks]]\nid = \"b\"\ntitle = \"B\"\n",
    )
    .unwrap();
    fs::write(
        dir.join("host.luau"),
        r#"tern.block.define("b", {
            init = function(cx, args) return { cwd = cx.cwd or "?" } end,
            view = function(s) return { main = tern.ui.text("cwd=" .. s.cwd) } end,
        })"#,
    )
    .unwrap();
    assert!(
        env.cli(&["plugin", "link", dir.to_str().unwrap()])
            .status
            .success()
    );
    let _daemon = env.start_daemon();
    let out = env.cli(&["new"]);
    let shell: u64 = String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .expect("a pane id");
    let work = env.root.join("work");
    fs::create_dir_all(&work).unwrap();
    let out = env
        .command()
        .env("AUK_PANE", shell.to_string())
        .current_dir(&work)
        .args(["block", "where.b"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let block: u64 = String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .expect("a block pane id");
    let ls: Value = serde_json::from_slice(&env.cli(&["ls", "--json"]).stdout).unwrap();
    let session = |id: u64| {
        ls["panes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == id)
            .map(|p| p["session"].clone())
    };
    assert_eq!(session(block), session(shell), "{ls}");
    let cwd = real(&work);
    wait_for("the block's view", || {
        block_text(&env, block)
            .contains(&format!("cwd={}", cwd.display()))
            .then_some(())
    });
}

#[test]
fn command_blocks_keep_their_line_cwd_raw_width_status_and_view_across_a_restart() {
    let env = env("lensrestore");
    let examples = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../third_party/tern-sdk/plugins/examples/terraform");
    assert!(examples.is_dir(), "the SDK example terraform is missing");
    copy_dir(&examples, &env.state.join("plugins/terraform"));
    let plan =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../plugins/tests/fixtures/terraform-plan.txt");
    assert!(plan.is_file(), "the plan fixture is missing");
    let tf = env.root.join("bin/terraform");
    executable(
        &tf,
        &format!("#!/bin/sh\ncat '{}'\nexit 2\n", plan.display()),
    );
    let daemon = env.start_daemon();
    let pane = env.new_pane(&["--size", "110x30"]);
    env.wait(pane, "prompt");
    env.run(pane, &format!("{} plan", tf.display()));
    wait_for("the command block's view", || {
        let b = env.daemon(&format!("lens {pane}"))["blocks"][0].clone();
        (!b["view"].is_null()).then_some(b)
    });
    // Views come at most every 250 ms: let the last one land before the checkpoint.
    std::thread::sleep(std::time::Duration::from_millis(600));
    wait_checkpoint(&env, &[pane]);
    let before = env.daemon(&format!("lens {pane}"))["blocks"][0].clone();
    assert_eq!(before["status"], 2);
    assert_eq!(before["cols"], 110);

    env.kill9(daemon);
    env.start_daemon();
    env.wait(pane, "prompt");
    let after = env.daemon(&format!("lens {pane}"))["blocks"][0].clone();
    for key in [
        "line", "typed", "cwd", "raw", "cols", "status", "done", "plugin", "lens", "view",
    ] {
        assert_eq!(after[key], before[key], "{key}: {after:#}");
    }
    // The view stays visible: windows that attach get it with the snapshot.
    let window = env.start_headless("w", &[]);
    let seen = wait_for("the restored view in a window", || {
        let v = window.request(&format!("lens {pane}"))["blocks"][0]["view"].clone();
        (!v.is_null()).then_some(v)
    });
    assert_eq!(seen, before["view"]);
    // The lens state behind it is rebuilt on the next event: the view's action still works.
    let ev =
        json!({"ev": "action", "id": "n1", "act": "copy-address", "value": "aws_instance.old"});
    window.request(&format!("event {pane} lens:0 {}", quote(&ev.to_string())));
    wait_for("the rehydrated lens's copy effect", || {
        window
            .request("effects")
            .as_array()?
            .iter()
            .find(|e| e["effect"] == "copy" && e["text"] == "aws_instance.old")
            .cloned()
    });
}

// ---------------------------------------------------------------------------------------------
// The resume hook

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
    // A stand-in `omp` on the shell's PATH says how it was started. A live omp 18.8.4
    // sets `omp_session_file` only when TERM_PROGRAM is `tern`, so here a
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
// In-place upgrade

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

// ---------------------------------------------------------------------------------------------
// A graceful stop (restore follows a quit, a crash or a reboot), and blobs across a restart

/// Waits until the daemon whose log is `daemon.log` says it stopped.
fn wait_stopped(env: &TestEnv) {
    let log = env.root.join("daemon.log");
    wait_for("the daemon to stop", || {
        fs::read_to_string(&log)
            .ok()?
            .contains("auk daemon: stopped")
            .then_some(())
    });
}

/// Brings a pane's state to a checkpoint, then removes the state file. Nothing changes after,
/// so the checkpointer has no reason to write it again: only the stop path can bring it back.
fn checkpoint_then_remove_the_state_file(env: &TestEnv, pane: u64) -> PathBuf {
    wait_checkpoint(env, &[pane]);
    let file = env.state.join("daemon.state");
    fs::remove_file(&file).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert!(
        !file.exists(),
        "the checkpointer rewrote an unchanged state: the stop path wouldn't be tested"
    );
    file
}

#[test]
fn a_graceful_stop_by_sigterm_or_shutdown_writes_the_state_file_and_panes_come_back() {
    let env = env("graceful");
    let daemon = env.start_daemon();
    let work = env.home.join("work");
    fs::create_dir_all(&work).unwrap();
    let pane = env.new_pane(&["--cwd", &work.to_string_lossy()]);
    env.wait(pane, "prompt");
    env.run(pane, "echo before-term-$((4*5))");
    let layout = env.daemon("ls")["layout"].clone();
    let shell = env.pane_info(pane)["pid"].as_u64().unwrap() as u32;

    // SIGTERM: the daemon writes the state file on its way out and hangs up its panes.
    let file = checkpoint_then_remove_the_state_file(&env, pane);
    let status = Command::new("/bin/kill")
        .args(["-TERM", &daemon.to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    wait_stopped(&env);
    assert!(file.is_file(), "SIGTERM didn't write the state file");
    wait_for("the old shell to end with the daemon", || {
        (!alive(shell)).then_some(())
    });

    env.start_daemon();
    assert_eq!(env.daemon("ls")["layout"], layout);
    env.wait(pane, "prompt");
    let info = env.pane_info(pane);
    let new_shell = info["pid"].as_u64().unwrap() as u32;
    assert_ne!(new_shell, shell);
    assert_eq!(real(Path::new(info["cwd"].as_str().unwrap())), real(&work));
    assert!(scrollback(&env, pane).contains("before-term-20"));

    // The `shutdown` command (the app's quit path for a private daemon), the same way.
    env.run(pane, "echo before-shutdown-$((5*6))");
    let file = checkpoint_then_remove_the_state_file(&env, pane);
    env.daemon("shutdown");
    wait_stopped(&env);
    assert!(file.is_file(), "shutdown didn't write the state file");
    wait_for("the second shell to end with the daemon", || {
        (!alive(new_shell)).then_some(())
    });

    env.start_daemon();
    env.wait(pane, "prompt");
    let text = scrollback(&env, pane);
    assert!(
        text.contains("before-term-20") && text.contains("before-shutdown-30"),
        "{text}"
    );
    env.run(pane, "echo after-$((7*6))");
    assert!(scrollback(&env, pane).contains("after-42"));
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Whether a window draws node `id` of `pane` as a picture, not a placeholder.
fn drawn_picture(window: &auk_conformance::harness::Headless, pane: u64, id: &str) -> bool {
    let tree = window.request(&format!("tree {pane} .sf-image"));
    let Some(els) = tree.as_array() else {
        return false;
    };
    let Some(at) = els.iter().position(|e| e["id"] == id) else {
        return false;
    };
    let class = els[at]["class"].as_str().unwrap_or("");
    !class.contains("missing")
        && els[at + 1..]
            .iter()
            .take_while(|e| e["id"].is_null())
            .any(|e| {
                e["class"]
                    .as_str()
                    .is_some_and(|c| c.contains("sf-img-svg"))
            })
}

#[test]
fn a_blob_survives_a_daemon_restart_and_a_restored_surface_still_shows_it() {
    let env = env("blobrestart");
    let daemon = env.start_daemon();
    let pane = env.new_pane(&[]);
    env.wait(pane, "prompt");

    // A program sends a picture, shows it in a kept flow surface, and exits.
    let svg = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"20\" height=\"10\">\
               <rect width=\"20\" height=\"10\" fill=\"#cc3311\"/></svg>";
    let id = sha256_hex(svg.as_bytes());
    let b64 = base64::engine::general_purpose::STANDARD;
    let probe = Probe::run_in_shell(&env, pane);
    exchange(&probe, &hello(&[]));
    exchange(
        &probe,
        &auk_conformance::probe::tsp("b", &[("mime", "image/svg+xml")], &b64.encode(svg)),
    );
    exchange(
        &probe,
        &open_with(
            "pic",
            "flow",
            json!([{"id": "img", "k": "image", "p": {"blob": id, "alt": "Pic"}}]),
        ),
    );
    exchange(&probe, &tsp_json("x", &json!({"id": "pic", "keep": true})));
    probe.tty("cooked");
    probe.exit(0);
    env.wait(pane, "prompt");
    let file = env.state.join("blobs").join(&id);
    assert!(file.is_file(), "the blob wasn't written through to disk");
    wait_checkpoint(&env, &[pane]);

    // The daemon dies; its memory goes with it.
    env.kill9(daemon);
    env.start_daemon();
    env.wait(pane, "prompt");
    let after = env.daemon(&format!("tsp {pane}"));
    let s = surface(&after, "pic").unwrap_or_else(|| panic!("pic missing: {after}"));
    assert_eq!(
        (&s["state"], &s["restored"]),
        (&json!("closed"), &json!(true)),
        "{s}"
    );
    assert_eq!(fs::read(&file).unwrap(), svg.as_bytes());

    // A window attaching now is sent the blob from disk and draws the picture, and showing it
    // makes it the most recently shown blob (its file's mtime moves).
    let shown_before = fs::metadata(&file).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    let window = env.start_headless("w", &[]);
    wait_for("the window to draw the restored picture", || {
        drawn_picture(&window, pane, "img").then_some(())
    });
    let shown_after = fs::metadata(&file).unwrap().modified().unwrap();
    assert!(
        shown_after > shown_before,
        "showing the blob didn't touch it: {shown_before:?} {shown_after:?}"
    );
    assert!(window.errors().is_empty(), "{:?}", window.errors());
}

/// A daemon whose socket file is deleted can't be reached again, so it stops and takes its
/// shells with it (no daemon outlives its runtime dir).
#[test]
fn a_daemon_whose_socket_is_deleted_stops_with_its_shells() {
    let env = env("orphan");
    let daemon = env.start_daemon();
    let pane = env.new_pane(&[]);
    let shell = env.daemon("ls --json")["panes"]
        .as_array()
        .and_then(|ps| ps.iter().find(|p| p["id"] == pane))
        .and_then(|p| p["pid"].as_u64())
        .expect("the pane's shell pid") as u32;
    assert!(alive(shell));
    fs::remove_file(&env.socket).expect("delete the socket");
    // The daemon is this test's child, so once it exits it is a zombie until reaped.
    let gone = |pid: u32| {
        let out = Command::new("/bin/ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output();
        out.map(|o| {
            String::from_utf8_lossy(&o.stdout).trim().is_empty() || o.stdout.starts_with(b"Z")
        })
        .unwrap_or(false)
    };
    wait_for("the daemon to stop", || gone(daemon).then_some(()));
    wait_for("its shell to end", || gone(shell).then_some(()));
}
