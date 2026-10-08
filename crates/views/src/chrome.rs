//! Portable chrome widgets (§22 `views`: "chrome widgets", D3): the sessions and tab bar, split
//! dividers and the focus ring, the status line and the command palette, built by the UI engine
//! into any document. The macOS app and the browser client draw their chrome with these, so both
//! show the same widgets with the same hooks: `data-action` names what a click does (`session:<id>`,
//! `tab:<id>`, `new-session`, `new-tab`, `run:<action>`, `palette:<row>`, `dismiss`), and ARIA
//! roles describe them to assistive technology.

use auk_ui::{Document, NodeId, Rect};

/// The tab bar's height, in points.
pub const BAR_HEIGHT: f64 = 30.0;

/// Width of the line between split panes, in points.
pub const DIVIDER: f64 = 1.0;

/// The status line's height, in points (§9.8 "Chrome formatters").
pub const STATUS_HEIGHT: f64 = 22.0;

/// Palette rows shown at once.
pub const PALETTE_ROWS: usize = 14;

/// The widgets' sheet. Hosts install it before their own chrome sheet; `html.light` switches to
/// the light look.
pub const CSS: &str = r#"
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
#statusline .sl-plugin[data-tone="warning"] { color: #c69026; }
html.light #statusline { background-color: #ececea; border-top-color: #d5d5d2; color: #6d7177; }
html.light #statusline .sl-plugin { color: #2a2d31; }
.tab .tab-app, #statusline .sl-app {
  padding: 0 5px; margin-right: 5px; border-radius: 4px; font-size: 10px;
  background-color: #34507e; color: #eceef0;
}
#statusline .sl-app { margin-right: 0; }
.tab .tab-agent {
  padding: 0 4px; margin-right: 5px; border-radius: 4px; font-size: 10px;
  background-color: #5b3f8c; color: #f1e9ff;
}
html.light .tab .tab-agent { background-color: #e4d8f7; color: #4a2d7a; }
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
.palette .p-app { padding: 2px 8px 4px; color: #8d939a; font-size: 11px; }
html.light .palette { background-color: #fbfbfa; border-color: #d5d5d2; }
html.light .palette .p-query { color: #1b1d20; border-bottom-color: #e2e2df; }
html.light .palette .p-row { color: #2a2d31; }
html.light .palette .p-row.selected { background-color: #cddcf5; color: #1b1d20; }
"#;

/// A session in the bar.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionItem {
    pub id: u64,
    pub name: String,
    pub active: bool,
}

/// A tab in the bar.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TabItem {
    pub id: u64,
    pub title: String,
    pub active: bool,
    pub zoomed: bool,
    /// The program its focused pane reports (the hello's `app`, §7.2), shown as the tab's icon
    /// while a listening surface is open.
    pub app: Option<String>,
    /// Its focused pane is an agent pane (§5.1): the tab shows the agent icon.
    pub agent: bool,
}

/// The agent icon's glyph: a sparkle every system font has, so the badge never draws as an empty
/// box. The badge is `span.tab-agent[data-icon=agent]`, named "Agent" to assistive technology.
pub const AGENT_GLYPH: &str = "\u{2726}";

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

/// The palette as drawn: the query and the shown rows `(label, group)`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PaletteView {
    /// The program the focused pane reports (the hello's `app`, §7.2), named in the palette's
    /// header while a listening surface is open (§12.2).
    pub app: Option<String>,
    pub query: String,
    pub rows: Vec<(String, String)>,
    pub selected: usize,
}

/// Appends `<tag class=…>` with optional text under `parent`.
pub fn child(
    doc: &mut Document,
    parent: NodeId,
    tag: &str,
    class: &str,
    text: Option<&str>,
) -> NodeId {
    let e = doc.create_element(tag);
    doc.set_attribute(e, "class", class);
    if let Some(text) = text {
        let t = doc.create_text(text);
        doc.append_child(e, t);
    }
    doc.append_child(parent, e);
    e
}

/// Places an absolutely positioned box, in points.
pub fn place(doc: &mut Document, id: NodeId, r: Rect) {
    doc.set_inline_style(
        id,
        &format!(
            "left: {}px; top: {}px; width: {}px; height: {}px",
            r.x, r.y, r.w, r.h
        ),
    );
}

/// The sessions and tab bar across the top.
pub fn tab_bar(
    doc: &mut Document,
    parent: NodeId,
    sessions: &[SessionItem],
    tabs: &[TabItem],
) -> NodeId {
    let bar = child(doc, parent, "div", "bar", None);
    doc.set_attribute(bar, "role", "toolbar");
    let list = child(doc, bar, "div", "sessions", None);
    doc.set_attribute(list, "role", "tablist");
    doc.set_attribute(list, "aria-label", "Sessions");
    for s in sessions {
        let class = if s.active {
            "session active"
        } else {
            "session"
        };
        let e = child(doc, list, "div", class, Some(&s.name));
        doc.set_attribute(e, "data-action", &format!("session:{}", s.id));
        doc.set_attribute(e, "role", "tab");
        if s.active {
            doc.set_attribute(e, "aria-selected", "true");
        }
    }
    let add = child(doc, list, "div", "add", Some("+"));
    doc.set_attribute(add, "data-action", "new-session");
    doc.set_attribute(add, "role", "button");
    doc.set_attribute(add, "aria-label", "New Session");
    child(doc, bar, "div", "sep", None);
    let list = child(doc, bar, "div", "tabs", None);
    doc.set_attribute(list, "role", "tablist");
    doc.set_attribute(list, "aria-label", "Tabs");
    for t in tabs {
        let class = match (t.active, t.zoomed) {
            (true, true) => "tab active zoomed",
            (true, false) => "tab active",
            (false, true) => "tab zoomed",
            (false, false) => "tab",
        };
        let plain = t.app.is_none() && !t.agent;
        let e = child(doc, list, "div", class, plain.then_some(t.title.as_str()));
        if t.agent {
            // An agent pane's tab carries the agent icon (§5.1).
            doc.set_attribute(e, "data-kind", "agent");
            let badge = child(doc, e, "span", "tab-agent", Some(AGENT_GLYPH));
            doc.set_attribute(badge, "role", "img");
            doc.set_attribute(badge, "aria-label", "Agent");
            doc.set_attribute(badge, "data-icon", "agent");
        }
        if let Some(app) = &t.app {
            // The pane's program names the tab (its icon) while it draws.
            doc.set_attribute(e, "data-app", app);
            child(doc, e, "span", "tab-app", Some(app));
        }
        if !plain {
            child(doc, e, "span", "tab-title", Some(&t.title));
        }
        doc.set_attribute(e, "data-action", &format!("tab:{}", t.id));
        doc.set_attribute(e, "role", "tab");
        if t.active {
            doc.set_attribute(e, "aria-selected", "true");
        }
    }
    let add = child(doc, list, "div", "add", Some("+"));
    doc.set_attribute(add, "data-action", "new-tab");
    doc.set_attribute(add, "role", "button");
    doc.set_attribute(add, "aria-label", "New Tab");
    bar
}

/// The lines between split panes, and the focus ring around the focused pane when the tab shows
/// more than one.
pub fn dividers(doc: &mut Document, parent: NodeId, lines: &[Rect], focus: Option<Rect>) {
    for &d in lines {
        let div = child(doc, parent, "div", "divider", None);
        place(doc, div, d);
    }
    if let Some(f) = focus
        && !lines.is_empty()
    {
        let ring = child(doc, parent, "div", "focus", None);
        place(doc, ring, f);
    }
}

/// The status line at `top`: the focused pane's program and title, then plugin segments.
pub fn status_line(
    doc: &mut Document,
    parent: NodeId,
    top: f64,
    app: Option<&str>,
    title: &str,
    segments: &[StatusItem],
) -> NodeId {
    let line = child(doc, parent, "div", "statusline", None);
    doc.set_attribute(line, "id", "statusline");
    doc.set_inline_style(line, &format!("top: {top}px"));
    doc.set_attribute(line, "role", "status");
    if let Some(app) = app {
        let badge = child(doc, line, "div", "sl-app", Some(app));
        doc.set_attribute(badge, "data-app", app);
    }
    child(doc, line, "div", "sl-left", Some(title));
    let right = child(doc, line, "div", "sl-right", None);
    for seg in segments {
        let s = child(doc, right, "div", "sl-plugin", Some(&seg.text));
        doc.set_attribute(s, "data-plugin", &seg.plugin);
        if let Some(tone) = &seg.tone {
            doc.set_attribute(s, "data-tone", tone);
        }
        if let Some(icon) = &seg.icon {
            doc.set_attribute(s, "data-icon", icon);
        }
        if let Some(cmd) = &seg.command {
            doc.set_attribute(s, "data-action", &format!("run:{cmd}"));
            doc.set_attribute(s, "role", "button");
        }
    }
    line
}

/// A backdrop over a `w`×`h` window that dismisses what it sits under when clicked.
pub fn backdrop(doc: &mut Document, parent: NodeId, w: f64, h: f64, dim: bool) -> NodeId {
    let back = child(
        doc,
        parent,
        "div",
        if dim { "backdrop dim" } else { "backdrop" },
        None,
    );
    doc.set_attribute(back, "data-action", "dismiss");
    doc.set_inline_style(back, &format!("width: {w}px; height: {h}px"));
    back
}

/// The command palette over a `w`×`h` window: its backdrop, the query and the shown rows.
pub fn palette(doc: &mut Document, parent: NodeId, w: f64, h: f64, p: &PaletteView) -> NodeId {
    backdrop(doc, parent, w, h, false);
    let box_ = child(doc, parent, "div", "palette", None);
    doc.set_attribute(box_, "role", "dialog");
    match &p.app {
        Some(app) => {
            doc.set_attribute(box_, "aria-label", &format!("Command Palette, {app}"));
            let head = child(
                doc,
                box_,
                "div",
                "p-app",
                Some(&format!("{app} \u{2014} Commands")),
            );
            doc.set_attribute(head, "id", "palette-app");
            doc.set_attribute(head, "data-app", app);
        }
        None => doc.set_attribute(box_, "aria-label", "Command Palette"),
    }
    let (class, text) = if p.query.is_empty() {
        ("p-query empty", "Type a command, or a path to open")
    } else {
        ("p-query", p.query.as_str())
    };
    let q = child(doc, box_, "div", class, Some(text));
    doc.set_attribute(q, "role", "textbox");
    let rows = child(doc, box_, "div", "p-rows", None);
    doc.set_attribute(rows, "role", "listbox");
    for (i, (label, group)) in p.rows.iter().enumerate().take(PALETTE_ROWS) {
        let class = if i == p.selected {
            "p-row selected"
        } else {
            "p-row"
        };
        let r = child(doc, rows, "div", class, None);
        doc.set_attribute(r, "role", "option");
        doc.set_attribute(r, "data-action", &format!("palette:{i}"));
        child(doc, r, "span", "p-label", Some(label));
        child(doc, r, "span", "p-group", Some(group));
    }
    box_
}

/// The tabs showing the agent icon, by tab id, each with the badge's box.
pub fn agent_badges(doc: &Document) -> Vec<(u64, Rect)> {
    let mut out = Vec::new();
    let mut stack = vec![doc.root()];
    while let Some(n) = stack.pop() {
        if doc.attribute(n, "data-icon") == Some("agent")
            && let Some(tab) = doc.parent(n)
            && let Some(id) = doc
                .attribute(tab, "data-action")
                .and_then(|a| a.strip_prefix("tab:"))
                .and_then(|id| id.parse().ok())
            && let Some(r) = doc.border_box(n)
        {
            out.push((id, r));
        }
        for &c in doc.children(n).iter().rev() {
            stack.push(c);
        }
    }
    out
}

/// Every element with a `data-action` and its box, in document order: what a click at a point
/// does is the last (topmost) one containing it.
pub fn actions(doc: &Document) -> Vec<(String, Rect)> {
    let mut out = Vec::new();
    let mut stack = vec![doc.root()];
    while let Some(n) = stack.pop() {
        if let Some(a) = doc.attribute(n, "data-action")
            && let Some(r) = doc.border_box(n)
        {
            out.push((a.to_owned(), r));
        }
        for &c in doc.children(n).iter().rev() {
            stack.push(c);
        }
    }
    out
}

/// The action under a point, in points: the nearest element with an action at or above the
/// box there.
pub fn action_at(doc: &Document, x: f32, y: f32) -> Option<String> {
    let mut node = auk_ui::hit_test(doc, x, y);
    while let Some(id) = node {
        if let Some(a) = doc.attribute(id, "data-action") {
            return Some(a.to_owned());
        }
        node = doc.parent(id);
    }
    None
}
