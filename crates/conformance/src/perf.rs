//! The `perf` suite's inputs and measuring tools (GOAL.md `perf`, spec §18.1, §23 S2): an
//! omp-style transcript of any length built from omp's own entries, percentiles, and the
//! comparison with Ghostty (the 100 MB file and the screen probe that times keystrokes and
//! prompts on screen).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use auk_replica::{Item, Replica, Role, Settings};
use serde_json::{Map, Value, json};

use crate::probe::{repo_root, tsp};

/// The `p`-quantile (0–1) of `values`, nearest rank.
pub fn percentile(values: &[f64], p: f64) -> f64 {
    assert!(!values.is_empty(), "no values");
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    v[((v.len() - 1) as f64 * p).round() as usize]
}

/// The median of `values`.
pub fn median(values: &[f64]) -> f64 {
    percentile(values, 0.5)
}

/// The final transcript entries of the recorded omp session (`fixtures/tsp/omp-session.jsonl`):
/// the children of its inline surface's `main` region once the whole recording has played into a
/// replica, as wire JSON, spacers left out.
pub fn omp_entries() -> Vec<Value> {
    let path = crate::fixtures_dir().join("omp-session.jsonl");
    let records = crate::load(&path).unwrap_or_else(|e| panic!("{e}"));
    let mut replica = Replica::new(120, 40, Settings::default(), Role::Authoritative);
    let mut sf = None;
    for rec in &records {
        // Everything the program sent but the closing `x`: the transcript stays open.
        if rec.dir != crate::Dir::Out || !matches!(rec.verb.as_str(), "q" | "o" | "f" | "t" | "b") {
            continue;
        }
        if rec.verb == "o" {
            sf = rec.body["id"].as_str().map(str::to_owned);
        }
        let params: Vec<(String, String)> = rec
            .params
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    v.as_str().map_or_else(|| v.to_string(), str::to_owned),
                )
            })
            .collect();
        let params: Vec<(&str, &str)> = params
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let body = match &rec.body {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        replica.push(Item::Output(&tsp(&rec.verb, &params, &body)));
    }
    let sf = sf.expect("the recording opens a surface");
    let surface = replica
        .tsp()
        .find(&sf, false)
        .unwrap_or_else(|| panic!("surface {sf} after the replay"));
    let main = surface.document().region("main").expect("a main region");
    let entries: Vec<Value> = main
        .children()
        .filter(|c| c.kind() != "spacer")
        .map(|c| c.to_json())
        .collect();
    assert!(entries.len() >= 10, "only {} omp entries", entries.len());
    entries
}

/// Gives every node in `node` a distinct id for copy `copy`.
fn reid(node: &mut Value, copy: usize) {
    if let Some(id) = node.get("id").and_then(Value::as_str) {
        node["id"] = json!(format!("{id}~{copy}"));
    }
    if let Some(children) = node.get_mut("c").and_then(Value::as_array_mut) {
        for c in children {
            reid(c, copy);
        }
    }
}

/// A TSP recording (Tern's JSONL) of an omp-style transcript with `entries` entries: omp's own
/// final entries ([`omp_entries`]) repeated in order, each copy with fresh ids, added to an inline
/// surface's `main` region `per_frame` at a time. Entries larger than `max_entry` bytes of JSON
/// are left out so the transcript stays within a pane's retention budget (§7.10).
pub fn transcript_recording(entries: usize, per_frame: usize, max_entry: usize) -> String {
    let source: Vec<Value> = omp_entries()
        .into_iter()
        .filter(|e| e.to_string().len() <= max_entry)
        .collect();
    let line = |verb: &str, body: Value| {
        json!({"t": 0, "dir": "out", "verb": verb, "params": Map::new(), "body": body}).to_string()
    };
    let mut out = vec![
        line(
            "q",
            json!({"q": "hello", "v": [1], "app": "omp", "features": ["edit", "undo", "send"]}),
        ),
        line(
            "o",
            json!({"id": "tx", "mode": "inline", "title": "transcript"}),
        ),
    ];
    // The regions, as omp's first frame adds them.
    let mut ops: Vec<Value> = ["main", "dock", "layer"]
        .iter()
        .map(|r| json!(["add", r, "tx", null, {"id": r, "k": "col"}]))
        .collect();
    let mut s = 1;
    for i in 0..entries {
        let mut node = source[i % source.len()].clone();
        reid(&mut node, i / source.len());
        ops.push(json!(["add", node["id"].clone(), "main", null, node]));
        if (i + 1) % per_frame == 0 || i + 1 == entries {
            out.push(line(
                "f",
                json!({"sf": "tx", "s": s, "ops": std::mem::take(&mut ops)}),
            ));
            s += 1;
        }
    }
    out.join("\n") + "\n"
}

/// Writes a `size`-byte text file of `cat`-style output: plain ASCII log lines, no escapes, so
/// nothing in it looks like the probe's marker.
pub fn write_text_file(path: &Path, size: usize) {
    let mut out = Vec::with_capacity(size + 128);
    let mut i = 0u64;
    while out.len() < size {
        i += 1;
        out.extend_from_slice(
            format!(
                "2026-10-08T12:00:00Z INFO request id={i} path=/api/v1/items status=200 \
                 dur=1.3ms the quick brown fox jumps over the lazy dog\n"
            )
            .as_bytes(),
        );
    }
    out.truncate(size);
    std::fs::write(path, out).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
}

// Against Ghostty (S2).

/// The Ghostty app the comparison runs.
pub const GHOSTTY_APP: &str = "/Applications/Ghostty.app";

/// What the screen probe needs, as the probe reports it.
#[derive(Clone, Debug, Default)]
pub struct Permissions {
    pub screen_recording: bool,
    pub accessibility: bool,
}

impl Permissions {
    /// The missing permissions, named as System Settings names them, for a failure message.
    pub fn missing(&self) -> Vec<&'static str> {
        let mut missing = Vec::new();
        if !self.screen_recording {
            missing.push(
                "Screen Recording (System Settings > Privacy & Security > Screen & System Audio \
                 Recording) for the terminal app running the tests: the probe times frames on screen",
            );
        }
        if !self.accessibility {
            missing.push(
                "Accessibility (System Settings > Privacy & Security > Accessibility) for the \
                 terminal app running the tests: the probe posts key events",
            );
        }
        missing
    }
}

/// The screen probe's source (`screen/probe.swift`).
pub fn probe_source() -> PathBuf {
    repo_root().join("crates/conformance/screen/probe.swift")
}

static PROBE: OnceLock<Result<PathBuf, String>> = OnceLock::new();

/// Builds the screen probe with `swiftc` into `dir` (a test's `CARGO_TARGET_TMPDIR`), once per
/// process, and returns its path. Panics naming `swiftc` when it is missing or the build fails.
pub fn screen_probe(dir: &Path) -> PathBuf {
    PROBE
        .get_or_init(|| {
            let src = probe_source();
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            let out = dir.join("screen-probe");
            let tmp = dir.join(format!("screen-probe.{}.tmp", std::process::id()));
            match Command::new("swiftc")
                .arg("-O")
                .arg("-o")
                .arg(&tmp)
                .arg(&src)
                .status()
            {
                Ok(s) if s.success() => {}
                Ok(s) => return Err(format!("swiftc failed building {}: {s}", src.display())),
                Err(e) => {
                    return Err(format!(
                        "prerequisite missing: `swiftc` (Xcode command line tools) builds the \
                         screen probe and could not run: {e}"
                    ));
                }
            }
            std::fs::rename(&tmp, &out).map_err(|e| format!("install {}: {e}", out.display()))?;
            Ok(out)
        })
        .clone()
        .unwrap_or_else(|e| panic!("{e}"))
}

/// Runs the probe with `args` and parses its JSON output; a non-zero exit is an error carrying
/// its stderr.
pub fn run_probe(probe: &Path, args: &[&str]) -> Result<Value, String> {
    let out = Command::new(probe)
        .args(args)
        .output()
        .map_err(|e| format!("run {}: {e}", probe.display()))?;
    if !out.status.success() {
        return Err(format!(
            "screen probe {args:?} failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| {
        format!(
            "screen probe {args:?}: bad output {:?}: {e}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

/// The permissions the probe has (it runs as a child of the test, so macOS attributes its
/// requests to the app that started the tests).
pub fn permissions(probe: &Path) -> Permissions {
    let v = run_probe(probe, &["check"]).unwrap_or_else(|e| panic!("{e}"));
    Permissions {
        screen_recording: v["screen_recording"] == true,
        accessibility: v["accessibility"] == true,
    }
}

/// A Ghostty instance running one command, hermetic: no config files of the user's, no saved
/// window state, closed when dropped.
#[derive(Debug)]
pub struct Ghostty {
    pub pid: i32,
}

impl Ghostty {
    /// Opens a new Ghostty instance running `script` in a window of `cols`×`rows` cells, and
    /// waits until `ready` exists (the script creates it). Fails naming Gatekeeper's first-launch
    /// prompt when Ghostty never runs the script.
    pub fn run(script: &Path, ready: &Path, cols: u16, rows: u16) -> Result<Ghostty, String> {
        if !Path::new(GHOSTTY_APP).is_dir() {
            return Err(format!(
                "prerequisite missing: {GHOSTTY_APP} (brew install --cask ghostty)"
            ));
        }
        let before = ghostty_pids();
        let status = Command::new("open")
            .args(["-na", GHOSTTY_APP, "--args"])
            .args([
                "--config-default-files=false",
                "--window-save-state=never",
                "--confirm-close-surface=false",
                "--quit-after-last-window-closed=true",
                "--cursor-style-blink=false",
                "--font-size=13",
                &format!("--window-width={cols}"),
                &format!("--window-height={rows}"),
                "-e",
            ])
            .arg(script)
            .status()
            .map_err(|e| format!("open Ghostty: {e}"))?;
        if !status.success() {
            return Err(format!("`open {GHOSTTY_APP}` failed: {status}"));
        }
        let started = Instant::now();
        while !ready.exists() {
            if started.elapsed() > Duration::from_secs(20) {
                if let Some(pid) = ghostty_pids().into_iter().find(|p| !before.contains(p)) {
                    let _ = Command::new("kill").arg(pid.to_string()).status();
                }
                return Err(format!(
                    "prerequisite missing: Ghostty never ran its command within 20 s. Ghostty has \
                     probably never been opened on this Mac: open {GHOSTTY_APP} once from Finder \
                     and accept Gatekeeper's first-launch prompt"
                ));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let pid = ghostty_pids()
            .into_iter()
            .find(|p| !before.contains(p))
            .ok_or("no new Ghostty process")?;
        Ok(Ghostty { pid })
    }
}

impl Drop for Ghostty {
    fn drop(&mut self) {
        let _ = Command::new("kill").arg(self.pid.to_string()).status();
    }
}

fn ghostty_pids() -> Vec<i32> {
    let Ok(out) = Command::new("pgrep").arg("-x").arg("ghostty").output() else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect()
}
