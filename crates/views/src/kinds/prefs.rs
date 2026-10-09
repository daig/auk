//! `prefs` (T2, amendment A23): the kit Preferences page drawn from the program's data (book:
//! `prefs`; research/elements.md §2.4, §6.4). In `main` it is a page (a nav column, the scrolling
//! document and Close; an icon rail below 760px); in `layer` it is a sheet docked at the right
//! edge, and in an inline surface the pane narrows beside it (feature `aside`). The program owns
//! every key: focus, menus and text drafts come as props (`focus`, `editing`); pointer actions
//! go back as `action`, `change`, `activate`, `select` and `focus` events.
//!
//! The page keeps its skeleton (`.pf`, the nav, `.pf-scroll`) across renders, so the document's
//! scroll survives the program's updates; a page or query change scrolls to the top and replays
//! the arrive motion, and a switch keeps its element so it slides.

use super::{begin, icon, slot};
use crate::common::{bool_prop, fmt_num, num_prop, str_prop};
use crate::dom::Dom;
use crate::events::Outcome;
use crate::view::{Cx, Look, Mount, PaneView};
use auk_tsp::{NodeRef, Surface, TspPane, ViewEvent};
use auk_ui::{Document, NodeId};
use serde_json::{Map, Value};
use std::collections::HashMap;

/// Child slots: anything else (`.pf-extra`), the editor card (`.pf-editor`), the status preview.
const EXTRA: usize = 0;
const EDITOR: usize = 1;
const PREVIEW: usize = 2;

/// Where the role of a child sends it (book: Children: placed by role).
pub(crate) fn route(child: NodeRef) -> usize {
    match str_prop(child.props(), "role") {
        Some("omp.prefs.preview.status") => PREVIEW,
        Some("omp.prefs.editor") => EDITOR,
        _ if child.kind() == "picker" => EDITOR,
        _ => EXTRA,
    }
}

/// What a `prefs` node keeps between renders.
#[derive(Debug, Default)]
pub(crate) struct Memo {
    /// `.pf`, `nav.pf-nav`, `.pf-scroll`, `.pf-doc` and `.pf-top`.
    skeleton: Option<[NodeId; 5]>,
    /// Content built inside the skeleton by the last render.
    inner: Vec<NodeId>,
    page: String,
    query: String,
    focus: Option<String>,
    /// The row whose menu was open.
    menu: Option<String>,
    /// Switch buttons by row (and ordered item), kept so a flip slides.
    switches: HashMap<String, NodeId>,
    /// The menu option the pointer last previewed (`row=value`).
    pub hovered: Option<String>,
}

/// The node's place: a sheet in `layer`, else a page.
fn in_layer(node: NodeRef) -> bool {
    std::iter::successors(node.parent(), |n| n.parent())
        .any(|n| n.id() == "layer" && n.parent().is_some_and(|root| root.parent().is_none()))
}

fn obj(v: &Value) -> Option<&Map<String, Value>> {
    v.as_object()
}

fn arr<'a>(p: &'a Map<String, Value>, k: &str) -> &'a [Value] {
    p.get(k)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

fn s<'a>(p: &'a Map<String, Value>, k: &str) -> Option<&'a str> {
    str_prop(p, k).filter(|v| !v.is_empty())
}

/// The `editing` prop: `(row, option, draft, cursor)`.
struct Editing<'a> {
    row: &'a str,
    option: Option<&'a str>,
    draft: Option<&'a str>,
    cursor: Option<u64>,
}

fn editing(p: &Map<String, Value>) -> Option<Editing<'_>> {
    let e = p.get("editing").and_then(obj)?;
    Some(Editing {
        row: str_prop(e, "row")?,
        option: str_prop(e, "option"),
        draft: str_prop(e, "draft"),
        cursor: num_prop(e, "cursor").map(|c| c.max(0.0) as u64),
    })
}

/// A choice control draws as segments: `segmented`, or `auto` with 2–4 options whose labels
/// total 28 characters or fewer.
fn segmented(ctl: &Map<String, Value>) -> bool {
    let opts = arr(ctl, "options");
    match str_prop(ctl, "style").unwrap_or("auto") {
        "segmented" => true,
        "menu" => false,
        _ => {
            (2..=4).contains(&opts.len())
                && opts
                    .iter()
                    .filter_map(obj)
                    .map(|o| option_label(o).chars().count())
                    .sum::<usize>()
                    <= 28
        }
    }
}

fn option_label(o: &Map<String, Value>) -> &str {
    s(o, "label").or(str_prop(o, "value")).unwrap_or("")
}

/// A number control's next value up (`up`) or down: the next key of `labels` when given, else
/// `value ± step` clamped to `min`/`max`. `None`: nowhere to go.
pub(crate) fn step(ctl: &Map<String, Value>, up: bool) -> Option<f64> {
    let value = num_prop(ctl, "value").unwrap_or(0.0);
    if let Some(labels) = ctl.get("labels").and_then(obj) {
        let mut keys: Vec<f64> = labels.keys().filter_map(|k| k.parse().ok()).collect();
        keys.sort_by(f64::total_cmp);
        return if up {
            keys.into_iter().find(|k| *k > value)
        } else {
            keys.into_iter().rev().find(|k| *k < value)
        };
    }
    let st = num_prop(ctl, "step").filter(|s| *s > 0.0).unwrap_or(1.0);
    let mut next = if up { value + st } else { value - st };
    if let Some(max) = num_prop(ctl, "max") {
        next = next.min(max);
    }
    if let Some(min) = num_prop(ctl, "min") {
        next = next.max(min);
    }
    ((next - value).abs() > f64::EPSILON && (next > value) == up).then_some(next)
}

/// A number control's shown value: its `labels` entry, else the number and unit.
fn number_text(ctl: &Map<String, Value>) -> String {
    let value = num_prop(ctl, "value").unwrap_or(0.0);
    let key = fmt_num(value);
    if let Some(l) = ctl
        .get("labels")
        .and_then(obj)
        .and_then(|l| l.get(&key))
        .and_then(Value::as_str)
    {
        return l.to_owned();
    }
    match s(ctl, "unit") {
        Some(u) => format!("{key} {u}"),
        None => key,
    }
}

/// A multi control's enabled values, in order.
fn multi_values(ctl: &Map<String, Value>) -> Vec<String> {
    arr(ctl, "values")
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

/// The values after flipping `v`: removed when on; else added (at the end of an ordered list,
/// in option order otherwise).
pub(crate) fn multi_flip(ctl: &Map<String, Value>, v: &str) -> Vec<String> {
    let mut vals = multi_values(ctl);
    if let Some(i) = vals.iter().position(|x| x == v) {
        vals.remove(i);
        return vals;
    }
    vals.push(v.to_owned());
    if !bool_prop(ctl, "ordered") {
        let order: Vec<&str> = arr(ctl, "options")
            .iter()
            .filter_map(obj)
            .filter_map(|o| str_prop(o, "value"))
            .collect();
        vals.sort_by_key(|x| order.iter().position(|o| o == x).unwrap_or(usize::MAX));
    }
    vals
}

/// The words of a search query (for `<mark>`).
fn words(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .map(str::to_lowercase)
        .filter(|w| !w.is_empty())
        .collect()
}

/// `text` into `parent`, with the query's words in `<mark>`.
fn marked(dom: &mut Dom, parent: NodeId, text: &str, words: &[String]) {
    let lower = text.to_lowercase();
    if words.is_empty() || lower.len() != text.len() {
        dom.text(parent, text);
        return;
    }
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for w in words {
        let mut from = 0;
        while let Some(i) = lower[from..].find(w.as_str()) {
            let a = from + i;
            ranges.push((a, a + w.len()));
            from = a + w.len().max(1);
        }
    }
    ranges.sort();
    let mut at = 0;
    for (a, b) in ranges {
        if a < at || !text.is_char_boundary(a) || !text.is_char_boundary(b) {
            continue;
        }
        if a > at {
            dom.text(parent, &text[at..a]);
        }
        dom.child_text(parent, "mark", "", &text[a..b]);
        at = b;
    }
    if at < text.len() {
        dom.text(parent, &text[at..]);
    }
}

/// A switch button: the kept one for `key` (so a flip slides), else a new one.
fn switch(
    dom: &mut Dom,
    parent: NodeId,
    kept: &mut HashMap<String, NodeId>,
    next: &mut HashMap<String, NodeId>,
    key: &str,
    on: bool,
    small: bool,
) -> NodeId {
    let class = format!(
        "pf-sw{}{}",
        if on { " on" } else { "" },
        if small { " sm" } else { "" }
    );
    let b = match kept.remove(key).filter(|&b| dom.doc.try_node(b).is_some()) {
        Some(b) => {
            dom.set_class(b, &class);
            dom.doc.append_child(parent, b);
            b
        }
        None => {
            let b = dom.child(parent, "button", &class);
            dom.child(b, "i", "knob");
            b
        }
    };
    dom.attr(b, "role", "switch");
    dom.attr(b, "aria-checked", if on { "true" } else { "false" });
    next.insert(key.to_owned(), b);
    b
}

pub(crate) fn prefs(cx: &mut Cx, node: NodeRef, m: &mut Mount, look: &mut Look) {
    begin(cx, m);
    look.title_is_content = true;
    let p = node.props();
    let id = node.id().to_owned();
    let sheet = in_layer(node);
    let rail = !sheet && f32::from(cx.cols) * cx.set.cell.0 < 760.0;
    let mut memo = cx.st.prefs.remove(&id).unwrap_or_default();
    // The skeleton, kept while it is still this node's.
    let fresh = memo.skeleton.is_none_or(|[root, ..]| {
        cx.dom.doc.try_node(root).is_none() || cx.dom.doc.parent(root) != Some(m.el)
    });
    if fresh {
        let root = cx.dom.child(m.el, "div", "pf on");
        let nav = cx.dom.child(root, "nav", "pf-nav");
        let scroll = cx.dom.child(root, "div", "pf-scroll");
        let doc = cx.dom.child(scroll, "div", "pf-doc arrive");
        let top = cx.dom.child(root, "div", "pf-top");
        memo = Memo {
            skeleton: Some([root, nav, scroll, doc, top]),
            ..Memo::default()
        };
    }
    let [root, nav, scroll, mut doc, top] = memo.skeleton.expect("built");
    // Switches leave before the old content goes, to be reused.
    let mut kept = std::mem::take(&mut memo.switches);
    for &b in kept.values() {
        if cx.dom.doc.try_node(b).is_some() {
            cx.dom.doc.detach(b);
        }
    }
    for n in std::mem::take(&mut memo.inner) {
        if cx.dom.doc.try_node(n).is_some() {
            cx.dom.doc.remove(n);
        }
    }
    let title = s(p, "title").unwrap_or("Settings");
    let page = str_prop(p, "page").unwrap_or("");
    let query = str_prop(p, "query").unwrap_or("").trim().to_owned();
    let searching = !query.is_empty();
    // A page or query change scrolls to the top and replays the arrive motion.
    if !fresh && (memo.page != page || memo.query != query) {
        cx.dom.doc.remove(doc);
        doc = cx.dom.child(scroll, "div", "pf-doc arrive");
        memo.skeleton = Some([root, nav, scroll, doc, top]);
        cx.dom.doc.set_scroll_offset(scroll, 0.0, 0.0);
    }
    let mut class = String::from("pf on");
    if rail {
        class.push_str(" rail");
    }
    if sheet {
        class.push_str(" sheet");
    }
    cx.dom.set_class(root, &class);
    cx.dom.attr(root, "aria-label", title);
    let pages: Vec<&Map<String, Value>> = arr(p, "pages").iter().filter_map(obj).collect();
    let mut inner = Vec::new();

    // Nav: brand, search, page groups.
    let brand = cx.dom.child(nav, "div", "pf-brand");
    inner.push(brand);
    let mark = cx.dom.child(brand, "span", "pf-mark");
    icon(cx, mark, "settings");
    cx.dom.child_text(brand, "span", "pf-brand-t", title);
    let close = cx.dom.child(brand, "button", "icon-btn pf-close");
    cx.dom.attr(close, "data-pf", "close");
    cx.dom.attr(close, "title", "Close");
    icon(cx, close, "x");
    let search = cx.dom.child(nav, "div", "pf-search");
    inner.push(search);
    icon(cx, search, "search");
    let has_cursor = p.get("cursor").and_then(Value::as_f64).is_some();
    let raw_query = str_prop(p, "query").unwrap_or("");
    if has_cursor {
        let q = cx.dom.child(search, "span", "q");
        let c = num_prop(p, "cursor").unwrap_or(0.0).max(0.0) as u64;
        let at = super::input::utf16_to_byte(raw_query, c);
        cx.dom.child_text(q, "span", "pre", &raw_query[..at]);
        cx.dom.child(q, "span", "pf-caret");
        cx.dom.child_text(q, "span", "post", &raw_query[at..]);
    } else if searching {
        let q = cx.dom.child(search, "span", "q");
        cx.dom.child_text(q, "span", "pre", &query);
    } else {
        cx.dom.child_text(search, "span", "q ph", "Search settings");
    }
    let groups = cx.dom.child(nav, "div", "pf-groups");
    inner.push(groups);
    let mut items: Option<NodeId> = None;
    let mut group_name: Option<Option<&str>> = None;
    for pg in &pages {
        let Some(pid) = str_prop(pg, "id") else {
            continue;
        };
        let g = s(pg, "group");
        if group_name != Some(g) {
            group_name = Some(g);
            let ng = cx.dom.child(groups, "div", "nav-group pf-group");
            if let Some(g) = g {
                cx.dom.child_text(ng, "div", "nav-head", g);
            }
            items = Some(cx.dom.child(ng, "div", "pf-items"));
        }
        let off = s(pg, "disabled");
        let on = pid == page && !searching;
        let row = cx.dom.child(
            items.expect("a group"),
            "button",
            &format!(
                "nav-row{}{}",
                if on { " on" } else { "" },
                if off.is_some() { " off" } else { "" }
            ),
        );
        cx.dom.attr(row, "data-v", pid);
        cx.dom.attr(row, "data-pf", "page");
        let label = s(pg, "label").unwrap_or(pid);
        cx.dom.attr(row, "title", off.unwrap_or(label));
        if off.is_some() {
            cx.dom.attr(row, "data-off", "");
        }
        icon(cx, row, s(pg, "icon").unwrap_or("sliders"));
        cx.dom.child_text(row, "span", "lbl", label);
        if let Some(n) = num_prop(pg, "changed").filter(|n| *n > 0.0) {
            let c = cx
                .dom
                .child_text(row, "span", "meta pf-changed", &fmt_num(n));
            cx.dom
                .attr(c, "title", &format!("{} changed from default", fmt_num(n)));
        }
    }

    // The document.
    let words = words(&query);
    let ed = editing(p);
    let focus = s(p, "focus");
    let head = cx.dom.child(
        doc,
        "header",
        if searching {
            "pf-head pf-found"
        } else {
            "pf-head"
        },
    );
    inner.push(head);
    let page_index = pages.iter().position(|pg| str_prop(pg, "id") == Some(page));
    let page_label = page_index
        .and_then(|i| s(pages[i], "label").or(str_prop(pages[i], "id")))
        .unwrap_or(page);
    let sections: Vec<&Map<String, Value>> = arr(p, "sections").iter().filter_map(obj).collect();
    let row_count: usize = sections.iter().map(|sec| arr(sec, "rows").len()).sum();
    if searching {
        let n = row_count;
        cx.dom.child_text(
            head,
            "div",
            "pf-eyebrow mono",
            &format!("{n} {}", if n == 1 { "result" } else { "results" }),
        );
        cx.dom
            .child_text(head, "h1", "pf-title", &format!("\u{201c}{query}\u{201d}"));
    } else {
        let eyebrow = match page_index {
            Some(i) => format!("{:02} \u{b7} {page_label}", i + 1),
            None => page_label.to_owned(),
        };
        cx.dom.child_text(head, "div", "pf-eyebrow mono", &eyebrow);
        cx.dom.child_text(head, "h1", "pf-title", page_label);
        if let Some(l) = s(p, "lead") {
            cx.dom.child_text(head, "p", "pf-lead", l);
        }
    }
    let mut targets: HashMap<String, NodeId> = HashMap::new();
    let mut switches = HashMap::new();
    let mut menu_open: Option<String> = None;
    let preview = node
        .children()
        .any(|c| route(c) == PREVIEW)
        .then(|| slot(cx, m, PREVIEW, "div", "pf-preview"));
    // Slots are created in order: make the first two exist too.
    let extra = slot(cx, m, EXTRA, "div", "pf-extra");
    let editor = slot(cx, m, EDITOR, "div", "pf-editor");
    let preview = preview.unwrap_or_else(|| slot(cx, m, PREVIEW, "div", "pf-preview"));
    if row_count == 0 {
        let none = if searching {
            format!("No settings match \u{201c}{query}\u{201d}")
        } else {
            "Nothing to set here".to_owned()
        };
        let e = cx.dom.child_text(doc, "div", "pf-none", &none);
        inner.push(e);
    }
    // Search results group by page under a page heading.
    let mut part_body: Option<(Option<&str>, NodeId)> = None;
    for sec in &sections {
        let rows = arr(sec, "rows");
        let sid = str_prop(sec, "id");
        if rows.is_empty() && sid != Some("status-line") {
            continue;
        }
        let parent = if searching {
            let sp = str_prop(sec, "page");
            match part_body {
                Some((pg, b)) if pg == sp => b,
                _ => {
                    let part = cx.dom.child(doc, "div", "pf-part");
                    inner.push(part);
                    let info =
                        sp.and_then(|sp| pages.iter().find(|pg| str_prop(pg, "id") == Some(sp)));
                    let off = info.and_then(|pg| s(pg, "disabled"));
                    let h = cx.dom.child(
                        part,
                        "div",
                        if off.is_some() {
                            "pf-part-h off"
                        } else {
                            "pf-part-h"
                        },
                    );
                    if let Some(sp) = sp {
                        cx.dom.attr(h, "data-pf", "page");
                        cx.dom.attr(h, "data-v", sp);
                        icon(
                            cx,
                            h,
                            info.and_then(|pg| s(pg, "icon")).unwrap_or("sliders"),
                        );
                        let label = info.and_then(|pg| s(pg, "label")).unwrap_or(sp);
                        cx.dom.child_text(h, "span", "lbl", label);
                    }
                    if let Some(off) = off {
                        cx.dom.attr(h, "title", off);
                        cx.dom.attr(h, "data-off", "");
                    }
                    let b = cx.dom.child(part, "div", "pf-part-b");
                    part_body = Some((sp, b));
                    b
                }
            }
        } else {
            doc
        };
        let section = cx.dom.child(
            parent,
            "section",
            if sid.is_some() && sid == focus {
                "pf-sec focus"
            } else {
                "pf-sec"
            },
        );
        if !searching {
            inner.push(section);
        }
        if let Some(sid) = sid {
            cx.dom.attr(section, "data-sec", sid);
            targets.insert(sid.to_owned(), section);
        }
        if let Some(t) = s(sec, "title") {
            cx.dom.child_text(section, "h2", "pf-sec-t", t);
        }
        if !rows.is_empty() {
            let list = cx.dom.child(section, "div", "pf-rows");
            for r in rows.iter().filter_map(obj) {
                let Some(rid) = str_prop(r, "id") else {
                    continue;
                };
                let el = draw_row(
                    cx,
                    list,
                    r,
                    rid,
                    ed.as_ref().filter(|e| e.row == rid),
                    focus == Some(rid),
                    &words,
                    &mut kept,
                    &mut switches,
                    &mut menu_open,
                    memo.menu.as_deref() == Some(rid),
                );
                targets.insert(rid.to_owned(), el);
            }
        }
        if sid == Some("status-line") {
            cx.dom.doc.append_child(section, preview);
        }
    }
    cx.dom.doc.append_child(doc, extra);
    // The editor card: a glass card over the page while it holds something.
    let has_editor = node.children().any(|c| route(c) == EDITOR);
    cx.dom.set_class(
        editor,
        if has_editor {
            "pf-editor on"
        } else {
            "pf-editor"
        },
    );
    cx.dom.doc.append_child(root, editor);
    // The page's Close.
    if !sheet {
        let b = cx.dom.child(top, "button", "btn sm quiet");
        inner.push(b);
        cx.dom.attr(b, "data-pf", "close");
        cx.dom.child_text(b, "span", "kbd", "esc");
        cx.dom.text(b, "Close");
    }
    // The focus ring scrolls into view when it moves: sections to the top, rows to the
    // nearest edge.
    let focus_owned = focus.map(str::to_owned);
    if focus_owned != memo.focus
        && let Some(f) = &focus_owned
        && let Some(&el) = targets.get(f)
    {
        let top_align = sections
            .iter()
            .any(|sec| str_prop(sec, "id") == Some(f.as_str()));
        cx.st.reveal_els.push((scroll, el, top_align));
    }
    for (_, b) in kept {
        if cx.dom.doc.try_node(b).is_some() {
            cx.dom.doc.remove(b);
        }
    }
    memo.inner = inner;
    memo.switches = switches;
    memo.page = page.to_owned();
    memo.query = query;
    memo.focus = focus_owned;
    if menu_open.is_none() {
        memo.hovered = None;
    }
    memo.menu = menu_open;
    cx.st.prefs.insert(id, memo);
}

#[allow(clippy::too_many_arguments)]
fn draw_row(
    cx: &mut Cx,
    list: NodeId,
    r: &Map<String, Value>,
    rid: &str,
    ed: Option<&Editing>,
    focus: bool,
    words: &[String],
    kept: &mut HashMap<String, NodeId>,
    switches: &mut HashMap<String, NodeId>,
    menu_open: &mut Option<String>,
    menu_was_open: bool,
) -> NodeId {
    let empty = Map::new();
    let ctl = r.get("control").and_then(obj).unwrap_or(&empty);
    let k = str_prop(ctl, "k").unwrap_or("");
    let changed = bool_prop(r, "changed");
    let warning = s(r, "warning");
    let disabled = s(r, "disabled");
    let mut class = String::from("pf-row");
    for (on, c) in [
        (changed, " changed"),
        (warning.is_some(), " warn"),
        (disabled.is_some(), " off"),
        (ed.is_some(), " editing"),
        (k == "multi", " stack"),
        (focus, " focus"),
        (k == "keys", " pf-key"),
    ] {
        if on {
            class.push_str(c);
        }
    }
    let row = cx.dom.child(list, "div", &class);
    cx.dom.attr(row, "data-row", rid);
    cx.dom.attr(row, "data-pf", "row");
    if let Some(d) = disabled {
        cx.dom.attr(row, "title", d);
    }
    let dot = cx.dom.child(row, "span", "pf-dot");
    if changed {
        let tip = match s(r, "defaultLabel") {
            Some(d) => format!("Changed from default: {d}"),
            None => "Changed from default".to_owned(),
        };
        cx.dom.attr(dot, "title", &tip);
    }
    let lbl = cx.dom.child(row, "div", "pf-lbl");
    let t = cx.dom.child(lbl, "div", "t");
    if warning.is_some() {
        let w = cx.dom.child(t, "span", "pf-warn-ic");
        icon(cx, w, "warn");
    }
    marked(&mut cx.dom, t, s(r, "label").unwrap_or(rid), words);
    if let Some(h) = s(r, "hint") {
        let hd = cx.dom.child(lbl, "div", "h");
        marked(&mut cx.dom, hd, h, words);
    }
    if let Some(w) = warning {
        cx.dom.child_text(lbl, "div", "pf-warn", w);
    }
    if let Some(d) = disabled {
        cx.dom.child_text(lbl, "div", "pf-why", d);
    }
    let c = cx.dom.child(row, "div", "pf-ctl");
    if changed && disabled.is_none() && !matches!(k, "action" | "keys") {
        let b = cx.dom.child_text(c, "button", "pf-reset", "Reset");
        cx.dom.attr(b, "data-pf", "reset");
        cx.dom.attr(b, "title", "Reset to default");
    }
    match k {
        "switch" => {
            let b = switch(
                &mut cx.dom,
                c,
                kept,
                switches,
                rid,
                bool_prop(ctl, "on"),
                false,
            );
            cx.dom.attr(b, "data-pf", "sw");
        }
        "choice" => {
            let value = str_prop(ctl, "value").unwrap_or("");
            let opts: Vec<&Map<String, Value>> =
                arr(ctl, "options").iter().filter_map(obj).collect();
            let mono = bool_prop(ctl, "mono");
            if segmented(ctl) {
                let seg = cx
                    .dom
                    .child(c, "div", if mono { "pf-seg mono" } else { "pf-seg" });
                for o in &opts {
                    let v = str_prop(o, "value").unwrap_or("");
                    let b = cx.dom.child_text(
                        seg,
                        "button",
                        if v == value { "on" } else { "" },
                        option_label(o),
                    );
                    cx.dom.attr(b, "data-pf", "seg");
                    cx.dom.attr(b, "data-v", v);
                    if let Some(d) = s(o, "detail") {
                        cx.dom.attr(b, "title", d);
                    }
                }
            } else {
                let pop = cx.dom.child(c, "div", "pf-pop");
                let cur = opts.iter().find(|o| str_prop(o, "value") == Some(value));
                let pick =
                    cx.dom
                        .child(pop, "button", if mono { "pf-pick mono" } else { "pf-pick" });
                cx.dom.attr(pick, "data-pf", "pick");
                cx.dom
                    .child_text(pick, "span", "lbl", cur.map_or(value, |o| option_label(o)));
                icon(cx, pick, "chev");
                if let Some(e) = ed {
                    *menu_open = Some(rid.to_owned());
                    let sel = e.option.unwrap_or(value);
                    let menu = cx.dom.child(
                        pop,
                        "div",
                        if menu_was_open {
                            "pf-menu still"
                        } else {
                            "pf-menu"
                        },
                    );
                    cx.dom.set_style(menu, "--pf-room: 360px");
                    cx.dom.attr(menu, "role", "listbox");
                    for o in &opts {
                        let v = str_prop(o, "value").unwrap_or("");
                        let d = s(o, "detail");
                        let mut mc = String::from("mi");
                        if v == value {
                            mc.push_str(" cur");
                        }
                        if v == sel {
                            mc.push_str(" sel");
                        }
                        if d.is_some() {
                            mc.push_str(" two");
                        }
                        let mi = cx.dom.child(menu, "div", &mc);
                        cx.dom.attr(mi, "data-pf", "mi");
                        cx.dom.attr(mi, "data-v", v);
                        cx.dom.attr(mi, "role", "option");
                        let chk = cx.dom.child(mi, "span", "pf-check");
                        if v == value {
                            icon(cx, chk, "check");
                        }
                        let ml = cx.dom.child(mi, "span", "pf-ml");
                        cx.dom.child_text(ml, "span", "t", option_label(o));
                        if let Some(d) = d {
                            cx.dom.child_text(ml, "span", "d", d);
                        }
                    }
                }
            }
        }
        "number" => {
            let st = cx.dom.child(c, "div", "pf-step");
            for (up, glyph) in [(false, "\u{2212}"), (true, "+")] {
                if up {
                    cx.dom.child_text(st, "span", "v", &number_text(ctl));
                }
                let next = step(ctl, up);
                let b = cx.dom.child_text(
                    st,
                    "button",
                    if next.is_some() {
                        "pf-stepb"
                    } else {
                        "pf-stepb dis"
                    },
                    glyph,
                );
                cx.dom.attr(b, "data-pf", if up { "up" } else { "down" });
                if next.is_none() {
                    cx.dom.attr(b, "disabled", "");
                }
            }
        }
        "text" => {
            let mut class = String::from("pf-in");
            if bool_prop(ctl, "mono") {
                class.push_str(" mono");
            }
            if ed.is_some() {
                class.push_str(" editing");
            }
            let secret = bool_prop(ctl, "secret");
            if secret {
                class.push_str(" secret");
            }
            let f = cx.dom.child(c, "div", &class);
            cx.dom.attr(f, "data-pf", "in");
            let dots = |t: &str| "\u{2022}".repeat(t.chars().count());
            match ed {
                Some(e) => {
                    let draft = e.draft.unwrap_or("");
                    let at = super::input::utf16_to_byte(
                        draft,
                        e.cursor.unwrap_or(draft.encode_utf16().count() as u64),
                    );
                    let v = cx.dom.child(f, "span", "v");
                    let (pre, post) = (&draft[..at], &draft[at..]);
                    let (pre, post) = if secret {
                        (dots(pre), dots(post))
                    } else {
                        (pre.to_owned(), post.to_owned())
                    };
                    cx.dom.child_text(v, "span", "pre", &pre);
                    cx.dom.child(v, "span", "pf-caret");
                    cx.dom.child_text(v, "span", "post", &post);
                }
                None => match str_prop(ctl, "value").filter(|v| !v.is_empty()) {
                    Some(v) => {
                        let shown = if secret { dots(v) } else { v.to_owned() };
                        cx.dom.child_text(f, "span", "v", &shown);
                    }
                    None => {
                        cx.dom.child_text(
                            f,
                            "span",
                            "ph",
                            s(ctl, "placeholder").unwrap_or("Not set"),
                        );
                    }
                },
            }
        }
        "keys" => {
            for chord in arr(ctl, "keys").iter().filter_map(Value::as_array) {
                let ch = cx.dom.child(c, "span", "pf-chord");
                for key in chord.iter().filter_map(Value::as_str) {
                    let (cap, spoken) = crate::icons::keycap(key);
                    let kb = cx.dom.child_text(ch, "span", "kbd", &cap);
                    if let Some(sp) = spoken {
                        cx.dom.attr(kb, "aria-label", sp);
                    }
                }
            }
        }
        "multi" => {
            let vals = multi_values(ctl);
            let sel = ed.and_then(|e| e.option);
            let opts: Vec<&Map<String, Value>> =
                arr(ctl, "options").iter().filter_map(obj).collect();
            if bool_prop(ctl, "ordered") {
                let list = cx.dom.child(c, "div", "pf-order");
                let mut order: Vec<&Map<String, Value>> = Vec::new();
                for v in &vals {
                    if let Some(o) = opts.iter().find(|o| str_prop(o, "value") == Some(v)) {
                        order.push(o);
                    }
                }
                for o in &opts {
                    if !vals
                        .iter()
                        .any(|v| Some(v.as_str()) == str_prop(o, "value"))
                    {
                        order.push(o);
                    }
                }
                for o in order {
                    let v = str_prop(o, "value").unwrap_or("");
                    let pos = vals.iter().position(|x| x == v);
                    let mut ic = String::from("pf-oi");
                    if pos.is_some() {
                        ic.push_str(" on");
                    }
                    if sel == Some(v) {
                        ic.push_str(" sel");
                    }
                    let it = cx.dom.child(list, "div", &ic);
                    cx.dom.attr(it, "data-v", v);
                    let g = cx.dom.child(it, "span", "pf-grip");
                    icon(cx, g, "grip");
                    cx.dom.child_text(
                        it,
                        "span",
                        "n",
                        &pos.map(|p| (p + 1).to_string()).unwrap_or_default(),
                    );
                    let lb = cx.dom.child_text(it, "span", "t", option_label(o));
                    if let Some(d) = s(o, "detail") {
                        cx.dom.attr(lb, "title", d);
                    }
                    let b = switch(
                        &mut cx.dom,
                        it,
                        kept,
                        switches,
                        &format!("{rid}\u{1f}{v}"),
                        pos.is_some(),
                        true,
                    );
                    cx.dom.attr(b, "data-pf", "chip");
                    cx.dom.attr(b, "data-v", v);
                }
            } else {
                let chips = cx.dom.child(c, "div", "pf-chips");
                for o in &opts {
                    let v = str_prop(o, "value").unwrap_or("");
                    let mut cc = String::from("pf-chip");
                    if vals.iter().any(|x| x == v) {
                        cc.push_str(" on");
                    }
                    if sel == Some(v) {
                        cc.push_str(" sel");
                    }
                    let b = cx.dom.child_text(chips, "button", &cc, option_label(o));
                    cx.dom.attr(b, "data-pf", "chip");
                    cx.dom.attr(b, "data-v", v);
                    if let Some(d) = s(o, "detail") {
                        cx.dom.attr(b, "title", d);
                    }
                }
            }
        }
        "action" => {
            let b = cx.dom.child_text(
                c,
                "button",
                "btn sm pf-act",
                s(ctl, "label").unwrap_or("Edit\u{2026}"),
            );
            cx.dom.attr(b, "data-pf", "act");
            cx.dom.attr(b, "data-v", s(ctl, "act").unwrap_or("edit"));
        }
        _ => {}
    }
    row
}

// ---------------------------------------------------------------------------------------------
// Pointer events

/// A press on a `prefs` node's parts (`parts`: the elements from the hit up to the node's).
/// `None` when the press isn't on the page (a child's own content).
pub(crate) fn click(
    doc: &Document,
    surface: &Surface,
    node: NodeRef,
    parts: &[NodeId],
) -> Option<Vec<ViewEvent>> {
    let p = node.props();
    let id = node.id().to_owned();
    let attr = |el: NodeId, k: &str| doc.attribute(el, k).map(str::to_owned);
    // The innermost part that acts, and the row it is in.
    let act = parts
        .iter()
        .find_map(|&el| attr(el, "data-pf").map(|a| (el, a)));
    let row_id = parts.iter().find_map(|&el| attr(el, "data-row"));
    let row = row_id.as_deref().and_then(|rid| find_row(p, rid));
    let mut out = Vec::new();
    // The keys are in a field outside the page: ask for them first.
    if let Some(f) = surface.document().focus()
        && f != id
        && !std::iter::successors(surface.document().get(f), |n| n.parent()).any(|n| n.id() == id)
    {
        out.push(ViewEvent::Focus { id: id.clone() });
    }
    let action = |a: String| ViewEvent::Action {
        id: id.clone(),
        action: a,
        mods: Default::default(),
        values: None,
    };
    let change = |item: &str, value: Value| ViewEvent::PrefsChange {
        id: id.clone(),
        item: item.to_owned(),
        value,
    };
    // An open menu closes on a press anywhere outside it (its button included).
    if let Some(open) = editing(p)
        .filter(|e| {
            find_row(p, e.row).is_some_and(|r| {
                let ctl = r.get("control").and_then(obj);
                ctl.is_some_and(|c| str_prop(c, "k") == Some("choice") && !segmented(c))
            })
        })
        .map(|e| e.row.to_owned())
    {
        let on_menu = act.as_ref().is_some_and(|(_, a)| a == "mi")
            && row_id.as_deref() == Some(open.as_str());
        if !on_menu {
            out.push(action(format!("close={open}")));
            return Some(out);
        }
    }
    let (el, a) = act?;
    let v = attr(el, "data-v");
    let ev = match a.as_str() {
        "page" => {
            if doc.attribute(el, "data-off").is_some() {
                return Some(Vec::new());
            }
            action(format!("page={}", v?))
        }
        "close" => action("close".to_owned()),
        _ => {
            let (rid, r) = (row_id.as_deref()?, row?);
            if s(r, "disabled").is_some() {
                return Some(Vec::new());
            }
            let empty = Map::new();
            let ctl = r.get("control").and_then(obj).unwrap_or(&empty);
            match a.as_str() {
                "reset" => change(rid, Value::Null),
                "sw" => change(rid, Value::Bool(!bool_prop(ctl, "on"))),
                "seg" | "mi" => change(rid, Value::String(v?)),
                "pick" | "in" => ViewEvent::Activate {
                    id: id.clone(),
                    item: rid.to_owned(),
                    values: None,
                },
                "up" | "down" => {
                    let n = step(ctl, a == "up")?;
                    let value = if n.fract() == 0.0 && n.abs() < 9.0e15 {
                        Value::from(n as i64)
                    } else {
                        Value::from(n)
                    };
                    change(rid, value)
                }
                "chip" => change(
                    rid,
                    Value::Array(
                        multi_flip(ctl, &v?)
                            .into_iter()
                            .map(Value::String)
                            .collect(),
                    ),
                ),
                "act" => action(format!("{}={rid}", v.unwrap_or_else(|| "edit".to_owned()))),
                "row" => ViewEvent::Select {
                    id: id.clone(),
                    item: rid.to_owned(),
                    values: None,
                },
                _ => return None,
            }
        }
    };
    out.push(ev);
    Some(out)
}

fn find_row<'a>(p: &'a Map<String, Value>, rid: &str) -> Option<&'a Map<String, Value>> {
    arr(p, "sections")
        .iter()
        .filter_map(obj)
        .flat_map(|sec| arr(sec, "rows").iter().filter_map(obj))
        .find(|r| str_prop(r, "id") == Some(rid))
}

impl PaneView {
    /// Pointing at an open menu's option previews it: `select {item: "<row>=<value>"}`, once
    /// per option (book: `prefs` events). Hosts call this after [`PaneView::hover`].
    pub fn hover_events(&mut self, doc: &Document) -> Vec<Outcome> {
        let chain = self.hover.clone();
        let Some(mi) = chain
            .iter()
            .copied()
            .find(|&e| doc.attribute(e, "data-pf") == Some("mi"))
        else {
            return Vec::new();
        };
        let up = |e: NodeId, k: &str| {
            std::iter::successors(Some(e), |&x| doc.parent(x))
                .find_map(|x| doc.attribute(x, k).map(|v| (x, v.to_owned())))
        };
        let (Some(v), Some((_, row)), Some((_, scope))) = (
            doc.attribute(mi, "data-v").map(str::to_owned),
            up(mi, "data-row"),
            up(mi, "data-scope"),
        ) else {
            return Vec::new();
        };
        // The prefs node: the nearest node element above the row.
        let Some((_, node_id)) = up(mi, "data-id") else {
            return Vec::new();
        };
        let Some(sv) = self.surfaces.iter_mut().find(|s| s.scope == scope) else {
            return Vec::new();
        };
        let item = format!("{row}={v}");
        let memo = sv.st.prefs.entry(node_id.clone()).or_default();
        if memo.hovered.as_deref() == Some(item.as_str()) {
            return Vec::new();
        }
        memo.hovered = Some(item.clone());
        vec![Outcome::Event {
            surface: sv.key,
            sf: sv.id.clone(),
            event: Box::new(ViewEvent::Select {
                id: node_id,
                item,
                values: None,
            }),
        }]
    }

    /// The aside (feature `aside`): while a `prefs` node sits in an inline surface's `layer`,
    /// the pane's flow and docks end beside the sheet. `--tv-aside` on the pane is the sheet's
    /// width either way.
    pub(crate) fn place_aside(&mut self, doc: &mut Document, pane: &TspPane) {
        let reserve = auk_tsp::aside_reserve(self.cols);
        let width = f32::from(reserve) * self.settings.cell.0;
        let docked = pane.aside(false).is_some();
        let mut dom = Dom {
            doc,
            w: &mut self.written,
        };
        dom.set_style(
            self.container,
            &format!("--tv-aside: {}px;", fmt_num(f64::from(width))),
        );
        dom.set_class(
            self.container,
            if docked {
                "auk-pane auk-aside"
            } else {
                "auk-pane"
            },
        );
    }
}
