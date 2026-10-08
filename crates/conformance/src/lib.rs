//! Conformance suites (spec §21).
//!
//! M0 provides the recording format and fixture validation. The TSP checklist
//! (`research/sdk-wire.md` §10) and the SDK and omp runs build on these in M3 and M5.

pub mod app;
pub mod fuzz;
pub mod goldens;
pub mod harness;
pub mod layout;
pub mod native;
pub mod perf;
pub mod probe;
pub mod remote;
pub mod screen;
pub mod surfaces;
pub mod viewer;
pub mod vt;
pub mod web;
pub mod webkit;

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Map, Value};

/// Who wrote a recorded message: the program (`out`) or the terminal (`in`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Dir {
    Out,
    In,
}

/// One line of a TSP recording in Tern's format (book:11579):
/// `{"t":ms,"dir":"out"|"in","verb":"f","params":{…},"body":{…}}`.
///
/// `surface-play` also accepts hand-written `osc` and `pty` lines, so `t` and `params`
/// are optional.
#[derive(Debug, Clone, Deserialize)]
pub struct Record {
    #[serde(default)]
    pub t: Option<f64>,
    pub dir: Dir,
    pub verb: String,
    #[serde(default)]
    pub params: Map<String, Value>,
    pub body: Value,
}

/// The frame ops of TSP v1 (spec §7.6).
pub const OPS: [&str; 12] = [
    "add", "set", "text", "splice", "move", "del", "settle", "focus", "reveal", "scroll",
    "suspend", "resume",
];

/// The terminal-to-program events of TSP v1 (spec §7.7).
pub const EVENTS: [&str; 16] = [
    "ack", "resize", "theme", "motion", "visible", "toggle", "select", "activate", "action",
    "change", "focus", "edit", "undo", "send", "error", "gone",
];

/// The directory of recorded TSP sessions.
pub fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/tsp")
}

/// Reads a JSONL recording, skipping blank lines.
pub fn load(path: &Path) -> Result<Vec<Record>, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(n, line)| {
            serde_json::from_str(line).map_err(|e| format!("{}:{}: {e}", path.display(), n + 1))
        })
        .collect()
}

/// Checks each record's shape and returns the problems found, one string each.
pub fn validate(records: &[Record]) -> Vec<String> {
    let mut problems = Vec::new();
    for (n, rec) in records.iter().enumerate() {
        let mut bad = |what: String| problems.push(format!("record {}: {what}", n + 1));
        let verb = rec.verb.as_str();
        match (rec.dir, verb) {
            (Dir::Out, "q" | "o" | "f" | "t" | "s" | "x") | (Dir::In, "r" | "e") => {
                let Some(body) = rec.body.as_object() else {
                    bad(format!("`{verb}` body is not an object"));
                    continue;
                };
                check_body(verb, body, &mut bad);
            }
            (Dir::Out, "b" | "osc" | "pty") => {
                if !rec.body.is_string() {
                    bad(format!("`{verb}` body is not a string"));
                }
            }
            (dir, verb) => bad(format!("unexpected verb `{verb}` for {dir:?}")),
        }
    }
    problems
}

fn check_body(verb: &str, body: &Map<String, Value>, bad: &mut impl FnMut(String)) {
    let has_str = |key: &str| body.get(key).is_some_and(Value::is_string);
    match verb {
        "q" if !has_str("q") => bad("query without `q`".into()),
        "o" | "x" if !has_str("id") => bad(format!("`{verb}` without an `id`")),
        "r" if !has_str("r") => bad("reply without `r`".into()),
        "e" => match body.get("ev").and_then(Value::as_str) {
            Some(ev) if EVENTS.contains(&ev) => {}
            other => bad(format!("unknown event {other:?}")),
        },
        "f" => {
            if !has_str("sf") || !body.get("s").is_some_and(Value::is_number) {
                bad("frame without `sf` and `s`".into());
            }
            match body.get("ops").and_then(Value::as_array) {
                None => bad("frame without an `ops` array".into()),
                Some(ops) => {
                    for op in ops {
                        match op.get(0).and_then(Value::as_str) {
                            Some(name) if OPS.contains(&name) => {}
                            other => bad(format!("unknown op {other:?}")),
                        }
                    }
                }
            }
        }
        _ => {}
    }
}
