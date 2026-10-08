//! The projector: TSP surfaces (auk-tsp) into the UI engine's DOM (auk-ui), incrementally from
//! [`TspPane::take_changes`]. One [`PaneView`] per pane per window; view state lives here and is
//! never replicated.

use crate::common::{self, Actions};
use crate::dom::{Dom, Written};
use crate::kinds;
use crate::palette;
use crate::state::SurfaceState;
use crate::{Region, ViewSettings};
use auk_tsp::{Changes, Mode, NodeRef, Surface, SurfaceKey, TspPane};
use auk_ui::{Document, NodeId};
use std::collections::{HashMap, HashSet};

/// Per-node render state kept by kinds with incremental sub-engines.
#[derive(Debug, Default)]
pub(crate) enum Extra {
    #[default]
    None,
    Md(Box<crate::md::MdState>),
    Code(Box<crate::kinds::rich::CodeState>),
    Ansi(Box<crate::ansi::AnsiState>),
    Field(Box<crate::kinds::input::FieldState>),
    Timer(crate::motion::Timer),
    Rate(crate::motion::RateState),
    Spin(crate::motion::SpinState),
    Bar(crate::motion::BarState),
    Shimmer(crate::motion::ShimmerState),
    Tool(Box<crate::kinds::agent::ToolState>),
    Toast(crate::kinds::layout::ToastState),
    Image(Box<crate::kinds::rich::ImageState>),
}

/// One TSP node's element and its parts.
#[derive(Debug)]
pub(crate) struct Mount {
    /// The node element (`.sf.sf-<kind>`), or the region element for a region node.
    pub el: NodeId,
    pub kind: String,
    pub tag: String,
    /// Generated nodes directly under `el`, in order.
    pub generated: Vec<NodeId>,
    /// Persistent containers that hold child elements (kinds route children into them).
    pub slots: Vec<NodeId>,
    /// Children go straight into `el`, after `gen`.
    pub direct: bool,
    /// Attributes the last render wrote (to remove the ones that stop applying).
    pub attrs: Vec<String>,
    pub extra: Extra,
    /// The node needs the clock (§7.13).
    pub clock: bool,
    pub region: bool,
    /// The sync pass that created it.
    pub stamp: u64,
}

impl Mount {
    fn new(el: NodeId, kind: &str, tag: &str) -> Mount {
        Mount {
            el,
            kind: kind.to_owned(),
            tag: tag.to_owned(),
            generated: Vec::new(),
            slots: Vec::new(),
            direct: false,
            attrs: Vec::new(),
            extra: Extra::None,
            clock: false,
            region: false,
            stamp: 0,
        }
    }
}

/// What a kind's render adds to the node element.
#[derive(Debug, Default)]
pub(crate) struct Look {
    pub class: String,
    pub style: String,
    pub attrs: Vec<(String, String)>,
    /// The kind uses `title` as content (`tool`, `picker`): no tooltip.
    pub title_is_content: bool,
}

impl Look {
    pub fn class(&mut self, c: &str) {
        if c.is_empty() {
            return;
        }
        if !self.class.is_empty() {
            self.class.push(' ');
        }
        self.class.push_str(c);
    }
    pub fn class_if(&mut self, on: bool, c: &str) {
        if on {
            self.class(c);
        }
    }
    pub fn attr(&mut self, name: &str, v: impl Into<String>) {
        self.attrs.push((name.to_owned(), v.into()));
    }
    pub fn style(&mut self, s: &str) {
        self.style.push_str(s);
        if !s.ends_with(' ') {
            self.style.push(' ');
        }
    }
}

/// The render context of one surface.
pub(crate) struct Cx<'a> {
    pub dom: Dom<'a>,
    pub now: f64,
    pub set: &'a ViewSettings,
    pub sf: &'a Surface,
    pub st: &'a mut SurfaceState,
    /// Pane width in cells.
    pub cols: u16,
    /// The sync pass.
    pub pass: u64,
    /// The pane's blobs (`image`).
    pub blobs: &'a dyn auk_tsp::BlobStore,
}

impl Cx<'_> {
    pub fn focus(&self) -> Option<&str> {
        self.sf.document().focus()
    }
}

/// One surface's projection.
#[derive(Debug)]
pub(crate) struct SurfaceView {
    pub key: SurfaceKey,
    pub id: String,
    pub scope: String,
    pub regions: [Option<NodeId>; 3],
    pub mounts: HashMap<String, Mount>,
    /// Program sheets installed: (sheet name, css).
    pub sheets: Vec<(String, String)>,
    pub st: SurfaceState,
    /// Nodes that need the clock.
    pub clocked: HashSet<String>,
    pub palette_style: String,
}

/// Where a region element sits in the pane container.
fn region_index(name: &str) -> Option<usize> {
    match name {
        "main" => Some(0),
        "dock" => Some(1),
        "layer" => Some(2),
        _ => None,
    }
}

/// The views of one pane in one window.
#[derive(Debug)]
pub struct PaneView {
    pub(crate) settings: ViewSettings,
    pub(crate) written: Written,
    pub(crate) surfaces: Vec<SurfaceView>,
    /// `div.auk-pane`: holds `.auk-flow` (inline and flow `main` regions), `.auk-docks`,
    /// `.auk-screen` (screen surfaces) and `.auk-layers`.
    pub(crate) container: NodeId,
    pub(crate) flow: NodeId,
    pub(crate) docks: NodeId,
    pub(crate) screen: NodeId,
    pub(crate) layers: NodeId,
    pub(crate) app: Option<String>,
    pub(crate) cols: u16,
    pub(crate) visible: bool,
    pub(crate) focused: bool,
    pub(crate) hover: Vec<NodeId>,
    pub(crate) toasts: crate::kinds::layout::ToastHost,
    pub(crate) last_now: f64,
    pub(crate) pass: u64,
    /// Status strips need refitting.
    pub(crate) fit_dirty: bool,
    pub(crate) pane_reveal: Option<(NodeId, auk_tsp::RevealAt)>,
    /// The part of the document the window shows (for virtualized containers an ancestor
    /// scrolls); `None` means the document viewport.
    pub(crate) view_rect: Option<auk_ui::Rect>,
    pub(crate) fit_viewport: (f32, f32),
    /// What the last measure loop settled on: the document's generation and the inputs the
    /// measure pass reads besides it (viewport, view rect, cell size).
    pub(crate) measured: Option<(u64, crate::measure::MeasureInputs)>,
    /// The CSS motion clock's pause (§7.13: motion pauses while the pane is hidden): time spent
    /// hidden, and since when it is hidden.
    pub(crate) paused_ms: f64,
    pub(crate) hidden_at: Option<f64>,
    /// `el` controls never flip (a lens layer's view, §9.6).
    pub(crate) inert_controls: bool,
}

impl PaneView {
    /// A pane view with its container element created (detached): put
    /// [`PaneView::element`] where the pane's surfaces should draw.
    pub fn new(doc: &mut Document, settings: ViewSettings) -> PaneView {
        let mut written = Written::default();
        let mut dom = Dom {
            doc,
            w: &mut written,
        };
        let container = dom.el("div", "auk-pane");
        let flow = dom.child(container, "div", "auk-flow");
        let docks = dom.child(container, "div", "auk-docks");
        let screen = dom.child(container, "div", "auk-screen sf-hidden");
        let layers = dom.child(container, "div", "auk-layers");
        let toasts = crate::kinds::layout::ToastHost::new(&mut dom, container);
        // Load the grammars off the main thread before the first code block needs them.
        crate::highlight::warm();
        // Reduce Motion reaches sheets (`prefers-reduced-motion`) and the CSS motion engine; the
        // other two preferences reach sheets (§8.4).
        settings.apply_media(doc);
        PaneView {
            settings,
            written,
            surfaces: Vec::new(),
            container,
            flow,
            docks,
            screen,
            layers,
            app: None,
            cols: 80,
            visible: true,
            focused: true,
            hover: Vec::new(),
            toasts,
            last_now: 0.0,
            pass: 0,
            fit_dirty: true,
            pane_reveal: None,
            view_rect: None,
            fit_viewport: (0.0, 0.0),
            measured: None,
            paused_ms: 0.0,
            hidden_at: None,
            inert_controls: false,
        }
    }

    /// The clock CSS transitions and animations run on: the view's clock less the time the pane
    /// spent hidden, so they pause without resetting.
    pub(crate) fn css_clock(&self, now: f64) -> f64 {
        now - self.paused_ms - self.hidden_at.map_or(0.0, |at| (now - at).max(0.0))
    }

    /// The pane container element.
    pub fn element(&self) -> NodeId {
        self.container
    }

    pub fn settings(&self) -> &ViewSettings {
        &self.settings
    }

    /// What the views wrote (inline style text and classes), for serialization.
    pub fn written(&self) -> &Written {
        &self.written
    }

    /// The region element of a surface.
    pub fn region(&self, key: SurfaceKey, region: Region) -> Option<NodeId> {
        self.surfaces
            .iter()
            .find(|s| s.key == key)
            .and_then(|s| s.regions[region as usize])
    }

    /// The element of a node, by surface and node id.
    pub fn node_element(&self, key: SurfaceKey, id: &str) -> Option<NodeId> {
        let sv = self.surfaces.iter().find(|s| s.key == key)?;
        sv.mounts.get(id).map(|m| m.el)
    }

    /// The program-sheet scope (`data-scope`) of a surface.
    pub fn scope(&self, key: SurfaceKey) -> Option<&str> {
        self.surfaces
            .iter()
            .find(|s| s.key == key)
            .map(|s| s.scope.as_str())
    }

    /// Bring the projection up to date with the pane's changes (drained with
    /// [`TspPane::take_changes`]). `now` is the view clock in ms; ages anchor to it.
    pub fn sync(&mut self, doc: &mut Document, pane: &TspPane, changes: &Changes, now: f64) {
        self.last_now = now;
        // Transitions the new frame starts begin now.
        doc.set_time(self.css_clock(now));
        self.pass += 1;
        if changes.pane {
            let app = pane.app().map(str::to_owned);
            if app != self.app {
                self.app = app;
                for i in 0..self.surfaces.len() {
                    self.write_regions(doc, pane, i);
                }
            }
        }
        for key in &changes.removed {
            self.drop_surface(doc, *key);
        }
        for sc in &changes.surfaces {
            let Some(surface) = pane.surface(sc.key) else {
                self.drop_surface(doc, sc.key);
                continue;
            };
            let i = match self.surfaces.iter().position(|s| s.key == sc.key) {
                Some(i) => i,
                None => self.add_surface(surface),
            };
            if sc.sheets || sc.opened {
                self.sync_sheets(doc, i, surface);
            }
            let fresh = sc.opened;
            // Status strips refit when a strip or a segment changed.
            let sv = &self.surfaces[i];
            if fresh
                || sc.doc.nodes.iter().chain(&sc.doc.children).any(|id| {
                    sv.mounts
                        .get(id)
                        .is_some_and(|m| matches!(m.kind.as_str(), "status" | "seg"))
                })
            {
                self.fit_dirty = true;
            }
            self.sync_doc(doc, pane.blobs(), i, surface, &sc.doc, fresh, now);
            if sc.meta || sc.palette || fresh || sc.doc.children.contains(surface.id()) {
                self.write_regions(doc, pane, i);
            }
            if let Some(r) = &sc.reveal {
                self.surfaces[i].st.reveal = Some(r.clone());
            }
            self.surfaces[i]
                .st
                .scrolls
                .extend(sc.scrolls.iter().cloned());
        }
        // Blobs that arrived redraw the images naming them (§7.12), whenever they arrive.
        if !changes.blobs.is_empty() {
            let arrived: HashSet<String> = changes
                .blobs
                .iter()
                .map(|b| b.to_ascii_lowercase())
                .collect();
            for i in 0..self.surfaces.len() {
                let Some(surface) = pane.surface(self.surfaces[i].key) else {
                    continue;
                };
                let ids: Vec<String> = self.surfaces[i]
                    .mounts
                    .iter()
                    .filter(|(_, m)| m.kind == "image")
                    .filter(|(id, _)| {
                        surface
                            .document()
                            .get(id)
                            .and_then(|n| n.prop("blob"))
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(|b| arrived.contains(&b.to_ascii_lowercase()))
                    })
                    .map(|(id, _)| id.clone())
                    .collect();
                if !ids.is_empty() {
                    self.rerender_ids(doc, pane, i, &ids, now);
                }
            }
        }
        // Opening, closing or covering one surface changes the others' region state.
        if !changes.removed.is_empty() || changes.surfaces.iter().any(|s| s.opened || s.meta) {
            for i in 0..self.surfaces.len() {
                self.write_regions(doc, pane, i);
            }
        }
        self.place_surfaces(doc, pane);
    }

    fn add_surface(&mut self, surface: &Surface) -> usize {
        self.surfaces.push(SurfaceView {
            key: surface.key(),
            id: surface.id().to_owned(),
            scope: format!("sf{}", surface.key().0),
            regions: [None; 3],
            mounts: HashMap::new(),
            sheets: Vec::new(),
            st: SurfaceState::default(),
            clocked: HashSet::new(),
            palette_style: String::new(),
        });
        self.surfaces.len() - 1
    }

    fn drop_surface(&mut self, doc: &mut Document, key: SurfaceKey) {
        let Some(i) = self.surfaces.iter().position(|s| s.key == key) else {
            return;
        };
        let sv = self.surfaces.remove(i);
        for r in sv.regions.into_iter().flatten() {
            if doc.try_node(r).is_some() {
                doc.remove(r);
            }
        }
        for (name, _) in &sv.sheets {
            doc.remove_stylesheet(&sheet_name(&sv.scope, name), Some(&sv.scope));
        }
        self.toasts.forget_surface(key);
    }

    /// Program sheets, scoped to the surface (§7.12), in the order first sent.
    fn sync_sheets(&mut self, doc: &mut Document, i: usize, surface: &Surface) {
        let sv = &mut self.surfaces[i];
        let want: Vec<(String, String)> = surface
            .sheets()
            .iter()
            .map(|(n, c)| (n.to_owned(), c.to_owned()))
            .collect();
        for (name, _) in &sv.sheets {
            if !want.iter().any(|(n, _)| n == name) {
                doc.remove_stylesheet(&sheet_name(&sv.scope, name), Some(&sv.scope));
            }
        }
        for (name, css) in &want {
            if sv.sheets.iter().any(|(n, c)| n == name && c == css) {
                continue;
            }
            // The engine's errors already reached the program through the SheetChecker.
            doc.set_stylesheet(
                &sheet_name(&sv.scope, name),
                auk_ui::SheetKind::Program,
                Some(&sv.scope),
                css,
            );
        }
        sv.sheets = want;
    }

    /// Region element attributes, classes and variables (§7.5, §8.3, §7.12).
    pub(crate) fn write_regions_pub(&mut self, doc: &mut Document, pane: &TspPane, i: usize) {
        self.write_regions(doc, pane, i)
    }

    fn write_regions(&mut self, doc: &mut Document, pane: &TspPane, i: usize) {
        let s = &self.settings;
        let sv = &mut self.surfaces[i];
        let Some(surface) = pane.surface(sv.key) else {
            return;
        };
        let zoom = s.font_px / 13.0;
        let cw = s.cell.0 / zoom;
        let lh = s.cell.1 / zoom;
        let pal = if s.program_colors {
            surface.palette().map(|p| palette::variables(p, s.dark))
        } else {
            None
        };
        let mut style = format!(
            "zoom: {}; --sf-zoom: {}; --sf-cw: {}px; --sf-lh: {}px; ",
            common::fmt_num(zoom as f64),
            common::fmt_num(zoom as f64),
            common::fmt_num(cw as f64),
            common::fmt_num(lh as f64)
        );
        if let Some(p) = &pal {
            style.push_str(p);
        }
        sv.palette_style = style.clone();
        let covered = pane.covered(sv.key);
        let mut dom = Dom {
            doc,
            w: &mut self.written,
        };
        for (r, name) in ["main", "dock", "layer"].iter().enumerate() {
            let Some(el) = sv.regions[r] else { continue };
            let role = surface
                .document()
                .region(name)
                .and_then(|n| common::str_prop(n.props(), "role"))
                .map(str::to_owned);
            let mut class = format!("sf-region sf-{name}");
            if s.reduce_motion {
                class.push_str(" sf-still");
            }
            if !self.visible {
                class.push_str(" sf-paused");
            }
            if !self.focused {
                class.push_str(" sf-unfocused");
            }
            if covered && r < 2 {
                class.push_str(" sf-covered");
            }
            if pal.is_some() {
                class.push_str(" sf-pal");
            }
            if !s.annotated_headings {
                class.push_str(" sf-h-sized");
            } else {
                class.push_str(" sf-h-annotated");
            }
            if surface.suspended() {
                class.push_str(" sf-suspended");
            }
            match surface.mode() {
                Mode::Screen => class.push_str(" sf-screen"),
                Mode::Flow => class.push_str(" sf-flowmode"),
                Mode::Inline => class.push_str(" sf-inline"),
            }
            dom.set_class(el, &class);
            dom.attr(el, "data-scope", &sv.scope);
            dom.attr_opt(el, "data-surface", surface.role());
            dom.attr_opt(el, "data-role", role.as_deref());
            dom.attr_opt(el, "data-app", self.app.as_deref());
            dom.set_style(el, style.trim_end());
        }
    }

    /// Put region elements in the pane container in pane order.
    fn place_surfaces(&mut self, doc: &mut Document, pane: &TspPane) {
        let mut flow = Vec::new();
        let mut docks = Vec::new();
        let mut screen = Vec::new();
        let mut layers = Vec::new();
        for s in pane.surfaces() {
            let Some(sv) = self.surfaces.iter().find(|v| v.key == s.key()) else {
                continue;
            };
            let [main, dock, layer] = sv.regions;
            if s.mode() == Mode::Screen {
                screen.extend(main);
                screen.extend(dock);
            } else {
                flow.extend(main);
                docks.extend(dock);
            }
            layers.extend(layer);
        }
        let mut dom = Dom {
            doc,
            w: &mut self.written,
        };
        // Only move regions the host left in our containers.
        let ours = |dom: &Dom, el: NodeId, c: NodeId| {
            dom.doc.parent(el).is_none() || dom.doc.parent(el) == Some(c)
        };
        for (c, list) in [
            (self.flow, flow),
            (self.docks, docks),
            (self.screen, screen),
            (self.layers, layers),
        ] {
            let list: Vec<NodeId> = list.into_iter().filter(|&e| ours(&dom, e, c)).collect();
            dom.reconcile(c, &list);
        }
        let any_screen = !dom.doc.children(self.screen).is_empty();
        dom.set_class(
            self.screen,
            if any_screen {
                "auk-screen"
            } else {
                "auk-screen sf-hidden"
            },
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn sync_doc(
        &mut self,
        doc: &mut Document,
        blobs: &dyn auk_tsp::BlobStore,
        i: usize,
        surface: &Surface,
        dc: &auk_tsp::doc::DocChanges,
        fresh: bool,
        now: f64,
    ) {
        let settings = &self.settings;
        let cols = self.cols;
        let sv = &mut self.surfaces[i];
        let mut cx = Cx {
            dom: Dom {
                doc,
                w: &mut self.written,
            },
            now,
            set: settings,
            sf: surface,
            st: &mut sv.st,
            cols,
            pass: self.pass,
            blobs,
        };
        let mounts = &mut sv.mounts;
        let clocked = &mut sv.clocked;
        // 1. Removals: the node and every mounted node under it.
        for id in &dc.removed {
            if mounts.contains_key(id.as_str()) {
                unmount(&mut cx, mounts, clocked, &mut sv.regions, id);
            }
        }
        let doc_t = surface.document();
        // 2. The root's children: regions.
        if fresh || dc.children.contains(surface.id()) {
            for name in ["main", "dock", "layer"] {
                let r = region_index(name).unwrap();
                match doc_t.region(name) {
                    Some(node) if !mounts.contains_key(name) => {
                        let el = cx.dom.el("div", &format!("sf-region sf-{name}"));
                        let mut m = Mount::new(el, node.kind(), "div");
                        m.region = true;
                        m.direct = true;
                        sv.regions[r] = Some(el);
                        mounts.insert(name.to_owned(), m);
                        place_children(&mut cx, mounts, clocked, node);
                    }
                    Some(_) => {}
                    None => {
                        if mounts.contains_key(name) {
                            unmount(&mut cx, mounts, clocked, &mut sv.regions, name);
                        }
                        sv.regions[r] = None;
                    }
                }
            }
        }
        // 3. Child lists.
        let mut placed: HashSet<&str> = HashSet::new();
        for id in &dc.children {
            if id == surface.id() {
                continue;
            }
            let Some(node) = doc_t.get(id) else { continue };
            if mounts.contains_key(id.as_str()) {
                place_children(&mut cx, mounts, clocked, node);
                placed.insert(id.as_str());
            }
        }
        // 4. Props and text. A list's `selected` and `filter` show on its items.
        let mut ids: Vec<String> = dc.nodes.iter().cloned().collect();
        for id in &dc.nodes {
            if let Some(n) = doc_t.get(id)
                && n.kind() == "list"
            {
                ids.extend(
                    n.children()
                        .filter(|c| c.kind() == "item" && !dc.nodes.contains(c.id()))
                        .map(|c| c.id().to_owned()),
                );
            }
        }
        for id in &ids {
            let Some(node) = doc_t.get(id) else { continue };
            let Some(mut m) = mounts.remove(id.as_str()) else {
                continue;
            };
            if m.region {
                mounts.insert(id.clone(), m);
                continue;
            }
            if m.stamp == cx.pass {
                mounts.insert(id.clone(), m);
                continue;
            }
            if needs_new_element(&m, node) {
                let new = rebuild_element(&mut cx, &mut m, node);
                m.el = new;
            }
            render_node(&mut cx, &mut m, node);
            if m.clock {
                clocked.insert(id.clone());
            } else {
                clocked.remove(id.as_str());
            }
            mounts.insert(id.clone(), m);
            if !placed.contains(id.as_str()) && kinds::has_children(node.kind()) {
                place_children(&mut cx, mounts, clocked, node);
            }
        }
        // 5. Focus moved: fields redraw their caret.
        if dc.focus || fresh {
            let ids: Vec<String> = mounts
                .iter()
                .filter(|(_, m)| matches!(m.kind.as_str(), "editor" | "input"))
                .map(|(k, _)| k.clone())
                .collect();
            for id in ids {
                if let (Some(node), Some(mut m)) = (doc_t.get(&id), mounts.remove(&id)) {
                    render_node(&mut cx, &mut m, node);
                    mounts.insert(id, m);
                }
            }
        }
        // Later overlays open with their transition (§7.13); what the first draw shows doesn't.
        cx.st.drawn = true;
    }

    /// Re-render some nodes of surface `i`, with their clocks and children.
    fn rerender_ids(
        &mut self,
        doc: &mut Document,
        pane: &TspPane,
        i: usize,
        ids: &[String],
        now: f64,
    ) {
        let Some(surface) = pane.surface(self.surfaces[i].key) else {
            return;
        };
        let settings = &self.settings;
        let cols = self.cols;
        let sv = &mut self.surfaces[i];
        let mut cx = Cx {
            dom: Dom {
                doc: &mut *doc,
                w: &mut self.written,
            },
            now,
            set: settings,
            sf: surface,
            st: &mut sv.st,
            cols,
            pass: self.pass,
            blobs: pane.blobs(),
        };
        for id in ids {
            let (Some(node), Some(mut m)) = (surface.document().get(id), sv.mounts.remove(id))
            else {
                continue;
            };
            render_node(&mut cx, &mut m, node);
            if m.clock {
                sv.clocked.insert(id.clone());
            } else {
                sv.clocked.remove(id);
            }
            sv.mounts.insert(id.clone(), m);
            if kinds::has_children(node.kind()) {
                place_children(&mut cx, &mut sv.mounts, &mut sv.clocked, node);
            }
        }
    }

    /// Re-render every node (settings, theme or width changed).
    pub(crate) fn rerender_all(&mut self, doc: &mut Document, pane: &TspPane, now: f64) {
        for i in 0..self.surfaces.len() {
            let Some(surface) = pane.surface(self.surfaces[i].key) else {
                continue;
            };
            let settings = &self.settings;
            let cols = self.cols;
            let sv = &mut self.surfaces[i];
            let mut cx = Cx {
                dom: Dom {
                    doc: &mut *doc,
                    w: &mut self.written,
                },
                now,
                set: settings,
                sf: surface,
                st: &mut sv.st,
                cols,
                pass: self.pass,
                blobs: pane.blobs(),
            };
            let ids: Vec<String> = sv.mounts.keys().cloned().collect();
            for id in ids {
                let Some(node) = surface.document().get(&id) else {
                    continue;
                };
                let Some(mut m) = sv.mounts.remove(&id) else {
                    continue;
                };
                if !m.region {
                    render_node(&mut cx, &mut m, node);
                    if m.clock {
                        sv.clocked.insert(id.clone());
                    } else {
                        sv.clocked.remove(&id);
                    }
                }
                sv.mounts.insert(id.clone(), m);
                if kinds::has_children(node.kind()) {
                    place_children(&mut cx, &mut sv.mounts, &mut sv.clocked, node);
                }
            }
            self.write_regions(doc, pane, i);
        }
    }
}

pub(crate) use place_children as place_children_pub;

/// Remove a node's element and forget it and every mounted node under it.
fn unmount(
    cx: &mut Cx,
    mounts: &mut HashMap<String, Mount>,
    clocked: &mut HashSet<String>,
    regions: &mut [Option<NodeId>; 3],
    id: &str,
) {
    let Some(m) = mounts.remove(id) else { return };
    clocked.remove(id);
    cx.st.forget(id);
    if m.region
        && let Some(r) = region_index(id)
    {
        regions[r] = None;
    }
    if cx.dom.doc.try_node(m.el).is_none() {
        return;
    }
    // Mounted descendants (their elements carry `data-id`), including slots' children.
    let mut stack: Vec<NodeId> = cx.dom.doc.children(m.el).to_vec();
    stack.extend(m.slots.iter().copied());
    while let Some(e) = stack.pop() {
        if cx.dom.doc.try_node(e).is_none() {
            continue;
        }
        if let Some(cid) = cx.dom.doc.attribute(e, "data-id").map(str::to_owned)
            && let Some(cm) = mounts.remove(&cid)
        {
            clocked.remove(&cid);
            cx.st.forget(&cid);
            stack.extend(cm.slots.iter().copied());
        }
        stack.extend(cx.dom.doc.children(e).iter().copied());
    }
    for s in &m.slots {
        if cx.dom.doc.try_node(*s).is_some() && cx.dom.doc.parent(*s).is_none() {
            cx.dom.doc.remove(*s);
        }
    }
    cx.dom.doc.remove(m.el);
}

pub(crate) fn sheet_name(scope: &str, name: &str) -> String {
    format!("tsp:{scope}:{name}")
}

/// Whether the node needs a new element: an `el` changed its tag.
fn needs_new_element(m: &Mount, node: NodeRef) -> bool {
    kinds::element_tag(node) != m.tag
}

/// Replace a node's element with one of the right tag, keeping its child elements and place.
fn rebuild_element(cx: &mut Cx, m: &mut Mount, node: NodeRef) -> NodeId {
    let tag = kinds::element_tag(node);
    let new = cx.dom.el(&tag, "");
    // Move persistent slots and direct children; generated parts are rebuilt by render.
    for s in std::mem::take(&mut m.slots) {
        cx.dom.doc.detach(s);
        m.slots.push(s);
    }
    let kids: Vec<NodeId> = cx.dom.doc.children(m.el).to_vec();
    for k in kids {
        if m.generated.contains(&k) {
            continue;
        }
        cx.dom.doc.append_child(new, k);
    }
    for g in std::mem::take(&mut m.generated) {
        if cx.dom.doc.try_node(g).is_some() {
            cx.dom.doc.remove(g);
        }
    }
    if let Some(parent) = cx.dom.doc.parent(m.el) {
        cx.dom.doc.insert_before(parent, new, Some(m.el));
    }
    cx.dom.doc.remove(m.el);
    m.tag = tag;
    new
}

/// Create a node's element and render its subtree.
fn mount_node(
    cx: &mut Cx,
    mounts: &mut HashMap<String, Mount>,
    clocked: &mut HashSet<String>,
    node: NodeRef,
) -> NodeId {
    if let Some(m) = mounts.get(node.id()) {
        return m.el;
    }
    let tag = kinds::element_tag(node);
    let el = cx.dom.el(&tag, "");
    let mut m = Mount::new(el, node.kind(), &tag);
    m.stamp = cx.pass;
    render_node(cx, &mut m, node);
    if m.clock {
        clocked.insert(node.id().to_owned());
    }
    mounts.insert(node.id().to_owned(), m);
    if kinds::has_children(node.kind()) {
        place_children(cx, mounts, clocked, node);
    }
    el
}

/// Render a node's own element: classes, attributes, inline style and generated parts.
pub(crate) fn render_node(cx: &mut Cx, m: &mut Mount, node: NodeRef) {
    let mut look = Look::default();
    kinds::render(cx, node, m, &mut look);
    apply_look(cx, m, node, look);
}

fn apply_look(cx: &mut Cx, m: &mut Mount, node: NodeRef, look: Look) {
    let p = node.props();
    let kind = node.kind();
    let mut class = String::from("sf sf-");
    class.push_str(kind);
    if kind == "el" {
        for c in common::str_prop(p, "class")
            .unwrap_or("")
            .split_ascii_whitespace()
        {
            // `sf*` classes are the terminal's.
            if !c.starts_with("sf") {
                class.push(' ');
                class.push_str(c);
            }
        }
    }
    if !look.class.is_empty() {
        class.push(' ');
        class.push_str(&look.class);
    }
    let actions = Actions::read(p, kind);
    if common::bool_prop(p, "hidden") {
        class.push_str(" sf-hidden");
    }
    if actions.clickable() {
        class.push_str(" sf-act");
    }
    let mut style = String::new();
    // `list` reads `max` as its own height, `seg` reads `min.w` as a truncation floor, and
    // `image` bounds its picture with `max.h`.
    let clip = if matches!(kind, "list" | "seg" | "image") {
        let mut q = p.clone();
        if kind == "list" {
            q.remove("max");
        } else if kind == "image" {
            if let Some(serde_json::Value::Object(max)) = q.get_mut("max") {
                max.remove("h");
            }
        } else {
            q.remove("min");
        }
        common::common_style(&q, &mut style)
    } else {
        common::common_style(p, &mut style)
    };
    if clip && kind != "ansi" {
        class.push_str(" sf-clip");
    }
    style.push_str(&look.style);
    cx.dom.set_class(m.el, &class);
    cx.dom.set_style(m.el, style.trim_end());
    let mut attrs: Vec<(String, String)> = vec![("data-id".into(), node.id().to_owned())];
    if let Some(r) = common::str_prop(p, "role") {
        attrs.push(("data-role".into(), r.to_owned()));
    }
    if let Some(t) = common::tone(p) {
        attrs.push(("data-tone".into(), t.to_owned()));
    }
    if let Some(mk) = common::str_prop(p, "mark") {
        attrs.push(("data-mark".into(), mk.to_owned()));
    }
    if let Some(h) = common::str_prop(p, "href") {
        attrs.push(("data-href".into(), h.to_owned()));
    }
    if !look.title_is_content
        && let Some(t) = common::str_prop(p, "title")
    {
        attrs.push(("title".into(), t.to_owned()));
    }
    if let Some(a) = common::str_prop(p, "aria") {
        attrs.push(("aria-label".into(), a.to_owned()));
    }
    for (k, v) in look.attrs {
        match attrs.iter_mut().find(|(n, _)| *n == k) {
            Some(slot) => slot.1 = v,
            None => attrs.push((k, v)),
        }
    }
    for old in std::mem::take(&mut m.attrs) {
        if !attrs.iter().any(|(n, _)| *n == old) {
            cx.dom.attr_opt(m.el, &old, None);
        }
    }
    for (k, v) in &attrs {
        cx.dom.attr(m.el, k, v);
    }
    m.attrs = attrs.into_iter().map(|(k, _)| k).collect();
}

/// Mount new children and put every child element where its kind routes it.
pub(crate) fn place_children(
    cx: &mut Cx,
    mounts: &mut HashMap<String, Mount>,
    clocked: &mut HashSet<String>,
    node: NodeRef,
) {
    if !kinds::has_children(node.kind()) && !mounts.get(node.id()).is_some_and(|m| m.region) {
        return;
    }
    let mut per_slot: Vec<Vec<NodeId>> = Vec::new();
    let mut direct: Vec<NodeId> = Vec::new();
    let (slots, is_direct) = match mounts.get(node.id()) {
        Some(m) => (m.slots.clone(), m.direct),
        None => return,
    };
    per_slot.resize(slots.len(), Vec::new());
    for child in node.children() {
        let route = if is_direct {
            kinds::Route::Direct
        } else {
            kinds::route(node, child)
        };
        let el = mount_node(cx, mounts, clocked, child);
        match route {
            kinds::Route::Direct => direct.push(el),
            kinds::Route::Slot(i) if i < per_slot.len() => per_slot[i].push(el),
            _ => {}
        }
    }
    let Some(m) = mounts.get_mut(node.id()) else {
        return;
    };
    if m.direct {
        let mut want = m.generated.clone();
        want.extend(direct);
        cx.dom.reconcile(m.el, &want);
    }
    for (s, want) in m.slots.clone().into_iter().zip(per_slot) {
        cx.dom.reconcile(s, &want);
    }
    let el = m.el;
    let kind = m.kind.clone();
    let region = m.region;
    // Long `main` and `col` virtualize (§8.3: `col` over 64 children, unless it wraps or
    // justifies).
    if region && node.id() == "main" || kind == "col" {
        let p = node.props();
        let plain =
            region || (!common::bool_prop(p, "wrap") && common::str_prop(p, "justify").is_none());
        let long = node.child_count() > 64 && plain;
        kinds::virtualize(cx, node.id(), el, long.then_some(60.0));
    }
    kinds::after_children(cx, node, &kind, el, mounts);
}
