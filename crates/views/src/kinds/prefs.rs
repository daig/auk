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
use std::hash::{Hash, Hasher};

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
    /// The nav's content and the page's Close, built again every render.
    inner: Vec<NodeId>,
    /// The document's content (its head, sections or search parts, the "nothing" line), kept
    /// while the structure is the same.
    body: Vec<NodeId>,
    /// The document's structure as last built (book: Updates): page, query, sections, row ids,
    /// control kinds and options. While it holds, rows patch in place.
    structure: Option<u64>,
    /// Each row drawn: its element, its list and the signature of what it shows.
    rows: HashMap<String, (NodeId, NodeId, u64)>,
    /// Sections by id (for the focus ring), and the `status-line` one (the preview's place).
    sections: Vec<(String, NodeId)>,
    status_sec: Option<NodeId>,
    /// Focus targets: sections and rows by id.
    targets: HashMap<String, NodeId>,
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

/// The pane's width in the region's CSS px (its zoom, font/13, taken out).
fn pane_px(cx: &Cx) -> f32 {
    let zoom = (cx.set.font_px / 13.0).max(0.1);
    f32::from(cx.cols) * cx.set.cell.0 / zoom
}

/// An open menu's width budget (`--pf-room`, book: prefs Variables), so long options wrap inside
/// the row: what the row's control may take, from the page's width (`surface-prefs.css`: the
/// nav, the document's 760px and paddings, the label's room) or the sheet's (its control's 55%).
fn menu_room(cx: &Cx, sheet: bool) -> f32 {
    let zoom = (cx.set.font_px / 13.0).max(0.1);
    let room = if sheet {
        let sheet = f32::from(auk_tsp::aside_reserve(cx.cols)) * cx.set.cell.0 / zoom;
        (sheet - 28.0 - 28.0) * 0.55
    } else {
        let pane = pane_px(cx);
        let nav = if pane < 760.0 { 56.0 } else { 220.0 };
        let doc = (pane - nav).min(760.0) - 56.0;
        doc - 32.0 - 12.0 - 140.0
    };
    room.max(200.0)
}

/// Whether `n` is a picker or holds one.
fn holds_picker_node(n: NodeRef) -> bool {
    n.kind() == "picker" || n.children().any(holds_picker_node)
}

/// The node's place: a sheet in `layer`, else a page.
pub(crate) fn in_layer(node: NodeRef) -> bool {
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

/// `text` into `parent`, with the query's words in `<mark>`. Matching is on the lowercase text;
/// a character whose lowercase is longer or shorter (`İ`) maps back to itself whole.
fn marked(dom: &mut Dom, parent: NodeId, text: &str, words: &[String]) {
    if words.is_empty() {
        dom.text(parent, text);
        return;
    }
    // Each character's place in the text and in its lowercase: (text start, text end, lower
    // start, lower end).
    let mut lower = String::with_capacity(text.len());
    let mut map: Vec<(usize, usize, usize, usize)> = Vec::new();
    for (i, c) in text.char_indices() {
        let l0 = lower.len();
        lower.extend(c.to_lowercase());
        map.push((i, i + c.len_utf8(), l0, lower.len()));
    }
    let back = |a: usize, b: usize| -> Option<(usize, usize)> {
        let start = map.iter().find(|m| a >= m.2 && a < m.3)?.0;
        let end = map.iter().find(|m| b > m.2 && b <= m.3)?.1;
        Some((start, end))
    };
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for w in words {
        let mut from = 0;
        while let Some(i) = lower[from..].find(w.as_str()) {
            let a = from + i;
            if let Some(r) = back(a, a + w.len()) {
                ranges.push(r);
            }
            from = a + w.len().max(1);
        }
    }
    ranges.sort();
    let mut at = 0;
    for (a, b) in ranges {
        if a < at {
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
    // Below 760 CSS px (the region's zoom, font/13, taken out) the nav is a rail.
    let rail = !sheet && pane_px(cx) < 760.0;
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
    let title = s(p, "title").unwrap_or("Settings");
    let page = str_prop(p, "page").unwrap_or("");
    let query = str_prop(p, "query").unwrap_or("").trim().to_owned();
    let searching = !query.is_empty();
    let structure = structure_sig(p, &query);
    // Book (Updates): the document rebuilds only when its structure changes; otherwise rows
    // patch in place, so a switch slides instead of redrawing.
    let patch = !fresh && memo.structure == Some(structure);
    // Rebuilding, the switches leave before the old content goes (a removed node's id is
    // reused), to be reused so a flip slides.
    let mut kept: HashMap<String, NodeId> = HashMap::new();
    if !patch {
        kept = std::mem::take(&mut memo.switches);
        for &b in kept.values() {
            if cx.dom.doc.try_node(b).is_some() {
                cx.dom.doc.detach(b);
            }
        }
    }
    // A page or query change scrolls to the top and replays the arrive motion.
    if !fresh && (memo.page != page || memo.query != query) {
        cx.dom.doc.remove(doc);
        doc = cx.dom.child(scroll, "div", "pf-doc arrive");
        memo.skeleton = Some([root, nav, scroll, doc, top]);
        memo.body.clear();
        memo.rows.clear();
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
    for n in std::mem::take(&mut memo.inner) {
        if cx.dom.doc.try_node(n).is_some() {
            cx.dom.doc.remove(n);
        }
    }
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
    let sections: Vec<&Map<String, Value>> = arr(p, "sections").iter().filter_map(obj).collect();
    // The row whose menu is open: a choice drawn as a menu, being edited.
    let menu_open: Option<String> = ed
        .as_ref()
        .filter(|e| {
            find_row(p, e.row).is_some_and(|r| {
                r.get("control")
                    .and_then(obj)
                    .is_some_and(|c| str_prop(c, "k") == Some("choice") && !segmented(c))
            })
        })
        .map(|e| e.row.to_owned());
    let row_sig = |r: &Map<String, Value>, rid: &str| {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for (k, v) in r {
            k.hash(&mut h);
            super::picker::hash_value(v, &mut h);
        }
        let e = ed.as_ref().filter(|e| e.row == rid);
        e.map(|e| (e.option, e.draft, e.cursor)).hash(&mut h);
        (focus == Some(rid), memo.menu.as_deref() == Some(rid)).hash(&mut h);
        h.finish()
    };
    let preview = node
        .children()
        .any(|c| route(c) == PREVIEW)
        .then(|| slot(cx, m, PREVIEW, "div", "pf-preview"));
    // Slots are created in order: make the first two exist too.
    let extra = slot(cx, m, EXTRA, "div", "pf-extra");
    let editor = slot(cx, m, EDITOR, "div", "pf-editor");
    let preview = preview.unwrap_or_else(|| slot(cx, m, PREVIEW, "div", "pf-preview"));
    let mut switches: HashMap<String, NodeId> = HashMap::new();
    if patch {
        for sec in &sections {
            for r in arr(sec, "rows").iter().filter_map(obj) {
                let Some(rid) = str_prop(r, "id") else {
                    continue;
                };
                let Some(&(old, list, sig)) = memo.rows.get(rid) else {
                    continue;
                };
                let new_sig = row_sig(r, rid);
                if sig == new_sig && cx.dom.doc.try_node(old).is_some() {
                    continue;
                }
                let prefix = format!("{rid}\u{1f}");
                let mine: Vec<String> = memo
                    .switches
                    .keys()
                    .filter(|k| *k == rid || k.starts_with(&prefix))
                    .cloned()
                    .collect();
                for k in mine {
                    if let Some(b) = memo.switches.remove(&k) {
                        if cx.dom.doc.try_node(b).is_some() {
                            cx.dom.doc.detach(b);
                        }
                        kept.insert(k, b);
                    }
                }
                let mut next = HashMap::new();
                let mut menu = None;
                let el = draw_row(
                    cx,
                    list,
                    r,
                    rid,
                    ed.as_ref().filter(|e| e.row == rid),
                    focus == Some(rid),
                    &words,
                    &mut kept,
                    &mut next,
                    &mut menu,
                    memo.menu.as_deref() == Some(rid),
                    sheet,
                );
                if cx.dom.doc.try_node(old).is_some() {
                    cx.dom.doc.insert_before(list, el, Some(old));
                    cx.dom.doc.remove(old);
                }
                memo.switches.extend(next);
                memo.rows.insert(rid.to_owned(), (el, list, new_sig));
                memo.targets.insert(rid.to_owned(), el);
            }
        }
        // The focus ring on sections.
        for (sid, el) in &memo.sections {
            if cx.dom.doc.try_node(*el).is_some() {
                cx.dom.set_class(
                    *el,
                    if Some(sid.as_str()) == focus {
                        "pf-sec focus"
                    } else {
                        "pf-sec"
                    },
                );
            }
        }
        switches = std::mem::take(&mut memo.switches);
        if let Some(sec) = memo.status_sec.filter(|&e| cx.dom.doc.try_node(e).is_some()) {
            cx.dom.doc.append_child(sec, preview);
        }
    } else {
        for n in std::mem::take(&mut memo.body) {
            if cx.dom.doc.try_node(n).is_some() {
                cx.dom.doc.remove(n);
            }
        }
        memo.rows.clear();
        memo.sections.clear();
        memo.status_sec = None;
        memo.targets.clear();
        let mut body = Vec::new();
        let head = cx.dom.child(
            doc,
            "header",
            if searching {
                "pf-head pf-found"
            } else {
                "pf-head"
            },
        );
        body.push(head);
        let page_index = pages.iter().position(|pg| str_prop(pg, "id") == Some(page));
        let page_label = page_index
            .and_then(|i| s(pages[i], "label").or(str_prop(pages[i], "id")))
            .unwrap_or(page);
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
        if row_count == 0 {
            let none = if searching {
                format!("No settings match \u{201c}{query}\u{201d}")
            } else {
                "Nothing to set here".to_owned()
            };
            let e = cx.dom.child_text(doc, "div", "pf-none", &none);
            body.push(e);
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
                        body.push(part);
                        let info = sp
                            .and_then(|sp| pages.iter().find(|pg| str_prop(pg, "id") == Some(sp)));
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
                body.push(section);
            }
            if let Some(sid) = sid {
                cx.dom.attr(section, "data-sec", sid);
                memo.targets.insert(sid.to_owned(), section);
                memo.sections.push((sid.to_owned(), section));
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
                    let mut menu = None;
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
                        &mut menu,
                        memo.menu.as_deref() == Some(rid),
                        sheet,
                    );
                    memo.targets.insert(rid.to_owned(), el);
                    memo.rows.insert(rid.to_owned(), (el, list, row_sig(r, rid)));
                }
            }
            if sid == Some("status-line") {
                cx.dom.doc.append_child(section, preview);
                memo.status_sec = Some(section);
            }
        }
        memo.body = body;
    }
    cx.dom.doc.append_child(doc, extra);
    // The editor card: a glass card over the page while it holds something. Holding a picker
    // (omp's submenu of more than 12 options, alone or in its editor column), the card is the
    // sheet's frame, sized for it (`.pk`): the picker fills it.
    let has_editor = node.children().any(|c| route(c) == EDITOR);
    let holds_picker = node
        .children()
        .filter(|&c| route(c) == EDITOR)
        .any(holds_picker_node);
    cx.dom.set_class(
        editor,
        match (has_editor, holds_picker) {
            (true, true) => "pf-editor on pk",
            (true, false) => "pf-editor on",
            _ => "pf-editor",
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
        && let Some(&el) = memo.targets.get(f)
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
    memo.structure = Some(structure);
    memo.page = page.to_owned();
    memo.query = query;
    memo.focus = focus_owned;
    if menu_open.is_none() {
        memo.hovered = None;
    }
    memo.menu = menu_open;
    cx.st.prefs.insert(id, memo);
}

/// The document's structure (book: prefs Updates): page, query, the header's texts, sections
/// (ids, titles, pages), row ids, control kinds and options. While it holds, rows patch.
fn structure_sig(p: &Map<String, Value>, query: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (str_prop(p, "page"), query, s(p, "lead")).hash(&mut h);
    for pg in arr(p, "pages").iter().filter_map(obj) {
        (str_prop(pg, "id"), s(pg, "label"), s(pg, "disabled")).hash(&mut h);
    }
    for sec in arr(p, "sections").iter().filter_map(obj) {
        (str_prop(sec, "id"), s(sec, "title"), str_prop(sec, "page")).hash(&mut h);
        for r in arr(sec, "rows").iter().filter_map(obj) {
            str_prop(r, "id").hash(&mut h);
            let ctl = r.get("control").and_then(obj);
            ctl.and_then(|c| str_prop(c, "k")).hash(&mut h);
            if let Some(o) = ctl.and_then(|c| c.get("options")) {
                super::picker::hash_value(o, &mut h);
            }
        }
    }
    h.finish()
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
    sheet: bool,
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
                    cx.dom
                        .set_style(menu, &format!("--pf-room: {}px", fmt_num(f64::from(menu_room(cx, sheet)))));
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
                    cx.dom.attr(g, "data-pf", "grip");
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
    // The page's own acts, sent as they are: a row's `act` may hold `=`.
    let action = |a: &str, value: Option<&str>| ViewEvent::Action {
        id: id.clone(),
        action: a.to_owned(),
        value: auk_tsp::ActValue::Exact(value.map(str::to_owned)),
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
            out.push(action("close", Some(&open)));
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
            action("page", Some(&v?))
        }
        "close" => action("close", None),
        _ => {
            let (rid, r) = (row_id.as_deref()?, row?);
            if s(r, "disabled").is_some() {
                return Some(Vec::new());
            }
            let empty = Map::new();
            let ctl = r.get("control").and_then(obj).unwrap_or(&empty);
            match a.as_str() {
                // A grip is dragged, not clicked (`PaneView::reorder_press`).
                "grip" => return Some(Vec::new()),
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
                "act" => action(v.as_deref().unwrap_or("edit"), Some(rid)),
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

/// An ordered multi list's item dragged by its grip (book: prefs, `multi` with `ordered`).
#[derive(Debug)]
pub(crate) struct Reorder {
    scope: String,
    /// The `prefs` node, its row, and the dragged item's value.
    node: String,
    row: String,
    value: String,
    /// The dragged `.pf-oi`, its class before the drag, and the pointer's y at the press.
    item: NodeId,
    class: String,
    y0: f32,
}

/// The new enabled values after dropping `value` on `target` (before it, or after it when
/// `after`), given the list's items in drawn order with whether each is on. The dragged item
/// is enabled. `None` when nothing moves.
pub(crate) fn reorder_values(
    items: &[(String, bool)],
    value: &str,
    target: &str,
    after: bool,
) -> Option<Vec<String>> {
    if value == target {
        return None;
    }
    let mut order: Vec<(String, bool)> = items.to_vec();
    let from = order.iter().position(|(v, _)| v == value)?;
    let (v, _) = order.remove(from);
    let to = order.iter().position(|(t, _)| t == target)?;
    order.insert(if after { to + 1 } else { to }, (v, true));
    let before: Vec<&str> = items.iter().filter(|(_, on)| *on).map(|(v, _)| v.as_str()).collect();
    let now: Vec<String> = order.into_iter().filter(|(_, on)| *on).map(|(v, _)| v).collect();
    (now.iter().map(String::as_str).collect::<Vec<_>>() != before).then_some(now)
}

impl PaneView {
    /// A press on an ordered list's grip starts dragging its item (book: prefs, `multi`):
    /// returns whether it did. Hosts call this before [`PaneView::click`], and then
    /// [`PaneView::reorder_drag`] and [`PaneView::reorder_release`].
    pub fn reorder_press(&mut self, doc: &mut Document, pane: &TspPane, x: f32, y: f32) -> bool {
        self.reorder = None;
        let Some(hit) = auk_ui::hit_test(doc, x, y) else {
            return false;
        };
        let chain: Vec<NodeId> = std::iter::successors(Some(hit), |&e| doc.parent(e)).collect();
        if !chain.iter().any(|&e| doc.attribute(e, "data-pf") == Some("grip")) {
            return false;
        }
        let up = |k: &str| {
            chain
                .iter()
                .find_map(|&e| doc.attribute(e, k).map(|v| (e, v.to_owned())))
        };
        let (Some((item, value)), Some((_, row)), Some((_, node)), Some((_, scope))) =
            (up("data-v"), up("data-row"), up("data-id"), up("data-scope"))
        else {
            return false;
        };
        // A disabled row's controls do nothing.
        let Some(sv) = self.surfaces.iter().find(|s| s.scope == scope) else {
            return false;
        };
        let Some(surface) = pane.surface(sv.key) else {
            return false;
        };
        let Some(n) = surface.document().get(&node).filter(|n| n.kind() == "prefs") else {
            return false;
        };
        if find_row(n.props(), &row).is_none_or(|r| s(r, "disabled").is_some()) {
            return false;
        }
        let class = self.written.class(item).unwrap_or("").to_owned();
        let mut dom = Dom {
            doc,
            w: &mut self.written,
        };
        dom.set_class(item, &format!("{class} drag"));
        self.reorder = Some(Reorder {
            scope,
            node,
            row,
            value,
            item,
            class,
            y0: y,
        });
        true
    }

    /// Whether a grip drag is under way.
    pub fn reordering(&self) -> bool {
        self.reorder.is_some()
    }

    /// The pointer moved while dragging: the item follows it. Returns whether a drag is under
    /// way.
    pub fn reorder_drag(&mut self, doc: &mut Document, _x: f32, y: f32) -> bool {
        let Some(r) = &self.reorder else {
            return false;
        };
        let (item, dy) = (r.item, y - r.y0);
        if doc.try_node(item).is_some() {
            let mut dom = Dom {
                doc,
                w: &mut self.written,
            };
            dom.set_style(item, &format!("transform: translateY({}px);", fmt_num(f64::from(dy))));
        }
        true
    }

    /// The release: dropped on another item of the list, the dragged one moves before it (after
    /// it on its lower half), is enabled, and the new order goes to the program as `change
    /// {item = <row>, value = [enabled values in order]}`. `None` when no drag was under way.
    pub fn reorder_release(&mut self, doc: &mut Document, x: f32, y: f32) -> Option<Vec<Outcome>> {
        let r = self.reorder.take()?;
        let _ = x;
        if doc.try_node(r.item).is_none() {
            return Some(Vec::new());
        }
        {
            let mut dom = Dom {
                doc: &mut *doc,
                w: &mut self.written,
            };
            dom.set_class(r.item, &r.class);
            dom.set_style(r.item, "");
        }
        let list = doc.parent(r.item)?;
        let items: Vec<NodeId> = doc.element_children(list);
        let drawn: Vec<(String, bool)> = items
            .iter()
            .filter_map(|&e| {
                let v = doc.attribute(e, "data-v")?.to_owned();
                let on = self.written.class(e).is_some_and(|c| c.split(' ').any(|c| c == "on"));
                Some((v, on))
            })
            .collect();
        // The item under the pointer's y, by the items' boxes (laid out without the drag).
        let target = items.iter().find_map(|&e| {
            let b = doc.border_box(e)?;
            (y >= b.y && y < b.y + b.h).then(|| (doc.attribute(e, "data-v").map(str::to_owned), y >= b.y + b.h / 2.0))
        });
        let Some((Some(target), after)) = target else {
            return Some(Vec::new());
        };
        let Some(values) = reorder_values(&drawn, &r.value, &target, after) else {
            return Some(Vec::new());
        };
        let sv = self.surfaces.iter().find(|s| s.scope == r.scope)?;
        Some(vec![Outcome::Event {
            surface: sv.key,
            sf: sv.id.clone(),
            event: Box::new(ViewEvent::PrefsChange {
                id: r.node,
                item: r.row,
                value: Value::Array(values.into_iter().map(Value::String).collect()),
            }),
        }])
    }

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
