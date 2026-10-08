//! The S1 input probe (spec §23): types keystrokes into an Auk window through the real AppKit
//! input path (`keyDown:`, the active input method, `NSTextInputClient`) and prints the bytes
//! each would send to the program, encoded by `auk_term::input`. With `--ghostty`, it posts the
//! same keystrokes to a Ghostty window running a byte recorder and compares.
//!
//! ```text
//! cargo run -p auk-app --example keyprobe -- [--kitty] [--option-as-alt left|right|both]
//!     [--ghostty] [STROKE...]
//! ```
//!
//! `--input-source ID` selects an input source for the run and restores the user's after, e.g.
//! `--input-source com.apple.inputmethod.Kotoeri.RomajiTyping.Japanese n i h o n space return`.
//!
//! Strokes are `mod+mod+key` (see `auk_platform_macos::synth::Stroke::parse`), e.g. `a`,
//! `shift+a`, `ctrl+c`, `opt+e e` (a dead key and its base), `ctrl+shift+up`, `f5`. Without
//! strokes it runs a default set covering plain keys, modifiers, Option and dead keys.
//!
//! The Ghostty comparison needs Ghostty installed and the Accessibility permission for the
//! terminal running this (to post events to another process). Set Ghostty's
//! `macos-option-as-alt` to match `--option-as-alt`. IME composition is checked the same way
//! with an input source selected (the manual checklist covers a real IME session).

use std::io::Write as _;
use std::time::Duration;

use auk_platform_macos::synth::{Stroke, input_source, post_to_pid, settle};
use auk_platform_macos::{
    Event, OptionAsAlt, Window, WindowAttributes, WindowHandler, run, run_on_main,
};
use auk_term::Modes;
use auk_term::input::encode_key;

const DEFAULT_STROKES: &[&str] = &[
    "a",
    "shift+a",
    "1",
    "shift+1",
    "space",
    "return",
    "tab",
    "shift+tab",
    "backspace",
    "esc",
    "up",
    "ctrl+up",
    "shift+left",
    "home",
    "end",
    "pageup",
    "delete",
    "f1",
    "f5",
    "f12",
    "ctrl+a",
    "ctrl+c",
    "ctrl+[",
    "ctrl+space",
    "ctrl+shift+a",
    "opt+a",
    "opt+f",
    "opt+b",
    "opt+e",
    "e",
    "opt+u",
    "o",
    "opt+n",
    "n",
    "kp1",
    "kpenter",
    "/",
    "\\",
    "`",
    "'",
];

struct Options {
    kitty: bool,
    option_as_alt: OptionAsAlt,
    ghostty: bool,
    input_source: Option<String>,
    strokes: Vec<String>,
}

fn options() -> Options {
    let mut o = Options {
        kitty: false,
        option_as_alt: OptionAsAlt::None,
        ghostty: false,
        input_source: None,
        strokes: Vec::new(),
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--kitty" => o.kitty = true,
            "--ghostty" => o.ghostty = true,
            "--input-source" => o.input_source = args.next(),
            "--option-as-alt" => {
                o.option_as_alt = match args.next().as_deref() {
                    Some("left") => OptionAsAlt::Left,
                    Some("right") => OptionAsAlt::Right,
                    Some("both") => OptionAsAlt::Both,
                    _ => OptionAsAlt::None,
                }
            }
            _ => o.strokes.push(arg),
        }
    }
    if o.strokes.is_empty() {
        o.strokes = DEFAULT_STROKES.iter().map(|s| s.to_string()).collect();
    }
    o
}

/// Records the bytes the window's key events encode to.
struct Recorder {
    modes: Modes,
    bytes: Vec<u8>,
    events: Vec<String>,
}

impl WindowHandler for Recorder {
    fn event(&mut self, window: &Window, event: Event) {
        match event {
            Event::Key(k) => {
                self.bytes.extend(encode_key(&k.event, &self.modes));
                self.events.push(format!("{:?}", k.event));
            }
            Event::Text(t) => {
                self.bytes.extend(t.as_bytes());
                self.events.push(format!("Text({t:?})"));
            }
            Event::Preedit(p) => self
                .events
                .push(format!("Preedit({:?})", p.map(|p| p.text))),
            Event::CloseRequested => window.close(),
            _ => {}
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn main() {
    let opts = options();
    let strokes: Vec<(String, Stroke)> = opts
        .strokes
        .iter()
        .map(|s| {
            (
                s.clone(),
                Stroke::parse(s).unwrap_or_else(|| panic!("bad stroke {s:?}")),
            )
        })
        .collect();
    let reference = if opts.ghostty {
        Some(ghostty::record(&strokes, opts.kitty))
    } else {
        None
    };
    let restore = opts.input_source.as_ref().map(|id| {
        let previous = input_source::current();
        assert!(
            input_source::select(id),
            "input source {id} is not installed"
        );
        previous
    });
    let modes = Modes {
        kitty_flags: if opts.kitty { 1 } else { 0 },
        ..Modes::default()
    };
    run(move |app| {
        SharedRecorder::install(Recorder {
            modes,
            bytes: Vec::new(),
            events: Vec::new(),
        });
        let attrs = WindowAttributes {
            title: "keyprobe".into(),
            option_as_alt: opts.option_as_alt,
            width: 400.0,
            height: 200.0,
        };
        let window = app.create_window(attrs, Box::new(SharedRecorder));
        // Type once the window has had a moment to become key, so the input method is live.
        PROBE.with(|p| *p.borrow_mut() = Some((window, strokes, reference, restore.flatten())));
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(500));
            run_on_main(probe);
        });
    });
}

type Probe = (
    Window,
    Vec<(String, Stroke)>,
    Option<Vec<Vec<u8>>>,
    Option<String>,
);

thread_local! {
    static PROBE: std::cell::RefCell<Option<Probe>> = const { std::cell::RefCell::new(None) };
}

fn probe() {
    let Some((window, strokes, reference, restore)) = PROBE.with(|p| p.borrow_mut().take()) else {
        return;
    };
    SharedRecorder::take();
    if !window.is_focused() {
        eprintln!(
            "keyprobe: the window is not key (the app is in the background), so the input method is inactive; keys fall back to their layout text"
        );
    }
    let mut mismatches = 0;
    for (i, (name, stroke)) in strokes.iter().enumerate() {
        window.type_stroke(*stroke);
        settle(Duration::from_millis(60));
        let (bytes, events) = SharedRecorder::take();
        let ours = hex(&bytes);
        match &reference {
            Some(theirs) => {
                let theirs = hex(&theirs[i]);
                let same = theirs == ours;
                mismatches += usize::from(!same);
                println!(
                    "{i:3} {name:16} auk [{ours}] ghostty [{theirs}] {}",
                    if same { "ok" } else { "DIFF" }
                );
            }
            None => println!("{i:3} {name:16} [{ours}] {}", events.join(" ")),
        }
    }
    if let Some(previous) = restore {
        input_source::select(&previous);
    }
    let _ = std::io::stdout().flush();
    std::process::exit(if mismatches > 0 { 1 } else { 0 });
}

/// The window's handler: forwards to the thread's recorder, which the probe reads between
/// strokes.
struct SharedRecorder;

thread_local! {
    static RECORDER: std::cell::RefCell<Option<Recorder>> = const { std::cell::RefCell::new(None) };
}

impl SharedRecorder {
    fn install(recorder: Recorder) {
        RECORDER.with(|r| *r.borrow_mut() = Some(recorder));
    }

    fn take() -> (Vec<u8>, Vec<String>) {
        RECORDER.with(|r| {
            r.borrow_mut().as_mut().map_or_else(Default::default, |r| {
                (std::mem::take(&mut r.bytes), std::mem::take(&mut r.events))
            })
        })
    }
}

impl WindowHandler for SharedRecorder {
    fn event(&mut self, window: &Window, event: Event) {
        RECORDER.with(|r| {
            if let Some(r) = r.borrow_mut().as_mut() {
                r.event(window, event);
            }
        });
    }
}

/// The reference side: Ghostty running `cat` on a raw tty into a file.
mod ghostty {
    use super::*;
    use std::process::Command;

    pub fn record(strokes: &[(String, Stroke)], kitty: bool) -> Vec<Vec<u8>> {
        let dir = std::env::temp_dir().join(format!("auk-keyprobe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let out = dir.join("bytes");
        let script = dir.join("record.sh");
        let push = if kitty { "printf '\\033[>1u'; " } else { "" };
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n{push}stty raw -echo; exec cat > '{}'\n",
                out.display()
            ),
        )
        .expect("write recorder");
        Command::new("chmod")
            .arg("+x")
            .arg(&script)
            .status()
            .expect("chmod");
        let before = pids();
        Command::new("open")
            .args(["-na", "Ghostty", "--args", "-e"])
            .arg(&script)
            .status()
            .expect("Ghostty is installed (brew install --cask ghostty)");
        std::thread::sleep(Duration::from_secs(3));
        let pid = pids()
            .into_iter()
            .find(|p| !before.contains(p))
            .expect("a new Ghostty process");
        let mut recorded = Vec::new();
        let mut seen = 0;
        for (_, stroke) in strokes {
            post_to_pid(pid, *stroke);
            std::thread::sleep(Duration::from_millis(150));
            let all = std::fs::read(&out).unwrap_or_default();
            recorded.push(all[seen.min(all.len())..].to_vec());
            seen = all.len();
        }
        let _ = Command::new("kill").arg(pid.to_string()).status();
        recorded
    }

    fn pids() -> Vec<i32> {
        let out = Command::new("pgrep")
            .arg("-x")
            .arg("ghostty")
            .output()
            .expect("pgrep");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.trim().parse().ok())
            .collect()
    }
}
