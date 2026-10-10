//! Pointer details of the pane (§6.2, §7.9, §7.14, §12.2): the hover readout of a link's target,
//! drag-selection inside a focused program field, and mapping document points of a pane's
//! surfaces back to window points.

use std::collections::BTreeMap;

use auk_mirror::PaneId;
use auk_mirror::layout::Window as Sessions;
use auk_platform_macos::{MouseEvent, Window};
use auk_views::Outcome;

use super::WinState;

/// A rectangle in window points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WinRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl WinState {
    /// The program `pane` reports (its hello's `app`), only while a listening surface is open
    /// (§7.2, §12.2: `TspPane::app` is `None` otherwise).
    fn pane_app(&self, pane: PaneId) -> Option<String> {
        let shared = self.conn.shared();
        shared
            .panes
            .get(&pane)?
            .replica
            .tsp()
            .app()
            .map(str::to_owned)
    }

    /// The focused pane's reporting program, for the status line.
    pub fn chrome_pane_app(&self) -> Option<String> {
        self.pane_app(self.focused_pane()?)
    }

    /// The reporting program of each shown tab's focused pane, by tab id.
    pub fn chrome_tab_apps(&self) -> BTreeMap<u64, String> {
        self.chrome_tab_apps_in(&self.sessions())
    }

    pub(super) fn chrome_tab_apps_in(&self, sessions: &Sessions) -> BTreeMap<u64, String> {
        let Some(session) = self.session(sessions) else {
            return BTreeMap::new();
        };
        session
            .tabs
            .iter()
            .filter_map(|t| {
                let pane = self
                    .tab_focus
                    .get(&t.id)
                    .copied()
                    .or_else(|| t.root.panes().first().copied())?;
                Some((t.id, self.pane_app(pane)?))
            })
            .collect()
    }

    /// The shown tabs whose focused pane is an agent pane (§5.1: drawn with the agent icon).
    pub(super) fn chrome_tab_agents_in(
        &self,
        sessions: &Sessions,
    ) -> std::collections::BTreeSet<u64> {
        let Some(session) = self.session(sessions) else {
            return Default::default();
        };
        session
            .tabs
            .iter()
            .filter(|t| {
                self.tab_focus
                    .get(&t.id)
                    .copied()
                    .filter(|p| t.root.contains(*p))
                    .or_else(|| t.root.panes().first().copied())
                    .is_some_and(|pane| sessions.kind(pane) == auk_mirror::layout::PaneTag::Agent)
            })
            .map(|t| t.id)
            .collect()
    }

    /// The link under a window point: an OSC 8 link on the grid, or a surface link (an element
    /// with `data-href`). Links show their target on hover and open only on an explicit gesture
    /// (§7.14, §12.2).
    pub(super) fn link_at(&self, x: f64, y: f64) -> Option<String> {
        let pane = self.tab_layout.pane_at(x, y)?;
        if let Some((dx, dy)) = self.surface_point(pane, x, y) {
            let sf = self.surfaces.get(&pane)?;
            let doc = sf.doc();
            let mut cur = auk_ui::hit_test(doc, dx, dy);
            while let Some(el) = cur {
                if let Some(href) = doc.attribute(el, "data-href") {
                    return Some(href.to_owned());
                }
                cur = doc.parent(el);
            }
            return None;
        }
        let (p, link) = self.grid_link_at(x, y)?;
        let shared = self.conn.shared();
        shared
            .panes
            .get(&p)?
            .replica
            .terminal()
            .links()
            .get(link as usize)
            .cloned()
    }

    /// The OSC 8 link of the grid cell under a window point: its pane and terminal link id.
    pub(super) fn grid_link_at(&self, x: f64, y: f64) -> Option<(PaneId, u32)> {
        let pane = self.tab_layout.pane_at(x, y)?;
        if self.surface_point(pane, x, y).is_some() {
            return None;
        }
        let (p, line, col) = self.cell_at(x, y)?;
        let shared = self.conn.shared();
        let term = shared.panes.get(&p)?.replica.terminal();
        let link = term.line(line)?.cells.get(usize::from(col))?.link;
        (link != 0).then_some((p, link))
    }

    /// The grid link a click would open now (A13): the one under the pointer while ⌘ is down,
    /// or with `links_plain_click` while the program doesn't take the mouse. It draws a solid
    /// accent underline (none with `hyperlinks` off) and the pointer is a pointing hand.
    pub fn armed_link(&self) -> Option<(PaneId, u32)> {
        let (pane, link) = self.hover_grid?;
        let plain =
            self.settings.links_plain_click && self.modes(pane).mouse == auk_term::MouseMode::Off;
        (self.pointer_cmd || plain).then_some((pane, link))
    }

    /// The grid view the frame draws `pane` with, as far as links go (A13): the `hyperlinks`
    /// style and the armed link when it is this pane's.
    pub fn link_view(&self, pane: PaneId) -> (auk_render::LinkStyle, Option<u32>) {
        let armed = self.armed_link();
        (
            self.settings.hyperlinks,
            armed.filter(|(p, _)| *p == pane).map(|(_, link)| link),
        )
    }

    /// How the pane's visible link cells draw: one entry per cell, `{row, col, underline,
    /// color}`, with `underline` `"dotted"`, `"solid"` or `null`, resolved as the frame resolves
    /// them.
    pub fn grid_link_cells(&self, pane: PaneId) -> Result<Vec<serde_json::Value>, String> {
        let (links, hover_link) = self.link_view(pane);
        let shared = self.conn.shared();
        let term = shared
            .panes
            .get(&pane)
            .ok_or_else(|| format!("no replica of pane {pane}"))?
            .replica
            .terminal();
        let view = auk_render::View {
            links,
            hover_link,
            cursor_on: false,
            ..auk_render::View::default()
        };
        let snap = auk_render::GridSnapshot::from_terminal(term, &self.theme, &view);
        let top = term.history_len();
        let mut out = Vec::new();
        for row in 0..snap.rows {
            let Some(line) = term.line(top + usize::from(row)) else {
                continue;
            };
            for (col, cell) in snap.row(row).iter().enumerate() {
                if line.cells.get(col).is_none_or(|c| c.link == 0) {
                    continue;
                }
                use auk_render::Attrs;
                let underline = if cell.attrs.has(Attrs::DOTTED_UNDERLINE) {
                    Some("dotted")
                } else if cell.attrs.has(Attrs::UNDERLINE) {
                    Some("solid")
                } else {
                    None
                };
                let c = cell.decoration;
                out.push(serde_json::json!({
                    "row": row, "col": col, "underline": underline,
                    "color": format!("#{:02x}{:02x}{:02x}", c.0, c.1, c.2),
                    "accent": c == self.theme.link,
                }));
            }
        }
        Ok(out)
    }

    /// Puts the pointing hand back while a link is armed, when AppKit has reset the cursor (a
    /// menu bar rebuilt on a settings reload, a window change) without the pointer moving.
    pub(super) fn reassert_pointer(&self, window: &Window) {
        if self.pointer_shape == auk_platform_macos::PointerShape::PointingHand
            && window.pointer() != self.pointer_shape
        {
            window.set_pointer(self.pointer_shape);
        }
    }

    /// Follows the pointer over grid links and the ⌘ key: what [`WinState::armed_link`] sees.
    /// Returns whether the armed link changed (the grid and the pointer change with it).
    pub(super) fn track_grid_link(
        &mut self,
        window: &Window,
        at: Option<(f64, f64)>,
        cmd: bool,
    ) -> bool {
        let before = self.armed_link();
        if let Some((x, y)) = at {
            self.hover_grid = self.grid_link_at(x, y);
        }
        self.pointer_cmd = cmd;
        let now = self.armed_link();
        let shape = if now.is_some() {
            auk_platform_macos::PointerShape::PointingHand
        } else {
            auk_platform_macos::PointerShape::Arrow
        };
        if shape != self.pointer_shape {
            self.pointer_shape = shape;
            window.set_pointer(shape);
        }
        now != before
    }

    /// The `title` of the surface node under a window point, or of its nearest ancestor with
    /// one: the node's tooltip (§8.1).
    pub(super) fn title_at(&self, x: f64, y: f64) -> Option<String> {
        let pane = self.tab_layout.pane_at(x, y)?;
        let (dx, dy) = self.surface_point(pane, x, y)?;
        let sf = self.surfaces.get(&pane)?;
        let doc = sf.doc();
        let top = sf.view().element();
        let mut cur = auk_ui::hit_test(doc, dx, dy);
        while let Some(el) = cur {
            if el == top {
                break;
            }
            if let Some(t) = doc.attribute(el, "title").filter(|t| !t.trim().is_empty()) {
                return Some(t.to_owned());
            }
            cur = doc.parent(el);
        }
        None
    }

    /// Updates the hover readouts (a link's target, a node's tooltip) for a pointer at a window
    /// point. Returns whether they changed.
    pub(super) fn hover_link_at(&mut self, x: f64, y: f64) -> bool {
        let link = self.link_at(x, y);
        let title = self.title_at(x, y);
        let mut changed = false;
        if link != self.hover_link {
            self.hover_link = link;
            changed = true;
        }
        // The tooltip stays where it appeared while the pointer stays over the same title.
        if title.as_deref() != self.hover_title.as_ref().map(|(t, ..)| t.as_str()) {
            self.hover_title = title.map(|t| (t, x, y));
            changed = true;
        }
        if changed {
            self.ui_dirty = true;
        }
        changed
    }

    /// A press that a surface took: when it went into the pane's focused program field with
    /// native editing, a drag from it selects in the field.
    /// A press on a `prefs` ordered list's grip starts dragging its item. Returns whether it did.
    pub(super) fn reorder_press(&mut self, pane: PaneId, m: &MouseEvent) -> bool {
        self.reorder_drag = None;
        let Some((dx, dy)) = self.surface_point(pane, m.x, m.y) else {
            return false;
        };
        let conn = self.conn.clone();
        let shared = conn.shared();
        let (Some(sf), Some(data)) = (self.surfaces.get_mut(&pane), shared.panes.get(&pane)) else {
            return false;
        };
        let (doc, pv) = sf.parts();
        if pv.reorder_press(doc, data.replica.tsp(), dx, dy) {
            self.reorder_drag = Some(pane);
            self.ui_dirty = true;
            true
        } else {
            false
        }
    }

    /// The pointer moved while dragging a `prefs` item: it follows. Returns whether a drag is
    /// under way.
    pub(super) fn reorder_dragged(&mut self, window: &Window, m: &MouseEvent) -> bool {
        let Some(pane) = self.reorder_drag else {
            return false;
        };
        if let Some((dx, dy)) = self.surface_point(pane, m.x, m.y)
            && let Some(sf) = self.surfaces.get_mut(&pane)
        {
            let (doc, pv) = sf.parts();
            pv.reorder_drag(doc, dx, dy);
            self.ui_dirty = true;
            window.request_frame();
        }
        true
    }

    /// The release of a `prefs` item drag: the new order goes to the program. Returns whether a
    /// drag was under way.
    pub(super) fn reorder_released(&mut self, window: &Window, m: &MouseEvent) -> bool {
        let Some(pane) = self.reorder_drag.take() else {
            return false;
        };
        let at = self
            .surface_point(pane, m.x, m.y)
            .unwrap_or((-1.0e6, -1.0e6));
        let out = match self.surfaces.get_mut(&pane) {
            Some(sf) => {
                let (doc, pv) = sf.parts();
                pv.reorder_release(doc, at.0, at.1).unwrap_or_default()
            }
            None => Vec::new(),
        };
        self.ui_dirty = true;
        self.outcomes(window, pane, out);
        window.request_frame();
        true
    }

    pub(super) fn field_press(&mut self, pane: PaneId, m: &MouseEvent) {
        self.field_drag = self.field_offset(pane, m.x, m.y).map(|off| (pane, off));
    }

    /// The UTF-16 offset in the focused field of `pane` under a window point.
    fn field_offset(&self, pane: PaneId, x: f64, y: f64) -> Option<u64> {
        let (dx, dy) = self.surface_point(pane, x, y)?;
        let sf = self.surfaces.get(&pane)?;
        let shared = self.conn.shared();
        let tsp = shared.panes.get(&pane)?.replica.tsp();
        sf.view().field_offset_at(sf.doc(), tsp, dx, dy)
    }

    /// A drag after [`field_press`](Self::field_press): the field's native selection runs from
    /// the press to the pointer (§7.9). Returns whether a field drag is under way.
    pub(super) fn field_dragged(&mut self, window: &Window, m: &MouseEvent) -> bool {
        let Some((pane, anchor)) = self.field_drag else {
            return false;
        };
        let Some(to) = self.field_offset(pane, m.x, m.y) else {
            return true;
        };
        let outcome = {
            let Some(sf) = self.surfaces.get_mut(&pane) else {
                return true;
            };
            let conn = self.conn.clone();
            let shared = conn.shared();
            let Some(data) = shared.panes.get(&pane) else {
                return true;
            };
            let (doc, pv) = sf.parts();
            pv.select(doc, data.replica.tsp(), anchor, to)
        };
        if !matches!(outcome, Outcome::Keys) {
            self.ui_dirty = true;
            self.outcomes(window, pane, vec![outcome]);
        }
        true
    }

    /// A rectangle of a pane's surface document (points) in window points, where the pane last
    /// drew it: in a block of the flow, the dock, or a screen surface.
    pub fn doc_rect_to_window(&self, pane: PaneId, r: auk_ui::Rect) -> Option<WinRect> {
        let view = self.views.get(&pane)?;
        let sf = self.surfaces.get(&pane)?;
        let doc = sf.doc();
        let scale = self.size.map_or(2.0, |s| s.scale) as f32;
        let a = &view.arranged;
        let (ox, oy) = view.grid_origin;
        let place = |top_doc: f32, top_px: f32| WinRect {
            x: ox + f64::from(r.x),
            y: oy + f64::from(top_px / scale) + f64::from(r.y - top_doc),
            w: f64::from(r.w),
            h: f64::from(r.h),
        };
        if let Some((_, b)) = a.screen {
            return Some(place(b.y, 0.0));
        }
        if let Some((_, b)) = a.dock
            && r.y >= b.y
            && r.y < b.y + b.h
        {
            return Some(place(b.y, a.flow_px));
        }
        for &(i, y) in &a.composed.blocks {
            let b = a.blocks.get(i)?;
            let top = doc.border_box(b.el)?.y;
            if r.y >= top && r.y < top + b.height / scale {
                return Some(place(top, y));
            }
        }
        None
    }

    /// The ids of a pane's surface nodes whose id ends with `pattern` or whose `role` is it,
    /// newest surface first: for automation that knows a control by its role or its id's tail
    /// (`/model/effort`), not by the program's per-session id prefix.
    pub fn node_ids_matching(&self, pane: PaneId, pattern: &str) -> Vec<String> {
        let shared = self.conn.shared();
        let Some(p) = shared.panes.get(&pane) else {
            return Vec::new();
        };
        let mut ids = Vec::new();
        for surface in p.replica.tsp().surfaces().iter().rev() {
            let mut stack = vec![surface.document().root()];
            while let Some(node) = stack.pop() {
                let role = node.prop("role").and_then(|r| r.as_str());
                if node.id().ends_with(pattern) || role == Some(pattern) {
                    ids.push(node.id().to_owned());
                }
                stack.extend(node.children());
            }
        }
        ids
    }

    /// The window box of a node of one of a pane's surfaces (the newest surface holding the id).
    pub fn node_box(&self, pane: PaneId, id: &str) -> Option<WinRect> {
        let sf = self.surfaces.get(&pane)?;
        let el = {
            let shared = self.conn.shared();
            let tsp = shared.panes.get(&pane)?.replica.tsp();
            tsp.surfaces()
                .iter()
                .rev()
                .find_map(|s| sf.view().node_element(s.key(), id))?
        };
        // Out of view in virtualized content, its box is only an estimate: not drawn.
        if !sf.doc().laid_out(el) {
            return None;
        }
        let r = sf.doc().border_box(el)?;
        // A layer overlays the pane from its top, over everything else (§7.5).
        if sf.in_layers(el) {
            let view = self.views.get(&pane)?;
            if !view.arranged.layers {
                return None;
            }
            let (ox, oy) = view.grid_origin;
            return Some(WinRect {
                x: ox + f64::from(r.x),
                y: oy + f64::from(r.y - sf.pane_top()),
                w: f64::from(r.w),
                h: f64::from(r.h),
            });
        }
        self.doc_rect_to_window(pane, r)
    }

    /// The focused program field's text box in window points, and the cell size in points.
    pub fn field_box(&self, pane: PaneId) -> Option<(WinRect, (f32, f32))> {
        let sf = self.surfaces.get(&pane)?;
        let r = {
            let shared = self.conn.shared();
            let tsp = shared.panes.get(&pane)?.replica.tsp();
            sf.view().field_text_box(sf.doc(), tsp)?
        };
        let cell = sf.view().settings().cell;
        Some((self.doc_rect_to_window(pane, r)?, cell))
    }
}
