//! The omp suite (GOAL.md row `omp`, spec §7, §8.2, M5, AC9): omp, the main TSP client, against
//! the real daemon and a window that draws, acks and edits natively.
//!
//! - **Replay:** `fixtures/tsp/omp-session.jsonl` (omp 18.8.5 on three prompts) is played into a
//!   daemon pane with `surface-play`; the window's replica projects it through `auk-views` into
//!   `auk-ui`, and the transcript, tool cards and diffs are drawn as their kinds.
//! - **Live:** the installed omp runs in a hermetic daemon pane: a zsh with the test `ZDOTDIR`, in
//!   a temporary cwd, with `HOME` pointing at the user's real home only for omp's login. The
//!   window is the pane's primary. Live turns spend real API credits, so there are two tiny
//!   prompts in one test, and every assertion checks structure, never model wording.
//! - **Fallback:** with TSP off, omp runs as plain text. Under Auk's T0+T1 hello (D22), omp's own
//!   record of what it sent (`PI_TUI_TSP_RECORD`) shows which T2 kinds it sends anyway, which it
//!   replaces, and which ops the daemon rejected as a result.
//! - **Version:** every test first checks that the installed omp is the recorded 18.8.5.

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
const OMP_VERSION: &str = "18.8.5";

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

/// The installed omp (`~/.local/bin/omp`), checked to be the recorded version. Fails clearly
/// otherwise.
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

/// How [`LiveOmp::start_with`] sets omp up.
#[derive(Default)]
struct Setup {
    /// The window's appearance is dark (its facts carry the dark theme's colors).
    dark: bool,
    /// A shell line run (to its next prompt) before omp starts.
    before: Option<String>,
    /// omp's arguments in place of `--no-session` (already quoted).
    args: Option<String>,
    /// The hello lists only T0 and T1 (host setting `t2-kinds off`).
    t1_only: bool,
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
        LiveOmp::start_with(name, tsp, Setup::default())
    }

    /// [`LiveOmp::start`] with the window's appearance, a shell line run first, and omp's
    /// arguments (`--no-session` by default).
    fn start_with(name: &str, tsp: bool, setup: Setup) -> LiveOmp {
        let omp = omp();
        let env = TestEnv::new(env!("CARGO_BIN_EXE_auk"), name);
        env.start_daemon();
        if !tsp {
            env.daemon("settings --native-surfaces off");
        }
        if setup.t1_only {
            env.daemon("settings --t2-kinds off");
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
        if setup.dark {
            window.set_dark(true);
        }
        window.wait("the window to become the pane's primary", UI, |w| {
            (w.is_primary(pane)
                && w.try_replica(pane)
                    .is_some_and(|r| r.terminal().cols() == SIZE.0))
            .then_some(())
        });
        let mut prompts = prompts;
        if let Some(before) = &setup.before {
            window.input(pane, format!("{before}\r").as_bytes());
            let done = env.daemon(&format!(
                "wait {pane} --until prompt --timeout {}",
                UI.as_secs()
            ));
            prompts = done["prompts"].as_u64().expect("a prompt count");
            assert_eq!(prompts, ready["prompts"].as_u64().unwrap() + 1, "{done}");
        }
        let args = setup.args.as_deref().unwrap_or("--no-session");
        let line = format!(
            "HOME={} EDITOR={} PI_TUI_TSP_RECORD={} {} {args} --no-title\r",
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
        self.quit_done();
    }

    /// Waits for the shell's next prompt after omp was told to quit.
    fn quit_done(&mut self) {
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
/// and 3D transforms among the other accepted-unused properties of
/// `auk-ui`'s `properties.rs`).
const UNDRAWN: [&str; 13] = [
    "filter",
    "backdrop-filter",
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
                || (name == "transform"
                    && (value.contains("3d(") || value.contains("perspective(")));
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
    let all: Vec<String> = auk_tsp::T0_KINDS
        .iter()
        .chain(auk_tsp::T1_KINDS.iter())
        .chain(auk_tsp::T2_KINDS.iter())
        .map(|k| (*k).to_owned())
        .collect();
    t2_report_for(records, &all)
}

/// [`t2_report`] for a hello that advertised `kinds` (when omp didn't record the reply).
fn t2_report_for(records: &[Value], kinds: &[String]) -> T2Report {
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
        // omp 18.8.5 doesn't record the hello reply; the kinds it saw are the daemon's (D22).
        .unwrap_or_else(|| kinds.iter().cloned().collect());
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
    let fixture = auk_conformance::fixtures_dir().join("omp-session.jsonl");
    let recorded_frames = auk_conformance::load(&fixture)
        .expect("load the recording")
        .iter()
        .filter(|r| r.dir == auk_conformance::Dir::Out && r.verb == "f")
        .count() as u64;
    // Everything but omp's final close: the session stays live, `dock` and `layer` included. (Not
    // `until` the last frame: the chart's blob comes after the frame that names it, and omp's
    // last frame can be that one.)
    let path = env.root.join("omp-session-open.jsonl");
    let open: String = std::fs::read_to_string(&fixture)
        .expect("read the recording")
        .lines()
        .filter(|l| {
            let r: Value = serde_json::from_str(l).expect("a record");
            !(r["dir"] == "out" && r["verb"] == "x")
        })
        .map(|l| format!("{l}\n"))
        .collect();
    std::fs::write(&path, open).expect("write the recording without its close");
    let played = env.daemon(&format!(
        "surface-play --pane {pane} {}",
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
    // and the chart of a numeric table (the `ls -la` summary) is an SVG blob sent after the frame
    // that names it, in an assistant message. Its id depends on the model's reply, so it's found
    // as the recording's one other image.
    assert!(doc.get("1").is_some(), "the welcome card arrived");
    let logo = doc.get("1.lockup/logo").expect("the welcome logo");
    assert_eq!(logo.prop("builtin"), Some(&Value::from("omp")));
    let charts: Vec<&str> = all
        .iter()
        .filter(|(id, k)| k == "image" && id != "1.lockup/logo")
        .map(|(id, _)| id.as_str())
        .collect();
    assert_eq!(charts.len(), 1, "one chart in the recording: {charts:?}");
    let chart = doc.get(charts[0]).expect("the table's chart");
    let blob = chart.prop("blob").and_then(Value::as_str).expect("a blob");
    assert!(tsp.blobs().has(blob), "the chart's blob reached the window");
    assert!(
        std::iter::successors(chart.parent(), |p| p.parent())
            .any(|p| p.prop("role") == Some(&Value::from("omp.assistant"))),
        "the chart is in an assistant message"
    );
    for id in ["1.lockup/logo", charts[0]] {
        let size = image_drawn(&window, pane, s, id)
            .unwrap_or_else(|| panic!("{id} isn't drawn as a picture"));
        eprintln!("image {id}: {size:?}");
    }

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
/// text in the composer. omp's settings (T2 `prefs`, not advertised yet) open as a screen surface
/// built from T1 kinds; its model hub is a `picker` sheet now (A23, tested in
/// `a23_omps_model_hub_and_model_dropdown_are_picker_sheets`). ⌃D quits omp, and the shell's
/// next prompt discards the inline surface.
/// §8.4: the effects the engine accepts but doesn't draw (filters, backdrop-filter, masks, 3D
/// transforms) are only a gap if omp sends them. It doesn't: neither recording holds a
/// stylesheet or an `el` style that names one, and after the replay through the daemon, the
/// window's surfaces hold no program sheet that does. (The live T2 test checks omp's own record.)
#[test]
fn omp_sends_none_of_the_css_effects_the_engine_leaves_undrawn() {
    assert_eq!(
        undrawn_effects(
            ".a { filter: blur(2px); box-shadow: 0 1px red; color: red } .b{transform: rotate3d(1,0,0,9deg)} .c { mask-image: none; transform: rotate(3deg) }"
        ),
        [
            "filter: blur(2px)",
            "transform: rotate3d(1,0,0,9deg)",
            "mask-image: none"
        ],
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
        assert!(
            found.is_empty(),
            "{name}: omp sends undrawn effects: {found:?}"
        );
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

    // `/settings` and `/model` open as T2 sheets in the session's layer: see the A23 tests (and
    // `omp_t2_fallback_under_t0_t1_hello` for their screen surfaces under a T0+T1 hello).

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

    // A numeric table with four rows: omp 18.8.5 draws a chart of such a table as an `image`.
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
        // The thinking block's body is in a collapsed section when the model thought: drawn,
        // but not laid out until expanded.
        let doc = omp.window.doc(pane);
        let collapsed = std::iter::successors(doc.parent(el), |&a| doc.parent(a)).any(|a| {
            omp.window
                .class(pane, a)
                .split(' ')
                .any(|c| c == "collapsed")
        });
        if collapsed {
            continue;
        }
        let b = doc.border_box(el).expect("a box");
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

/// T2 fallback under an explicitly T0+T1 hello (host setting `t2-kinds off`; §8.2, D22, D24, A23),
/// from omp's own record of a live start: omp guards `effort` (a `row` of `meter` and `text`
/// instead) and sends nothing unadvertised. Its welcome logo, an `image` it sends without checking
/// `supports()`, is T1: the welcome card draws with omp's built-in mark, and omp gets no errors.
/// Without `prefs`, `/settings` is a screen surface over the session, built from T0+T1 kinds.
#[test]
fn omp_t2_fallback_under_t0_t1_hello() {
    let mut omp = LiveOmp::start_with(
        "omp-t2",
        true,
        Setup {
            t1_only: true,
            ..Setup::default()
        },
    );
    let id = omp.ready();
    let pane = omp.pane;
    // The composer bar's effort widget.
    omp.window.wait("omp's effort widget", UI, |w| {
        let s = w.live_surface(pane, "s:1")?;
        s.document().get("3.bar/effort").map(|_| ())
    });
    let config = omp.window.tsp(pane).config().clone();
    assert!(
        !config.features.iter().any(|f| f == "aside"),
        "no prefs, so no aside"
    );
    let records = omp.records();
    let report = t2_report_for(&records, &config.kinds);
    eprintln!("advertised: {:?}", report.advertised);
    eprintln!("errors: {:?}", report.errors);
    eprintln!("T2 kinds sent anyway: {:?}", report.sent_anyway);
    let expected: BTreeSet<String> = auk_tsp::T0_KINDS
        .iter()
        .chain(auk_tsp::T1_KINDS.iter())
        .map(|k| (*k).to_owned())
        .collect();
    let hello: BTreeSet<String> = config.kinds.iter().cloned().collect();
    assert_eq!(hello, expected, "the hello lists exactly T0 and T1");
    assert_eq!(report.advertised, expected);
    assert!(report.advertised.contains("image"), "image is T1 (D24)");
    assert!(
        report.sent_anyway.is_empty(),
        "omp 18.8.5 sends nothing unadvertised: {:?}",
        report.sent_anyway
    );
    assert!(
        report.errors.is_empty(),
        "omp got errors: {:?}",
        report.errors
    );
    // §8.4: none of the CSS effects the engine leaves undrawn.
    let undrawn: Vec<String> = program_css(&records)
        .iter()
        .flat_map(|c| undrawn_effects(c))
        .collect();
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

    // Without `prefs`, `/settings` is a screen surface over the session, from advertised kinds.
    for (command, role) in [("/settings", "omp.overlay.settings")] {
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
            assert!(!used.contains(k), "{command} sent unadvertised T2 kind {k}");
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

    let report = t2_report_for(&omp.records(), &config.kinds);
    assert!(report.sent_anyway.is_empty(), "{:?}", report.sent_anyway);
    assert!(
        report.errors.is_empty(),
        "omp got errors: {:?}",
        report.errors
    );
    assert!(omp.window.errors().is_empty(), "{:?}", omp.window.errors());
    omp.clear(&id);
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

/// The text of the toast the pane's toast layer shows, if any.
fn shown_toast(w: &NativeWindow, pane: u64) -> Option<String> {
    let doc = w.doc(pane);
    let host = doc
        .element_children(w.view(pane).element())
        .into_iter()
        .find(|&e| w.class(pane, e).split(' ').any(|c| c == "auk-toasts"))
        .expect("the pane's toast layer");
    let toast = *doc.element_children(host).first()?;
    Some(w.text_content(pane, toast))
}

#[test]
fn omp_closing_toast_shows_for_its_ttl_then_goes() {
    // Quitting omp shows its status toast "Closing session…" (ttl 2.4 s) in the burst that
    // closes its surface with `keep`. The window shows it once and drops it on time, while the
    // shell takes the pane back.
    let mut omp = LiveOmp::start("omp-closing", true);
    omp.ready();
    let pane = omp.pane;
    omp.window.ctrl(pane, 'd');
    let start = std::time::Instant::now();
    let mut first = None;
    let mut last = None;
    while start.elapsed() < Duration::from_secs(8) {
        omp.window.frame();
        if let Some(text) = shown_toast(&omp.window, pane) {
            assert!(text.ends_with("Closing session…"), "{text:?}");
            first.get_or_insert(start.elapsed());
            last = Some(start.elapsed());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let (first, last) = (
        first.expect("omp's closing toast shows"),
        last.expect("shown"),
    );
    let shown = last - first;
    assert!(
        shown >= Duration::from_millis(2000) && shown < Duration::from_millis(2900),
        "the toast shows for its ttl (2.4 s), not {shown:?}"
    );
    // What omp sent: the toast with its ttl, and the close with `keep`, after it.
    let records = omp.records();
    let at = |pred: &dyn Fn(&Value) -> bool| records.iter().position(pred);
    let toast = at(&|r: &Value| {
        r["dir"] == "out"
            && r["verb"] == "f"
            && r["body"]["ops"].as_array().is_some_and(|ops| {
                ops.iter().any(|op| {
                    op[0] == "add"
                        && op[4]["k"] == "toast"
                        && op[4]["p"]["text"] == "Closing session…"
                        && op[4]["p"]["ttl"] == 2400
                })
            })
    })
    .expect("omp sent its closing toast");
    let close = at(&|r: &Value| r["dir"] == "out" && r["verb"] == "x" && r["body"]["keep"] == true)
        .expect("omp closed its surface with keep");
    assert!(toast < close);
    omp.quit_done();
}

/// An omp session file of `turns` short question-and-answer turns, written into `dir`, for omp
/// to resume: a transcript many panes long, with no model call.
fn long_session(dir: &Path, cwd: &Path, turns: usize) -> PathBuf {
    let id = "01a20000-0000-7000-8000-00000000a16a";
    let at = |i: usize| format!("2026-10-08T07:{:02}:{:02}.000Z", i / 60 % 60, i % 60);
    let mut lines = vec![
        serde_json::json!({"type": "session", "version": 3, "id": id, "timestamp": at(0), "cwd": cwd}),
        serde_json::json!({"type": "model_change", "id": "m0000000", "parentId": null, "timestamp": at(0),
                           "model": "xai-oauth/grok-4.7", "resolvedModelIsFallback": false}),
    ];
    let mut parent = "m0000000".to_owned();
    for t in 0..turns {
        let (u, a) = (format!("u{t:07}"), format!("a{t:07}"));
        let answer: Vec<String> = (0..8)
            .map(|l| format!("- Point {l} of answer {t}."))
            .collect();
        lines.push(serde_json::json!({"type": "message", "id": u, "parentId": parent, "timestamp": at(2 * t + 1),
            "message": {"role": "user", "content": [{"type": "text", "text": format!("Question {t}?")}],
                        "attribution": "user", "timestamp": 1_791_445_349_000u64 + t as u64 * 2000}}));
        lines.push(serde_json::json!({"type": "message", "id": a, "parentId": u, "timestamp": at(2 * t + 2),
            "message": {"role": "assistant", "content": [{"type": "text", "text": format!("## Answer {t}\n\n{}", answer.join("\n"))}],
                        "api": "openai-responses", "provider": "xai-oauth", "model": "grok-4.7",
                        "usage": {"input": 10, "output": 10, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 20,
                                  "cost": {"input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0, "total": 0.0}},
                        "stopReason": "stop", "timestamp": 1_791_445_350_000u64 + t as u64 * 2000}}));
        parent = a;
    }
    let path = dir.join(format!("2026-10-08T07-00-00-000Z_{id}.jsonl"));
    let text: Vec<String> = lines.iter().map(Value::to_string).collect();
    std::fs::write(&path, text.join("\n") + "\n").expect("write the session");
    path
}

/// Renders the part `(x, y, w, h)` of a window's document offscreen, as the app's UI layer draws
/// it.
fn render_part(doc: &auk_ui::Document, (x, y, w, h): (f32, f32, u32, u32)) -> auk_render::Image {
    use auk_render::{Gpu, OFFSCREEN_FORMAT, OffscreenTarget, UiLayer, wgpu};
    let gpu = Gpu::new_blocking().expect("a Metal adapter");
    let mut scene = auk_render::vello::Scene::new();
    auk_ui::paint::paint_with_transform(
        doc,
        &mut scene,
        auk_render::vello::kurbo::Affine::translate((-f64::from(x), -f64::from(y))),
    );
    let mut ui = UiLayer::new(&gpu, OFFSCREEN_FORMAT).expect("a UI layer");
    ui.render(&gpu, &scene, w, h).expect("the UI layer renders");
    let target = OffscreenTarget::new(&gpu, w, h);
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("window part"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target.view(),
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        ui.draw(&mut pass);
    }
    gpu.queue.submit([encoder.finish()]);
    target.read(&gpu).expect("readback")
}

/// The first element under `el` (itself included) whose written class list has `class`.
fn first_with_class(
    w: &NativeWindow,
    pane: u64,
    el: auk_ui::NodeId,
    class: &str,
) -> auk_ui::NodeId {
    w.with_class(pane, el, class)
        .first()
        .copied()
        .unwrap_or_else(|| panic!("no .{class}"))
}

/// Amendments A15-A19 on a live omp 18.8.5, with no prompt: omp resumes a session file many
/// panes long (no model call), in a dark window. Its Switch Model sheet (opened by clicking the
/// composer's model control, as a user does; a `picker` since A23) sits in the pane's visible
/// area and lays out its list (A16), drawn over its dim, not under it (A15); a window focus
/// change doesn't replay the sheet-in (A17). Its Usage overlay (the cost control) takes a click on its
/// Details tab, which omp makes active, and the card omp's new props build doesn't pop again.
/// The pane's OSC 11 answer is the dark theme's background and its TSP facts say dark (A19).
/// Esc closes each overlay; nothing is picked, and omp's config is left as it was.
#[test]
fn a15_to_a19_omps_overlays_over_a_long_transcript_in_the_dark() {
    let config = real_home().join(".omp/agent/config.yml");
    let config_before = std::fs::read(&config).ok();
    let dir = std::env::temp_dir().join(format!("auk-omp-a16-{}", std::process::id()));
    let sessions = dir.join("sessions");
    std::fs::create_dir_all(&sessions).expect("a session dir");
    // Asks OSC 11 from the pane's shell as omp does at its start, and keeps the answer.
    let probe = dir.join("osc11.py");
    std::fs::write(
        &probe,
        "import os, select, sys, termios, tty\n\
         fd = os.open('/dev/tty', os.O_RDWR)\n\
         old = termios.tcgetattr(fd)\n\
         tty.setraw(fd)\n\
         os.write(fd, b'\\x1b]11;?\\x07')\n\
         buf = b''\n\
         while not buf.endswith(b'\\x07') and select.select([fd], [], [], 5)[0]:\n\
         \x20   buf += os.read(fd, 64)\n\
         termios.tcsetattr(fd, termios.TCSADRAIN, old)\n\
         open(sys.argv[1], 'wb').write(buf)\n",
    )
    .expect("write the OSC 11 probe");
    let answer = dir.join("osc11.txt");
    let session = long_session(&sessions, &dir, 40);
    let mut omp = LiveOmp::start_with(
        "omp-a16",
        true,
        Setup {
            dark: true,
            before: Some(format!(
                "python3 -I {} {}",
                quote(&probe.to_string_lossy()),
                quote(&answer.to_string_lossy())
            )),
            args: Some(format!(
                "--session-dir {} --resume {}",
                quote(&sessions.to_string_lossy()),
                quote(&session.to_string_lossy())
            )),
            ..Setup::default()
        },
    );
    let pane = omp.pane;
    omp.ready();

    // A19: the dark window's background answers OSC 11, and the facts omp hears are dark.
    let osc11 = std::fs::read(&answer).expect("the OSC 11 answer");
    assert_eq!(
        String::from_utf8_lossy(&osc11),
        "\x1b]11;rgb:1616/1818/1a1a\x07",
        "OSC 11 answers the dark theme's background"
    );
    assert_eq!(
        omp.env.daemon(&format!("tsp {pane}"))["facts"]["dark"],
        true
    );

    // The resumed transcript is many viewports long (omp may still be sending it after `ready`).
    let viewport = omp.window.doc(pane).viewport();
    omp.window
        .wait("the resumed transcript to lay out", UI, |w| {
            let page = w.view(pane).element();
            let long = w.doc(pane).border_box(page)?;
            (long.h > viewport.1 * 2.0).then_some(())
        });
    let page = omp.window.view(pane).element();

    // Clicking the composer's model control opens omp's Switch Model sheet: with `picker`
    // advertised (A23) a `picker` in the layer, where A15-A17 hold for it as for an overlay.
    let model = omp.with_role("omp.composer.model");
    assert_eq!(model.len(), 1, "omp's model control: {model:?}");
    let s = omp
        .window
        .live_surface(pane, "s:1")
        .expect("the session surface");
    let el = omp.window.element_of(pane, s, &model[0].0).expect("drawn");
    let b = omp.window.doc(pane).border_box(el).expect("laid out");
    let out = omp.window.click(pane, b.x + b.w / 2.0, b.y + b.h / 2.0);
    assert!(
        out.iter().any(|o| matches!(o, Outcome::Event { .. })),
        "the click goes to omp: {out:?}"
    );
    let pk = omp.window.wait("omp's model sheet", UI, |w| {
        let s = w.live_surface(pane, "s:1")?;
        let kinds = kinds_under(s.document().region("layer")?);
        Some(kinds.iter().find(|(_, k)| k == "picker")?.0.clone())
    });
    let s = omp
        .window
        .live_surface(pane, "s:1")
        .expect("the session surface");
    let pk_el = omp.window.element_of(pane, s, &pk).expect("drawn");
    let card = first_with_class(&omp.window, pane, pk_el, "pk-sheet");
    // Its sheet-in finishes.
    std::thread::sleep(Duration::from_millis(300));
    omp.window.frame();
    let opacity = |w: &NativeWindow| w.doc(pane).computed_style(card).map(|c| c.opacity);
    assert_eq!(opacity(&omp.window), Some(1.0), "the sheet-in finished");

    // A16: the sheet lies in the pane's visible area (the layer, one viewport from the pane's
    // top), not ~a transcript's length down.
    let doc = omp.window.doc(pane);
    let layers = first_with_class(&omp.window, pane, page, "auk-layers");
    let l = doc.border_box(layers).expect("the layer laid out");
    let c = doc.border_box(card).expect("the sheet laid out");
    assert!(
        (l.h - viewport.1).abs() < 1.0,
        "the layer is one viewport tall: {l:?}"
    );
    assert!(
        c.h > 0.0 && c.y >= l.y && c.y + c.h <= l.y + l.h,
        "the sheet {c:?} lies in the visible area {l:?}"
    );
    // Its list lays out the rows it shows.
    let rows = omp.window.with_class(pane, pk_el, "pk-row");
    assert!(rows.len() >= 2, "models to pick from: {}", rows.len());
    assert!(doc.laid_out(rows[0]), "the list's first row is laid out");

    // A15: the sheet draws over its dim, not under it: a pixel of its action bar's empty
    // middle is the sheet's own color, not dimmed.
    let bg = doc.computed_style(card).expect("styled").background_color;
    assert_eq!(bg.a, 1.0, "an opaque sheet");
    let want = [bg.r, bg.g, bg.b].map(|v| (v * 255.0).round() as u8);
    let fill = first_with_class(&omp.window, pane, pk_el, "pk-bar-fill");
    let f = doc.border_box(fill).expect("laid out");
    let (x0, y0) = (c.x.floor(), c.y.floor());
    let px = render_part(doc, (x0, y0, c.w.ceil() as u32 + 2, c.h.ceil() as u32 + 2));
    let p = px.pixel((f.x + f.w / 2.0 - x0) as u32, (f.y + 4.0 - y0) as u32);
    assert!(
        [p.0, p.1, p.2]
            .iter()
            .zip(want)
            .all(|(a, b)| a.abs_diff(b) <= 3),
        "the sheet's background {p:?} is its color {want:?}, not dimmed"
    );
    // Clicks land in the sheet, not the dim (A15's hit order).
    let hit = auk_ui::hit_test(doc, f.x + f.w / 2.0, f.y + 4.0).expect("a hit");
    assert!(
        std::iter::successors(Some(hit), |&e| doc.parent(e)).any(|e| e == card),
        "the press lands in the sheet"
    );

    // A17: a window focus change restyles the sheet without replaying its sheet-in.
    omp.window.set_focused(pane, false);
    assert_eq!(opacity(&omp.window), Some(1.0), "no replay on blur");
    omp.window.set_focused(pane, true);
    omp.window.frame();
    assert_eq!(opacity(&omp.window), Some(1.0), "no replay on focus");

    // Esc closes it, using no model.
    omp.window.press(pane, Key::Escape, Mods::default());
    omp.window.wait("the model sheet to close", UI, |w| {
        let s = w.live_surface(pane, "s:1")?;
        s.document().get(&pk).is_none().then_some(())
    });

    // omp's Usage overlay (its cost control): a click on its Details tab reaches omp, which
    // makes it the active tab; the card built again with the new props doesn't pop again.
    let usage = omp.with_role("omp.composer.usage");
    assert_eq!(usage.len(), 1, "omp's cost control: {usage:?}");
    let s = omp
        .window
        .live_surface(pane, "s:1")
        .expect("the session surface");
    let el = omp.window.element_of(pane, s, &usage[0].0).expect("drawn");
    let b = omp.window.doc(pane).border_box(el).expect("laid out");
    omp.window.click(pane, b.x + b.w / 2.0, b.y + b.h / 2.0);
    let (ov, tabs) = omp.window.wait("omp's usage overlay", UI, |w| {
        let s = w.live_surface(pane, "s:1")?;
        let kinds = kinds_under(s.document().root());
        let ov = kinds.iter().find(|(_, k)| k == "overlay")?.0.clone();
        let tabs = kinds_under(s.document().get(&ov)?)
            .into_iter()
            .find(|(_, k)| k == "tabs")?
            .0;
        Some((ov, tabs))
    });
    let s = omp
        .window
        .live_surface(pane, "s:1")
        .expect("the session surface");
    let active = |s: &auk_tsp::Surface| {
        s.document()
            .get(&tabs)
            .and_then(|n| n.prop("active"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    assert_eq!(active(s).as_deref(), Some("overview"));
    let ov_el = omp.window.element_of(pane, s, &ov).expect("drawn");
    let tabs_el = omp.window.element_of(pane, s, &tabs).expect("drawn");
    std::thread::sleep(Duration::from_millis(300));
    omp.window.frame();
    let card = first_with_class(&omp.window, pane, ov_el, "sf-ov-card");
    let c = omp.window.doc(pane).border_box(card).expect("laid out");
    assert!(
        c.y >= l.y && c.y + c.h <= l.y + l.h,
        "the usage card {c:?} lies in the visible area {l:?}"
    );
    // The innermost element reading "Details" is the tab.
    let doc = omp.window.doc(pane);
    let mut tab = None;
    let mut stack = vec![tabs_el];
    while let Some(n) = stack.pop() {
        if doc.node(n).is_element() && omp.window.text_content(pane, n).trim() == "Details" {
            tab = Some(n);
        }
        stack.extend(doc.children(n).iter().rev());
    }
    let r = doc
        .border_box(tab.expect("the Details tab"))
        .expect("laid out");
    let out = omp.window.click(pane, r.x + r.w / 2.0, r.y + r.h / 2.0);
    assert!(
        out.iter().any(|o| matches!(o, Outcome::Event { .. })),
        "the tab's click goes to omp: {out:?}"
    );
    omp.window
        .wait("omp to make Details the active tab", UI, |w| {
            let s = w.live_surface(pane, "s:1")?;
            (active(s).as_deref() == Some("detail")).then_some(())
        });
    let card = first_with_class(&omp.window, pane, ov_el, "sf-ov-card");
    assert_eq!(
        omp.window.doc(pane).computed_style(card).map(|c| c.opacity),
        Some(1.0),
        "no replay on new props"
    );

    // Esc goes back to the first tab, then closes it; omp quits; its config is as it was.
    let open = |w: &NativeWindow| {
        w.live_surface(pane, "s:1")
            .is_some_and(|s| s.document().get(&ov).is_some())
    };
    for _ in 0..3 {
        if !open(&omp.window) {
            break;
        }
        omp.window.press(pane, Key::Escape, Mods::default());
        let start = std::time::Instant::now();
        while open(&omp.window) && start.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(25));
            omp.window.frame();
        }
    }
    assert!(!open(&omp.window), "Esc closes the usage overlay");
    omp.quit();
    assert_eq!(
        std::fs::read(&config).ok(),
        config_before,
        "omp's config is unchanged"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The `picker` in the session's layer, if one is open: its node id.
fn open_picker(w: &NativeWindow, pane: u64) -> Option<String> {
    let s = w.live_surface(pane, "s:1")?;
    let kinds = kinds_under(s.document().region("layer")?);
    Some(kinds.into_iter().find(|(_, k)| k == "picker")?.0)
}

/// A prop of the open picker.
fn picker_prop(w: &NativeWindow, pane: u64, id: &str, prop: &str) -> Option<Value> {
    let s = w.live_surface(pane, "s:1")?;
    s.document().get(id)?.prop(prop).cloned()
}

/// Amendment A23 (`picker`, T2) on a live omp 18.8.5 with no prompt. Advertised, `picker`
/// changes both of omp's model sheets. `/model` opens the model hub as a `picker` in the session's
/// layer (not a screen surface): scopes, tabs and rows draw in the visible area; typed keys filter
/// it (drawn as the head's query, keys stay omp's); Down moves the selection, which the view
/// shows; Enter opens the role strip, drawn above the action bar and in view with the selected
/// chip ringed (`focus:"strip"`); Right moves the chip; Esc cancels. A click on a tab reaches omp,
/// which makes it active. The composer's model control opens the md "Switch model" sheet (72px
/// from the top, at most 680 wide); a click on a row selects it. Esc closes both; nothing is
/// assigned or switched, and omp's config is unchanged.
#[test]
fn a23_omps_model_hub_and_model_dropdown_are_picker_sheets() {
    let config = real_home().join(".omp/agent/config.yml");
    let config_before = std::fs::read(&config).ok();
    let mut omp = LiveOmp::start("omp-picker", true);
    let id = omp.ready();
    let pane = omp.pane;
    let page = omp.window.view(pane).element();

    // `/model`: the hub, as a picker in the session's layer.
    omp.window.type_keys(pane, "/model");
    omp.wait_text(&id, "/model");
    omp.window.press(pane, Key::Enter, Mods::default());
    let pk = omp.window.wait("omp's model hub", UI, |w| {
        let pk = open_picker(w, pane)?;
        picker_prop(w, pane, &pk, "scopes")?;
        Some(pk)
    });
    assert!(
        omp.window
            .tsp(pane)
            .surfaces()
            .iter()
            .all(|s| !s.is_live() || s.mode() != auk_tsp::Mode::Screen),
        "no screen surface"
    );
    std::thread::sleep(Duration::from_millis(300));
    omp.window.frame();
    let s = omp.window.live_surface(pane, "s:1").expect("the session");
    let pk_el = omp.window.element_of(pane, s, &pk).expect("drawn");
    let class = omp.window.class(pane, pk_el);
    assert!(
        class.contains("sf-picker") && class.contains("sz-lg"),
        "{class}"
    );
    let layers = first_with_class(&omp.window, pane, page, "auk-layers");
    let doc = omp.window.doc(pane);
    let l = doc.border_box(layers).expect("the layer");
    let sheet = first_with_class(&omp.window, pane, pk_el, "pk-sheet");
    let sh = doc.border_box(sheet).expect("the sheet");
    assert!(
        sh.y >= l.y && sh.y + sh.h <= l.y + l.h + 0.5 && sh.w <= 1120.5,
        "the sheet {sh:?} in the visible area {l:?}"
    );
    assert!(
        !omp.window.with_class(pane, pk_el, "pk-scope").is_empty(),
        "scopes"
    );
    assert!(
        !omp.window.with_class(pane, pk_el, "pk-tab").is_empty(),
        "tabs"
    );
    let rows = omp.window.with_class(pane, pk_el, "pk-row");
    assert!(rows.len() >= 2, "rows: {}", rows.len());

    // Typing filters: the query is omp's, drawn in the head.
    omp.window.type_keys(pane, "son");
    omp.window.wait("omp to take the query", UI, |w| {
        (picker_prop(w, pane, &pk, "query")? == "son").then_some(())
    });
    omp.window.frame();
    let q = first_with_class(&omp.window, pane, pk_el, "q");
    assert_eq!(omp.window.text_content(pane, q), "son");
    // Down moves omp's selection; the view shows it.
    let before = picker_prop(&omp.window, pane, &pk, "selected");
    omp.window.press(pane, Key::Down, Mods::default());
    let sel = omp.window.wait("the selection to move", UI, |w| {
        let now = picker_prop(w, pane, &pk, "selected")?;
        (Some(&now) != before.as_ref()).then_some(now)
    });
    omp.window.frame();
    let shown_sel: Vec<String> = omp
        .window
        .with_class(pane, pk_el, "sel")
        .into_iter()
        .filter_map(|e| {
            omp.window
                .doc(pane)
                .attribute(e, "data-v")
                .map(str::to_owned)
        })
        .collect();
    assert_eq!(shown_sel, [sel.as_str().unwrap().to_owned()]);

    // Enter opens the role strip: in view, above the bar, its selected chip ringed.
    omp.window.press(pane, Key::Enter, Mods::default());
    omp.window.wait("the role strip", UI, |w| {
        let strip = picker_prop(w, pane, &pk, "strip")?;
        (strip.is_object() && picker_prop(w, pane, &pk, "focus")? == "strip").then_some(())
    });
    omp.window.frame();
    let doc = omp.window.doc(pane);
    let strip = first_with_class(&omp.window, pane, pk_el, "pk-strip");
    let bar = first_with_class(&omp.window, pane, pk_el, "pk-bar");
    let (st, b) = (doc.border_box(strip).unwrap(), doc.border_box(bar).unwrap());
    assert!(
        st.h > 0.0 && (st.y + st.h - b.y).abs() < 1.0 && b.y + b.h <= l.y + l.h + 0.5,
        "the strip {st:?} sits on the bar {b:?} in view {l:?}"
    );
    let chips = omp.window.with_class(pane, strip, "pk-schip");
    let n = picker_prop(&omp.window, pane, &pk, "strip").unwrap()["items"]
        .as_array()
        .map_or(0, Vec::len);
    assert_eq!(chips.len(), n);
    let sheet = first_with_class(&omp.window, pane, pk_el, "pk-sheet");
    assert!(omp.window.class(pane, sheet).contains("f-strip"));
    let ringed = |w: &NativeWindow| {
        let strip = first_with_class(w, pane, pk_el, "pk-strip");
        w.with_class(pane, strip, "pk-schip")
            .iter()
            .position(|&c| w.class(pane, c).split(' ').any(|x| x == "sel"))
    };
    let at = ringed(&omp.window).expect("a selected chip");
    omp.window.press(pane, Key::Right, Mods::default());
    omp.window.wait("the strip's chip to move", UI, |w| {
        (ringed(w)? != at).then_some(())
    });
    // Esc cancels the strip: nothing assigned.
    omp.window.press(pane, Key::Escape, Mods::default());
    omp.window.wait("the strip to close", UI, |w| {
        picker_prop(w, pane, &pk, "strip")
            .is_none_or(|s| s.is_null())
            .then_some(())
    });

    // A click on a tab reaches omp, which makes it active.
    omp.window.frame();
    let tabs = omp.window.with_class(pane, pk_el, "pk-tab");
    let (tab_el, tab_id) = tabs
        .iter()
        .filter(|&&t| !omp.window.class(pane, t).split(' ').any(|c| c == "on"))
        .find_map(|&t| {
            let v = omp.window.doc(pane).attribute(t, "data-v")?.to_owned();
            Some((t, v))
        })
        .expect("an inactive tab");
    let r = omp.window.doc(pane).border_box(tab_el).unwrap();
    let out = omp.window.click(pane, r.x + r.w / 2.0, r.y + r.h / 2.0);
    assert!(
        out.iter().any(|o| matches!(o, Outcome::Event { .. })),
        "{out:?}"
    );
    omp.window.wait("omp to make the tab active", UI, |w| {
        (picker_prop(w, pane, &pk, "tab")? == tab_id.as_str()).then_some(())
    });

    // Esc (clearing the query first) closes the hub.
    for _ in 0..4 {
        if open_picker(&omp.window, pane).is_none() {
            break;
        }
        omp.window.press(pane, Key::Escape, Mods::default());
        let start = std::time::Instant::now();
        let q = picker_prop(&omp.window, pane, &pk, "query");
        while open_picker(&omp.window, pane).is_some()
            && picker_prop(&omp.window, pane, &pk, "query") == q
            && start.elapsed() < Duration::from_secs(3)
        {
            std::thread::sleep(Duration::from_millis(25));
            omp.window.frame();
        }
    }
    assert!(
        open_picker(&omp.window, pane).is_none(),
        "Esc closes the hub"
    );
    omp.ready();

    // The composer's model control: the md Switch model sheet.
    let model = omp.with_role("omp.composer.model");
    let s = omp.window.live_surface(pane, "s:1").expect("the session");
    let el = omp.window.element_of(pane, s, &model[0].0).expect("drawn");
    let b = omp.window.doc(pane).border_box(el).expect("laid out");
    omp.window.click(pane, b.x + b.w / 2.0, b.y + b.h / 2.0);
    let pk = omp.window.wait("omp's model dropdown", UI, |w| {
        let pk = open_picker(w, pane)?;
        (picker_prop(w, pane, &pk, "size")? == "md").then_some(pk)
    });
    std::thread::sleep(Duration::from_millis(300));
    omp.window.frame();
    let s = omp.window.live_surface(pane, "s:1").expect("the session");
    let pk_el = omp.window.element_of(pane, s, &pk).expect("drawn");
    let doc = omp.window.doc(pane);
    let sheet = first_with_class(&omp.window, pane, pk_el, "pk-sheet");
    let sh = doc.border_box(sheet).expect("the sheet");
    assert!(
        sh.w <= 680.5 && (sh.y - l.y - 72.0).abs() < 1.0,
        "an md sheet: {sh:?} in {l:?}"
    );
    // A click on an unselected row selects it (omp moves `selected`).
    let selected = picker_prop(&omp.window, pane, &pk, "selected");
    let row = omp
        .window
        .with_class(pane, pk_el, "pk-row")
        .into_iter()
        .find(|&r| {
            let c = omp.window.class(pane, r);
            let c: Vec<&str> = c.split(' ').collect();
            !c.contains(&"grp") && !c.contains(&"sel") && !c.contains(&"off")
        })
        .expect("another model");
    let item = omp
        .window
        .doc(pane)
        .attribute(row, "data-v")
        .unwrap()
        .to_owned();
    let r = omp.window.doc(pane).border_box(row).unwrap();
    let out = omp.window.click(pane, r.x + r.w / 2.0, r.y + r.h / 2.0);
    assert!(
        out.iter().any(|o| matches!(o, Outcome::Event { .. })),
        "{out:?}"
    );
    omp.window.wait("omp to select the clicked row", UI, |w| {
        (picker_prop(w, pane, &pk, "selected")? == item.as_str()).then_some(())
    });
    assert_ne!(
        selected.as_ref().and_then(Value::as_str),
        Some(item.as_str())
    );
    omp.window.press(pane, Key::Escape, Mods::default());
    omp.window.wait("the dropdown to close", UI, |w| {
        open_picker(w, pane).is_none().then_some(())
    });
    assert!(omp.window.errors().is_empty(), "{:?}", omp.window.errors());
    let report = t2_report(&omp.records());
    assert!(
        report.errors.is_empty(),
        "omp got errors: {:?}",
        report.errors
    );
    omp.ready();
    omp.quit();
    assert_eq!(
        std::fs::read(&config).ok(),
        config_before,
        "omp's config is unchanged"
    );
}

/// A23 `chart` on a live omp 18.8.5, with no prompt: once Auk advertises `chart`, omp's Usage
/// overlay (its cost control) draws a year of activity as a native heatmap (`supports("chart")`;
/// a table of glyphs otherwise). The chart reaches the window with omp's props (7 weekday rows
/// of up to 53 weeks, month labels, per-day tips, the summary), is a `figure` named by its
/// summary, lays out md cells (11px, 3px apart), and paints each day's level from the theme's
/// accent over the card, in the dark and then in the light. Skipped (with a note) while `chart`
/// isn't in the hello's kinds.
#[test]
fn a23_omps_usage_heatmap_is_a_native_chart_in_dark_and_light() {
    if !auk_tsp::Config::default()
        .kinds
        .iter()
        .any(|k| k == "chart")
    {
        eprintln!("skipped: `chart` isn't advertised yet (A23 adds it to the hello's kinds)");
        return;
    }
    let mut omp = LiveOmp::start_with(
        "omp-chart",
        true,
        Setup {
            dark: true,
            ..Setup::default()
        },
    );
    let pane = omp.pane;
    omp.ready();
    let usage = omp.window.wait("omp's cost control", UI, |w| {
        let s = w.live_surface(pane, "s:1")?;
        kinds_under(s.document().root())
            .into_iter()
            .find(|(id, _)| {
                s.document()
                    .get(id)
                    .and_then(|n| n.prop("role"))
                    .and_then(Value::as_str)
                    == Some("omp.composer.usage")
            })
    });
    let s = omp
        .window
        .live_surface(pane, "s:1")
        .expect("the session surface");
    let el = omp.window.element_of(pane, s, &usage.0).expect("drawn");
    let b = omp.window.doc(pane).border_box(el).expect("laid out");
    omp.window.click(pane, b.x + b.w / 2.0, b.y + b.h / 2.0);
    // The heatmap arrives once omp has read the usage history.
    let chart = omp
        .window
        .wait("omp's activity heatmap", Duration::from_secs(60), |w| {
            let s = w.live_surface(pane, "s:1")?;
            kinds_under(s.document().root())
                .into_iter()
                .find(|(_, k)| k == "chart")
                .map(|(id, _)| id)
        });
    let s = omp
        .window
        .live_surface(pane, "s:1")
        .expect("the session surface");
    let n = s.document().get(&chart).expect("the chart node");
    assert_eq!(n.prop("kind").and_then(Value::as_str), Some("heatmap"));
    let cells = n
        .prop("cells")
        .and_then(Value::as_array)
        .expect("cells")
        .clone();
    assert_eq!(cells.len(), 7, "a row per weekday");
    let weeks = cells
        .iter()
        .filter_map(Value::as_array)
        .map(Vec::len)
        .max()
        .unwrap_or(0);
    assert!(weeks > 0 && weeks <= 53, "{weeks} weeks");
    let summary = n
        .prop("summary")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    assert!(!summary.is_empty(), "omp names its chart");
    let report = t2_report(&omp.records());
    // No error answered any op (an unadvertised kind would be one).
    assert!(
        report.errors.is_empty(),
        "omp got errors: {:?}",
        report.errors
    );

    let el = omp.window.element_of(pane, s, &chart).expect("drawn");
    let doc = omp.window.doc(pane);
    assert_eq!(doc.attribute(el, "role"), Some("figure"));
    assert_eq!(doc.attribute(el, "aria-label"), Some(summary.as_str()));
    let ax = omp.window.view(pane).accessibility(doc);
    let fig = ax
        .find(&|a| a.id.as_deref() == Some(chart.as_str()))
        .expect("in the tree");
    assert_eq!(
        (fig.role.as_str(), fig.name.as_deref()),
        ("figure", Some(summary.as_str()))
    );
    let wks = omp.window.with_class(pane, el, "wk");
    assert_eq!(wks.len(), weeks);
    let all = omp.window.with_class(pane, el, "c");
    assert_eq!(all.len(), 7 * weeks);
    let c0 = doc.border_box(all[0]).expect("a cell laid out");
    let c1 = doc.border_box(all[1]).expect("a cell laid out");
    assert_eq!((c0.w, c0.h), (11.0, 11.0), "md cells");
    assert!(
        (c1.y - c0.y - 14.0).abs() < 0.01,
        "3px apart: {c0:?} {c1:?}"
    );

    // Each drawn day paints its level's color over the card, in the dark and in the light.
    for dark in [true, false] {
        if !dark {
            omp.window.set_dark(false);
        }
        // Past the arrivals (omp builds the chart again for the new theme's facts, so its columns
        // fade in again).
        let start = std::time::Instant::now();
        let el = loop {
            omp.window.frame();
            let s = omp
                .window
                .live_surface(pane, "s:1")
                .expect("the session surface");
            let el = omp.window.element_of(pane, s, &chart).expect("drawn");
            let wks = omp.window.with_class(pane, el, "wk");
            let doc = omp.window.doc(pane);
            let settled = start.elapsed() > Duration::from_millis(300)
                && wks
                    .iter()
                    .all(|&w| doc.computed_style(w).is_some_and(|c| c.opacity == 1.0));
            if settled {
                break el;
            }
            assert!(start.elapsed() < UI, "the chart's arrivals finish");
            std::thread::sleep(Duration::from_millis(50));
        };
        let all = omp.window.with_class(pane, el, "c");
        let ov_el = std::iter::successors(Some(el), |&e| omp.window.doc(pane).parent(e))
            .find(|&e| {
                omp.window
                    .class(pane, e)
                    .split(' ')
                    .any(|c| c == "sf-ov-card")
            })
            .expect("the chart is in the overlay's card");
        let doc = omp.window.doc(pane);
        let card = doc.computed_style(ov_el).expect("styled").background_color;
        assert_eq!(card.a, 1.0, "an opaque card");
        let hm = omp.window.with_class(pane, el, "hm")[0];
        let r = doc.border_box(hm).expect("laid out");
        let (x0, y0) = (r.x.floor(), r.y.floor());
        let px = render_part(doc, (x0, y0, r.w.ceil() as u32 + 1, r.h.ceil() as u32 + 1));
        let to8 = |v: f32| (v * 255.0).round() as u8;
        let mut levels = BTreeSet::new();
        for &c in &all {
            let l = doc.attribute(c, "data-l").unwrap_or("n").to_owned();
            let bg = doc.computed_style(c).expect("styled").background_color;
            let want = [
                bg.r * bg.a + card.r * (1.0 - bg.a),
                bg.g * bg.a + card.g * (1.0 - bg.a),
                bg.b * bg.a + card.b * (1.0 - bg.a),
            ]
            .map(to8);
            let b = doc.border_box(c).expect("laid out");
            if b.x < r.x || b.x + b.w > r.x + r.w {
                continue; // scrolled out of view
            }
            let p = px.pixel((b.x + b.w / 2.0 - x0) as u32, (b.y + b.h / 2.0 - y0) as u32);
            assert!(
                [p.0, p.1, p.2]
                    .iter()
                    .zip(want)
                    .all(|(a, b)| a.abs_diff(b) <= 3),
                "dark={dark}: level {l} cell {p:?}, want {want:?}"
            );
            levels.insert(l);
        }
        eprintln!(
            "dark={dark}: levels drawn {levels:?}; heatmap {r:?} scrolls to {:?}; card {:?}",
            doc.scroll_size(hm),
            doc.border_box(ov_el)
        );
        // The full level is omp's palette accent (`--sf-p-accent`, book:6986), else the theme's.
        let hex = |v: &str| -> Option<[u8; 3]> {
            let h = v.trim().strip_prefix('#')?;
            let byte = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok();
            Some([byte(0)?, byte(2)?, byte(4)?])
        };
        let custom = &doc.computed_style(el).expect("styled").custom;
        let palette = custom.get("--sf-p-accent").and_then(|v| hex(v));
        let theme = hex(custom.get("--accent").expect("--accent")).expect("a hex accent");
        eprintln!("dark={dark}: palette accent {palette:?}, theme accent {theme:?}");
        let accent = palette.unwrap_or(theme);
        if let Some(&c) = all
            .iter()
            .find(|&&c| doc.attribute(c, "data-l") == Some("4"))
        {
            let bg = doc.computed_style(c).expect("styled").background_color;
            assert_eq!([to8(bg.r), to8(bg.g), to8(bg.b)], accent, "dark={dark}");
        }
    }
    omp.window.press(pane, Key::Escape, Mods::default());
    omp.window.wait("the usage overlay to close", UI, |w| {
        let s = w.live_surface(pane, "s:1")?;
        s.document().get(&chart).is_none().then_some(())
    });
    omp.quit();
}

// ---------------------------------------------------------------------------------------------
// A23: math

/// The answer of [`math_session`]: md math in every delimiter of omp's dialect, and a typo.
const MATH_ANSWER: &str = "## Math\n\n\
    Euler: $e^{i\\pi} + 1 = 0$, and \\(\\alpha_1 + \\beta\\) inline.\n\n\
    $$\\sum_{k=1}^{n} k = \\frac{n(n+1)}{2}$$\n\n\
    \\[\n\\begin{pmatrix} a & b \\\\ c & d \\end{pmatrix}\n\\]\n\n\
    \\begin{aligned}\nf(x) &= (x+1)^2 \\\\\n&= x^2 + 2x + 1\n\\end{aligned}\n\n\
    A typo: $\\frac{1}$.";

/// A session file with one question and [`MATH_ANSWER`], for `--resume` (no model call).
fn math_session(dir: &Path, cwd: &Path) -> PathBuf {
    let id = "01a23000-0000-7000-8000-00000000a23a";
    let at = |i: usize| format!("2026-10-08T08:00:{i:02}.000Z");
    let lines = [
        serde_json::json!({"type": "session", "version": 3, "id": id, "timestamp": at(0), "cwd": cwd}),
        serde_json::json!({"type": "model_change", "id": "m0000000", "parentId": null, "timestamp": at(0),
                           "model": "xai-oauth/grok-4.7", "resolvedModelIsFallback": false}),
        serde_json::json!({"type": "message", "id": "u0000000", "parentId": "m0000000", "timestamp": at(1),
            "message": {"role": "user", "content": [{"type": "text", "text": "Some math?"}],
                        "attribution": "user", "timestamp": 1_791_449_000_000u64}}),
        serde_json::json!({"type": "message", "id": "a0000000", "parentId": "u0000000", "timestamp": at(2),
            "message": {"role": "assistant", "content": [{"type": "text", "text": MATH_ANSWER}],
                        "api": "openai-responses", "provider": "xai-oauth", "model": "grok-4.7",
                        "usage": {"input": 10, "output": 10, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 20,
                                  "cost": {"input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0, "total": 0.0}},
                        "stopReason": "stop", "timestamp": 1_791_449_002_000u64}}),
    ];
    let path = dir.join(format!("2026-10-08T08-00-00-000Z_{id}.jsonl"));
    let text: Vec<String> = lines.iter().map(Value::to_string).collect();
    std::fs::write(&path, text.join("\n") + "\n").expect("write the session");
    path
}

/// A23 math on a live omp 18.8.5, with no prompt: omp resumes a session whose answer has math
/// (no model call) and sends it as `md` with the TeX as written. The window typesets every
/// delimiter of the dialect (`$…$`, `\(…\)` inline; `$$…$$`, `\[…\]`, `\begin{env}` display)
/// into `svg`s painting in the text color, and shows the typo as `code.math-err` titled with
/// the error. Nothing is picked; omp's config is left as it was.
#[test]
fn a23_omps_answer_draws_md_math_typeset() {
    let config = real_home().join(".omp/agent/config.yml");
    let config_before = std::fs::read(&config).ok();
    let dir = std::env::temp_dir().join(format!("auk-omp-a23-math-{}", std::process::id()));
    let sessions = dir.join("sessions");
    std::fs::create_dir_all(&sessions).expect("a session dir");
    let session = math_session(&sessions, &dir);
    let mut omp = LiveOmp::start_with(
        "omp-a23-math",
        true,
        Setup {
            dark: true,
            before: None,
            args: Some(format!(
                "--session-dir {} --resume {}",
                quote(&sessions.to_string_lossy()),
                quote(&session.to_string_lossy())
            )),
            ..Setup::default()
        },
    );
    let pane = omp.pane;
    omp.ready();

    // omp sends the answer as md, the TeX as written.
    let md = omp.window.wait("the answer's md", UI, |w| {
        let s = w.live_surface(pane, "s:1")?;
        kinds_under(s.document().root())
            .into_iter()
            .find(|(id, k)| {
                k == "md"
                    && s.document()
                        .get(id)
                        .and_then(|n| n.prop("text"))
                        .and_then(Value::as_str)
                        .is_some_and(|t| t.contains("\\begin{aligned}"))
            })
            .map(|(id, _)| id)
    });
    let s = omp
        .window
        .live_surface(pane, "s:1")
        .expect("the session surface");
    let text = s
        .document()
        .get(&md)
        .unwrap()
        .prop("text")
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned();
    for part in [
        "$e^{i\\pi} + 1 = 0$",
        "\\(\\alpha_1 + \\beta\\)",
        "\\[",
        "\\begin{pmatrix}",
    ] {
        assert!(text.contains(part), "omp keeps {part}: {text}");
    }
    let el = omp.window.element_of(pane, s, &md).expect("drawn");
    omp.window.frame();
    let doc = omp.window.doc(pane);
    let tex = |e: auk_ui::NodeId| doc.attribute(e, "data-tex").unwrap_or("").to_owned();
    let inline: Vec<String> = omp
        .window
        .with_class(pane, el, "math")
        .into_iter()
        .map(tex)
        .collect();
    assert_eq!(inline, ["e^{i\\pi} + 1 = 0", "\\alpha_1 + \\beta"]);
    let display = omp.window.with_class(pane, el, "math-display");
    let shown: Vec<String> = display.iter().map(|&e| tex(e)).collect();
    assert_eq!(shown.len(), 3, "{shown:?}");
    assert!(
        shown[0].contains("\\sum")
            && shown[1].contains("pmatrix")
            && shown[2].starts_with("\\begin{aligned}")
    );
    let err = omp.window.with_class(pane, el, "math-err");
    assert_eq!(err.len(), 1);
    assert_eq!(omp.window.text_content(pane, err[0]), "\\frac{1}");
    assert!(
        doc.attribute(err[0], "title")
            .is_some_and(|t| !t.is_empty())
    );

    // Each formula is a picture painted in the text color: light ink on the dark page.
    let formulas: Vec<auk_ui::NodeId> = omp
        .window
        .with_class(pane, el, "math")
        .into_iter()
        .chain(display)
        .collect();
    for f in formulas {
        let svg = *doc.children(f).first().expect("the svg");
        assert!(
            doc.image(svg).is_some() && doc.image_tint(svg),
            "{} is typeset",
            tex(f)
        );
        let b = doc.border_box(svg).expect("laid out");
        assert!(b.w > 8.0 && b.h > 8.0, "{b:?}");
        let color = doc.computed_style(svg).expect("styled").color;
        let want = [color.r, color.g, color.b].map(|v| (v * 255.0).round() as u8);
        let (x0, y0) = (b.x.floor(), b.y.floor());
        let px = render_part(doc, (x0, y0, b.w.ceil() as u32 + 1, b.h.ceil() as u32 + 1));
        // The page is the most common pixel; the ink, the pixel furthest from it.
        let mut counts: BTreeMap<[u8; 3], usize> = BTreeMap::new();
        let mut pixels = Vec::new();
        for y in 0..px.height {
            for x in 0..px.width {
                let p = px.pixel(x, y);
                *counts.entry([p.0, p.1, p.2]).or_default() += 1;
                pixels.push([p.0, p.1, p.2]);
            }
        }
        let page = *counts.iter().max_by_key(|(_, n)| **n).unwrap().0;
        let dist = |p: &[u8; 3]| -> u32 {
            p.iter()
                .zip(page)
                .map(|(a, b)| u32::from(a.abs_diff(b)))
                .sum()
        };
        let ink = *pixels.iter().max_by_key(|p| dist(p)).unwrap();
        let lum = |c: [u8; 3]| c.iter().map(|&v| u32::from(v)).sum::<u32>();
        assert!(
            ink.iter().zip(want).all(|(a, b)| a.abs_diff(b) <= 8) && lum(ink) > lum(page),
            "{}: ink {ink:?} is the text color {want:?}, light on {page:?}",
            tex(f)
        );
    }
    omp.quit();
    assert_eq!(
        std::fs::read(&config).ok(),
        config_before,
        "omp's config is unchanged"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A23 `prefs` and `effort` on a live omp 18.8.5 under Auk's full hello, with no prompt. The
/// composer's footer shows omp's thinking effort as an `effort` node (not the T1 fallback row),
/// drawn as the book's meter with omp's level. `/settings` opens omp's settings as a `prefs` sheet
/// in the session's layer (not a screen surface): it docks at the pane's right edge (`aside`), so
/// omp hears the narrower width and the full width back when it goes; Down moves omp's focus,
/// which the sheet shows; Esc closes it. Nothing is changed: omp's config is byte-identical.
#[test]
fn a23_omps_settings_open_as_prefs_and_footer_shows_effort() {
    let config = real_home().join(".omp/agent/config.yml");
    let config_before = std::fs::read(&config).ok();
    let mut omp = LiveOmp::start("omp-prefs", true);
    let id = omp.ready();
    let pane = omp.pane;
    let advertised = &omp.window.tsp(pane).config().clone();
    for k in T2 {
        assert!(advertised.kinds.iter().any(|a| a == k), "{k} is advertised");
    }
    assert!(advertised.features.iter().any(|f| f == "aside"));

    // The footer's effort chip: an `effort` node, drawn as a meter.
    let (effort, level) = omp.window.wait("omp's effort chip", UI, |w| {
        let s = w.live_surface(pane, "s:1")?;
        kinds_under(s.document().root())
            .into_iter()
            .find(|(_, k)| k == "effort")
            .and_then(|(id, _)| {
                let level = s.document().get(&id)?.prop("level")?.as_str()?.to_owned();
                Some((id, level))
            })
    });
    omp.window.frame();
    let s = omp.window.live_surface(pane, "s:1").expect("the session");
    assert!(
        s.document().get("3.bar/effort").is_none(),
        "no fallback row"
    );
    let el = omp.window.element_of(pane, s, &effort).expect("drawn");
    let doc = omp.window.doc(pane);
    assert!(omp.window.class(pane, el).contains("sf-effort"));
    assert_eq!(doc.attribute(el, "role"), Some("meter"));
    assert_eq!(doc.attribute(el, "data-level"), Some(level.as_str()));
    let b = doc.border_box(el).expect("laid out");
    assert!(b.w > 0.0 && b.h > 0.0, "the ring has a size: {b:?}");
    eprintln!("effort {effort}: level {level}, {b:?}");

    // `/settings`: the prefs sheet, docked beside the session.
    omp.window.type_keys(pane, "/settings");
    omp.wait_text(&id, "/settings");
    omp.window.press(pane, Key::Enter, Mods::default());
    let pf = omp.window.wait("omp's settings sheet", UI, |w| {
        let s = w.live_surface(pane, "s:1")?;
        let layer = s.document().region("layer")?;
        kinds_under(layer)
            .into_iter()
            .find(|(_, k)| k == "prefs")
            .map(|(id, _)| id)
    });
    assert!(
        omp.window
            .tsp(pane)
            .surfaces()
            .iter()
            .all(|s| !s.is_live() || s.mode() != auk_tsp::Mode::Screen),
        "no screen surface"
    );
    std::thread::sleep(Duration::from_millis(300));
    omp.window.frame();
    let page = omp.window.view(pane).element();
    assert!(
        omp.window.class(pane, page).contains("auk-aside"),
        "the sheet docks"
    );
    let s = omp.window.live_surface(pane, "s:1").expect("the session");
    let pf_el = omp.window.element_of(pane, s, &pf).expect("drawn");
    let doc = omp.window.doc(pane);
    let sheet = doc.border_box(pf_el).expect("laid out");
    let pane_box = doc.border_box(page).expect("laid out");
    assert!(
        (sheet.x + sheet.w - (pane_box.x + pane_box.w)).abs() < 1.0 && sheet.w < pane_box.w * 0.61,
        "docked at the right edge: {sheet:?} in {pane_box:?}"
    );
    assert!(
        !omp.window.with_class(pane, pf_el, "pf-row").is_empty(),
        "rows are drawn"
    );
    let log = omp.log.clone();
    let resizes = || -> Vec<u64> {
        std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|r| r["dir"] == "in" && r["body"]["ev"] == "resize")
            .filter_map(|r| r["body"]["cols"].as_u64())
            .collect()
    };
    let narrow = u64::from(SIZE.0 - auk_tsp::aside_reserve(SIZE.0));
    omp.window.wait("omp to hear the narrower width", UI, |_| {
        resizes().contains(&narrow).then_some(())
    });

    // Down moves omp's focus; the sheet follows.
    let prop = |w: &NativeWindow, p: &str| {
        let s = w.live_surface(pane, "s:1")?;
        s.document().get(&pf)?.prop(p).cloned()
    };
    let before = prop(&omp.window, "focus");
    omp.window.press(pane, Key::Down, Mods::default());
    let focus = omp.window.wait("omp to move the focus", UI, |w| {
        let now = prop(w, "focus")?;
        (Some(&now) != before.as_ref()).then_some(now)
    });
    eprintln!("focus {before:?} -> {focus}");
    omp.window.frame();

    // Esc closes the sheet, and the full width comes back.
    for _ in 0..3 {
        let open = |w: &NativeWindow| {
            w.live_surface(pane, "s:1")
                .is_some_and(|s| s.document().get(&pf).is_some())
        };
        if !open(&omp.window) {
            break;
        }
        omp.window.press(pane, Key::Escape, Mods::default());
        let start = std::time::Instant::now();
        let f = prop(&omp.window, "focus");
        while open(&omp.window)
            && prop(&omp.window, "focus") == f
            && start.elapsed() < Duration::from_secs(3)
        {
            std::thread::sleep(Duration::from_millis(25));
            omp.window.frame();
        }
    }
    omp.window.wait("the settings sheet to close", UI, |w| {
        let s = w.live_surface(pane, "s:1")?;
        s.document().get(&pf).is_none().then_some(())
    });
    omp.window.wait("omp to hear the full width", UI, |_| {
        let r = resizes();
        let at = r.iter().position(|&c| c == narrow)?;
        r[at..].contains(&u64::from(SIZE.0)).then_some(())
    });
    omp.window.frame();
    assert!(
        !omp.window
            .class(pane, omp.window.view(pane).element())
            .contains("auk-aside")
    );
    assert!(omp.window.errors().is_empty(), "{:?}", omp.window.errors());
    let report = t2_report(&omp.records());
    assert!(
        report.errors.is_empty(),
        "omp got errors: {:?}",
        report.errors
    );
    assert!(report.sent_anyway.is_empty(), "{:?}", report.sent_anyway);
    omp.ready();
    omp.clear(&id);
    omp.quit();
    assert_eq!(
        std::fs::read(&config).ok(),
        config_before,
        "omp's config is unchanged"
    );
}

/// A23 `prefs` with a picker in its editor card, on a live omp 18.8.5 with no prompt: in
/// `/settings`, Enter on Dark Theme (the first row, focused when the sheet opens; 102 themes,
/// over omp's 12) opens omp's submenu as an md `picker` child of the `prefs` node. It draws in the
/// editor card, which is the sheet's frame (`.pf-editor.on.pk`): md-sheet tall where the sheet has
/// room, the picker filling it with rows inside. Esc closes the picker, Esc the sheet; nothing is
/// applied, and omp's config is byte-identical.
#[test]
fn a23_omps_theme_submenu_picker_fills_the_prefs_editor_card() {
    let config = real_home().join(".omp/agent/config.yml");
    let config_before = std::fs::read(&config).ok();
    let mut omp = LiveOmp::start("omp-prefs-picker", true);
    let id = omp.ready();
    let pane = omp.pane;
    omp.window.type_keys(pane, "/settings");
    omp.wait_text(&id, "/settings");
    omp.window.press(pane, Key::Enter, Mods::default());
    let pf = omp.window.wait("omp's settings sheet", UI, |w| {
        let s = w.live_surface(pane, "s:1")?;
        let layer = s.document().region("layer")?;
        let pf = kinds_under(layer)
            .into_iter()
            .find(|(_, k)| k == "prefs")?
            .0;
        (s.document().get(&pf)?.prop("focus")?.as_str() == Some("theme.dark")).then_some(pf)
    });
    omp.window.press(pane, Key::Enter, Mods::default());
    let picker = omp.window.wait("omp's theme picker", UI, |w| {
        let s = w.live_surface(pane, "s:1")?;
        let node = s.document().get(&pf)?;
        kinds_under(node)
            .into_iter()
            .find(|(_, k)| k == "picker")
            .map(|(id, _)| id)
    });
    std::thread::sleep(Duration::from_millis(400));
    omp.window.frame();
    let s = omp.window.live_surface(pane, "s:1").expect("the session");
    let pf_el = omp.window.element_of(pane, s, &pf).expect("drawn");
    let pk_el = omp.window.element_of(pane, s, &picker).expect("drawn");
    let doc = omp.window.doc(pane);
    let card = omp.window.with_class(pane, pf_el, "pf-editor")[0];
    assert_eq!(omp.window.class(pane, card), "pf-editor on pk");
    assert!(
        std::iter::successors(Some(pk_el), |&e| doc.parent(e)).any(|e| e == card),
        "the picker is in the card"
    );
    let page = doc
        .border_box(omp.window.with_class(pane, pf_el, "pf")[0])
        .expect("laid out");
    let c = doc.border_box(card).expect("laid out");
    let sheet = doc
        .border_box(omp.window.with_class(pane, pk_el, "pk-sheet")[0])
        .expect("laid out");
    assert!(
        (c.h - 540f32.min(page.h - 80.0)).abs() < 1.0,
        "card {c:?} in {page:?}"
    );
    assert!(
        c.x >= page.x + 15.0 && c.x + c.w <= page.x + page.w - 15.0,
        "{c:?} {page:?}"
    );
    assert!(
        (sheet.h - (c.h - 2.0)).abs() < 1.0 && (sheet.w - (c.w - 2.0)).abs() < 1.0,
        "{sheet:?} {c:?}"
    );
    let rows: Vec<_> = omp
        .window
        .with_class(pane, pk_el, "pk-row")
        .into_iter()
        .filter_map(|r| doc.border_box(r))
        .filter(|r| r.y >= c.y && r.y + r.h <= c.y + c.h)
        .collect();
    assert!(rows.len() >= 8, "rows show in the card: {rows:?}");
    eprintln!("card {c:?}, {} rows in view", rows.len());

    // Esc closes the picker (omp's `close`, bound to esc), Esc the sheet.
    omp.window.press(pane, Key::Escape, Mods::default());
    omp.window.wait("the picker to close", UI, |w| {
        let s = w.live_surface(pane, "s:1")?;
        s.document().get(&picker).is_none().then_some(())
    });
    omp.window.press(pane, Key::Escape, Mods::default());
    omp.window.wait("the settings sheet to close", UI, |w| {
        let s = w.live_surface(pane, "s:1")?;
        s.document().get(&pf).is_none().then_some(())
    });
    assert!(omp.window.errors().is_empty(), "{:?}", omp.window.errors());
    let report = t2_report(&omp.records());
    assert!(
        report.errors.is_empty(),
        "omp got errors: {:?}",
        report.errors
    );
    omp.ready();
    omp.clear(&id);
    omp.quit();
    assert_eq!(
        std::fs::read(&config).ok(),
        config_before,
        "omp's config is unchanged"
    );
}
