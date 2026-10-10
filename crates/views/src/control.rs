//! The pane view's clock, settings and host-facing controls.

use crate::ViewSettings;
use crate::dom::Dom;
use crate::kinds;
use crate::view::{Cx, PaneView, render_node};
use auk_tsp::{RevealAt, SurfaceKey, TspPane};
use auk_ui::{Document, NodeId};

impl PaneView {
    /// Re-render nodes after a local change (folds, flips, selection), with their children placed.
    pub(crate) fn refresh(
        &mut self,
        doc: &mut Document,
        pane: &TspPane,
        key: SurfaceKey,
        ids: &[String],
    ) {
        let Some(surface) = pane.surface(key) else {
            return;
        };
        let Some(i) = self.surfaces.iter().position(|s| s.key == key) else {
            return;
        };
        let now = self.last_now;
        self.pass += 1;
        let settings = &self.settings;
        let cols = self.cols;
        let pass = self.pass;
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
            pass,
            blobs: pane.blobs(),
        };
        for id in ids {
            let Some(node) = surface.document().get(id) else {
                continue;
            };
            let Some(mut m) = sv.mounts.remove(id) else {
                continue;
            };
            if !m.region {
                render_node(&mut cx, &mut m, node);
            }
            sv.mounts.insert(id.clone(), m);
            if kinds::has_children(node.kind()) {
                crate::view::place_children_pub(&mut cx, &mut sv.mounts, &mut sv.clocked, node);
            }
        }
        self.fit_dirty = true;
    }

    /// Advance motion to `now` (ms, the view clock). Returns when the next frame is due, or
    /// `None` when nothing moves (§7.13: frames only while something animates and the pane
    /// shows).
    pub fn tick(&mut self, doc: &mut Document, pane: &TspPane, now: f64) -> Option<f64> {
        self.last_now = now;
        // CSS transitions and animations, on the same clock (§7.13, §8.4).
        doc.set_time(self.css_clock(now));
        // Toasts the last sync asked for.
        let mut next: Option<f64> = None;
        let mut dom = Dom {
            doc: &mut *doc,
            w: &mut self.written,
        };
        for sv in &mut self.surfaces {
            for req in std::mem::take(&mut sv.st.toasts) {
                self.toasts.queue.push(req);
            }
        }
        if let Some(req) = self.toasts.queue.pop() {
            self.toasts.queue.clear();
            self.toasts.show(&mut dom, req, now);
        }
        if let Some(t) = self.toasts.tick(&mut dom, now) {
            next = Some(t);
        }
        if !self.visible {
            return next;
        }
        let settings = &self.settings;
        let cols = self.cols;
        let pass = self.pass;
        let mut any = false;
        // Highlighting drawn in per frame, at most (the rest next frame).
        self.hl_budget = crate::highlight::PASS_LINES;
        self.hl_finish = crate::highlight::FINISH_LINES;
        let hl_budget = &mut self.hl_budget;
        for sv in &mut self.surfaces {
            let Some(surface) = pane.surface(sv.key) else {
                continue;
            };
            let ids: Vec<String> = sv.clocked.iter().cloned().collect();
            // Under a sheet (`.sf-covered`), spinners and shimmers in main and dock hold their
            // frame; timers and the rest still run.
            let held = sv.covered.then_some(sv.regions[2]);
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
                pass,
                blobs: pane.blobs(),
            };
            for id in ids {
                let Some(node) = surface.document().get(&id) else {
                    sv.clocked.remove(&id);
                    continue;
                };
                let Some(m) = sv.mounts.get_mut(&id) else {
                    sv.clocked.remove(&id);
                    continue;
                };
                // Highlighting the background finished draws in (§8.5), when the node is in
                // view; out of view, the measure loop draws it in once it's laid out.
                let drawing_in = !m.hl.is_empty() && cx.dom.doc.laid_out(m.el) && {
                    m.hl.retain_mut(|d| d.draw_in(&mut cx.dom, &mut 0, hl_budget));
                    if m.hl.is_empty() {
                        cx.st.hl_waiting.remove(&id);
                    }
                    !m.hl.is_empty()
                };
                if let Some(layer) = held
                    && matches!(
                        m.extra,
                        crate::view::Extra::Spin(_) | crate::view::Extra::Shimmer(_)
                    )
                    && !layer.is_some_and(|l| {
                        std::iter::successors(Some(m.el), |&e| cx.dom.doc.parent(e)).any(|e| e == l)
                    })
                {
                    // The Spindle mark rests under the sheet (`--sf-spindle: still`).
                    crate::motion::spindle_rest(&mut cx, m);
                    any |= drawing_in;
                    continue;
                }
                if kinds::tick(&mut cx, node, m) || drawing_in {
                    any = true;
                } else {
                    sv.clocked.remove(&id);
                }
            }
        }
        if doc.animating() {
            any = true;
        }
        if any {
            // Display rate: the host paces frames; ask for the next one.
            let frame = now + 1000.0 / 60.0;
            next = Some(next.map_or(frame, |n| n.min(frame)));
        }
        next
    }

    /// Finishes what renders left to the background and draws it in: the highlighting (on
    /// this thread), and the pictures decoding (waited for; the next tick draws them). For
    /// hosts that draw deterministically (headless goldens, tests).
    pub fn finish_background(&mut self, doc: &mut Document) {
        crate::image::settle();
        let mut dom = Dom {
            doc,
            w: &mut self.written,
        };
        for sv in &mut self.surfaces {
            for m in sv.mounts.values_mut() {
                for mut d in std::mem::take(&mut m.hl) {
                    let (mut finish, mut all) = (usize::MAX, usize::MAX);
                    d.draw_in(&mut dom, &mut finish, &mut all);
                }
            }
        }
    }

    /// Change settings (font, cell size, appearance, Reduce Motion, program colors, Edits,
    /// native editing): everything re-renders.
    pub fn set_settings(&mut self, doc: &mut Document, pane: &TspPane, settings: ViewSettings) {
        if settings == self.settings {
            return;
        }
        let theme = settings.dark != self.settings.dark;
        let media = settings.media() != self.settings.media();
        self.settings = settings;
        if theme {
            crate::sheets::set_theme(doc, self.settings.dark);
        }
        if media {
            self.settings.apply_media(doc);
        }
        let now = self.last_now;
        self.pass += 1;
        self.rerender_all(doc, pane, now);
        self.fit_dirty = true;
    }

    /// The pane width in cells (ansi reflow, diff split). Re-renders width-dependent kinds.
    pub fn set_cols(&mut self, doc: &mut Document, pane: &TspPane, cols: u16) {
        if cols == self.cols {
            return;
        }
        self.cols = cols;
        let now = self.last_now;
        self.pass += 1;
        self.rerender_kinds(doc, pane, now, &["ansi", "diff", "rows", "picker", "prefs"]);
        self.place_aside(doc, pane);
        self.fit_dirty = true;
    }

    /// The pane was hidden or shown: motion pauses (`.sf-paused`).
    pub fn set_visible(&mut self, doc: &mut Document, pane: &TspPane, visible: bool) {
        if visible != self.visible {
            let now = self.last_now;
            if visible {
                if let Some(at) = self.hidden_at.take() {
                    self.paused_ms += (now - at).max(0.0);
                }
            } else {
                self.hidden_at = Some(now);
            }
            self.visible = visible;
            for i in 0..self.surfaces.len() {
                self.write_regions_pub(doc, pane, i);
            }
        }
    }

    /// The pane gained or lost keyboard focus (`.sf-unfocused`). Unfocused, nothing in it
    /// matches `:focus*` and a press under way loses `:active`.
    pub fn set_focused(&mut self, doc: &mut Document, pane: &TspPane, focused: bool) {
        if focused != self.focused {
            self.focused = focused;
            for i in 0..self.surfaces.len() {
                self.write_regions_pub(doc, pane, i);
            }
            if !focused {
                self.release(doc);
            }
            self.update_focus(doc, pane);
        }
    }

    /// A `reveal` the surface couldn't scroll itself (the pane scrolls): the element and where.
    pub fn take_pane_reveal(&mut self) -> Option<(NodeId, RevealAt)> {
        self.pane_reveal.take()
    }

    /// The native selection of a field, if the view holds one.
    pub fn selection(&self, key: SurfaceKey, id: &str) -> Option<crate::Selection> {
        self.surfaces
            .iter()
            .find(|s| s.key == key)?
            .st
            .selection
            .get(id)
            .copied()
    }

    /// Whether a fold is shown collapsed (the local state).
    pub fn is_collapsed(
        &self,
        key: SurfaceKey,
        state_key_id: &str,
        pane: &TspPane,
    ) -> Option<bool> {
        let surface = pane.surface(key)?;
        let node = surface.document().get(state_key_id)?;
        let sv = self.surfaces.iter().find(|s| s.key == key)?;
        let k = crate::state::state_key(node);
        sv.st.folds.get(&k).map(|f| f.local)
    }

    fn rerender_kinds(&mut self, doc: &mut Document, pane: &TspPane, now: f64, kinds_: &[&str]) {
        let settings = &self.settings;
        let cols = self.cols;
        let pass = self.pass;
        for sv in &mut self.surfaces {
            let Some(surface) = pane.surface(sv.key) else {
                continue;
            };
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
                pass,
                blobs: pane.blobs(),
            };
            let ids: Vec<String> = sv
                .mounts
                .iter()
                .filter(|(_, m)| kinds_.contains(&m.kind.as_str()))
                .map(|(k, _)| k.clone())
                .collect();
            for id in ids {
                let Some(node) = surface.document().get(&id) else {
                    continue;
                };
                let Some(mut m) = sv.mounts.remove(&id) else {
                    continue;
                };
                render_node(&mut cx, &mut m, node);
                sv.mounts.insert(id, m);
            }
        }
    }
}
