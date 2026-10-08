//! Persistence (GOAL.md): survival of window exit (AC1) and zsh integration (M2), against the
//! real `auk` binary with a hermetic daemon per test.

use std::fs;
use std::path::Path;

use auk_conformance::harness::{TestEnv, alive, wait_for};

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
