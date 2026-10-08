//! `el`: HTML elements from a fixed tag list with an attribute allowlist (§7.14,
//! research/elements.md §2.9).

use super::begin;
use crate::common::{bool_prop, str_prop};
use crate::state::SurfaceState;
use crate::view::{Cx, Look, Mount};
use auk_tsp::NodeRef;
use auk_ui::ElementState;
use serde_json::Value;

/// The 42 tags `el` builds; the replica owns the list and reports unknown tags (§7.7).
pub use auk_tsp::EL_TAGS as TAGS;

/// Attributes the terminal owns; `attrs` can't set them.
const OWNED: [&str; 8] = [
    "data-id",
    "data-role",
    "data-tone",
    "data-mark",
    "data-href",
    "data-scope",
    "aria-label",
    "aria-checked",
];

/// The requested tag, lowercased.
pub(crate) fn requested(node: NodeRef) -> String {
    str_prop(node.props(), "tag")
        .unwrap_or("div")
        .to_ascii_lowercase()
}

/// Whether the node is an `el` checkbox or radio.
pub(crate) fn control_type(node: NodeRef) -> Option<&'static str> {
    if node.kind() != "el" || requested(node) != "input" {
        return None;
    }
    match str_prop(node.props(), "type") {
        Some("radio") => Some("radio"),
        Some("checkbox") => Some("checkbox"),
        // Any other type, or none, draws as an empty `div` (book:8622).
        _ => None,
    }
}

/// The element tag: `form` and unknown tags draw as `div`, checkbox and radio as `span`.
pub(crate) fn tag(node: NodeRef) -> &'static str {
    let t = requested(node);
    match t.as_str() {
        "form" => "div",
        "input" if control_type(node).is_some() => "span",
        "input" => "div",
        other => TAGS.iter().find(|x| **x == other).copied().unwrap_or("div"),
    }
}

fn attr_allowed(name: &str, control: bool) -> bool {
    if OWNED.contains(&name) {
        return false;
    }
    let tail_ok = |t: &str| {
        !t.is_empty()
            && t.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
    };
    if let Some(t) = name.strip_prefix("data-") {
        return tail_ok(t);
    }
    if let Some(t) = name.strip_prefix("aria-") {
        return tail_ok(t);
    }
    match name {
        "role" => !control,
        "colspan" | "rowspan" => true,
        _ => false,
    }
}

/// A control's shown checked state, with the local flip (§7.8).
pub(crate) fn checked(st: &mut SurfaceState, node: NodeRef) -> bool {
    SurfaceState::flip(&mut st.checks, node.id(), node.prop("checked"), false)
}

pub(crate) fn el(cx: &mut Cx, node: NodeRef, m: &mut Mount, look: &mut Look) {
    begin(cx, m);
    let p = node.props();
    let req = requested(node);
    let control = control_type(node);
    m.direct = !matches!(req.as_str(), "input" | "hr");
    if req == "form" {
        look.class("sf-form");
    }
    if let Some(Value::Object(attrs)) = p.get("attrs") {
        for (k, v) in attrs {
            let k = k.to_ascii_lowercase();
            if !attr_allowed(&k, control.is_some()) {
                continue;
            }
            let v = match v {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                _ => continue,
            };
            look.attr(&k, v);
        }
    }
    if let Some(kind) = control {
        let on = checked(cx.st, node);
        let disabled = bool_prop(p, "disabled");
        look.class(if kind == "radio" {
            "sf-radio"
        } else {
            "sf-check"
        });
        look.class_if(on, "on");
        look.attr("role", kind);
        look.attr("aria-checked", if on { "true" } else { "false" });
        if disabled {
            look.attr("aria-disabled", "true");
        }
        if let Some(n) = str_prop(p, "name") {
            look.attr("data-name", n);
        }
        cx.dom.state(m.el, ElementState::CHECKED, on);
        cx.dom.state(m.el, ElementState::DISABLED, disabled);
        return;
    }
    cx.dom.state(m.el, ElementState::CHECKED, false);
    if req == "button" && bool_prop(p, "disabled") {
        cx.dom.state(m.el, ElementState::DISABLED, true);
    }
    let text = match p.get("text") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    };
    if !text.is_empty() && m.direct {
        let t = cx.dom.doc.create_text(&text);
        // Before any children.
        let first = cx.dom.doc.children(m.el).first().copied();
        cx.dom.doc.insert_before(m.el, t, first);
        m.generated.push(t);
    }
}
