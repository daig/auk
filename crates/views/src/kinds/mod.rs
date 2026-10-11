//! Kinds as components: each kind renders its node element's
//! classes, attributes and generated parts, and routes its children into slots.

pub(crate) mod agent;
pub(crate) mod block;
pub(crate) mod chart;
pub(crate) mod data;
pub(crate) mod edmd;
pub(crate) mod effort;
pub(crate) mod el;
pub(crate) mod input;
pub(crate) mod layout;
pub(crate) mod picker;
pub(crate) mod prefs;
pub(crate) mod rich;

use crate::common;
use crate::spans;
use crate::view::{Cx, Look, Mount, Mounts};
use auk_tsp::NodeRef;
use auk_ui::NodeId;

/// The kinds this crate draws and the hello advertises: T0, T1 and the T2 kinds drawn so far.
pub const KINDS: &[&str] = &[
    "col",
    "row",
    "card",
    "section",
    "rule",
    "spacer",
    "text",
    "badge",
    "kv",
    "icon",
    "spinner",
    "elapsed",
    "progress",
    "ansi",
    "rows",
    "el",
    "md",
    "code",
    "diff",
    "editor",
    "input",
    "status",
    "seg",
    "overlay",
    "toast",
    "shimmer",
    "rate",
    "meter",
    "list",
    "item",
    "tabs",
    "table",
    "tree",
    "kbd",
    "tool",
    "agent",
    "checklist",
    "image",
    "picker",
    "prefs",
    "effort",
    "chart",
    "math",
    "block",
];

/// Where a child element goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Route {
    Direct,
    Slot(usize),
}

/// Inline kinds render as spans.
pub(crate) fn is_inline(kind: &str) -> bool {
    matches!(
        kind,
        "badge" | "kbd" | "icon" | "spinner" | "shimmer" | "elapsed" | "rate" | "effort"
    )
}

/// The element tag for a node.
pub(crate) fn element_tag(node: NodeRef) -> String {
    match node.kind() {
        "el" => el::tag(node).to_owned(),
        k if is_inline(k) => "span".to_owned(),
        _ => "div".to_owned(),
    }
}

/// Kinds that draw children.
pub(crate) fn has_children(kind: &str) -> bool {
    matches!(
        kind,
        "col"
            | "row"
            | "card"
            | "section"
            | "el"
            | "status"
            | "seg"
            | "overlay"
            | "list"
            | "tool"
            | "agent"
            | "image"
            | "picker"
            | "prefs"
            | "block"
    ) || !KINDS.contains(&kind)
}

/// Kinds whose own drawing depends on their children (not only where they go): `prefs` turns
/// its editor card on for an editor child, `picker` says "Nothing to preview" without one, and
/// `block` shows Native/Raw only with both. They re-render when their children change.
pub(crate) fn reads_children(kind: &str) -> bool {
    matches!(kind, "prefs" | "picker" | "block")
}

/// Where `child` of `node` goes.
pub(crate) fn route(node: NodeRef, child: NodeRef) -> Route {
    match node.kind() {
        "card" | "section" => {
            if common::str_prop(node.props(), "head") == Some(child.id()) {
                Route::Slot(1)
            } else {
                Route::Slot(0)
            }
        }
        "status" => {
            if child.kind() == "seg" && common::str_prop(child.props(), "side") == Some("right") {
                Route::Slot(1)
            } else {
                Route::Slot(0)
            }
        }
        "seg" | "overlay" | "list" | "tool" | "agent" | "image" | "picker" => Route::Slot(0),
        "prefs" => Route::Slot(prefs::route(child)),
        // The native child, else the raw one: the block mounts only the one it shows.
        "block" => Route::Slot(usize::from(block::is_raw(node, child))),
        _ => Route::Direct,
    }
}

/// Detach persistent slots and drop generated parts, before a kind builds them again.
pub(crate) fn begin(cx: &mut Cx, m: &mut Mount) {
    for &s in &m.slots {
        cx.dom.doc.detach(s);
    }
    for g in std::mem::take(&mut m.generated) {
        if cx.dom.doc.try_node(g).is_some() {
            if m.slots.contains(&g) {
                cx.dom.doc.detach(g);
            } else {
                // A part freed by something else may have its id reused by now: removing it
                // would free another node.
                debug_assert_eq!(
                    cx.dom.doc.parent(g),
                    Some(m.el),
                    "generated {g:?} moved or reused"
                );
                cx.dom.doc.remove(g);
            }
        }
    }
}

/// A persistent slot element, created on first use.
pub(crate) fn slot(cx: &mut Cx, m: &mut Mount, i: usize, tag: &str, class: &str) -> NodeId {
    while m.slots.len() <= i {
        let s = cx.dom.el(tag, class);
        m.slots.push(s);
    }
    let s = m.slots[i];
    cx.dom.set_class(s, class);
    s
}

/// Virtualize a long container with an estimated child height, or stop. Containers that
/// an ancestor scrolls get their visible window from the measure pass.
pub(crate) fn virtualize(cx: &mut Cx, id: &str, el: NodeId, estimate: Option<f32>) {
    let had = cx.st.virt.get(id).copied();
    match estimate {
        Some(e) => {
            // A new element, or a new estimate (a picker switching layouts changes its row
            // height): set it again.
            if had != Some((el, e)) {
                cx.dom.doc.set_virtual(el, Some(e));
                cx.st.virt.insert(id.to_owned(), (el, e));
            }
        }
        None => {
            if let Some((h, _)) = had {
                if cx.dom.doc.try_node(h).is_some() {
                    cx.dom.doc.set_virtual(h, None);
                    cx.dom.doc.set_virtual_window(h, None);
                }
                cx.st.virt.remove(id);
            }
        }
    }
}

/// Append a generated top-level part to the node element.
pub(crate) fn part(cx: &mut Cx, m: &mut Mount, tag: &str, class: &str) -> NodeId {
    let id = cx.dom.child(m.el, tag, class);
    m.generated.push(id);
    id
}

/// An icon glyph span (`.ico`) inside `parent`.
pub(crate) fn icon(cx: &mut Cx, parent: NodeId, name: &str) -> NodeId {
    let s = cx.dom.child(parent, "span", "ico");
    cx.dom.attr(s, "aria-hidden", "true");
    if let Some(g) = crate::icons::glyph(name) {
        cx.dom.text(s, &g.to_string());
    }
    s
}

/// Spans into `parent`; returns whether an effect needs the clock.
pub(crate) fn spans_into(cx: &mut Cx, parent: NodeId, v: Option<&serde_json::Value>) -> bool {
    let s = spans::parse_spans(v);
    spans::render(&mut cx.dom, parent, &s)
}

/// Render a node: the kind writes `look` and its parts.
pub(crate) fn render(cx: &mut Cx, node: NodeRef, m: &mut Mount, look: &mut Look) {
    m.clock = false;
    match node.kind() {
        "col" | "row" => layout::flex(cx, node, m, look),
        "card" => layout::card(cx, node, m, look),
        "section" => layout::section(cx, node, m, look),
        "rule" => layout::rule(cx, node, m, look),
        "spacer" => layout::spacer(cx, node, m, look),
        "status" => layout::status(cx, node, m, look),
        "seg" => layout::seg(cx, node, m, look),
        "overlay" => layout::overlay(cx, node, m, look),
        "toast" => layout::toast(cx, node, m, look),
        "tabs" => layout::tabs(cx, node, m, look),
        "list" => layout::list(cx, node, m, look),
        "picker" => picker::picker(cx, node, m, look),
        "item" => layout::item(cx, node, m, look),
        "text" => data::text(cx, node, m, look),
        "badge" => data::badge(cx, node, m, look),
        "kbd" => data::kbd(cx, node, m, look),
        "icon" => data::icon_kind(cx, node, m, look),
        "kv" => data::kv(cx, node, m, look),
        "table" => data::table(cx, node, m, look),
        "tree" => data::tree(cx, node, m, look),
        "spinner" => crate::motion::spinner(cx, node, m, look),
        "shimmer" => crate::motion::shimmer(cx, node, m, look),
        "elapsed" => crate::motion::elapsed(cx, node, m, look),
        "rate" => crate::motion::rate(cx, node, m, look),
        "progress" => crate::motion::progress(cx, node, m, look),
        "meter" => crate::motion::meter(cx, node, m, look),
        "md" => rich::md(cx, node, m, look),
        "code" => rich::code(cx, node, m, look),
        "diff" => rich::diff(cx, node, m, look),
        "ansi" => rich::ansi(cx, node, m, look),
        "rows" => rich::rows(cx, node, m, look),
        "math" => rich::math(cx, node, m, look),
        "chart" => chart::chart(cx, node, m, look),
        "image" => rich::image(cx, node, m, look),
        "editor" | "input" => input::field(cx, node, m, look),
        "tool" => agent::tool(cx, node, m, look),
        "agent" => agent::agent(cx, node, m, look),
        "checklist" => agent::checklist(cx, node, m, look),
        "effort" => effort::effort(cx, node, m, look),
        "el" => el::el(cx, node, m, look),
        "block" => block::block(cx, node, m, look),
        "prefs" => prefs::prefs(cx, node, m, look),
        _ => generic(cx, node, m, look),
    }
}

/// A kind this view doesn't draw (T2 beyond the advertised set): its children in order.
fn generic(cx: &mut Cx, _node: NodeRef, m: &mut Mount, look: &mut Look) {
    begin(cx, m);
    m.direct = true;
    look.class("sf-unknown");
}

/// Work that needs the children in place (status fitting, tool body separators).
pub(crate) fn after_children(cx: &mut Cx, node: NodeRef, kind: &str, _el: NodeId, mounts: &Mounts) {
    if kind == "tool" {
        agent::mark_kind_changes(cx, node, mounts);
    }
    // Chains of thoughts among the children (decorations). Without a thinking child
    // there is no chain to sum or unsum (`thought_chains` only touches thinking sections), so a
    // long list of other entries costs one look at each kind.
    if node.children().any(crate::deco::is_thought) {
        crate::deco::thought_chains(cx, node);
    }
}

/// Clock-driven updates of a node. Returns whether it still needs the clock.
pub(crate) fn tick(cx: &mut Cx, node: NodeRef, m: &mut Mount) -> bool {
    match node.kind() {
        "spinner" | "shimmer" | "elapsed" | "rate" | "progress" | "meter" => {
            crate::motion::tick(cx, node, m)
        }
        "md" => rich::tick_md(cx, node, m),
        "code" => rich::tick_code(cx, node, m),
        "text" | "card" | "section" | "seg" | "item" | "kv" | "table" | "status" => {
            crate::motion::tick_fx(cx, m)
        }
        "tool" | "agent" => agent::tick(cx, node, m),
        "image" => rich::tick_image(cx, node, m),
        "math" => rich::tick_math(cx, node, m),
        "editor" | "input" => input::tick(cx, node, m),
        _ => false,
    }
}
