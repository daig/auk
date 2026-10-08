//! The window chrome (D3): the sessions and tab bar, pane dividers and the focus ring, drawn by
//! our UI engine as one document per window. The panes' areas stay transparent so the grids
//! show through.

use auk_ui::{Document, NodeId, SheetKind};

use crate::geometry::Rect;

/// The tab bar's height, in points.
pub const BAR_HEIGHT: f64 = 30.0;

/// Width of the line between split panes, in points.
pub const DIVIDER: f64 = 1.0;

/// The status line's height, in points (§9.8 "Chrome formatters").
pub const STATUS_HEIGHT: f64 = 22.0;

const CSS: &str = r#"
html { font-family: system-ui, sans-serif; font-size: 12px; color: #d4d7db; }
body { margin: 0; }
.bar {
  position: absolute; left: 0; top: 0; right: 0; height: 30px; box-sizing: border-box;
  display: flex; align-items: center; gap: 8px; padding: 0 10px;
  background-color: #1b1d20; border-bottom: 1px solid #2c2f33;
}
.sessions, .tabs { display: flex; align-items: center; gap: 3px; }
.tabs { flex: 1 1 auto; min-width: 0; overflow: hidden; }
.session, .tab, .add {
  padding: 3px 9px; border-radius: 5px; white-space: nowrap; color: #8d939a;
}
.tab { max-width: 220px; overflow: hidden; text-overflow: ellipsis; }
.session.active { background-color: #34383d; color: #eceef0; }
.tab.active { background-color: #2a2d31; color: #eceef0; }
.add { padding: 3px 7px; }
.zoomed { color: #e2b45d; }
.sep { width: 1px; height: 16px; background-color: #34373b; }
.divider { position: absolute; background-color: #2c2f33; }
.focus { position: absolute; box-sizing: border-box; border: 1px solid #4f7fd8; }
html.light { color: #2a2d31; }
html.light .bar { background-color: #ececea; border-bottom-color: #d5d5d2; }
html.light .session, html.light .tab, html.light .add { color: #6d7177; }
html.light .session.active { background-color: #d7d8d6; color: #1b1d20; }
html.light .tab.active { background-color: #fbfbfa; color: #1b1d20; }
html.light .sep, html.light .divider { background-color: #d5d5d2; }
#statusline {
  position: absolute; left: 0; right: 0; height: 22px; box-sizing: border-box;
  display: flex; align-items: center; gap: 10px; padding: 0 10px; font-size: 11px;
  background-color: #1b1d20; border-top: 1px solid #2c2f33; color: #8d939a;
}
#statusline .sl-left { flex: 1 1 auto; min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
#statusline .sl-right { display: flex; gap: 10px; }
#statusline .sl-plugin { white-space: nowrap; color: #c3c7cc; }
#statusline .sl-plugin[data-tone="muted"] { color: #8d939a; }
#statusline .sl-plugin[data-tone="error"] { color: #e5534b; }
#statusline .sl-plugin[data-tone="success"] { color: #57ab5a; }
html.light #statusline { background-color: #ececea; border-top-color: #d5d5d2; color: #6d7177; }
html.light #statusline .sl-plugin { color: #2a2d31; }
.toasts { position: absolute; right: 12px; top: 40px; width: 300px; display: flex; flex-direction: column; gap: 6px; }
.toast {
  padding: 8px 10px; border-radius: 7px; background-color: #2a2d31; border: 1px solid #3a3e44;
  color: #eceef0; box-sizing: border-box;
}
.toast.toast-error { border-color: #a1403a; }
.toast.toast-success { border-color: #3f7e44; }
.toast .t-sub { color: #9da3a9; font-size: 11px; margin-top: 2px; }
html.light .toast { background-color: #fbfbfa; border-color: #d5d5d2; color: #1b1d20; }
.backdrop { position: absolute; left: 0; top: 0; }
.backdrop.dim { background-color: rgba(0, 0, 0, 0.6); }
.palette {
  position: absolute; left: 50%; top: 60px; width: 520px; margin-left: -260px;
  background-color: #23262a; border: 1px solid #3a3e44; border-radius: 9px; padding: 6px;
  box-sizing: border-box;
}
.palette .p-query { padding: 6px 8px; color: #eceef0; border-bottom: 1px solid #34373b; margin-bottom: 4px; min-height: 16px; }
.palette .p-query.empty { color: #6d7177; }
.palette .p-row { display: flex; padding: 5px 8px; border-radius: 5px; color: #c3c7cc; }
.palette .p-row.selected { background-color: #34507e; color: #ffffff; }
.palette .p-label { flex: 1 1 auto; }
.palette .p-group { color: #8d939a; font-size: 11px; }
html.light .palette { background-color: #fbfbfa; border-color: #d5d5d2; }
html.light .palette .p-query { color: #1b1d20; border-bottom-color: #e2e2df; }
html.light .palette .p-row { color: #2a2d31; }
html.light .palette .p-row.selected { background-color: #cddcf5; color: #1b1d20; }
.ctxmenu {
  position: absolute; min-width: 160px; padding: 4px; box-sizing: border-box;
  background-color: #23262a; border: 1px solid #3a3e44; border-radius: 7px;
}
.ctxmenu .ctx-item { padding: 4px 10px; border-radius: 4px; color: #d4d7db; white-space: nowrap; }
.ctxmenu .ctx-item.selected { background-color: #34507e; color: #ffffff; }
html.light .ctxmenu { background-color: #fbfbfa; border-color: #d5d5d2; }
html.light .ctxmenu .ctx-item { color: #1b1d20; }
.viewer { position: absolute; left: 40px; top: 50px; display: flex; flex-direction: column; align-items: center; }
.viewer .v-bar { display: flex; gap: 12px; align-items: center; color: #eceef0; padding: 6px 0; }
.viewer .v-btn { padding: 2px 9px; border-radius: 5px; background-color: #34383d; color: #eceef0; }
.viewer .v-title { color: #eceef0; }
"#;

/// What the chrome shows. The document is rebuilt when this changes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChromeModel {
    pub width: f64,
    pub height: f64,
    pub dark: bool,
    /// (session id, name, active).
    pub sessions: Vec<(u64, String, bool)>,
    /// (tab id, title, active, zoomed).
    pub tabs: Vec<(u64, String, bool, bool)>,
    pub dividers: Vec<Rect>,
    /// The focused pane's rectangle, ringed when the tab shows more than one pane.
    pub focus: Option<Rect>,
    /// The status line's left side: the focused pane's title.
    pub status_title: String,
    /// Plugin status segments, in plugin id order.
    pub status: Vec<StatusItem>,
    pub toasts: Vec<ToastItem>,
    pub palette: Option<PaletteView>,
    pub menu: Option<MenuView>,
    pub viewer: Option<ViewerView>,
}

/// A status line segment (`tern.chrome.status`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatusItem {
    pub plugin: String,
    pub text: String,
    pub icon: Option<String>,
    pub tone: Option<String>,
    /// The action a click runs.
    pub command: Option<String>,
}

/// A toast on screen.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ToastItem {
    /// `info`, `success` or `error`.
    pub level: String,
    pub text: String,
    pub sub: Option<String>,
}

/// The palette as drawn: the query and the shown rows `(label, group)`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PaletteView {
    pub query: String,
    pub rows: Vec<(String, String)>,
    pub selected: usize,
}

/// A context menu (§7.8 `menu` actions) at a point, in points.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MenuView {
    pub x: f64,
    pub y: f64,
    pub entries: Vec<String>,
    pub selected: Option<usize>,
}

/// The image viewer (the `zoom` action): the shown image's title and place in its set.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ViewerView {
    pub title: String,
    pub index: usize,
    pub count: usize,
    /// The shown blob, whose picture [`Chrome::set_viewer_image`] supplies.
    pub blob: String,
    /// The picture's size in CSS px.
    pub size: (f64, f64),
    pub open: Option<String>,
}

/// Something the chrome lets the user do with a click.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChromeAction {
    Session(u64),
    Tab(u64),
    NewSession,
    NewTab,
    /// Run an action by name (a status segment's `command`).
    Run(String),
    /// A palette row, by its index among the shown rows.
    Palette(usize),
    /// A context menu entry.
    Menu(usize),
    /// A toast, to dismiss it.
    Toast(usize),
    /// A click outside an overlay: close it.
    Dismiss,
    ViewerPrev,
    ViewerNext,
    ViewerOpen,
}

/// The chrome document.
pub struct Chrome {
    pub doc: Document,
    model: Option<ChromeModel>,
    body: NodeId,
    /// Installed plugin sheets `(name, css)`, in order (§9.8 "CSS").
    sheets: Vec<(String, String)>,
    viewer_image: Option<auk_ui::ImageContent>,
}

impl std::fmt::Debug for Chrome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Chrome")
            .field("model", &self.model)
            .finish()
    }
}

impl Chrome {
    pub fn new(scale: f32) -> Chrome {
        let mut doc = Document::new();
        let errors = doc.set_stylesheet("chrome", SheetKind::BuiltIn, None, CSS);
        debug_assert!(errors.is_empty(), "{errors:?}");
        doc.set_device_scale(scale);
        let root = doc.root();
        let body = doc.create_element("body");
        doc.append_child(root, body);
        Chrome {
            doc,
            model: None,
            body,
            sheets: Vec::new(),
            viewer_image: None,
        }
    }

    /// Installs plugin sheets (`plugin:local:<id>:…`) over the chrome's own, replacing the last
    /// set. Returns whether anything changed.
    pub fn set_plugin_sheets(&mut self, sheets: &[(String, String)]) -> bool {
        if self.sheets == sheets {
            return false;
        }
        for (name, _) in &self.sheets {
            self.doc.remove_stylesheet(name, None);
        }
        for (name, css) in sheets {
            let errors = self.doc.set_stylesheet(name, SheetKind::Plugin, None, css);
            if !errors.is_empty() {
                report_sheet_errors(name, &errors);
            }
        }
        self.sheets = sheets.to_vec();
        self.doc.update();
        true
    }

    /// The installed plugin sheet names, in order.
    pub fn plugin_sheets(&self) -> Vec<String> {
        self.sheets.iter().map(|(n, _)| n.clone()).collect()
    }

    /// The picture the viewer shows; the next [`update`](Self::update) puts it in place.
    pub fn set_viewer_image(&mut self, image: Option<auk_ui::ImageContent>) {
        self.viewer_image = image;
        self.model = None;
    }

    /// Rebuilds the document if `model` differs from what it shows. Returns whether it did.
    pub fn update(&mut self, model: &ChromeModel, scale: f32) -> bool {
        self.doc.set_device_scale(scale);
        if self.model.as_ref() == Some(model) {
            return false;
        }
        self.doc
            .set_viewport(model.width as f32, model.height as f32);
        let root = self.doc.root();
        self.doc
            .set_attribute(root, "class", if model.dark { "dark" } else { "light" });
        for child in self.doc.children(self.body).to_vec() {
            self.doc.remove(child);
        }
        let bar = self.child(self.body, "div", "bar", None);
        self.doc.set_attribute(bar, "role", "toolbar");
        let sessions = self.child(bar, "div", "sessions", None);
        self.doc.set_attribute(sessions, "role", "tablist");
        self.doc.set_attribute(sessions, "aria-label", "Sessions");
        for (id, name, active) in &model.sessions {
            let class = if *active { "session active" } else { "session" };
            let s = self.child(sessions, "div", class, Some(name));
            self.doc
                .set_attribute(s, "data-action", &format!("session:{id}"));
            self.doc.set_attribute(s, "role", "tab");
            if *active {
                self.doc.set_attribute(s, "aria-selected", "true");
            }
        }
        let add = self.child(sessions, "div", "add", Some("+"));
        self.doc.set_attribute(add, "data-action", "new-session");
        self.doc.set_attribute(add, "role", "button");
        self.doc.set_attribute(add, "aria-label", "New Session");
        self.child(bar, "div", "sep", None);
        let tabs = self.child(bar, "div", "tabs", None);
        self.doc.set_attribute(tabs, "role", "tablist");
        self.doc.set_attribute(tabs, "aria-label", "Tabs");
        for (id, title, active, zoomed) in &model.tabs {
            let class = match (*active, *zoomed) {
                (true, true) => "tab active zoomed",
                (true, false) => "tab active",
                (false, true) => "tab zoomed",
                (false, false) => "tab",
            };
            let t = self.child(tabs, "div", class, Some(title));
            self.doc
                .set_attribute(t, "data-action", &format!("tab:{id}"));
            self.doc.set_attribute(t, "role", "tab");
            if *active {
                self.doc.set_attribute(t, "aria-selected", "true");
            }
        }
        let add = self.child(tabs, "div", "add", Some("+"));
        self.doc.set_attribute(add, "data-action", "new-tab");
        self.doc.set_attribute(add, "role", "button");
        self.doc.set_attribute(add, "aria-label", "New Tab");
        for d in &model.dividers {
            let div = self.child(self.body, "div", "divider", None);
            self.place(div, d);
        }
        if let Some(f) = &model.focus
            && !model.dividers.is_empty()
        {
            let ring = self.child(self.body, "div", "focus", None);
            self.place(ring, f);
        }
        self.overlays(model);
        self.doc.update();
        self.model = Some(model.clone());
        true
    }

    /// The status line, toasts, palette, context menu and image viewer.
    fn overlays(&mut self, model: &ChromeModel) {
        // Boxes that span the window are placed in px: the document's own height is its content.
        let (w, h) = (model.width, model.height);
        let line = self.child(self.body, "div", "statusline", None);
        self.doc.set_attribute(line, "id", "statusline");
        self.doc
            .set_inline_style(line, &format!("top: {}px", h - STATUS_HEIGHT));
        self.doc.set_attribute(line, "role", "status");
        self.child(line, "div", "sl-left", Some(&model.status_title));
        let right = self.child(line, "div", "sl-right", None);
        for seg in &model.status {
            let s = self.child(right, "div", "sl-plugin", Some(&seg.text));
            self.doc.set_attribute(s, "data-plugin", &seg.plugin);
            if let Some(tone) = &seg.tone {
                self.doc.set_attribute(s, "data-tone", tone);
            }
            if let Some(icon) = &seg.icon {
                self.doc.set_attribute(s, "data-icon", icon);
            }
            if let Some(cmd) = &seg.command {
                self.doc
                    .set_attribute(s, "data-action", &format!("run:{cmd}"));
                self.doc.set_attribute(s, "role", "button");
            }
        }
        if !model.toasts.is_empty() {
            let stack = self.child(self.body, "div", "toasts", None);
            for (i, t) in model.toasts.iter().enumerate() {
                let el = self.child(stack, "div", &format!("toast toast-{}", t.level), None);
                self.doc.set_attribute(el, "role", "alert");
                self.doc
                    .set_attribute(el, "data-action", &format!("toast:{i}"));
                self.child(el, "div", "t-text", Some(&t.text));
                if let Some(sub) = &t.sub {
                    self.child(el, "div", "t-sub", Some(sub));
                }
            }
        }
        if let Some(p) = &model.palette {
            let back = self.child(self.body, "div", "backdrop", None);
            self.doc.set_attribute(back, "data-action", "dismiss");
            self.doc
                .set_inline_style(back, &format!("width: {w}px; height: {h}px"));
            let box_ = self.child(self.body, "div", "palette", None);
            self.doc.set_attribute(box_, "role", "dialog");
            self.doc
                .set_attribute(box_, "aria-label", "Command Palette");
            let (class, text) = if p.query.is_empty() {
                ("p-query empty", "Type a command, or a path to open")
            } else {
                ("p-query", p.query.as_str())
            };
            let q = self.child(box_, "div", class, Some(text));
            self.doc.set_attribute(q, "role", "textbox");
            let rows = self.child(box_, "div", "p-rows", None);
            self.doc.set_attribute(rows, "role", "listbox");
            for (i, (label, group)) in p.rows.iter().enumerate().take(14) {
                let class = if i == p.selected {
                    "p-row selected"
                } else {
                    "p-row"
                };
                let r = self.child(rows, "div", class, None);
                self.doc.set_attribute(r, "role", "option");
                self.doc
                    .set_attribute(r, "data-action", &format!("palette:{i}"));
                self.child(r, "span", "p-label", Some(label));
                self.child(r, "span", "p-group", Some(group));
            }
        }
        if let Some(m) = &model.menu {
            let back = self.child(self.body, "div", "backdrop", None);
            self.doc.set_attribute(back, "data-action", "dismiss");
            self.doc
                .set_inline_style(back, &format!("width: {w}px; height: {h}px"));
            let menu = self.child(self.body, "div", "ctxmenu", None);
            self.doc.set_attribute(menu, "role", "menu");
            self.doc
                .set_inline_style(menu, &format!("left: {}px; top: {}px", m.x, m.y));
            for (i, label) in m.entries.iter().enumerate() {
                let class = if m.selected == Some(i) {
                    "ctx-item selected"
                } else {
                    "ctx-item"
                };
                let item = self.child(menu, "div", class, Some(label));
                self.doc.set_attribute(item, "role", "menuitem");
                self.doc
                    .set_attribute(item, "data-action", &format!("menu:{i}"));
            }
        }
        if let Some(v) = &model.viewer {
            let back = self.child(self.body, "div", "backdrop dim", None);
            self.doc.set_attribute(back, "data-action", "dismiss");
            self.doc
                .set_inline_style(back, &format!("width: {w}px; height: {h}px"));
            let viewer = self.child(self.body, "div", "viewer", None);
            self.doc.set_inline_style(
                viewer,
                &format!(
                    "width: {}px; height: {}px",
                    (w - 80.0).max(1.0),
                    (h - 90.0).max(1.0)
                ),
            );
            self.doc.set_attribute(viewer, "role", "dialog");
            self.doc.set_attribute(viewer, "aria-label", "Image Viewer");
            let bar = self.child(viewer, "div", "v-bar", None);
            if v.count > 1 {
                let prev = self.child(bar, "div", "v-btn", Some("‹"));
                self.doc.set_attribute(prev, "data-action", "viewer:prev");
                self.doc.set_attribute(prev, "role", "button");
            }
            let title = if v.count > 1 {
                format!("{} ({}/{})", v.title, v.index + 1, v.count)
            } else {
                v.title.clone()
            };
            self.child(bar, "div", "v-title", Some(&title));
            if v.count > 1 {
                let next = self.child(bar, "div", "v-btn", Some("›"));
                self.doc.set_attribute(next, "data-action", "viewer:next");
                self.doc.set_attribute(next, "role", "button");
            }
            if v.open.is_some() {
                let open = self.child(bar, "div", "v-btn", Some("Open"));
                self.doc.set_attribute(open, "data-action", "viewer:open");
                self.doc.set_attribute(open, "role", "button");
            }
            let img = self.child(viewer, "div", "v-img", None);
            self.doc.set_attribute(img, "role", "img");
            self.doc.set_attribute(img, "aria-label", &v.title);
            // Fit the picture in the viewer, never past its own size.
            let (aw, ah) = ((w - 80.0).max(1.0), (h - 130.0).max(1.0));
            let (w, h) = (v.size.0.max(1.0), v.size.1.max(1.0));
            let k = (aw / w).min(ah / h).min(1.0);
            self.doc.set_inline_style(
                img,
                &format!(
                    "width: {}px; height: {}px",
                    (w * k).floor(),
                    (h * k).floor()
                ),
            );
            self.doc.set_image(img, self.viewer_image.clone());
        }
    }

    /// Every element with an action and its box, in points (for the automation endpoint).
    pub fn actions(&self) -> Vec<(String, auk_ui::Rect)> {
        let mut out = Vec::new();
        let mut stack = vec![self.doc.root()];
        while let Some(n) = stack.pop() {
            if let Some(a) = self.doc.attribute(n, "data-action")
                && let Some(r) = self.doc.border_box(n)
            {
                out.push((a.to_owned(), r));
            }
            for &c in self.doc.children(n).iter().rev() {
                stack.push(c);
            }
        }
        out
    }

    fn child(&mut self, parent: NodeId, tag: &str, class: &str, text: Option<&str>) -> NodeId {
        let e = self.doc.create_element(tag);
        self.doc.set_attribute(e, "class", class);
        if let Some(text) = text {
            let t = self.doc.create_text(text);
            self.doc.append_child(e, t);
        }
        self.doc.append_child(parent, e);
        e
    }

    fn place(&mut self, id: NodeId, r: &Rect) {
        self.doc.set_inline_style(
            id,
            &format!(
                "left: {}px; top: {}px; width: {}px; height: {}px",
                r.x, r.y, r.w, r.h
            ),
        );
    }

    /// The action under a point (in points), if any.
    pub fn action_at(&self, x: f64, y: f64) -> Option<ChromeAction> {
        let mut node = auk_ui::hit_test(&self.doc, x as f32, y as f32);
        while let Some(id) = node {
            if let Some(action) = self.doc.attribute(id, "data-action") {
                return parse_action(action);
            }
            node = self.doc.parent(id);
        }
        None
    }
}

fn report_sheet_errors(name: &str, errors: &[impl std::fmt::Debug]) {
    eprintln!("auk: plugin sheet {name}: {errors:?}");
}

fn parse_action(s: &str) -> Option<ChromeAction> {
    match s {
        "new-session" => Some(ChromeAction::NewSession),
        "new-tab" => Some(ChromeAction::NewTab),
        "dismiss" => Some(ChromeAction::Dismiss),
        "viewer:prev" => Some(ChromeAction::ViewerPrev),
        "viewer:next" => Some(ChromeAction::ViewerNext),
        "viewer:open" => Some(ChromeAction::ViewerOpen),
        _ if s.starts_with("run:") => Some(ChromeAction::Run(s[4..].to_owned())),
        _ => {
            let (kind, id) = s.split_once(':')?;
            let id = id.parse().ok()?;
            match kind {
                "session" => Some(ChromeAction::Session(id)),
                "tab" => Some(ChromeAction::Tab(id)),
                "palette" => Some(ChromeAction::Palette(id as usize)),
                "menu" => Some(ChromeAction::Menu(id as usize)),
                "toast" => Some(ChromeAction::Toast(id as usize)),
                _ => None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> ChromeModel {
        ChromeModel {
            width: 800.0,
            height: 500.0,
            dark: true,
            sessions: vec![(1, "1".into(), true), (5, "work".into(), false)],
            tabs: vec![
                (2, "~/src".into(), true, false),
                (3, "vim".into(), false, true),
            ],
            dividers: vec![Rect::new(400.0, 30.0, 1.0, 470.0)],
            focus: Some(Rect::new(0.0, 30.0, 400.0, 470.0)),
            ..ChromeModel::default()
        }
    }

    #[test]
    fn clicks_find_their_actions() {
        let mut c = Chrome::new(2.0);
        assert!(c.update(&model(), 2.0));
        assert!(!c.update(&model(), 2.0));
        // Find each tab's box and click its middle.
        let mut found = Vec::new();
        for x in (0..800).step_by(4) {
            if let Some(a) = c.action_at(f64::from(x), 15.0)
                && found.last() != Some(&a)
            {
                found.push(a);
            }
        }
        assert_eq!(
            found,
            vec![
                ChromeAction::Session(1),
                ChromeAction::Session(5),
                ChromeAction::NewSession,
                ChromeAction::Tab(2),
                ChromeAction::Tab(3),
                ChromeAction::NewTab,
            ]
        );
        assert_eq!(c.action_at(200.0, 200.0), None);
    }
}
