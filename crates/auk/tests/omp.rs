//! The omp suite (GOAL.md row `omp`, spec §7, §8.2, M5, AC9): omp, the main TSP client, against
//! the real daemon and a window that draws, acks and edits natively.
//!
//! - **Replay:** `fixtures/tsp/omp-session.jsonl` (omp 18.8.4 on three prompts) is played into a
//!   daemon pane with `surface-play`; the window's replica projects it through `auk-views` into
//!   `auk-ui`, and the transcript, tool cards and diffs are drawn as their kinds.
//! - **Live:** the installed omp runs in a hermetic daemon pane: a zsh with the test `ZDOTDIR`, in
//!   a temporary cwd, with `HOME` pointing at the user's real home only for omp's login. The
//!   window is the pane's primary. Live turns spend real API credits, so there are two tiny
//!   prompts in one test, and every assertion checks structure, never model wording.
//! - **Fallback:** with TSP off, omp runs as plain text. Under Auk's T0+T1 hello (D22), omp's own
//!   record of what it sent (`PI_TUI_TSP_RECORD`) shows which T2 kinds it sends anyway, which it
//!   replaces, and which ops the daemon rejected as a result.
//! - **Version:** every test first checks that the installed omp is the recorded 18.8.4.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use auk_conformance::harness::{TestEnv, quote};
use auk_conformance::native::{NativeWindow, kinds_under};
use auk_term::input::{Key, Mods};
use auk_views::Outcome;
use serde_json::Value;

/// The omp version the fixtures were recorded with (`fixtures/tsp/omp-session.meta.json`).
const OMP_VERSION: &str = "18.8.4";

/// How long a live turn may take, from the prompt to the turn's usage row.
const TURN: Duration = Duration::from_secs(180);

/// How long omp may take to start, or to react to a key.
const UI: Duration = Duration::from_secs(30);

/// The T2 kinds of §8.2 (omp's pickers and settings, rich media). `image` joined T1 (D24).
const T2: [&str; 6] = ["math", "chart", "picker", "prefs", "effort", "block"];

/// The pane size the window asks for.
const SIZE: (u16, u16) = (120, 40);

// ---------------------------------------------------------------------------------------------
// omp itself

/// The user's real home: omp's login lives there.
fn real_home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").expect("HOME is set for the test process"))
}

/// The installed omp, checked to be the recorded version. Fails clearly otherwise.
fn omp() -> PathBuf {
    let path = real_home().join(".local/bin/omp");
    assert!(
        path.is_file(),
        "omp is not installed at {} (the omp suite needs omp {OMP_VERSION}, logged in)",
        path.display()
    );
    let out = Command::new(&path)
        .arg("--version")
        .env("HOME", real_home())
        .output()
        .unwrap_or_else(|e| panic!("run {} --version: {e}", path.display()));
    let version = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    assert_eq!(
        version,
        format!("omp/{OMP_VERSION}"),
        "the installed omp ({}) must be the recorded {OMP_VERSION}; re-record the omp fixtures \
         (tools/tsp_record.py) and update this suite before moving to another version",
        path.display()
    );
    path
}

#[test]
fn omp_is_the_recorded_version() {
    omp();
    let meta: Value = serde_json::from_str(
        &std::fs::read_to_string(auk_conformance::fixtures_dir().join("omp-session.meta.json"))
            .expect("read omp-session.meta.json"),
    )
    .expect("parse omp-session.meta.json");
    assert!(
        meta["note"]
            .as_str()
            .is_some_and(|n| n.contains(&format!("omp {OMP_VERSION}"))),
        "the recording names another omp: {}",
        meta["note"]
    );
}

/// A live omp in a hermetic daemon pane, shown by a native window.
struct LiveOmp {
    env: TestEnv,
    pane: u64,
    window: NativeWindow,
    /// omp's own record of the TSP messages it sent and received.
    log: PathBuf,
    /// omp's working directory: an empty temporary directory.
    work: PathBuf,
    /// The shell's prompt count when omp started.
    prompts: u64,
}

impl LiveOmp {
    /// Starts a daemon (with TSP switched off when `tsp` is false), a zsh pane in an empty
    /// temporary cwd and the window, then runs omp from the shell's prompt.
    fn start(name: &str, tsp: bool) -> LiveOmp {
        let omp = omp();
        let env = TestEnv::new(env!("CARGO_BIN_EXE_auk"), name);
        env.start_daemon();
        if !tsp {
            env.daemon("settings --native-surfaces off");
        }
        let work = env.root.join("work");
        std::fs::create_dir_all(&work).expect("create omp's cwd");
        let editor = env.root.join("editor.sh");
        std::fs::write(
            &editor,
            "#!/bin/sh\n# The external editor: appends a marker to the file omp hands it.\n\
             printf '%s + ext-edit' \"$(cat \"$1\")\" > \"$1\"\n",
        )
        .expect("write the editor script");
        make_executable(&editor);
        let log = env.root.join("omp-tsp.jsonl");

        let pane = env.new_pane(&["--cwd", &work.to_string_lossy()]);
        let ready = env.wait(pane, "prompt");
        let prompts = ready["prompts"].as_u64().expect("a prompt count");
        let mut window = NativeWindow::attach(&env, "main", SIZE);
        window.wait("the window to become the pane's primary", UI, |w| {
            (w.is_primary(pane)
                && w.try_replica(pane)
                    .is_some_and(|r| r.terminal().cols() == SIZE.0))
            .then_some(())
        });
        let line = format!(
            "HOME={} EDITOR={} PI_TUI_TSP_RECORD={} {} --no-session --no-title\r",
            quote(&real_home().to_string_lossy()),
            quote(&editor.to_string_lossy()),
            quote(&log.to_string_lossy()),
            quote(&omp.to_string_lossy()),
        );
        window.input(pane, line.as_bytes());
        LiveOmp {
            env,
            pane,
            window,
            log,
            work,
            prompts,
        }
    }

    /// Waits until omp's session surface is live with its composer focused and sendable, and
    /// returns the composer's node id.
    fn ready(&mut self) -> String {
        let pane = self.pane;
        self.window
            .wait("omp's composer to be focused and sendable", UI, |w| {
                let (sf, id) = w.focused_field(pane)?;
                let n = w.field(pane, &id)?;
                (sf == "s:1" && n.prop("sendable") == Some(&Value::Bool(true))).then_some(id)
            })
    }

    /// The composer's text as omp last set it.
    fn field_text(&self, id: &str) -> Option<String> {
        self.window
            .field(self.pane, id)
            .map(|n| n.text().unwrap_or("").to_owned())
    }

    /// Waits until the composer holds `text`.
    fn wait_text(&mut self, id: &str, text: &str) {
        let pane = self.pane;
        let what = format!("the composer to read {text:?}");
        self.window.wait(&what, UI, |w| {
            (w.field(pane, id)?.text().unwrap_or("") == text).then_some(())
        });
    }

    /// Empties the composer natively: select all, then delete the selection (an `edit`).
    fn clear(&mut self, id: &str) {
        if self.field_text(id).is_none_or(|t| t.is_empty()) {
            return;
        }
        let o = self.window.select(self.pane, 0, u64::MAX);
        assert_eq!(o, Outcome::Redraw, "selecting is local");
        let o = self.window.type_in_field(self.pane, "");
        assert!(
            matches!(o, Outcome::Event { .. }),
            "deleting a selection is an edit: {o:?}"
        );
        self.wait_text(id, "");
    }

    /// Nodes of the session surface whose `role` prop is `role`.
    fn with_role(&self, role: &str) -> Vec<(String, String)> {
        let Some(s) = self.window.live_surface(self.pane, "s:1") else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (id, kind) in kinds_under(s.document().root()) {
            if s.document()
                .get(&id)
                .and_then(|n| n.prop("role"))
                .and_then(Value::as_str)
                == Some(role)
            {
                out.push((id, kind));
            }
        }
        out
    }

    /// Waits until the transcript has `n` finished turns (each ends with omp's usage row).
    fn wait_turns(&mut self, n: usize) {
        let pane = self.pane;
        let what = format!("{n} finished omp turn(s)");
        self.window.wait(&what, TURN, |w| {
            let s = w.live_surface(pane, "s:1")?;
            let turns = kinds_under(s.document().root())
                .iter()
                .filter(|(id, _)| {
                    s.document()
                        .get(id)
                        .and_then(|n| n.prop("role"))
                        .and_then(Value::as_str)
                        == Some("omp.turn.usage")
                })
                .count();
            (turns >= n).then_some(())
        });
    }

    /// omp's own record of the TSP messages it sent and received.
    fn records(&self) -> Vec<Value> {
        let text = std::fs::read_to_string(&self.log)
            .unwrap_or_else(|e| panic!("read omp's TSP record {}: {e}", self.log.display()));
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).expect("a JSON line in omp's TSP record"))
            .collect()
    }

    /// Quits omp (⌃D on an empty composer) and waits for the shell's next prompt.
    fn quit(&mut self) {
        self.window.ctrl(self.pane, 'd');
        let done = self.env.daemon(&format!(
            "wait {} --until prompt --timeout {}",
            self.pane,
            UI.as_secs()
        ));
        assert_eq!(
            done["timeout"],
            Value::Null,
            "omp didn't exit on ⌃D: {done}"
        );
        assert_eq!(
            done["prompts"].as_u64(),
            Some(self.prompts + 1),
            "the shell's next prompt after omp: {done}"
        );
        self.window.converge(&self.env, self.pane);
    }
}

impl Drop for LiveOmp {
    fn drop(&mut self) {
        // omp must never outlive its test, whatever happened: its command line names this
        // test's own record file, so the match can't reach anything else.
        let _ = Command::new("pkill")
            .args(["-9", "-f", &self.log.to_string_lossy()])
            .status();
    }
}

fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perm = std::fs::metadata(path).expect("stat").permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(path, perm).expect("chmod");
}

fn ctrl() -> Mods {
    Mods {
        ctrl: true,
        ..Mods::default()
    }
}

/// Every node an `add` op inserts, with its kind: `(sf, id, kind)`.
fn added_kinds(records: &[Value]) -> Vec<(String, String, String)> {
    fn walk(sf: &str, n: &Value, out: &mut Vec<(String, String, String)>) {
        out.push((
            sf.to_owned(),
            n["id"].as_str().unwrap_or("").to_owned(),
            n["k"].as_str().unwrap_or("").to_owned(),
        ));
        for c in n["c"].as_array().into_iter().flatten() {
            walk(sf, c, out);
        }
    }
    let mut out = Vec::new();
    for r in records {
        if r["dir"] == "out" && r["verb"] == "f" {
            let sf = r["body"]["sf"].as_str().unwrap_or("");
            for op in r["body"]["ops"].as_array().into_iter().flatten() {
                if op[0] == "add" {
                    walk(sf, &op[4], &mut out);
                }
            }
        }
    }
    out
}

/// CSS effects the engine accepts but doesn't draw (§8.4 "Effects": filters, backdrop-filter,
/// masks, 3D transforms, and box-shadow among the other accepted-unused properties of
/// `auk-ui`'s `properties.rs`).
const UNDRAWN: [&str; 14] = [
    "filter",
    "backdrop-filter",
    "box-shadow",
    "clip-path",
    "mix-blend-mode",
    "isolation",
    "perspective",
    "perspective-origin",
    "translate",
    "rotate",
    "scale",
    "transform-style",
    "backface-visibility",
    "background-blend-mode",
];

/// The declarations in `css` that name an effect the engine doesn't draw: an [`UNDRAWN`]
/// property, any `mask*`, or a `transform` with a 3D function.
fn undrawn_effects(css: &str) -> Vec<String> {
    css.split(['{', '}', ';'])
        .filter_map(|decl| {
            let (name, value) = decl.split_once(':')?;
            let name = name.trim().to_ascii_lowercase();
            let value = value.to_ascii_lowercase();
            let undrawn = UNDRAWN.contains(&name.as_str())
                || name.starts_with("mask")
                || name.starts_with("-webkit-mask")
                || (name == "transform" && (value.contains("3d(") || value.contains("perspective(")));
            undrawn.then(|| decl.trim().to_owned())
        })
        .collect()
}

/// Every piece of CSS omp's records carry: the stylesheets it installs (`s`) and the inline
/// `style` of the `el` nodes it adds or sets.
fn program_css(records: &[Value]) -> Vec<String> {
    fn el_styles(n: &Value, out: &mut Vec<String>) {
        if n["k"] == "el"
            && let Some(style) = n["p"]["style"].as_str()
        {
            out.push(style.to_owned());
        }
        for c in n["c"].as_array().into_iter().flatten() {
            el_styles(c, out);
        }
    }
    let mut out = Vec::new();
    for r in records.iter().filter(|r| r["dir"] == "out") {
        match r["verb"].as_str() {
            Some("s") => out.push(r["body"]["css"].as_str().unwrap_or_default().to_owned()),
            Some("f") => {
                for op in r["body"]["ops"].as_array().into_iter().flatten() {
                    match op[0].as_str() {
                        Some("add") => el_styles(&op[4], &mut out),
                        Some("set") => {
                            if let Some(style) = op[2]["style"].as_str() {
                                out.push(style.to_owned());
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// What omp's record says about T2 kinds under the hello it got: the hello's kinds, the
/// unadvertised kinds omp sent anyway, and every error the daemon answered, each checked to be
/// explained by an unadvertised kind (directly, or through an op on a node whose `add` failed).
struct T2Report {
    advertised: BTreeSet<String>,
    sent_anyway: BTreeMap<String, Vec<String>>,
    errors: Vec<String>,
    unexplained: Vec<String>,
}

fn t2_report(records: &[Value]) -> T2Report {
    let advertised: BTreeSet<String> = records
        .iter()
        .find(|r| r["dir"] == "in" && r["verb"] == "r" && r["body"]["r"] == "hello")
        .map(|r| {
            r["body"]["kinds"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        // omp 18.8.4 doesn't record the hello reply; the kinds it saw are the daemon's (D22).
        .unwrap_or_else(|| {
            auk_tsp::T0_KINDS
                .iter()
                .chain(auk_tsp::T1_KINDS.iter())
                .map(|k| (*k).to_owned())
                .collect()
        });
    let mut sent_anyway: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (_, id, kind) in added_kinds(records) {
        if !advertised.contains(&kind) {
            sent_anyway.entry(kind).or_default().push(id);
        }
    }
    // Frames by (sf, s), to find the op an error names.
    let mut frames: BTreeMap<(String, u64), &Value> = BTreeMap::new();
    for r in records {
        if r["dir"] == "out" && r["verb"] == "f" {
            let key = (
                r["body"]["sf"].as_str().unwrap_or("").to_owned(),
                r["body"]["s"].as_u64().unwrap_or(0),
            );
            frames.insert(key, &r["body"]);
        }
    }
    let mut failed: BTreeSet<String> = BTreeSet::new();
    let mut errors = Vec::new();
    let mut unexplained = Vec::new();
    for r in records {
        let b = &r["body"];
        if !(r["dir"] == "in" && r["verb"] == "e" && b["ev"] == "error") {
            continue;
        }
        let msg = b["msg"].as_str().unwrap_or("").to_owned();
        errors.push(b.to_string());
        let op = b["sf"]
            .as_str()
            .zip(b["s"].as_u64())
            .and_then(|(sf, s)| frames.get(&(sf.to_owned(), s)))
            .and_then(|f| b["op"].as_u64().map(|i| &f["ops"][i as usize]));
        let Some(op) = op else {
            unexplained.push(b.to_string());
            continue;
        };
        let ids: Vec<&str> = op
            .as_array()
            .into_iter()
            .flatten()
            .skip(1)
            .filter_map(Value::as_str)
            .collect();
        let unknown_kind = sent_anyway
            .keys()
            .any(|k| msg.ends_with(&format!("unknown kind {k}")));
        let cascade = ids.iter().any(|id| failed.contains(*id));
        if unknown_kind || cascade {
            if op[0] == "add" {
                failed.insert(op[1].as_str().unwrap_or("").to_owned());
                if let Some(subtree) = op.get(4) {
                    for (_, id, _) in added_kinds(&[serde_json::json!({
                        "dir": "out", "verb": "f",
                        "body": {"sf": "", "ops": [["add", "", "", null, subtree]]}
                    })]) {
                        failed.insert(id);
                    }
                }
            }
        } else {
            unexplained.push(b.to_string());
        }
    }
    T2Report {
        advertised,
        sent_anyway,
        errors,
        unexplained,
    }
}

/// Whether node `id` shows: neither it nor an ancestor is `hidden`, and no ancestor is a folded
/// card, section, tool or agent it isn't the head of.
fn shown(doc: &auk_tsp::Document, id: &str) -> bool {
    let truthy = |n: &auk_tsp::NodeRef<'_>, p: &str| n.prop(p) == Some(&Value::Bool(true));
    let Some(node) = doc.get(id) else {
        return false;
    };
    if truthy(&node, "hidden") {
        return false;
    }
    let mut child = node;
    while let Some(parent) = child.parent() {
        if truthy(&parent, "hidden") {
            return false;
        }
        let head = parent.prop("head").and_then(Value::as_str);
        if truthy(&parent, "collapsed") && head != Some(child.id()) {
            return false;
        }
        child = parent;
    }
    true
}

/// The drawn size of an `image` node: its element is `.sf-image` without the placeholder, and
/// its picture element (`.sf-img-frame` for rasters, `.sf-img-svg` for SVG) holds a decoded
/// picture and has a box. `None` until then.
fn image_drawn(
    window: &NativeWindow,
    pane: u64,
    surface: &auk_tsp::Surface,
    id: &str,
) -> Option<(f32, f32)> {
    let el = window.element_of(pane, surface, id)?;
    let class = window.class(pane, el);
    if !class.split(' ').any(|c| c == "sf-image") || class.split(' ').any(|c| c == "missing") {
        return None;
    }
    let pic = window
        .with_class(pane, el, "sf-img-frame")
        .into_iter()
        .chain(window.with_class(pane, el, "sf-img-svg"))
        .next()?;
    window.doc(pane).image(pic)?;
    let b = window.doc(pane).border_box(pic)?;
    (b.w > 0.0 && b.h > 0.0).then_some((b.w, b.h))
}

// ---------------------------------------------------------------------------------------------
// Replay of the recording

/// AC9 from the recording: omp's transcript (user and assistant messages in `md`), its tool cards
/// and its diffs reach the daemon through `surface-play` and are drawn natively: every such node is
/// an element of its kind in the window's projected document, with a laid-out box. The composer
/// is a focused `editor` in `dock`, and `main`, `dock` and `layer` are the surface's regions.
#[test]
fn ac09_replay_draws_transcript_tool_cards_and_diffs_natively() {
    omp();
    let env = TestEnv::new(env!("CARGO_BIN_EXE_auk"), "omp-replay");
    env.start_daemon();
    let pane = env.new_pane(&[]);
    env.wait(pane, "prompt");
    let mut window = NativeWindow::attach(&env, "main", SIZE);
    let path = auk_conformance::fixtures_dir().join("omp-session.jsonl");
    let recorded_frames = auk_conformance::load(&path)
        .expect("load the recording")
        .iter()
        .filter(|r| r.dir == auk_conformance::Dir::Out && r.verb == "f")
        .count() as u64;
    // Every frame, but not omp's final close: the session stays live, `dock` and `layer` included.
    let played = env.daemon(&format!(
        "surface-play --pane {pane} {} until {recorded_frames}",
        quote(&path.to_string_lossy())
    ));
    assert_eq!(played["frames"].as_u64(), Some(recorded_frames), "{played}");
    window.converge(&env, pane);

    let tsp = window.tsp(pane);
    let s = tsp
        .surfaces()
        .iter()
        .find(|s| s.id() == "s:1")
        .expect("omp's session surface");
    assert_eq!(s.role(), Some("omp.session"));
    let doc = s.document();
    for region in ["main", "dock", "layer"] {
        assert!(doc.region(region).is_some(), "region {region} is missing");
    }
    let all = kinds_under(doc.root());
    let count = |k: &str| all.iter().filter(|(_, kind)| kind == k).count();
    for (kind, at_least) in [
        ("md", 3),
        ("tool", 5),
        ("diff", 1),
        ("editor", 1),
        ("status", 1),
    ] {
        assert!(
            count(kind) >= at_least,
            "{kind}: {} in the final document, expected at least {at_least}",
            count(kind)
        );
    }
    let roles = |role: &str| {
        all.iter()
            .filter(|(id, _)| {
                doc.get(id)
                    .and_then(|n| n.prop("role"))
                    .and_then(Value::as_str)
                    == Some(role)
            })
            .count()
    };
    assert_eq!(roles("omp.user"), 3, "three user messages");
    assert!(roles("omp.assistant") >= 3, "assistant messages");

    // Every transcript, tool-card and diff node is drawn as its kind, with a box.
    let mut drawn = BTreeMap::<&str, usize>::new();
    for (id, kind) in &all {
        if ![
            "md", "tool", "diff", "code", "editor", "status", "card", "section",
        ]
        .contains(&kind.as_str())
        {
            continue;
        }
        let el = window
            .element_of(pane, s, id)
            .unwrap_or_else(|| panic!("{kind} node {id} isn't drawn"));
        let class = window.class(pane, el);
        assert!(
            class.split(' ').any(|c| c == format!("sf-{kind}")),
            "{kind} node {id} is drawn as `{class}`"
        );
        if shown(doc, id) {
            let b = window
                .doc(pane)
                .border_box(el)
                .unwrap_or_else(|| panic!("{kind} node {id} has no box"));
            assert!(b.w > 0.0, "{kind} node {id} has no width: {b:?}");
        }
        *drawn.entry(kind).or_default() += 1;
    }
    eprintln!("drawn natively: {drawn:?}");

    // The diffs draw their hunks: added and removed lines as rows of the diff.
    for (id, _) in all.iter().filter(|(_, k)| k == "diff") {
        let el = window.element_of(pane, s, id).expect("a diff element");
        let text = window.text_content(pane, el);
        assert!(text.contains("Bytes"), "diff {id} shows the edit: {text:?}");
        assert!(
            text.contains("Size"),
            "diff {id} shows the removed line: {text:?}"
        );
    }
    // A tool card shows its title and target.
    let edit = all
        .iter()
        .find(|(id, k)| {
            k == "tool" && doc.get(id).and_then(|n| n.prop("name")) == Some(&Value::from("edit"))
        })
        .expect("the edit tool card");
    let el = window
        .element_of(pane, s, &edit.0)
        .expect("the edit card's element");
    let text = window.text_content(pane, el);
    assert!(
        text.contains("table.py"),
        "the edit card names its file: {text:?}"
    );

    // The recording's `image` nodes draw (D24): the welcome card's logo is omp's built-in mark,
    // and the chart of a numeric table is an SVG blob sent after the frame that names it.
    assert!(doc.get("1").is_some(), "the welcome card arrived");
    let logo = doc.get("1.lockup/logo").expect("the welcome logo");
    assert_eq!(logo.prop("builtin"), Some(&Value::from("omp")));
    let chart = doc.get("1g.t1.1").expect("the table's chart");
    let blob = chart.prop("blob").and_then(Value::as_str).expect("a blob");
    assert!(tsp.blobs().has(blob), "the chart's blob reached the window");
    for id in ["1.lockup/logo", "1g.t1.1"] {
        let size = image_drawn(&window, pane, s, id)
            .unwrap_or_else(|| panic!("{id} isn't drawn as a picture"));
        eprintln!("image {id}: {size:?}");
    }
    assert!(
        doc.get("1g.t1.2").is_some(),
        "the text after the chart arrived"
    );

    // The composer: a focused editor in `dock`.
    let focus = doc.focus().expect("a focused field");
    let editor = doc.get(focus).expect("the focused node");
    assert_eq!(editor.kind(), "editor");
    let mut up = editor.parent();
    while let Some(p) = up {
        if p.parent().is_none_or(|pp| pp.id() == s.id()) {
            assert_eq!(p.id(), "dock", "the composer sits in `dock`");
            break;
        }
        up = p.parent();
    }
}

// ---------------------------------------------------------------------------------------------
// Live sessions

/// AC9 live, composer (no model turn): omp opens its inline session surface with `main`, `dock`
/// and `layer`; keys typed in the pane reach omp, which owns the text; a native selection replaced
/// by typing becomes an `edit` event omp applies; ⌘Z becomes an `undo` event omp applies; ⌃G runs
/// `$EDITOR` and omp comes back by adopting its closed surface (`o adopt:true`), with the edited
/// text in the composer. omp's settings and model picker (T2 `prefs` and `picker`) open as screen
/// surfaces built from T1 kinds. ⌃D quits omp, and the shell's next prompt discards the inline
/// surface.
/// §8.4: the effects the engine accepts but doesn't draw (filters, backdrop-filter, masks, 3D
/// transforms, box-shadow) are only a gap if omp sends them. It doesn't: neither recording holds a
/// stylesheet or an `el` style that names one, and after the replay through the daemon, the
/// window's surfaces hold no program sheet that does. (The live T2 test checks omp's own record.)
#[test]
fn omp_sends_none_of_the_css_effects_the_engine_leaves_undrawn() {
    assert_eq!(
        undrawn_effects(".a { box-shadow: 0 1px red; color: red } .b{transform: rotate3d(1,0,0,9deg)} .c { mask-image: none; transform: rotate(3deg) }"),
        ["box-shadow: 0 1px red", "transform: rotate3d(1,0,0,9deg)", "mask-image: none"],
        "the check itself"
    );
    for name in ["omp-session.jsonl", "omp-session-own.jsonl"] {
        let path = auk_conformance::fixtures_dir().join(name);
        let records: Vec<Value> = std::fs::read_to_string(&path)
            .expect("read the recording")
            .lines()
            .map(|l| serde_json::from_str(l).expect("a record"))
            .collect();
        let css = program_css(&records);
        let found: Vec<String> = css.iter().flat_map(|c| undrawn_effects(c)).collect();
        assert!(found.is_empty(), "{name}: omp sends undrawn effects: {found:?}");
    }
    omp();
    let env = TestEnv::new(env!("CARGO_BIN_EXE_auk"), "omp-effects");
    env.start_daemon();
    let pane = env.new_pane(&[]);
    env.wait(pane, "prompt");
    let mut window = NativeWindow::attach(&env, "main", SIZE);
    let path = auk_conformance::fixtures_dir().join("omp-session.jsonl");
    let played = env.daemon(&format!(
        "surface-play --pane {pane} {}",
        quote(&path.to_string_lossy())
    ));
    assert!(played["frames"].as_u64().is_some_and(|f| f > 0), "{played}");
    window.converge(&env, pane);
    let tsp = window.tsp(pane);
    assert!(!tsp.surfaces().is_empty());
    for s in tsp.surfaces() {
        for (name, css) in s.sheets().iter() {
            let found = undrawn_effects(css);
            assert!(found.is_empty(), "sheet {name} of {}: {found:?}", s.id());
        }
    }
}

#[test]
fn ac09_live_composer_native_editing_adopt_and_discard() {
    let mut omp = LiveOmp::start("omp-composer", true);
    let id = omp.ready();
    let pane = omp.pane;
    let tsp = omp.window.tsp(pane);
    let hello = tsp.hello().expect("omp's hello");
    for f in ["edit", "undo", "send"] {
        assert!(hello.has(f), "omp's hello lists `{f}`");
    }
    let s = omp
        .window
        .live_surface(pane, "s:1")
        .expect("the session surface");
    assert_eq!(s.role(), Some("omp.session"));
    assert_eq!(s.mode(), auk_tsp::Mode::Inline);
    for region in ["main", "dock", "layer"] {
        assert!(s.document().region(region).is_some(), "region {region}");
    }
    let editor_el = omp
        .window
        .element_of(pane, s, &id)
        .expect("the composer is drawn");
    assert!(omp.window.class(pane, editor_el).contains("sf-editor"));

    // Typing: keys reach omp, which answers with text ops.
    omp.window.type_keys(pane, "hello world");
    omp.wait_text(&id, "hello world");

    // Native selection, then typing over it: one `edit` event.
    let o = omp.window.select(pane, 6, 11);
    assert_eq!(o, Outcome::Redraw, "selecting is local");
    let s = omp
        .window
        .live_surface(pane, "s:1")
        .expect("the session surface");
    let editor_el = omp.window.element_of(pane, s, &id).expect("the composer");
    let sel = omp.window.with_class(pane, editor_el, "sf-ed-sel");
    assert!(!sel.is_empty(), "the native selection is drawn");
    let selected: String = sel
        .iter()
        .map(|e| omp.window.text_content(pane, *e))
        .collect();
    assert_eq!(selected, "world");
    let o = omp.window.type_in_field(pane, "there");
    assert!(
        matches!(o, Outcome::Event { .. }),
        "typing over a selection is an edit: {o:?}"
    );
    let ev = omp
        .window
        .sent_events
        .last()
        .expect("the edit event")
        .clone();
    assert_eq!(ev["ev"], "edit");
    assert_eq!(
        (ev["from"].as_u64(), ev["to"].as_u64()),
        (Some(6), Some(11))
    );
    assert_eq!(ev["text"], "there");
    omp.wait_text(&id, "hello there");
    let cursor = omp
        .window
        .field(pane, &id)
        .and_then(|n| n.prop("cursor").cloned());
    assert_eq!(
        cursor,
        Some(Value::from(11)),
        "omp moved the caret after the edit"
    );

    // ⌘Z: an `undo` event, and omp undoes the edit.
    let o = omp.window.undo(pane);
    assert!(
        matches!(o, Outcome::Event { .. }),
        "⌘Z is an undo event: {o:?}"
    );
    assert_eq!(
        omp.window.sent_events.last().expect("the undo")["ev"],
        "undo"
    );
    omp.wait_text(&id, "hello world");

    // ⌃G: the external editor, then omp adopts its closed surface.
    let before_key = omp.window.live_surface(pane, "s:1").expect("live").key();
    omp.window.ctrl(pane, 'g');
    omp.wait_text(&id, "hello world + ext-edit");
    let records = omp.records();
    let opens: Vec<&Value> = records
        .iter()
        .filter(|r| r["dir"] == "out" && r["verb"] == "o" && r["body"]["id"] == "s:1")
        .map(|r| &r["body"])
        .collect();
    assert_eq!(opens.len(), 2, "omp opened, then adopted: {opens:?}");
    assert_eq!(
        opens[1]["adopt"], true,
        "the second open adopts: {}",
        opens[1]
    );
    let closed_before_adopt = records.iter().any(|r| {
        r["dir"] == "out"
            && r["verb"] == "x"
            && r["body"]["id"] == "s:1"
            && r["body"]["keep"] == true
    });
    assert!(
        closed_before_adopt,
        "omp closed its surface with keep:true for the editor"
    );
    let gone_for_adopt = records
        .iter()
        .any(|r| r["dir"] == "in" && r["body"]["ev"] == "gone" && r["body"]["sf"].is_null());
    assert!(
        !gone_for_adopt,
        "the adopt found the closed surface (no `gone`)"
    );
    let sessions: Vec<_> = omp
        .window
        .tsp(pane)
        .surfaces()
        .iter()
        .filter(|s| s.id() == "s:1")
        .collect();
    assert_eq!(sessions.len(), 1, "the adopted surface is the same surface");
    assert!(sessions[0].is_live());
    assert_eq!(
        sessions[0].key(),
        before_key,
        "same surface, reopened in place"
    );
    assert!(
        sessions[0]
            .document()
            .region("main")
            .is_some_and(|m| m.child_count() > 0),
        "the transcript carried over"
    );

    // Settings and the model picker: screen surfaces over the session, from T1 kinds.
    for (command, role) in [
        ("/settings", "omp.overlay.settings"),
        ("/model", "omp.overlay.model-hub"),
    ] {
        omp.clear(&id);
        omp.window.type_keys(pane, command);
        omp.wait_text(&id, command);
        omp.window.press(pane, Key::Enter, Mods::default());
        let what = format!("omp's {command} screen");
        let kinds = omp.window.wait(&what, UI, |w| {
            let screen = w
                .tsp(pane)
                .surfaces()
                .iter()
                .find(|s| s.is_live() && s.mode() == auk_tsp::Mode::Screen)?;
            let found = kinds_under(screen.document().root())
                .into_iter()
                .find(|(id, _)| {
                    screen
                        .document()
                        .get(id)
                        .and_then(|n| n.prop("role"))
                        .and_then(Value::as_str)
                        == Some(role)
                })?;
            let node = screen.document().get(&found.0)?;
            Some(kinds_under(node))
        });
        let inline = omp
            .window
            .live_surface(pane, "s:1")
            .expect("the session under the screen");
        assert_eq!(
            inline.mode(),
            auk_tsp::Mode::Inline,
            "the screen surface leaves the inline one"
        );
        let used: BTreeSet<&str> = kinds.iter().map(|(_, k)| k.as_str()).collect();
        eprintln!("{command}: {used:?}");
        for k in T2 {
            assert!(!used.contains(k), "{command} sent T2 kind {k}");
        }
        omp.window.press(pane, Key::Escape, Mods::default());
        omp.window.wait("the screen surface to close", UI, |w| {
            let screens = w
                .tsp(pane)
                .surfaces()
                .iter()
                .filter(|s| s.mode() == auk_tsp::Mode::Screen)
                .count();
            (screens == 0).then_some(())
        });
        omp.ready();
    }

    // Leave with an empty composer, then ⌃D.
    omp.clear(&id);
    omp.quit();
    let left: Vec<_> = omp
        .window
        .tsp(pane)
        .surfaces()
        .iter()
        .map(|s| s.id().to_owned())
        .collect();
    assert!(
        left.is_empty(),
        "the next prompt discarded omp's surfaces: {left:?}"
    );
    assert!(
        omp.window.view(pane).scope(before_key).is_none(),
        "the window no longer draws the discarded surface"
    );

    let report = t2_report(&omp.records());
    assert!(
        report.unexplained.is_empty(),
        "errors not caused by unadvertised kinds: {:?}",
        report.unexplained
    );
    eprintln!("errors: {:?}", report.errors);
    eprintln!("T2 kinds sent anyway: {:?}", report.sent_anyway);
    assert!(omp.window.errors().is_empty(), "{:?}", omp.window.errors());
    let _ = &omp.work;
}

/// AC9 live, transcript (two tiny model turns): a prompt submitted through `send` is held until
/// the composer reports `sendable:true`, then omp runs the turn: the user message and the
/// assistant's reply are drawn as `md` in `main`. A second prompt typed as keys makes omp call its
/// bash tool, drawn as a `tool` card. Structure only, never wording.
#[test]
fn ac09_live_transcript_send_and_tool_card() {
    let mut omp = LiveOmp::start("omp-turns", true);
    let id = omp.ready();
    let pane = omp.pane;

    // A numeric table with four rows: omp 18.8.4 draws a chart of such a table as an `image`.
    let prompt = "Reply with only this markdown table and nothing else:\n\n| Name | Size |\n| --- | --- |\n| a | 10 |\n| b | 20 |\n| c | 30 |\n| d | 40 |";
    let o = omp
        .window
        .send_prompt(pane, &id, prompt)
        .expect("the composer takes `send`");
    assert!(matches!(o, Outcome::Event { .. }));
    let ev = omp.window.sent_events.last().expect("the send").clone();
    assert_eq!(ev["ev"], "send");
    assert_eq!(ev["id"], id.as_str());
    omp.wait_turns(1);

    let users = omp.with_role("omp.user");
    assert_eq!(users.len(), 1, "one user message: {users:?}");
    let s = omp
        .window
        .live_surface(pane, "s:1")
        .expect("the session surface");
    let user = s.document().get(&users[0].0).expect("the user message");
    let user_md: Vec<_> = kinds_under(user)
        .into_iter()
        .filter(|(_, k)| k == "md")
        .collect();
    assert!(!user_md.is_empty(), "the user message is md");
    let shown = s
        .document()
        .get(&user_md[0].0)
        .and_then(|n| n.text())
        .unwrap_or("");
    assert!(
        shown.contains("| a | 10 |"),
        "the user message is the prompt sent: {shown:?}"
    );
    let assistants = omp.with_role("omp.assistant");
    assert!(!assistants.is_empty(), "an assistant message");
    let reply = s
        .document()
        .get(&assistants.last().expect("one").0)
        .expect("the reply");
    let reply_md: Vec<_> = kinds_under(reply)
        .into_iter()
        .filter(|(_, k)| k == "md")
        .collect();
    assert!(!reply_md.is_empty(), "the reply is md");
    for (nid, kind) in user_md.iter().chain(&reply_md) {
        let el = omp
            .window
            .element_of(pane, s, nid)
            .unwrap_or_else(|| panic!("{nid} isn't drawn"));
        assert!(
            omp.window.class(pane, el).split(' ').any(|c| c == "sf-md"),
            "{nid} drawn as {kind}"
        );
        let b = omp.window.doc(pane).border_box(el).expect("a box");
        assert!(b.w > 0.0 && b.h > 0.0, "{nid} has a box: {b:?}");
    }
    assert_eq!(
        omp.field_text(&id).as_deref(),
        Some(""),
        "the composer emptied after send"
    );

    // The table's chart: omp sends it as an `image` (D24), its SVG blob after the frame that
    // names it; the window draws it once the blob arrives.
    let reply_text: String = reply_md
        .iter()
        .filter_map(|(nid, _)| s.document().get(nid).and_then(|n| n.text()))
        .collect();
    let charts: Vec<String> = added_kinds(&omp.records())
        .into_iter()
        .filter(|(_, _, kind)| kind == "image")
        .map(|(_, nid, _)| nid)
        .filter(|nid| nid != "1.lockup/logo")
        .collect();
    eprintln!("reply: {reply_text:?}; chart images: {charts:?}");
    if reply_text.contains("| d | 40 |") {
        assert_eq!(charts.len(), 1, "omp charted the numeric table: {charts:?}");
    }
    for nid in &charts {
        let size = omp
            .window
            .wait(&format!("the chart {nid} to draw"), UI, |w| {
                let s = w.live_surface(pane, "s:1")?;
                image_drawn(w, pane, s, nid)
            });
        eprintln!("chart {nid}: {size:?}");
    }

    // A tool call, prompted with keys.
    omp.window.type_keys(
        pane,
        "Use the bash tool to run the command: echo auk-omp-check",
    );
    omp.wait_text(
        &id,
        "Use the bash tool to run the command: echo auk-omp-check",
    );
    omp.window.press(pane, Key::Enter, Mods::default());
    omp.wait_turns(2);
    let s = omp
        .window
        .live_surface(pane, "s:1")
        .expect("the session surface");
    let tools: Vec<_> = kinds_under(s.document().root())
        .into_iter()
        .filter(|(nid, k)| {
            k == "tool"
                && s.document().get(nid).and_then(|n| n.prop("name")) == Some(&Value::from("bash"))
        })
        .collect();
    assert!(!tools.is_empty(), "a bash tool card");
    for (nid, _) in &tools {
        let el = omp
            .window
            .element_of(pane, s, nid)
            .expect("the tool card is drawn");
        assert!(
            omp.window
                .class(pane, el)
                .split(' ')
                .any(|c| c == "sf-tool")
        );
        let b = omp.window.doc(pane).border_box(el).expect("a box");
        assert!(b.w > 0.0 && b.h > 0.0, "the tool card has a box: {b:?}");
        let text = omp.window.text_content(pane, el);
        assert!(
            text.contains("echo auk-omp-check"),
            "the card shows its command: {text:?}"
        );
    }

    omp.quit();
    assert!(
        omp.window.tsp(pane).surfaces().is_empty(),
        "discarded at the prompt"
    );

    let report = t2_report(&omp.records());
    eprintln!("errors: {:?}", report.errors);
    eprintln!("T2 kinds sent anyway: {:?}", report.sent_anyway);
    assert!(
        report.errors.is_empty(),
        "omp got errors: {:?}",
        report.errors
    );
    assert!(omp.window.errors().is_empty(), "{:?}", omp.window.errors());
}

/// T2 fallback under Auk's T0+T1 hello (§8.2, D22, D24), from omp's own record of a live start:
/// omp guards `effort` (a `row` of `meter` and `text` instead) and sends nothing unadvertised. Its
/// welcome logo, an `image` it sends without checking `supports()`, is T1 now: the welcome card
/// draws with omp's built-in mark, and omp gets no errors.
#[test]
fn omp_t2_fallback_under_t0_t1_hello() {
    let mut omp = LiveOmp::start("omp-t2", true);
    omp.ready();
    let pane = omp.pane;
    // The composer bar's effort widget.
    omp.window.wait("omp's effort widget", UI, |w| {
        let s = w.live_surface(pane, "s:1")?;
        s.document().get("3.bar/effort").map(|_| ())
    });
    let records = omp.records();
    let report = t2_report(&records);
    eprintln!("advertised: {:?}", report.advertised);
    eprintln!("errors: {:?}", report.errors);
    eprintln!("T2 kinds sent anyway: {:?}", report.sent_anyway);
    let expected: BTreeSet<String> = auk_tsp::T0_KINDS
        .iter()
        .chain(auk_tsp::T1_KINDS.iter())
        .map(|k| (*k).to_owned())
        .collect();
    assert_eq!(report.advertised, expected, "the hello lists exactly T0+T1");
    assert!(report.advertised.contains("image"), "image is T1 (D24)");
    assert!(
        report.sent_anyway.is_empty(),
        "omp 18.8.4 sends nothing unadvertised: {:?}",
        report.sent_anyway
    );
    assert!(
        report.errors.is_empty(),
        "omp got errors: {:?}",
        report.errors
    );
    // §8.4: none of the CSS effects the engine leaves undrawn.
    let undrawn: Vec<String> = program_css(&records).iter().flat_map(|c| undrawn_effects(c)).collect();
    assert!(undrawn.is_empty(), "omp sends undrawn effects: {undrawn:?}");
    let logos: Vec<String> = added_kinds(&records)
        .into_iter()
        .filter(|(_, _, kind)| kind == "image")
        .map(|(_, id, _)| id)
        .collect();
    assert_eq!(
        logos,
        ["1.lockup/logo"],
        "the welcome logo is omp's one image at start"
    );
    // The welcome card and its logo draw.
    let size = omp.window.wait("the welcome logo to draw", UI, |w| {
        let s = w.live_surface(pane, "s:1")?;
        s.document().get("1")?;
        image_drawn(w, pane, s, "1.lockup/logo")
    });
    eprintln!("welcome logo: {size:?}");
    let s = omp
        .window
        .live_surface(pane, "s:1")
        .expect("the session surface");
    let logo = s.document().get("1.lockup/logo").expect("the logo");
    assert_eq!(logo.prop("builtin"), Some(&Value::from("omp")));
    // `effort` fell back: the widget is a row of T1 kinds.
    let effort = s.document().get("3.bar/effort").expect("the effort widget");
    let kinds: BTreeSet<String> = kinds_under(effort).into_iter().map(|(_, k)| k).collect();
    assert!(
        kinds.iter().all(|k| expected.contains(k)),
        "effort fell back to {kinds:?}"
    );
    assert_eq!(effort.kind(), "row");
    assert!(omp.window.errors().is_empty(), "{:?}", omp.window.errors());
    omp.quit();
}

/// AC9 fallback: with TSP off in the host, omp's hello goes unanswered and omp runs as plain
/// text: its composer draws in the grid and takes typing; no surface opens and the daemon writes
/// no TSP to the pane.
#[test]
fn ac09_tsp_off_runs_as_plain_text() {
    let mut omp = LiveOmp::start("omp-plain", false);
    let pane = omp.pane;
    let marker = "plain text check";
    let env = &omp.env;
    omp.window
        .wait("omp to draw in the grid and read raw keys", UI, |w| {
            let text = w.try_replica(pane)?.screen_text();
            let tty = env.daemon(&format!("tty-mode {pane}"));
            (text.contains(OMP_VERSION) && tty["canonical"] == false && tty["echo"] == false)
                .then_some(())
        });
    omp.window.type_keys(pane, marker);
    omp.window
        .wait("the typed text in omp's grid composer", UI, |w| {
            w.try_replica(pane)?
                .screen_text()
                .contains(marker)
                .then_some(())
        });
    assert!(
        omp.window.tsp(pane).surfaces().is_empty(),
        "no surface opened"
    );
    let tsp = omp.env.daemon(&format!("tsp {pane}"));
    assert_eq!(tsp["enabled"], false, "{tsp}");
    assert_eq!(
        tsp["gate_stats"]["written"], 0,
        "nothing written to the pane: {tsp}"
    );
    // omp starts its TSP backend (and its record) only on a hello reply.
    assert!(
        !omp.log.exists(),
        "omp's TSP backend started without a hello reply: {:?}",
        std::fs::read_to_string(&omp.log)
    );
    // ⌃C clears the composer, ⌃D quits.
    omp.window.press(pane, Key::Char('c'), ctrl());
    let cleared = omp.window.wait("omp to redraw after ⌃C", UI, |w| {
        let text = w.try_replica(pane)?.screen_text();
        (!text.contains(marker)).then_some(text)
    });
    assert!(
        cleared.contains(OMP_VERSION),
        "omp still draws in the grid: {cleared}"
    );
    omp.quit();
}
