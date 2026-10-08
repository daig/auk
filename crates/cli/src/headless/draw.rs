//! Offscreen drawing for a headless window (§11.3, §21): the shown tab's panes laid out in a
//! window of a given size, each pane's grid through `auk-render`'s `GridRenderer` and its surfaces
//! through `auk-views` into an `auk-ui` document painted with Vello, composited as a window does
//! (§6.2): inline and flow surfaces take their natural height in place of their anchor rows, docks
//! sit at the pane's bottom, a screen surface covers the pane and layers overlay it. The result is
//! a PNG and a layout JSON describing where everything went.
//!
//! Everything here runs on the window's draw thread; the GPU objects never leave it.

use std::collections::BTreeMap;
use std::time::Instant;

use auk_mirror::PaneId;
use auk_mirror::layout::{Node, SplitDir, Tab, Window};
use auk_render::vello::Scene;
use auk_render::{
    GridRenderer, GridSnapshot, Image, OFFSCREEN_FORMAT, OffscreenTarget, RowSlot, Theme, UiLayer,
    View as GridView,
};
use auk_text::{FontConfig, GridFonts};
use auk_tsp::Changes;
use auk_ui::{Document, NodeId};
use auk_views::compose::Scroll;
use auk_views::{PaneSurfaces, Place, Region, ViewSettings};
use serde_json::{Value, json};

/// Points between a pane's edge and its grid.
pub const PADDING: f64 = 6.0;
/// Points between split panes.
pub const DIVIDER: f64 = 1.0;
/// The grid's font size, in points.
pub const FONT_SIZE: f32 = 13.0;

/// The window a headless window pretends to be.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowSpec {
    /// Points.
    pub width: u32,
    pub height: u32,
    /// Pixels per point.
    pub scale: f32,
    pub dark: bool,
}

impl Default for WindowSpec {
    fn default() -> Self {
        WindowSpec {
            width: 960,
            height: 600,
            scale: 1.0,
            dark: true,
        }
    }
}

impl WindowSpec {
    /// Parses `WxH[@S]`.
    pub fn parse_size(&mut self, s: &str) -> Result<(), String> {
        let (dims, scale) = match s.split_once('@') {
            Some((d, sc)) => (d, Some(sc)),
            None => (s, None),
        };
        let (w, h) = dims
            .split_once('x')
            .and_then(|(w, h)| Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?)))
            .filter(|&(w, h)| (64..=8192).contains(&w) && (64..=8192).contains(&h))
            .ok_or_else(|| format!("expected WxH[@S], got `{s}`"))?;
        let scale = match scale {
            Some(sc) => sc
                .parse::<f32>()
                .ok()
                .filter(|v| (0.5..=4.0).contains(v))
                .ok_or_else(|| format!("bad scale `{sc}`"))?,
            None => self.scale,
        };
        (self.width, self.height, self.scale) = (w, h, scale);
        Ok(())
    }

    fn px(&self) -> (u32, u32) {
        (
            (self.width as f32 * self.scale).round() as u32,
            (self.height as f32 * self.scale).round() as u32,
        )
    }
}

/// A rectangle in points.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    fn json(&self) -> Value {
        json!([r2(self.x), r2(self.y), r2(self.w), r2(self.h)])
    }
}

fn r2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// The tab a window shows: the one holding `focused`, else the first session's first tab.
pub fn shown_tab(window: &Window, focused: Option<PaneId>) -> Option<(u64, &Tab)> {
    let all = || {
        window
            .sessions
            .iter()
            .flat_map(|s| s.tabs.iter().map(move |t| (s.id, t)))
    };
    focused
        .and_then(|f| all().find(|(_, t)| t.root.contains(f)))
        .or_else(|| all().next())
}

/// A tab's panes laid out in `area` (zoom shows only the zoomed pane).
pub fn layout_tab(tab: &Tab, area: Rect) -> Vec<(PaneId, Rect)> {
    let mut out = Vec::new();
    match tab.zoom.filter(|z| tab.root.contains(*z)) {
        Some(z) => out.push((z, area)),
        None => layout_node(&tab.root, area, &mut out),
    }
    out
}

fn layout_node(node: &Node, area: Rect, out: &mut Vec<(PaneId, Rect)>) {
    match node {
        Node::Pane { pane } => out.push((*pane, area)),
        Node::Split {
            dir,
            ratio,
            first,
            second,
        } => {
            let share = f64::from((*ratio).clamp(1, 999)) / 1000.0;
            match dir {
                SplitDir::Right => {
                    let avail = (area.w - DIVIDER).max(0.0);
                    let w1 = (avail * share).round();
                    layout_node(first, Rect { w: w1, ..area }, out);
                    let x = area.x + w1 + DIVIDER;
                    layout_node(
                        second,
                        Rect {
                            x,
                            w: (avail - w1).max(0.0),
                            ..area
                        },
                        out,
                    );
                }
                SplitDir::Down => {
                    let avail = (area.h - DIVIDER).max(0.0);
                    let h1 = (avail * share).round();
                    layout_node(first, Rect { h: h1, ..area }, out);
                    let y = area.y + h1 + DIVIDER;
                    layout_node(
                        second,
                        Rect {
                            y,
                            h: (avail - h1).max(0.0),
                            ..area
                        },
                        out,
                    );
                }
            }
        }
    }
}

/// One pane's surfaces document.
type PaneDoc = PaneSurfaces;

/// What a pane needs to draw, gathered by the window under its lock.
#[derive(Debug)]
pub struct PaneInput<'a> {
    pub pane: PaneId,
    pub replica: &'a auk_replica::Replica,
    /// TSP changes since the last draw, in order.
    pub changes: Vec<Changes>,
}

/// One shot's timings.
#[derive(Clone, Copy, Debug, Default)]
pub struct Timing {
    pub cpu_ms: f64,
    pub readback_ms: f64,
}

/// The draw thread's state: the GPU, a grid renderer per pane, the UI layer and a document per
/// pane.
pub struct Drawer {
    gpu: auk_render::Gpu,
    spec: WindowSpec,
    grids: BTreeMap<PaneId, GridRenderer>,
    ui: UiLayer,
    docs: BTreeMap<PaneId, PaneDoc>,
    pub timings: Vec<Timing>,
}

fn fonts(scale: f32) -> Result<GridFonts, String> {
    GridFonts::new(&FontConfig {
        size_pt: FONT_SIZE,
        scale,
        ..FontConfig::default()
    })
    .map_err(|e| format!("grid fonts: {e:?}"))
}

impl std::fmt::Debug for Drawer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Drawer")
            .field("spec", &self.spec)
            .field("panes", &self.docs.len())
            .finish_non_exhaustive()
    }
}

impl Drawer {
    pub fn new(spec: WindowSpec) -> Result<Drawer, String> {
        let gpu = auk_render::Gpu::new_blocking().map_err(|e| format!("GPU: {e}"))?;
        let ui = UiLayer::new(&gpu, OFFSCREEN_FORMAT).map_err(|e| e.to_string())?;
        Ok(Drawer {
            gpu,
            spec,
            grids: BTreeMap::new(),
            ui,
            docs: BTreeMap::new(),
            timings: Vec::new(),
        })
    }

    /// The cell size in pixels at the current scale.
    pub fn cell(&mut self) -> Result<(u32, u32), String> {
        let m = fonts(self.spec.scale)?.metrics();
        Ok((m.cell_width, m.cell_height))
    }

    fn settings(&self, cell: (u32, u32)) -> ViewSettings {
        ViewSettings {
            font_px: FONT_SIZE,
            cell: (
                cell.0 as f32 / self.spec.scale,
                cell.1 as f32 / self.spec.scale,
            ),
            dark: self.spec.dark,
            ..ViewSettings::default()
        }
    }

    /// Changes the window: new grid fonts on a scale change, restyled documents on an
    /// appearance change.
    pub fn set_spec(&mut self, spec: WindowSpec) {
        if spec.scale != self.spec.scale {
            self.grids.clear();
        }
        self.spec = spec;
    }

    /// Takes changes for panes that aren't drawn now, so their documents stay current.
    pub fn absorb(&mut self, inputs: &[PaneInput<'_>], now: f64) -> Result<(), String> {
        let cell = self.cell()?;
        for input in inputs {
            self.sync(input, cell, now);
        }
        Ok(())
    }

    fn sync(&mut self, input: &PaneInput<'_>, cell: (u32, u32), now: f64) -> &mut PaneDoc {
        let settings = self.settings(cell);
        let scale = self.spec.scale;
        let pd = self
            .docs
            .entry(input.pane)
            .or_insert_with(|| PaneSurfaces::new(settings.clone(), scale));
        let tsp = input.replica.tsp();
        for c in &input.changes {
            pd.sync(tsp, c, now);
        }
        pd.configure(tsp, settings, scale);
        pd
    }

    /// Forgets panes that are gone.
    pub fn retain(&mut self, alive: &dyn Fn(PaneId) -> bool) {
        self.docs.retain(|p, _| alive(*p));
        self.grids.retain(|p, _| alive(*p));
    }

    /// The grid size each shown pane should have: its rectangle less the padding, in cells.
    pub fn fit(
        &mut self,
        window: &Window,
        focused: Option<PaneId>,
    ) -> Result<Vec<(PaneId, u16, u16)>, String> {
        let (cw, ch) = self.cell()?;
        let s = f64::from(self.spec.scale);
        let Some((_, tab)) = shown_tab(window, focused) else {
            return Ok(Vec::new());
        };
        let area = Rect {
            x: 0.0,
            y: 0.0,
            w: f64::from(self.spec.width),
            h: f64::from(self.spec.height),
        };
        Ok(layout_tab(tab, area)
            .into_iter()
            .map(|(pane, r)| {
                let cols = (((r.w - 2.0 * PADDING) * s).max(0.0) as u32 / cw).clamp(1, 1000);
                let rows = (((r.h - 2.0 * PADDING) * s).max(0.0) as u32 / ch).clamp(1, 1000);
                (pane, cols as u16, rows as u16)
            })
            .collect())
    }

    /// Draws the window: the image and the layout.
    pub fn draw(
        &mut self,
        window: &Window,
        focused: Option<PaneId>,
        inputs: &[PaneInput<'_>],
        now: f64,
    ) -> Result<(Image, Value), String> {
        let start = Instant::now();
        let spec = self.spec;
        let s = f64::from(spec.scale);
        let (wpx, hpx) = spec.px();
        let theme = if spec.dark {
            Theme::dark()
        } else {
            Theme::light()
        };
        let cell = self.cell()?;
        let (cw, ch) = (cell.0 as f32, cell.1 as f32);
        let shown = shown_tab(window, focused);
        let area = Rect {
            x: 0.0,
            y: 0.0,
            w: f64::from(spec.width),
            h: f64::from(spec.height),
        };
        let rects = shown.map(|(_, t)| layout_tab(t, area)).unwrap_or_default();

        // Every pane's document takes its changes, shown or not.
        for input in inputs {
            self.sync(input, cell, now);
        }

        let mut scene = Scene::new();
        let mut grids_drawn: Vec<(PaneId, (u32, u32, u32, u32))> = Vec::new();
        let mut panes_json = Vec::new();
        for (pane, rect) in &rects {
            let Some(input) = inputs.iter().find(|i| i.pane == *pane) else {
                panes_json.push(json!({"pane": pane, "rect": rect.json(), "missing": true}));
                continue;
            };
            let term = input.replica.terminal();
            let tsp = input.replica.tsp();
            let content = Rect {
                x: rect.x + PADDING,
                y: rect.y + PADDING,
                w: (rect.w - 2.0 * PADDING).max(0.0),
                h: (rect.h - 2.0 * PADDING).max(0.0),
            };
            let pd = self.docs.get_mut(pane).expect("synced above");
            pd.layout(
                term,
                tsp,
                (content.w as f32, content.h as f32),
                Some(*pane) == focused,
                now,
            );
            let main_screen = !term.modes().alt_screen;
            // The flow (§6.2), following the tail: rows one cell high, blocks at their height.
            let arranged = pd.arrange(
                term,
                tsp,
                ch,
                content.h as f32 * spec.scale,
                spec.scale,
                Scroll::default(),
            );
            let first = arranged.composed.first;
            let slots: Vec<RowSlot> = arranged
                .composed
                .slots
                .iter()
                .map(|sl| RowSlot {
                    top: sl.top,
                    height: sl.height,
                })
                .collect();
            let uniform = arranged.composed.is_uniform(ch);
            let rows = slots.len().clamp(1, usize::from(u16::MAX)) as u16;
            let origin = (
                (content.x * s).round() as f32,
                (content.y * s).round() as f32,
            );
            let clip = (
                (rect.x * s).round() as u32,
                (rect.y * s).round() as u32,
                (rect.w * s).round() as u32,
                (rect.h * s).round() as u32,
            );

            if arranged.grid_shown() {
                let gv = GridView {
                    scroll: 0,
                    selection: None,
                    focused: Some(*pane) == focused,
                    cursor_on: true,
                };
                let snap = GridSnapshot::from_lines(term, &theme, &gv, first, rows);
                let r =
                    match self.grids.entry(*pane) {
                        std::collections::btree_map::Entry::Occupied(e) => e.into_mut(),
                        std::collections::btree_map::Entry::Vacant(v) => v.insert(
                            GridRenderer::new(&self.gpu, OFFSCREEN_FORMAT, fonts(spec.scale)?),
                        ),
                    };
                r.invalidate();
                r.set_origin(origin.0, origin.1);
                r.set_row_slots((!uniform).then(|| slots.clone()));
                r.prepare(&self.gpu, &snap, (wpx, hpx));
                grids_drawn.push((*pane, clip));
            }

            // Surfaces, each painted where the flow put it.
            let pd = self.docs.get_mut(pane).expect("synced above");
            let blocks_json: Vec<Value> = arranged
                .composed
                .blocks
                .iter()
                .map(|&(i, y)| {
                    let b = &arranged.blocks[i];
                    json!({
                        "sf": b.sf, "line": b.line,
                        "top": r2(f64::from(y) / s), "height": r2(f64::from(b.height) / s),
                    })
                })
                .collect();
            let dock_json = match arranged.dock {
                Some((_, b)) => json!({
                    "top": r2(content.h - f64::from(arranged.dock_px) / s),
                    "height": r2(f64::from(b.h)),
                }),
                None => Value::Null,
            };
            pd.paint(
                &arranged,
                &mut scene,
                Place {
                    x: content.x * s,
                    y: content.y * s,
                    w: content.w * s,
                    h: content.h * s,
                    scale: s,
                },
            );
            let screen = arranged.screen;
            let grid_first = first;
            panes_json.push(json!({
                "pane": pane,
                "rect": rect.json(),
                "cols": term.cols(),
                "rows": term.rows(),
                "focused": Some(*pane) == focused,
                "alt_screen": !main_screen,
                "first_line": grid_first,
                "lines_drawn": slots.len(),
                "blocks": blocks_json,
                "dock": dock_json,
                "screen": screen.is_some(),
                "boxes": boxes(pd, tsp),
            }));
        }

        // Render: the grids, then the surfaces over them.
        self.ui
            .render(&self.gpu, &scene, wpx, hpx)
            .map_err(|e| e.to_string())?;
        let target = OffscreenTarget::new(&self.gpu, wpx, hpx);
        let mut encoder = self.gpu.device.create_command_encoder(&Default::default());
        {
            use auk_render::wgpu;
            let bg = theme.background;
            let c = |v: u8| f64::from(v) / 255.0;
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("headless window"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target.view(),
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: c(bg.0),
                            g: c(bg.1),
                            b: c(bg.2),
                            a: c(bg.3),
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            for (pane, (x, y, w, h)) in &grids_drawn {
                let (x, y) = ((*x).min(wpx), (*y).min(hpx));
                let (w, h) = ((*w).min(wpx - x), (*h).min(hpx - y));
                if w == 0 || h == 0 {
                    continue;
                }
                pass.set_scissor_rect(x, y, w, h);
                if let Some(r) = self.grids.get(pane) {
                    r.draw(&mut pass);
                }
            }
            pass.set_scissor_rect(0, 0, wpx, hpx);
            self.ui.draw(&mut pass);
        }
        self.gpu.queue.submit([encoder.finish()]);
        let cpu = start.elapsed();
        let image = target.read(&self.gpu).map_err(|e| e.to_string())?;
        let readback = start.elapsed() - cpu;
        self.timings.push(Timing {
            cpu_ms: cpu.as_secs_f64() * 1000.0,
            readback_ms: readback.as_secs_f64() * 1000.0,
        });

        let layout = json!({
            "size": [spec.width, spec.height],
            "scale": spec.scale,
            "appearance": if spec.dark { "dark" } else { "light" },
            "clock": now,
            "cell": [r2(f64::from(cw) / s), r2(f64::from(ch) / s)],
            "session": shown.map(|(s, _)| s),
            "tab": shown.map(|(_, t)| t.id),
            "panes": panes_json,
        });
        Ok((image, layout))
    }

    /// The layout boxes of a pane's surfaces document (`tree`), optionally only elements matching
    /// `selector` and their subtrees.
    pub fn tree(&self, pane: PaneId, selector: Option<&str>) -> Result<Value, String> {
        let pd = self
            .docs
            .get(&pane)
            .ok_or_else(|| format!("pane {pane} hasn't been drawn"))?;
        let sel = selector.map(Simple::parse).transpose()?;
        let mut out = Vec::new();
        walk(pd, pd.view().element(), sel.as_ref(), false, &mut out);
        Ok(json!(out))
    }

    /// Computed style values of the first element matching `selector` (`css`).
    pub fn css(&self, pane: PaneId, selector: &str, props: &[String]) -> Result<Value, String> {
        let pd = self
            .docs
            .get(&pane)
            .ok_or_else(|| format!("pane {pane} hasn't been drawn"))?;
        let sel = Simple::parse(selector)?;
        let el = find(pd, pd.view().element(), &sel)
            .ok_or_else(|| format!("nothing matches `{selector}`"))?;
        let style = pd
            .doc()
            .computed_style(el)
            .ok_or("the element has no computed style")?;
        let debug = format!("{style:?}");
        let mut values = serde_json::Map::new();
        for p in props {
            values.insert(p.clone(), json!(style_field(&debug, p)));
        }
        let b = pd.doc().border_box(el).unwrap_or_default();
        Ok(json!({
            "class": pd.view().written().class(el),
            "box": [b.x, b.y, b.w, b.h],
            "style": values,
        }))
    }
}

/// The value a computed style shows for a property, read from its debug form (`font_size: 13.0`
/// for `font-size`).
fn style_field(debug: &str, prop: &str) -> Option<String> {
    let key = format!("{}: ", prop.replace('-', "_"));
    let at = debug.find(&key)? + key.len();
    let rest = &debug[at..];
    let mut depth = 0i32;
    let end = rest
        .char_indices()
        .find(|&(_, c)| {
            match c {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' if depth > 0 => depth -= 1,
                ',' | '}' if depth == 0 => return true,
                _ => {}
            }
            false
        })
        .map_or(rest.len(), |(i, _)| i);
    Some(rest[..end].trim().to_owned())
}

/// A simple selector: a tag and classes (`div.sf-row`, `.sf-text`, `table`), or `*`.
struct Simple {
    tag: Option<String>,
    classes: Vec<String>,
}

impl Simple {
    fn parse(s: &str) -> Result<Simple, String> {
        let s = s.trim();
        if s.is_empty() || s.contains(|c: char| c.is_whitespace() || "#[]:>+~".contains(c)) {
            return Err(format!(
                "selector `{s}`: use a tag and classes, like `div.sf-row` or `.sf-text`"
            ));
        }
        let mut parts = s.split('.');
        let tag = parts
            .next()
            .filter(|t| !t.is_empty() && *t != "*")
            .map(str::to_owned);
        Ok(Simple {
            tag,
            classes: parts.map(str::to_owned).collect(),
        })
    }

    fn matches(&self, pd: &PaneDoc, el: NodeId) -> bool {
        if let Some(tag) = &self.tag
            && tag_of(pd.doc(), el) != *tag
        {
            return false;
        }
        let class = class_of(pd, el);
        self.classes
            .iter()
            .all(|c| class.split_whitespace().any(|w| w == c))
    }
}

fn tag_of(doc: &Document, el: NodeId) -> String {
    match &doc.node(el).kind {
        auk_ui::dom::NodeKind::Element(e) => e.tag.as_str().to_owned(),
        _ => String::new(),
    }
}

fn class_of(pd: &PaneDoc, el: NodeId) -> String {
    pd.view()
        .written()
        .class(el)
        .map(str::to_owned)
        .or_else(|| pd.doc().attribute(el, "class").map(str::to_owned))
        .unwrap_or_default()
}

fn walk(pd: &PaneDoc, el: NodeId, sel: Option<&Simple>, inside: bool, out: &mut Vec<Value>) {
    let hit = inside || sel.is_none_or(|s| s.matches(pd, el));
    if hit {
        let b = pd.doc().border_box(el).unwrap_or_default();
        let r = |v: f32| (v * 100.0).round() / 100.0;
        out.push(json!({
            "tag": tag_of(pd.doc(), el),
            "class": class_of(pd, el),
            "id": pd.doc().attribute(el, "data-id"),
            "box": [r(b.x), r(b.y), r(b.w), r(b.h)],
        }));
    }
    for c in pd.doc().element_children(el) {
        walk(pd, c, sel, hit, out);
    }
}

fn find(pd: &PaneDoc, el: NodeId, sel: &Simple) -> Option<NodeId> {
    if sel.matches(pd, el) {
        return Some(el);
    }
    pd.doc()
        .element_children(el)
        .into_iter()
        .find_map(|c| find(pd, c, sel))
}

/// Element boxes of each surface's regions, for the layout JSON: `[surface, region, tag, class,
/// x, y, w, h]` in document points, region-relative.
fn boxes(pd: &PaneDoc, tsp: &auk_tsp::TspPane) -> Value {
    let mut out = Vec::new();
    for sf in tsp.surfaces() {
        for (name, region) in [
            ("main", Region::Main),
            ("dock", Region::Dock),
            ("layer", Region::Layer),
        ] {
            let Some(el) = pd.view().region(sf.key(), region) else {
                continue;
            };
            let Some(origin) = pd.doc().border_box(el) else {
                continue;
            };
            if pd.hidden(el) {
                continue;
            }
            let mut stack = vec![el];
            while let Some(e) = stack.pop() {
                if let Some(b) = pd.doc().border_box(e) {
                    let r = |v: f32| (f64::from(v) * 100.0).round() / 100.0;
                    out.push(json!([
                        sf.id(),
                        name,
                        tag_of(pd.doc(), e),
                        class_of(pd, e),
                        r(b.x - origin.x),
                        r(b.y - origin.y),
                        r(b.w),
                        r(b.h),
                    ]));
                }
                let mut kids = pd.doc().element_children(e);
                kids.reverse();
                stack.extend(kids);
            }
        }
    }
    json!(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_parse() {
        let mut spec = WindowSpec::default();
        spec.parse_size("800x500@2").unwrap();
        assert_eq!((spec.width, spec.height, spec.scale), (800, 500, 2.0));
        spec.parse_size("640x400").unwrap();
        assert_eq!(spec.scale, 2.0);
        assert!(spec.parse_size("10x10").is_err());
        assert!(spec.parse_size("800x500@9").is_err());
    }

    #[test]
    fn splits_lay_out_with_dividers() {
        let tab = Tab {
            id: 1,
            title: None,
            root: Node::Split {
                dir: SplitDir::Right,
                ratio: 500,
                first: Box::new(Node::Pane { pane: 1 }),
                second: Box::new(Node::Pane { pane: 2 }),
            },
            zoom: None,
        };
        let area = Rect {
            x: 0.0,
            y: 0.0,
            w: 801.0,
            h: 600.0,
        };
        let r = layout_tab(&tab, area);
        assert_eq!(r[0].1.w, 400.0);
        assert_eq!(r[1].1.x, 401.0);
        assert_eq!(r[1].1.w, 400.0);
    }

    #[test]
    fn style_fields_read_from_debug() {
        let d = "ComputedStyle { display: Block, font_size: 13.0, color: Rgba(1, 2, 3), x: 1 }";
        assert_eq!(style_field(d, "font-size").as_deref(), Some("13.0"));
        assert_eq!(style_field(d, "color").as_deref(), Some("Rgba(1, 2, 3)"));
        assert_eq!(style_field(d, "nope"), None);
    }
}
