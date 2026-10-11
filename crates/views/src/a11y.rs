//! A simple accessibility tree of the projected surfaces: roles such as progressbar, meter,
//! tree, listbox, dialog and timer, with names and values, for the platform layer to feed to
//! AccessKit.

use crate::view::PaneView;
use auk_ui::{Document, NodeId};

/// One accessible node.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct AxNode {
    /// The role: an ARIA role, or `group`/`text` for plain containers and text.
    pub role: String,
    pub name: Option<String>,
    pub value: Option<String>,
    /// The TSP node id, when the element is a node.
    pub id: Option<String>,
    pub expanded: Option<bool>,
    pub selected: Option<bool>,
    pub checked: Option<bool>,
    pub children: Vec<AxNode>,
}

impl AxNode {
    /// Depth-first search.
    pub fn find(&self, f: &dyn Fn(&AxNode) -> bool) -> Option<&AxNode> {
        if f(self) {
            return Some(self);
        }
        self.children.iter().find_map(|c| c.find(f))
    }

    /// Every node, depth first.
    pub fn walk<'a>(&'a self, out: &mut Vec<&'a AxNode>) {
        out.push(self);
        for c in &self.children {
            c.walk(out);
        }
    }
}

fn implicit_role(doc: &Document, el: NodeId) -> Option<&'static str> {
    let tag = doc.node(el).element()?.tag.as_str().to_owned();
    Some(match tag.as_str() {
        "button" => "button",
        "table" => "table",
        "tr" => "row",
        "td" => "cell",
        "th" => "columnheader",
        "ul" | "ol" => "list",
        "li" => "listitem",
        "h1" | "h2" | "h3" | "h4" => "heading",
        "hr" => "separator",
        "form" => "form",
        "img" => "img",
        _ => return None,
    })
}

fn build(doc: &Document, el: NodeId, text: &mut String) -> Vec<AxNode> {
    let n = doc.node(el);
    if let Some(t) = n.text() {
        text.push_str(t);
        return Vec::new();
    }
    if !n.is_element() {
        return Vec::new();
    }
    if doc
        .computed_style(el)
        .is_some_and(|s| s.display == auk_ui::style::Display::None)
    {
        return Vec::new();
    }
    if doc.attribute(el, "aria-hidden") == Some("true") {
        return Vec::new();
    }
    let role = doc
        .attribute(el, "role")
        .map(str::to_owned)
        .or_else(|| implicit_role(doc, el).map(str::to_owned));
    let id = doc.attribute(el, "data-id").map(str::to_owned);
    let mut own_text = String::new();
    let mut kids = Vec::new();
    for &c in doc.children(el) {
        kids.extend(build(doc, c, &mut own_text));
    }
    if role.is_none() && id.is_none() {
        // Transparent: pass text and children up.
        text.push_str(&own_text);
        return kids;
    }
    let attr = |k: &str| doc.attribute(el, k).map(str::to_owned);
    let flag = |k: &str| doc.attribute(el, k).map(|v| v == "true");
    let mut node = AxNode {
        role: role.unwrap_or_else(|| "group".to_owned()),
        name: attr("aria-label").or_else(|| attr("alt")),
        value: attr("aria-valuenow").or_else(|| attr("aria-valuetext")),
        id,
        expanded: flag("aria-expanded"),
        selected: flag("aria-selected"),
        checked: flag("aria-checked"),
        children: kids,
    };
    let own = own_text.trim();
    if !own.is_empty() {
        if node.name.is_none() && node.children.is_empty() {
            node.name = Some(own.to_owned());
        } else {
            node.children.insert(
                0,
                AxNode {
                    role: "text".to_owned(),
                    name: Some(own.to_owned()),
                    ..Default::default()
                },
            );
        }
    }
    // A timer's value is its text.
    if node.role == "timer" && node.value.is_none() {
        node.value = node.name.clone();
    }
    vec![node]
}

impl PaneView {
    /// The accessibility tree of the pane's surfaces.
    pub fn accessibility(&self, doc: &Document) -> AxNode {
        let mut root = AxNode {
            role: "group".to_owned(),
            name: Some("Pane".to_owned()),
            ..Default::default()
        };
        for sv in &self.surfaces {
            for (r, name) in sv.regions.iter().zip(["main", "dock", "layer"]) {
                let Some(r) = *r else { continue };
                if doc.try_node(r).is_none() {
                    continue;
                }
                let mut text = String::new();
                let mut kids = Vec::new();
                for &c in doc.children(r) {
                    kids.extend(build(doc, c, &mut text));
                }
                root.children.push(AxNode {
                    role: "region".to_owned(),
                    name: Some(format!("{} {name}", sv.id)),
                    children: kids,
                    ..Default::default()
                });
            }
        }
        root
    }
}
