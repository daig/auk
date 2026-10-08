//! The window chrome (D3): the sessions and tab bar, pane dividers and the focus ring, drawn by
//! our UI engine as one document per window. The panes' areas stay transparent so the grids
//! show through.

use auk_ui::{Document, NodeId, SheetKind};

use crate::geometry::Rect;

// The portable widgets (tab bar, dividers and focus ring, status line, palette) live in auk-views
// so the browser client draws the same ones (§22, D3); this module adds the app's own overlays.
pub use auk_views::chrome::{BAR_HEIGHT, DIVIDER, PaletteView, STATUS_HEIGHT, StatusItem};
use auk_views::chrome as widgets;

const CSS: &str = r#"
html { font-family: system-ui, sans-serif; font-size: 12px; color: #d4d7db; }
body { margin: 0; }
.toasts { position: absolute; right: 12px; top: 40px; width: 300px; display: flex; flex-direction: column; gap: 6px; }
.toast {
  padding: 8px 10px; border-radius: 7px; background-color: #2a2d31; border: 1px solid #3a3e44;
  color: #eceef0; box-sizing: border-box;
}
.toast.toast-error { border-color: #a1403a; }
.toast.toast-success { border-color: #3f7e44; }
.toast .t-sub { color: #9da3a9; font-size: 11px; margin-top: 2px; }
html.light .toast { background-color: #fbfbfa; border-color: #d5d5d2; color: #1b1d20; }
.card {
  position: absolute; left: 50%; top: 80px; width: 360px; margin-left: -180px; padding: 12px 14px;
  border-radius: 9px; background-color: #2a2d31; border: 1px solid #a1403a; color: #eceef0;
  box-sizing: border-box; display: flex; flex-direction: column; gap: 6px;
}
.card .c-title { font-weight: 600; }
.card .c-sub { color: #9da3a9; font-size: 11px; }
.card .c-buttons { display: flex; gap: 8px; justify-content: flex-end; margin-top: 4px; }
.card .c-button { padding: 3px 10px; border-radius: 5px; border: 1px solid #3a3e44; }
.card .c-button.primary { background-color: #3a3e44; }
html.light .card { background-color: #fbfbfa; color: #1b1d20; }
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
.linkhint {
  position: absolute; left: 8px; max-width: 70%; padding: 2px 8px; box-sizing: border-box;
  border-radius: 5px; font-size: 11px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
  background-color: #23262a; border: 1px solid #3a3e44; color: #c3c7cc;
}
.findbar {
  position: absolute; right: 14px; top: 38px; width: 300px; display: flex; gap: 8px; align-items: center;
  padding: 5px 8px; box-sizing: border-box; border-radius: 7px;
  background-color: #23262a; border: 1px solid #3a3e44;
}
.findbar .f-query { flex: 1 1 auto; min-width: 0; color: #eceef0; white-space: nowrap; overflow: hidden; }
.findbar .f-query.empty { color: #6d7177; }
.findbar .f-count { color: #8d939a; font-size: 11px; white-space: nowrap; }
.tooltip {
  position: absolute; max-width: 360px; padding: 3px 8px; box-sizing: border-box;
  border-radius: 5px; font-size: 11px; white-space: pre-wrap;
  background-color: #23262a; border: 1px solid #3a3e44; color: #d4d7db;
}
html.light .tooltip { background-color: #fbfbfa; border-color: #d5d5d2; color: #2a2d31; }
html.light .linkhint, html.light .findbar { background-color: #fbfbfa; border-color: #d5d5d2; color: #2a2d31; }
html.light .findbar .f-query { color: #1b1d20; }
"#;

/// What the chrome shows. The document is rebuilt when this changes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChromeModel {
    pub width: f64,
    pub height: f64,
    pub dark: bool,
    /// The media features plugin sheets match (§8.4): reduced motion, reduced transparency,
    /// more contrast.
    pub media: (bool, bool, bool),
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
    /// The program a tab's focused pane reports (`app` of its hello), by tab id: only while a
    /// listening surface is open (§7.2, §12.2).
    pub tab_apps: std::collections::BTreeMap<u64, String>,
    /// Tabs whose focused pane is an agent pane, shown with the agent icon (§5.1).
    pub tab_agents: std::collections::BTreeSet<u64>,
    /// The focused pane's reporting program, in the status line.
    pub pane_app: Option<String>,
    /// The target of the link under the pointer (§7.14).
    pub hover_link: Option<String>,
    /// The `title` tooltip of the surface node under the pointer, shown below the pointer at
    /// that window point (§8.1).
    pub tooltip: Option<(String, f64, f64)>,
    pub find: Option<FindView>,
    /// A problem card with a retry ("Could not start the block", §9.5).
    pub card: Option<CardView>,
}

/// A card over the panes: a title, what went wrong, and Retry (runs `retry`) or Dismiss.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CardView {
    pub title: String,
    pub sub: String,
    /// The action Retry runs.
    pub retry: String,
}

/// The find bar: the query and "current of count".
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FindView {
    pub query: String,
    pub current: Option<usize>,
    pub count: usize,
}


/// A toast on screen.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ToastItem {
    /// `info`, `success` or `error`.
    pub level: String,
    pub text: String,
    pub sub: Option<String>,
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
    /// The `title` tooltip element, while one is drawn.
    tooltip_el: Option<NodeId>,
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
        let errors = doc.set_stylesheet("chrome-widgets", SheetKind::BuiltIn, None, widgets::CSS);
        debug_assert!(errors.is_empty(), "{errors:?}");
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
            tooltip_el: None,
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

    /// The tabs drawn with the agent icon, by tab id, with the icon's box.
    pub fn agent_badges(&self) -> Vec<(u64, auk_ui::Rect)> {
        widgets::agent_badges(&self.doc)
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
        if self.model.as_ref().is_none_or(|m| m.media != model.media) {
            let (motion, transparency, contrast) = model.media;
            self.doc.set_media_preferences(motion, transparency, contrast);
        }
        let root = self.doc.root();
        self.doc
            .set_attribute(root, "class", if model.dark { "dark" } else { "light" });
        for child in self.doc.children(self.body).to_vec() {
            self.doc.remove(child);
        }
        let sessions: Vec<widgets::SessionItem> = model
            .sessions
            .iter()
            .map(|(id, name, active)| widgets::SessionItem {
                id: *id,
                name: name.clone(),
                active: *active,
            })
            .collect();
        let tabs: Vec<widgets::TabItem> = model
            .tabs
            .iter()
            .map(|(id, title, active, zoomed)| widgets::TabItem {
                id: *id,
                title: title.clone(),
                active: *active,
                zoomed: *zoomed,
                app: model.tab_apps.get(id).cloned(),
                agent: model.tab_agents.contains(id),
            })
            .collect();
        widgets::tab_bar(&mut self.doc, self.body, &sessions, &tabs);
        let rect = |r: &Rect| auk_ui::Rect::new(r.x as f32, r.y as f32, r.w as f32, r.h as f32);
        let lines: Vec<auk_ui::Rect> = model.dividers.iter().map(rect).collect();
        widgets::dividers(&mut self.doc, self.body, &lines, model.focus.as_ref().map(rect));
        self.overlays(model);
        self.doc.update();
        self.model = Some(model.clone());
        true
    }

    /// The status line, toasts, palette, context menu and image viewer.
    fn overlays(&mut self, model: &ChromeModel) {
        // Boxes that span the window are placed in px: the document's own height is its content.
        let (w, h) = (model.width, model.height);
        widgets::status_line(
            &mut self.doc,
            self.body,
            h - STATUS_HEIGHT,
            model.pane_app.as_deref(),
            &model.status_title,
            &model.status,
        );
        if let Some(link) = &model.hover_link {
            let hint = self.child(self.body, "div", "linkhint", Some(link));
            self.doc.set_attribute(hint, "role", "tooltip");
            self.doc
                .set_inline_style(hint, &format!("top: {}px", h - STATUS_HEIGHT - 26.0));
        }
        self.tooltip_el = None;
        if let Some((title, x, y)) = &model.tooltip {
            let tip = self.child(self.body, "div", "tooltip", Some(title));
            self.tooltip_el = Some(tip);
            self.doc.set_attribute(tip, "role", "tooltip");
            // Below and right of the pointer, kept inside the window.
            let left = (x + 4.0).min(w - 120.0).max(4.0);
            let top = (y + 18.0).min(h - STATUS_HEIGHT - 24.0).max(4.0);
            self.doc
                .set_inline_style(tip, &format!("left: {left}px; top: {top}px"));
        }
        if let Some(f) = &model.find {
            let bar = self.child(self.body, "div", "findbar", None);
            self.doc.set_attribute(bar, "role", "search");
            self.doc.set_attribute(bar, "aria-label", "Find");
            let (class, text) = if f.query.is_empty() {
                ("f-query empty", "Find")
            } else {
                ("f-query", f.query.as_str())
            };
            let q = self.child(bar, "div", class, Some(text));
            self.doc.set_attribute(q, "role", "textbox");
            let count = match (f.count, f.current) {
                (0, _) if f.query.is_empty() => String::new(),
                (0, _) => "No matches".to_owned(),
                (n, Some(i)) => format!("{} of {n}", i + 1),
                (n, None) => format!("{n}"),
            };
            self.child(bar, "div", "f-count", Some(&count));
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
        if let Some(c) = &model.card {
            let card = self.child(self.body, "div", "card", None);
            self.doc.set_attribute(card, "role", "alertdialog");
            self.child(card, "div", "c-title", Some(&c.title));
            self.child(card, "div", "c-sub", Some(&c.sub));
            let row = self.child(card, "div", "c-buttons", None);
            let retry = self.child(row, "div", "c-button primary", Some("Retry"));
            self.doc
                .set_attribute(retry, "data-action", &format!("run:{}", c.retry));
            let dismiss = self.child(row, "div", "c-button", Some("Dismiss"));
            self.doc.set_attribute(dismiss, "data-action", "run:card:dismiss");
        }
        if let Some(p) = &model.palette {
            widgets::palette(&mut self.doc, self.body, w, h, p);
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

    /// The status line's segments as drawn: `(data-plugin, text, data-tone)`, in order.
    pub fn status_segments(&self) -> Vec<(String, String, Option<String>)> {
        let mut out = Vec::new();
        let mut stack = vec![self.doc.root()];
        while let Some(n) = stack.pop() {
            if let Some(plugin) = self.doc.attribute(n, "data-plugin") {
                let text: String = self
                    .doc
                    .children(n)
                    .iter()
                    .filter_map(|&c| self.doc.node(c).text())
                    .collect();
                let tone = self.doc.attribute(n, "data-tone").map(str::to_owned);
                out.push((plugin.to_owned(), text, tone));
            }
            for &c in self.doc.children(n).iter().rev() {
                stack.push(c);
            }
        }
        out
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

    /// The tooltip drawn (role `tooltip`, not the link readout): its text and box, in points.
    pub fn tooltip(&self) -> Option<(String, auk_ui::Rect)> {
        let el = self.tooltip_el.filter(|&e| self.doc.try_node(e).is_some())?;
        let text: String = self
            .doc
            .children(el)
            .iter()
            .filter_map(|&c| self.doc.node(c).text())
            .collect();
        Some((text, self.doc.border_box(el)?))
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
