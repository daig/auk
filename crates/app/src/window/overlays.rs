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
    /// The shown picture is decoding in the background (amendment A9): frames look again.
    pub decoding: bool,
}

/// The window's overlays.
#[derive(Debug, Default)]
pub struct Overlays {
    pub palette: Option<Palette>,
    pub menu: Option<ContextMenu>,
    pub viewer: Option<Viewer>,
    /// The session chip's menu is open.
    pub session_menu: bool,
}

impl Overlays {
    pub fn any(&self) -> bool {
        self.palette.is_some() || self.menu.is_some() || self.viewer.is_some() || self.session_menu
    }
}

impl WinState {
    // -----------------------------------------------------------------------------------------
    // The chrome model

    /// Adds the status line, toasts, overlays and plugin tab titles to the chrome model.
    pub(super) fn decorate_chrome(&mut self, model: &mut ChromeModel) {
        self.plugins_chrome();
        for t in &mut model.bar.tabs {
            if let Some(title) = self.plugins.tab_titles.get(&t.id) {
                t.title = title.clone();
            }
        }
        model.status = self.plugins.status.clone();
        model.card = self.plugins.block_card.as_ref().map(|c| crate::chrome::CardView {
            title: "Could not start the block".to_owned(),
            sub: c.error.clone(),
            retry: "card:retry".to_owned(),
        });
        // Presence (§10.1): who else is attached to the host.
        if let Some(text) = self.presence_shown.clone() {
            model.status.push(crate::chrome::StatusItem {
                plugin: "auk.presence".to_owned(),
                text,
                icon: Some("users".to_owned()),
                tone: Some("muted".to_owned()),
                command: None,
            });
        }
        // The host's identity chain and the link's state (§10.1).
        model.status.extend(self.link_segments());
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
            app: model.pane_app.clone(),
            query: p.query.clone(),
            rows: p.rows().into_iter().map(|r| (r.label, r.group)).collect(),
            selected: p.selected,
            placeholder: p.placeholder(),
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
                svg: self.viewer_svg,
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
        self.show_palette(Palette::new(rows));
    }

    fn show_palette(&mut self, palette: Palette) {
        self.overlays.menu = None;
        self.overlays.session_menu = false;
        self.overlays.palette = Some(palette);
        self.ui_dirty = true;
    }

    /// "Switch Session…": the palette lists the window key's sessions, the shown one marked,
    /// then New Session, Rename Session… and Lock (or Unlock) Session.
    pub(super) fn open_session_switcher(&mut self) {
        let sessions = self.sessions();
        let mut rows: Vec<Row> = sessions
            .sessions
            .iter()
            .map(|s| {
                let tabs = match s.tabs.len() {
                    1 => "1 tab".to_owned(),
                    n => format!("{n} tabs"),
                };
                let current = Some(s.id) == self.active_session;
                Row {
                    label: s.name.clone(),
                    group: if current {
                        format!("{tabs} \u{b7} current")
                    } else {
                        tabs
                    },
                    icon: if s.locked { "lock" } else { "" }.to_owned(),
                    action: RowAction::Session(s.id),
                }
            })
            .collect();
        let locked = self.session(&sessions).is_some_and(|s| s.locked);
        for (cmd, label) in [
            (crate::commands::Cmd::NewSession, "New Session"),
            (crate::commands::Cmd::RenameSession, "Rename Session"),
            (
                crate::commands::Cmd::LockSession,
                if locked {
                    "Unlock Session"
                } else {
                    "Lock Session"
                },
            ),
        ] {
            rows.push(Row {
                label: label.to_owned(),
                group: "Session".to_owned(),
                icon: String::new(),
                action: RowAction::Builtin(cmd),
            });
        }
        let mut palette = Palette::new(rows);
        palette.scope = Some("Switch to a session".to_owned());
        self.show_palette(palette);
    }

    /// "Rename Session…": the palette asks for the shown session's new name.
    pub(super) fn open_session_rename(&mut self) {
        let sessions = self.sessions();
        if let Some(s) = self.session(&sessions) {
            self.show_palette(Palette::rename_session(s.id, &s.name));
        }
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
            RowAction::Session(id) => self.select_session(id),
            RowAction::RenameSession(session, name) => {
                self.edit(auk_mirror::LayoutEdit::RenameSession { session, name });
            }
            RowAction::Block(kind) => {
                let cwd = self.focused_cwd();
                let _ = self.open_block(
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
        } else if self.overlays.session_menu {
            if key.event.key == Key::Escape {
                self.overlays.session_menu = false;
            }
            true
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
                self.overlays.session_menu = false;
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
            ChromeAction::SessionMenu
            | ChromeAction::Session(_)
            | ChromeAction::Tab(_)
            | ChromeAction::NewSession
            | ChromeAction::NewTab
            | ChromeAction::RenameSession
            | ChromeAction::LockSession => {}
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
        self.overlays.viewer = Some(Viewer {
            pane,
            zoom,
            decoding: false,
        });
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

    /// Shows the image from the pane's blobs in the chrome: decoded now when small, else in
    /// the background, the viewer empty until it's ready ([`WinState::poll_viewer`]).
    fn load_viewer_image(&mut self) {
        use auk_views::image::{Poll, request_blob};
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
                .map(|b| request_blob(&b, scale))
        }
        // A picture the terminal made (a mermaid diagram's Open), not a blob.
        .or_else(|| auk_views::image::load_made_for_viewer(&blob, scale).map(Poll::Ready));
        let decoding = matches!(picture, Some(Poll::Pending));
        match picture {
            Some(Poll::Ready(Ok(p))) => {
                self.viewer_size = (f64::from(p.size.0), f64::from(p.size.1));
                self.viewer_svg = p.svg;
                // An SVG scales up to fit: drawn again at that scale, so it stays sharp.
                let window = self.size.map_or((0.0, 0.0), |s| (s.width, s.height));
                let k = crate::chrome::viewer_fit(window, self.viewer_size, p.svg);
                let p = if p.svg && k > 1.0 {
                    let shared = self.conn.shared();
                    shared
                        .panes
                        .get(&v.pane)
                        .and_then(|pd| pd.replica.tsp().blobs().get(&blob).cloned())
                        .and_then(|b| auk_views::image::load_blob(&b, scale * k as f32).ok())
                        .or_else(|| {
                            auk_views::image::load_made_for_viewer(&blob, scale * k as f32)
                                .and_then(Result::ok)
                        })
                        .unwrap_or(p)
                } else {
                    p
                };
                self.chrome
                    .set_viewer_image(p.frames.first().map(|f| f.content.clone()));
            }
            Some(Poll::Ready(Err(e))) => {
                self.toast(ToastLevel::Error, "Can't show the image", Some(&e));
                self.close_viewer();
            }
            None | Some(Poll::Pending) => {
                self.viewer_size = (1.0, 1.0);
                self.viewer_svg = false;
                self.chrome.set_viewer_image(None);
            }
        }
        if let Some(v) = self.overlays.viewer.as_mut() {
            v.decoding = decoding;
        }
        self.ui_dirty = true;
    }

    /// Before a frame: shows the viewer's picture once its background decode is done. Returns
    /// whether it is still decoding (the window asks for another frame).
    pub(super) fn poll_viewer(&mut self) -> bool {
        if !self.overlays.viewer.as_ref().is_some_and(|v| v.decoding) {
            return false;
        }
        let blob = self.overlays.viewer.as_ref().map(|v| v.zoom.images[v.zoom.index].blob.clone());
        if blob.is_some_and(|b| auk_views::image::decoding_blob(&b.to_ascii_lowercase())) {
            return true;
        }
        self.load_viewer_image();
        self.overlays.viewer.as_ref().is_some_and(|v| v.decoding)
    }

    /// Sends a surface pointer event's outcome that opens a path or link through routes.
    pub(super) fn open_from_surface(&mut self, pane: PaneId, target: &str) {
        self.open_routed(target, "link", Some(pane));
    }
}
