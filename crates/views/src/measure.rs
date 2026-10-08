//! Measure-then-layout loops (§8.3): card and tool preview clamps ("N more lines"), text cuts,
//! table column fitting, status segment dropping, overlay anchoring, follow-tail, field caret
//! scrolling, and `reveal`/`scroll` requests.

use crate::kinds::data::{cut, fit_columns};
use crate::view::{Extra, PaneView};
use auk_tsp::{RevealAt, ScrollBy};
use auk_ui::{Document, NodeId};

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
        let passes = doc.update_with(8, |doc| self.measure(doc));
        self.apply_scrolls(doc);
        passes
    }

    /// The part of the document the window shows, in document coordinates (the pane's scroll
    /// position). Virtualized containers lay out what's near it. `None`: the document viewport.
    pub fn set_view_rect(&mut self, rect: Option<auk_ui::Rect>) {
        self.view_rect = rect;
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
                let Some(r) = doc.border_box(el) else {
                    continue;
                };
                let want = (view.y - r.y, view.y + view.h - r.y);
                let have = sv.st.virt_window.get(&el).copied();
                let moved = have.is_none_or(|h| {
                    (h.0 - want.0).abs() > vh * 0.25 || (h.1 - want.1).abs() > vh * 0.25
                });
                if moved {
                    doc.set_virtual_window(el, Some(want));
                    sv.st.virt_window.insert(el, want);
                    changed = true;
                }
            }
        }
        let mut dom = crate::dom::Dom {
            doc,
            w: &mut self.written,
        };
        for sv in &mut self.surfaces {
            // Preview clamps.
            for (id, m) in &sv.mounts {
                if !matches!(m.kind.as_str(), "card" | "tool") {
                    continue;
                }
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
                let _ = id;
            }
            // Text cuts (`truncate` start/middle on one line).
            for t in sv.st.truncs.values() {
                if dom.doc.try_node(t.el).is_none() {
                    continue;
                }
                let Some(r) = dom.doc.border_box(t.el) else {
                    continue;
                };
                let cells = (r.w / cw).floor().max(1.0) as usize;
                let want = cut(&t.full, cells, &t.mode);
                let have = crate::events::text_content(dom.doc, t.el);
                if have != want {
                    dom.set_only_text(t.el, &want);
                    changed = true;
                }
            }
            // Table column fitting.
            for info in sv.st.tables.values() {
                if dom.doc.try_node(info.grid).is_none() {
                    continue;
                }
                let Some(r) = dom.doc.border_box(info.grid) else {
                    continue;
                };
                let fit = fit_columns(&info.cols, r.w, cw);
                let tpl: Vec<String> = fit
                    .iter()
                    .filter(|(shown, _)| *shown)
                    .map(|(_, w)| format!("{}px", crate::common::fmt_num(*w as f64)))
                    .collect();
                let css = format!("grid-template-columns: {}", tpl.join(" "));
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
        changed
    }

    /// Scrolling after layout: follow-tail, field carets, `topline`, `reveal` and `scroll`.
    fn apply_scrolls(&mut self, doc: &mut Document) {
        let lh = self.settings.cell.1;
        for sv in &mut self.surfaces {
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
            if let Some(r) = sv.st.reveal.take()
                && let Some(m) = sv.mounts.get(&r.id)
            {
                let el = m.el;
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
                // An inline surface's `main` ignores it (§7.6).
                let Some(sc) = scroll_parent_or_self(doc, m.el) else {
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
    }
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
