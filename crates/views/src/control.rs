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
        for sv in &mut self.surfaces {
            let Some(surface) = pane.surface(sv.key) else {
                continue;
            };
            let ids: Vec<String> = sv.clocked.iter().cloned().collect();
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
                if kinds::tick(&mut cx, node, m) {
                    any = true;
                } else {
                    sv.clocked.remove(&id);
                }
            }
        }
        if any {
            // Display rate: the host paces frames; ask for the next one.
            let frame = now + 1000.0 / 60.0;
            next = Some(next.map_or(frame, |n| n.min(frame)));
        }
        next
    }

    /// Change settings (font, cell size, appearance, Reduce Motion, program colors, Edits,
    /// native editing): everything re-renders.
    pub fn set_settings(&mut self, doc: &mut Document, pane: &TspPane, settings: ViewSettings) {
        if settings == self.settings {
            return;
        }
        let theme = settings.dark != self.settings.dark;
        let motion = settings.reduce_motion != self.settings.reduce_motion;
        self.settings = settings;
        if theme {
            crate::sheets::set_theme(doc, self.settings.dark);
        }
        if motion {
            doc.set_media_preferences(self.settings.reduce_motion, false, false);
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
        self.rerender_kinds(doc, pane, now, &["ansi", "diff", "rows"]);
        self.fit_dirty = true;
    }

    /// The pane was hidden or shown: motion pauses (`.sf-paused`).
    pub fn set_visible(&mut self, doc: &mut Document, pane: &TspPane, visible: bool) {
        if visible != self.visible {
            self.visible = visible;
            for i in 0..self.surfaces.len() {
                self.write_regions_pub(doc, pane, i);
            }
        }
    }

    /// The pane gained or lost keyboard focus (`.sf-unfocused`).
    pub fn set_focused(&mut self, doc: &mut Document, pane: &TspPane, focused: bool) {
        if focused != self.focused {
            self.focused = focused;
            for i in 0..self.surfaces.len() {
                self.write_regions_pub(doc, pane, i);
            }
        }
    }

    /// Errors the view found that the program should hear (`unknown el tag`), as
    /// `(surface, node id, message)`.
    pub fn take_reports(&mut self) -> Vec<(SurfaceKey, String, String)> {
        let mut out = Vec::new();
        for sv in &mut self.surfaces {
            for (id, msg) in std::mem::take(&mut sv.st.reports) {
                if sv.st.reported.insert((id.clone(), msg.clone())) {
                    out.push((sv.key, id, msg));
                }
            }
        }
        out
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
