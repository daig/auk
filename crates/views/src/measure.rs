//! Measure-then-layout loops (§8.3): card and tool preview clamps ("N more lines"), text cuts,
//! table column fitting, status segment dropping, overlay anchoring, follow-tail, field caret
//! scrolling, and `reveal`/`scroll` requests.

use crate::kinds::data::{cut, fit_columns};
use crate::view::{Extra, PaneView};
use auk_tsp::{RevealAt, ScrollBy};
use auk_ui::{Document, NodeId};

/// What one [`PaneView::layout`] did, for frame statistics: measure passes run (each with its
/// document update) and the engine's work summed over them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LayoutWork {
    /// Document updates: 1 when the measure loop was skipped or settled at once.
    pub passes: usize,
    /// Measure targets visited (preview clamps, cuts, tables, strips, anchors), all passes.
    pub measured: usize,
    pub restyled: usize,
    pub inline_rebuilt: usize,
    pub laid_out: usize,
}

impl LayoutWork {
    fn add(&mut self, s: auk_ui::UpdateStats) {
        self.restyled += s.restyled;
        self.inline_rebuilt += s.inline_rebuilt;
        self.laid_out += s.laid_out;
    }
}

/// What a measure pass reads besides the document.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MeasureInputs {
    viewport: (f32, f32),
    view_rect: Option<(f32, f32, f32, f32)>,
    cell: (f32, f32),
    /// The scroll offsets of the scrollers around virtualized containers (hashed): scrolling
    /// one (an overlay card's body) moves the windows of the lists inside it.
    scrolls: u64,
}

impl PaneView {
    /// Lay the document out, running the measure loops until they settle. Call after
    /// [`PaneView::sync`] or [`PaneView::tick`], before painting.
    pub fn layout(&mut self, doc: &mut Document) -> usize {
        // Refit status strips from scratch when something changed or the window resized.
        let vp = doc.viewport();
        if self.fit_dirty || self.fit_viewport != vp {
            self.fit_dirty = false;
            self.fit_viewport = vp;
            let mut dom = crate::dom::Dom {
                doc: &mut *doc,
                w: &mut self.written,
            };
            for sv in &self.surfaces {
                for id in &sv.st.fit {
                    let Some(m) = sv.mounts.get(id) else { continue };
                    for &s in m.slots.iter().take(2) {
                        for k in dom.doc.element_children(s) {
                            let c = dom.class_of(k).to_owned();
                            if c.split(' ').any(|x| x == "sf-fit-off") {
                                let new = c
                                    .split(' ')
                                    .filter(|x| *x != "sf-fit-off")
                                    .collect::<Vec<_>>()
                                    .join(" ");
                                dom.set_class(k, &new);
                            }
                        }
                    }
                }
            }
        }
        // The measure loop (as `Document::update_with(8, …)`), skipped when nothing it reads
        // changed since it last settled: a long transcript costs a pass over every mount.
        doc.update();
        let mut work = LayoutWork::default();
        work.add(doc.last_update_stats());
        let mut passes = 1;
        let mut again: Vec<(usize, auk_tsp::Reveal)> = Vec::new();
        for round in 0..3 {
            let inputs = MeasureInputs {
                viewport: doc.viewport(),
                view_rect: self.view_rect.map(|r| (r.x, r.y, r.w, r.h)),
                cell: self.settings.cell,
                scrolls: self.virtual_scrolls(doc),
            };
            if self.measured != Some((doc.generation(), inputs)) {
                self.measured = None;
                loop {
                    if passes >= 8 {
                        break;
                    }
                    if !self.measure(doc) {
                        self.measured = Some((doc.generation(), inputs));
                        break;
                    }
                    doc.update();
                    work.add(doc.last_update_stats());
                    passes += 1;
                }
            }
            if round == 2 {
                break;
            }
            // A reveal or scroll that moved a scroller around a virtualized list measures
            // again, so the list lays out what came into view this frame; a reveal of a row
            // that was only estimated (out of the list's window) applies again once laid out.
            for (i, r) in std::mem::take(&mut again) {
                self.surfaces[i].st.reveal = Some(r);
            }
            again = self.apply_scrolls(doc);
            if again.is_empty()
                && self
                    .measured
                    .is_some_and(|(_, i)| i.scrolls == self.virtual_scrolls(doc))
            {
                break;
            }
        }
        work.passes = passes;
        work.measured = std::mem::take(&mut self.measure_visits);
        self.work = work;
        passes
    }

    /// A hash of the scroll offsets of every scroller around a virtualized container.
    fn virtual_scrolls(&self, doc: &Document) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for sv in &self.surfaces {
            for &(el, _) in sv.st.virt.values() {
                if doc.try_node(el).is_none() {
                    continue;
                }
                let mut cur = doc.parent(el);
                while let Some(p) = cur {
                    let (x, y) = doc.scroll_offset(p);
                    if (x, y) != (0.0, 0.0) {
                        (p, x.to_bits(), y.to_bits()).hash(&mut h);
                    }
                    cur = doc.parent(p);
                }
            }
        }
        h.finish()
    }

    /// What the last [`PaneView::layout`] did.
    pub fn last_work(&self) -> LayoutWork {
        self.work
    }

    /// The part of the document the window shows, in document coordinates (the pane's scroll
    /// position). Virtualized containers lay out what's near it. `None`: the document viewport.
    pub fn set_view_rect(&mut self, rect: Option<auk_ui::Rect>) {
        self.view_rect = rect;
        // The next measure pass gives the windows from it, even when it is the same rect.
        self.windows_from = None;
        self.measured = None;
    }

    /// Where the window shows parts of the document: spans of it (document top, document
    /// bottom) and where each span's top shows, from the top of the layer (the pane's visible
    /// area), in points. Overlays anchored to a node or caret outside the layer are placed
    /// from where it shows; one whose anchor shows nowhere hides (`.lost`). Returns whether
    /// that changed a placement: lay out again.
    pub fn set_shown(&mut self, spans: Vec<(f32, f32, f32)>) -> bool {
        if self.shown == spans {
            return false;
        }
        self.shown = spans;
        let anchored = self.surfaces.iter().any(|sv| !sv.st.anchored.is_empty());
        if anchored {
            self.measured = None;
        }
        anchored
    }

    /// Whether a window kept on a virtualized container's end (the view showed the end) no
    /// longer has the end in view at the view rect: the list grew under a view that stays
    /// where it is (scrolled up), so the window goes back to the view
    /// ([`PaneView::set_view_rect`] again, then lay out).
    pub fn end_windows_left_view(&self, doc: &Document) -> bool {
        let Some(view) = self.view_rect else {
            return false;
        };
        self.surfaces.iter().any(|sv| {
            sv.st.virt_window.iter().any(|(&el, w)| {
                w.2 && doc.try_node(el).is_some()
                    && !inside(doc, el, self.layers)
                    && doc
                        .border_box(el)
                        .is_some_and(|r| view.y + view.h < r.y + r.h - 0.5)
            })
        })
    }

    /// The view rect last set ([`PaneView::set_view_rect`]).
    pub fn view_rect(&self) -> Option<auk_ui::Rect> {
        self.view_rect
    }

    /// One measure pass; returns whether it changed the document.
    pub(crate) fn measure(&mut self, doc: &mut Document) -> bool {
        let lh = self.settings.cell.1;
        let cw = self.settings.cell.0;
        let mut changed = false;
        // Virtualized containers an ancestor scrolls: their window from the view rect.
        let (vw, vh) = doc.viewport();
        let view = self
            .view_rect
            .unwrap_or(auk_ui::Rect::new(0.0, 0.0, vw, vh));
        // The view rect the windows were last given from: until the host gives one again, a
        // window kept on a container's end stays there (the list grew under a view that
        // follows its end; the host's next view rect says where the view is now). Without a
        // view rect the view stays at the top: no window is kept on an end.
        let fresh = self.windows_from != Some(view);
        self.windows_from = Some(view);
        let ends = self.view_rect.is_some();
        let layers = self.layers;
        for sv in &mut self.surfaces {
            for &(el, _) in sv.st.virt.values() {
                if doc.try_node(el).is_none() {
                    continue;
                }
                if doc
                    .computed_style(el)
                    .is_some_and(|s| s.is_scroll_container())
                {
                    continue;
                }
                // One inside content that is itself virtualized out of view has only an
                // estimated box: its window waits until it is laid out.
                if !doc.laid_out(el) {
                    continue;
                }
                let Some(r) = doc.border_box(el) else {
                    continue;
                };
                // In the layer (an overlay's list), the view is the layer's: the pane's visible
                // area, not the flow's part of the document.
                let in_layer = layer_view(doc, el, layers);
                let view = in_layer.unwrap_or(view);
                // With its end in view (a transcript the view follows), the window is kept as
                // distances up from the end, so as the list grows its layout lays out the new
                // end, not where the end was. Not in the layer: what moves a list there is its
                // card's scroller, not the view rect, so its window always follows its view.
                let bottom = r.y + r.h;
                let at_end = ends && in_layer.is_none() && view.y + view.h >= bottom - 0.5;
                let want = if at_end {
                    (bottom - view.y, bottom - view.y - view.h, true)
                } else {
                    (view.y - r.y, view.y + view.h - r.y, false)
                };
                let have = sv.st.virt_window.get(&el).copied();
                if !fresh && have.is_some_and(|h| h.2) {
                    continue;
                }
                let moved = have.is_none_or(|h| {
                    h.2 != want.2
                        || (h.0 - want.0).abs() > vh * 0.1
                        || (h.1 - want.1).abs() > vh * 0.1
                });
                if moved {
                    doc.set_virtual_window_at(el, Some((want.0, want.1)), want.2);
                    sv.st.virt_window.insert(el, want);
                    changed = true;
                }
            }
        }
        // Clamps, cuts and column fits depend only on their element's laid-out size: revisit
        // just what layout computed since the last pass. A box layout skipped (virtualized out
        // of view, §7.10, §8.3) or took from its cache is as it was last measured, so a load or
        // a stream costs in proportion to what changed and what's in view.
        let since = self.measure_mark;
        self.measure_mark = doc.layout_count();
        let mut visits = 0;
        let (pane_el, docks_el, toasts_el) = (self.container, self.docks, self.toasts.el);
        let mut dom = crate::dom::Dom {
            doc,
            w: &mut self.written,
        };
        // Highlighting this frame may still finish on this thread (what the background hasn't
        // done yet of texts coming into view); the rest draws in on the clock as it finishes.
        let (hl_finish, hl_budget) = (&mut self.hl_finish, &mut self.hl_budget);
        let shown = &self.shown;
        for sv in &mut self.surfaces {
            visits += sv.st.fit.len() + sv.st.anchored.len();
            // Highlighting that waited out of view draws in as its node comes into view, what
            // the background hasn't finished yet finished here within the pass's budget.
            let came: Vec<String> = sv
                .st
                .hl_waiting
                .iter()
                .filter(|(_, el)| dom.doc.laid_out_since(**el, since))
                .map(|(id, _)| id.clone())
                .collect();
            for id in came {
                let Some(m) = sv.mounts.get_mut(&id) else {
                    sv.st.hl_waiting.remove(&id);
                    continue;
                };
                visits += 1;
                let before = (m.hl.len(), *hl_budget);
                m.hl.retain_mut(|d| d.draw_in(&mut dom, hl_finish, hl_budget));
                changed |= (m.hl.len(), *hl_budget) != before;
                if m.hl.is_empty() {
                    sv.st.hl_waiting.remove(&id);
                } else {
                    // In view with lines still waiting (the background is on them): the clock
                    // draws them in as they finish. Out of view the clock would let them go.
                    m.clock = true;
                    sv.clocked.insert(id);
                }
            }
            // Preview clamps.
            for (id, &el) in &sv.st.previews {
                if !dom.doc.laid_out_since(el, since) {
                    continue;
                }
                let Some(m) = sv.mounts.get(id) else { continue };
                visits += 1;
                let class = dom.class_of(m.el).to_owned();
                if !class.split(' ').any(|c| c == "preview") {
                    continue;
                }
                let Some(&body) = m.slots.first() else {
                    continue;
                };
                let Some(&more) = m.generated.last() else {
                    continue;
                };
                if dom.doc.try_node(body).is_none() || dom.doc.parent(more) != Some(m.el) {
                    continue;
                }
                let clamp = dom.doc.border_box(body).map(|r| r.h).unwrap_or(0.0);
                let content = dom.doc.scroll_size(body).1;
                let hidden = ((content - clamp) / lh + 0.75).floor().max(0.0) as usize;
                let more_class = if m.kind == "card" {
                    "sf-more"
                } else {
                    "sf-tool-more"
                };
                let want_class = if hidden > 0 {
                    more_class.to_owned()
                } else {
                    format!("{more_class} sf-hidden")
                };
                let clamped = class.split(' ').any(|c| c == "clamped");
                if (hidden > 0) != clamped {
                    let new = if hidden > 0 {
                        format!("{class} clamped")
                    } else {
                        class
                            .split(' ')
                            .filter(|c| *c != "clamped")
                            .collect::<Vec<_>>()
                            .join(" ")
                    };
                    dom.set_class(m.el, &new);
                    changed = true;
                }
                if dom.class_of(more) != want_class {
                    dom.set_class(more, &want_class);
                    changed = true;
                }
                let label = format!("{hidden} more line{}", if hidden == 1 { "" } else { "s" });
                let text = crate::events::text_content(dom.doc, more);
                if hidden > 0 && text != label {
                    dom.set_only_text(more, &label);
                    dom.attr(more, "title", &format!("Show all  {label}"));
                    changed = true;
                }
            }
            // Diffs under the Edits setting's Split: side by side once wider than 112 cells.
            let diffs: Vec<String> = sv
                .st
                .diffs
                .iter()
                .filter(|(_, el)| dom.doc.laid_out_since(**el, since) && dom.doc.laid_out(**el))
                .map(|(id, _)| id.clone())
                .collect();
            for id in diffs {
                let Some(m) = sv.mounts.get_mut(&id) else {
                    continue;
                };
                visits += 1;
                if crate::kinds::rich::redecide_diff(&mut dom, m, cw) {
                    changed = true;
                    if !m.hl.is_empty() {
                        sv.st.hl_waiting.insert(id.clone(), m.el);
                        sv.clocked.insert(id);
                        m.clock = true;
                    }
                }
            }
            // Text cuts (`truncate` start/middle on one line).
            for (id, t) in &sv.st.truncs {
                if !dom.doc.laid_out_since(t.el, since) {
                    continue;
                }
                visits += 1;
                let Some(r) = dom.doc.border_box(t.el) else {
                    continue;
                };
                // Half a pixel of slack: a box exactly as wide as its text can come out a hair
                // under it.
                let cells = ((r.w + 0.5) / cw).floor().max(1.0) as usize;
                let want = cut(&t.full, cells, &t.mode);
                let have = crate::events::text_content(dom.doc, t.el);
                // A box that shrinks to its text is as wide as its cut text, which can be a
                // little narrower than its cells (the ellipsis): a cut that fits stays, or each
                // pass would cut one more cell.
                let fits = dom.doc.scroll_size(t.el).0 <= r.w + 0.5;
                let shorter = want.chars().count() < have.chars().count();
                if have != want && !(shorter && have != t.full && fits) {
                    dom.set_only_text(t.el, &want);
                    // The cut text is what the node's next render takes away: the spans it
                    // replaced are freed, and their ids go to whatever is made next.
                    if let Some(m) = sv.mounts.get_mut(id) {
                        m.generated = dom.doc.children(t.el).to_vec();
                    }
                    changed = true;
                }
            }
            // Table column fitting.
            for info in sv.st.tables.values() {
                if !dom.doc.laid_out_since(info.grid, since) {
                    continue;
                }
                visits += 1;
                let Some(r) = dom.doc.border_box(info.grid) else {
                    continue;
                };
                let fit = fit_columns(&info.cols, r.w, cw);
                let tpl: Vec<String> = fit
                    .iter()
                    .filter(|(shown, _)| *shown)
                    .map(|(_, w)| format!("{}px", crate::common::fmt_num(*w as f64)))
                    .collect();
                let css = crate::kinds::data::table_columns(&tpl.join(" "));
                if dom.w.style(info.grid) != Some(css.as_str()) {
                    dom.set_style(info.grid, &css);
                    changed = true;
                }
                for (col, (shown, _)) in info.cols.iter().zip(&fit) {
                    for &c in &col.cells {
                        let class = dom.class_of(c).to_owned();
                        let off = class.split(' ').any(|x| x == "off");
                        if off == *shown {
                            let new = if *shown {
                                class
                                    .split(' ')
                                    .filter(|x| *x != "off")
                                    .collect::<Vec<_>>()
                                    .join(" ")
                            } else {
                                format!("{class} off")
                            };
                            dom.set_class(c, &new);
                            changed = true;
                        }
                    }
                }
            }
            // Status strips: drop the lowest-priority seg while the row overflows.
            for id in sv.st.fit.clone() {
                let Some(m) = sv.mounts.get(&id) else {
                    continue;
                };
                if m.slots.len() < 2 {
                    continue;
                }
                // Overflow: a side's segments are wider than the side.
                let over = m.slots[..2].iter().any(|&s| {
                    dom.doc
                        .border_box(s)
                        .is_some_and(|b| dom.doc.scroll_size(s).0 > b.w + 0.5)
                });
                let (l, rr) = (m.slots[0], m.slots[1]);
                let mut segs: Vec<(NodeId, f64, usize, bool)> = Vec::new();
                for (side, slot) in [(0usize, l), (1usize, rr)] {
                    let kids = dom.doc.element_children(slot);
                    let n = kids.len();
                    for (i, k) in kids.into_iter().enumerate() {
                        let pr = dom
                            .doc
                            .attribute(k, "data-priority")
                            .and_then(|p| p.parse::<f64>().ok())
                            .unwrap_or(0.0);
                        // Distance from its edge: left segs from the left, right from the right.
                        let dist = if side == 0 { i } else { n - 1 - i };
                        let hidden = dom.class_of(k).split(' ').any(|c| c == "sf-fit-off");
                        segs.push((k, pr, dist, hidden));
                    }
                }
                let visible = segs.iter().filter(|s| !s.3).count();
                if over && visible > 1 {
                    let victim = segs
                        .iter()
                        .filter(|s| !s.3 && !dom.class_of(s.0).split(' ').any(|c| c == "shrinks"))
                        .min_by(|a, b| {
                            a.1.partial_cmp(&b.1)
                                .unwrap_or(std::cmp::Ordering::Equal)
                                .then(b.2.cmp(&a.2))
                        })
                        .map(|s| s.0);
                    if let Some(v) = victim {
                        let c = dom.class_of(v).to_owned();
                        dom.set_class(v, &format!("{c} sf-fit-off"));
                        changed = true;
                    }
                }
            }
            // Overlay anchoring to a caret or a node: below, or flipped above, clamped 8px.
            for id in sv.st.anchored.clone() {
                let Some(m) = sv.mounts.get(&id) else {
                    continue;
                };
                let Some(card) = m
                    .generated
                    .iter()
                    .copied()
                    .find(|g| dom.class_of(*g) == "sf-ov-card")
                else {
                    continue;
                };
                let Some(layer) = sv.regions[2] else { continue };
                let Some(lr) = dom.doc.border_box(layer) else {
                    continue;
                };
                let target = sv.st.anchor_targets.get(&id).and_then(|t| {
                    sv.mounts.get(t).and_then(|tm| match &tm.extra {
                        Extra::Field(f) if sv.st.anchor_caret.contains(&id) => f.caret,
                        _ => Some(tm.el),
                    })
                });
                let Some(target) = target else { continue };
                let Some(tr) = dom.doc.border_box(target) else {
                    continue;
                };
                let Some(cr) = dom.doc.border_box(card) else {
                    continue;
                };
                // A target outside the layer is placed from where the window shows it.
                let tr = if shown.is_empty() || inside(dom.doc, target, layers) {
                    Some(tr)
                } else {
                    shown
                        .iter()
                        .find(|&&(a, b, _)| tr.y >= a && tr.y < b)
                        .map(|&(a, _, at)| {
                            auk_ui::Rect::new(tr.x, lr.y + at + tr.y - a, tr.w, tr.h)
                        })
                };
                let lost = tr.is_none();
                let c = dom.class_of(m.el).to_owned();
                if lost != c.split(' ').any(|x| x == "lost") {
                    let new = if lost {
                        format!("{c} lost")
                    } else {
                        c.split(' ')
                            .filter(|x| *x != "lost")
                            .collect::<Vec<_>>()
                            .join(" ")
                    };
                    dom.set_class(m.el, &new);
                    changed = true;
                }
                let Some(tr) = tr else { continue };
                let below = lr.y + lr.h - (tr.y + tr.h);
                let above = tr.y - lr.y;
                let flip = cr.h + 8.0 > below && above > below;
                let top = if flip {
                    tr.y - cr.h - 4.0
                } else {
                    tr.y + tr.h + 4.0
                };
                let left = tr.x.min(lr.x + lr.w - cr.w - 8.0).max(lr.x + 8.0);
                let top = top.clamp(lr.y + 8.0, (lr.y + lr.h - cr.h - 8.0).max(lr.y + 8.0));
                let css = format!(
                    "left: {}px; top: {}px",
                    crate::common::fmt_num((left - lr.x) as f64),
                    crate::common::fmt_num((top - lr.y) as f64)
                );
                if dom.w.style(card) != Some(css.as_str()) {
                    dom.set_style(card, &css);
                    let c = dom.class_of(m.el).to_owned();
                    let has_above = c.split(' ').any(|x| x == "above");
                    if flip != has_above {
                        let new = if flip {
                            format!("{c} above")
                        } else {
                            c.split(' ')
                                .filter(|x| *x != "above")
                                .collect::<Vec<_>>()
                                .join(" ")
                        };
                        dom.set_class(m.el, &new);
                    }
                    changed = true;
                }
            }
        }
        // The toast layer rises to 12px above the dock while the dock shows something (a
        // composer), so a notice never covers the input (book:9547-9553).
        let lift = match (dom.doc.border_box(pane_el), dom.doc.border_box(docks_el)) {
            (Some(p), Some(d)) if d.h > 0.5 => format!(
                "bottom: {}px",
                crate::common::fmt_num((p.y + p.h - d.y + 12.0) as f64)
            ),
            _ => String::new(),
        };
        if dom.w.style(toasts_el).unwrap_or("") != lift {
            dom.set_style(toasts_el, &lift);
            changed = true;
        }
        self.measure_visits += visits;
        changed
    }

    /// Scrolling after layout: follow-tail, field carets, `topline`, `reveal` and `scroll`.
    /// Returns the reveals of rows virtualized out of view (their boxes estimates), by surface.
    fn apply_scrolls(&mut self, doc: &mut Document) -> Vec<(usize, auk_tsp::Reveal)> {
        let lh = self.settings.cell.1;
        let mut estimated = Vec::new();
        for (i, sv) in self.surfaces.iter_mut().enumerate() {
            crate::kinds::picker::apply_reveals(doc, &mut sv.st.pickers);
            for &clip in sv.st.follow.values() {
                if doc.try_node(clip).is_none() {
                    continue;
                }
                let (_, my) = doc.max_scroll(clip);
                let (_, y) = doc.scroll_offset(clip);
                let last = sv.st.follow_at.get(&clip).copied();
                // Follow while the user hasn't scrolled up from the end.
                if last.is_none_or(|l| (y - l).abs() < 1.0) {
                    doc.set_scroll_offset(clip, 0.0, my);
                    sv.st.follow_at.insert(clip, doc.scroll_offset(clip).1);
                }
            }
            for el in std::mem::take(&mut sv.st.scroll_end_x) {
                if doc.try_node(el).is_some() {
                    let (mx, _) = doc.max_scroll(el);
                    doc.set_scroll_offset(el, mx, doc.scroll_offset(el).1);
                }
            }
            for &(scroll, top) in sv.st.topline.values() {
                if doc.try_node(scroll).is_some() {
                    doc.set_scroll_offset(scroll, 0.0, top as f32 * lh);
                }
            }
            for &(scroll, caret) in sv.st.caret_follow.values() {
                let (Some(caret), true) = (caret, doc.try_node(scroll).is_some()) else {
                    continue;
                };
                if doc.try_node(caret).is_none() {
                    continue;
                }
                let (Some(sr), Some(cr)) = (doc.border_box(scroll), doc.border_box(caret)) else {
                    continue;
                };
                let (_, y) = doc.scroll_offset(scroll);
                if cr.y < sr.y {
                    doc.set_scroll_offset(scroll, 0.0, y - (sr.y - cr.y));
                } else if cr.y + cr.h > sr.y + sr.h {
                    doc.set_scroll_offset(scroll, 0.0, y + (cr.y + cr.h - sr.y - sr.h));
                }
            }
            for (sc, el, top) in std::mem::take(&mut sv.st.reveal_els) {
                let (Some(sr), Some(er)) = (
                    doc.try_node(sc).and(doc.border_box(sc)),
                    doc.try_node(el).and(doc.border_box(el)),
                ) else {
                    continue;
                };
                let (_, y) = doc.scroll_offset(sc);
                let to = if top || er.y < sr.y {
                    y + er.y - sr.y
                } else if er.y + er.h > sr.y + sr.h {
                    y + er.y + er.h - sr.y - sr.h
                } else {
                    y
                };
                doc.set_scroll_offset(sc, 0.0, to);
            }
            if let Some(r) = sv.st.reveal.take()
                && let Some(m) = sv.mounts.get(&r.id)
            {
                let el = m.el;
                if !doc.laid_out(el) {
                    estimated.push((i, r.clone()));
                }
                match scroll_parent(doc, el) {
                    Some(sc) => {
                        if let (Some(sr), Some(er)) = (doc.border_box(sc), doc.border_box(el)) {
                            let (_, y) = doc.scroll_offset(sc);
                            let to = match r.at {
                                RevealAt::Start => y + er.y - sr.y,
                                RevealAt::End => y + er.y + er.h - sr.y - sr.h,
                                RevealAt::Nearest => {
                                    if er.y < sr.y {
                                        y + er.y - sr.y
                                    } else if er.y + er.h > sr.y + sr.h {
                                        y + er.y + er.h - sr.y - sr.h
                                    } else {
                                        y
                                    }
                                }
                            };
                            doc.set_scroll_offset(sc, 0.0, to);
                        }
                    }
                    None => self.pane_reveal = Some((el, r.at)),
                }
            }
            for s in std::mem::take(&mut sv.st.scrolls) {
                let Some(m) = sv.mounts.get(&s.id) else {
                    continue;
                };
                // An inline surface's `main` ignores it (§7.6): a region steps only itself, and
                // only a screen surface's `main` is a scroll container. It never reaches into a
                // scroller inside it.
                let sc = if m.region {
                    is_scroller(doc, m.el).then_some(m.el)
                } else {
                    scroll_parent_or_self(doc, m.el)
                };
                let Some(sc) = sc else {
                    continue;
                };
                let page = doc.border_box(sc).map(|r| r.h).unwrap_or(0.0);
                let (_, y) = doc.scroll_offset(sc);
                let (_, my) = doc.max_scroll(sc);
                let to = match s.by {
                    ScrollBy::LineUp => y - lh,
                    ScrollBy::LineDown => y + lh,
                    ScrollBy::PageUp => y - page,
                    ScrollBy::PageDown => y + page,
                    ScrollBy::Start => 0.0,
                    ScrollBy::End => my,
                };
                doc.set_scroll_offset(sc, 0.0, to);
                // `end` resumes follow.
                if s.by == ScrollBy::End {
                    sv.st.follow_at.insert(sc, doc.scroll_offset(sc).1);
                }
            }
        }
        estimated
    }
}

/// The part of the layer `el` shows in, when it is in the layer: the layer's box (the pane's
/// visible area) less what the scrolling ancestors between them (a card's body) clip.
fn layer_view(doc: &Document, el: NodeId, layers: NodeId) -> Option<auk_ui::Rect> {
    let mut clips = Vec::new();
    let mut cur = doc.parent(el);
    while let Some(p) = cur {
        if p == layers {
            let mut v = doc.border_box(layers)?;
            for c in clips {
                v = intersect(v, c);
            }
            return Some(v);
        }
        if is_scroller(doc, p)
            && let Some(b) = doc.border_box(p)
        {
            clips.push(b);
        }
        cur = doc.parent(p);
    }
    None
}

fn inside(doc: &Document, el: NodeId, ancestor: NodeId) -> bool {
    std::iter::successors(Some(el), |&e| doc.parent(e)).any(|e| e == ancestor)
}

/// Where two rects overlap (empty, with zero size, when they don't).
fn intersect(a: auk_ui::Rect, b: auk_ui::Rect) -> auk_ui::Rect {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let r = (a.x + a.w).min(b.x + b.w);
    let bo = (a.y + a.h).min(b.y + b.h);
    auk_ui::Rect::new(x, y, (r - x).max(0.0), (bo - y).max(0.0))
}

fn is_scroller(doc: &Document, el: NodeId) -> bool {
    doc.computed_style(el)
        .is_some_and(|s| s.is_scroll_container())
}

fn scroll_parent(doc: &Document, el: NodeId) -> Option<NodeId> {
    let mut cur = doc.parent(el);
    while let Some(p) = cur {
        if is_scroller(doc, p) {
            return Some(p);
        }
        if doc.attribute(p, "data-scope").is_some() {
            return None;
        }
        cur = doc.parent(p);
    }
    None
}

fn scroll_parent_or_self(doc: &Document, el: NodeId) -> Option<NodeId> {
    if is_scroller(doc, el) {
        return Some(el);
    }
    // The node's own scroller part (list scroll, ansi clip, editor scroll).
    let mut stack: Vec<NodeId> = doc.children(el).to_vec();
    let mut depth = 0;
    while let Some(c) = stack.pop() {
        if doc.node(c).is_element() && is_scroller(doc, c) {
            return Some(c);
        }
        depth += 1;
        if depth < 32 {
            stack.extend(doc.children(c).iter().copied());
        }
    }
    scroll_parent(doc, el)
}
