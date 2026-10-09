//! A pane as the browser client draws it (§6.2, §9.6, §10.3, §10.4): its TSP surfaces (plugin
//! blocks are panes with a screen surface, §9.5), its command blocks with their lens views, the
//! catalog's plugin styles, and pointer input to all of them. Pure, so it runs and is tested
//! natively; the wasm client draws the grid where [`PaneDraw::arrange`] puts the rows and paints
//! the rest with [`PaneDraw::paint`].
//!
//! **Command blocks.** The replica claims lensed commands on its own and keeps each block's raw
//! output and its latest view (a side message, §15.1). Each block is drawn as a card at its anchor
//! row in the flow: the lens's view when one arrived, else the raw output as a mini terminal
//! (§9.6: "replicas show raw output until a view arrives"). A block's card is rendered by the
//! views into a document of its own, through an inline surface of a private TSP engine that
//! only this client sees, so lens views draw with every kind the views know. Clicks in it become
//! view events for `lens:<block>` (§9.6). View node ids are unstable (§9.6), so nodes without an
//! id get one here.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use auk_mirror::{ClientMsg, PaneId};
use auk_replica::{LensBlock, Replica, SF_PREFIX};
use auk_term::{AnchorKey, Terminal};
use auk_tsp::{Changes, MemoryBlobs, Mods, TspPane};
use auk_ui::{NodeId, SheetKind};
use auk_views::compose::{self, Block, Composed, Flow, Hit, Scroll};
use auk_views::{
    Arranged, Button, Outcome, PaneSurfaces, Place, PointerEvent, Region, SurfaceBlock,
    ViewSettings,
};
use serde_json::{Map, Value, json};
use vello::Scene;

/// The surface id of a command block's card in its private engine.
const LENS_SF: &str = "lens";

/// Card ids in that surface: a new one per card, so whether the engine took a card shows.
const CARD_PREFIX: &str = "lens-card-";

/// A document the views draw into, set up the same way for a pane and for a command block.
fn new_surfaces(settings: &ViewSettings, scale: f32, font: Option<&Arc<Vec<u8>>>) -> PaneSurfaces {
    let mut surfaces = PaneSurfaces::new(settings.clone(), scale);
    // The browser has no system fonts: surfaces draw in the server's font.
    if let Some(font) = font {
        surfaces.use_font(font.to_vec());
    }
    surfaces
}

/// The catalog's styles as plugin sheets (§8.4 cascade step 3), replacing the last set.
fn install_styles(surfaces: &mut PaneSurfaces, old: &[(String, String)], new: &[(String, String)]) {
    let (doc, _) = surfaces.parts();
    for (id, _) in old {
        if !new.iter().any(|(n, _)| n == id) {
            doc.remove_stylesheet(&sheet_name(id), None);
        }
    }
    for (id, css) in new {
        if !old.contains(&(id.clone(), css.clone())) {
            doc.set_stylesheet(&sheet_name(id), SheetKind::Plugin, None, css);
        }
    }
}

fn sheet_name(plugin: &str) -> String {
    format!("plugin.{plugin}")
}

/// What a click asks the client to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Send this to the daemon (a view event).
    Send(ClientMsg),
    /// Put text on the clipboard.
    Copy(String),
    /// Open a link or path.
    Open(String),
}

/// A pointer press on a pane, in pixels from its content area's top-left.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Press {
    pub x: f32,
    pub y: f32,
    pub clicks: u8,
    pub mods: Mods,
    pub secondary: bool,
}

/// One command block's card: a private engine with one inline surface, and the document the
/// views render it into.
struct LensDoc {
    term: Terminal,
    tsp: TspPane,
    surfaces: PaneSurfaces,
    /// The card last rendered.
    shown: Option<Value>,
    /// The id of the card in the surface.
    card: Option<String>,
    /// Why the last card couldn't be drawn, if it couldn't (the raw output shows instead).
    error: Option<String>,
    /// The next frame's sequence number.
    s: u64,
    /// The card's region element and its height in pixels, after the last layout.
    el: Option<NodeId>,
    height: f32,
}

impl LensDoc {
    fn new(settings: &ViewSettings, scale: f32, font: Option<&Arc<Vec<u8>>>, cols: u16) -> LensDoc {
        let mut term = Terminal::new(cols.max(2), 1, auk_term::Config::default());
        // The engine takes every kind the views draw.
        let mut config = auk_tsp::Config::default();
        for kind in auk_views::KINDS {
            if !config.kinds.iter().any(|k| k == kind) {
                config.kinds.push((*kind).to_owned());
            }
        }
        let mut tsp = TspPane::with(
            config,
            auk_views::sheet_checker(),
            Box::new(MemoryBlobs::default()),
        );
        // The handshake, so the engine reports refused frames.
        let hello = json!({"q": "hello", "v": [1], "app": "auk-lens", "features": []});
        tsp.on_apc(&mut term, format!("tsp;q;{hello}").as_bytes());
        let open = json!({"id": LENS_SF, "mode": "inline", "listen": false});
        tsp.on_apc(&mut term, format!("tsp;o;{open}").as_bytes());
        let _ = tsp.take_output();
        let mut surfaces = new_surfaces(settings, scale, font);
        surfaces.set_command_blocks(true);
        LensDoc {
            term,
            tsp,
            surfaces,
            shown: None,
            card: None,
            error: None,
            s: 1,
            el: None,
            height: 0.0,
        }
    }

    /// Renders `card` when it differs from the last one. A card the engine refuses (a view
    /// node the engine can't take) gives way to `fallback`, the raw output alone, and `error`
    /// says so.
    fn show(&mut self, card: Value, fallback: impl FnOnce() -> Value, now: f64) {
        if self.shown.as_ref() == Some(&card) {
            return;
        }
        self.error = None;
        if !self.frame(&card) {
            self.error = Some("the lens view couldn't be drawn; showing the raw output".into());
            self.frame(&fallback());
        }
        let changes = self.tsp.take_changes();
        self.surfaces.sync(&self.tsp, &changes, now);
        // A private engine has no program to answer.
        let _ = self.tsp.take_output();
        self.shown = Some(card);
    }

    /// Sends one frame putting `card` in place of the last one, under a fresh id; whether the
    /// engine took it.
    fn frame(&mut self, card: &Value) -> bool {
        let id = format!("{CARD_PREFIX}{}", self.s);
        let mut card = card.clone();
        card["id"] = json!(id);
        let has_main = self
            .tsp
            .surfaces()
            .last()
            .is_some_and(|s| s.document().get("main").is_some());
        // The old card goes first: the new one reuses its node ids.
        let mut ops = Vec::new();
        if let Some(old) = self.card.take() {
            ops.push(json!(["del", old]));
        }
        ops.push(if has_main {
            json!(["add", id, "main", null, card])
        } else {
            json!(["add", "main", LENS_SF, null, {"id": "main", "k": "col", "c": [card]}])
        });
        let frame = json!({"sf": LENS_SF, "s": self.s, "ops": ops});
        self.s += 1;
        self.tsp
            .on_apc(&mut self.term, format!("tsp;f;{frame}").as_bytes());
        let took = self
            .tsp
            .surfaces()
            .last()
            .is_some_and(|s| s.document().get(&id).is_some());
        if took {
            self.card = Some(id);
        }
        took
    }

    fn key(&self) -> Option<auk_tsp::SurfaceKey> {
        self.tsp.surfaces().last().map(|s| s.key())
    }
}

/// A command block's card, the views' `block` kind (research/elements.md `block`): the lens's
/// view as its native child when one arrived, and the raw output as its `raw` mini terminal,
/// which shows until then (§9.6).
pub fn lens_card(b: &LensBlock, raw: &[u8]) -> Value {
    let mut c = Vec::new();
    if let Some(view) = &b.view {
        c.push(with_ids(view));
    }
    c.push(json!({"id": "raw", "k": "ansi", "p": {
        "text": String::from_utf8_lossy(raw),
        "cols": b.cols,
    }}));
    let mut p = json!({
        "cmd": b.typed,
        "line": b.line,
        "lens": format!("{}.{}", b.plugin, b.lens),
        "role": b.role(),
        "state": if b.done { "done" } else { "running" },
    });
    if let Some(cwd) = &b.cwd {
        p["cwd"] = json!(cwd);
    }
    if let Some(status) = b.status {
        p["exit"] = json!(status);
    }
    if let Some(ms) = b.took_ms {
        p["took"] = json!(ms);
    }
    json!({"id": CARD_PREFIX, "k": "block", "p": p, "c": c})
}

/// A lens view with an id on every node: views come from plugins and their ids are unstable
/// (§9.6), so missing and repeated ids get fresh ones (`v1`, `v2`, … in tree order).
pub fn with_ids(view: &Value) -> Value {
    // The card's own ids and the region names stay free.
    let mut seen: BTreeSet<String> = ["main", "dock", "layer", "raw"].map(str::to_owned).into();
    let mut next = 0u32;
    fn walk(v: &Value, seen: &mut BTreeSet<String>, next: &mut u32) -> Value {
        let Some(obj) = v.as_object() else {
            return v.clone();
        };
        let mut out: Map<String, Value> = obj.clone();
        let id = obj
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty() && !id.starts_with(CARD_PREFIX) && !seen.contains(*id))
            .map(str::to_owned)
            .unwrap_or_else(|| {
                loop {
                    *next += 1;
                    let id = format!("v{next}");
                    if !seen.contains(&id) {
                        break id;
                    }
                }
            });
        seen.insert(id.clone());
        out.insert("id".into(), Value::String(id));
        if let Some(children) = obj.get("c").and_then(Value::as_array) {
            let c: Vec<Value> = children.iter().map(|c| walk(c, seen, next)).collect();
            out.insert("c".into(), Value::Array(c));
        }
        Value::Object(out)
    }
    walk(view, &mut seen, &mut next)
}

/// Where a command block was placed this frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LensPlaced {
    pub block: u64,
    /// Its anchor row's buffer line.
    pub line: usize,
    /// Its top, in pixels from the top of the flow area.
    pub y: f32,
    pub height: f32,
}

/// One pane's drawing state, never replicated.
pub struct PaneDraw {
    surfaces: PaneSurfaces,
    lens: BTreeMap<u64, LensDoc>,
    pub scroll: Scroll,
    /// The pane as last arranged: the whole flow (grid rows, surface and command blocks) in
    /// `composed`, whose `blocks` index the surface blocks only.
    pub arranged: Arranged,
    /// Every flow block, surfaces' and command blocks', in line order (for hit testing).
    flow: Vec<Block>,
    /// Command blocks shown this frame.
    pub lens_placed: Vec<LensPlaced>,
    settings: ViewSettings,
    scale: f32,
    font: Option<Arc<Vec<u8>>>,
    styles: Vec<(String, String)>,
    styles_serial: Option<u64>,
}

impl std::fmt::Debug for PaneDraw {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PaneDraw")
            .field("lens", &self.lens.keys().collect::<Vec<_>>())
            .field("lens_placed", &self.lens_placed)
            .finish_non_exhaustive()
    }
}

impl PaneDraw {
    /// `font` is the one font surfaces draw in, for a host without system fonts.
    pub fn new(settings: ViewSettings, scale: f32, font: Option<Arc<Vec<u8>>>) -> PaneDraw {
        PaneDraw {
            surfaces: new_surfaces(&settings, scale, font.as_ref()),
            lens: BTreeMap::new(),
            scroll: Scroll::default(),
            arranged: Arranged::default(),
            flow: Vec::new(),
            lens_placed: Vec::new(),
            settings,
            scale,
            font,
            styles: Vec::new(),
            styles_serial: None,
        }
    }

    pub fn surfaces(&self) -> &PaneSurfaces {
        &self.surfaces
    }

    /// Takes a batch of the replica's TSP changes.
    pub fn sync(&mut self, tsp: &TspPane, changes: &Changes, now: f64) {
        self.surfaces.sync(tsp, changes, now);
    }

    /// Window settings and scale, for the pane and its command blocks.
    pub fn configure(&mut self, tsp: &TspPane, settings: ViewSettings, scale: f32) {
        self.surfaces.configure(tsp, settings.clone(), scale);
        for l in self.lens.values_mut() {
            l.surfaces.configure(&l.tsp, settings.clone(), scale);
        }
        self.settings = settings;
        self.scale = scale;
    }

    /// The catalog's plugin styles (§10.4: the serving host's manifests), installed when
    /// `serial` changed.
    pub fn set_styles(&mut self, serial: u64, styles: &[(String, String)]) {
        if self.styles_serial == Some(serial) {
            return;
        }
        install_styles(&mut self.surfaces, &self.styles, styles);
        for l in self.lens.values_mut() {
            install_styles(&mut l.surfaces, &self.styles, styles);
        }
        self.styles = styles.to_vec();
        self.styles_serial = Some(serial);
    }

    /// The plugin sheets installed, by plugin id.
    pub fn style_ids(&self) -> Vec<&str> {
        self.styles.iter().map(|(id, _)| id.as_str()).collect()
    }

    /// Lays the pane's documents out for a content area of `size` points. Returns whether a
    /// surface animates (wants another frame).
    pub fn layout(&mut self, replica: &Replica, size: (f32, f32), focused: bool, now: f64) -> bool {
        let term = replica.terminal();
        let animating = self
            .surfaces
            .layout(term, replica.tsp(), size, focused, now)
            .is_some();
        // Command blocks: one card each, rendered when the block or its view changed.
        let blocks = replica.lens_blocks();
        self.lens.retain(|id, _| blocks.iter().any(|b| b.id == *id));
        for b in blocks {
            let raw = replica.lens_raw(b.id).unwrap_or_default();
            let card = lens_card(b, raw);
            let fallback = || {
                let mut b = b.clone();
                b.view = None;
                lens_card(&b, raw)
            };
            let doc = match self.lens.entry(b.id) {
                std::collections::btree_map::Entry::Occupied(e) => e.into_mut(),
                std::collections::btree_map::Entry::Vacant(e) => {
                    let mut doc =
                        LensDoc::new(&self.settings, self.scale, self.font.as_ref(), term.cols());
                    install_styles(&mut doc.surfaces, &[], &self.styles);
                    e.insert(doc)
                }
            };
            doc.show(card, fallback, now);
            doc.surfaces.layout(
                &doc.term,
                &doc.tsp,
                (size.0, size.1.max(1024.0)),
                false,
                now,
            );
            doc.el = doc
                .key()
                .and_then(|k| doc.surfaces.view().region(k, Region::Main));
            doc.height = doc
                .el
                .and_then(|el| doc.surfaces.doc().border_box(el))
                .map_or(0.0, |b| b.h * self.scale);
        }
        animating
    }

    /// Arranges the pane after [`PaneDraw::layout`]: the flow for a content area `content_px`
    /// pixels high with cells `cell_px` high, surface and command blocks at their anchor rows.
    pub fn arrange(&mut self, replica: &Replica, cell_px: f32, content_px: f32) -> &Arranged {
        let term = replica.terminal();
        let mut arranged = self.surfaces.arrange_shown(
            term,
            replica.tsp(),
            cell_px,
            content_px,
            self.scale,
            self.scroll,
        );
        // Command blocks show on the main screen, at their anchor rows.
        let mut lens: Vec<(usize, u64, f32)> = Vec::new();
        if !term.modes().alt_screen {
            for b in replica.lens_blocks() {
                let (Some(line), Some(doc)) =
                    (term.anchor_line(AnchorKey(b.anchor)), self.lens.get(&b.id))
                else {
                    continue;
                };
                if doc.height > 0.0 && !arranged.blocks.iter().any(|s| s.line == line) {
                    lens.push((line, b.id, doc.height));
                }
            }
        }
        // One flow with both kinds of block.
        enum Item {
            Surface(usize),
            Lens(u64),
        }
        let mut items: Vec<(usize, f32, Item)> = arranged
            .blocks
            .iter()
            .enumerate()
            .map(|(i, b)| (b.line, b.height, Item::Surface(i)))
            .chain(lens.iter().map(|&(line, id, h)| (line, h, Item::Lens(id))))
            .collect();
        items.sort_by_key(|(line, ..)| *line);
        self.flow = items
            .iter()
            .map(|&(line, height, _)| Block { line, height })
            .collect();
        self.lens_placed.clear();
        if !lens.is_empty() {
            let flow = Flow {
                lines: term.history_len() + usize::from(term.rows()),
                screen_top: term.history_len(),
                tail: compose::tail_line(term, &self.flow),
                cell: cell_px,
                view: arranged.flow_px,
                blocks: &self.flow,
                hide_from: None,
            };
            let composed: Composed = compose::compose(&flow, self.scroll);
            let mut surface_blocks = Vec::new();
            for &(i, y) in &composed.blocks {
                match items[i].2 {
                    Item::Surface(j) => surface_blocks.push((j, y)),
                    Item::Lens(block) => self.lens_placed.push(LensPlaced {
                        block,
                        line: items[i].0,
                        y,
                        height: items[i].1,
                    }),
                }
            }
            arranged.composed = Composed {
                blocks: surface_blocks,
                ..composed
            };
        }
        self.scroll = arranged.composed.scroll;
        self.arranged = arranged;
        &self.arranged
    }

    /// The surfaces shown this frame: (surface id, top in the flow area, height), in pixels. A
    /// screen surface (a plugin block's, §9.5) covers the content area from its top.
    pub fn surfaces_placed(&self, tsp: &TspPane) -> Vec<(String, f32, f32)> {
        let mut out: Vec<(String, f32, f32)> = self
            .arranged
            .composed
            .blocks
            .iter()
            .map(|&(i, y)| {
                let b = &self.arranged.blocks[i];
                (b.sf.clone(), y, b.height)
            })
            .collect();
        if let Some((_, b)) = self.arranged.screen
            && let Some(sf) = tsp
                .surfaces()
                .iter()
                .rev()
                .find(|s| s.mode() == auk_tsp::Mode::Screen && s.is_live())
        {
            out.push((sf.id().to_owned(), 0.0, b.h * self.scale));
        }
        out
    }

    /// Where a node of a command block's card was drawn: `[x, y, w, h]` in pixels from the
    /// content area's top-left (`card` is the block, view nodes keep their ids or get `v<n>`).
    pub fn lens_node_rect(&self, block: u64, id: &str) -> Option<[f32; 4]> {
        let placed = self.lens_placed.iter().find(|p| p.block == block)?;
        let doc = self.lens.get(&block)?;
        let top = doc.surfaces.doc().border_box(doc.el?)?.y;
        let node = doc.surfaces.view().node_element(doc.key()?, id)?;
        let b = doc.surfaces.doc().border_box(node)?;
        let s = self.scale;
        Some([b.x * s, placed.y + (b.y - top) * s, b.w * s, b.h * s])
    }

    /// Why a command block's view couldn't be drawn (it shows the raw output instead).
    pub fn lens_error(&self, block: u64) -> Option<&str> {
        self.lens.get(&block)?.error.as_deref()
    }

    /// Where a node of a surface was drawn, a block in the flow or the screen surface:
    /// `[x, y, w, h]` in pixels from the content area's top-left.
    pub fn surface_node_rect(&self, tsp: &TspPane, sf: &str, id: &str) -> Option<[f32; 4]> {
        let surface = tsp.surfaces().iter().rev().find(|s| s.id() == sf)?;
        let doc = self.surfaces.doc();
        let node = self.surfaces.view().node_element(surface.key(), id)?;
        let b = doc.border_box(node)?;
        let s = self.scale;
        let (top, at) = if surface.mode() == auk_tsp::Mode::Screen {
            let (_, screen) = self.arranged.screen?;
            (screen.y, 0.0)
        } else {
            let i = self.arranged.blocks.iter().position(|b| b.sf == sf)?;
            let &(_, y) = self
                .arranged
                .composed
                .blocks
                .iter()
                .find(|(j, _)| *j == i)?;
            (doc.border_box(self.arranged.blocks[i].el)?.y, y)
        };
        Some([b.x * s, at + (b.y - top) * s, b.w * s, b.h * s])
    }

    /// [`PaneDraw::surface_node_rect`] for a surface, or [`PaneDraw::lens_node_rect`] for a
    /// command block named `lens:<block>`.
    pub fn node_rect(&self, tsp: &TspPane, sf: &str, id: &str) -> Option<[f32; 4]> {
        match sf.strip_prefix(SF_PREFIX).and_then(|b| b.parse().ok()) {
            Some(block) => self.lens_node_rect(block, id),
            None => self.surface_node_rect(tsp, sf, id),
        }
    }

    /// Paints the surfaces and command blocks where [`PaneDraw::arrange`] put them. Returns
    /// whether anything was painted.
    pub fn paint(&mut self, scene: &mut Scene, place: Place) -> bool {
        let mut painted = self.surfaces.paint(&self.arranged, scene, place);
        for placed in &self.lens_placed {
            let Some(doc) = self.lens.get_mut(&placed.block) else {
                continue;
            };
            let Some(el) = doc.el else { continue };
            let one = Arranged {
                composed: Composed {
                    blocks: vec![(0, placed.y)],
                    ..Composed::default()
                },
                blocks: vec![SurfaceBlock {
                    sf: LENS_SF.into(),
                    line: 0,
                    el,
                    height: placed.height,
                }],
                dock: None,
                dock_px: self.arranged.dock_px,
                screen: None,
                layers: false,
                flow_px: self.arranged.flow_px,
                cell_px: self.arranged.cell_px,
            };
            painted |= doc.surfaces.paint(&one, scene, place);
        }
        painted
    }

    /// A pointer press at `press` (pixels from the content area's top-left): a surface's or a
    /// command block's view takes it (§7.8) and says what to do. `None` when no view is there
    /// (the press is the grid's).
    pub fn click(
        &mut self,
        pane: PaneId,
        replica: &Replica,
        press: Press,
        content_px: f32,
    ) -> Option<Vec<Action>> {
        let ev = |x: f32, y: f32| PointerEvent {
            x,
            y,
            clicks: press.clicks.clamp(1, 2),
            mods: press.mods,
            button: if press.secondary {
                Button::Secondary
            } else {
                Button::Primary
            },
        };
        match self.target(press, content_px)? {
            Target::Lens { block, x, y } => {
                let doc = self.lens.get_mut(&block)?;
                let (d, pv) = doc.surfaces.parts();
                let outcomes = pv.click(d, &doc.tsp, ev(x, y));
                Some(actions(
                    pane,
                    Some(&format!("{SF_PREFIX}{block}")),
                    outcomes,
                ))
            }
            Target::Surfaces { x, y } => {
                let (d, pv) = self.surfaces.parts();
                let outcomes = pv.click(d, replica.tsp(), ev(x, y));
                if outcomes.iter().all(|o| matches!(o, Outcome::Keys)) {
                    return None;
                }
                Some(actions(pane, None, outcomes))
            }
        }
    }

    /// The node a press at this point lands on, without pressing: its surface (`lens:<block>`
    /// for a command block's card) and the innermost node's id. `None` on the grid or between
    /// nodes.
    pub fn hit(&self, tsp: &TspPane, press: Press, content_px: f32) -> Option<(String, String)> {
        match self.target(press, content_px)? {
            Target::Lens { block, x, y } => {
                let doc = self.lens.get(&block)?;
                let (_, id) = node_at(doc.surfaces.doc(), doc.surfaces.view(), &doc.tsp, x, y)?;
                Some((format!("{SF_PREFIX}{block}"), id))
            }
            Target::Surfaces { x, y } => {
                node_at(self.surfaces.doc(), self.surfaces.view(), tsp, x, y)
            }
        }
    }

    /// Which document a press lands in, and where in it: a command block's card in the flow, or
    /// the pane's surfaces.
    fn target(&self, press: Press, content_px: f32) -> Option<Target> {
        if !self.arranged.layers
            && self.arranged.screen.is_none()
            && press.y < self.arranged.flow_px
            && let Some(Hit::Block(i, off)) = self.arranged.composed.hit(press.y, &self.flow)
            && let Some(placed) = self
                .lens_placed
                .iter()
                .find(|p| self.flow.get(i).is_some_and(|b| b.line == p.line))
            && let Some(doc) = self.lens.get(&placed.block)
            && let Some(el) = doc.el
        {
            let top = doc.surfaces.doc().border_box(el)?.y;
            return Some(Target::Lens {
                block: placed.block,
                x: press.x / self.scale,
                y: top + off / self.scale,
            });
        }
        let (x, y) =
            self.surfaces
                .doc_point(&self.arranged, press.x, press.y, content_px, self.scale)?;
        Some(Target::Surfaces { x, y })
    }
}

/// Where a press lands, in a document's coordinates.
enum Target {
    Lens { block: u64, x: f32, y: f32 },
    Surfaces { x: f32, y: f32 },
}

/// The innermost surface node at a point of a views document, and its surface's id.
fn node_at(
    doc: &auk_ui::Document,
    view: &auk_views::PaneView,
    tsp: &TspPane,
    x: f32,
    y: f32,
) -> Option<(String, String)> {
    let mut el = auk_ui::hit_test(doc, x, y)?;
    let mut id = None;
    loop {
        if doc.node(el).is_element() {
            if id.is_none() {
                id = doc.attribute(el, "data-id").map(str::to_owned);
            }
            if let Some(scope) = doc.attribute(el, "data-scope")
                && let Some(sf) = tsp
                    .surfaces()
                    .iter()
                    .find(|s| view.scope(s.key()) == Some(scope))
            {
                return Some((sf.id().to_owned(), id?));
            }
        }
        el = doc.parent(el)?;
    }
}

/// What the view's outcomes ask of the client; events go to their surface of `pane`, or to `to`
/// (a command block's `lens:<block>`).
fn actions(pane: PaneId, to: Option<&str>, outcomes: Vec<Outcome>) -> Vec<Action> {
    outcomes
        .into_iter()
        .filter_map(|o| match o {
            Outcome::Event { event, sf, .. } => {
                let mut body = event.body("");
                if let Some(obj) = body.as_object_mut() {
                    obj.remove("sf");
                }
                Some(Action::Send(ClientMsg::ViewEvent {
                    pane,
                    sf: to.map_or(sf, str::to_owned),
                    event: body,
                }))
            }
            Outcome::Copy(text) => Some(Action::Copy(text)),
            Outcome::Open(target) => Some(Action::Open(target)),
            // The browser has no image viewer: a zoom opens nothing.
            Outcome::Zoom(_) | Outcome::Keys | Outcome::Redraw => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use auk_plugins::catalog::{LensRule, PluginEntry};
    use auk_plugins::{Catalog, Status};
    use auk_replica::{Item, Record, Role, Settings, claim_catalog};

    const AHEM: &[u8] = include_bytes!("../../../fixtures/fonts/Ahem.ttf");
    const CELL: f32 = 16.0;
    const CONTENT: (f32, f32) = (640.0, 24.0 * CELL);

    fn catalog() -> Catalog {
        Catalog {
            plugins: vec![PluginEntry {
                id: "tf".into(),
                name: "TF".into(),
                version: "1".into(),
                description: "d".into(),
                icon: "stack".into(),
                css: String::new(),
                blocks: Vec::new(),
                lenses: vec![LensRule {
                    id: "plan".into(),
                    patterns: vec!["terraform plan".into()],
                }],
                host: true,
                window: false,
                status: Status::Ready,
            }],
            problems: Vec::new(),
            jit: false,
        }
    }

    fn feed(r: &mut Replica, bytes: &[u8]) {
        let seq = r.seq() + 1;
        r.apply(seq, Item::Output(bytes)).expect("in order");
    }

    /// A pane that ran a lensed `terraform plan` (three lines of output) and is at a prompt.
    fn lensed() -> Replica {
        let mut r = Replica::new(80, 24, Settings::default(), Role::Follower);
        let record = Record::Catalog {
            catalog: claim_catalog(&catalog()),
        };
        r.apply(r.seq() + 1, Item::Record(&record)).unwrap();
        feed(
            &mut r,
            b"\x1b]133;A\x07$ terraform plan\r\n\x1b]133;C;cmdline_url=terraform%20plan\x07",
        );
        feed(&mut r, b"one\r\ntwo\r\nPlan: 1 to add\r\n");
        feed(&mut r, b"\x1b]133;D;0\x07\x1b]133;A\x07$ ");
        assert_eq!(r.lens_blocks().len(), 1, "the command was claimed");
        r
    }

    fn apc(verb: &str, body: Value) -> Vec<u8> {
        format!("\x1b_tsp;{verb};{body}\x1b\\").into_bytes()
    }

    fn draw() -> PaneDraw {
        let settings = ViewSettings {
            cell: (8.0, CELL),
            ..ViewSettings::default()
        };
        PaneDraw::new(settings, 1.0, Some(Arc::new(AHEM.to_vec())))
    }

    fn frame(d: &mut PaneDraw, r: &mut Replica) {
        let changes = r.take_tsp_changes();
        d.sync(r.tsp(), &changes, 0.0);
        d.layout(r, CONTENT, true, 0.0);
        d.arrange(r, CELL, CONTENT.1);
    }

    fn press_at(rect: [f32; 4]) -> Press {
        Press {
            x: rect[0] + rect[2] / 2.0,
            y: rect[1] + rect[3] / 2.0,
            clicks: 1,
            ..Press::default()
        }
    }

    #[test]
    fn command_blocks_show_raw_output_then_the_lens_view_and_clicks_reach_the_lens() {
        let mut r = lensed();
        let mut d = draw();
        frame(&mut d, &mut r);
        // Raw output first, in a block at the anchor row, taller than a grid row.
        assert_eq!(d.lens_placed.len(), 1, "{d:?}");
        let raw = d.lens_placed[0];
        assert_eq!(raw.block, 0);
        assert!(raw.height > CELL, "{raw:?}");
        assert!(d.lens_node_rect(0, "raw").is_some(), "the raw output shows");
        // The block takes its place in the flow: the prompt row comes after it.
        let anchor = raw.line;
        let prompt = d
            .arranged
            .composed
            .slot_of(anchor + 1)
            .expect("the prompt row");
        assert!(prompt.top >= raw.y + raw.height - 0.5, "{prompt:?} {raw:?}");

        // The view arrives: it shows as the block's native child.
        let view = json!({"k": "col", "c": [
            {"k": "text", "p": {"text": "1 to add"}},
            {"id": "go", "k": "text", "p": {"text": "Deploy", "actions": {"click": "deploy"}}},
        ]});
        assert!(r.set_lens_view(0, Some(view)));
        frame(&mut d, &mut r);
        let go = d.lens_node_rect(0, "go").expect("the view's node is drawn");
        assert!(
            d.lens_node_rect(0, "v1").is_some(),
            "nodes without ids get one"
        );

        // A click on it is a view event for `lens:0`.
        let actions = d
            .click(7, &r, press_at(go), CONTENT.1)
            .expect("the view takes it");
        match &actions[..] {
            [Action::Send(ClientMsg::ViewEvent { pane: 7, sf, event })] => {
                assert_eq!(sf, "lens:0");
                assert_eq!(event["ev"], "action", "{event}");
                assert_eq!(event["act"], "deploy", "{event}");
                assert_eq!(event["id"], "go", "{event}");
            }
            other => panic!("{other:?}"),
        }
        // A click on the grid isn't the views'.
        let grid = Press {
            x: 4.0,
            y: prompt.top + 2.0,
            clicks: 1,
            ..Press::default()
        };
        assert_eq!(d.click(7, &r, grid, CONTENT.1), None);

        // The block goes when its anchor row does (a reset clears scrollback and screen).
        feed(&mut r, b"\x1bc");
        frame(&mut d, &mut r);
        assert!(d.lens_placed.is_empty(), "{d:?}");
    }

    #[test]
    fn surface_clicks_become_view_events_for_their_surface() {
        let mut r = Replica::new(80, 24, Settings::default(), Role::Follower);
        feed(
            &mut r,
            &apc(
                "q",
                json!({"q": "hello", "v": [1], "app": "t", "features": []}),
            ),
        );
        feed(&mut r, &apc("o", json!({"id": "s1", "mode": "inline"})));
        let main = json!({"id": "main", "k": "col", "c": [
            {"id": "ok", "k": "text", "p": {"text": "OK", "actions": {"click": "accept"}}},
        ]});
        feed(
            &mut r,
            &apc(
                "f",
                json!({"sf": "s1", "s": 1, "ops": [["add", "main", "s1", null, main]]}),
            ),
        );
        let mut d = draw();
        frame(&mut d, &mut r);
        let ok = d
            .surface_node_rect(r.tsp(), "s1", "ok")
            .expect("the surface's node is drawn");
        assert_eq!(d.surfaces_placed(r.tsp())[0].0, "s1");
        let actions = d
            .click(3, &r, press_at(ok), CONTENT.1)
            .expect("a surface takes it");
        match &actions[..] {
            [Action::Send(ClientMsg::ViewEvent { pane: 3, sf, event })] => {
                assert_eq!(sf, "s1");
                assert_eq!(
                    (event["ev"].as_str(), event["act"].as_str()),
                    (Some("action"), Some("accept"))
                );
                assert!(
                    event.get("sf").is_none(),
                    "the message names the surface: {event}"
                );
            }
            other => panic!("{other:?}"),
        }
    }

    /// The hit test (`window.aukHitTest` on the page) names what a press would land on, as
    /// the click itself finds it, without pressing.
    #[test]
    fn the_hit_test_names_the_node_a_press_lands_on() {
        // A command block's card.
        let mut r = lensed();
        let mut d = draw();
        assert!(r.set_lens_view(
            0,
            Some(json!({"k": "col", "c": [
                {"k": "text", "p": {"text": "1 to add"}},
                {"id": "go", "k": "text", "p": {"text": "Deploy", "actions": {"click": "deploy"}}},
            ]}))
        ));
        frame(&mut d, &mut r);
        let go = d.lens_node_rect(0, "go").expect("drawn");
        assert_eq!(
            d.hit(r.tsp(), press_at(go), CONTENT.1),
            Some(("lens:0".to_owned(), "go".to_owned()))
        );
        // The grid row after the block: no node.
        let raw = d.lens_placed[0];
        let prompt = d
            .arranged
            .composed
            .slot_of(raw.line + 1)
            .expect("the prompt row");
        let grid = Press {
            x: 4.0,
            y: prompt.top + 2.0,
            clicks: 1,
            ..Press::default()
        };
        assert_eq!(d.hit(r.tsp(), grid, CONTENT.1), None);
        // Hit testing presses nothing: the click still finds the node afterwards.
        assert!(d.click(7, &r, press_at(go), CONTENT.1).is_some());

        // A surface's nested node, innermost first.
        let mut r = Replica::new(80, 24, Settings::default(), Role::Follower);
        feed(&mut r, &apc("o", json!({"id": "s1", "mode": "inline"})));
        let main = json!({"id": "main", "k": "col", "c": [
            {"id": "row", "k": "row", "c": [
                {"id": "ok", "k": "text", "p": {"text": "OK", "actions": {"click": "accept"}}},
            ]},
        ]});
        feed(
            &mut r,
            &apc(
                "f",
                json!({"sf": "s1", "s": 1, "ops": [["add", "main", "s1", null, main]]}),
            ),
        );
        let mut d = draw();
        frame(&mut d, &mut r);
        let ok = d.surface_node_rect(r.tsp(), "s1", "ok").expect("drawn");
        assert_eq!(
            d.hit(r.tsp(), press_at(ok), CONTENT.1),
            Some(("s1".to_owned(), "ok".to_owned()))
        );
    }

    #[test]
    fn a_screen_surface_covers_the_pane_and_takes_clicks() {
        // A plugin block's shape (§9.5): a screen surface with no program features.
        let mut r = Replica::new(80, 24, Settings::default(), Role::Follower);
        feed(&mut r, &apc("o", json!({"id": "s1", "mode": "screen"})));
        let main = json!({"id": "main", "k": "col", "c": [
            {"id": "hit", "k": "text", "p": {"text": "bump", "actions": {"click": "bump"}}},
        ]});
        feed(
            &mut r,
            &apc(
                "f",
                json!({"sf": "s1", "s": 1, "ops": [["add", "main", "s1", null, main]]}),
            ),
        );
        let mut d = draw();
        frame(&mut d, &mut r);
        assert!(!d.arranged.grid_shown());
        assert_eq!(
            d.surfaces_placed(r.tsp()),
            vec![("s1".to_owned(), 0.0, CONTENT.1)]
        );
        let hit = d.surface_node_rect(r.tsp(), "s1", "hit").expect("drawn");
        match &d
            .click(1, &r, press_at(hit), CONTENT.1)
            .expect("the block takes it")[..]
        {
            [Action::Send(ClientMsg::ViewEvent { sf, event, .. })] => {
                assert_eq!((sf.as_str(), event["act"].as_str()), ("s1", Some("bump")));
            }
            other => panic!("{other:?}"),
        }
        // Below its content the screen surface still covers the pane: no grid there.
        let below = Press {
            x: 10.0,
            y: CONTENT.1 - 10.0,
            clicks: 1,
            ..Press::default()
        };
        assert_eq!(
            d.click(1, &r, below, CONTENT.1),
            None,
            "a click on nothing does nothing"
        );
    }

    #[test]
    fn catalog_styles_apply_to_surfaces_and_command_blocks() {
        let mut r = lensed();
        let mut d = draw();
        frame(&mut d, &mut r);
        let before = d.lens_placed[0].height;
        let css = ".sf-ansi { padding-top: 100px; }".to_owned();
        d.set_styles(1, &[("tf".into(), css)]);
        assert_eq!(d.style_ids(), ["tf"]);
        frame(&mut d, &mut r);
        let after = d.lens_placed[0].height;
        assert!(
            after >= before + 99.0,
            "the plugin sheet applies: {before} → {after}"
        );
        // A catalog without them drops them.
        d.set_styles(2, &[]);
        frame(&mut d, &mut r);
        assert!((d.lens_placed[0].height - before).abs() < 0.5);
        assert!(d.style_ids().is_empty());
    }

    #[test]
    fn a_view_the_views_cant_draw_leaves_the_raw_output() {
        let mut r = lensed();
        // A child without a string kind: the engine refuses the frame.
        assert!(r.set_lens_view(0, Some(json!({"k": "col", "c": [{"k": 5}]}))));
        let mut d = draw();
        frame(&mut d, &mut r);
        assert!(d.lens_error(0).is_some_and(|e| !e.is_empty()));
        assert!(d.lens_node_rect(0, "raw").is_some(), "the raw output shows");
        assert!(d.lens_placed[0].height > CELL);
        // A good view later replaces it.
        assert!(r.set_lens_view(
            0,
            Some(json!({"id": "ok", "k": "text", "p": {"text": "fine"}}))
        ));
        frame(&mut d, &mut r);
        assert_eq!(d.lens_error(0), None);
        assert!(d.lens_node_rect(0, "ok").is_some());
    }

    #[test]
    fn view_ids_are_filled_in_and_made_unique() {
        let v = with_ids(&json!({"k": "col", "c": [
            {"id": "a", "k": "text"}, {"id": "a", "k": "text"}, {"k": "text"}, {"id": "raw", "k": "text"},
            {"id": "dock", "k": "text"},
        ]}));
        let ids: Vec<&str> = v["c"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["id"].as_str().unwrap())
            .collect();
        assert_eq!(v["id"], "v1");
        assert_eq!(ids, ["a", "v2", "v3", "v4", "v5"]);
    }
}
