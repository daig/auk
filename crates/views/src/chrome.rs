//! Portable chrome widgets (§22 `views`: "chrome widgets", D3): the tab bar with its session
//! chip and session menu, split dividers and the focus ring, the status line and the command
//! palette, built by the UI engine into any document; and [`ChromeDoc`], the document a host
//! draws them in. The macOS app and the browser client draw their chrome with these, so both
//! show the same widgets with the same hooks: `data-action` names what a click does
//! ([`ChromeAction`]: `session-menu`, `session:<id>`, `tab:<id>`, `new-session`, `new-tab`,
//! `rename-session`, `lock-session`, `run:<action>`, `palette:<row>`, `dismiss`), and ARIA roles
//! describe them to assistive technology.
use auk_ui::{Document, NodeId, Rect, SheetKind};

/// The tab bar's height, in points.
pub const BAR_HEIGHT: f64 = 30.0;

/// Width of the line between split panes, in points.
pub const DIVIDER: f64 = 1.0;

/// The status line's height, in points (§9.8 "Chrome formatters").
pub const STATUS_HEIGHT: f64 = 22.0;

/// Palette rows shown at once.
pub const PALETTE_ROWS: usize = 14;

/// The widgets' sheet, with the chrome's base font and color. Hosts install it before their own
/// chrome sheet; `html.light` switches to the light look.
pub const CSS: &str = r#"
html { font-family: system-ui, sans-serif; font-size: 12px; color: #d4d7db; }
body { margin: 0; }
.bar {
  position: absolute; left: 0; top: 0; right: 0; height: 30px; box-sizing: border-box;
  display: flex; align-items: center; gap: 8px; padding: 0 10px;
  background-color: #1b1d20; border-bottom: 1px solid #2c2f33;
}
.tabs { display: flex; align-items: center; gap: 3px; }
.tabs { flex: 1 1 auto; min-width: 0; overflow: hidden; }
.tab, .add {
  padding: 3px 9px; border-radius: 5px; white-space: nowrap; color: #8d939a;
}
.session-chip {
  display: flex; align-items: center; gap: 4px; padding: 3px 7px 3px 9px; border-radius: 5px;
  white-space: nowrap; color: #c3c7cc; background-color: #2a2d31;
}
.session-chip.open { background-color: #34383d; color: #eceef0; }
.session-chip .chip-icon { font-family: 'Symbols Nerd Font Mono', 'Symbols Nerd Font'; font-size: 10px; color: #8d939a; }
.session-chip .chip-lock { color: #e2b45d; }
.tab { max-width: 220px; overflow: hidden; text-overflow: ellipsis; }
.tab.active { background-color: #2a2d31; color: #eceef0; }
.add { padding: 3px 7px; }
.zoomed { color: #e2b45d; }
.sep { width: 1px; height: 16px; background-color: #34373b; }
.divider { position: absolute; background-color: #2c2f33; }
.focus { position: absolute; box-sizing: border-box; border: 1px solid #4f7fd8; }
html.light .bar { background-color: #ececea; border-bottom-color: #d5d5d2; }
html.light .tab, html.light .add { color: #6d7177; }
html.light .session-chip { background-color: #dfdfdc; color: #2a2d31; }
html.light .session-chip.open { background-color: #d7d8d6; color: #1b1d20; }
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
.smenu {
  position: absolute; min-width: 200px; padding: 4px; box-sizing: border-box;
  background-color: #23262a; border: 1px solid #3a3e44; border-radius: 7px;
}
.smenu .sm-row { display: flex; gap: 12px; padding: 4px 10px; border-radius: 4px; color: #d4d7db; white-space: nowrap; }
.smenu .sm-row.current { background-color: #34383d; color: #ffffff; }
.smenu .sm-name { flex: 1 1 auto; }
.smenu .sm-count { color: #8d939a; font-size: 11px; }
.smenu .sm-sep { height: 1px; margin: 4px 6px; background-color: #34373b; }
html.light .smenu { background-color: #fbfbfa; border-color: #d5d5d2; }
html.light .smenu .sm-row { color: #1b1d20; }
html.light .smenu .sm-row.current { background-color: #e2e2df; }
html.light .smenu .sm-sep { background-color: #e2e2df; }
"#;

/// A session of the window key: the shown one (`active`) is the bar's chip, and each is a row
/// of the session menu.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionItem {
    pub id: u64,
    pub name: String,
    pub active: bool,
    pub locked: bool,
    /// The user named it (it didn't keep its generated name).
    pub named: bool,
    /// How many tabs it has.
    pub tabs: usize,
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
    /// What the empty query field says, when not the default ("Type a command…").
    pub placeholder: Option<String>,
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

/// The session chip's glyphs, in the bundled Nerd Font: a lock and a down chevron.
const LOCK_GLYPH: &str = "\u{ea75}";
const CHEVRON_GLYPH: &str = "\u{eab4}";

/// The session chip a bar shows for the shown session, if any: only when it says something, that
/// is when the window key has more than one session, or the session is locked or was named by
/// the user. A lone session with its generated name shows tabs only.
pub fn session_chip(sessions: &[SessionItem]) -> Option<&SessionItem> {
    let active = sessions.iter().find(|s| s.active)?;
    (sessions.len() > 1 || active.locked || active.named).then_some(active)
}

/// What a tab says: the title, unless its program's badge already says the same thing (an app
/// named `omp` whose title is `omp` shows `omp` once).
pub fn tab_label<'a>(title: &'a str, app: Option<&str>) -> Option<&'a str> {
    let t = title.trim();
    match app {
        Some(app) if t.is_empty() || t.eq_ignore_ascii_case(app.trim()) => None,
        _ => Some(title),
    }
}

/// The tab bar across the top: the session chip (`name ▾`, which opens the session menu) when
/// there is one, then the shown session's tabs, then `+` for a new tab when the host can create
/// one.
pub fn tab_bar(
    doc: &mut Document,
    parent: NodeId,
    chip: Option<&SessionItem>,
    tabs: &[TabItem],
    can_create: bool,
    menu_open: bool,
) -> NodeId {
    let bar = child(doc, parent, "div", "bar", None);
    doc.set_attribute(bar, "role", "toolbar");
    if let Some(s) = chip {
        let e = child(
            doc,
            bar,
            "div",
            if menu_open {
                "session-chip open"
            } else {
                "session-chip"
            },
            None,
        );
        doc.set_attribute(e, "id", "session-chip");
        doc.set_attribute(e, "data-action", "session-menu");
        doc.set_attribute(e, "role", "button");
        doc.set_attribute(e, "aria-haspopup", "menu");
        doc.set_attribute(e, "aria-expanded", if menu_open { "true" } else { "false" });
        let label = if s.locked {
            format!("Session {}, locked", s.name)
        } else {
            format!("Session {}", s.name)
        };
        doc.set_attribute(e, "aria-label", &label);
        if s.locked {
            doc.set_attribute(e, "data-locked", "true");
            let lock = child(doc, e, "span", "chip-icon chip-lock", Some(LOCK_GLYPH));
            doc.set_attribute(lock, "data-icon", "lock");
        }
        child(doc, e, "span", "chip-name", Some(&s.name));
        child(doc, e, "span", "chip-icon chip-caret", Some(CHEVRON_GLYPH));
        child(doc, bar, "div", "sep", None);
    }
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
        // Assistive technology always hears the program and the whole title.
        match &t.app {
            Some(app) if !t.title.trim().is_empty() => {
                doc.set_attribute(e, "aria-label", &format!("{app} \u{2014} {}", t.title))
            }
            Some(app) => doc.set_attribute(e, "aria-label", app),
            None => doc.set_attribute(e, "aria-label", &t.title),
        }
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
        if !plain && let Some(label) = tab_label(&t.title, t.app.as_deref()) {
            child(doc, e, "span", "tab-title", Some(label));
        }
        doc.set_attribute(e, "data-action", &format!("tab:{}", t.id));
        doc.set_attribute(e, "role", "tab");
        if t.active {
            doc.set_attribute(e, "aria-selected", "true");
        }
    }
    if can_create {
        let add = child(doc, list, "div", "add", Some("+"));
        doc.set_attribute(add, "data-action", "new-tab");
        doc.set_attribute(add, "role", "button");
        doc.set_attribute(add, "aria-label", "New Tab");
    }
    bar
}

/// The session menu, opened from the chip (whose box is `chip`), over a `w`×`h` window: a row
/// per session of the window key, the shown one marked, then, when the host can change sessions,
/// New Session, Rename Session… and Lock (or Unlock) Session.
pub fn session_menu(
    doc: &mut Document,
    parent: NodeId,
    (w, h): (f64, f64),
    chip: Rect,
    sessions: &[SessionItem],
    can_create: bool,
) -> NodeId {
    backdrop(doc, parent, w, h, false);
    let menu = child(doc, parent, "div", "smenu", None);
    doc.set_attribute(menu, "id", "session-menu");
    doc.set_attribute(menu, "role", "menu");
    doc.set_attribute(menu, "aria-label", "Sessions");
    doc.set_inline_style(
        menu,
        &format!("left: {}px; top: {}px", chip.x, chip.y + chip.h + 3.0),
    );
    for s in sessions {
        let row = child(
            doc,
            menu,
            "div",
            if s.active { "sm-row current" } else { "sm-row" },
            None,
        );
        doc.set_attribute(row, "role", "menuitemradio");
        doc.set_attribute(row, "aria-checked", if s.active { "true" } else { "false" });
        doc.set_attribute(row, "data-action", &format!("session:{}", s.id));
        child(doc, row, "span", "sm-name", Some(&s.name));
        let count = match s.tabs {
            1 => "1 tab".to_owned(),
            n => format!("{n} tabs"),
        };
        child(doc, row, "span", "sm-count", Some(&count));
    }
    if can_create {
        child(doc, menu, "div", "sm-sep", None);
        let locked = sessions.iter().any(|s| s.active && s.locked);
        for (label, action) in [
            ("New Session", "new-session"),
            ("Rename Session\u{2026}", "rename-session"),
            (
                if locked {
                    "Unlock Session"
                } else {
                    "Lock Session"
                },
                "lock-session",
            ),
        ] {
            let row = child(doc, menu, "div", "sm-row", Some(label));
            doc.set_attribute(row, "role", "menuitem");
            doc.set_attribute(row, "data-action", action);
        }
    }
    menu
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
        (
            "p-query empty",
            p.placeholder
                .as_deref()
                .unwrap_or("Type a command, or a path to open"),
        )
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

/// Something a click on the chrome asks for (an element's `data-action`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChromeAction {
    /// Open (or close) the session menu.
    SessionMenu,
    Session(u64),
    Tab(u64),
    NewSession,
    NewTab,
    RenameSession,
    LockSession,
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

/// The action a `data-action` value names.
pub fn parse_action(s: &str) -> Option<ChromeAction> {
    match s {
        "session-menu" => Some(ChromeAction::SessionMenu),
        "new-session" => Some(ChromeAction::NewSession),
        "new-tab" => Some(ChromeAction::NewTab),
        "rename-session" => Some(ChromeAction::RenameSession),
        "lock-session" => Some(ChromeAction::LockSession),
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

/// What a host's tab bar shows: the window key's sessions, the shown session's tabs, the split
/// lines and the focus ring, over a `width`×`height` window (points).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BarModel {
    pub width: f64,
    pub height: f64,
    pub dark: bool,
    /// The media features plugin sheets match (§8.4): reduced motion, reduced transparency,
    /// more contrast.
    pub media: (bool, bool, bool),
    /// Every session of the window key, the shown one `active`.
    pub sessions: Vec<SessionItem>,
    /// The shown session's tabs.
    pub tabs: Vec<TabItem>,
    pub dividers: Vec<Rect>,
    /// The focused pane's rectangle, ringed when the tab shows more than one pane.
    pub focus: Option<Rect>,
    /// The host creates tabs and sessions (the `+` button and the session menu's New, Rename and
    /// Lock rows). The browser client views and switches only.
    pub can_create: bool,
    /// The session menu is open.
    pub session_menu: bool,
}

/// The chrome document a host draws: the widgets' sheet, the host's own sheet, plugin sheets,
/// and the tab bar, dividers and focus ring built from a [`BarModel`]. Hosts add their overlays
/// between [`ChromeDoc::begin`] and [`ChromeDoc::finish`]; the session menu goes on top of them.
pub struct ChromeDoc {
    pub doc: Document,
    body: NodeId,
    /// Installed plugin sheets `(name, css)`, in order (§9.8 "CSS").
    sheets: Vec<(String, String)>,
    shown: Option<BarModel>,
}

impl std::fmt::Debug for ChromeDoc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChromeDoc")
            .field("shown", &self.shown)
            .finish()
    }
}

impl ChromeDoc {
    /// A chrome document at `scale` device pixels per point, styled by the widgets' sheet and
    /// then `host_css` (the host's own chrome sheet, which may be empty).
    pub fn new(scale: f32, host_css: &str) -> ChromeDoc {
        let mut doc = Document::new();
        let errors = doc.set_stylesheet("chrome-widgets", SheetKind::BuiltIn, None, CSS);
        debug_assert!(errors.is_empty(), "{errors:?}");
        if !host_css.is_empty() {
            let errors = doc.set_stylesheet("chrome", SheetKind::BuiltIn, None, host_css);
            debug_assert!(errors.is_empty(), "{errors:?}");
        }
        doc.set_device_scale(scale);
        // The chip's glyphs, and plugins' chrome styles may set Nerd Font icons (§9.8).
        crate::install_fonts(&mut doc);
        let root = doc.root();
        let body = doc.create_element("body");
        doc.append_child(root, body);
        ChromeDoc {
            doc,
            body,
            sheets: Vec::new(),
            shown: None,
        }
    }

    pub fn body(&self) -> NodeId {
        self.body
    }

    /// Draws the chrome's text in this font (a TTF/OTF file's bytes): for hosts without system
    /// fonts (the browser client).
    pub fn use_font(&mut self, data: Vec<u8>) {
        crate::surfaces::use_font(&mut self.doc, data);
        self.shown = None;
    }

    /// Installs plugin sheets (`plugin:…`) over the chrome's own, replacing the last set.
    /// Returns whether anything changed, and each sheet's errors.
    pub fn set_plugin_sheets(&mut self, sheets: &[(String, String)]) -> (bool, Vec<String>) {
        if self.sheets == sheets {
            return (false, Vec::new());
        }
        for (name, _) in &self.sheets {
            self.doc.remove_stylesheet(name, None);
        }
        let mut problems = Vec::new();
        for (name, css) in sheets {
            let errors = self.doc.set_stylesheet(name, SheetKind::Plugin, None, css);
            if !errors.is_empty() {
                problems.push(format!("plugin sheet {name}: {errors:?}"));
            }
        }
        self.sheets = sheets.to_vec();
        self.doc.update();
        (true, problems)
    }

    /// The installed plugin sheet names, in order.
    pub fn plugin_sheets(&self) -> Vec<String> {
        self.sheets.iter().map(|(n, _)| n.clone()).collect()
    }

    /// Starts a rebuild: the viewport, media features and look, then the tab bar, dividers and
    /// focus ring. Whatever the host appends next sits above them.
    pub fn begin(&mut self, bar: &BarModel) {
        self.doc.set_viewport(bar.width as f32, bar.height as f32);
        if self.shown.as_ref().is_none_or(|m| m.media != bar.media) {
            let (motion, transparency, contrast) = bar.media;
            self.doc
                .set_media_preferences(motion, transparency, contrast);
        }
        let root = self.doc.root();
        self.doc
            .set_attribute(root, "class", if bar.dark { "dark" } else { "light" });
        for child in self.doc.children(self.body).to_vec() {
            self.doc.remove(child);
        }
        let chip = session_chip(&bar.sessions);
        tab_bar(
            &mut self.doc,
            self.body,
            chip,
            &bar.tabs,
            bar.can_create,
            bar.session_menu && chip.is_some(),
        );
        dividers(&mut self.doc, self.body, &bar.dividers, bar.focus);
    }

    /// Ends a rebuild: the session menu over everything when it is open, then layout.
    pub fn finish(&mut self, bar: &BarModel) {
        if bar.session_menu && session_chip(&bar.sessions).is_some() {
            // The chip's box places the menu under it.
            self.doc.update();
            let chip = self
                .element_by_id("session-chip")
                .and_then(|c| self.doc.border_box(c))
                .unwrap_or(Rect::new(10.0, 0.0, 0.0, BAR_HEIGHT as f32 - 3.0));
            session_menu(
                &mut self.doc,
                self.body,
                (bar.width, bar.height),
                chip,
                &bar.sessions,
                bar.can_create,
            );
        }
        self.doc.update();
        self.shown = Some(bar.clone());
    }

    /// Rebuilds the bar if `bar` differs from what the document shows. Returns whether it did.
    /// For hosts with no overlays of their own.
    pub fn update(&mut self, bar: &BarModel) -> bool {
        if self.shown.as_ref() == Some(bar) {
            return false;
        }
        self.begin(bar);
        self.finish(bar);
        true
    }

    /// The bar the document shows.
    pub fn shown(&self) -> Option<&BarModel> {
        self.shown.as_ref()
    }

    fn element_by_id(&self, id: &str) -> Option<NodeId> {
        let mut stack = vec![self.doc.root()];
        while let Some(n) = stack.pop() {
            if self.doc.attribute(n, "id") == Some(id) {
                return Some(n);
            }
            stack.extend(self.doc.children(n).iter().copied());
        }
        None
    }

    /// Every element with an action and its box, in points, in document order.
    pub fn actions(&self) -> Vec<(String, Rect)> {
        actions(&self.doc)
    }

    /// The action under a point, in points.
    pub fn action_at(&self, x: f32, y: f32) -> Option<ChromeAction> {
        parse_action(&action_at(&self.doc, x, y)?)
    }

    /// The tabs drawn with the agent icon, by tab id, with the icon's box.
    pub fn agent_badges(&self) -> Vec<(u64, Rect)> {
        agent_badges(&self.doc)
    }

    /// The elements assistive technology sees, in document order: `(role, label, selected,
    /// action, box)`. Hosts whose chrome isn't in an accessibility tree of its own (the browser)
    /// mirror these.
    pub fn accessible(&self) -> Vec<AccessibleNode> {
        let mut out = Vec::new();
        let mut stack = vec![self.doc.root()];
        while let Some(n) = stack.pop() {
            if let Some(role) = self.doc.attribute(n, "role") {
                let label = self
                    .doc
                    .attribute(n, "aria-label")
                    .map(str::to_owned)
                    .unwrap_or_else(|| text_of(&self.doc, n));
                out.push(AccessibleNode {
                    role: role.to_owned(),
                    label,
                    selected: self.doc.attribute(n, "aria-selected") == Some("true")
                        || self.doc.attribute(n, "aria-checked") == Some("true"),
                    action: self.doc.attribute(n, "data-action").map(str::to_owned),
                    rect: self.doc.border_box(n),
                });
            }
            for &c in self.doc.children(n).iter().rev() {
                stack.push(c);
            }
        }
        out
    }
}

/// A tab as drawn: what its program badge and its title say (no title when the badge alone
/// names it), and its accessible name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DrawnTab {
    pub id: u64,
    pub app: Option<String>,
    pub title: Option<String>,
    pub label: String,
}

fn has_class(doc: &Document, n: NodeId, class: &str) -> bool {
    doc.node(n)
        .element()
        .is_some_and(|e| e.classes.iter().any(|c| c.as_str() == class))
}

impl ChromeDoc {
    /// The tabs as drawn, in order.
    pub fn tabs(&self) -> Vec<DrawnTab> {
        let doc = &self.doc;
        let mut out = Vec::new();
        let mut stack = vec![doc.root()];
        while let Some(n) = stack.pop() {
            if let Some(id) = doc
                .attribute(n, "data-action")
                .and_then(|a| a.strip_prefix("tab:"))
                .and_then(|id| id.parse().ok())
            {
                let mut app = None;
                let mut title = None;
                for &k in doc.children(n) {
                    if let Some(t) = doc.node(k).text() {
                        title = Some(t.to_owned());
                    } else if has_class(doc, k, "tab-app") {
                        app = Some(text_of(doc, k));
                    } else if has_class(doc, k, "tab-title") {
                        title = Some(text_of(doc, k));
                    }
                }
                out.push(DrawnTab {
                    id,
                    app,
                    title,
                    label: doc
                        .attribute(n, "aria-label")
                        .unwrap_or_default()
                        .to_owned(),
                });
                continue;
            }
            for &c in doc.children(n).iter().rev() {
                stack.push(c);
            }
        }
        out
    }
}

/// An element of the chrome as assistive technology sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct AccessibleNode {
    pub role: String,
    pub label: String,
    pub selected: bool,
    pub action: Option<String>,
    pub rect: Option<Rect>,
}

/// The text under a node.
fn text_of(doc: &Document, n: NodeId) -> String {
    let mut out = String::new();
    let mut stack = vec![n];
    while let Some(n) = stack.pop() {
        if let Some(t) = doc.node(n).text() {
            out.push_str(t);
        }
        for &c in doc.children(n).iter().rev() {
            stack.push(c);
        }
    }
    out
}
