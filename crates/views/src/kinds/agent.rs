//! Agent kinds: `tool`, `agent`, `checklist` (research/elements.md §2.8).

use super::layout::{preview_lines, tool_icon};
use super::{begin, icon, part, slot, spans_into};
use crate::common::{bool_prop, num_prop, str_prop};
use crate::motion::{anchor, format_duration};
use crate::spans;
use crate::state::{SurfaceState, state_key};
use crate::view::{Cx, Extra, Look, Mount, Mounts};
use auk_tsp::NodeRef;
use auk_ui::NodeId;
use serde_json::Value;
use std::collections::HashMap;

/// A live timer inside a composite kind.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LiveTimer {
    pub text: NodeId,
    /// Start, or the end of a countdown.
    pub anchor: f64,
    pub countdown: bool,
    pub prefix: String,
    pub last: String,
}

#[derive(Debug, Default)]
pub(crate) struct ToolState {
    pub timers: Vec<LiveTimer>,
    /// Checklist item statuses last drawn (`.fresh` marks only newly done ones).
    pub done: HashMap<String, String>,
}

fn timers(m: &mut Mount) -> &mut ToolState {
    if !matches!(m.extra, Extra::Tool(_)) {
        m.extra = Extra::Tool(Box::default());
    }
    let Extra::Tool(t) = &mut m.extra else {
        unreachable!()
    };
    t
}

fn live(
    cx: &mut Cx,
    m: &mut Mount,
    parent: NodeId,
    key: String,
    age: Option<&Value>,
    prefix: &str,
) {
    let Some(start) = anchor(cx, key, age) else {
        return;
    };
    let countdown = age.and_then(Value::as_f64).is_some_and(|a| a < 0.0);
    let text = cx.dom.text(parent, "");
    timers(m).timers.push(LiveTimer {
        text,
        anchor: start,
        countdown,
        prefix: prefix.to_owned(),
        last: String::new(),
    });
}

/// Tokens of a command line: the command word, flags and the rest.
fn command_tokens(cmd: &str) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    let mut first = true;
    for (i, w) in cmd.split(' ').enumerate() {
        if i > 0 {
            out.push(("", " ".to_owned()));
        }
        if w.is_empty() {
            continue;
        }
        let class = if first {
            first = false;
            "cmd"
        } else if w.starts_with('-') {
            "flag"
        } else if matches!(w, "|" | "&&" | "||" | ";" | ">" | ">>" | "<") {
            first = true;
            "op"
        } else {
            ""
        };
        out.push((class, w.to_owned()));
    }
    out
}

pub(crate) fn tool(cx: &mut Cx, node: NodeRef, m: &mut Mount, look: &mut Look) {
    begin(cx, m);
    timers(m).timers.clear();
    let p = node.props();
    look.title_is_content = true;
    let status = str_prop(p, "status").unwrap_or("pending");
    let running = matches!(status, "running" | "pending");
    let collapsible = bool_prop(p, "collapsible");
    let collapsed = collapsible && cx.st.fold(node, false);
    look.class_if(cx.st.unfolding(node, collapsed), "sf-unfold");
    let inline = str_prop(p, "frame") == Some("inline");
    let tk = str_prop(p, "targetKind").unwrap_or("text");
    look.class("card");
    look.class(&format!("st-{status}"));
    look.class_if(collapsible, "collapsible");
    look.class_if(collapsed, "collapsed");
    look.class_if(inline, "inline");
    look.attr("data-tk", tk);
    let preview = p.get("preview");
    let tail = preview.and_then(|v| v.get("tail")).is_some();
    if collapsed {
        match preview_lines(preview).or_else(|| {
            preview
                .and_then(|v| v.get("tail"))
                .and_then(Value::as_f64)
                .map(|t| t as u32)
        }) {
            Some(n) => {
                look.class("preview");
                look.class_if(tail, "tail");
                look.style(&format!("--sf-clamp: calc({n}*var(--sf-lh));"));
            }
            None => look.class("nopreview"),
        }
    }
    let bar = part(cx, m, "i", "sf-tool-bar");
    let _ = bar;
    let head = part(cx, m, "div", "sf-tool-head");
    if collapsible {
        cx.dom.attr(head, "role", "button");
        cx.dom.attr(head, "tabindex", "0");
        cx.dom.attr(
            head,
            "aria-expanded",
            if collapsed { "false" } else { "true" },
        );
    }
    let ic = cx.dom.child(head, "span", "sf-tool-ic");
    icon(cx, ic, tool_icon(str_prop(p, "name").unwrap_or("")));
    let line = cx.dom.child(head, "span", "sf-tool-line");
    if let Some(t) = str_prop(p, "title") {
        cx.dom.child_text(line, "span", "sf-tool-title", t);
    }
    if let Some(target) = str_prop(p, "target") {
        let te = cx.dom.child(line, "span", &format!("sf-tool-target {tk}"));
        if let Some(h) = str_prop(p, "href") {
            cx.dom
                .set_class(te, &format!("sf-tool-target {tk} sf-link"));
            cx.dom.attr(te, "data-href", h);
        }
        if tk == "command" {
            for (class, w) in command_tokens(target) {
                if class.is_empty() {
                    cx.dom.text(te, &w);
                } else {
                    cx.dom.child_text(te, "span", class, &w);
                }
            }
        } else if tk == "path"
            && let Some(i) = target.rfind('/')
        {
            cx.dom.child_text(te, "span", "dir", &target[..=i]);
            cx.dom.text(te, &target[i + 1..]);
        } else {
            cx.dom.text(te, target);
        }
    }
    if let Some(Value::Array(meta)) = p.get("meta")
        && !meta.is_empty()
    {
        let me = cx.dom.child(line, "span", "sf-tool-meta");
        for (i, mv) in meta.iter().enumerate() {
            if i > 0 {
                cx.dom.text(me, " ");
            }
            m.clock |= spans_into(cx, me, Some(mv));
        }
    }
    if let Some(Value::Array(badges)) = p.get("badges") {
        for b in badges {
            let (text, tone) = match b {
                Value::String(s) => (s.clone(), None),
                Value::Object(o) => (
                    o.get("text")
                        .or(o.get("t"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                    o.get("tone").and_then(Value::as_str).map(str::to_owned),
                ),
                _ => continue,
            };
            let be = cx.dom.child_text(line, "span", "sf-badge", &text);
            if let Some(t) = tone {
                cx.dom.attr(be, "data-tone", &t);
            }
        }
    }
    if running && let Some(intent) = str_prop(p, "intent") {
        cx.dom.child_text(line, "span", "sf-tool-intent", intent);
    }
    cx.dom.child(line, "span", "sf-tool-sp");
    if let Some(note) = p.get("note").filter(|n| !n.is_null()) {
        let ne = cx.dom.child(line, "span", "sf-tool-note");
        spans_into(cx, ne, Some(note));
    }
    if let Some(exit) = p.get("exit").and_then(Value::as_i64).filter(|e| *e != 0) {
        cx.dom.child_text(
            line,
            "span",
            "sf-chip st-error sf-tool-exit",
            &format!("exit {exit}"),
        );
    }
    // Time: a live timer from `age` while running, the frozen `took` after.
    if let Some(took) = num_prop(p, "took") {
        let te = cx.dom.child(
            head,
            "span",
            if took < 1000.0 {
                "sf-tool-time fast"
            } else {
                "sf-tool-time"
            },
        );
        cx.dom.attr(te, "role", "timer");
        cx.dom.text(te, &format_duration(took, false));
    } else if running && p.get("age").is_some() {
        let te = cx.dom.child(head, "span", "sf-tool-time");
        cx.dom.attr(te, "role", "timer");
        live(
            cx,
            m,
            te,
            crate::motion::anchor_key(node.id(), "age"),
            p.get("age"),
            "",
        );
    }
    let stc = cx.dom.child(head, "span", "sf-tool-st");
    match status {
        "running" | "pending" => {
            cx.dom.child(stc, "i", "sf-tool-spin");
        }
        "done" | "success" => {
            icon(cx, stc, "check");
        }
        "error" | "failed" => {
            icon(cx, stc, "x");
        }
        "cancelled" => {
            icon(cx, stc, "slash-circle");
        }
        _ => {}
    }
    if collapsible {
        let c = cx.dom.child(head, "span", "sf-chev");
        icon(cx, c, if collapsed { "chev-r" } else { "chev" });
    }
    if let Some(Value::Array(tools)) = p.get("tools")
        && !tools.is_empty()
    {
        let acts = cx.dom.child(head, "span", "sf-tool-acts");
        for t in tools {
            let Some(o) = t.as_object() else { continue };
            let id = o.get("id").and_then(Value::as_str).unwrap_or("");
            let label = o.get("label").and_then(Value::as_str).unwrap_or(id);
            let b = cx.dom.child_text(acts, "button", "sf-tool-act", label);
            cx.dom.attr(b, "data-act", id);
            if let Some(k) = o.get("keys").and_then(Value::as_str)
                && let Some(caps) = crate::icons::key_id(k)
            {
                cx.dom.child_text(b, "span", "kbd", &caps);
            }
        }
    }
    let body = slot(cx, m, 0, "div", "sf-tool-body");
    cx.dom.doc.append_child(m.el, body);
    m.generated.push(body);
    if collapsed && preview.is_some() {
        let more = part(cx, m, "button", "sf-tool-more sf-hidden");
        cx.dom.attr(more, "data-more", "tool");
    }
    m.clock |= !timers(m).timers.is_empty();
}

/// `.sf-tool-hr` on body children whose kind differs from the previous one; nested cards and
/// tools draw unframed.
pub(crate) fn mark_kind_changes(cx: &mut Cx, node: NodeRef, mounts: &Mounts) {
    let mut prev: Option<&str> = None;
    for c in node.children() {
        let Some(cm) = mounts.get(c.id()) else {
            continue;
        };
        let class = cx.dom.class_of(cm.el).to_owned();
        let has = class.split(' ').any(|x| x == "sf-tool-hr");
        let want = prev.is_some_and(|p| p != c.kind());
        if want != has {
            let new = if want {
                format!("{class} sf-tool-hr")
            } else {
                class
                    .split(' ')
                    .filter(|x| *x != "sf-tool-hr")
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            cx.dom.set_class(cm.el, &new);
        }
        prev = Some(c.kind());
    }
}

/// Live timers of `tool` and `agent`.
pub(crate) fn tick(cx: &mut Cx, _node: NodeRef, m: &mut Mount) -> bool {
    let now = cx.now;
    let Extra::Tool(t) = &mut m.extra else {
        return false;
    };
    let mut any = false;
    for lt in &mut t.timers {
        let ms = if lt.countdown {
            (lt.anchor - now).max(0.0)
        } else {
            (now - lt.anchor).max(0.0)
        };
        let text = format!("{}{}", lt.prefix, format_duration(ms, false));
        if text != lt.last {
            cx.dom.doc.set_text(lt.text, &text);
            lt.last = text;
        }
        any |= !lt.countdown || ms > 0.0;
    }
    any
}

fn fmt_tokens(n: f64) -> String {
    if n >= 1_000_000.0 {
        format!("{:.1}M", n / 1_000_000.0)
    } else if n >= 1000.0 {
        format!("{:.1}K", n / 1000.0)
    } else {
        format!("{}", n as i64)
    }
}

pub(crate) fn agent(cx: &mut Cx, node: NodeRef, m: &mut Mount, look: &mut Look) {
    begin(cx, m);
    timers(m).timers.clear();
    let p = node.props();
    let status = str_prop(p, "status").unwrap_or("pending");
    let collapsible = bool_prop(p, "collapsible");
    let collapsed = collapsible && cx.st.fold(node, false);
    look.class(&format!("st-{status}"));
    look.class_if(collapsible, "collapsible");
    look.class_if(collapsed, "collapsed");
    look.class_if(cx.st.unfolding(node, collapsed), "sf-unfold");
    let depth = num_prop(p, "depth").unwrap_or(0.0).clamp(0.0, 6.0);
    look.style(&format!("--sf-depth: {};", depth as u32));
    let row = part(cx, m, "div", "sf-agent-row");
    if collapsible {
        cx.dom.attr(row, "role", "button");
        cx.dom.attr(row, "tabindex", "0");
        cx.dom.attr(
            row,
            "aria-expanded",
            if collapsed { "false" } else { "true" },
        );
    }
    let g = cx.dom.child(row, "span", "sf-agent-g");
    cx.dom
        .child(g, "i", if status == "running" { "live" } else { "" });
    let name = str_prop(p, "name")
        .or(str_prop(p, "agent"))
        .unwrap_or("agent");
    cx.dom.child_text(row, "span", "sf-agent-name", name);
    if let Some(Value::Array(badges)) = p.get("badges") {
        for b in badges.iter().filter_map(Value::as_str) {
            cx.dom.child_text(row, "span", "sf-badge", b);
        }
    }
    if let Some(task) = p.get("task").filter(|t| !t.is_null()) {
        let te = cx.dom.child(row, "span", "sf-agent-task");
        m.clock |= spans_into(cx, te, Some(task));
    }
    cx.dom.child(row, "span", "sf-sp");
    let stats = cx.dom.child(row, "span", "sf-agent-stats");
    if let Some(model) = str_prop(p, "model") {
        let me = cx.dom.child(stats, "span", "model");
        cx.dom.child(me, "i", "dot");
        cx.dom.text(me, model);
    }
    let s = p
        .get("stats")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let n = |k: &str| s.get(k).and_then(Value::as_f64);
    if let Some(t) = n("tools") {
        let e = cx.dom.child(stats, "span", "tools");
        icon(cx, e, "wrench");
        cx.dom.text(e, &format!("{}", t as i64));
    }
    if let Some(r) = n("requests") {
        cx.dom
            .child_text(stats, "span", "req", &format!("{} req", r as i64));
    }
    if let Some(d) = n("done") {
        cx.dom.child_text(
            stats,
            "span",
            "done",
            &format!("{}%", (d * 100.0).round() as i64),
        );
    }
    if let Some(c) = n("context") {
        let level = if c >= 0.9 {
            " bad"
        } else if c >= 0.75 {
            " warn"
        } else {
            ""
        };
        let me = cx
            .dom
            .child(stats, "span", &format!("sf-agent-meter{level}"));
        if let Some(l) = s.get("contextLabel").and_then(Value::as_str) {
            cx.dom.attr(me, "title", l);
        }
        let f = cx.dom.child(me, "i", "fill");
        cx.dom.set_style(
            f,
            &format!(
                "width: {}%",
                crate::common::fmt_num(c.clamp(0.0, 1.0) * 100.0)
            ),
        );
    }
    if let Some(t) = n("tokens") {
        cx.dom.child_text(stats, "span", "tokens", &fmt_tokens(t));
    }
    if let Some(c) = n("cost") {
        cx.dom
            .child_text(stats, "span", "cost", &format!("${c:.2}"));
    }
    if let Some(took) = n("took") {
        let e = cx
            .dom
            .child_text(stats, "span", "time", &format_duration(took, false));
        cx.dom.attr(e, "role", "timer");
    } else if s.get("age").is_some() && matches!(status, "running" | "pending" | "idle") {
        let e = cx.dom.child(stats, "span", "time");
        cx.dom.attr(e, "role", "timer");
        live(
            cx,
            m,
            e,
            crate::motion::anchor_key(node.id(), "stats.age"),
            s.get("age"),
            "",
        );
    }
    if let Some(Value::Object(t)) = p.get("tool") {
        let now = part(cx, m, "div", "sf-agent-now");
        let ic = cx.dom.child(now, "span", "ic");
        icon(
            cx,
            ic,
            tool_icon(t.get("name").and_then(Value::as_str).unwrap_or("")),
        );
        if let Some(name) = t.get("name").and_then(Value::as_str) {
            cx.dom.child_text(now, "span", "tn", name);
        }
        if let Some(intent) = t.get("intent").and_then(Value::as_str) {
            cx.dom.child_text(now, "span", "intent", intent);
        }
        if t.get("age").is_some() {
            let te = cx.dom.child(now, "span", "time");
            live(
                cx,
                m,
                te,
                crate::motion::anchor_key(node.id(), "tool.age"),
                t.get("age"),
                "",
            );
        }
    }
    if let Some(Value::Object(r)) = p.get("retry") {
        let re = part(cx, m, "div", "sf-agent-retry");
        let attempt = r.get("attempt").and_then(Value::as_i64).unwrap_or(1);
        let max = r.get("max").and_then(Value::as_i64);
        cx.dom.text(
            re,
            &match max {
                Some(mx) => format!("Retry {attempt}/{mx} "),
                None => format!("Retry {attempt} "),
            },
        );
        if let (Some(delay), Some(age)) = (
            r.get("delay").and_then(Value::as_f64),
            r.get("age").and_then(Value::as_f64),
        ) {
            // A countdown to the retry: `delay` ms after the age's start.
            let remaining = -(delay - age);
            let te = cx.dom.child(re, "span", "time");
            live(
                cx,
                m,
                te,
                crate::motion::anchor_key(node.id(), "retry.age"),
                Some(&Value::from(remaining)),
                "in ",
            );
        }
        if let Some(e) = r.get("error").and_then(Value::as_str) {
            cx.dom.child_text(re, "span", "err", e);
        }
    }
    let kids = slot(cx, m, 0, "div", "sf-agent-kids");
    cx.dom.doc.append_child(m.el, kids);
    m.generated.push(kids);
    m.clock |= !timers(m).timers.is_empty();
}

fn ring(cx: &mut Cx, parent: NodeId, done: usize, total: usize) {
    let full = total > 0 && done == total;
    let r = cx.dom.child(
        parent,
        "span",
        if full { "sf-ring full" } else { "sf-ring" },
    );
    cx.dom.attr(r, "aria-hidden", "true");
    let frac = if total == 0 {
        0.0
    } else {
        done as f64 / total as f64
    };
    cx.dom
        .set_style(r, &format!("--sf-meter: {}", crate::common::fmt_num(frac)));
}

pub(crate) fn checklist(cx: &mut Cx, node: NodeRef, m: &mut Mount, look: &mut Look) {
    begin(cx, m);
    let p = node.props();
    let mode = str_prop(p, "mode").unwrap_or("full");
    look.class(mode);
    look.attr("role", "group");
    // The `toggle` action folds the whole list to its phase heads (§7.8); `collapsed` sets it.
    let collapsed = cx.st.fold(node, false);
    look.class_if(collapsed, "collapsed");
    look.class_if(cx.st.unfolding(node, collapsed), "sf-unfold");
    let phases: Vec<serde_json::Map<String, Value>> = p
        .get("phases")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|x| x.as_object().cloned()).collect())
        .unwrap_or_default();
    let all: Vec<(String, String, String, Option<String>)> = phases
        .iter()
        .flat_map(|ph| {
            ph.get("items")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|it| {
                    let o = it.as_object()?;
                    Some((
                        o.get("id")
                            .map(super::layout::value_key)
                            .unwrap_or_default(),
                        o.get("text")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned(),
                        o.get("status")
                            .and_then(Value::as_str)
                            .unwrap_or("pending")
                            .to_owned(),
                        o.get("note").and_then(Value::as_str).map(str::to_owned),
                    ))
                })
        })
        .collect();
    let done = all
        .iter()
        .filter(|i| i.2 == "done" || i.2 == "dropped")
        .count();
    look.attr("aria-label", format!("Todos, {done} of {} done", all.len()));
    let prev = std::mem::take(&mut timers(m).done);
    let fresh: Vec<String> = all
        .iter()
        .filter(|i| i.2 == "done" && prev.get(&i.0).is_some_and(|s| s != "done"))
        .map(|i| i.0.clone())
        .collect();
    timers(m).done = all.iter().map(|i| (i.0.clone(), i.2.clone())).collect();
    let item = |cx: &mut Cx, parent: NodeId, it: &(String, String, String, Option<String>)| {
        let mut class = format!("sf-ck-item st-{}", it.2);
        if fresh.contains(&it.0) && !cx.set.reduce_motion {
            class.push_str(" fresh");
        }
        let ie = cx.dom.child(parent, "div", &class);
        let g = cx.dom.child(ie, "span", "sf-ck-g");
        match it.2.as_str() {
            "done" => {
                icon(cx, g, "check");
            }
            "dropped" => {
                icon(cx, g, "minus");
            }
            "blocked" => {
                icon(cx, g, "warn");
            }
            _ => {
                cx.dom.child(g, "i", "dot");
            }
        }
        cx.dom.child_text(ie, "span", "sf-ck-t", &it.1);
        if let Some(n) = &it.3 {
            cx.dom.child_text(ie, "div", "sf-ck-note", n);
        }
    };
    let id = node.id().to_owned();
    let full_list = |cx: &mut Cx, parent: NodeId| {
        for ph in &phases {
            let pid = ph
                .get("id")
                .map(super::layout::value_key)
                .unwrap_or_default();
            let items: Vec<(String, String, String, Option<String>)> = ph
                .get("items")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|it| {
                    let o = it.as_object()?;
                    Some((
                        o.get("id")
                            .map(super::layout::value_key)
                            .unwrap_or_default(),
                        o.get("text")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned(),
                        o.get("status")
                            .and_then(Value::as_str)
                            .unwrap_or("pending")
                            .to_owned(),
                        o.get("note").and_then(Value::as_str).map(str::to_owned),
                    ))
                })
                .collect();
            let pdone = items
                .iter()
                .filter(|i| i.2 == "done" || i.2 == "dropped")
                .count();
            let key = format!("{id}\u{1f}phase\u{1f}{pid}");
            let folded =
                SurfaceState::flip(&mut cx.st.folds, &key, ph.get("collapsed"), false) || collapsed;
            let mut class = String::from("sf-ck-phase");
            if pdone == items.len() && !items.is_empty() {
                class.push_str(" done");
            }
            if folded {
                class.push_str(" folded");
            }
            let pe = cx.dom.child(parent, "div", &class);
            let ph_head = cx.dom.child(pe, "div", "sf-ck-ph");
            cx.dom.attr(ph_head, "role", "button");
            cx.dom.attr(ph_head, "tabindex", "0");
            cx.dom.attr(
                ph_head,
                "aria-expanded",
                if folded { "false" } else { "true" },
            );
            cx.dom.attr(ph_head, "data-phase", &pid);
            ring(cx, ph_head, pdone, items.len());
            cx.dom.child_text(
                ph_head,
                "span",
                "t",
                ph.get("title").and_then(Value::as_str).unwrap_or(""),
            );
            cx.dom
                .child_text(ph_head, "span", "n", &format!("{pdone}/{}", items.len()));
            let list = cx.dom.child(
                pe,
                "div",
                if folded {
                    "sf-ck-items sf-hidden"
                } else {
                    "sf-ck-items"
                },
            );
            for it in &items {
                item(cx, list, it);
            }
        }
    };
    match mode {
        "hud" => {
            let open = cx.st.hud.get(&id).copied().unwrap_or(false);
            look.class_if(open, "open");
            let pill = part(cx, m, "button", "sf-ck-pill");
            cx.dom
                .attr(pill, "aria-expanded", if open { "true" } else { "false" });
            cx.dom.attr(pill, "data-hud", "1");
            ring(cx, pill, done, all.len());
            let current = all
                .iter()
                .find(|i| i.2 == "active")
                .or_else(|| all.iter().find(|i| i.2 == "pending"))
                .map(|i| i.1.clone())
                .unwrap_or_else(|| "All done".to_owned());
            cx.dom.child_text(pill, "span", "t", &current);
            cx.dom
                .child_text(pill, "span", "n", &format!("{done}/{}", all.len()));
            let pop = part(
                cx,
                m,
                "div",
                if open {
                    "sf-ck-pop"
                } else {
                    "sf-ck-pop sf-hidden"
                },
            );
            if open {
                full_list(cx, pop);
            }
        }
        "reminder" => {
            let key = format!("{id}\u{1f}reminder");
            let open = cx.st.more.get(&key).copied().unwrap_or(false);
            look.class_if(open, "open");
            let rh = part(cx, m, "div", "sf-ck-rh");
            cx.dom.attr(rh, "role", "button");
            cx.dom.attr(rh, "tabindex", "0");
            cx.dom.attr(rh, "data-reminder", "1");
            let ic = cx.dom.child(rh, "span", "ic");
            icon(cx, ic, "warn");
            let open_items: Vec<_> = all
                .iter()
                .filter(|i| i.2 != "done" && i.2 != "dropped")
                .collect();
            cx.dom.child_text(
                rh,
                "span",
                "t",
                &format!(
                    "{} todo{} still open",
                    open_items.len(),
                    if open_items.len() == 1 { "" } else { "s" }
                ),
            );
            if let Some(note) = str_prop(p, "note") {
                cx.dom.child_text(rh, "span", "n", note);
            }
            let c = cx.dom.child(rh, "span", "sf-chev");
            icon(cx, c, if open { "chev" } else { "chev-r" });
            let list = part(
                cx,
                m,
                "div",
                if open {
                    "sf-ck-items"
                } else {
                    "sf-ck-items sf-hidden"
                },
            );
            for it in open_items {
                item(cx, list, it);
            }
        }
        _ => {
            let holder = m.el;
            let before: Vec<NodeId> = cx.dom.doc.children(holder).to_vec();
            full_list(cx, holder);
            let after = cx.dom.doc.children(holder).to_vec();
            m.generated
                .extend(after.into_iter().filter(|c| !before.contains(c)));
            if let Some(n) = p.get("note").filter(|n| !n.is_null()) {
                let ne = part(cx, m, "div", "sf-ck-foot");
                spans_into(cx, ne, Some(n));
            }
        }
    }
    let _ = spans::parse_spans;
    let _ = state_key;
}
