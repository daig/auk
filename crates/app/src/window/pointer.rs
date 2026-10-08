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
        let (p, line, col) = self.cell_at(x, y)?;
        let shared = self.conn.shared();
        let term = shared.panes.get(&p)?.replica.terminal();
        let link = term.line(line)?.cells.get(usize::from(col))?.link;
        (link != 0)
            .then(|| term.links().get(link as usize).cloned())
            .flatten()
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
