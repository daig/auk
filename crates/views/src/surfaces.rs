//! A pane's surfaces composited with its grid (§6.2), shared by every window that draws: the app,
//! the headless window and the web client.
//!
//! [`PaneSurfaces`] owns the pane's UI document and its [`PaneView`]. Each frame the host
//! 1. feeds it the replica's TSP changes ([`PaneSurfaces::sync`]),
//! 2. lays the document out at the pane's size ([`PaneSurfaces::layout`]),
//! 3. arranges the pane ([`PaneSurfaces::arrange`]): inline and flow surfaces become blocks at
//!    their anchor rows in the [`compose`](crate::compose) flow, docks sit at the pane's bottom, a
//!    screen surface covers the pane and layers overlay it,
//! 4. draws the grid rows where [`Arranged::composed`] put them, and
//! 5. paints the surfaces into a Vello scene over the grid ([`PaneSurfaces::paint`]).
//!
//! Pixels are device pixels; the document is in points (CSS px), `scale` pixels each.

use auk_term::{Line, Terminal};
use auk_tsp::{Changes, Mode, TspPane};
use auk_ui::{Document, NodeId, Rect, SheetKind};
use vello::Scene;
use vello::kurbo::{Affine, Rect as KRect};
use vello::peniko::Fill;

use crate::compose::{self, Block, Composed, Flow, Hit, Scroll};
use crate::{PaneView, Region, ViewSettings};

/// The host's own CSS around a pane's surfaces. The pane fills the content area at least, so a
/// screen surface or a layer (absolutely placed over the pane) covers it and takes its clicks.
const HOST_CSS: &str = "body { margin: 0; } .auk-pane { min-height: 100vh; }";

/// The pane container's children, in paint order.
const CONTAINERS: [&str; 4] = ["auk-flow", "auk-docks", "auk-screen", "auk-layers"];

/// One pane's surfaces: its document and the view projecting the replica's TSP state into it.
pub struct PaneSurfaces {
    doc: Document,
    view: PaneView,
    /// The pane's command blocks (§9.6), once it has any.
    lens: Option<crate::lenses::LensLayer>,
    /// Plugin sheets (§8.4 cascade step 3), installed after the built-in ones.
    plugin_sheets: Vec<(String, String)>,
}

impl std::fmt::Debug for PaneSurfaces {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PaneSurfaces").finish_non_exhaustive()
    }
}

/// A surface block in the flow.
#[derive(Clone, Debug, PartialEq)]
pub struct SurfaceBlock {
    /// The surface's id.
    pub sf: String,
    /// The anchor row's buffer line.
    pub line: Line,
    /// The surface's main region element.
    pub el: NodeId,
    /// Its height, in pixels.
    pub height: f32,
}

/// Where a pane's content goes this frame.
#[derive(Clone, Debug, Default)]
pub struct Arranged {
    /// The grid rows and blocks in the flow area (the content area less the dock).
    pub composed: Composed,
    /// The blocks, in line order; [`Composed::blocks`] indexes into these.
    pub blocks: Vec<SurfaceBlock>,
    /// The docks container and its box (points), when something is docked.
    pub dock: Option<(NodeId, Rect)>,
    /// The dock's height, in pixels.
    pub dock_px: f32,
    /// The screen surface's container and box, when a screen surface covers the pane. The grid
    /// isn't drawn then.
    pub screen: Option<(NodeId, Rect)>,
    /// Whether a layer is up.
    pub layers: bool,
    /// The flow area's height, in pixels.
    pub flow_px: f32,
    /// The cell height, in pixels.
    pub cell_px: f32,
}

impl Arranged {
    /// The flow's blocks, for [`Composed::hit`].
    pub fn flow_blocks(&self) -> Vec<Block> {
        self.blocks
            .iter()
            .map(|b| Block {
                line: b.line,
                height: b.height,
            })
            .collect()
    }

    /// Whether the grid shows: no screen surface covers the pane.
    pub fn grid_shown(&self) -> bool {
        self.screen.is_none()
    }

    /// What is under a point `y` pixels below the top of the content area, in the flow. `None`
    /// over the dock.
    pub fn hit(&self, y: f32) -> Option<Hit> {
        if y >= self.flow_px {
            return None;
        }
        self.composed.hit(y, &self.flow_blocks())
    }
}

/// Where a pane's content area is in the target, in pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Place {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    /// Pixels per point.
    pub scale: f64,
}

impl PaneSurfaces {
    /// A pane's document with the built-in sheets for `settings`, at `scale` pixels per point.
    pub fn new(settings: ViewSettings, scale: f32) -> PaneSurfaces {
        let mut doc = Document::new();
        doc.set_device_scale(scale);
        let errors = crate::install_sheets(&mut doc, settings.dark);
        debug_assert!(errors.is_empty(), "built-in sheets: {errors:?}");
        doc.set_stylesheet("host", SheetKind::BuiltIn, None, HOST_CSS);
        let root = doc.root();
        let body = doc.create_element("body");
        doc.append_child(root, body);
        let view = PaneView::new(&mut doc, settings);
        let el = view.element();
        doc.append_child(body, el);
        PaneSurfaces {
            doc,
            view,
            lens: None,
            plugin_sheets: Vec::new(),
        }
    }

    /// Installs plugin sheets (manifest styles and `tern.css`, §8.4 cascade step 3) into the
    /// pane's documents, replacing the last set. Returns whether anything changed.
    pub fn set_plugin_sheets(&mut self, sheets: &[(String, String)]) -> bool {
        if self.plugin_sheets == sheets {
            return false;
        }
        for (name, _) in &self.plugin_sheets {
            self.doc.remove_stylesheet(name, None);
        }
        for (name, css) in sheets {
            self.doc.set_stylesheet(name, SheetKind::Plugin, None, css);
        }
        if let Some(l) = &mut self.lens {
            l.set_plugin_sheets(sheets);
        }
        self.plugin_sheets = sheets.to_vec();
        true
    }

    /// The plugin sheets installed, in order.
    pub fn plugin_sheets(&self) -> Vec<String> {
        self.plugin_sheets.iter().map(|(n, _)| n.clone()).collect()
    }

    /// Takes the replica's command blocks (§9.6): each draws at its anchor row as a `block`.
    /// Returns whether the drawn blocks changed.
    pub fn sync_lenses(&mut self, blocks: &[crate::lenses::LensInput], now: f64) -> bool {
        if blocks.is_empty() && self.lens.as_ref().is_none_or(|l| l.is_empty()) {
            return false;
        }
        let settings = self.view.settings().clone();
        let scale = self.doc.device_scale();
        let sheets = self.plugin_sheets.clone();
        let layer = self.lens.get_or_insert_with(|| {
            let mut l = crate::lenses::LensLayer::new(settings, scale);
            l.set_plugin_sheets(&sheets);
            l
        });
        layer.sync(blocks, now)
    }

    /// The command blocks' layer, once the pane has had any.
    pub fn lenses(&self) -> Option<&crate::lenses::LensLayer> {
        self.lens.as_ref()
    }

    /// A press in a command block, at a point [`PaneSurfaces::lens_point`] found.
    pub fn lens_click(&mut self, ev: crate::PointerEvent) -> Vec<crate::Outcome> {
        self.lens.as_mut().map(|l| l.click(ev)).unwrap_or_default()
    }

    /// Hover in the command blocks at a lens document point (`None`: nowhere over them).
    /// Returns whether the hover changed.
    pub fn lens_hover(&mut self, at: Option<(f32, f32)>) -> bool {
        let Some(l) = &mut self.lens else {
            return false;
        };
        let (x, y) = at.unwrap_or((-1.0, -1.0));
        l.hover(x, y)
    }

    /// Whether a flow block is a command block (and which).
    fn lens_block(&self, b: &SurfaceBlock) -> Option<u64> {
        let id: u64 = b.sf.strip_prefix(crate::lenses::LENS_PREFIX)?.parse().ok()?;
        let l = self.lens.as_ref()?;
        (l.element(&format!("lens{id}")) == Some(b.el)).then_some(id)
    }

    /// The command-block document point under `(x, y)` (pixels from the content area's top-left),
    /// if a command block is there.
    pub fn lens_point(
        &self,
        arranged: &Arranged,
        x: f32,
        y: f32,
        scale: f32,
    ) -> Option<(f32, f32)> {
        let Some(Hit::Block(i, off)) = arranged.hit(y) else {
            return None;
        };
        let b = arranged.blocks.get(i)?;
        self.lens_block(b)?;
        let top = self.lens.as_ref()?.doc().border_box(b.el)?.y;
        Some((x / scale, top + off / scale))
    }

    /// Makes a font (a TTF/OTF file's bytes) every generic family of the document: for hosts
    /// without system fonts (the browser client), so surfaces' text draws in the one font the
    /// host has.
    pub fn use_font(&mut self, data: Vec<u8>) {
        use parley::fontique::GenericFamily;
        let collection = &mut self.doc.fonts_mut().font_cx.collection;
        let blob = parley::fontique::Blob::new(std::sync::Arc::new(data));
        let ids: Vec<_> = collection
            .register_fonts(blob, None)
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        if ids.is_empty() {
            return;
        }
        for generic in [
            GenericFamily::Serif,
            GenericFamily::SansSerif,
            GenericFamily::Monospace,
            GenericFamily::SystemUi,
            GenericFamily::UiSerif,
            GenericFamily::UiSansSerif,
            GenericFamily::UiMonospace,
            GenericFamily::Emoji,
        ] {
            collection.set_generic_families(generic, ids.iter().copied());
        }
        self.doc.update();
    }

    pub fn doc(&self) -> &Document {
        &self.doc
    }

    pub fn view(&self) -> &PaneView {
        &self.view
    }

    /// The document and the view, for input routing ([`PaneView::click`] and friends).
    pub fn parts(&mut self) -> (&mut Document, &mut PaneView) {
        (&mut self.doc, &mut self.view)
    }

    /// Takes a batch of the replica's TSP changes.
    pub fn sync(&mut self, tsp: &TspPane, changes: &Changes, now: f64) {
        self.view.sync(&mut self.doc, tsp, changes, now);
    }

    /// Applies window settings (appearance, cell size) and the scale; re-renders on a change.
    pub fn configure(&mut self, tsp: &TspPane, settings: ViewSettings, scale: f32) {
        if let Some(l) = &mut self.lens {
            l.configure(settings.clone(), scale);
        }
        if self.view.settings() != &settings {
            crate::sheets::set_theme(&mut self.doc, settings.dark);
            self.view.set_settings(&mut self.doc, tsp, settings);
        }
        self.doc.set_device_scale(scale);
    }

    /// Lays the document out for a content area of `size` points. Returns when the view next
    /// wants a frame (motion), in the clock's seconds.
    pub fn layout(
        &mut self,
        term: &Terminal,
        tsp: &TspPane,
        size: (f32, f32),
        focused: bool,
        now: f64,
    ) -> Option<f64> {
        self.view.set_cols(&mut self.doc, tsp, term.cols());
        self.view.set_focused(&mut self.doc, tsp, focused);
        self.doc.set_viewport(size.0, size.1);
        let next = self.view.tick(&mut self.doc, tsp, now);
        self.view.layout(&mut self.doc);
        let lens_next = self
            .lens
            .as_mut()
            .filter(|l| !l.is_empty())
            .and_then(|l| l.layout(term.cols(), size.0, now));
        match (next, lens_next) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// The pane container's child with this class (`auk-flow`, `auk-docks`, …).
    pub fn container(&self, class: &str) -> Option<NodeId> {
        self.doc
            .element_children(self.view.element())
            .into_iter()
            .find(|&el| {
                self.view
                    .written()
                    .class(el)
                    .is_some_and(|c| c.split_whitespace().next() == Some(class))
            })
    }

    /// Whether the view hid this element (`sf-hidden`).
    pub fn hidden(&self, el: NodeId) -> bool {
        self.view
            .written()
            .class(el)
            .is_some_and(|c| c.split_whitespace().any(|w| w == "sf-hidden"))
    }

    /// The containers a paint pass for `keep` leaves out: every other one.
    fn others(&self, keep: &str) -> Vec<NodeId> {
        CONTAINERS
            .iter()
            .filter(|&&class| class != keep)
            .filter_map(|class| self.container(class))
            .collect()
    }

    /// Arranges the pane after [`PaneSurfaces::layout`]: the flow for a content area `content_px`
    /// pixels high with cells `cell_px` high, at `scale` pixels per point.
    pub fn arrange(
        &self,
        term: &Terminal,
        tsp: &TspPane,
        cell_px: f32,
        content_px: f32,
        scale: f32,
        scroll: Scroll,
    ) -> Arranged {
        let main_screen = !term.modes().alt_screen;
        let mut blocks = Vec::new();
        // Stray output under a live inline surface stays hidden until the surface closes or
        // suspends (§7.3): the lines below its anchor take no space.
        let hide_from = tsp
            .surfaces()
            .iter()
            .filter(|sf| main_screen && sf.mode() == Mode::Inline && sf.is_live())
            .filter(|sf| !sf.suspended())
            .filter_map(|sf| term.anchor_line(sf.anchor()?))
            .max()
            .map(|line| line + 1);
        for sf in tsp.surfaces() {
            if sf.mode() == Mode::Screen || !main_screen {
                continue;
            }
            let (Some(anchor), Some(el)) = (sf.anchor(), self.view.region(sf.key(), Region::Main))
            else {
                continue;
            };
            let (Some(line), Some(b)) = (term.anchor_line(anchor), self.doc.border_box(el)) else {
                continue;
            };
            if self.hidden(el) || b.h <= 0.0 {
                continue;
            }
            blocks.push(SurfaceBlock {
                sf: sf.id().to_owned(),
                line,
                el,
                height: b.h * scale,
            });
        }
        if main_screen && let Some(l) = &self.lens {
            for p in l.placed(term, scale) {
                blocks.push(SurfaceBlock {
                    sf: format!("{}{}", crate::lenses::LENS_PREFIX, p.id),
                    line: p.line,
                    el: p.el,
                    height: p.height,
                });
            }
        }
        blocks.sort_by_key(|b| b.line);
        // One block per line: the first surface anchored there wins.
        blocks.dedup_by_key(|b| b.line);
        let dock = self
            .container("auk-docks")
            .and_then(|el| Some((el, self.doc.border_box(el)?)))
            .filter(|(_, b)| b.h > 0.0);
        let screen = self
            .container("auk-screen")
            .filter(|&el| !self.hidden(el))
            .and_then(|el| Some((el, self.doc.border_box(el)?)));
        let layers = self
            .container("auk-layers")
            .is_some_and(|el| !self.doc.children(el).is_empty());

        let dock_px = dock.map_or(0.0, |(_, b)| b.h * scale);
        let flow_px = (content_px - dock_px).max(cell_px);
        let flow_blocks: Vec<Block> = blocks
            .iter()
            .map(|b| Block {
                line: b.line,
                height: b.height,
            })
            .collect();
        let flow = Flow {
            lines: term.history_len() + usize::from(term.rows()),
            screen_top: term.history_len(),
            tail: compose::tail_line(term, &flow_blocks),
            cell: cell_px,
            view: flow_px,
            blocks: &flow_blocks,
            hide_from,
        };
        Arranged {
            composed: compose::compose(&flow, scroll),
            blocks,
            dock,
            dock_px,
            screen,
            layers,
            flow_px,
            cell_px,
        }
    }

    /// Paints the surfaces where `arranged` put them, into `scene`, for a content area at
    /// `place`. One pass per container (flow, docks, screen, layers), the others hidden, each
    /// region clipped to where it goes. Returns whether anything was painted.
    pub fn paint(&mut self, arranged: &Arranged, scene: &mut Scene, place: Place) -> bool {
        // (element top in the document, where that top goes, the clip)
        type Op = (f32, f64, KRect);
        let s = place.scale;
        let content = KRect::new(place.x, place.y, place.x + place.w, place.y + place.h);
        let flow_bottom = content.y1 - f64::from(arranged.dock_px);
        let mut flow_ops: Vec<Op> = Vec::new();
        let mut lens_ops: Vec<Op> = Vec::new();
        for &(i, y) in &arranged.composed.blocks {
            let b = &arranged.blocks[i];
            let lens = self.lens_block(b).is_some();
            let el_top = if lens {
                self.lens.as_ref().and_then(|l| l.doc().border_box(b.el))
            } else {
                self.doc.border_box(b.el)
            }
            .map_or(0.0, |r| r.y);
            let at = place.y + f64::from(y);
            let clip = KRect::new(
                content.x0,
                at.max(content.y0),
                content.x1,
                (at + f64::from(b.height)).min(flow_bottom),
            );
            if clip.y1 > clip.y0 {
                if lens {
                    lens_ops.push((el_top, at, clip));
                } else {
                    flow_ops.push((el_top, at, clip));
                }
            }
        }
        // Command blocks paint from their own document.
        let lens_painted = !lens_ops.is_empty();
        if let Some(l) = self.lens.as_ref().filter(|_| lens_painted) {
            for (el_top, at, clip) in lens_ops {
                scene.push_clip_layer(Fill::NonZero, Affine::IDENTITY, &clip);
                let xf =
                    Affine::translate((place.x, at - f64::from(el_top) * s)) * Affine::scale(s);
                let cull = Rect::new(
                    ((clip.x0 - place.x) / s) as f32,
                    el_top + ((clip.y0 - at) / s) as f32,
                    (clip.width() / s) as f32,
                    (clip.height() / s) as f32,
                );
                auk_ui::paint::paint_except(l.doc(), scene, xf, &[], Some(cull));
                scene.pop_layer();
            }
        }
        let mut passes: Vec<(&str, Vec<Op>)> = vec![("auk-flow", flow_ops)];
        if let Some((_, b)) = arranged.dock {
            passes.push((
                "auk-docks",
                vec![(
                    b.y,
                    flow_bottom,
                    KRect::new(content.x0, flow_bottom, content.x1, content.y1),
                )],
            ));
        }
        if let Some((_, b)) = arranged.screen {
            passes.push(("auk-screen", vec![(b.y, content.y0, content)]));
        }
        if arranged.layers {
            // Layers keep their place in the pane container, over the pane.
            let pane_top = self
                .doc
                .border_box(self.view.element())
                .map_or(0.0, |r| r.y);
            passes.push(("auk-layers", vec![(pane_top, content.y0, content)]));
        }
        if passes.iter().all(|(_, ops)| ops.is_empty()) {
            return lens_painted;
        }
        for (keep, ops) in passes {
            if ops.is_empty() {
                continue;
            }
            // Each pass paints its container alone; the others are left out of the paint (not
            // hidden through styles, which would restyle and lay the document out twice a frame).
            let skip = self.others(keep);
            for (el_top, at, clip) in ops {
                scene.push_clip_layer(Fill::NonZero, Affine::IDENTITY, &clip);
                let xf =
                    Affine::translate((place.x, at - f64::from(el_top) * s)) * Affine::scale(s);
                // The clip in document coordinates: only what can show there is painted.
                let cull = Rect::new(
                    ((clip.x0 - place.x) / s) as f32,
                    el_top + ((clip.y0 - at) / s) as f32,
                    (clip.width() / s) as f32,
                    (clip.height() / s) as f32,
                );
                auk_ui::paint::paint_except(&self.doc, scene, xf, &skip, Some(cull));
                scene.pop_layer();
            }
        }
        true
    }

    /// The document point under a point `(x, y)` pixels from the content area's top-left, if a
    /// surface is there: a layer first, then a screen surface, the dock, or a block in the flow.
    pub fn doc_point(
        &self,
        arranged: &Arranged,
        x: f32,
        y: f32,
        content_px: f32,
        scale: f32,
    ) -> Option<(f32, f32)> {
        let dx = x / scale;
        if arranged.layers
            && let Some(layers) = self.container("auk-layers")
        {
            let pane_top = self
                .doc
                .border_box(self.view.element())
                .map_or(0.0, |r| r.y);
            let py = pane_top + y / scale;
            if let Some(hit) = auk_ui::hit_test(&self.doc, dx, py)
                && self.inside(hit, layers)
            {
                return Some((dx, py));
            }
        }
        if let Some((_, b)) = arranged.screen {
            return Some((dx, b.y + y / scale));
        }
        let flow_bottom = content_px - arranged.dock_px;
        if let Some((_, b)) = arranged.dock
            && y >= flow_bottom
        {
            return Some((dx, b.y + (y - flow_bottom) / scale));
        }
        match arranged.hit(y)? {
            Hit::Block(i, off) => {
                let b = arranged.blocks.get(i)?;
                if self.lens_block(b).is_some() {
                    return None;
                }
                let el = b.el;
                let top = self.doc.border_box(el)?.y;
                Some((dx, top + off / scale))
            }
            Hit::Line(..) => None,
        }
    }

    fn inside(&self, mut el: NodeId, ancestor: NodeId) -> bool {
        loop {
            if el == ancestor {
                return true;
            }
            match self.doc.parent(el) {
                Some(p) => el = p,
                None => return false,
            }
        }
    }
}
