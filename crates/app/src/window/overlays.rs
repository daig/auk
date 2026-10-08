//! What the window draws over its panes and the input that goes to it: the command palette,
//! context menus for surface `menu` actions (§7.8), the image viewer for `zoom`, toasts and the
//! status line; and the program-field keys native editing takes (§7.9).

use auk_mirror::PaneId;
use auk_platform_macos::{KeyInput, MouseButton, MouseEvent, Window};
use auk_plugins::ToastLevel;
use auk_term::input::Key;
use auk_views::{FieldKey, Outcome, PointerEvent, Zoom};

use super::WinState;
use crate::chrome::{
    BAR_HEIGHT, ChromeAction, ChromeModel, MenuView, PaletteView, STATUS_HEIGHT, ViewerView,
};
use crate::palette::{self, Palette, Row, RowAction};

/// An open context menu: the pane, the document point it was asked for, and its entries.
#[derive(Clone, Debug)]
pub struct ContextMenu {
    pub pane: PaneId,
    pub doc_point: (f32, f32),
    pub at: (f64, f64),
    pub entries: Vec<auk_views::MenuEntry>,
    pub selected: Option<usize>,
}

/// The image viewer: the pane whose blobs it shows and the zoom set.
#[derive(Clone, Debug)]
pub struct Viewer {
    pub pane: PaneId,
    pub zoom: Zoom,
}

/// The window's overlays.
#[derive(Debug, Default)]
pub struct Overlays {
    pub palette: Option<Palette>,
    pub menu: Option<ContextMenu>,
    pub viewer: Option<Viewer>,
}

impl Overlays {
    pub fn any(&self) -> bool {
        self.palette.is_some() || self.menu.is_some() || self.viewer.is_some()
    }
}

impl WinState {
    // -----------------------------------------------------------------------------------------
    // The chrome model

    /// Adds the status line, toasts, overlays and plugin tab titles to the chrome model.
    pub(super) fn decorate_chrome(&mut self, model: &mut ChromeModel) {
        self.plugins_chrome();
        for t in &mut model.tabs {
            if let Some(title) = self.plugins.tab_titles.get(&t.0) {
                t.1 = title.clone();
            }
        }
        model.status = self.plugins.status.clone();
        model.card = self.plugins.block_card.as_ref().map(|c| crate::chrome::CardView {
            title: "Could not start the block".to_owned(),
            sub: c.error.clone(),
            retry: "card:retry".to_owned(),
        });
        // Presence (§10.1): who else is attached to the host.
        if let Some(text) = self.presence_text() {
            model.status.push(crate::chrome::StatusItem {
                plugin: "auk.presence".to_owned(),
                text,
                icon: Some("users".to_owned()),
                tone: Some("muted".to_owned()),
                command: None,
            });
        }
        model.status_title = self
            .focused_pane()
            .map(|p| {
                let shared = self.conn.shared();
                shared
                    .panes
                    .get(&p)
                    .map(|d| d.replica.terminal().title().to_owned())
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        model.toasts = self.plugins.toasts.iter().map(|t| t.item.clone()).collect();
        model.palette = self.overlays.palette.as_ref().map(|p| PaletteView {
            query: p.query.clone(),
            rows: p.rows().into_iter().map(|r| (r.label, r.group)).collect(),
            selected: p.selected,
        });
        model.menu = self.overlays.menu.as_ref().map(|m| MenuView {
            x: m.at.0,
            y: m.at.1,
            entries: m.entries.iter().map(|e| e.label.clone()).collect(),
            selected: m.selected,
        });
        model.viewer = self.overlays.viewer.as_ref().map(|v| {
            let img = &v.zoom.images[v.zoom.index];
            ViewerView {
                title: img.title.clone().unwrap_or_else(|| "Image".to_owned()),
                index: v.zoom.index,
                count: v.zoom.images.len(),
                blob: img.blob.clone(),
                size: self.viewer_size,
                open: img.open.clone(),
            }
        });
    }

    // -----------------------------------------------------------------------------------------
    // Palette

    /// The palette's rows: built-in commands, then plugin commands that are available now.
    fn palette_rows(&mut self) -> Vec<Row> {
        let mut rows = palette::builtin_rows();
        let commands = self
            .plugins
            .rt
            .as_ref()
            .map(auk_plugins::WindowPlugins::commands)
            .unwrap_or_default();
        for c in commands {
            let action = c.action.clone();
            let shown = self
                .with_rt(|rt, host| rt.available(host, &action))
                .unwrap_or(false);
            if shown {
                rows.push(Row {
                    label: c.title,
                    group: c.group,
                    icon: c.icon,
                    action: RowAction::Plugin(c.action),
                });
            }
        }
        // "New <title> block" for each Ready plugin's palette block types (§9.5).
        self.plugins_catalog();
        for (p, b) in self.plugins.catalog.block_types() {
            if b.palette {
                rows.push(Row {
                    label: format!("New {} block", b.title),
                    group: p.name.clone(),
                    icon: b.icon.clone(),
                    action: RowAction::Block(format!("{}.{}", p.id, b.id)),
                });
            }
        }
        rows
    }

    pub(super) fn open_palette(&mut self) {
        let rows = self.palette_rows();
        self.overlays.menu = None;
        self.overlays.palette = Some(Palette::new(rows));
        self.ui_dirty = true;
    }

    /// Runs a palette row.
    pub(super) fn palette_run(&mut self, row: Row) {
        self.overlays.palette = None;
        self.ui_dirty = true;
        match row.action {
            RowAction::Builtin(cmd) => {
                if let Some(w) = self.window.clone() {
                    self.command(&w, cmd);
                }
            }
            RowAction::Plugin(action) => {
                if let Err(e) = self.run_action_named(&action) {
                    eprintln!("auk: palette: {e}");
                }
            }
            RowAction::Open(path) => {
                self.open_path_from_palette(&path);
            }
            RowAction::Block(kind) => {
                let cwd = self.focused_cwd();
                self.open_block(
                    &kind,
                    &[],
                    auk_plugins::window::How::Tab,
                    auk_plugins::window::Reveal { focus: true },
                    cwd,
                );
            }
        }
    }

    fn palette_key(&mut self, key: &KeyInput) -> bool {
        let Some(p) = self.overlays.palette.as_mut() else {
            return false;
        };
        match key.event.key {
            Key::Escape => self.overlays.palette = None,
            Key::Up => p.step(false),
            Key::Down => p.step(true),
            Key::Backspace => p.backspace(),
            Key::Enter | Key::Keypad('\r') => {
                if let Some(row) = p.chosen() {
                    self.palette_run(row);
                } else {
                    self.overlays.palette = None;
                }
            }
            _ => {
                if key.event.mods.meta || key.event.mods.ctrl {
                    return true;
                }
                // Text arrives as a Text event.
                return false;
            }
        }
        self.ui_dirty = true;
        true
    }

    /// Text typed while the palette is open.
    pub(super) fn palette_text(&mut self, text: &str) -> bool {
        let Some(p) = self.overlays.palette.as_mut() else {
            return false;
        };
        p.type_text(text);
        self.ui_dirty = true;
        if let Some(w) = &self.window {
            w.request_frame();
        }
        true
    }

    // -----------------------------------------------------------------------------------------
    // Keys for overlays and program fields

    /// Keys an overlay takes before anything else. Returns whether one did.
    pub(super) fn overlay_key(&mut self, window: &Window, key: &KeyInput) -> bool {
        let took = if self.overlays.palette.is_some() {
            self.palette_key(key)
        } else if let Some(m) = self.overlays.menu.as_mut() {
            let n = m.entries.len();
            match key.event.key {
                Key::Escape => self.overlays.menu = None,
                Key::Up if n > 0 => {
                    m.selected = Some(m.selected.map_or(n - 1, |i| (i + n - 1) % n))
                }
                Key::Down if n > 0 => m.selected = Some(m.selected.map_or(0, |i| (i + 1) % n)),
                Key::Enter | Key::Keypad('\r') => {
                    if let Some(i) = m.selected {
                        self.menu_pick(window, i);
                    } else {
                        self.overlays.menu = None;
                    }
                }
                _ => {}
            }
            true
        } else if self.overlays.viewer.is_some() {
            match key.event.key {
                Key::Escape | Key::Char(' ') => self.close_viewer(),
                Key::Left => self.viewer_step(false),
                Key::Right => self.viewer_step(true),
                _ => {}
            }
            true
        } else {
            false
        };
        if took {
            self.ui_dirty = true;
            window.request_frame();
        }
        took
    }

    /// A key in the focused pane's program field (§7.9): native editing takes selecting keys,
    /// caret moves and deletes with a selection as `edit` events. Returns whether it did.
    pub(super) fn field_key_input(
        &mut self,
        window: &Window,
        pane: PaneId,
        key: &KeyInput,
    ) -> bool {
        let m = key.event.mods;
        let fk = match key.event.key {
            Key::Left if m.meta => FieldKey::LineStart,
            Key::Right if m.meta => FieldKey::LineEnd,
            Key::Left => FieldKey::Left,
            Key::Right => FieldKey::Right,
            Key::Up => FieldKey::Up,
            Key::Down => FieldKey::Down,
            Key::Home => FieldKey::LineStart,
            Key::End => FieldKey::LineEnd,
            Key::Backspace => FieldKey::Backspace,
            Key::Delete => FieldKey::Delete,
            Key::Char('a') if m.meta => FieldKey::SelectAll,
            Key::Char('z') if m.meta || m.ctrl => {
                return self.field_outcome(window, pane, |pv, _, tsp| pv.undo(tsp));
            }
            Key::Char('x') if m.meta => {
                // Cut: the selection to the clipboard, then deleted as an edit.
                if let Some(text) = self.field_selection_text(pane) {
                    crate::app::clipboard_set(&text);
                    return self
                        .field_outcome(window, pane, |pv, doc, tsp| pv.delete_selection(doc, tsp));
                }
                return false;
            }
            Key::Char('c') if m.meta => {
                if let Some(text) = self.field_selection_text(pane) {
                    crate::app::clipboard_set(&text);
                    return true;
                }
                return false;
            }
            _ => return false,
        };
        if (m.ctrl || m.alt) && !matches!(fk, FieldKey::SelectAll) {
            return false;
        }
        self.field_outcome(window, pane, |pv, doc, tsp| {
            pv.field_key(doc, tsp, fk, m.shift)
        })
    }

    /// The focused field's native selection text.
    pub(super) fn field_selection_text(&self, pane: PaneId) -> Option<String> {
        let sf = self.surfaces.get(&pane)?;
        let shared = self.conn.shared();
        let tsp = shared.panes.get(&pane)?.replica.tsp();
        sf.view().field_selection(tsp)
    }

    /// Runs a view call on the pane's focused field and carries out its outcome. Returns
    /// whether the view took the input.
    fn field_outcome(
        &mut self,
        window: &Window,
        pane: PaneId,
        f: impl FnOnce(&mut auk_views::PaneView, &mut auk_ui::Document, &auk_tsp::TspPane) -> Outcome,
    ) -> bool {
        let Some(sf) = self.surfaces.get_mut(&pane) else {
            return false;
        };
        let outcome = {
            let conn = self.conn.clone();
            let shared = conn.shared();
            let Some(data) = shared.panes.get(&pane) else {
                return false;
            };
            let tsp = data.replica.tsp();
            if !sf.view().field_focused(tsp) {
                return false;
            }
            let (doc, pv) = sf.parts();
            f(pv, doc, tsp)
        };
        let took = !matches!(outcome, Outcome::Keys);
        if took {
            self.ui_dirty = true;
            self.outcomes(window, pane, vec![outcome]);
        }
        took
    }

    /// IME composition in the focused pane's program field: drawn at the field's caret.
    /// Returns whether a field took it.
    pub(super) fn field_preedit(&mut self, pane: PaneId, text: &str, cursor: usize) -> bool {
        let Some(sf) = self.surfaces.get_mut(&pane) else {
            return false;
        };
        let conn = self.conn.clone();
        let shared = conn.shared();
        let Some(data) = shared.panes.get(&pane) else {
            return false;
        };
        let tsp = data.replica.tsp();
        if !sf.view().field_focused(tsp) {
            return false;
        }
        let (doc, pv) = sf.parts();
        pv.ime_preedit(doc, tsp, text, cursor);
        self.ui_dirty = true;
        true
    }

    // -----------------------------------------------------------------------------------------
    // Pointer: chrome, context menus, hover

    /// A press the chrome takes: the tab bar, the status line, toasts, and anything while an
    /// overlay is open. Returns whether it did.
    pub(super) fn chrome_press(
        &mut self,
        window: &Window,
        m: &MouseEvent,
        button: MouseButton,
    ) -> bool {
        let height = self.size.map_or(800.0, |s| s.height);
        let action = self.chrome.action_at(m.x, m.y);
        let overlay = self.overlays.any();
        let in_bars = m.y < BAR_HEIGHT || m.y > height - STATUS_HEIGHT;
        let toast = matches!(action, Some(ChromeAction::Toast(_)));
        // The block card's buttons (§9.5).
        let card = matches!(&action, Some(ChromeAction::Run(n)) if n.starts_with("card:"));
        if !(overlay || in_bars || toast || card) {
            return false;
        }
        if button == MouseButton::Left
            && let Some(action) = action
        {
            self.chrome_action(action);
        } else if overlay && action.is_none() && self.overlays.menu.is_some() {
            self.overlays.menu = None;
        }
        self.ui_dirty = true;
        window.request_frame();
        true
    }

    /// Actions of the status line, toasts and overlays.
    pub(super) fn overlay_action(&mut self, action: ChromeAction) {
        let Some(window) = self.window.clone() else {
            return;
        };
        match action {
            ChromeAction::Run(name) => {
                if let Err(e) = self.run_keymap_action(&name) {
                    eprintln!("auk: status line: {e}");
                }
            }
            ChromeAction::Palette(i) => {
                if let Some(row) = self
                    .overlays
                    .palette
                    .as_ref()
                    .and_then(|p| p.rows().into_iter().nth(i))
                {
                    self.palette_run(row);
                }
            }
            ChromeAction::Menu(i) => self.menu_pick(&window, i),
            ChromeAction::Toast(i) => {
                if i < self.plugins.toasts.len() {
                    self.plugins.toasts.remove(i);
                }
            }
            ChromeAction::Dismiss => {
                self.overlays.palette = None;
                self.overlays.menu = None;
                self.close_viewer();
            }
            ChromeAction::ViewerPrev => self.viewer_step(false),
            ChromeAction::ViewerNext => self.viewer_step(true),
            ChromeAction::ViewerOpen => {
                let target = self
                    .overlays
                    .viewer
                    .as_ref()
                    .and_then(|v| v.zoom.images[v.zoom.index].open.clone());
                if let Some(t) = target {
                    let pane = self.overlays.viewer.as_ref().map(|v| v.pane);
                    self.close_viewer();
                    self.open_routed(&t, "link", pane);
                }
            }
            ChromeAction::Session(_)
            | ChromeAction::Tab(_)
            | ChromeAction::NewSession
            | ChromeAction::NewTab => {}
        }
        self.ui_dirty = true;
        window.request_frame();
    }

    /// A pane-relative press point in its surfaces' document, if a surface is under it.
    pub(super) fn surface_point(&self, pane: PaneId, x: f64, y: f64) -> Option<(f32, f32)> {
        let scale = self.size.map_or(2.0, |s| s.scale);
        let view = self.views.get(&pane)?;
        let px = ((x - view.grid_origin.0) * scale) as f32;
        let py = ((y - view.grid_origin.1) * scale) as f32;
        if px < 0.0 || py < 0.0 {
            return None;
        }
        let sf = self.surfaces.get(&pane)?;
        sf.doc_point(&view.arranged, px, py, view.content_px, scale as f32)
    }

    /// The command-block document point under a window point, if a command block is there.
    fn lens_point(&self, pane: PaneId, x: f64, y: f64) -> Option<(f32, f32)> {
        let scale = self.size.map_or(2.0, |s| s.scale);
        let view = self.views.get(&pane)?;
        let px = ((x - view.grid_origin.0) * scale) as f32;
        let py = ((y - view.grid_origin.1) * scale) as f32;
        if px < 0.0 || py < 0.0 {
            return None;
        }
        self.surfaces
            .get(&pane)?
            .lens_point(&view.arranged, px, py, scale as f32)
    }

    /// A secondary press over a surface opens its context menu (§7.8 `menu`). Returns whether
    /// one opened.
    pub(super) fn context_menu_at(&mut self, pane: PaneId, m: &MouseEvent) -> bool {
        let Some((dx, dy)) = self.surface_point(pane, m.x, m.y) else {
            return false;
        };
        let entries = {
            let Some(sf) = self.surfaces.get(&pane) else {
                return false;
            };
            let shared = self.conn.shared();
            let Some(data) = shared.panes.get(&pane) else {
                return false;
            };
            sf.view().menu(sf.doc(), data.replica.tsp(), dx, dy)
        };
        if entries.is_empty() {
            return false;
        }
        self.overlays.palette = None;
        self.overlays.menu = Some(ContextMenu {
            pane,
            doc_point: (dx, dy),
            at: (m.x, m.y),
            entries,
            selected: None,
        });
        self.ui_dirty = true;
        true
    }

    /// Runs context menu entry `i` on the node it was opened for.
    pub(super) fn menu_pick(&mut self, window: &Window, i: usize) {
        let Some(menu) = self.overlays.menu.take() else {
            return;
        };
        let Some(entry) = menu.entries.get(i) else {
            return;
        };
        let outcomes = {
            let Some(sf) = self.surfaces.get_mut(&menu.pane) else {
                return;
            };
            let conn = self.conn.clone();
            let shared = conn.shared();
            let Some(data) = shared.panes.get(&menu.pane) else {
                return;
            };
            let (doc, pv) = sf.parts();
            let (x, y) = menu.doc_point;
            let mut out = pv.menu_pick(doc, data.replica.tsp(), x, y, &entry.action);
            if out.is_empty() && entry.action == "copy" {
                // Copy with no node holding a menu: the text under the point.
                out = pv.click(
                    doc,
                    data.replica.tsp(),
                    PointerEvent {
                        x,
                        y,
                        clicks: 1,
                        ..PointerEvent::default()
                    },
                );
                out.retain(|o| matches!(o, Outcome::Copy(_)));
            }
            out
        };
        self.ui_dirty = true;
        self.outcomes(window, menu.pane, outcomes);
    }

    /// Hover over surfaces (`:hover`). Returns whether anything may have changed.
    pub(super) fn hover_at(&mut self, x: f64, y: f64) -> bool {
        let over = self.tab_layout.pane_at(x, y);
        let mut changed = false;
        let panes: Vec<PaneId> = self.surfaces.keys().copied().collect();
        for pane in panes {
            let (point, lens_point) = if Some(pane) == over {
                (self.surface_point(pane, x, y), self.lens_point(pane, x, y))
            } else {
                (None, None)
            };
            let Some(sf) = self.surfaces.get_mut(&pane) else {
                continue;
            };
            // Command blocks show their toolbar on hover (§9.6).
            changed |= sf.lens_hover(lens_point);
            let (doc, pv) = sf.parts();
            let (hx, hy) = point.unwrap_or((-1.0e6, -1.0e6));
            let before = pv.hovered().to_vec();
            pv.hover(doc, hx, hy);
            if pv.hovered() != before.as_slice() {
                changed = true;
            }
        }
        if changed {
            self.ui_dirty = true;
        }
        changed
    }

    // -----------------------------------------------------------------------------------------
    // Image viewer

    pub(super) fn show_viewer(&mut self, pane: PaneId, zoom: Zoom) {
        self.overlays.palette = None;
        self.overlays.menu = None;
        self.overlays.viewer = Some(Viewer { pane, zoom });
        self.load_viewer_image();
    }

    fn close_viewer(&mut self) {
        if self.overlays.viewer.take().is_some() {
            self.chrome.set_viewer_image(None);
        }
    }

    fn viewer_step(&mut self, forward: bool) {
        if let Some(v) = self.overlays.viewer.as_mut() {
            let n = v.zoom.images.len();
            if n > 1 {
                v.zoom.index = if forward {
                    (v.zoom.index + 1) % n
                } else {
                    (v.zoom.index + n - 1) % n
                };
            }
        }
        self.load_viewer_image();
    }

    /// Decodes the shown image from the pane's blobs into the chrome.
    fn load_viewer_image(&mut self) {
        let Some(v) = &self.overlays.viewer else {
            return;
        };
        let scale = self.size.map_or(2.0, |s| s.scale) as f32;
        let blob = v.zoom.images[v.zoom.index].blob.clone();
        let picture = {
            let shared = self.conn.shared();
            shared
                .panes
                .get(&v.pane)
                .and_then(|p| p.replica.tsp().blobs().get(&blob).cloned())
                .map(|b| auk_views::image::load_blob(&b, scale))
        };
        match picture {
            Some(Ok(p)) => {
                self.viewer_size = (f64::from(p.size.0), f64::from(p.size.1));
                self.chrome
                    .set_viewer_image(p.frames.first().map(|f| f.content.clone()));
            }
            Some(Err(e)) => {
                self.toast(ToastLevel::Error, "Can't show the image", Some(&e));
                self.close_viewer();
            }
            None => {
                self.viewer_size = (1.0, 1.0);
                self.chrome.set_viewer_image(None);
            }
        }
        self.ui_dirty = true;
    }

    /// Sends a surface pointer event's outcome that opens a path or link through routes.
    pub(super) fn open_from_surface(&mut self, pane: PaneId, target: &str) {
        self.open_routed(target, "link", Some(pane));
    }
}
