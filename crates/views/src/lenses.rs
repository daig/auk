//! Command blocks drawn in the flow (§6.2, §9.6): a pane's lens blocks, each a `block` node
//! (Tern's command-lens block, drawn internally per §8.2) at its anchor row.
//!
//! The replica keeps the blocks (line, raw output, status, the current lens view); a window
//! draws them through a [`LensLayer`]: a TSP engine of its own, never replicated, with one inline
//! surface (`lens`) whose main region holds every block. Each block's element is placed in the
//! pane's flow at its anchor row the way a surface block is. Local view state (Native/Raw, folds
//! in a lens view) lives in the layer's [`PaneView`], as for any surface.
//!
//! Node ids in a lens view are unstable (§9.6), so the layer gives every node of block `N` an id
//! under `lens<N>`; an event from inside block `N` goes to the daemon as surface `lens:N`.

use std::collections::BTreeMap;

use auk_term::Terminal;
use auk_tsp::{AnchorError, AnchorKey, Config, Grid, Line, MemoryBlobs, SurfaceKey, TspPane};
use auk_ui::{Document, NodeId, SheetKind};
use serde_json::{Value, json};

use crate::events::{Outcome, PointerEvent};
use crate::{PaneView, Region, ViewSettings};

/// The prefix of a command block's surface name (`lens:<block>`), in view events and in
/// [`SurfaceBlock::sf`](crate::SurfaceBlock).
pub const LENS_PREFIX: &str = "lens:";

/// The layer's one surface.
const SF: &str = "lens";
/// The column under the main region that holds the blocks (not virtualized: every block is laid
/// out, so each has a box).
const HOLDER: &str = "blocks";

/// One command block as the replica has it.
#[derive(Clone, Debug, PartialEq)]
pub struct LensInput {
    /// The block's number in the pane.
    pub id: u64,
    /// Its anchor row in the pane's grid.
    pub anchor: AnchorKey,
    /// The `block` node (props, then `native` when there is a view, then `raw`).
    pub node: Value,
    /// The capture width (Copy wraps the raw output at it).
    pub cols: u16,
}

/// A grid for the layer's engine: its surface's anchor is never drawn (blocks are placed by the
/// pane's own anchors), so any line will do.
#[derive(Debug, Default)]
struct NoGrid {
    cols: u16,
}

impl Grid for NoGrid {
    fn open_anchor(&mut self, _key: AnchorKey) -> Result<Line, AnchorError> {
        Ok(0)
    }
    fn undo_anchor(&mut self, _key: AnchorKey) -> bool {
        false
    }
    fn drop_anchor(&mut self, _key: AnchorKey) {}
    fn anchor_line(&self, _key: AnchorKey) -> Option<Line> {
        Some(0)
    }
    fn alt_screen(&self) -> bool {
        false
    }
    fn cols(&self) -> u16 {
        self.cols.max(2)
    }
}

/// A command block placed in the flow.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LensPlaced {
    pub id: u64,
    pub line: Line,
    pub el: NodeId,
    /// Height in pixels.
    pub height: f32,
}

/// A pane's command blocks, drawn.
pub struct LensLayer {
    doc: Document,
    view: PaneView,
    engine: TspPane,
    grid: NoGrid,
    /// What each block last sent: its node (with assigned ids) and anchor.
    sent: BTreeMap<u64, (Value, AnchorKey)>,
    /// The frame number.
    s: u64,
    key: Option<SurfaceKey>,
    /// Plugin sheets installed (§8.4 step 3).
    sheets: Vec<(String, String)>,
}

impl std::fmt::Debug for LensLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LensLayer")
            .field("blocks", &self.sent.len())
            .finish_non_exhaustive()
    }
}

/// Gives every node an id under `prefix` (`<parent>.<key|index>`), as block reconcilers do.
fn assign_ids(node: &mut Value, id: &str) {
    let Some(obj) = node.as_object_mut() else {
        return;
    };
    obj.insert("id".into(), Value::String(id.to_owned()));
    if let Some(Value::Array(children)) = obj.get_mut("c") {
        let mut seen = std::collections::HashSet::new();
        for (i, child) in children.iter_mut().enumerate() {
            let key = child
                .get("p")
                .and_then(|p| p.get("key"))
                .and_then(|k| match k {
                    Value::String(s) => Some(s.clone()),
                    Value::Number(n) => Some(n.to_string()),
                    _ => None,
                })
                .filter(|k| {
                    !k.is_empty()
                        && k.chars()
                            .all(|c| c.is_ascii_alphanumeric() || "_-".contains(c))
                });
            let mut cid = match key {
                Some(k) => format!("{id}.{k}"),
                None => format!("{id}.{i}"),
            };
            if !seen.insert(cid.clone()) {
                cid = format!("{id}.{i}");
                seen.insert(cid.clone());
            }
            assign_ids(child, &cid);
        }
    }
}

/// The block node the layer draws: ids under `lens<N>`, the raw child with the capture width.
fn prepare(input: &LensInput) -> Value {
    let base = format!("lens{}", input.id);
    let mut node = input.node.clone();
    let Some(obj) = node.as_object_mut() else {
        return node;
    };
    obj.insert("id".into(), Value::String(base.clone()));
    obj.insert("k".into(), Value::String("block".into()));
    if let Some(Value::Array(children)) = obj.get_mut("c") {
        let last = children.len().saturating_sub(1);
        for (i, child) in children.iter_mut().enumerate() {
            let raw = child["k"] == "ansi"
                && (i == last || child["id"].as_str().is_some_and(|s| s.ends_with("raw")));
            if raw {
                assign_ids(child, &format!("{base}.raw"));
                if let Some(p) = child
                    .as_object_mut()
                    .map(|c| c.entry("p").or_insert(json!({})))
                {
                    p["cols"] = input.cols.into();
                }
            } else {
                assign_ids(child, &format!("{base}.native"));
            }
        }
    }
    node
}

/// The block a node id belongs to (`lens<N>…`).
fn block_of(id: &str) -> Option<u64> {
    let rest = id.strip_prefix("lens")?;
    let n: String = rest.chars().take_while(char::is_ascii_digit).collect();
    n.parse().ok()
}

impl LensLayer {
    pub fn new(settings: ViewSettings, scale: f32) -> LensLayer {
        let mut doc = Document::new();
        doc.set_device_scale(scale);
        let _ = crate::install_sheets(&mut doc, settings.dark);
        doc.set_stylesheet("host", SheetKind::BuiltIn, None, "body { margin: 0; }");
        let root = doc.root();
        let body = doc.create_element("body");
        doc.append_child(root, body);
        let mut view = PaneView::new(&mut doc, settings);
        // `el` controls don't flip in lens views (§9.6).
        view.inert_controls = true;
        let el = view.element();
        doc.append_child(body, el);
        let mut kinds: Vec<String> = crate::KINDS.iter().map(|k| (*k).to_owned()).collect();
        kinds.extend(["block", "math", "effort"].map(str::to_owned));
        let config = Config {
            kinds,
            features: Vec::new(),
            retention: u64::MAX,
            credits: u32::MAX,
            ..Config::default()
        };
        let engine = TspPane::with(
            config,
            crate::sheet_checker(),
            Box::new(MemoryBlobs::default()),
        );
        let mut layer = LensLayer {
            doc,
            view,
            engine,
            grid: NoGrid { cols: 80 },
            sent: BTreeMap::new(),
            s: 0,
            key: None,
            sheets: Vec::new(),
        };
        layer.apc(
            "q",
            &json!({"q": "hello", "v": [1], "app": "auk", "features": []}),
        );
        layer
    }

    /// Feeds one message to the engine, chunked when it is long.
    fn apc(&mut self, verb: &str, body: &Value) {
        let text = body.to_string();
        let limit = auk_tsp::wire::MAX_APC - 1024;
        if text.len() <= limit {
            let payload = format!("tsp;{verb};{text}");
            self.engine.on_apc(&mut self.grid, payload.as_bytes());
            return;
        }
        let pieces: Vec<&[u8]> = auk_tsp::wire::chunks(text.as_bytes(), limit).collect();
        let last = pieces.len() - 1;
        self.s += 1;
        let chunk = auk_tsp::wire::base36(self.s);
        for (i, piece) in pieces.into_iter().enumerate() {
            let mut payload = format!("tsp;{verb};c={chunk};").into_bytes();
            if i < last {
                payload.extend_from_slice(b"m=1;");
            }
            payload.extend_from_slice(piece);
            self.engine.on_apc(&mut self.grid, &payload);
        }
    }

    fn frame(&mut self, ops: Vec<Value>) {
        if ops.is_empty() {
            return;
        }
        self.s += 1;
        let s = self.s;
        self.apc("f", &json!({"sf": SF, "s": s, "ops": ops}));
        self.engine.ack(&self.grid, SF, s);
        let _ = self.engine.take_output();
    }

    /// Brings the drawn blocks up to the replica's. Returns whether anything changed.
    pub fn sync(&mut self, blocks: &[LensInput], now: f64) -> bool {
        let mut ops = Vec::new();
        if self.key.is_none() {
            self.apc("o", &json!({"id": SF, "mode": "inline"}));
            ops.push(
                json!(["add", "main", SF, null, {"id": "main", "k": "col", "c": [
                    {"id": HOLDER, "k": "col", "p": {"justify": "start"}, "c": []}
                ]}]),
            );
        }
        let want: BTreeMap<u64, &LensInput> = blocks.iter().map(|b| (b.id, b)).collect();
        for id in self.sent.keys().copied().collect::<Vec<_>>() {
            if !want.contains_key(&id) {
                self.sent.remove(&id);
                ops.push(json!(["del", format!("lens{id}")]));
            }
        }
        let mut before: Option<String> = None;
        // Blocks in reverse id order, each added before the next one.
        for (&id, input) in want.iter().rev() {
            let node = prepare(input);
            let name = format!("lens{id}");
            match self.sent.get(&id) {
                Some((old, _)) if *old == node => {}
                Some((old, _)) => match update_ops(old, &node) {
                    Some(update) => ops.extend(update),
                    None => {
                        ops.push(json!(["del", name.clone()]));
                        ops.push(json!([
                            "add",
                            name.clone(),
                            HOLDER,
                            before.clone(),
                            node.clone()
                        ]));
                    }
                },
                None => {
                    ops.push(json!([
                        "add",
                        name.clone(),
                        HOLDER,
                        before.clone(),
                        node.clone()
                    ]));
                }
            }
            self.sent.insert(id, (node, input.anchor));
            before = Some(name);
        }
        let changed = !ops.is_empty();
        self.frame(ops);
        let changes = self.engine.take_changes();
        if self.key.is_none() {
            self.key = self.engine.find(SF, false).map(|s| s.key());
        }
        self.view.sync(&mut self.doc, &self.engine, &changes, now);
        changed
    }

    /// Whether the layer draws any block.
    pub fn is_empty(&self) -> bool {
        self.sent.is_empty()
    }

    pub fn configure(&mut self, settings: ViewSettings, scale: f32) {
        if self.view.settings() != &settings {
            crate::sheets::set_theme(&mut self.doc, settings.dark);
            self.view
                .set_settings(&mut self.doc, &self.engine, settings);
        }
        self.doc.set_device_scale(scale);
    }

    /// Installs plugin sheets (cascade step 3), replacing the last set.
    pub fn set_plugin_sheets(&mut self, sheets: &[(String, String)]) {
        if self.sheets == sheets {
            return;
        }
        for (name, _) in &self.sheets {
            self.doc.remove_stylesheet(name, None);
        }
        for (name, css) in sheets {
            self.doc.set_stylesheet(name, SheetKind::Plugin, None, css);
        }
        self.sheets = sheets.to_vec();
    }

    /// Lays the blocks out `width` points wide at the pane's width in cells.
    pub fn layout(&mut self, cols: u16, width: f32, now: f64) -> Option<f64> {
        self.grid.cols = cols;
        self.view.set_cols(&mut self.doc, &self.engine, cols);
        self.doc.set_viewport(width, 10_000.0);
        let next = self.view.tick(&mut self.doc, &self.engine, now);
        self.view.layout(&mut self.doc);
        next
    }

    /// The blocks with an anchor row in `term`'s main screen, with their elements and heights.
    pub fn placed(&self, term: &Terminal, scale: f32) -> Vec<LensPlaced> {
        let Some(key) = self.key else {
            return Vec::new();
        };
        self.sent
            .iter()
            .filter_map(|(&id, (_, anchor))| {
                let line = term.anchor_line(*anchor)?;
                let el = self.view.node_element(key, &format!("lens{id}"))?;
                let b = self.doc.border_box(el)?;
                (b.h > 0.0).then_some(LensPlaced {
                    id,
                    line,
                    el,
                    height: b.h * scale,
                })
            })
            .collect()
    }

    pub fn doc(&self) -> &Document {
        &self.doc
    }

    pub fn view(&self) -> &PaneView {
        &self.view
    }

    /// The main region (for painting: everything else in the document is left out).
    pub fn region(&self) -> Option<NodeId> {
        self.view.region(self.key?, Region::Main)
    }

    /// The element of a node in block `id` (`lens<id>`, `lens<id>.raw`, …).
    pub fn element(&self, node: &str) -> Option<NodeId> {
        self.view.node_element(self.key?, node)
    }

    /// A press at a document point. Events from a lens view are addressed to `lens:<block>`;
    /// Native/Raw and Copy are local.
    pub fn click(&mut self, ev: PointerEvent) -> Vec<Outcome> {
        let out = self.view.click(&mut self.doc, &self.engine, ev);
        out.into_iter()
            .map(|o| match o {
                Outcome::Event { surface, event, .. } => {
                    let block = block_of(event.id()).unwrap_or(0);
                    Outcome::Event {
                        surface,
                        sf: format!("{LENS_PREFIX}{block}"),
                        event,
                    }
                }
                other => other,
            })
            .collect()
    }

    /// Hover at a document point; returns whether the hovered elements changed.
    pub fn hover(&mut self, x: f32, y: f32) -> bool {
        let before = self.view.hovered().to_vec();
        self.view.hover(&mut self.doc, x, y);
        before != self.view.hovered()
    }

    /// The block's toolbar buttons and clickable view nodes, as drawn: `(class, data-view or
    /// data-id, text, [x, y, w, h])` with `y` from the block's top, in points.
    pub fn parts(&self, id: u64) -> Vec<(String, String, String, [f32; 4])> {
        let Some(el) = self.element(&format!("lens{id}")) else {
            return Vec::new();
        };
        let Some(top) = self.doc.border_box(el) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut stack = vec![el];
        while let Some(n) = stack.pop() {
            let class = self.view.written().class(n).unwrap_or("").to_owned();
            let wanted = class
                .split(' ')
                .any(|c| matches!(c, "sf-block-opt" | "sf-block-copy" | "sf-act"));
            if wanted && let Some(b) = self.doc.border_box(n) {
                let name = self
                    .doc
                    .attribute(n, "data-view")
                    .or_else(|| self.doc.attribute(n, "data-id"))
                    .unwrap_or("")
                    .to_owned();
                out.push((
                    class,
                    name,
                    text_of(&self.doc, n),
                    [b.x, b.y - top.y, b.w, b.h],
                ));
            }
            for &c in self.doc.children(n).iter().rev() {
                stack.push(c);
            }
        }
        out
    }

    /// The block's text as drawn (what it shows: the view or the raw output).
    pub fn text(&self, id: u64) -> String {
        self.element(&format!("lens{id}"))
            .map(|el| text_of(&self.doc, el))
            .unwrap_or_default()
    }

    /// Which view (`native` or `raw`) block `id` shows, as drawn.
    pub fn shown(&self, id: u64) -> Option<String> {
        let el = self.element(&format!("lens{id}"))?;
        self.doc.attribute(el, "data-view").map(str::to_owned)
    }
}

/// The text under an element, in tree order (attached nodes only).
fn text_of(doc: &Document, el: NodeId) -> String {
    let mut out = String::new();
    let mut stack = vec![el];
    while let Some(n) = stack.pop() {
        if let Some(t) = doc.node(n).text() {
            out.push_str(t);
        }
        for &c in doc.children(n).iter().rev() {
            stack.push(c);
        }
    }
    out
}

/// The ops that turn block `old` into `new` in place (props and raw text), when the children are
/// the same nodes; `None` when the block has to be added again.
fn update_ops(old: &Value, new: &Value) -> Option<Vec<Value>> {
    let id = new["id"].as_str().unwrap_or_default().to_owned();
    let oc = old["c"].as_array().cloned().unwrap_or_default();
    let nc = new["c"].as_array().cloned().unwrap_or_default();
    let same_shape = oc.len() == nc.len()
        && oc
            .iter()
            .zip(&nc)
            .all(|(a, b)| a["id"] == b["id"] && a["k"] == b["k"]);
    if !same_shape {
        return None;
    }
    let mut ops = Vec::new();
    let mut props = serde_json::Map::new();
    let empty = serde_json::Map::new();
    let op = old["p"].as_object().unwrap_or(&empty);
    let np = new["p"].as_object().unwrap_or(&empty);
    for (k, v) in np {
        if op.get(k) != Some(v) {
            props.insert(k.clone(), v.clone());
        }
    }
    for k in op.keys() {
        if !np.contains_key(k) {
            props.insert(k.clone(), Value::Null);
        }
    }
    if !props.is_empty() {
        ops.push(json!(["set", id, props]));
    }
    for (a, b) in oc.iter().zip(&nc) {
        if a == b {
            continue;
        }
        let cid = b["id"].as_str().unwrap_or_default();
        if b["k"] == "ansi" && a["p"]["cols"] == b["p"]["cols"] {
            let (at, bt) = (
                a["p"]["text"].as_str().unwrap_or(""),
                b["p"]["text"].as_str().unwrap_or(""),
            );
            match bt.strip_prefix(at) {
                Some(tail) if !at.is_empty() => ops.push(json!(["text", cid, "append", tail])),
                _ => ops.push(json!(["text", cid, "replace", bt])),
            }
        } else {
            ops.push(json!(["del", cid]));
            ops.push(json!(["add", cid, id, null, b]));
        }
    }
    Some(ops)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(id: u64, view: Option<Value>, raw: &str, done: bool) -> LensInput {
        let mut c = Vec::new();
        if let Some(v) = view {
            c.push(json!({"id": format!("lens{id}.native"), "k": "col", "p": {}, "c": [v]}));
        }
        c.push(json!({"id": format!("lens{id}.raw"), "k": "ansi", "p": {"text": raw}}));
        LensInput {
            id,
            anchor: AnchorKey(1 << 48 | id),
            node: json!({"id": format!("lens{id}"), "k": "block", "p": {
                "cmd": "terraform plan", "line": "terraform plan", "lens": "plugin.t.plan",
                "role": "lens.plugin.t.plan", "state": if done { "done" } else { "running" },
                "exit": 2, "took": 2400
            }, "c": c}),
            cols: 40,
        }
    }

    #[test]
    fn blocks_draw_raw_until_a_view_then_native_with_a_local_toggle() {
        let mut layer = LensLayer::new(ViewSettings::default(), 1.0);
        layer.sync(&[block(0, None, "line one\n", false)], 0.0);
        layer.layout(40, 400.0, 0.0);
        assert_eq!(layer.shown(0).as_deref(), Some("raw"));
        let el = layer.element("lens0").unwrap();
        assert!(layer.doc().border_box(el).unwrap().h > 0.0);

        // Output grows in place, then a view arrives.
        layer.sync(&[block(0, None, "line one\nline two\n", true)], 1.0);
        let view = json!({"k": "text", "p": {"text": "Plan: 2 to add"}});
        layer.sync(
            &[block(0, Some(view.clone()), "line one\nline two\n", true)],
            2.0,
        );
        layer.layout(40, 400.0, 2.0);
        assert_eq!(layer.shown(0).as_deref(), Some("native"));
        let class = layer.view().written().class(el).unwrap_or("").to_owned();
        assert!(class.contains("failed"), "{class}");
        // The view's node got an id under the block.
        assert!(layer.element("lens0.native.0").is_some());
    }

    /// §9.6: "`el` controls don't flip in lens views." A click on a checkbox, a radio or a
    /// label in a lens view changes nothing and sends nothing; the view's own actions still go
    /// to `lens:<block>`.
    #[test]
    fn el_controls_dont_flip_in_lens_views() {
        let mut layer = LensLayer::new(ViewSettings::default(), 1.0);
        let view = json!({"k": "el", "p": {"tag": "form"}, "c": [
            {"k": "el", "p": {"key": "cb", "tag": "input", "type": "checkbox", "name": "fast"}},
            {"k": "el", "p": {"key": "lb", "tag": "label"}, "c": [
                {"k": "el", "p": {"key": "r", "tag": "input", "type": "radio", "name": "size", "value": "s"}},
                {"k": "el", "p": {"key": "t", "tag": "span", "text": "Small"}}
            ]},
            {"k": "el", "p": {"key": "go", "tag": "button", "text": "Apply", "actions": {"click": "apply"}}}
        ]});
        layer.sync(&[block(0, Some(view), "out\n", true)], 0.0);
        layer.layout(40, 400.0, 0.0);
        assert_eq!(layer.shown(0).as_deref(), Some("native"));
        let click = |layer: &mut LensLayer, node: &str| {
            let el = layer.element(node).unwrap_or_else(|| panic!("no {node}"));
            let b = layer.doc().border_box(el).unwrap();
            let out = layer.click(PointerEvent {
                x: b.x + b.w / 2.0,
                y: b.y + b.h / 2.0,
                clicks: 1,
                ..Default::default()
            });
            layer.layout(40, 400.0, 1.0);
            out
        };
        let on = |layer: &LensLayer, node: &str| {
            let el = layer.element(node).unwrap();
            let class = layer.view().written().class(el).unwrap_or("").to_owned();
            class.split(' ').any(|c| c == "on")
        };
        for node in ["lens0.native.0.cb", "lens0.native.0.lb.r", "lens0.native.0.lb.t"] {
            assert_eq!(click(&mut layer, node), Vec::new(), "{node}");
        }
        assert!(!on(&layer, "lens0.native.0.cb") && !on(&layer, "lens0.native.0.lb.r"));
        // The button's action is still an event, addressed to the block.
        let out = click(&mut layer, "lens0.native.0.go");
        match out.as_slice() {
            [Outcome::Event { sf, event, .. }] => {
                assert_eq!(sf, "lens:0");
                assert!(matches!(**event, auk_tsp::ViewEvent::Action { ref action, .. } if action == "apply"), "{event:?}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn ids_follow_keys_and_indexes() {
        let mut v = json!({"k": "col", "c": [{"k": "text", "p": {"key": "a"}}, {"k": "text"}, {"k": "text", "p": {"key": "a"}}]});
        assign_ids(&mut v, "x");
        assert_eq!(v["c"][0]["id"], "x.a");
        assert_eq!(v["c"][1]["id"], "x.1");
        assert_eq!(v["c"][2]["id"], "x.2");
        assert_eq!(block_of("lens12.native.0"), Some(12));
    }
}
