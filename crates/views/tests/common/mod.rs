//! Test support: a grid for the TSP engine, the real engine with the UI engine's sheet checker,
//! a UI document with the built-in sheets, and a pane view projecting into it. Recorded TSP
//! sessions replay through all three.

#![allow(dead_code)]

use auk_conformance::{Dir, Record, fixtures_dir, load};
use auk_tsp::{
    AnchorError, AnchorKey, Config, Grid, Line, MemoryBlobs, Outgoing, TspPane, ViewEvent,
};
use auk_ui::{Document, NodeId};
use auk_views::{PaneView, ViewSettings};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A grid with just enough behavior for anchors.
#[derive(Debug, Default)]
pub struct TestGrid {
    pub anchors: BTreeMap<AnchorKey, Line>,
    pub alt: bool,
    pub cols: u16,
    next_line: Line,
}

impl Grid for TestGrid {
    fn open_anchor(&mut self, key: AnchorKey) -> Result<Line, AnchorError> {
        if self.alt {
            return Err(AnchorError::AltScreen);
        }
        let line = self.next_line;
        self.next_line += 1;
        self.anchors.insert(key, line);
        Ok(line)
    }
    fn undo_anchor(&mut self, key: AnchorKey) -> bool {
        self.anchors.remove(&key).is_some()
    }
    fn drop_anchor(&mut self, key: AnchorKey) {
        self.anchors.remove(&key);
    }
    fn anchor_line(&self, key: AnchorKey) -> Option<Line> {
        self.anchors.get(&key).copied()
    }
    fn alt_screen(&self) -> bool {
        self.alt
    }
    fn cols(&self) -> u16 {
        self.cols
    }
}

pub fn payload(verb: &str, params: &[(&str, &str)], body: &str) -> Vec<u8> {
    let mut out = format!("tsp;{verb};").into_bytes();
    for (k, v) in params {
        out.extend_from_slice(format!("{k}={v};").as_bytes());
    }
    out.extend_from_slice(body.as_bytes());
    out
}

/// Decode what the engine sent the program (`ESC _ tsp;<r|e>;<json> ESC \`).
pub fn decode(out: &[Outgoing]) -> Vec<Value> {
    out.iter()
        .filter_map(|o| {
            let bytes = o.encode();
            let s = String::from_utf8_lossy(&bytes).into_owned();
            let start = s.find("tsp;")? + 6;
            let end = s.rfind("\x1b\\")?;
            serde_json::from_str(&s[start..end]).ok()
        })
        .collect()
}

pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

/// The host's own CSS around the pane (the app's window sheets stand in here).
pub const HOST_CSS: &str = "body { margin: 0; background-color: var(--page); }";

/// The engine, the document and the view of one pane.
pub struct Harness {
    pub grid: TestGrid,
    pub pane: TspPane,
    pub doc: Document,
    pub view: PaneView,
    pub body: NodeId,
    /// Everything the engine sent the program.
    pub sent: Vec<Value>,
    pub now: f64,
    /// Paint each replayed frame into a scene (for timing).
    pub paint: bool,
    /// Per replayed frame (`f`): engine + sync + tick + layout (+ paint) time, in ms.
    pub frame_times: Vec<f64>,
    /// Per replayed frame: (engine + sync, tick + layout, paint) in ms.
    pub frame_parts: Vec<(f64, f64, f64)>,
}

impl Harness {
    pub fn new() -> Harness {
        Harness::with(Config::default(), ViewSettings::default())
    }

    pub fn with(config: Config, settings: ViewSettings) -> Harness {
        let pane = TspPane::with(
            config,
            auk_views::sheet_checker(),
            Box::new(MemoryBlobs::default()),
        );
        let mut doc = Document::new();
        let errors = auk_views::install_sheets(&mut doc, settings.dark);
        assert!(errors.is_empty(), "built-in sheets: {errors:?}");
        doc.set_stylesheet("host", auk_ui::SheetKind::BuiltIn, None, HOST_CSS);
        let root = doc.root();
        let body = doc.create_element("body");
        doc.append_child(root, body);
        doc.set_viewport(960.0, 800.0);
        let view = PaneView::new(&mut doc, settings);
        let el = view.element();
        doc.append_child(body, el);
        let mut h = Harness {
            grid: TestGrid {
                cols: 120,
                ..Default::default()
            },
            pane,
            doc,
            view,
            body,
            sent: Vec::new(),
            now: 1_000_000.0,
            paint: false,
            frame_times: Vec::new(),
            frame_parts: Vec::new(),
        };
        let grid: &dyn Grid = &h.grid;
        h.pane.on_resize(grid);
        let _ = &mut h.view;
        h
    }

    /// Feed one APC payload and sync the view.
    pub fn apc(&mut self, verb: &str, body: &str) {
        self.pane.on_apc(&mut self.grid, &payload(verb, &[], body));
        self.sync();
    }

    pub fn sync(&mut self) {
        let changes = self.pane.take_changes();
        self.view
            .sync(&mut self.doc, &self.pane, &changes, self.now);
        let out = self.pane.take_output();
        self.sent.extend(decode(&out));
    }

    /// Lay out (with the measure loops) and run one clock tick.
    pub fn frame(&mut self) {
        self.view.tick(&mut self.doc, &self.pane, self.now);
        self.view.layout(&mut self.doc);
    }

    pub fn hello(&mut self, features: &[&str]) {
        let f: Vec<String> = features.iter().map(|s| format!("\"{s}\"")).collect();
        self.apc(
            "q",
            &format!(
                "{{\"q\":\"hello\",\"v\":[1],\"app\":\"test\",\"features\":[{}]}}",
                f.join(",")
            ),
        );
    }

    /// Open a surface and add `main` (and optionally `dock`) children in one frame.
    pub fn surface(&mut self, id: &str, mode: &str, main: Value) {
        self.apc("o", &format!("{{\"id\":\"{id}\",\"mode\":\"{mode}\"}}"));
        let frame = serde_json::json!({"sf": id, "s": 1, "ops": [["add", "main", id, null, {"id": "main", "k": "col", "c": main}]]});
        self.apc("f", &frame.to_string());
    }

    pub fn frame_ops(&mut self, sf: &str, s: u64, ops: Value) {
        let frame = serde_json::json!({"sf": sf, "s": s, "ops": ops});
        self.apc("f", &frame.to_string());
    }

    /// The element of a node in the newest surface with that id.
    pub fn el(&self, sf: &str, id: &str) -> NodeId {
        let key = self
            .pane
            .surfaces()
            .iter()
            .rev()
            .find(|s| s.id() == sf)
            .expect("surface")
            .key();
        self.view
            .node_element(key, id)
            .unwrap_or_else(|| panic!("no element for {id}"))
    }

    pub fn class(&self, el: NodeId) -> String {
        self.view.written().class(el).unwrap_or("").to_owned()
    }

    pub fn attr(&self, el: NodeId, name: &str) -> Option<String> {
        self.doc.attribute(el, name).map(str::to_owned)
    }

    pub fn text(&self, el: NodeId) -> String {
        let mut out = String::new();
        let mut stack = vec![el];
        while let Some(n) = stack.pop() {
            if let Some(t) = self.doc.node(n).text() {
                out.push_str(t);
            }
            for &c in self.doc.children(n).iter().rev() {
                stack.push(c);
            }
        }
        out
    }

    /// Elements under `el` whose class list has `class`.
    pub fn find_class(&self, el: NodeId, class: &str) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack = vec![el];
        while let Some(n) = stack.pop() {
            if self.doc.node(n).is_element() && self.class(n).split(' ').any(|c| c == class) {
                out.push(n);
            }
            for &c in self.doc.children(n).iter().rev() {
                stack.push(c);
            }
        }
        out
    }

    /// Send a view event through the engine as the primary would.
    pub fn send(&mut self, sf: &str, ev: ViewEvent) {
        let grid: &dyn Grid = &self.grid;
        let _ = self.pane.view_event(grid, sf, ev);
        let out = self.pane.take_output();
        self.sent.extend(decode(&out));
    }

    pub fn errors(&self) -> Vec<&Value> {
        self.sent.iter().filter(|e| e["ev"] == "error").collect()
    }
}

/// The engine configured as the recording terminal advertised.
pub fn config_for(path: &Path, records: &[Record]) -> Config {
    let mut config = Config::default();
    let is_reply = |r: &&Record| r.dir == Dir::In && r.verb == "r" && r.body["r"] == "hello";
    let companion = path
        .to_string_lossy()
        .strip_suffix("-own.jsonl")
        .map(|p| load(Path::new(&format!("{p}.jsonl"))).unwrap());
    let reply = records
        .iter()
        .find(is_reply)
        .or_else(|| companion.as_ref().and_then(|c| c.iter().find(is_reply)));
    if let Some(reply) = reply {
        let strings = |key: &str| {
            reply.body[key]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_owned())
                .collect()
        };
        config.kinds = strings("kinds");
        config.features = strings("features");
    }
    config
}

pub fn fixture(name: &str) -> (PathBuf, Vec<Record>) {
    let path = fixtures_dir().join(name);
    let records = load(&path).unwrap();
    (path, records)
}

/// Replay a recording through engine and view: program messages in order, recorded acks as the
/// primary's draws, each frame synced and laid out. `t` drives the clock. Returns the harness.
pub fn replay(name: &str, each: impl FnMut(&mut Harness, &Record)) -> Harness {
    replay_with(name, false, each)
}

pub fn replay_with(
    name: &str,
    paint: bool,
    mut each: impl FnMut(&mut Harness, &Record),
) -> Harness {
    let (path, records) = fixture(name);
    let mut h = Harness::with(config_for(&path, &records), ViewSettings::default());
    h.paint = paint;
    let mut scene = vello::Scene::new();
    for rec in &records {
        if let Some(t) = rec.t {
            h.now = t;
        }
        match (rec.dir, rec.verb.as_str()) {
            (Dir::Out, "osc") => {
                let body = rec.body.as_str().unwrap_or("");
                if body.starts_with("0;") || body.starts_with("2;") {
                    h.pane.on_title();
                }
            }
            (Dir::Out, "pty") => {}
            (Dir::Out, verb) => {
                let params: Vec<(&str, &str)> = rec
                    .params
                    .iter()
                    .map(|(k, v)| (k.as_str(), v.as_str().unwrap_or("")))
                    .collect();
                let body = match &rec.body {
                    Value::String(s) if verb == "b" => s.clone(),
                    other => other.to_string(),
                };
                let ms = |t: std::time::Instant| t.elapsed().as_secs_f64() * 1000.0;
                let t = std::time::Instant::now();
                h.pane.on_apc(&mut h.grid, &payload(verb, &params, &body));
                h.sync();
                let a = ms(t);
                h.frame();
                let b = ms(t);
                if h.paint {
                    scene.reset();
                    auk_ui::paint(&h.doc, &mut scene);
                }
                if verb == "f" {
                    let c = ms(t);
                    h.frame_times.push(c);
                    h.frame_parts.push((a, b - a, c - b));
                }
            }
            (Dir::In, "e") if rec.body["ev"] == "ack" => {
                let grid: &dyn Grid = &h.grid;
                h.pane.ack(
                    grid,
                    rec.body["sf"].as_str().unwrap(),
                    rec.body["s"].as_u64().unwrap(),
                );
                let out = h.pane.take_output();
                h.sent.extend(decode(&out));
            }
            _ => {}
        }
        each(&mut h, rec);
    }
    h
}
