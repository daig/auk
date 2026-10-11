//! Terminal: the reference grid, live programs in a daemon pane, and the terminal protocols.
//!
//! # Reference grid
//!
//! The streams are recordings of real programs in a PTY (`fixtures/terminal/*.vt.jsonl`, made
//! by `tools/vt_record.py`; `fixtures/terminal/record.sh` re-records them, and each
//! `.meta.json` keeps the command, size, environment and program version): vim, less, htop,
//! `git log --color` paged by less, tmux with splits, wide characters, emoji and a shell resized
//! through many sizes. Fuzzed streams are built from them: cut at random bytes, resized at random
//! points, and spliced from random pieces of every recording. Each leaves the same grid in Auk as
//! in `alacritty_terminal` (cells, colors, attributes, links, wide-character kinds, wrap flags,
//! scrollback, cursor, reflow), compared after every read and resize.
//!
//! Differences the comparison allows, with reasons (numbers as in the list at the top of
//! `crates/term/tests/differential.rs`):
//! - **7. VS16** (U+FE0F) widens an emoji-presentation character to two cells in Auk, as kitty,
//!   WezTerm and Ghostty do; alacritty keeps it narrow. From the first cell holding U+FE0F to the
//!   end of its row (and the rows a wrapped line continues on), cells aren't compared; the vim
//!   and emoji recordings meet this. The fuzzed streams strip U+FE0F: cut at other points or
//!   resized, a widened cell pushes text into rows that hold none, where vim's explicit
//!   positioning then draws over it differently.
//! - **6, 8, 11, 12** (broken wide characters, tab cells, row occupancy after reflow, stale wrap
//!   flags): the harness applies Auk's rule to the reference after every step
//!   (`auk_conformance::vt::Reference`).
//! - **1–5, 9, and 11's RIS** (DCH past the row's end, ED 1 on row 1, vertical moves in a scroll
//!   region, absolute columns in origin mode, CBT with a wrap pending, modes alacritty lacks): no
//!   recording meets them, which the as-recorded tests prove by passing with no filter. Spliced
//!   streams can, so the spliced fuzz drops those sequences (`vt::allowed`).
//! - **10. Anchor rows and TSP** have no counterpart in alacritty; no recording holds TSP, and
//!   the `tsp` suite covers them.
//! - A bug in the reference, not a difference: `vte` 0.15 drops the ASCII byte after a UTF-8
//!   character split across two reads when a non-ASCII character follows, so the reference is
//!   fed split characters whole (`vt::Reference::feed`).
//!
//! Alternate-screen sessions are resized at most once in the fuzz (difference 12: alacritty
//! revives a cut wrap flag on its inactive main grid, which the harness can't repair).
//!
//! # Live, and protocols
//!
//! Through the real daemon: the recordings replayed byte for byte in a pane match the reference
//! as a client's replica sees them; vim edits and saves a file; tmux splits; a 100 MB `cat` ends
//! with the file's tail on screen; and OSC 8, kitty keyboard flag 1, bracketed paste, DA1 and
//! CPR, with `tools/pty_probe.py` as the program and a headless window encoding the keys.

use std::fs;
use std::io::Write as _;
use std::path::Path;
use std::time::{Duration, Instant};

use auk_conformance::harness::{Headless, TestEnv, quote, wait_for};
use auk_conformance::probe::{Input, Probe, require_program, split_input};
use auk_conformance::viewer::Viewer;
use auk_conformance::vt::{self, Pair, Recording, Reference, Rng, Step};

fn env(name: &str) -> TestEnv {
    TestEnv::new(env!("CARGO_BIN_EXE_auk"), name)
}

/// The daemon's default scrollback (`auk_term::Config::default`), which the reference copies.
const SCROLLBACK: usize = 10_000;

// Reference grid: recordings as recorded.

/// Replays a recording read by read and compares after each step. Returns the rows left out
/// for difference 7 (VS16), summed over the comparisons.
fn replay(rec: &Recording) -> usize {
    let mut pair = Pair::new(rec.cols, rec.rows, SCROLLBACK);
    pair.compare().expect("blank grids match");
    let mut skipped = 0;
    for (i, step) in rec.steps.iter().enumerate() {
        match step {
            Step::Output(b) => pair.feed(b),
            Step::Resize(c, r) => pair.resize(*c, *r),
        }
        skipped += pair.compare().unwrap_or_else(|e| {
            panic!("{}: step {i} ({step:?}): {e}", rec.name, step = kind(step))
        });
    }
    skipped
}

fn kind(step: &Step) -> String {
    match step {
        Step::Output(b) => format!("{} bytes", b.len()),
        Step::Resize(c, r) => format!("resize {c}x{r}"),
    }
}

/// Loads a recording and checks it is what its metadata says: the program, real output, and
/// resizes in the stream.
fn recording(name: &str, program: &str) -> Recording {
    let rec = vt::load(name);
    let argv = rec.meta["argv"].to_string();
    assert!(
        argv.contains(program),
        "{name}: recorded {argv}, not {program}"
    );
    let bytes = rec.bytes().len();
    assert_eq!(
        rec.meta["counts"]["out_bytes"].as_u64(),
        Some(bytes as u64),
        "{name}: the stream doesn't match its metadata"
    );
    assert!(
        rec.steps.iter().any(|s| matches!(s, Step::Resize(..))),
        "{name}: no resize recorded"
    );
    rec
}

#[test]
fn reference_grid_vim() {
    let rec = recording("vim", "vim");
    // The alternate screen, scroll regions, truecolor syntax colors, splits and two resizes.
    let bytes = rec.bytes();
    for needle in [&b"\x1b[?1049h"[..], b"\x1b[38;2;", b"r\x1b["] {
        assert!(contains(&bytes, needle), "vim recording lacks {needle:?}");
    }
    let skipped = replay(&rec);
    // sample.rs has `1️⃣` and `❤️` on one line, which vim (thinking them one cell wide) draws
    // over in Auk (difference 7).
    assert!(skipped > 0, "the VS16 line never showed");
}

#[test]
fn reference_grid_less() {
    let rec = recording("less", "less");
    assert_eq!(replay(&rec), 0);
}

#[test]
fn reference_grid_htop() {
    let rec = recording("htop", "htop");
    assert_eq!(replay(&rec), 0);
}

#[test]
fn reference_grid_git_log_color() {
    let rec = recording("gitlog", "gitlog.sh");
    assert!(
        contains(&rec.bytes(), b"\x1b[33mcommit"),
        "no colored git log"
    );
    assert_eq!(replay(&rec), 0);
}

#[test]
fn reference_grid_tmux() {
    let rec = recording("tmux", "tmux");
    assert_eq!(replay(&rec), 0);
}

#[test]
fn reference_grid_wide_characters() {
    let rec = recording("wide", "zsh");
    assert_eq!(replay(&rec), 0);
}

#[test]
fn reference_grid_emoji() {
    let rec = recording("emoji", "zsh");
    assert!(replay(&rec) > 0, "the VS16 rows never showed");
}

#[test]
fn reference_grid_resizes_and_reflow() {
    let rec = recording("resize", "zsh");
    let resizes = rec
        .steps
        .iter()
        .filter(|s| matches!(s, Step::Resize(..)))
        .count();
    assert!(resizes >= 6, "only {resizes} resizes");
    assert_eq!(replay(&rec), 0);
}

// Reference grid: fuzzed streams.

/// Runs steps from a fuzzer: sequences in the intended differences are dropped (`vt::allowed`),
/// and an alternate-screen session is resized at most once (difference 12).
fn run_fuzzed(what: &str, cols: u16, rows: u16, steps: &[Step], filter: bool) {
    let mut pair = Pair::new(cols, rows, SCROLLBACK);
    let mut resized_in_alt = false;
    for (i, step) in steps.iter().enumerate() {
        match step {
            Step::Output(b) if filter => {
                for token in vt::tokens(b) {
                    if vt::allowed(&pair.auk, token) {
                        pair.feed(token);
                    }
                }
            }
            Step::Output(b) => pair.feed(b),
            Step::Resize(c, r) => {
                if pair.auk.modes().alt_screen {
                    if resized_in_alt {
                        continue;
                    }
                    resized_in_alt = true;
                }
                pair.resize(*c, *r);
            }
        }
        if !pair.auk.modes().alt_screen {
            resized_in_alt = false;
        }
        if let Err(e) = pair.compare() {
            panic!("{what}: step {i} ({}): {e}", kind(step));
        }
    }
}

#[test]
fn reference_grid_holds_at_any_read_boundary() {
    // The same bytes cut anywhere, mid-sequence and mid-character included.
    for name in vt::RECORDINGS {
        let rec = vt::load(name);
        for seed in 1..=6u64 {
            let mut rng = Rng::new(seed);
            let mut steps = Vec::new();
            for step in &rec.steps {
                match step {
                    Step::Output(b) => {
                        let b = strip_vs16(b);
                        let mut at = 0;
                        while at < b.len() {
                            let n = rng.range(1, 97) as usize;
                            let end = (at + n).min(b.len());
                            steps.push(Step::Output(b[at..end].to_vec()));
                            at = end;
                        }
                    }
                    resize => steps.push(resize.clone()),
                }
            }
            run_fuzzed(
                &format!("{name} seed {seed}"),
                rec.cols,
                rec.rows,
                &steps,
                false,
            );
        }
    }
}

#[test]
fn reference_grid_holds_under_random_resizes() {
    // Each recording with extra resizes at random points, to sizes from 2x1 up.
    for name in vt::RECORDINGS {
        let rec = vt::load(name);
        let bytes = strip_vs16(&rec.bytes());
        for seed in 1..=8u64 {
            let mut rng = Rng::new(seed);
            let mut steps = Vec::new();
            let mut chunk = Vec::new();
            for token in vt::tokens(&bytes) {
                chunk.extend_from_slice(token);
                if rng.below(150) == 0 {
                    steps.push(Step::Output(std::mem::take(&mut chunk)));
                    steps.push(Step::Resize(
                        rng.range(2, 160) as u16,
                        rng.range(1, 60) as u16,
                    ));
                }
            }
            steps.push(Step::Output(chunk));
            run_fuzzed(
                &format!("{name} seed {seed}"),
                rec.cols,
                rec.rows,
                &steps,
                true,
            );
        }
    }
}

#[test]
fn reference_grid_holds_for_spliced_recordings() {
    // Random pieces of every recording, cut at random bytes, joined, with random resizes.
    let sources: Vec<Vec<u8>> = vt::RECORDINGS
        .iter()
        .map(|name| strip_vs16(&vt::load(name).bytes()))
        .collect();
    for seed in 1..=400u64 {
        let mut rng = Rng::new(seed);
        let mut stream = Vec::new();
        let mut resizes = Vec::new();
        for _ in 0..rng.range(3, 20) {
            if rng.below(4) == 0 {
                resizes.push((
                    stream.len(),
                    rng.range(2, 140) as u16,
                    rng.range(1, 50) as u16,
                ));
                continue;
            }
            let src = &sources[rng.below(sources.len() as u64) as usize];
            let start = rng.below(src.len() as u64) as usize;
            let end = (start + rng.range(1, 3000) as usize).min(src.len());
            stream.extend_from_slice(&src[start..end]);
        }
        // Cut into reads at token boundaries of the whole stream, so a sequence split by a join
        // is judged as the parsers see it.
        let mut steps = Vec::new();
        let mut chunk = Vec::new();
        let mut at = 0;
        let mut next = 0;
        for token in vt::tokens(&stream) {
            chunk.extend_from_slice(token);
            at += token.len();
            while next < resizes.len() && resizes[next].0 <= at {
                steps.push(Step::Output(std::mem::take(&mut chunk)));
                steps.push(Step::Resize(resizes[next].1, resizes[next].2));
                next += 1;
            }
            if rng.below(8) == 0 {
                steps.push(Step::Output(std::mem::take(&mut chunk)));
            }
        }
        steps.push(Step::Output(chunk));
        run_fuzzed(&format!("seed {seed}"), 80, 24, &steps, true);
    }
}

fn strip_vs16(bytes: &[u8]) -> Vec<u8> {
    let vs16 = "\u{fe0f}".as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(vs16) {
            i += vs16.len();
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

// Through the daemon.

#[test]
fn recordings_replayed_in_a_daemon_pane_match_the_reference() {
    let env = env("replay");
    env.start_daemon();
    for name in vt::RECORDINGS {
        let rec = vt::load(name);
        let size = format!("{}x{}", rec.cols, rec.rows);
        let probe = Probe::start(&env, &["--size", &size]);
        // Output processing off, so the recorded bytes reach the terminal unchanged.
        probe.tty("binary");
        let pane = probe.pane;
        let viewer = Viewer::attach(&env, "main");
        let mut reference = Reference::new(rec.cols, rec.rows, SCROLLBACK);
        for (n, ((cols, rows), bytes)) in rec.segments().into_iter().enumerate() {
            if n > 0 {
                env.daemon(&format!("resize {pane} {cols} {rows}"));
                reference.resize(cols, rows);
            }
            for piece in bytes.chunks(32 * 1024) {
                probe.write(piece);
            }
            reference.feed(&bytes);
            // The daemon reads the PTY on its own time: wait until the client's replica, at the
            // daemon's stream position, matches the reference (or report the last difference).
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                viewer.converge(&env, pane);
                let result = viewer
                    .with(pane, |r| vt::compare(r.terminal(), &reference))
                    .expect("the viewer has the pane");
                match result {
                    Ok(_) => break,
                    Err(e) if Instant::now() > deadline => {
                        panic!("{name}, segment {n} through the daemon: {e}")
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(20)),
                }
            }
        }
        assert!(viewer.errors().is_empty(), "{name}: {:?}", viewer.errors());
        probe.exit(0);
    }
}

/// A shell pane in the test's home, at its first prompt, with a headless window attached.
fn shell(env: &TestEnv, size: &str) -> (u64, Headless) {
    let window = env.start_headless("w", &["--size", size]);
    let pane = env.new_pane(&["--size", size]);
    env.wait(pane, "prompt");
    window.request(&format!("focus {pane}"));
    wait_for("the window's replica", || window.hash(pane));
    (pane, window)
}

fn screen(env: &TestEnv, pane: u64) -> String {
    env.daemon(&format!("capture {pane}"))["text"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

fn wait_screen(env: &TestEnv, pane: u64, what: &str, f: impl Fn(&str) -> bool) -> String {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let s = screen(env, pane);
        if f(&s) {
            return s;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}; the screen:\n{s}"
        );
        std::thread::sleep(Duration::from_millis(30));
    }
}

/// Types into the pane through the window, which encodes keys from its replica.
fn keys(window: &Headless, pane: u64, keys: &str) {
    let words: Vec<String> = keys.split(' ').map(quote).collect();
    window.request(&format!("send {pane} keys {}", words.join(" ")));
}

fn text(window: &Headless, pane: u64, text: &str) {
    window.request(&format!("send {pane} text {}", quote(text)));
}

#[test]
fn vim_edits_and_saves_a_file_in_a_pane() {
    let env = env("vim");
    env.start_daemon();
    require_program("vim");
    let file = env.home.join("notes.txt");
    fs::write(&file, "one\ntwo\nthree\n").unwrap();
    let (pane, w) = shell(&env, "80x24");
    env.daemon(&format!("run {pane} 'vim -u NONE -N -n -i NONE notes.txt'"));
    let s = wait_screen(&env, pane, "vim to show the file", |s| {
        s.contains("\"notes.txt\" 3L") && s.lines().filter(|l| *l == "~").count() > 10
    });
    assert!(s.starts_with("one\ntwo\nthree\n"), "{s}");
    assert!(env.pane_info(pane)["title"].is_string());

    // Delete line two, append to line one, add a line with wide text, save and quit.
    keys(&w, pane, "j d d k A");
    text(&w, pane, " edited");
    wait_screen(&env, pane, "insert mode", |s| {
        s.contains("one edited") && s.contains("-- INSERT --")
    });
    keys(&w, pane, "Esc");
    keys(&w, pane, "G o");
    text(&w, pane, "四 wide 🦀");
    keys(&w, pane, "Esc");
    wait_screen(&env, pane, "the new line", |s| {
        s.starts_with("one edited\nthree\n四 wide 🦀\n") && !s.contains("-- INSERT --")
    });
    text(&w, pane, ":wq");
    keys(&w, pane, "Enter");
    env.wait(pane, "prompt");
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        "one edited\nthree\n四 wide 🦀\n"
    );
    // vim left the alternate screen: the shell's screen is back, without vim's tildes.
    let s = wait_screen(&env, pane, "the shell's screen", |s| {
        s.contains("vim -u NONE") && !s.contains("\n~\n")
    });
    assert!(!s.contains("-- INSERT --"), "{s}");
    env.run(pane, "cat notes.txt");
    wait_screen(&env, pane, "the saved file", |s| {
        s.contains("one edited\nthree\n四 wide 🦀\n")
    });
}

#[test]
fn tmux_splits_in_a_pane() {
    let env = env("tmux");
    env.start_daemon();
    require_program("tmux");
    let (pane, w) = shell(&env, "100x30");
    // Its own server socket (-L) in the test's directory, and no config.
    let tmpdir = env.root.join("tmux");
    fs::create_dir_all(&tmpdir).unwrap();
    let tmux = format!(
        "TMUX_TMPDIR={} tmux -L auk -f /dev/null",
        quote(&tmpdir.to_string_lossy())
    );
    env.daemon(&format!(
        "run {pane} {}",
        quote(&format!("{tmux} new-session"))
    ));
    let status = |s: &str| s.lines().last().is_some_and(|l| l.starts_with("[0] 0:"));
    wait_screen(&env, pane, "tmux's status line", |s| {
        status(s) && s.lines().any(|l| l.ends_with("% ") || l.ends_with('%'))
    });
    text(&w, pane, "echo left-$((40+2))");
    keys(&w, pane, "Enter");
    wait_screen(&env, pane, "output in tmux", |s| s.contains("\nleft-42"));

    // C-b % splits left and right.
    keys(&w, pane, "C-b %");
    wait_screen(&env, pane, "a vertical split", |s| {
        s.lines().filter(|l| l.contains('│')).count() >= 25
    });
    text(&w, pane, "echo right-$((40+3))");
    keys(&w, pane, "Enter");
    let s = wait_screen(&env, pane, "output in the right pane", |s| {
        s.contains("right-43")
    });
    let col = |s: &str, needle: &str| {
        s.lines()
            .find_map(|l| l.find(needle).map(|b| l[..b].chars().count()))
            .unwrap_or_else(|| panic!("{needle} not on screen:\n{s}"))
    };
    let divider = col(&s, "│");
    assert!((45..=55).contains(&divider), "divider at {divider}:\n{s}");
    assert!(col(&s, "left-42") < divider, "{s}");
    assert!(col(&s, "right-43") > divider, "{s}");

    // C-b " splits the right pane top and bottom.
    keys(&w, pane, "C-b \"");
    text(&w, pane, "echo bottom-$((40+4))");
    keys(&w, pane, "Enter");
    let s = wait_screen(&env, pane, "output in the bottom pane", |s| {
        s.contains("bottom-44")
    });
    let row = |needle: &str| s.lines().position(|l| l.contains(needle)).unwrap();
    let rule = s
        .lines()
        .position(|l| l.chars().skip(divider + 1).take(20).all(|c| c == '─'))
        .unwrap_or_else(|| panic!("no horizontal rule:\n{s}"));
    assert!(row("right-43") < rule && rule < row("bottom-44"), "{s}");
    assert!(col(&s, "bottom-44") > divider, "{s}");
    // Both left and right are still on screen.
    assert!(s.contains("left-42"), "{s}");

    // The window's replica agrees with the daemon's.
    wait_for("the window to match the daemon", || {
        (w.hash(pane) == Some(env.daemon_hash(pane))).then_some(())
    });

    text(&w, pane, &format!("{tmux} kill-server"));
    keys(&w, pane, "Enter");
    env.wait(pane, "prompt");
    wait_screen(&env, pane, "the shell after tmux", |s| !status(s));
}

#[test]
fn cat_of_100_mb_ends_with_the_files_tail_on_screen() {
    let env = env("cat100");
    env.start_daemon();
    let path = env.root.join("big.txt");
    let lines = write_big_file(&path, 100 * 1024 * 1024);
    let (pane, _w) = shell(&env, "120x40");
    let start = Instant::now();
    env.daemon(&format!("run {pane} 'cat ../big.txt'"));
    let done = env.daemon(&format!("wait {pane} --until prompt --timeout 240"));
    let took = start.elapsed();
    assert!(done.get("timeout").is_none(), "{done}");
    let s = screen(&env, pane);
    let shown: Vec<&str> = s.lines().collect();
    // The last 39 rows: the file's last 38 lines, then the prompt.
    let tail = &lines[lines.len() - 38..];
    let at = shown
        .iter()
        .position(|l| *l == tail[0])
        .unwrap_or_else(|| panic!("the tail isn't on screen:\n{s}"));
    assert_eq!(&shown[at..at + tail.len()], tail, "{s}");
    assert_eq!(shown[at + tail.len()], "~ %", "the prompt follows:\n{s}");
    eprintln!(
        "cat of 100 MB through the daemon: {:.2} s ({:.0} MB/s)",
        took.as_secs_f64(),
        100.0 / took.as_secs_f64()
    );
}

/// Writes numbered lines of varied length up to `size` bytes; returns the last 100 lines.
fn write_big_file(path: &Path, size: usize) -> Vec<String> {
    let mut f = std::io::BufWriter::new(fs::File::create(path).unwrap());
    let mut written = 0;
    let mut n = 0u64;
    let mut last = std::collections::VecDeque::new();
    while written < size {
        n += 1;
        let line = format!(
            "{n:09} {}",
            "abcdefghijklmnopqrstuvwxyz0123456789".repeat(1 + (n % 3) as usize)
        );
        writeln!(f, "{line}").unwrap();
        written += line.len() + 1;
        last.push_back(line);
        if last.len() > 100 {
            last.pop_front();
        }
    }
    f.flush().unwrap();
    last.into()
}

// Protocols, with the probe as the program.

/// The probe in a pane in raw mode, plus a window to type through.
fn probe(env: &TestEnv) -> (Probe, Headless) {
    let window = env.start_headless("w", &["--size", "80x24"]);
    let probe = Probe::start(env, &["--size", "80x24"]);
    window.request(&format!("focus {}", probe.pane));
    wait_for("the window's replica", || window.hash(probe.pane));
    (probe, window)
}

/// What the program reads after the window sends `send` words, up to the end marker `until`.
fn typed(probe: &Probe, window: &Headless, send: &str, until: &[u8]) -> Vec<u8> {
    window.request(&format!("send {} {send}", probe.pane));
    probe.read_until(&format!("the bytes for `{send}`"), |b| contains(b, until))
}

#[test]
fn da1_is_answered_with_a_digit_through_the_pane() {
    let env = env("da1");
    env.start_daemon();
    let (probe, _w) = probe(&env);
    probe.write(b"\x1b[c");
    let input = probe.read_until("the DA1 answer", |b| {
        split_input(b).iter().any(|i| matches!(i, Input::Da1(_)))
    });
    let Some(Input::Da1(answer)) = split_input(&input)
        .into_iter()
        .find(|i| matches!(i, Input::Da1(_)))
    else {
        unreachable!()
    };
    // CSI ? Ps ; … c with at least one digit.
    let params = &answer[3..answer.len() - 1];
    assert!(params.iter().any(u8::is_ascii_digit), "{answer:?}");
    assert!(params.iter().all(|b| b.is_ascii_digit() || *b == b';'));
    // `CSI 0 c` is the same query.
    probe.write(b"\x1b[0c");
    let again = probe.read_until("the second DA1 answer", |b| b.ends_with(b"c"));
    assert_eq!(again, answer);
}

#[test]
fn cpr_reports_the_cursor_through_the_pane() {
    let env = env("cpr");
    env.start_daemon();
    let (probe, _w) = probe(&env);
    probe.write(b"\x1b[5;10H\x1b[6n");
    assert_eq!(
        probe.read_until("CPR", |b| b.ends_with(b"R")),
        b"\x1b[5;10R"
    );
    // After text, a wide character and a line feed.
    probe.write("abc中\r\n  \x1b[6n".as_bytes());
    assert_eq!(probe.read_until("CPR", |b| b.ends_with(b"R")), b"\x1b[6;3R");
    // Origin mode reports relative to the scroll region.
    probe.write(b"\x1b[3;20r\x1b[?6h\x1b[4;7H\x1b[6n");
    assert_eq!(probe.read_until("CPR", |b| b.ends_with(b"R")), b"\x1b[4;7R");
    probe.write(b"\x1b[?6l\x1b[r");
}

#[test]
fn bracketed_paste_through_the_pane() {
    let env = env("paste");
    env.start_daemon();
    let (probe, w) = probe(&env);
    // Off: the text as is.
    assert_eq!(
        typed(&probe, &w, "paste 'pasted words'", b"words"),
        b"pasted words"
    );
    // On: wrapped in the markers.
    probe.write(b"\x1b[?2004h");
    wait_for("the window to see the mode", || {
        (w.hash(probe.pane) == Some(env.daemon_hash(probe.pane))).then_some(())
    });
    assert_eq!(
        typed(&probe, &w, "paste 'pasted words'", b"\x1b[201~"),
        b"\x1b[200~pasted words\x1b[201~"
    );
    // A pasted end marker can't end the paste early.
    let got = typed(&probe, &w, "paste 'a\x1b[201~b'", b"b\x1b[201~");
    assert!(got.starts_with(b"\x1b[200~"), "{got:?}");
    let inner = &got[6..got.len() - 6];
    assert!(!contains(inner, b"\x1b[201~"), "{got:?}");
    // Off again.
    probe.write(b"\x1b[?2004l");
    wait_for("the window to see the mode", || {
        (w.hash(probe.pane) == Some(env.daemon_hash(probe.pane))).then_some(())
    });
    assert_eq!(typed(&probe, &w, "paste plain", b"plain"), b"plain");
}

#[test]
fn kitty_keyboard_flag_1_through_the_pane() {
    use tern_sdk::keys::Decoder;
    let decode = |b: &[u8]| {
        let mut d = Decoder::new();
        let mut k = d.feed(b);
        k.extend(d.flush());
        k
    };
    let env = env("kitty");
    env.start_daemon();
    let (probe, w) = probe(&env);
    let synced = || {
        wait_for("the window to see the mode", || {
            (w.hash(probe.pane) == Some(env.daemon_hash(probe.pane))).then_some(())
        })
    };
    // Legacy: Escape is a bare ESC.
    assert_eq!(typed(&probe, &w, "keys Esc", b"\x1b"), b"\x1b");

    // Push flag 1 (disambiguate), as every SDK does; the query reports it.
    probe.write(b"\x1b[>1u\x1b[?u");
    assert_eq!(
        probe.read_until("the flags", |b| b.ends_with(b"u")),
        b"\x1b[?1u"
    );
    synced();
    let esc = typed(&probe, &w, "keys Esc", b"u");
    assert_eq!(esc, b"\x1b[27u");
    let keys = decode(&esc);
    assert!(keys.len() == 1 && keys[0].is("escape"), "{keys:?}");
    let ctrl_c = typed(&probe, &w, "keys C-c", b"u");
    assert_eq!(ctrl_c, b"\x1b[99;5u");
    assert!(decode(&ctrl_c)[0].is("ctrl+c"), "{:?}", decode(&ctrl_c));
    let alt_x = typed(&probe, &w, "keys M-x", b"u");
    assert_eq!(alt_x, b"\x1b[120;3u");
    assert!(decode(&alt_x)[0].is("alt+x"), "{:?}", decode(&alt_x));
    // Plain text, Enter, Tab and Backspace stay as they were; no release events, no text
    // reports (only flag 1 was asked for).
    assert_eq!(typed(&probe, &w, "text hello", b"o"), b"hello");
    assert_eq!(typed(&probe, &w, "keys Enter", b"\r"), b"\r");
    assert_eq!(typed(&probe, &w, "keys Tab", b"\t"), b"\t");
    assert_eq!(typed(&probe, &w, "keys BSpace", b"\x7f"), b"\x7f");

    // Pop: legacy again.
    probe.write(b"\x1b[<u\x1b[?u");
    assert_eq!(
        probe.read_until("the flags", |b| b.ends_with(b"u")),
        b"\x1b[?0u"
    );
    synced();
    assert_eq!(typed(&probe, &w, "keys Esc", b"\x1b"), b"\x1b");
    assert_eq!(typed(&probe, &w, "keys C-c", b"\x03"), b"\x03");
}

#[test]
fn osc_8_links_reach_a_client_through_the_pane() {
    let env = env("osc8");
    env.start_daemon();
    let probe = Probe::start(&env, &["--size", "80x24"]);
    let pane = probe.pane;
    probe.write(
        b"before \x1b]8;;https://example.com/auk?q=1\x1b\\linked text\x1b]8;;\x1b\\ after\r\n\
          \x1b]8;id=x;file:///tmp/a%20b\x07two\x1b]8;;\x07\r\n",
    );
    wait_screen(&env, pane, "the links' text", |s| {
        s.starts_with("before linked text after\ntwo")
    });
    let viewer = Viewer::attach(&env, "main");
    viewer.converge(&env, pane);
    let cells = viewer
        .with(pane, |r| {
            let t = r.terminal();
            let base = t.history_len();
            (0..2)
                .flat_map(|row| {
                    let line = t.line(base + row).unwrap();
                    line.cells
                        .iter()
                        .map(|c| {
                            let link = t.links().get(c.link as usize).cloned().unwrap_or_default();
                            (c.text.clone(), link)
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        })
        .expect("the viewer has the pane");
    let link_of = |range: std::ops::Range<usize>| -> Vec<&str> {
        cells[range].iter().map(|(_, l)| l.as_str()).collect()
    };
    let text_of = |range: std::ops::Range<usize>| -> String {
        cells[range]
            .iter()
            .map(|(t, _)| if t.is_empty() { " " } else { t.as_str() })
            .collect()
    };
    // Row 0: "before " (0..7), the link (7..18), " after" (18..24). Row 1 starts at 80.
    assert_eq!(text_of(7..18), "linked text");
    assert!(
        link_of(7..18)
            .iter()
            .all(|l| *l == "https://example.com/auk?q=1")
    );
    assert!(
        link_of(0..7)
            .iter()
            .chain(&link_of(18..80))
            .all(|l| l.is_empty())
    );
    assert_eq!(text_of(80..83), "two");
    assert!(link_of(80..83).iter().all(|l| *l == "file:///tmp/a%20b"));
    assert!(link_of(83..160).iter().all(|l| l.is_empty()));
}
