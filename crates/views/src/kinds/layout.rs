//! Layout, bar, list and overlay kinds: `col row card section rule spacer status seg overlay toast
//! tabs list item` (research/elements.md §2.1, §2.4, §2.7).

use super::{begin, icon, part, slot, spans_into};
use crate::common::{self, bool_prop, num_prop, str_prop};
use crate::dom::Dom;
use crate::spans;
use crate::view::{Cx, Look, Mount};
use auk_tsp::{NodeRef, SurfaceKey};
use auk_ui::NodeId;
use serde_json::Value;

/// `gap`, `align`, `justify`, `wrap` classes shared by `col` and `row`.
pub(crate) fn flex(cx: &mut Cx, node: NodeRef, m: &mut Mount, look: &mut Look) {
    begin(cx, m);
    m.direct = true;
    let p = node.props();
    if let Some(g) = str_prop(p, "gap").filter(|g| matches!(*g, "none" | "xs" | "sm" | "md" | "lg"))
    {
        look.class(&format!("gap-{g}"));
    }
    if let Some(a) = str_prop(p, "align")
        .filter(|a| matches!(*a, "start" | "center" | "end" | "baseline" | "stretch"))
    {
        look.class(&format!("al-{a}"));
    }
    if let Some(j) = str_prop(p, "justify").filter(|j| matches!(*j, "between" | "end")) {
        look.class(&format!("js-{j}"));
    }
    look.class_if(bool_prop(p, "wrap"), "wrap");
    let grows = num_prop(p, "grow").is_some_and(|g| g > 0.0);
    look.class_if(node.child_count() == 0 && !grows, "sf-void");
}

/// The icon a `role` implies on `card`, `seg`, `overlay` (book:4461).
pub(crate) fn role_icon(role: &str) -> Option<&'static str> {
    let mut parts = role.split('.');
    let first = parts.next()?;
    let second = parts.next()?;
    let known = |n: &str| crate::icons::glyph(n).is_some();
    let pick = |n: &str| -> Option<&'static str> {
        crate::icon_table::ICONS
            .iter()
            .map(|(k, _, _)| *k)
            .find(|k| *k == n)
            .or_else(|| {
                crate::icon_table::ALIASES
                    .iter()
                    .find(|(a, _)| *a == n)
                    .map(|(_, t)| *t)
            })
    };
    match first {
        "lens" => Some(
            pick(second)
                .or(match second {
                    "build" => Some("app-hammer"),
                    "k8s" | "kubectl" | "docker" => Some("box"),
                    _ => None,
                })
                .unwrap_or("terminal"),
        ),
        "omp" if second == "tool" => {
            let tool = parts.next().unwrap_or("");
            Some(tool_icon(tool))
        }
        _ => {
            if known(second) {
                pick(second)
            } else {
                None
            }
        }
    }
}

/// omp tool names to icons.
pub(crate) fn tool_icon(tool: &str) -> &'static str {
    match tool {
        "bash" | "shell" | "exec" => "terminal",
        "python" => "app-python",
        "read" => "doc",
        "edit" | "write" | "patch" | "apply_patch" => "pen",
        "grep" | "search" | "find" => "search",
        "glob" | "ls" => "folder",
        "web" | "web_search" | "fetch" | "webfetch" => "globe",
        "todo" | "todo_write" | "todowrite" => "list-checks",
        "task" | "agent" => "layers",
        "lsp" => "code",
        "notebook" => "note",
        "ask" => "help",
        _ => "plug",
    }
}

fn status_chip(cx: &mut Cx, parent: NodeId, status: &str) -> Option<NodeId> {
    let (ico, label) = match status {
        "pending" => ("clock", "Pending"),
        "running" => ("", "Running"),
        "done" => ("check", "Done"),
        "error" => ("x", "Failed"),
        "cancelled" => ("stop", "Canceled"),
        _ => return None,
    };
    let chip = cx
        .dom
        .child(parent, "span", &format!("sf-chip st-{status}"));
    if ico.is_empty() {
        cx.dom.child(chip, "i", "sf-live-dot");
    } else {
        icon(cx, chip, ico);
    }
    cx.dom.text(chip, label);
    Some(chip)
}

fn chevron(cx: &mut Cx, parent: NodeId, collapsed: bool) {
    let c = cx.dom.child(parent, "span", "sf-chev");
    cx.dom.attr(c, "aria-hidden", "true");
    icon(cx, c, if collapsed { "chev-r" } else { "chev" });
}

/// Whether a `head` prop names one of the node's children.
fn head_child(node: NodeRef) -> bool {
    let Some(h) = str_prop(node.props(), "head") else {
        return false;
    };
    node.children().any(|c| c.id() == h)
}

/// The preview line count of a `preview` prop (`"auto"` = 10, `{lines}`).
pub(crate) fn preview_lines(v: Option<&Value>) -> Option<u32> {
    match v? {
        Value::String(s) if s == "auto" => Some(10),
        Value::Object(o) => o
            .get("lines")
            .and_then(Value::as_f64)
            .filter(|l| *l >= 0.0)
            .map(|l| l as u32),
        Value::Number(n) => n.as_f64().filter(|l| *l >= 0.0).map(|l| l as u32),
        _ => None,
    }
}

pub(crate) fn card(cx: &mut Cx, node: NodeRef, m: &mut Mount, look: &mut Look) {
    begin(cx, m);
    let p = node.props();
    let collapsible = bool_prop(p, "collapsible");
    let collapsed = collapsible && cx.st.fold(node, false);
    look.class_if(cx.st.unfolding(node, collapsed), "sf-unfold");
    let status = str_prop(p, "status").unwrap_or("");
    let has_head = p.get("head").is_some_and(|h| !h.is_null() && h != "");
    let nohead = !has_head && status.is_empty() && !collapsible;
    let preview = preview_lines(p.get("preview"));
    look.class_if(collapsible, "collapsible");
    look.class_if(collapsed, "collapsed");
    look.class_if(nohead, "nohead");
    look.class_if(bool_prop(p, "selected"), "selected");
    look.class_if(bool_prop(p, "inset"), "inset");
    look.class_if(str_prop(p, "variant") == Some("bare"), "bare");
    if !status.is_empty() {
        look.class(&format!("st-{status}"));
    }
    if collapsed {
        match preview {
            Some(n) => {
                look.class("preview");
                look.style(&format!("--sf-clamp: calc({n}*var(--sf-lh));"));
            }
            None => look.class("nopreview"),
        }
    }
    let head = part(cx, m, "div", "sf-card-head");
    if collapsible {
        cx.dom.attr(head, "role", "button");
        cx.dom.attr(head, "tabindex", "0");
        cx.dom.attr(
            head,
            "aria-expanded",
            if collapsed { "false" } else { "true" },
        );
    }
    let ic = cx.dom.child(head, "span", "sf-card-ic");
    cx.dom.attr(ic, "aria-hidden", "true");
    match str_prop(p, "role").and_then(role_icon) {
        Some(name) => {
            icon(cx, ic, name);
        }
        None => cx.dom.set_class(ic, "sf-card-ic sf-hidden"),
    }
    if head_child(node) {
        let s = slot(cx, m, 1, "div", "sf-head");
        cx.dom.doc.append_child(head, s);
    } else {
        let h = cx.dom.child(head, "div", "sf-head");
        if has_head {
            m.clock |= spans_into(cx, h, p.get("head"));
        }
    }
    status_chip(cx, head, status);
    if collapsible {
        chevron(cx, head, collapsed);
    }
    let body = slot(cx, m, 0, "div", "sf-card-body");
    cx.dom.doc.append_child(m.el, body);
    m.generated.push(body);
    if collapsed && preview.is_some() {
        let more = part(cx, m, "button", "sf-more sf-hidden");
        cx.dom.attr(more, "data-more", "card");
    }
}

pub(crate) fn section(cx: &mut Cx, node: NodeRef, m: &mut Mount, look: &mut Look) {
    begin(cx, m);
    let p = node.props();
    let collapsible = bool_prop(p, "collapsible");
    let collapsed = collapsible && cx.st.fold(node, false);
    look.class_if(collapsible, "collapsible");
    look.class_if(collapsed, "collapsed");
    look.class_if(cx.st.unfolding(node, collapsed), "sf-unfold");
    let head = part(cx, m, "div", "sf-section-head");
    if collapsible {
        cx.dom.attr(head, "role", "button");
        cx.dom.attr(head, "tabindex", "0");
        cx.dom.attr(
            head,
            "aria-expanded",
            if collapsed { "false" } else { "true" },
        );
    }
    chevron(cx, head, collapsed);
    if head_child(node) {
        let s = slot(cx, m, 1, "div", "sf-head");
        cx.dom.doc.append_child(head, s);
    } else {
        let h = cx.dom.child(head, "div", "sf-head");
        m.clock |= spans_into(cx, h, p.get("head"));
    }
    let body = slot(cx, m, 0, "div", "sf-section-body");
    cx.dom.doc.append_child(m.el, body);
    m.generated.push(body);
}

pub(crate) fn rule(cx: &mut Cx, node: NodeRef, m: &mut Mount, look: &mut Look) {
    begin(cx, m);
    look.attr("role", "separator");
    let label = node.props().get("label");
    if label.is_some_and(|l| !l.is_null() && l != "") {
        look.class("labeled");
        let l = part(cx, m, "span", "sf-rule-label");
        spans_into(cx, l, label);
    }
}

pub(crate) fn spacer(cx: &mut Cx, node: NodeRef, m: &mut Mount, look: &mut Look) {
    begin(cx, m);
    let size = match str_prop(node.props(), "size") {
        None => "md",
        Some(s @ ("none" | "xs" | "sm" | "md" | "lg")) => s,
        Some(_) => "x",
    };
    look.class(&format!("sz-{size}"));
    look.attr("aria-hidden", "true");
}

pub(crate) fn status(cx: &mut Cx, node: NodeRef, m: &mut Mount, look: &mut Look) {
    begin(cx, m);
    let p = node.props();
    look.class_if(bool_prop(p, "transparent"), "transparent");
    look.attr("role", "group");
    if str_prop(p, "aria").is_none() {
        look.attr("aria-label", "Status");
    }
    let row = part(cx, m, "div", "sf-st-row");
    let l = slot(cx, m, 0, "div", "sf-st-l");
    let r = slot(cx, m, 1, "div", "sf-st-r");
    cx.dom.doc.append_child(row, l);
    cx.dom.doc.append_child(row, r);
    cx.st.fit.insert(node.id().to_owned());
}

pub(crate) fn seg(cx: &mut Cx, node: NodeRef, m: &mut Mount, look: &mut Look) {
    begin(cx, m);
    let p = node.props();
    if let Some(side) = str_prop(p, "side").filter(|s| *s == "right") {
        look.class(&format!("side-{side}"));
    }
    if let Some(Value::Object(min)) = p.get("min")
        && let Some(w) = min.get("w").and_then(common::extent)
    {
        look.class("shrinks");
        look.style(&format!("min-width: {w};"));
    }
    if let Some(pr) = num_prop(p, "priority") {
        look.attr("data-priority", common::fmt_num(pr));
    }
    let ic = part(cx, m, "span", "sf-seg-ic");
    let name = str_prop(p, "icon")
        .map(str::to_owned)
        .or_else(|| str_prop(p, "role").and_then(role_icon).map(str::to_owned));
    match name {
        Some(n) => {
            icon(cx, ic, &n);
        }
        None => cx.dom.set_class(ic, "sf-seg-ic sf-hidden"),
    }
    let k = slot(
        cx,
        m,
        0,
        "span",
        if node.child_count() > 0 {
            "sf-seg-k"
        } else {
            "sf-seg-k sf-hidden"
        },
    );
    cx.dom.doc.append_child(m.el, k);
    m.generated.push(k);
    let t = part(cx, m, "span", "sf-seg-t");
    let v = p.get("spans").filter(|v| v.is_array()).or(p.get("text"));
    m.clock |= spans_into(cx, t, v);
}

pub(crate) fn overlay(cx: &mut Cx, node: NodeRef, m: &mut Mount, look: &mut Look) {
    begin(cx, m);
    let p = node.props();
    let size = str_prop(p, "size").unwrap_or("md");
    look.class(&format!("sz-{size}"));
    let anchor = match p.get("anchor") {
        Some(Value::String(s)) if matches!(s.as_str(), "top" | "bottom" | "center") => s.clone(),
        Some(Value::Object(o)) if o.contains_key("caret") => "caret".to_owned(),
        Some(Value::Object(o)) if o.contains_key("node") => "node".to_owned(),
        _ => "center".to_owned(),
    };
    look.class(&format!("at-{anchor}"));
    // An overlay that opens into a drawn surface pops in (§7.13).
    let drawn = cx.st.drawn;
    let pop = *cx.st.overlays.entry(node.id().to_owned()).or_insert(drawn);
    look.class_if(pop, "sf-pop");
    let modal = bool_prop(p, "modal");
    look.class_if(modal, "modal");
    if modal {
        let dim = part(cx, m, "div", "sf-ov-dim");
        cx.dom.attr(dim, "aria-hidden", "true");
    }
    let card = part(cx, m, "div", "sf-ov-card");
    cx.dom.attr(card, "role", "dialog");
    if modal {
        cx.dom.attr(card, "aria-modal", "true");
    }
    let head_v = p.get("head");
    let title_text = spans::spans_text(&spans::parse_spans(head_v));
    if let Some(a) = str_prop(p, "aria")
        .map(str::to_owned)
        .or_else(|| (!title_text.is_empty()).then_some(title_text.clone()))
    {
        cx.dom.attr(card, "aria-label", &a);
    }
    if head_v.is_some_and(|h| !h.is_null()) || str_prop(p, "role").and_then(role_icon).is_some() {
        let head = cx.dom.child(card, "div", "sf-ov-head");
        let ic = cx.dom.child(head, "span", "sf-ov-ic");
        match str_prop(p, "role").and_then(role_icon) {
            Some(n) => {
                icon(cx, ic, n);
            }
            None => cx.dom.set_class(ic, "sf-ov-ic sf-hidden"),
        }
        let t = cx.dom.child(head, "span", "sf-ov-title");
        m.clock |= spans_into(cx, t, head_v);
    }
    let body = slot(cx, m, 0, "div", "sf-ov-body");
    cx.dom.doc.append_child(card, body);
    let id = node.id().to_owned();
    cx.st.anchor_caret.remove(&id);
    cx.st.anchor_targets.remove(&id);
    match p.get("anchor") {
        Some(Value::Object(o)) if o.contains_key("caret") => {
            if let Some(t) = o.get("caret").and_then(Value::as_str) {
                cx.st.anchor_targets.insert(id.clone(), t.to_owned());
                cx.st.anchor_caret.insert(id.clone());
            }
            cx.st.anchored.insert(id);
        }
        Some(Value::Object(o)) if o.contains_key("node") => {
            if let Some(t) = o.get("node").and_then(Value::as_str) {
                cx.st.anchor_targets.insert(id.clone(), t.to_owned());
            }
            cx.st.anchored.insert(id);
        }
        _ => {
            cx.st.anchored.remove(&id);
        }
    }
}

/// The toast request a `toast` node makes; the pane's toast layer shows one at a time.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ToastReq {
    pub surface: SurfaceKey,
    pub id: String,
    pub text: String,
    pub sub: Option<String>,
    pub tone: String,
    pub ttl: Option<f64>,
    /// When the program asked for it (view clock, ms): its duration runs from here, so a toast
    /// asked for while the pane wasn't drawn doesn't show late or stay longer.
    pub at: f64,
}

#[derive(Debug, Default)]
pub(crate) struct ToastState {
    pub last: Option<(String, Option<String>, String, Option<f64>)>,
}

pub(crate) fn toast(cx: &mut Cx, node: NodeRef, m: &mut Mount, look: &mut Look) {
    begin(cx, m);
    look.attr("aria-hidden", "true");
    let p = node.props();
    let text = str_prop(p, "text").unwrap_or("").to_owned();
    let sub = str_prop(p, "sub").map(|s| {
        let first = s.lines().next().unwrap_or("");
        first.chars().take(120).collect::<String>()
    });
    let tone = str_prop(p, "tone").unwrap_or("").to_owned();
    let ttl = num_prop(p, "ttl");
    let sig = (text.clone(), sub.clone(), tone.clone(), ttl);
    let st = match &mut m.extra {
        crate::view::Extra::Toast(t) => t,
        e => {
            *e = crate::view::Extra::Toast(ToastState::default());
            let crate::view::Extra::Toast(t) = e else {
                unreachable!()
            };
            t
        }
    };
    // Re-shows when text, sub, tone or ttl change.
    if st.last.as_ref() != Some(&sig) && !text.is_empty() && !bool_prop(p, "hidden") {
        st.last = Some(sig);
        cx.st.toasts.push(ToastReq {
            surface: cx.sf.key(),
            id: node.id().to_owned(),
            text,
            sub,
            tone,
            ttl,
            at: cx.now,
        });
    }
}

/// The window-level toast layer at bottom center: one toast at a time (book:7636-7648).
#[derive(Debug)]
pub(crate) struct ToastHost {
    pub el: NodeId,
    pub showing: Option<(ToastReq, f64)>,
    pub queue: Vec<ToastReq>,
}

impl ToastHost {
    pub fn new(dom: &mut Dom, parent: NodeId) -> ToastHost {
        let el = dom.child(parent, "div", "auk-toasts");
        ToastHost {
            el,
            showing: None,
            queue: Vec::new(),
        }
    }

    pub fn forget_surface(&mut self, key: SurfaceKey) {
        self.queue.retain(|t| t.surface != key);
    }

    /// Show the newest request; it replaces the one showing. One whose time is already over
    /// (asked for while the pane wasn't drawn) clears the layer instead.
    pub fn show(&mut self, dom: &mut Dom, req: ToastReq, now: f64) {
        dom.clear(self.el);
        self.showing = None;
        let start = req.at.min(now);
        if now >= start + Self::duration(&req) {
            return;
        }
        let class = match req.tone.as_str() {
            "error" => "toast err",
            "info" | "accent" => "toast info",
            _ => "toast ok",
        };
        let t = dom.child(self.el, "div", class);
        dom.attr(t, "role", "alert");
        dom.attr(t, "aria-live", "assertive");
        let ic = dom.child(t, "span", "ico");
        let name = match req.tone.as_str() {
            "error" => "x-circle",
            "info" | "accent" => "info",
            _ => "check",
        };
        if let Some(g) = crate::icons::glyph(name) {
            dom.text(ic, &g.to_string());
        }
        dom.child_text(t, "span", "", &req.text);
        if let Some(s) = &req.sub {
            dom.child_text(t, "span", "mono", s);
        }
        self.showing = Some((req, start));
    }

    /// How long the showing toast stays: `ttl`, else 2.2 s, or 5.2 s for errors and long text.
    pub fn duration(req: &ToastReq) -> f64 {
        if let Some(t) = req.ttl.filter(|t| *t > 0.0) {
            return t;
        }
        if req.tone == "error" || req.sub.is_some() {
            5200.0
        } else {
            2200.0
        }
    }

    /// Expire the toast; returns the next deadline.
    pub fn tick(&mut self, dom: &mut Dom, now: f64) -> Option<f64> {
        let (req, at) = self.showing.as_ref()?;
        let end = at + Self::duration(req);
        if now >= end {
            dom.clear(self.el);
            self.showing = None;
            return None;
        }
        Some(end)
    }
}

pub(crate) fn tabs(cx: &mut Cx, node: NodeRef, m: &mut Mount, look: &mut Look) {
    begin(cx, m);
    let p = node.props();
    look.attr("role", "tablist");
    let active = p.get("active").map(value_key);
    for it in p
        .get("items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(o) = it.as_object() else { continue };
        let Some(id) = o.get("id").map(value_key) else {
            continue;
        };
        let on = active.as_deref() == Some(id.as_str());
        let t = part(cx, m, "div", if on { "sf-tab on" } else { "sf-tab" });
        cx.dom.attr(t, "role", "tab");
        cx.dom
            .attr(t, "aria-selected", if on { "true" } else { "false" });
        cx.dom.attr(t, "data-tab", &id);
        let label = o.get("label").or(o.get("id"));
        spans_into(cx, t, label);
    }
}

/// An id-like prop value as a string.
pub(crate) fn value_key(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

pub(crate) fn list(cx: &mut Cx, node: NodeRef, m: &mut Mount, look: &mut Look) {
    begin(cx, m);
    let p = node.props();
    look.attr("role", "listbox");
    let scroll = part(cx, m, "div", "sf-list-scroll");
    let max_style = match p.get("max") {
        Some(Value::Object(o)) => o.get("lines").and_then(Value::as_f64).map(|l| {
            format!(
                "max-height: calc({}*var(--sf-lh) + {}px)",
                common::fmt_num(l),
                common::fmt_num(l * 6.0)
            )
        }),
        Some(Value::Number(n)) => n
            .as_f64()
            .filter(|f| *f > 0.0)
            .map(|f| format!("max-height: {}vh", common::fmt_num(f * 100.0))),
        _ => None,
    };
    if let Some(s) = &max_style {
        cx.dom.set_class(scroll, "sf-list-scroll max");
        cx.dom.set_style(scroll, s);
    }
    let s = slot(cx, m, 0, "div", "sf-list-slot");
    cx.dom.doc.append_child(scroll, s);
    // Virtualizes past 200 items (§8.3).
    let virt = node.child_count() > 200 || bool_prop(p, "virtual");
    super::virtualize(cx, node.id(), s, virt.then_some(24.0));
    if node.child_count() == 0 {
        let e = part(cx, m, "div", "sf-list-empty");
        let empty = str_prop(p, "empty").unwrap_or("No results");
        cx.dom.text(e, empty);
    }
    if max_style.is_some() {
        cx.st.scrollers.insert(node.id().to_owned(), scroll);
    }
}

/// Filter marking: substring first, else a fuzzy subsequence (book:6061). Returns char ranges.
pub(crate) fn filter_marks(label: &str, filter: &str) -> Vec<(usize, usize)> {
    if filter.is_empty() {
        return Vec::new();
    }
    let l: Vec<char> = label.chars().flat_map(char::to_lowercase).collect();
    let f: Vec<char> = filter.chars().flat_map(char::to_lowercase).collect();
    if l.len() == label.chars().count()
        && let Some(at) = l.windows(f.len()).position(|w| w == f.as_slice())
    {
        return vec![(at, at + f.len())];
    }
    let mut out: Vec<(usize, usize)> = Vec::new();
    let mut fi = 0;
    for (i, c) in l.iter().enumerate() {
        if fi < f.len() && *c == f[fi] {
            match out.last_mut() {
                Some(r) if r.1 == i => r.1 = i + 1,
                _ => out.push((i, i + 1)),
            }
            fi += 1;
        }
    }
    if fi == f.len() { out } else { Vec::new() }
}

pub(crate) fn item(cx: &mut Cx, node: NodeRef, m: &mut Mount, look: &mut Look) {
    begin(cx, m);
    let p = node.props();
    let list = node.parent().filter(|l| l.kind() == "list");
    let selected = list
        .and_then(|l| l.prop("selected"))
        .is_some_and(|s| value_key(s) == node.id());
    let filter = list
        .and_then(|l| str_prop(l.props(), "filter"))
        .unwrap_or("");
    look.class_if(selected, "sel");
    look.class_if(bool_prop(p, "disabled"), "disabled");
    look.attr("role", "option");
    look.attr("aria-selected", if selected { "true" } else { "false" });
    if let Some(n) = str_prop(p, "icon") {
        let ic = part(cx, m, "span", "sf-item-ic");
        icon(cx, ic, n);
    }
    let label = part(cx, m, "span", "sf-item-label");
    let spans = spans::parse_spans(p.get("label"));
    let text = spans::spans_text(&spans);
    let marks = filter_marks(&text, filter);
    if marks.is_empty() {
        spans::render(&mut cx.dom, label, &spans);
    } else {
        let chars: Vec<char> = text.chars().collect();
        let mut at = 0;
        for (a, b) in marks {
            if a > at {
                cx.dom.text(label, &chars[at..a].iter().collect::<String>());
            }
            let mk = cx.dom.child(label, "mark", "sf-mark");
            cx.dom.text(mk, &chars[a..b].iter().collect::<String>());
            at = b;
        }
        if at < chars.len() {
            cx.dom.text(label, &chars[at..].iter().collect::<String>());
        }
    }
    if p.get("detail").is_some_and(|d| !d.is_null()) {
        let d = part(cx, m, "span", "sf-item-detail");
        spans_into(cx, d, p.get("detail"));
    }
    if p.get("value").is_some_and(|d| !d.is_null()) {
        let d = part(cx, m, "span", "sf-item-value");
        spans_into(cx, d, p.get("value"));
    }
    if let Some(Value::Array(keys)) = p.get("hint") {
        let h = part(cx, m, "span", "sf-item-hint kbds");
        for k in keys.iter().filter_map(Value::as_str) {
            let (cap, _) = crate::icons::keycap(k);
            cx.dom.child_text(h, "span", "kbd", &cap);
        }
    }
}
