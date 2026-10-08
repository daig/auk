//! The accessibility tree (§8.3, §19), fed to AccessKit's macOS adapter: the window, the UI
//! engine's roles (the chrome's tab lists and buttons; surfaces' progress bars, meters, trees,
//! list boxes, dialogs and timers from `auk-views`), and each visible pane as a terminal whose
//! value is its screen text, with its surfaces' nodes as children.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use accesskit::{
    Action, ActionRequest, Node, NodeId, Rect as AxRect, Role, TreeId, TreeInfo, TreeUpdate,
};
use auk_platform_macos::{AccessibilityHandler, Window};
use auk_ui::Document;

use crate::window::WinState;

const ROOT: NodeId = NodeId(1);
/// Chrome elements: this plus the UI node's index.
const CHROME_BASE: u64 = 1 << 32;
/// Panes: this plus the pane id.
const PANE_BASE: u64 = 2 << 32;
/// A pane's surfaces: this plus the pane id shifted by 20, plus the node's walk index.
const SURFACE_BASE: u64 = 3 << 32;

/// Hands the window's accessibility to AccessKit.
pub fn enable(state: &Rc<RefCell<WinState>>, window: &Window) {
    window.enable_accessibility(Box::new(Handler(Rc::downgrade(state))));
}

/// Pushes the current tree if an assistive app is listening.
pub fn update(state: &WinState, window: &Window) {
    window.update_accessibility(|| tree(state));
}

struct Handler(Weak<RefCell<WinState>>);

impl AccessibilityHandler for Handler {
    fn initial_tree(&mut self) -> TreeUpdate {
        match self.0.upgrade() {
            Some(state) => match state.try_borrow() {
                Ok(state) => tree(&state),
                Err(_) => placeholder(),
            },
            None => placeholder(),
        }
    }

    fn action(&mut self, request: ActionRequest) {
        let Some(state) = self.0.upgrade() else {
            return;
        };
        let Ok(mut state) = state.try_borrow_mut() else {
            return;
        };
        let id = request.target_node.0;
        if (PANE_BASE..PANE_BASE + (1 << 32)).contains(&id)
            && matches!(request.action, Action::Focus | Action::Click)
        {
            state.focus_pane(id - PANE_BASE);
        }
    }
}

fn placeholder() -> TreeUpdate {
    TreeUpdate {
        nodes: vec![(ROOT, Node::new(Role::Window))],
        tree: Some(TreeInfo::new(ROOT)),
        tree_id: TreeId::ROOT,
        focus: ROOT,
    }
}

/// The whole tree.
pub fn tree(state: &WinState) -> TreeUpdate {
    let scale = state.size.map_or(2.0, |s| s.scale);
    let mut nodes = Vec::new();
    let mut root = Node::new(Role::Window);
    root.set_label(format!("Auk — {}", state.conn.window_key));
    let mut children = ui_nodes(state.chrome.doc(), CHROME_BASE, scale, &mut nodes);

    let focused = state.focused_pane();
    let mut focus = ROOT;
    {
        let shared = state.conn.shared();
        for (pane, rect) in &state.tab_layout.panes {
            let id = NodeId(PANE_BASE + pane);
            let mut node = Node::new(Role::Terminal);
            node.set_label(format!("Pane {pane}"));
            if let Some(data) = shared.panes.get(pane) {
                let text = data.replica.screen_text();
                node.set_value(text.trim_end().to_owned());
                let title = data.replica.terminal().title();
                if !title.is_empty() {
                    node.set_description(title.to_owned());
                }
            }
            node.set_bounds(AxRect::new(
                rect.x * scale,
                rect.y * scale,
                (rect.x + rect.w) * scale,
                (rect.y + rect.h) * scale,
            ));
            node.add_action(Action::Focus);
            // The pane's surfaces, through the views' roles.
            if let Some(sf) = state.surfaces.get(pane) {
                let ax = sf.view().accessibility(sf.doc());
                let mut next = SURFACE_BASE + (pane << 20);
                let kids: Vec<NodeId> = ax
                    .children
                    .iter()
                    .map(|c| surface_node(c, &mut next, &mut nodes))
                    .collect();
                if !kids.is_empty() {
                    node.set_children(kids);
                }
            }
            if Some(*pane) == focused {
                focus = id;
            }
            children.push(id);
            nodes.push((id, node));
        }
    }
    root.set_children(children);
    nodes.push((ROOT, root));
    TreeUpdate {
        nodes,
        tree: Some(TreeInfo::new(ROOT)),
        tree_id: TreeId::ROOT,
        focus,
    }
}

/// A surfaces accessibility node and its subtree, as AccessKit nodes.
fn surface_node(ax: &auk_views::AxNode, next: &mut u64, out: &mut Vec<(NodeId, Node)>) -> NodeId {
    let id = NodeId(*next);
    *next += 1;
    let role = match ax.role.as_str() {
        "text" => Role::Label,
        other => role_of(other).unwrap_or(Role::Group),
    };
    let mut node = Node::new(role);
    if let Some(name) = &ax.name {
        node.set_label(name.clone());
    }
    if let Some(value) = &ax.value {
        match value.parse::<f64>() {
            Ok(v) if matches!(role, Role::ProgressIndicator | Role::Meter | Role::Slider) => {
                node.set_numeric_value(v)
            }
            _ => node.set_value(value.clone()),
        }
    }
    if let Some(e) = ax.expanded {
        node.set_expanded(e);
    }
    if let Some(sel) = ax.selected {
        node.set_selected(sel);
    }
    if let Some(c) = ax.checked {
        node.set_toggled(if c {
            accesskit::Toggled::True
        } else {
            accesskit::Toggled::False
        });
    }
    let kids: Vec<NodeId> = ax
        .children
        .iter()
        .map(|c| surface_node(c, next, out))
        .collect();
    node.set_children(kids);
    out.push((id, node));
    id
}

/// The ARIA role names the UI engine's elements carry (§8.3), as AccessKit roles.
pub fn role_of(name: &str) -> Option<Role> {
    Some(match name {
        "button" => Role::Button,
        "tab" => Role::Tab,
        "tablist" => Role::TabList,
        "toolbar" => Role::Toolbar,
        "progressbar" => Role::ProgressIndicator,
        "meter" => Role::Meter,
        "tree" => Role::Tree,
        "treeitem" => Role::TreeItem,
        "listbox" => Role::ListBox,
        "option" => Role::ListBoxOption,
        "list" => Role::List,
        "listitem" => Role::ListItem,
        "dialog" => Role::Dialog,
        "alertdialog" => Role::AlertDialog,
        "timer" => Role::Timer,
        "status" => Role::Status,
        "alert" => Role::Alert,
        "textbox" => Role::TextInput,
        "checkbox" => Role::CheckBox,
        "radio" => Role::RadioButton,
        "switch" => Role::Switch,
        "slider" => Role::Slider,
        "link" => Role::Link,
        "img" | "image" => Role::Image,
        "table" | "grid" => Role::Table,
        "row" => Role::Row,
        "cell" | "gridcell" => Role::Cell,
        "columnheader" => Role::ColumnHeader,
        "heading" => Role::Heading,
        "group" | "region" => Role::Group,
        "menu" => Role::Menu,
        "menuitem" => Role::MenuItem,
        "separator" => Role::Splitter,
        "log" => Role::Log,
        "marquee" => Role::Marquee,
        "note" => Role::Note,
        "article" => Role::Article,
        "code" => Role::Code,
        _ => return None,
    })
}

/// Nodes for the elements of `doc` that carry a known `role`, nested by the nearest such
/// ancestor. Returns the top-level ones.
pub fn ui_nodes(
    doc: &Document,
    base: u64,
    scale: f64,
    out: &mut Vec<(NodeId, Node)>,
) -> Vec<NodeId> {
    let mut top = Vec::new();
    walk(doc, doc.root(), base, scale, out, &mut top);
    top
}

fn walk(
    doc: &Document,
    id: auk_ui::NodeId,
    base: u64,
    scale: f64,
    out: &mut Vec<(NodeId, Node)>,
    siblings: &mut Vec<NodeId>,
) {
    let role = doc.attribute(id, "role").and_then(role_of);
    match role {
        Some(role) => {
            let ax = NodeId(base + id.index() as u64);
            let mut node = Node::new(role);
            let label = doc
                .attribute(id, "aria-label")
                .map(str::to_owned)
                .unwrap_or_else(|| text_of(doc, id));
            if !label.is_empty() {
                node.set_label(label);
            }
            if let Some(value) = doc
                .attribute(id, "aria-valuenow")
                .and_then(|v| v.parse::<f64>().ok())
            {
                node.set_numeric_value(value);
            }
            if let Some(v) = doc
                .attribute(id, "aria-valuemin")
                .and_then(|v| v.parse::<f64>().ok())
            {
                node.set_min_numeric_value(v);
            }
            if let Some(v) = doc
                .attribute(id, "aria-valuemax")
                .and_then(|v| v.parse::<f64>().ok())
            {
                node.set_max_numeric_value(v);
            }
            if doc.attribute(id, "aria-selected") == Some("true") {
                node.set_selected(true);
            } else if matches!(role, Role::Tab | Role::ListBoxOption | Role::TreeItem) {
                node.set_selected(false);
            }
            if let Some(b) = doc.border_box(id) {
                node.set_bounds(AxRect::new(
                    f64::from(b.x) * scale,
                    f64::from(b.y) * scale,
                    f64::from(b.x + b.w) * scale,
                    f64::from(b.y + b.h) * scale,
                ));
            }
            if matches!(role, Role::Button | Role::Tab) {
                node.add_action(Action::Click);
            }
            let mut children = Vec::new();
            for c in doc.element_children(id) {
                walk(doc, c, base, scale, out, &mut children);
            }
            node.set_children(children);
            siblings.push(ax);
            out.push((ax, node));
        }
        None => {
            for c in doc.element_children(id) {
                walk(doc, c, base, scale, out, siblings);
            }
        }
    }
}

/// An element's text content, whitespace collapsed, at most 200 characters.
fn text_of(doc: &Document, id: auk_ui::NodeId) -> String {
    fn collect(doc: &Document, id: auk_ui::NodeId, out: &mut String) {
        if let Some(t) = doc.node(id).text() {
            out.push_str(t);
            out.push(' ');
        }
        for c in doc.children(id) {
            collect(doc, *c, out);
        }
    }
    let mut s = String::new();
    collect(doc, id, &mut s);
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    s.chars().take(200).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_from_the_ui_engine() {
        let mut doc = Document::new();
        let root = doc.root();
        let body = doc.create_element("body");
        doc.append_child(root, body);
        let list = doc.create_element("div");
        doc.set_attribute(list, "role", "tablist");
        doc.append_child(body, list);
        let plain = doc.create_element("div");
        doc.append_child(list, plain);
        let tab = doc.create_element("div");
        doc.set_attribute(tab, "role", "tab");
        doc.set_attribute(tab, "aria-selected", "true");
        let text = doc.create_text("  build   logs ");
        doc.append_child(tab, text);
        doc.append_child(plain, tab);
        let bar = doc.create_element("div");
        doc.set_attribute(bar, "role", "progressbar");
        doc.set_attribute(bar, "aria-valuenow", "0.5");
        doc.append_child(body, bar);
        doc.update();
        let mut nodes = Vec::new();
        let top = ui_nodes(&doc, 100, 2.0, &mut nodes);
        assert_eq!(top.len(), 2);
        let find = |role| nodes.iter().find(|(_, n)| n.role() == role).unwrap();
        let (_, l) = find(Role::TabList);
        assert_eq!(l.children().len(), 1);
        let (_, t) = find(Role::Tab);
        assert_eq!(t.label(), Some("build logs"));
        assert_eq!(t.is_selected(), Some(true));
        let (_, p) = find(Role::ProgressIndicator);
        assert_eq!(p.numeric_value(), Some(0.5));
    }
}
