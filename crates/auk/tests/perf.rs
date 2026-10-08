//! The `perf` suite (GOAL.md, spec §18.1, §23 S2). It runs alone, after every other suite
//! (`scripts/verify.sh`), in release builds, so nothing else skews its numbers. Each test prints
//! its numbers; `PROGRESS.md` records them.
//!
//! **Frame budget (§18.1).** The real app (`auk`, no subcommand) on a test daemon, its window
//! sized to 1200×800 points at the display's scale (2× here). Two loads:
//! - `omp-session` (`fixtures/tsp/omp-session.jsonl`) played with `surface-play … paced` (its
//!   recorded timing, gaps capped at 2 s by the daemon) into a pane the app shows;
//! - a 10,000-entry omp-style transcript (omp's own final entries from that session, repeated
//!   with fresh ids: user cards, assistant markdown, tool cards with diffs and output) loaded into
//!   a pane, then scrolled through with wheel events, a few lines at a time at about 120 Hz.
//!
//! Frame time is read from the app's automation endpoint (`frame-stats`, crates/app/src/gfx.rs
//! `FrameClock`). For each presented frame it runs from the start of the window's `frame` work
//! (syncing surfaces, layout, paint, grid snapshots, encoding) to the later of (a) the GPU
//! finishing that frame's command buffers, as wgpu's submitted-work-done callback reports it from
//! a thread blocked in `device.poll` (so it fires as the Metal command buffer completes), and
//! (b) the CPU finishing its own tail (acks, the accessibility update), minus the time the frame
//! spent in `get_current_texture` waiting for a drawable, which is vsync pacing, not work. The
//! budget is p95 ≤ 8.3 ms over every frame drawn during the load.
//!
//! **Against Ghostty (S2).** The same measurement for both terminals, through the screen probe
//! (`crates/conformance/screen/probe.swift`, built fresh with `swiftc`), which watches the
//! terminal's window with ScreenCaptureKit (every composed frame, with its display time in host
//! time):
//! - *`cat` of a 100 MB file*: each terminal runs the same script in a fresh window/pane of the
//!   same size in cells; the script waits on a FIFO, then `cat`s the file and prints a full-width
//!   pure green bar. The probe writes the FIFO and reports the display time of the first frame
//!   showing the bar, minus the write time: start of the command to the prompt-equivalent being
//!   visible. Median of 5 runs each. Ghostty runs with `--config-default-files=false`, so the
//!   user's config doesn't apply, and its default scrollback.
//! - *Keystroke to screen*: each terminal runs `stty -icanon; cat` (the tty echoes each key); the
//!   probe posts `x` key presses to the app's process as `CGEvent`s (as a keyboard's reach it),
//!   at a random phase against the refresh, and reports the display time of the first frame that
//!   differs from the one before the press, minus the post time. Median of 40 presses each.
//!   Cursor blinking is off in both, so nothing else changes the window.
//!
//! Either check passes when Auk's median is at most 1.5× Ghostty's. Screen capture needs the
//! Screen Recording permission, and posting keys needs Accessibility, both for the app that runs
//! the tests; Ghostty must run its command within 20 s of launch (if it never has been opened,
//! Gatekeeper's first-launch prompt stops it). A missing one fails the test, named. Before it needs
//! Accessibility, the latency test prints what it can measure without: keys typed through Auk's
//! endpoint (`type`: an `NSEvent` sent to its window), and for both terminals an `x` written to
//! the terminal's tty (output to screen, the bulk of a keystroke's echo), timed the same way.
//!
//! **Parser throughput.** A 100 MB mixed stream through `auk-term` at 80×24 with 10,000 rows of
//! scrollback must parse at 200 MB/s or more.
#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use auk_conformance::app::App;
use auk_conformance::harness::{TestEnv, wait_for};
use auk_conformance::perf::{
    Ghostty, Permissions, big_jpeg, image_recording, median, percentile, permissions, run_probe,
    screen_probe, transcript_recording, write_text_file,
};
use serde_json::Value;

/// The frame budget (§18.1): 120 Hz.
const BUDGET_MS: f64 = 8.3;
/// Auk's number may be at most this many times Ghostty's (§23 S2).
const GHOSTTY_FACTOR: f64 = 1.5;
/// `cat` runs per terminal.
const RUNS: usize = 5;
/// The window, in points.
const WINDOW: (f64, f64) = (1200.0, 800.0);

fn env(name: &str) -> TestEnv {
    let env = TestEnv::new(env!("CARGO_BIN_EXE_auk"), name);
    env.start_daemon();
    env
}

fn tmp_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
}

/// The app, its window sized to [`WINDOW`], showing a new pane started with `args` (`new` flags
/// and command). Returns the app and the pane.
fn app_with_pane(env: &TestEnv, args: &[&str]) -> (App, u64) {
    let app = App::start(env, "a", &[]);
    app.request(&format!("window-size {} {}", WINDOW.0, WINDOW.1));
    let pane = env.new_pane(args);
    wait_for("the app to show the pane", || {
        (app.send(&format!("focus {pane}"))["ok"] == true).then_some(())
    });
    app.wait_primary(pane);
    wait_for("the window at 1200×800 points", || {
        let windows = app.request("windows");
        (windows[0]["size"] == serde_json::json!([WINDOW.0, WINDOW.1])).then_some(())
    });
    (app, pane)
}

/// Plays `file` into `pane` with `surface-play` and `options`, to completion.
fn play(env: &TestEnv, pane: u64, file: &Path, options: &[&str]) {
    let pane = pane.to_string();
    let file = file.to_string_lossy();
    let mut args = vec!["surface-play", "--pane", &pane, &file];
    args.extend(options);
    let out = env.cli(&args);
    assert!(out.status.success(), "surface-play {options:?}: {out:?}");
}

/// Restarts `pane` as `surface-play`'s stand-in program (a recording of one empty `pty` line),
/// so the measured play starts on a settled pane.
fn stand_in(env: &TestEnv, pane: u64) {
    let empty = env.root.join("empty.jsonl");
    std::fs::write(&empty, "{\"dir\":\"out\",\"verb\":\"pty\",\"body\":\"\"}\n").expect("write");
    play(env, pane, &empty, &[]);
    std::thread::sleep(Duration::from_millis(300));
}

/// Prints and returns the app's frame statistics.
fn frame_stats(app: &App, what: &str) -> Value {
    let stats = app.request("frame-stats");
    eprintln!("{what}: {stats}");
    stats
}

fn assert_budget(stats: &Value, what: &str) {
    let frames = stats["frame"]["count"].as_u64().unwrap_or(0);
    assert!(frames >= 50, "{what}: only {frames} frames timed: {stats}");
    let p95 = stats["frame"]["p95"].as_f64().expect("a p95");
    assert!(
        p95 <= BUDGET_MS,
        "{what}: p95 frame time {p95:.2} ms is over the {BUDGET_MS} ms budget ({stats})"
    );
}

#[test]
fn frame_budget_omp_session_replay() {
    let env = env("perf-omp");
    let (app, pane) = app_with_pane(&env, &[]);
    let file = auk_conformance::fixtures_dir().join("omp-session.jsonl");
    stand_in(&env, pane);
    app.request("frame-stats --reset");
    let started = Instant::now();
    play(&env, pane, &file, &["paced"]);
    // Let the last frames draw.
    std::thread::sleep(Duration::from_millis(500));
    eprintln!("replayed in {:.1} s", started.elapsed().as_secs_f64());
    let layout = app.request(&format!("pane-layout {pane}"));
    assert!(
        layout["blocks"].as_array().is_some_and(|b| !b.is_empty()),
        "the session's surface isn't in the pane: {layout}"
    );
    let stats = frame_stats(&app, "omp-session replay");
    assert_budget(&stats, "omp-session replay");
}

#[test]
fn frame_budget_scrolling_a_10000_entry_transcript() {
    let env = env("perf-scroll");
    let recording = env.root.join("transcript.jsonl");
    let jsonl = transcript_recording(10_000, 50, 16 << 10, 0);
    eprintln!("transcript recording: {:.1} MB", jsonl.len() as f64 / 1e6);
    std::fs::write(&recording, jsonl).expect("write");
    let (app, pane) = app_with_pane(&env, &[]);
    stand_in(&env, pane);
    let started = Instant::now();
    play(&env, pane, &recording, &[]);
    let seq = env.daemon(&format!("state-hash {pane}"))["seq"]
        .as_u64()
        .expect("a seq");
    app.caught_up(pane, seq);
    // The window syncs the transcript in over several frames: wait until its height holds.
    let mut last = -1.0;
    let layout = wait_for("the whole transcript laid out in the pane", || {
        std::thread::sleep(Duration::from_millis(1000));
        let l = app.request(&format!("pane-layout {pane}"));
        let h = l["max_scroll"].as_f64()?;
        let settled = h > 100_000.0 && h == last;
        last = h;
        settled.then_some(l)
    });
    eprintln!(
        "loaded in {:.1} s; scrollable {} px",
        started.elapsed().as_secs_f64(),
        layout["max_scroll"]
    );
    std::thread::sleep(Duration::from_millis(500));
    app.request("frame-stats --reset");
    let (x, y) = (WINDOW.0 / 2.0, WINDOW.1 / 2.0);
    // Up through a good part of the transcript, then back down part of the way.
    for i in 0..1000 {
        let lines = if i < 700 { 6 } else { -6 };
        app.request(&format!("scroll {lines} {x} {y}"));
        std::thread::sleep(Duration::from_millis(8));
    }
    std::thread::sleep(Duration::from_millis(300));
    let after = app.request(&format!("pane-layout {pane}"));
    let moved = after["from_bottom"].as_f64().unwrap_or(0.0);
    eprintln!("scrolled to {moved} px from the bottom");
    assert!(moved > 10_000.0, "the scroll didn't move far: {after}");
    let stats = frame_stats(&app, "10,000-entry transcript scroll");
    assert_budget(&stats, "10,000-entry transcript scroll");
}

/// The load check (amendment A10, §7.10 cost, §18.1): the 10,000-entry omp-style transcript
/// arriving as a program would stream it, 50 entries per 16 ms frame (`surface-play … paced`,
/// about 3.2 s of input), into a pane the app shows. Every frame drawn during the load stays in
/// the frame budget at p95, and the window has drawn the last entry within 1.5× the input's
/// duration (the time `surface-play` took to send it all) of the first frame being sent.
#[test]
fn frame_budget_loading_a_10000_entry_transcript() {
    const STEP_MS: u64 = 16;
    const SETTLE_FACTOR: f64 = 1.5;
    let env = env("perf-load");
    let recording = env.root.join("transcript.jsonl");
    let jsonl = transcript_recording(10_000, 50, 16 << 10, STEP_MS);
    // The last entry's id: the window has caught up once it draws it.
    let last_frame: Value = serde_json::from_str(jsonl.lines().last().expect("frames"))
        .expect("a JSON line");
    let last_id = last_frame["body"]["ops"]
        .as_array()
        .and_then(|ops| ops.last())
        .and_then(|op| op[1].as_str())
        .expect("the last op adds an entry")
        .to_owned();
    let frames = jsonl.lines().filter(|l| l.contains("\"verb\":\"f\"")).count();
    let nominal = (frames as u64 - 1) * STEP_MS;
    eprintln!(
        "transcript recording: {:.1} MB, {frames} frames {STEP_MS} ms apart ({nominal} ms)",
        jsonl.len() as f64 / 1e6
    );
    std::fs::write(&recording, jsonl).expect("write");
    let (app, pane) = app_with_pane(&env, &[]);
    stand_in(&env, pane);
    app.request("frame-stats --reset");
    let started = Instant::now();
    play(&env, pane, &recording, &["paced"]);
    let input = started.elapsed().as_secs_f64();
    let drawn = |app: &App| {
        app.try_request(&format!("node-box {pane} {last_id}"))
            .and_then(|b| b[2].as_f64())
            .is_some_and(|w| w > 0.0)
    };
    let settled = loop {
        if drawn(&app) {
            break started.elapsed().as_secs_f64();
        }
        assert!(
            started.elapsed().as_secs_f64() < input * 10.0 + 10.0,
            "the window never drew the transcript's last entry ({last_id})"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    let stats = app.request("frame-stats --series");
    let series = stats["series"].clone();
    let mut stats = stats;
    stats.as_object_mut().expect("an object").remove("series");
    eprintln!("10,000-entry transcript load: {stats}");
    let cpu: Vec<f64> = series["cpu"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_f64).collect())
        .unwrap_or_default();
    // How per-frame work changes as the transcript grows (§7.10: in proportion to the ops, not
    // to the document): each phase's median over each fifth of the load's frames.
    let fifths = |v: &[f64]| -> String {
        let n = v.len() / 5;
        if n == 0 {
            return String::new();
        }
        (0..5)
            .map(|i| format!("{:.2}", median(&v[i * n..(i + 1) * n])))
            .collect::<Vec<_>>()
            .join(" / ")
    };
    eprintln!("CPU per frame by fifth of the load (median ms): {}", fifths(&cpu));
    if let Some(phases) = series["phases"].as_object() {
        for (name, v) in phases {
            let v: Vec<f64> = v
                .as_array()
                .map(|a| a.iter().filter_map(Value::as_f64).collect())
                .unwrap_or_default();
            eprintln!("  {name}: {}", fifths(&v));
        }
    }
    eprintln!(
        "input sent in {input:.2} s ({:.2} s nominal); last entry drawn at {settled:.2} s ({:.2}× the input)",
        nominal as f64 / 1000.0,
        settled / input
    );
    let layout = app.request(&format!("pane-layout {pane}"));
    assert!(
        layout["max_scroll"].as_f64().is_some_and(|h| h > 100_000.0),
        "the transcript isn't in the pane: {layout}"
    );
    assert_budget(&stats, "10,000-entry transcript load");
    assert!(
        settled <= SETTLE_FACTOR * input,
        "the load settled in {settled:.2} s, over {SETTLE_FACTOR}× the input's {input:.2} s"
    );
}

/// Amendment A9: a big picture decodes off the frame path. A 4096×4096 JPEG arrives in a pane
/// the app shows and is drawn, and no frame from its arrival to its drawing takes as long as
/// the decode itself (timed here, in this process, with the views' decoder): none waited for
/// it.
#[test]
fn image_decoding_never_stalls_a_frame() {
    let jpeg = big_jpeg(4096, 4096);
    let started = Instant::now();
    let picture = auk_views::image::decode(&jpeg, Some("image/jpeg"), 2.0, "p").expect("decode");
    let decode_ms = started.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(picture.size, (4096.0, 4096.0));
    drop(picture);
    eprintln!(
        "4096×4096 JPEG: {:.1} MB, decodes in {decode_ms:.0} ms here",
        jpeg.len() as f64 / 1e6
    );
    let env = env("perf-image");
    let recording = env.root.join("image.jsonl");
    let (jsonl, _) = image_recording(&jpeg, "image/jpeg", "photo");
    std::fs::write(&recording, jsonl).expect("write");
    let (app, pane) = app_with_pane(&env, &[]);
    stand_in(&env, pane);
    app.request("frame-stats --reset");
    play(&env, pane, &recording, &[]);
    // Drawn: the picture's box (square, the pane's width), not the placeholder card.
    wait_for("the photo drawn", || {
        let b = app.try_request(&format!("node-box {pane} photo"))?;
        let (w, h) = (b[2].as_f64()?, b[3].as_f64()?);
        (h > 500.0 && (w - h).abs() < 4.0).then_some(())
    });
    std::thread::sleep(Duration::from_millis(300));
    let stats = frame_stats(&app, "4096×4096 JPEG arriving");
    let max = stats["frame"]["max"].as_f64().expect("a max");
    assert!(
        max < decode_ms,
        "a frame took {max:.1} ms, as long as the {decode_ms:.0} ms decode: it waited for it"
    );
}

// Against Ghostty (S2).

/// Fails naming each missing permission.
fn require(perms: &Permissions, need_keys: bool) {
    let mut missing = perms.missing();
    if !need_keys {
        missing.retain(|m| !m.starts_with("Accessibility"));
    }
    assert!(
        missing.is_empty(),
        "prerequisite missing: {}",
        missing.join("; ")
    );
}

/// Writes an executable script `name` in `dir`.
fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write a script");
    let status = std::process::Command::new("chmod")
        .arg("+x")
        .arg(&path)
        .status()
        .expect("chmod");
    assert!(status.success());
    path
}

/// A run's files: the FIFO the probe starts it with, and the file the script creates once it's
/// up.
struct Run {
    go: PathBuf,
    ready: PathBuf,
    script: PathBuf,
}

/// The `cat` script for run `n`: waits for "go", cats `file`, prints the green bar.
fn cat_run(dir: &Path, n: &str, file: &Path) -> Run {
    let go = dir.join(format!("go-{n}"));
    let ready = dir.join(format!("ready-{n}"));
    let _ = std::fs::remove_file(&go);
    let status = std::process::Command::new("mkfifo")
        .arg(&go)
        .status()
        .expect("mkfifo");
    assert!(status.success());
    let body = format!(
        "stty -echo\nclear\n: > '{ready}'\nread go < '{go}'\ncat '{file}'\nprintf '\\033[48;2;0;255;0m%s\\033[0m\\n' \"$(printf '%*s' 60 '')\"\nexec sleep 600",
        ready = ready.display(),
        go = go.display(),
        file = file.display(),
    );
    let script = script(dir, &format!("cat-{n}.sh"), &body);
    Run { go, ready, script }
}

fn probe_marker(probe: &Path, pid: i32, run: &Run) -> f64 {
    let v = run_probe(
        probe,
        &[
            "marker",
            "--pid",
            &pid.to_string(),
            "--go",
            &run.go.to_string_lossy(),
            "--timeout",
            "60",
        ],
    )
    .unwrap_or_else(|e| panic!("{e}"));
    v["ms"].as_f64().expect("ms")
}

/// The pane's size in cells.
fn pane_cells(app: &App, pane: u64) -> (u16, u16) {
    let panes = app.request("panes");
    let p = panes["panes"]
        .as_array()
        .and_then(|ps| ps.iter().find(|p| p["id"] == pane))
        .expect("the pane")
        .clone();
    (
        p["cols"].as_u64().expect("cols") as u16,
        p["rows"].as_u64().expect("rows") as u16,
    )
}

#[test]
fn s2_cat_100mb_within_1_5x_of_ghostty() {
    let probe = screen_probe(&tmp_dir());
    require(&permissions(&probe), false);
    let env = env("perf-cat");
    let file = env.root.join("100mb.txt");
    write_text_file(&file, 100 << 20);

    let mut ours = Vec::new();
    let mut cells = (0, 0);
    {
        let app = App::start(&env, "a", &[]);
        app.request(&format!("window-size {} {}", WINDOW.0, WINDOW.1));
        for n in 0..RUNS {
            let run = cat_run(&env.root, &format!("auk{n}"), &file);
            let pane = env.new_pane(&["--", "/bin/sh", &run.script.to_string_lossy()]);
            wait_for("the app to show the pane", || {
                (app.send(&format!("focus {pane}"))["ok"] == true).then_some(())
            });
            app.wait_primary(pane);
            wait_for("the script to start", || run.ready.exists().then_some(()));
            cells = pane_cells(&app, pane);
            app.request("activate");
            std::thread::sleep(Duration::from_millis(500));
            ours.push(probe_marker(&probe, app.pid as i32, &run));
            env.daemon(&format!("close {pane}"));
        }
        eprintln!("Auk: cat 100 MB to the marker on screen: {ours:.0?} ms");
        app.quit();
    }

    let mut theirs = Vec::new();
    for n in 0..RUNS {
        let run = cat_run(&env.root, &format!("ghostty{n}"), &file);
        let ghostty = Ghostty::run(&run.script, &run.ready, cells.0, cells.1)
            .unwrap_or_else(|e| panic!("{e}"));
        std::thread::sleep(Duration::from_millis(800));
        theirs.push(probe_marker(&probe, ghostty.pid, &run));
    }
    eprintln!("Ghostty: cat 100 MB to the marker on screen: {theirs:.0?} ms");
    let (a, g) = (median(&ours), median(&theirs));
    eprintln!(
        "cat 100 MB, median: Auk {a:.0} ms, Ghostty {g:.0} ms ({:.2}×)",
        a / g
    );
    assert!(
        a <= GHOSTTY_FACTOR * g,
        "cat 100 MB: Auk {a:.0} ms is over {GHOSTTY_FACTOR}× Ghostty's {g:.0} ms"
    );
}

/// The keystroke script: the tty echoes each key as it's typed. It writes its tty's path to
/// `tty-<n>` and then creates `ready-<n>`. Returns the script, the ready file and the tty file.
fn echo_script(dir: &Path, n: &str) -> (PathBuf, PathBuf, PathBuf) {
    let ready = dir.join(format!("ready-{n}"));
    let tty = dir.join(format!("tty-{n}"));
    let body = format!(
        "stty -icanon\nclear\ntty > '{}'\n: > '{}'\nexec cat > /dev/null",
        tty.display(),
        ready.display()
    );
    (script(dir, &format!("echo-{n}.sh"), &body), ready, tty)
}

/// Runs the probe's latency mode on `pid` with `extra` arguments; the latencies in ms.
fn latencies(probe: &Path, pid: i32, extra: &[&str]) -> Vec<f64> {
    let pid = pid.to_string();
    let mut args = vec!["latency", "--pid", &pid, "--trials", "40"];
    args.extend(extra);
    let v = run_probe(probe, &args).unwrap_or_else(|e| panic!("{e}"));
    v["latencies_ms"]
        .as_array()
        .expect("latencies")
        .iter()
        .filter_map(Value::as_f64)
        .collect()
}

fn report(what: &str, ms: &[f64]) {
    eprintln!(
        "{what}: median {:.1} ms, p95 {:.1} ms",
        median(ms),
        percentile(ms, 0.95)
    );
}

#[test]
fn s2_keystroke_latency_within_1_5x_of_ghostty() {
    let probe = screen_probe(&tmp_dir());
    let perms = permissions(&probe);
    require(&perms, false);
    let env = env("perf-keys");
    let (script, ready, tty) = echo_script(&env.root, "auk");
    let (app, pane) = app_with_pane(&env, &["--", "/bin/sh", &script.to_string_lossy()]);
    wait_for("the script to start", || ready.exists().then_some(()));
    let cells = pane_cells(&app, pane);
    let auk_tty = std::fs::read_to_string(&tty).expect("the pane's tty");
    app.request("activate");
    std::thread::sleep(Duration::from_millis(500));

    // What needs no Accessibility, timed on screen the same way: keys typed through Auk's
    // endpoint (an NSEvent sent to its window), and output written to each terminal's tty.
    let control = app.control.to_string_lossy().into_owned();
    report(
        "Auk, typed through the endpoint to the screen",
        &latencies(&probe, app.pid as i32, &["--type-via", &control]),
    );
    let auk_out = latencies(&probe, app.pid as i32, &["--write-tty", auk_tty.trim()]);
    report("Auk, tty output to the screen", &auk_out);
    let (script, ready, tty) = echo_script(&env.root, "ghostty");
    let ghostty = Ghostty::run(&script, &ready, cells.0, cells.1).unwrap_or_else(|e| panic!("{e}"));
    let ghostty_tty = std::fs::read_to_string(&tty).expect("Ghostty's tty");
    std::thread::sleep(Duration::from_millis(800));
    let ghostty_out = latencies(&probe, ghostty.pid, &["--write-tty", ghostty_tty.trim()]);
    report("Ghostty, tty output to the screen", &ghostty_out);
    eprintln!(
        "tty output to screen, Auk/Ghostty: {:.2}×",
        median(&auk_out) / median(&ghostty_out)
    );

    // Keystrokes: posted as a keyboard's, which needs Accessibility.
    require(&perms, true);
    let theirs = latencies(&probe, ghostty.pid, &[]);
    drop(ghostty);
    app.request("activate");
    std::thread::sleep(Duration::from_millis(500));
    let ours = latencies(&probe, app.pid as i32, &[]);
    let (a, g) = (median(&ours), median(&theirs));
    eprintln!(
        "keystroke to screen: Auk median {a:.1} ms (p95 {:.1}), Ghostty median {g:.1} ms (p95 {:.1}), {:.2}×",
        percentile(&ours, 0.95),
        percentile(&theirs, 0.95),
        a / g
    );
    assert!(
        a <= GHOSTTY_FACTOR * g,
        "keystroke to screen: Auk {a:.1} ms is over {GHOSTTY_FACTOR}× Ghostty's {g:.1} ms"
    );
}

// Parser throughput (moved from auk-term's tests: it runs here, alone).

fn throughput_stream(size: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(size + 4096);
    let mut i = 0u64;
    while out.len() < size {
        i += 1;
        match i % 10 {
            // `ls --color`-like output.
            0 | 1 => out.extend_from_slice(
                format!("\x1b[0m\x1b[01;34mdirectory-{i}\x1b[0m  \x1b[01;32mexecutable\x1b[0m  plain-file-{i}.txt  notes.md\r\n").as_bytes(),
            ),
            // CJK and emoji.
            2 => out.extend_from_slice(format!("{i}: 中文字符的输出 — ünïcödé text ✓ 😀 mixed with ASCII words\r\n").as_bytes()),
            // A long line that wraps several times.
            3 => {
                for _ in 0..4 {
                    out.extend_from_slice(b"the quick brown fox jumps over the lazy dog; ");
                }
                out.extend_from_slice(b"\r\n");
            }
            // Plain log lines.
            _ => out.extend_from_slice(
                format!("2026-10-08T12:00:00Z INFO request id={i} path=/api/v1/items status=200 dur=1.3ms\r\n").as_bytes(),
            ),
        }
    }
    out
}

/// A 100 MB mixed stream (plain ASCII lines, colored output, CJK text, long wrapped lines)
/// through an 80×24 terminal with 10,000 rows of scrollback, read in 64 KiB chunks as a PTY
/// reader would. Release builds must reach 200 MB/s; debug builds run a smaller stream and only
/// report.
#[test]
fn parser_throughput_100mb() {
    use auk_term::{Config, Terminal};
    let size = if cfg!(debug_assertions) {
        4 << 20
    } else {
        100 << 20
    };
    let data = throughput_stream(size);
    let mut t = Terminal::new(80, 24, Config { scrollback: 10_000 });
    let start = Instant::now();
    for chunk in data.chunks(64 << 10) {
        let mut rest = chunk;
        while !rest.is_empty() {
            let (n, _) = t.advance(rest);
            rest = &rest[n..];
        }
    }
    let secs = start.elapsed().as_secs_f64();
    let mbps = data.len() as f64 / secs / 1e6;
    eprintln!(
        "throughput: {:.1} MB in {secs:.3} s = {mbps:.0} MB/s",
        data.len() as f64 / 1e6
    );
    assert_eq!(t.history_len(), 10_000);
    assert!(
        t.text(t.history_len()..t.history_len() + 24)
            .contains("status=200")
    );
    if !cfg!(debug_assertions) {
        assert!(mbps >= 200.0, "{mbps:.0} MB/s is below 200 MB/s");
    }
}
