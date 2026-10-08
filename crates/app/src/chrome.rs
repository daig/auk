//! The window chrome (D3): the sessions and tab bar, pane dividers and the focus ring, drawn by
//! our UI engine as one document per window. The panes' areas stay transparent so the grids
//! show through.

use auk_ui::{Document, NodeId, SheetKind};

use crate::geometry::Rect;

/// The tab bar's height, in points.
pub const BAR_HEIGHT: f64 = 30.0;

/// Width of the line between split panes, in points.
pub const DIVIDER: f64 = 1.0;

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
}

/// Something the chrome lets the user do with a click.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChromeAction {
    Session(u64),
    Tab(u64),
    NewSession,
    NewTab,
}

/// The chrome document.
pub struct Chrome {
    pub doc: Document,
    model: Option<ChromeModel>,
    body: NodeId,
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
        }
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
        self.doc.update();
        self.model = Some(model.clone());
        true
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

fn parse_action(s: &str) -> Option<ChromeAction> {
    match s {
        "new-session" => Some(ChromeAction::NewSession),
        "new-tab" => Some(ChromeAction::NewTab),
        _ => {
            let (kind, id) = s.split_once(':')?;
            let id = id.parse().ok()?;
            match kind {
                "session" => Some(ChromeAction::Session(id)),
                "tab" => Some(ChromeAction::Tab(id)),
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
