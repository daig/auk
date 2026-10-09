//! `picker` (book:6274-6528, research/elements.md §2.4, §6.4): omp's data-first modal sheet. The
//! program sends the catalog and what to show; the view draws the glass sheet: a head with a
//! drawn search line (no real field: `query` with a caret at `cursor`; keys stay the program's),
//! an optional scope column, a tab row, fixed-height rows with fact columns, a preview pane (the
//! node's children), the `strip` of chips above the action bar (always visible), and the action
//! bar. Pointer input becomes `select`, `activate` and `action` events.
//!
//! Rows are cached per item id with a signature of what they draw, so a keystroke that only
//! filters (`order`, `hits`) or moves the selection rebuilds the rows that changed and moves the
//! rest. Past 200 rows the scroller is virtualized (fixed row heights).

use super::{Cx, Look, Mount, begin, icon, part, slot, spans_into};
use crate::common::{TONES, bool_prop, fmt_num, str_prop};
use crate::spans;
use auk_tsp::NodeRef;
use auk_ui::NodeId;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

/// The sheet-in animation length (ms), as an overlay's pop.
const SHEET_IN_MS: f64 = 220.0;
/// Rows past which the list is virtualized.
const VIRTUAL_AFTER: usize = 200;
/// A label keeps this much room before fact columns hide (book:6288).
const LABEL_MIN: f32 = 260.0;

/// One picker's view state, by node id.
#[derive(Debug, Default)]
pub(crate) struct PickerState {
    /// The row elements drawn, by row key (`i:<id>` or `g:<group>`): element and signature.
    rows: HashMap<String, (NodeId, u64)>,
    /// The scroller the rows sit in.
    scroll: Option<NodeId>,
    /// What the last render showed: selection, layout, the row keys' hash.
    selected: Option<String>,
    layout: String,
    order_hash: u64,
    /// Where the selected row sits in the scroller (top, bottom), as last drawn.
    sel_at: Option<(f32, f32)>,
    /// A scroll that brings (top, bottom) into view, applied after layout.
    pub reveal: Option<(f32, f32)>,
    /// The row clicked last, and whether that click activated it (a double click's second
    /// press then sends nothing).
    pub last_click: Option<(String, bool)>,
    /// The narrow head's scope menu is open.
    pub menu: bool,
    /// The confirm strip drawn over the selected row.
    confirm: Option<NodeId>,
}

/// What a press on a picker part means.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Press {
    /// A row: `select` or `activate` (decided by the caller from the click state).
    Row { item: String, disabled: bool },
    /// `action` with `act` (and `value` for scope/tab/strip); `None` for a disabled button.
    Act(Option<(String, Option<String>)>),
    /// The narrow head's scope button: opens or closes the menu (local).
    Menu,
    /// Group headers and the sheet itself: nothing.
    Nothing,
}

/// The press under the parts of a hit (innermost first), up to the picker element.
pub(crate) fn press(doc: &auk_ui::Document, node: NodeRef, parts: &[NodeId]) -> Option<Press> {
    let p = node.props();
    for &el in parts {
        let Some(pk) = doc.attribute(el, "data-pk") else {
            continue;
        };
        let v = doc.attribute(el, "data-v").unwrap_or("").to_owned();
        return Some(match pk {
            "row" => {
                let disabled = catalog(p).get(&v).is_some_and(|it| disabled(it).is_some());
                Press::Row { item: v, disabled }
            }
            "act" => {
                let off = actions(p).iter().any(|a| a.id == v && a.disabled.is_some());
                Press::Act((!off).then_some((v, None)))
            }
            "dim" => Press::Act(Some((escape_action(p), None))),
            "scope" | "tab" | "strip" => Press::Act(Some((pk.to_owned(), Some(v)))),
            "menu" => Press::Menu,
            _ => Press::Nothing,
        });
    }
    None
}

/// The action the backdrop and the head `esc` send: the one bound to Escape, else `close`.
pub(crate) fn escape_action(p: &Map<String, Value>) -> String {
    actions(p)
        .into_iter()
        .find(|a| a.esc())
        .map(|a| a.id)
        .unwrap_or_else(|| "close".to_owned())
}

/// After layout: scroll each picker's list to the row it asked to reveal (nearest edge).
pub(crate) fn apply_reveals(
    doc: &mut auk_ui::Document,
    pickers: &mut HashMap<String, PickerState>,
) {
    for ps in pickers.values_mut() {
        let (Some(sc), Some((top, bottom))) = (ps.scroll, ps.reveal) else {
            continue;
        };
        if doc.try_node(sc).is_none() || !doc.laid_out(sc) {
            continue;
        }
        let Some(r) = doc.border_box(sc) else {
            continue;
        };
        if r.h <= 0.0 {
            continue;
        }
        ps.reveal = None;
        let (_, y) = doc.scroll_offset(sc);
        let to = if top < y {
            top
        } else if bottom > y + r.h {
            bottom - r.h
        } else {
            y
        };
        if to != y {
            doc.set_scroll_offset(sc, 0.0, to);
            // A virtualized list lays out the rows that came into view now, not next frame.
            doc.update();
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Props

#[derive(Clone, Debug)]
struct Action {
    id: String,
    label: Value,
    keys: Vec<String>,
    primary: bool,
    end: bool,
    danger: bool,
    on: bool,
    disabled: Option<String>,
}

impl Action {
    fn esc(&self) -> bool {
        self.keys.iter().any(|k| k == "esc" || k == "escape")
    }
}

fn reason(v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::Bool(true)) => Some(String::new()),
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    }
}

fn actions(p: &Map<String, Value>) -> Vec<Action> {
    let mut out = Vec::new();
    for a in p
        .get("actions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(o) = a.as_object() else { continue };
        let Some(id) = o.get("id").and_then(Value::as_str) else {
            continue;
        };
        out.push(Action {
            id: id.to_owned(),
            label: o
                .get("label")
                .filter(|l| !l.is_null())
                .cloned()
                .unwrap_or_else(|| Value::String(id.to_owned())),
            keys: o
                .get("keys")
                .and_then(Value::as_array)
                .map(|k| {
                    k.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            primary: bool_prop(o, "primary"),
            end: bool_prop(o, "end"),
            danger: bool_prop(o, "danger"),
            on: bool_prop(o, "on"),
            disabled: reason(o.get("disabled")),
        });
    }
    out
}

/// The catalog: `items`, then `itemsAdd` upserted and `itemsDel` removed (omp sends the deltas
/// against the `items` it sent, so this is a pure function of the props), in catalog order.
fn catalog_list(p: &Map<String, Value>) -> Vec<(String, &Map<String, Value>)> {
    let mut list: Vec<(String, &Map<String, Value>)> = Vec::new();
    let mut at: HashMap<String, usize> = HashMap::new();
    let items = p
        .get("items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten();
    let adds = p
        .get("itemsAdd")
        .and_then(Value::as_array)
        .into_iter()
        .flatten();
    for it in items.chain(adds) {
        let Some(o) = it.as_object() else { continue };
        let Some(id) = o.get("id").and_then(item_id) else {
            continue;
        };
        match at.get(&id) {
            Some(&i) => list[i].1 = o,
            None => {
                at.insert(id.clone(), list.len());
                list.push((id, o));
            }
        }
    }
    let del: HashSet<String> = p
        .get("itemsDel")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(item_id)
        .collect();
    if !del.is_empty() {
        list.retain(|(id, _)| !del.contains(id));
    }
    list
}

fn catalog(p: &Map<String, Value>) -> HashMap<String, &Map<String, Value>> {
    catalog_list(p).into_iter().collect()
}

pub(crate) fn item_id(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn disabled(it: &Map<String, Value>) -> Option<String> {
    reason(it.get("disabled"))
}

/// One entry of what the list shows.
enum Entry<'a> {
    Group {
        key: String,
        label: Option<&'a Value>,
        count: Option<f64>,
    },
    Item(String, &'a Map<String, Value>),
}

fn entries<'a>(
    p: &'a Map<String, Value>,
    cat: &[(String, &'a Map<String, Value>)],
) -> Vec<Entry<'a>> {
    let by: HashMap<&str, &Map<String, Value>> =
        cat.iter().map(|(id, it)| (id.as_str(), *it)).collect();
    match p.get("order").and_then(Value::as_array) {
        Some(order) => {
            let mut out = Vec::new();
            let mut seen = HashSet::new();
            for o in order {
                match o {
                    Value::Object(g) if g.contains_key("group") => {
                        let key = g
                            .get("group")
                            .map(super::layout::value_key)
                            .unwrap_or_default();
                        out.push(Entry::Group {
                            key,
                            label: g.get("label"),
                            count: g.get("count").and_then(Value::as_f64),
                        });
                    }
                    other => {
                        let Some(id) = item_id(other) else { continue };
                        if let Some(it) = by.get(id.as_str())
                            && seen.insert(id.clone())
                        {
                            out.push(Entry::Item(id, it));
                        }
                    }
                }
            }
            out
        }
        None => cat
            .iter()
            .map(|(id, it)| Entry::Item(id.clone(), it))
            .collect(),
    }
}

#[derive(Clone, Debug)]
struct Column {
    id: String,
    head: String,
    format: String,
    priority: f64,
    /// Width in px.
    w: f32,
}

fn columns(p: &Map<String, Value>) -> Vec<Column> {
    let mut out = Vec::new();
    for c in p
        .get("columns")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(8)
    {
        let Some(o) = c.as_object() else { continue };
        let Some(id) = o.get("id").and_then(Value::as_str) else {
            continue;
        };
        let format = match str_prop(o, "format") {
            Some(f @ ("text" | "num" | "price" | "bar" | "time" | "elapsed" | "dim")) => f,
            _ => "text",
        };
        let w: f32 = match format {
            "num" => 52.0,
            "price" => 76.0,
            "bar" | "time" => 44.0,
            "elapsed" => 52.0,
            "dim" => 64.0,
            _ => 96.0,
        };
        let min = o
            .get("min")
            .and_then(Value::as_f64)
            .map(|ch| ch as f32 * 7.0 + 8.0)
            .unwrap_or(0.0);
        out.push(Column {
            id: id.to_owned(),
            head: str_prop(o, "head").unwrap_or("").to_owned(),
            format: format.to_owned(),
            priority: o.get("priority").and_then(Value::as_f64).unwrap_or(100.0),
            w: w.max(min),
        });
    }
    out
}

/// Row height by layout (book:6285).
fn row_h(layout: &str, it: Option<&Map<String, Value>>) -> f32 {
    match layout {
        "cards" => 54.0,
        "timeline" => {
            if it.and_then(|i| str_prop(i, "node")) == Some("user") {
                34.0
            } else {
                26.0
            }
        }
        "tree" => 24.0,
        _ => 36.0,
    }
}
const GROUP_H: f32 = 32.0;

/// UTF-16 offset → char index in `s`.
fn utf16_to_char(s: &str, at: u64) -> usize {
    let mut u = 0u64;
    for (i, c) in s.chars().enumerate() {
        if u >= at {
            return i;
        }
        u += c.len_utf16() as u64;
    }
    s.chars().count()
}

/// `[from, to)` UTF-16 ranges as positional pairs.
fn ranges(v: Option<&Value>) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    for r in v.and_then(Value::as_array).into_iter().flatten() {
        let pair = match r {
            Value::Array(a) if a.len() >= 2 => (a[0].as_u64(), a[1].as_u64()),
            Value::Object(o) => (
                o.get("from").and_then(Value::as_u64),
                o.get("to").and_then(Value::as_u64),
            ),
            _ => (None, None),
        };
        if let (Some(a), Some(b)) = pair
            && b > a
        {
            out.push((a, b));
        }
    }
    out
}

/// Hashes a JSON value in place (no copy, no serializing): rows' signatures hash every item
/// on every render.
pub(crate) fn hash_value(v: &Value, h: &mut impl Hasher) {
    match v {
        Value::Null => 0u8.hash(h),
        Value::Bool(b) => (1u8, b).hash(h),
        Value::Number(n) => {
            2u8.hash(h);
            match n.as_f64() {
                Some(f) => f.to_bits().hash(h),
                None => n.to_string().hash(h),
            }
        }
        Value::String(s) => (3u8, s).hash(h),
        Value::Array(a) => {
            (4u8, a.len()).hash(h);
            for x in a {
                hash_value(x, h);
            }
        }
        Value::Object(o) => {
            (5u8, o.len()).hash(h);
            for (k, x) in o {
                k.hash(h);
                hash_value(x, h);
            }
        }
    }
}

/// A number shown compact: `272k`, `1.6M` (book:6339). The unit is picked after rounding, so
/// 999 950 is `1M`, not `1000k`.
pub(crate) fn compact(n: f64) -> String {
    const UNITS: [(f64, &str); 4] = [(1.0, ""), (1e3, "k"), (1e6, "M"), (1e9, "B")];
    let round = |v: f64| {
        if v.abs() < 10.0 {
            (v * 10.0).round() / 10.0
        } else {
            v.round()
        }
    };
    let a = n.abs();
    let mut i = UNITS.iter().rposition(|(s, _)| a >= *s).unwrap_or(0);
    let mut v = round(n / UNITS[i].0);
    if v.abs() >= 1000.0 && i + 1 < UNITS.len() {
        i += 1;
        v = round(n / UNITS[i].0);
    }
    format!("{}{}", fmt_num(v), UNITS[i].1)
}

/// A count with thousands separators.
pub(crate) fn thousands(n: f64) -> String {
    let i = n.round() as i64;
    let s = i.unsigned_abs().to_string();
    let mut out = String::new();
    for (k, c) in s.chars().enumerate() {
        if k > 0 && (s.len() - k).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if i < 0 { format!("-{out}") } else { out }
}

/// Initials for a mark: the first two non-space characters.
fn initials(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace())
        .take(2)
        .collect()
}

/// The gradient a mark draws on, from its seed.
fn mark_style(seed: &str) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    seed.hash(&mut h);
    let hue = (h.finish() % 360) as i64;
    format!(
        "background-image: linear-gradient(135deg, hsl({hue}, 62%, 58%), hsl({}, 58%, 44%))",
        (hue + 40) % 360
    )
}

/// A dot: a tone (`.t-<tone>`) or a palette name (`--sf-p-<name>`).
fn dot(cx: &mut Cx, parent: NodeId, name: &str, extra: &str) -> NodeId {
    let class = if TONES.contains(&name) {
        format!("pk-dot{extra} t-{name}")
    } else {
        format!("pk-dot{extra}")
    };
    let d = cx.dom.child(parent, "span", &class);
    cx.dom.attr(d, "aria-hidden", "true");
    if !TONES.contains(&name) && valid_name(name) {
        cx.dom.set_style(
            d,
            &format!("background-color: var(--sf-p-{name}, var(--t3))"),
        );
    }
    d
}

fn valid_name(n: &str) -> bool {
    !n.is_empty()
        && n.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn keycaps(cx: &mut Cx, parent: NodeId, keys: &[String]) {
    if keys.is_empty() {
        return;
    }
    let k = cx.dom.child(parent, "span", "kbds");
    for key in keys {
        let (cap, _) = crate::icons::keycap(key);
        cx.dom.child_text(k, "span", "kbd", &cap);
    }
}

// ---------------------------------------------------------------------------------------------
// Render

/// The `prefs` node whose editor card holds `node` (directly, or in its editor column).
fn editor_host(node: NodeRef) -> Option<NodeRef> {
    std::iter::successors(node.parent(), |n| n.parent()).find(|n| n.kind() == "prefs")
}

/// The sheet's width, estimated from the pane's (for the width states and the fact columns). In
/// a `prefs` editor card it is the card's: the page's (or the docked sheet's) width less the
/// card's 16px insets, at most 600px (`surface-prefs.css` `.pf-editor.on`).
fn sheet_width(cx: &Cx, node: NodeRef, size: &str) -> f32 {
    let zoom = (cx.set.font_px / 13.0).max(0.1);
    let pane = f32::from(cx.cols.max(1)) * cx.set.cell.0 / zoom;
    if let Some(prefs) = editor_host(node) {
        let host = if super::prefs::in_layer(prefs) {
            f32::from(auk_tsp::aside_reserve(cx.cols)) * cx.set.cell.0 / zoom
        } else {
            pane
        };
        return (host - 32.0).clamp(0.0, 600.0);
    }
    match size {
        "screen" => pane,
        "md" => (pane - 32.0).min(680.0),
        _ => (pane - 32.0).min(1120.0),
    }
}

pub(crate) fn picker(cx: &mut Cx, node: NodeRef, m: &mut Mount, look: &mut Look) {
    let id = node.id().to_owned();
    // The scroller (with its rows and scroll offset) outlives the parts rebuilt around it. It
    // isn't a routing slot: children go to the preview only.
    let kept = cx.st.pickers.get(&id).and_then(|s| s.scroll);
    let kept = kept.filter(|&sc| {
        cx.dom.doc.try_node(sc).is_some() && cx.dom.class_of(sc).starts_with("pk-scroll pk-spacer")
    });
    if let Some(sc) = kept {
        cx.dom.doc.detach(sc);
    }
    begin(cx, m);
    let p = node.props();
    look.title_is_content = true;
    let size = match str_prop(p, "size") {
        Some(s @ ("md" | "lg" | "screen")) => s,
        _ => "lg",
    };
    let layout = match str_prop(p, "layout") {
        Some(l @ ("rows" | "cards" | "timeline" | "tree")) => l,
        _ => "rows",
    };
    let preview = match str_prop(p, "preview") {
        Some(v @ ("side" | "below" | "none")) => v,
        _ if size == "md" => "none",
        _ => "side",
    };
    let focus = match str_prop(p, "focus") {
        Some(f @ ("list" | "scopes" | "tabs" | "strip" | "preview")) => f,
        _ => "list",
    };
    let state = match str_prop(p, "state") {
        Some(s @ ("loading" | "error")) => s,
        _ => "ready",
    };
    let noun = str_prop(p, "noun").unwrap_or("items");
    let query: Option<&str> = str_prop(p, "query");
    let title = p.get("title").filter(|t| !t.is_null());
    let current: HashSet<String> = p
        .get("current")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(item_id)
        .collect();
    let selected = p.get("selected").and_then(item_id);
    let acts = actions(p);
    let cat = catalog_list(p);
    let list = entries(p, &cat);
    let shown = list.iter().filter(|e| matches!(e, Entry::Item(..))).count();
    let scopes: Vec<&Map<String, Value>> = p
        .get("scopes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .filter(|s| s.get("id").is_some())
        .collect();
    let tabs: Vec<&Map<String, Value>> = p
        .get("tabs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .filter(|s| s.get("id").is_some())
        .collect();

    // Width states (book:6281-6282).
    let w = sheet_width(cx, node, size);
    let narrow = w < 760.0;
    let pv_below_auto = preview == "side" && w < 980.0;
    let pv_hide = preview != "none" && w < 640.0;
    look.class(&format!("sz-{size}"));
    look.class(&format!("lay-{layout}"));
    look.class(&format!("pv-{preview}"));
    look.class_if(!current.is_empty(), "has-cur");
    look.class_if(title.is_none(), "no-title");
    look.class_if(query.is_none(), "no-query");
    look.class_if(state == "loading", "st-loading");
    look.class_if(state == "error", "st-error");
    look.class_if(narrow, "narrow");
    look.class_if(pv_below_auto, "pv-below-auto");
    look.class_if(pv_hide, "pv-hide");

    // The sheet animates in once, when it opens into a drawn surface (as an overlay pops).
    let opened = cx.st.drawn.then_some(cx.now);
    let opened = *cx.st.overlays.entry(id.clone()).or_insert(opened);
    look.class_if(opened.is_some_and(|t| cx.now < t + SHEET_IN_MS), "pk-in");

    let ps_menu = cx.st.pickers.get(&id).is_some_and(|s| s.menu) && narrow && !scopes.is_empty();

    let dim = part(cx, m, "div", "pk-dim");
    cx.dom.attr(dim, "aria-hidden", "true");
    cx.dom.attr(dim, "data-pk", "dim");
    let sheet = part(cx, m, "div", &format!("pk-sheet f-{focus}"));
    cx.dom.attr(sheet, "role", "dialog");
    cx.dom.attr(sheet, "aria-modal", "true");
    let title_text = spans::spans_text(&spans::parse_spans(title));
    let name = if !title_text.is_empty() {
        title_text.clone()
    } else if let Some(ph) = str_prop(p, "placeholder") {
        ph.to_owned()
    } else {
        noun.to_owned()
    };
    cx.dom.attr(sheet, "aria-label", &name);

    // Head.
    let head = cx.dom.child(sheet, "div", "pk-head");
    let ic = cx.dom.child(head, "span", "pk-ic");
    icon(cx, ic, str_prop(p, "icon").unwrap_or("list"));
    if title.is_some() {
        let t = cx.dom.child(head, "div", "pk-title");
        let tt = cx.dom.child(t, "span", "t");
        m.clock |= spans_into(cx, tt, title);
        if p.get("subtitle").is_some_and(|s| !s.is_null()) {
            let sub = cx.dom.child(t, "span", "sub");
            m.clock |= spans_into(cx, sub, p.get("subtitle"));
        }
    }
    if narrow && !scopes.is_empty() {
        let b = cx.dom.child(
            head,
            "div",
            if ps_menu {
                "pk-scopebtn open"
            } else {
                "pk-scopebtn"
            },
        );
        cx.dom.attr(b, "role", "button");
        cx.dom.attr(b, "data-pk", "menu");
        let active = str_prop(p, "scope");
        let label = scopes
            .iter()
            .find(|s| s.get("id").map(super::layout::value_key).as_deref() == active)
            .and_then(|s| s.get("label"));
        let l = cx.dom.child(b, "span", "l");
        spans_into(cx, l, label);
        let ch = cx.dom.child(b, "span", "pk-chev");
        icon(cx, ch, "chevron-down");
    }
    if let Some(q) = query {
        let s = cx.dom.child(head, "div", "pk-search");
        let caret_on = focus == "list";
        if q.is_empty() {
            if caret_on {
                cx.dom.child(s, "span", "caret");
            }
            let ph = str_prop(p, "placeholder")
                .map(str::to_owned)
                .unwrap_or_else(|| format!("Search {noun}…"));
            cx.dom.child_text(s, "span", "ph", &ph);
        } else {
            let at = p
                .get("cursor")
                .and_then(Value::as_u64)
                .map(|c| utf16_to_char(q, c))
                .unwrap_or_else(|| q.chars().count());
            let before: String = q.chars().take(at).collect();
            let after: String = q.chars().skip(at).collect();
            let qs = cx.dom.child(s, "span", "q");
            if !before.is_empty() {
                cx.dom.text(qs, &before);
            }
            if caret_on {
                cx.dom.child(qs, "span", "caret");
            }
            if !after.is_empty() {
                cx.dom.text(qs, &after);
            }
        }
    } else {
        // The head keeps its layout: the title takes the room.
        cx.dom.child(head, "span", "pk-head-fill");
    }
    let count = if state == "loading" {
        "…".to_owned()
    } else {
        match p.get("total").and_then(Value::as_f64) {
            Some(t) if (t - shown as f64).abs() >= 0.5 => {
                format!("{} of {}", thousands(shown as f64), thousands(t))
            }
            _ => thousands(shown as f64),
        }
    };
    cx.dom.child_text(head, "span", "pk-count", &count);
    let esc_act = acts.iter().find(|a| a.esc());
    // Action bar folding (book:6290): estimated from the labels, the Escape action moves into
    // the head's `esc` first, then secondary buttons drop their keycaps.
    let btn_w = |a: &Action, keys: bool| {
        let label = spans::spans_text(&spans::parse_spans(Some(&a.label)));
        let k: f32 = if keys {
            a.keys
                .iter()
                .map(|k| crate::icons::keycap(k).0.chars().count() as f32 * 7.0 + 12.0)
                .sum()
        } else {
            0.0
        };
        label.chars().count() as f32 * 7.0 + 24.0 + k + 6.0
    };
    let bar_room = w - 24.0;
    let full: f32 = acts.iter().map(|a| btn_w(a, true)).sum();
    let fold_esc = full > bar_room && esc_act.is_some_and(|a| !a.primary);
    let after_esc: f32 = acts
        .iter()
        .filter(|a| !(fold_esc && a.esc() && !a.primary))
        .map(|a| btn_w(a, true))
        .sum();
    // Then (or first, with no Escape action to move) the keycaps go.
    let fold_keys = after_esc > bar_room;
    look.class_if(fold_esc, "fold-esc");
    look.class_if(fold_keys, "fold-keys");
    if esc_act.is_none() || fold_esc {
        let e = cx.dom.child(head, "div", "pk-esc");
        cx.dom.attr(e, "role", "button");
        cx.dom.attr(e, "data-pk", "act");
        cx.dom.attr(e, "data-v", &escape_action(p));
        cx.dom.child_text(e, "span", "kbd", "esc");
    }
    if ps_menu {
        let menu = cx.dom.child(head, "div", "pk-menu");
        cx.dom.attr(menu, "role", "menu");
        scope_rows(cx, menu, p, &scopes);
    }

    // Body.
    let body = cx.dom.child(sheet, "div", "pk-body");
    if !scopes.is_empty() && !narrow {
        let col = cx.dom.child(body, "div", "pk-scopes");
        cx.dom.attr(col, "role", "tablist");
        scope_rows(cx, col, p, &scopes);
    }
    let center = cx.dom.child(body, "div", "pk-center");
    let main = cx.dom.child(center, "div", "pk-main");
    if !tabs.is_empty() {
        let seg = tabs.len() <= 4;
        let t = cx.dom.child(
            main,
            "div",
            if seg { "pk-tabs seg" } else { "pk-tabs chips" },
        );
        let tin = cx.dom.child(t, "div", "pk-tabs-in");
        cx.dom.attr(tin, "role", "tablist");
        let active = p.get("tab").map(super::layout::value_key);
        for tab in &tabs {
            let tid = tab
                .get("id")
                .map(super::layout::value_key)
                .unwrap_or_default();
            let on = active.as_deref() == Some(tid.as_str());
            let b = cx
                .dom
                .child(tin, "div", if on { "pk-tab on" } else { "pk-tab" });
            cx.dom.attr(b, "role", "tab");
            cx.dom
                .attr(b, "aria-selected", if on { "true" } else { "false" });
            cx.dom.attr(b, "data-pk", "tab");
            cx.dom.attr(b, "data-v", &tid);
            let l = cx.dom.child(b, "span", "l");
            spans_into(cx, l, tab.get("label").or(tab.get("id")));
            if let Some(n) = tab.get("count").and_then(Value::as_f64) {
                cx.dom.child_text(b, "span", "n", &thousands(n));
            }
        }
    }

    // Fact columns that fit (book:6288): the list's width, less the lead, the label's 260px and
    // the check slot; lowest priority hides first. A text column grows to its widest value.
    let mut cols = columns(p);
    for c in cols.iter_mut().filter(|c| c.format == "text") {
        let widest = list
            .iter()
            .filter_map(|e| match e {
                Entry::Item(_, it) => it.get("facts").and_then(|f| f.get(&c.id)),
                _ => None,
            })
            .map(|v| match v {
                Value::Number(n) => compact(n.as_f64().unwrap_or(0.0)).chars().count(),
                v => spans::spans_text(&spans::parse_spans(Some(v)))
                    .chars()
                    .count(),
            })
            .max()
            .unwrap_or(0);
        c.w = c.w.max((widest as f32 * 7.0 + 12.0).min(320.0));
    }
    let side = !scopes.is_empty() && !narrow;
    let pv_side = preview == "side" && !pv_below_auto && !pv_hide;
    let mut list_w = w
        - if side { 200.0 } else { 0.0 }
        - if pv_side {
            (w * 0.38).clamp(260.0, 420.0)
        } else {
            0.0
        };
    list_w -= 16.0 + 28.0 + 24.0;
    let mut shown_cols: Vec<usize> = (0..cols.len()).collect();
    let used = |s: &[usize], cols: &[Column]| s.iter().map(|&i| cols[i].w).sum::<f32>();
    while !shown_cols.is_empty() && list_w - used(&shown_cols, &cols) < LABEL_MIN {
        // The text columns narrow back to their defaults before any column hides.
        if let Some(i) = shown_cols
            .iter()
            .copied()
            .find(|&i| cols[i].format == "text" && cols[i].w > 96.0)
        {
            cols[i].w = 96.0;
            continue;
        }
        let lowest = shown_cols
            .iter()
            .enumerate()
            .min_by(|a, b| {
                cols[*a.1]
                    .priority
                    .partial_cmp(&cols[*b.1].priority)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(b.0.cmp(&a.0))
            })
            .map(|(k, _)| k)
            .unwrap_or(0);
        shown_cols.remove(lowest);
    }
    for i in 0..cols.len() {
        look.class_if(!shown_cols.contains(&i), &format!("h{i}"));
    }
    let any_head = shown_cols.iter().any(|&i| !cols[i].head.is_empty());
    let starts_with_group = matches!(list.first(), Some(Entry::Group { .. }));
    if any_head && !starts_with_group && state == "ready" && shown > 0 {
        let ch = cx.dom.child(main, "div", "pk-colhead");
        cx.dom.child(ch, "span", "pk-colhead-l");
        heads(cx, ch, &cols, &shown_cols);
        if !current.is_empty() {
            cx.dom.child(ch, "span", "pk-cur");
        }
    }

    // The list.
    let lst = cx.dom.child(main, "div", "pk-list");
    let scroll = match kept {
        Some(sc) => sc,
        None => cx.dom.el("div", "pk-scroll pk-spacer"),
    };
    cx.dom.doc.append_child(lst, scroll);
    cx.dom.attr(scroll, "role", "listbox");
    cx.dom.attr(scroll, "aria-label", &name);
    match &selected {
        Some(s)
            if list
                .iter()
                .any(|e| matches!(e, Entry::Item(i, _) if i == s)) =>
        {
            cx.dom
                .attr(scroll, "aria-activedescendant", &format!("pk-{s}"));
        }
        _ => cx.dom.attr_opt(scroll, "aria-activedescendant", None),
    }
    let hits = p.get("hits").and_then(Value::as_object);
    let has_cur = !current.is_empty();
    let show_rows = state == "ready";
    let mut want = Vec::new();
    let mut y = 0.0f32;
    let mut sel_at = None;
    let mut keys_hash = std::collections::hash_map::DefaultHasher::new();
    let mut fresh: HashMap<String, (NodeId, u64)> = HashMap::new();
    let mut old = cx
        .st
        .pickers
        .get_mut(&id)
        .map(|s| std::mem::take(&mut s.rows))
        .unwrap_or_default();
    if kept.is_none() {
        // A new scroller: rows of an old one went with it.
        old.clear();
    }
    let col_sig = {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for &i in &shown_cols {
            (&cols[i].id, &cols[i].format, cols[i].w.to_bits()).hash(&mut h);
        }
        (layout, has_cur).hash(&mut h);
        h.finish()
    };
    if show_rows {
        for e in &list {
            let (key, sig, h) = match e {
                Entry::Group { key, label, count } => {
                    let mut hs = std::collections::hash_map::DefaultHasher::new();
                    (
                        col_sig,
                        any_head,
                        label.map(|l| l.to_string()),
                        count.map(f64::to_bits),
                    )
                        .hash(&mut hs);
                    (format!("g:{key}"), hs.finish(), GROUP_H)
                }
                Entry::Item(iid, it) => {
                    let sel = selected.as_deref() == Some(iid.as_str());
                    let cur = current.contains(iid);
                    let hit = hits.and_then(|h| h.get(iid.as_str()));
                    let mut hs = std::collections::hash_map::DefaultHasher::new();
                    (col_sig, sel, cur).hash(&mut hs);
                    for (k, v) in it.iter() {
                        k.hash(&mut hs);
                        hash_value(v, &mut hs);
                    }
                    match hit {
                        Some(v) => hash_value(v, &mut hs),
                        None => 0u8.hash(&mut hs),
                    }
                    let h = row_h(layout, Some(it));
                    if sel {
                        sel_at = Some((y, y + h));
                    }
                    (format!("i:{iid}"), hs.finish(), h)
                }
            };
            key.hash(&mut keys_hash);
            y += h;
            if fresh.contains_key(&key) {
                continue;
            }
            let el = match old.remove(&key) {
                Some((el, s)) if s == sig && cx.dom.doc.try_node(el).is_some() => el,
                prev => {
                    if let Some((el, _)) = prev
                        && cx.dom.doc.try_node(el).is_some()
                    {
                        cx.dom.doc.remove(el);
                    }
                    match e {
                        Entry::Group { label, count, .. } => group_row(
                            cx,
                            *label,
                            *count,
                            any_head,
                            &cols,
                            &shown_cols,
                            has_cur,
                        ),
                        Entry::Item(iid, it) => {
                            let sel = selected.as_deref() == Some(iid.as_str());
                            let hit = hits.and_then(|h| h.get(iid.as_str()));
                            item_row(
                                cx,
                                iid,
                                it,
                                hit,
                                sel,
                                current.contains(iid),
                                has_cur,
                                layout,
                                &cols,
                                &shown_cols,
                            )
                        }
                    }
                }
            };
            fresh.insert(key, (el, sig));
            want.push(el);
        }
    }
    for (_, (el, _)) in old {
        if cx.dom.doc.try_node(el).is_some() {
            cx.dom.doc.remove(el);
        }
    }
    // The confirm strip (book: `confirm`): over the selected row, a trash icon, the text, a
    // danger button sending `confirm.act` (default "confirm") and Keep sending `cancel`.
    if let Some(old) = cx.st.pickers.get_mut(&id).and_then(|s| s.confirm.take())
        && cx.dom.doc.try_node(old).is_some()
    {
        cx.dom.doc.remove(old);
    }
    let confirm = p.get("confirm").and_then(Value::as_object);
    let mut strip = None;
    let sel_row = selected
        .as_ref()
        .and_then(|s| fresh.get(&format!("i:{s}")))
        .map(|&(el, _)| el);
    if let (Some(c), Some(row)) = (confirm, sel_row.filter(|_| show_rows)) {
        // In the row (absolute, inset 0), so it rides with it through scrolling and
        // virtualization; `data-pk="confirm"` keeps a press on it off the row.
        let el = cx.dom.child(row, "div", "pk-confirm");
        cx.dom.attr(el, "role", "alertdialog");
        cx.dom.attr(el, "data-pk", "confirm");
        let ic = cx.dom.child(el, "span", "pk-ct-ic");
        icon(cx, ic, "trash");
        let t = cx.dom.child(el, "span", "pk-ct");
        m.clock |= spans_into(cx, t, c.get("text"));
        let label = crate::events::text_content(cx.dom.doc, t);
        cx.dom.attr(el, "aria-label", &label);
        let act = str_prop(c, "act").filter(|a| !a.is_empty()).unwrap_or("confirm");
        let label = str_prop(c, "label").filter(|l| !l.is_empty()).unwrap_or("Delete");
        for (class, v, l) in [("pk-btn danger", act, label), ("pk-btn", "cancel", "Keep")] {
            let b = cx.dom.child(el, "div", class);
            cx.dom.attr(b, "role", "button");
            cx.dom.attr(b, "data-pk", "act");
            cx.dom.attr(b, "data-v", v);
            cx.dom.child_text(b, "span", "l", l);
        }
        strip = Some(el);
    }
    cx.dom.set_class(
        scroll,
        if strip.is_some() {
            "pk-scroll pk-spacer confirming"
        } else {
            "pk-scroll pk-spacer"
        },
    );
    cx.dom.reconcile(scroll, &want);
    super::virtualize(
        cx,
        &id,
        scroll,
        (want.len() > VIRTUAL_AFTER).then_some(row_h(layout, None)),
    );

    // States (book:6305): loading skeletons, the error card, the empty texts.
    if state == "loading" {
        let st = cx.dom.child(main, "div", "pk-state loading");
        for _ in 0..8 {
            let r = cx.dom.child(st, "div", "pk-sk");
            cx.dom.child(r, "span", "pk-sk-l");
        }
    } else if state == "error" {
        let st = cx.dom.child(main, "div", "pk-state error");
        let card = cx.dom.child(st, "div", "pk-err");
        cx.dom.attr(card, "role", "alert");
        let ic = cx.dom.child(card, "span", "pk-err-ic");
        icon(cx, ic, "alert");
        let t = cx.dom.child(card, "span", "pk-err-t");
        spans_into(cx, t, p.get("message"));
        if let Some(a) = acts.iter().find(|a| a.id == "retry") {
            state_button(cx, card, a);
        }
    } else if shown == 0 {
        let st = cx.dom.child(main, "div", "pk-state empty");
        let e = cx.dom.child(st, "div", "pk-empty");
        let q = query.unwrap_or("");
        if !q.is_empty() {
            cx.dom
                .child_text(e, "span", "pk-empty-t", &format!("No {noun} match “{q}”"));
            let b = cx.dom.child(e, "div", "pk-btn");
            cx.dom.attr(b, "role", "button");
            cx.dom.attr(b, "data-pk", "act");
            cx.dom.attr(b, "data-v", "clear");
            cx.dom.child_text(b, "span", "l", "Clear search");
        } else {
            let t = cx.dom.child(e, "span", "pk-empty-t");
            match p.get("empty").filter(|v| !v.is_null()) {
                Some(v) => {
                    spans_into(cx, t, Some(v));
                }
                None => {
                    cx.dom.text(t, &format!("No {noun} yet"));
                }
            }
            if let Some(a) = acts.iter().find(|a| a.keys.iter().any(|k| k == "tab")) {
                state_button(cx, e, a);
            }
        }
    }

    // Preview.
    if preview != "none" {
        let pv = cx.dom.child(center, "div", "pk-pv");
        if node.child_count() == 0 {
            cx.dom
                .child_text(pv, "span", "pk-pv-empty", "Nothing to preview");
        }
        let s = slot(cx, m, 0, "div", "pk-pv-slot");
        cx.dom.doc.append_child(pv, s);
    } else {
        // Children still mount (hidden) so their state survives a preview switch.
        let s = slot(cx, m, 0, "div", "pk-pv-slot sf-void");
        cx.dom.doc.append_child(center, s);
    }

    // Strip (above the bar, always visible) and the action bar.
    if let Some(strip) = p.get("strip").and_then(Value::as_object) {
        let st = cx.dom.child(sheet, "div", "pk-strip");
        cx.dom.attr(st, "role", "toolbar");
        if strip.get("label").is_some_and(|l| !l.is_null()) {
            let l = cx.dom.child(st, "span", "pk-strip-l");
            m.clock |= spans_into(cx, l, strip.get("label"));
        }
        let chips = cx.dom.child(st, "div", "pk-strip-in");
        let sel = strip.get("selected").and_then(item_id);
        for c in strip
            .get("items")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(o) = c.as_object() else { continue };
            let Some(cid) = o.get("id").and_then(item_id) else {
                continue;
            };
            let on = bool_prop(o, "on");
            let is_sel = sel.as_deref() == Some(cid.as_str());
            let mut class = String::from("pk-schip");
            if on {
                class.push_str(" on");
            }
            if is_sel {
                class.push_str(" sel");
            }
            let b = cx.dom.child(chips, "div", &class);
            cx.dom.attr(b, "role", "switch");
            cx.dom
                .attr(b, "aria-checked", if on { "true" } else { "false" });
            cx.dom.attr(b, "data-pk", "strip");
            cx.dom.attr(b, "data-v", &cid);
            let ck = cx.dom.child(b, "span", "pk-schk");
            if on {
                icon(cx, ck, "check");
            }
            if let Some(d) = str_prop(o, "dot") {
                dot(cx, b, d, "");
            }
            let l = cx.dom.child(b, "span", "l");
            spans_into(cx, l, o.get("label").or(o.get("id")));
        }
    }
    let bar = cx.dom.child(sheet, "div", "pk-bar");
    cx.dom.attr(bar, "role", "toolbar");
    let secondary: Vec<&Action> = acts.iter().filter(|a| !a.primary && !a.end).collect();
    let ends: Vec<&Action> = acts.iter().filter(|a| !a.primary && a.end).collect();
    let primary: Vec<&Action> = acts.iter().filter(|a| a.primary).collect();
    for a in &secondary {
        if fold_esc && a.esc() {
            continue;
        }
        bar_button(cx, bar, a, !fold_keys);
    }
    cx.dom.child(bar, "span", "pk-bar-fill");
    for a in ends.iter().chain(primary.iter()) {
        if fold_esc && a.esc() && !a.primary {
            continue;
        }
        bar_button(cx, bar, a, a.primary || !fold_keys);
    }

    // Keep the selection in view (book:6286): on open, on a layout change, when `selected`
    // changes, and when rows change while the selection was visible.
    let order_hash = keys_hash.finish();
    let ps = cx.st.pickers.entry(id.clone()).or_default();
    let first = kept.is_none();
    let sel_changed = ps.selected != selected;
    let layout_changed = ps.layout != layout;
    let rows_changed = ps.order_hash != order_hash;
    let was_visible = ps.sel_at.is_some_and(|(t, b)| {
        let d = &cx.dom.doc;
        if d.try_node(scroll).is_none() || !d.laid_out(scroll) {
            return true;
        }
        let (_, sy) = d.scroll_offset(scroll);
        let h = d.border_box(scroll).map(|r| r.h).unwrap_or(0.0);
        b > sy && t < sy + h
    });
    if let Some(at) = sel_at
        && (first || sel_changed || layout_changed || (rows_changed && was_visible))
    {
        ps.reveal = Some(at);
    }
    ps.rows = fresh;
    ps.scroll = Some(scroll);
    ps.confirm = strip;
    ps.selected = selected;
    ps.layout = layout.to_owned();
    ps.order_hash = order_hash;
    ps.sel_at = sel_at;
}

fn scope_rows(cx: &mut Cx, parent: NodeId, p: &Map<String, Value>, scopes: &[&Map<String, Value>]) {
    let active = p.get("scope").map(super::layout::value_key);
    let mut group: Option<Option<String>> = None;
    for s in scopes {
        let sid = s
            .get("id")
            .map(super::layout::value_key)
            .unwrap_or_default();
        let g = str_prop(s, "group").map(str::to_owned);
        match (&group, &g) {
            (Some(prev), Some(now)) if prev.as_ref() == Some(now) => {}
            (_, Some(now)) => {
                cx.dom.child_text(parent, "div", "pk-sgh", now);
            }
            (Some(Some(_)), None) => {
                cx.dom.child(parent, "div", "pk-ssep");
            }
            _ => {}
        }
        group = Some(g);
        let on = active.as_deref() == Some(sid.as_str());
        let dis = reason(s.get("disabled"));
        let mut class = String::from("pk-scope nav-row");
        if on {
            class.push_str(" on");
        }
        if dis.is_some() {
            class.push_str(" dis");
        }
        let r = cx.dom.child(parent, "div", &class);
        cx.dom.attr(r, "role", "tab");
        cx.dom
            .attr(r, "aria-selected", if on { "true" } else { "false" });
        cx.dom.attr(r, "data-pk", "scope");
        cx.dom.attr(r, "data-v", &sid);
        if let Some(t) = dis.as_deref().filter(|t| !t.is_empty()) {
            cx.dom.attr(r, "title", t);
        }
        let lead = cx.dom.child(r, "span", "pk-lead");
        if let Some(mk) = s.get("mark").and_then(Value::as_object) {
            let text = str_prop(mk, "text").unwrap_or("");
            let mark = cx.dom.child_text(lead, "span", "pk-mark", &initials(text));
            cx.dom
                .set_style(mark, &mark_style(str_prop(mk, "seed").unwrap_or(text)));
        } else if let Some(n) = str_prop(s, "icon") {
            icon(cx, lead, n);
        }
        let l = cx.dom.child(r, "span", "pk-sl");
        spans_into(cx, l, s.get("label").or(s.get("id")));
        if let Some(d) = str_prop(s, "dot") {
            dot(cx, r, d, "");
        }
        if dis.is_some() {
            let lk = cx.dom.child(r, "span", "pk-lock");
            icon(cx, lk, "lock");
        } else if let Some(n) = s.get("count").and_then(Value::as_f64) {
            cx.dom.child_text(r, "span", "n", &thousands(n));
        }
    }
}

fn heads(cx: &mut Cx, parent: NodeId, cols: &[Column], shown: &[usize]) {
    let f = cx.dom.child(parent, "span", "pk-facts");
    for &i in shown {
        let c = &cols[i];
        let h = cx.dom.child_text(
            f,
            "span",
            &format!("pk-fact c{i} f-{} head", c.format),
            &c.head,
        );
        cx.dom
            .set_style(h, &format!("width: {}px", fmt_num(f64::from(c.w))));
    }
}

fn group_row(
    cx: &mut Cx,
    label: Option<&Value>,
    count: Option<f64>,
    with_heads: bool,
    cols: &[Column],
    shown: &[usize],
    has_cur: bool,
) -> NodeId {
    let r = cx.dom.el("div", "pk-row grp");
    cx.dom.attr(r, "role", "presentation");
    cx.dom.attr(r, "data-pk", "grp");
    let l = cx.dom.child(r, "span", "pk-gl");
    spans_into(cx, l, label);
    if let Some(n) = count {
        cx.dom.child_text(r, "span", "pk-gc", &thousands(n));
    }
    cx.dom.child(r, "span", "pk-grow");
    if with_heads {
        heads(cx, r, cols, shown);
        if has_cur {
            cx.dom.child(r, "span", "pk-cur");
        }
    }
    r
}

#[allow(clippy::too_many_arguments)]
fn item_row(
    cx: &mut Cx,
    iid: &str,
    it: &Map<String, Value>,
    hit: Option<&Value>,
    sel: bool,
    cur: bool,
    has_cur: bool,
    layout: &str,
    cols: &[Column],
    shown: &[usize],
) -> NodeId {
    let dis = disabled(it);
    let mut class = String::from("pk-row");
    if sel {
        class.push_str(" sel");
    }
    if cur {
        class.push_str(" cur");
    }
    if dis.is_some() {
        class.push_str(" off");
    }
    let r = cx.dom.el("div", &class);
    cx.dom.attr(r, "role", "option");
    cx.dom
        .attr(r, "aria-selected", if sel { "true" } else { "false" });
    cx.dom.attr(r, "data-pk", "row");
    cx.dom.attr(r, "data-v", iid);
    cx.dom.attr(r, "id", &format!("pk-{iid}"));
    if dis.is_some() {
        cx.dom.attr(r, "aria-disabled", "true");
    }
    if let Some(t) = crate::common::tone(it) {
        cx.dom.attr(r, "data-tone", t);
    }
    let title = str_prop(it, "title")
        .map(str::to_owned)
        .or_else(|| dis.clone().filter(|d| !d.is_empty()));
    if let Some(t) = title {
        cx.dom.attr(r, "title", &t);
    }
    let node_style = str_prop(it, "node").unwrap_or("assistant");
    if layout == "timeline" {
        cx.dom.attr(r, "data-node", node_style);
    }
    // Lead: tree guides and chevron, a timeline node, or a mark / icon / dot.
    if layout == "tree" {
        let depth = it.get("depth").and_then(Value::as_u64).unwrap_or(0).min(16);
        if depth > 0 {
            let accent = str_prop(it, "dot") == Some("accent");
            let g = cx.dom.child(
                r,
                "span",
                if accent {
                    "pk-guides accent"
                } else {
                    "pk-guides"
                },
            );
            for _ in 0..depth {
                cx.dom.child(g, "span", "pk-g");
            }
        }
        let ch = cx.dom.child(
            r,
            "span",
            match it.get("open") {
                Some(Value::Bool(true)) => "pk-chev open",
                Some(Value::Bool(false)) => "pk-chev",
                _ => "pk-chev leaf",
            },
        );
        if matches!(it.get("open"), Some(Value::Bool(_))) {
            icon(cx, ch, "chevron-right");
        }
    }
    if layout == "timeline" {
        let n = cx.dom.child(
            r,
            "span",
            &format!(
                "pk-node n-{}",
                if valid_name(node_style) {
                    node_style
                } else {
                    "assistant"
                }
            ),
        );
        if node_style == "tool"
            && let Some(name) = str_prop(it, "icon")
                .or_else(|| str_prop(it, "role").and_then(super::layout::role_icon))
        {
            icon(cx, n, name);
        }
    } else {
        let lead = cx.dom.child(r, "span", "pk-lead");
        let mut has_visual = false;
        if let Some(mk) = it.get("mark").and_then(Value::as_object) {
            let text = str_prop(mk, "text").unwrap_or("");
            let mark = cx.dom.child_text(lead, "span", "pk-mark", &initials(text));
            cx.dom
                .set_style(mark, &mark_style(str_prop(mk, "seed").unwrap_or(text)));
            has_visual = true;
        } else if let Some(n) =
            str_prop(it, "icon").or_else(|| str_prop(it, "role").and_then(super::layout::role_icon))
        {
            icon(cx, lead, n);
            has_visual = true;
        }
        if let Some(d) = str_prop(it, "dot")
            && layout != "tree"
        {
            dot(cx, lead, d, if has_visual { " corner" } else { "" });
        }
    }
    // Text.
    let txt = cx.dom.child(r, "div", "pk-txt");
    let l1 = cx.dom.child(txt, "div", "pk-l1");
    let label_spans = spans::parse_spans(it.get("label"));
    let plain = label_spans
        .iter()
        .all(|s| s.s.is_empty() && s.href.is_none());
    let text = spans::spans_text(&label_spans);
    let mono = bool_prop(it, "mono");
    let marks: Vec<(usize, usize)> = {
        let src = hit.or(it.get("hits"));
        ranges(src)
            .into_iter()
            .map(|(a, b)| (utf16_to_char(&text, a), utf16_to_char(&text, b)))
            .filter(|(a, b)| b > a)
            .collect()
    };
    if mono && plain && text.contains('/') {
        let lab = cx.dom.child(l1, "span", "pk-label mono path");
        let cut = text
            .rfind('/')
            .map(|i| text[..=i].chars().count())
            .unwrap_or(0);
        let chars: Vec<char> = text.chars().collect();
        let dir = cx.dom.child(lab, "span", "dir");
        marked(cx, dir, &chars, 0, cut, &marks);
        let name = cx.dom.child(lab, "span", "name");
        marked(cx, name, &chars, cut, chars.len(), &marks);
    } else {
        let lab = cx
            .dom
            .child(l1, "span", if mono { "pk-label mono" } else { "pk-label" });
        if marks.is_empty() {
            spans::render(&mut cx.dom, lab, &label_spans);
        } else if plain {
            let chars: Vec<char> = text.chars().collect();
            marked(cx, lab, &chars, 0, chars.len(), &marks);
        } else {
            marked_spans(cx, lab, &label_spans, &marks);
        }
    }
    // Badges (3, then +N) and chips.
    let badges: Vec<&Map<String, Value>> = it
        .get("badges")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .collect();
    let chips: Vec<&Map<String, Value>> = it
        .get("chips")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .collect();
    if !badges.is_empty() || !chips.is_empty() {
        let tags = cx.dom.child(l1, "span", "pk-tags");
        for b in badges.iter().take(3) {
            let e = cx
                .dom
                .child_text(tags, "span", "pk-badge", str_prop(b, "text").unwrap_or(""));
            if let Some(t) = crate::common::tone(b) {
                cx.dom.attr(e, "data-tone", t);
            }
            if let Some(t) = str_prop(b, "title") {
                cx.dom.attr(e, "title", t);
            }
        }
        if badges.len() > 3 {
            let rest: Vec<&str> = badges[3..]
                .iter()
                .filter_map(|b| str_prop(b, "text"))
                .collect();
            let e = cx.dom.child_text(
                tags,
                "span",
                "pk-badge more",
                &format!("+{}", badges.len() - 3),
            );
            cx.dom.attr(e, "title", &rest.join(", "));
        }
        for c in &chips {
            let mut class = String::from("pk-chip");
            if bool_prop(c, "on") {
                class.push_str(" on");
            }
            if bool_prop(c, "auto") {
                class.push_str(" auto");
            }
            let e = cx.dom.child(tags, "span", &class);
            if let Some(d) = str_prop(c, "dot") {
                dot(cx, e, d, "");
            }
            cx.dom.text(e, str_prop(c, "text").unwrap_or(""));
        }
    }
    if it.get("detail").is_some_and(|d| !d.is_null()) {
        let parent = if layout == "cards" {
            cx.dom.child(txt, "div", "pk-l2")
        } else {
            l1
        };
        let d = cx.dom.child(parent, "span", "pk-detail");
        let t = cx.dom.child(d, "span", "t");
        spans_into(cx, t, it.get("detail"));
    }
    // Facts.
    if !shown.is_empty() {
        let f = cx.dom.child(r, "span", "pk-facts");
        let facts = it.get("facts").and_then(Value::as_object);
        for &i in shown {
            let c = &cols[i];
            let v = facts.and_then(|f| f.get(&c.id)).filter(|v| !v.is_null());
            let e = cx
                .dom
                .child(f, "span", &format!("pk-fact c{i} f-{}", c.format));
            cx.dom
                .set_style(e, &format!("width: {}px", fmt_num(f64::from(c.w))));
            match v {
                Some(Value::Number(n)) if c.format == "bar" => {
                    let x = n.as_f64().unwrap_or(0.0).clamp(0.0, 1.0);
                    let mt = cx.dom.child(e, "span", "pk-meter");
                    let fill = cx.dom.child(mt, "span", "pk-meter-f");
                    cx.dom
                        .set_style(fill, &format!("width: {}%", fmt_num(x * 100.0)));
                    if let Some(t) = str_prop(it, "title") {
                        cx.dom.attr(e, "title", t);
                    }
                }
                Some(Value::Number(n)) => {
                    let s = compact(n.as_f64().unwrap_or(0.0));
                    cx.dom.text(e, &s);
                }
                Some(v) => {
                    let text = spans::spans_text(&spans::parse_spans(Some(v)));
                    spans_into(cx, e, Some(v));
                    // A value still cut short shows whole as the tooltip (book: Fact columns),
                    // by the estimate the column widths use (7px a character, 12px padding).
                    if text.chars().count() as f32 * 7.0 + 12.0 > c.w {
                        cx.dom.attr(e, "title", &text);
                    }
                }
                None => {}
            }
        }
    }
    if has_cur {
        let c = cx.dom.child(r, "span", "pk-cur");
        if cur {
            icon(cx, c, "check");
        }
    }
    r
}

/// `chars[from..to]` with the parts inside `marks` (char ranges) in `mark.sf-mark`.
/// Styled `spans` with the parts inside `marks` (char ranges over their text) in
/// `mark.sf-mark`, each piece keeping its span's style.
fn marked_spans(cx: &mut Cx, parent: NodeId, spans_in: &[spans::Span], marks: &[(usize, usize)]) {
    let in_mark = |i: usize| marks.iter().any(|&(a, b)| i >= a && i < b);
    let mut at = 0usize;
    for sp in spans_in {
        let chars: Vec<char> = sp.t.chars().collect();
        let mut from = 0;
        while from < chars.len() {
            let on = in_mark(at + from);
            let mut to = from + 1;
            while to < chars.len() && in_mark(at + to) == on {
                to += 1;
            }
            let piece = spans::Span {
                t: chars[from..to].iter().collect(),
                ..sp.clone()
            };
            let into = if on {
                cx.dom.child(parent, "mark", "sf-mark")
            } else {
                parent
            };
            spans::render(&mut cx.dom, into, &[piece]);
            from = to;
        }
        at += chars.len();
    }
}

fn marked(
    cx: &mut Cx,
    parent: NodeId,
    chars: &[char],
    from: usize,
    to: usize,
    marks: &[(usize, usize)],
) {
    let mut at = from;
    let mut sorted: Vec<(usize, usize)> = marks
        .iter()
        .map(|&(a, b)| (a.max(from), b.min(to)))
        .filter(|(a, b)| b > a)
        .collect();
    sorted.sort_unstable();
    for (a, b) in sorted {
        let a = a.max(at);
        if b <= a {
            continue;
        }
        if a > at {
            cx.dom
                .text(parent, &chars[at..a].iter().collect::<String>());
        }
        let mk = cx.dom.child(parent, "mark", "sf-mark");
        cx.dom.text(mk, &chars[a..b].iter().collect::<String>());
        at = b;
    }
    if at < to {
        cx.dom
            .text(parent, &chars[at..to].iter().collect::<String>());
    }
}

fn bar_button(cx: &mut Cx, bar: NodeId, a: &Action, keys: bool) {
    let mut class = String::from("pk-btn");
    for (on, c) in [
        (a.primary, " primary"),
        (a.danger, " danger"),
        (a.on, " on"),
        (a.disabled.is_some(), " off"),
        (a.esc(), " esc"),
        (a.end, " end"),
    ] {
        if on {
            class.push_str(c);
        }
    }
    let b = cx.dom.child(bar, "div", &class);
    cx.dom.attr(b, "role", "button");
    cx.dom.attr(b, "data-pk", "act");
    cx.dom.attr(b, "data-v", &a.id);
    if a.on {
        cx.dom.attr(b, "aria-pressed", "true");
    }
    if let Some(r) = &a.disabled {
        cx.dom.attr(b, "aria-disabled", "true");
        if !r.is_empty() {
            cx.dom.attr(b, "title", r);
        }
    }
    let l = cx.dom.child(b, "span", "l");
    spans_into(cx, l, Some(&a.label));
    if keys {
        keycaps(cx, b, &a.keys);
    }
}

/// The empty or error state's button: an action of the bar, disabled (`.off`, the reason as its
/// tooltip, its clicks sending nothing) as it is there.
fn state_button(cx: &mut Cx, parent: NodeId, a: &Action) {
    let b = cx.dom.child(
        parent,
        "div",
        if a.disabled.is_some() {
            "pk-btn off"
        } else {
            "pk-btn"
        },
    );
    cx.dom.attr(b, "role", "button");
    cx.dom.attr(b, "data-pk", "act");
    cx.dom.attr(b, "data-v", &a.id);
    if let Some(r) = &a.disabled {
        cx.dom.attr(b, "aria-disabled", "true");
        if !r.is_empty() {
            cx.dom.attr(b, "title", r);
        }
    }
    let l = cx.dom.child(b, "span", "l");
    spans_into(cx, l, Some(&a.label));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn numbers_show_compact_and_counts_with_separators() {
        assert_eq!(compact(272_000.0), "272k");
        assert_eq!(compact(1_600_000.0), "1.6M");
        assert_eq!(compact(500_000.0), "500k");
        assert_eq!(compact(3.0), "3");
        assert_eq!(compact(1_000.0), "1k");
        assert_eq!(compact(999_950.0), "1M");
        assert_eq!(compact(999.96), "1k");
        assert_eq!(compact(9_960.0), "10k");
        assert_eq!(compact(-999_950.0), "-1M");
        assert_eq!(thousands(1604.0), "1,604");
        assert_eq!(thousands(47.0), "47");
        assert_eq!(thousands(1_234_567.0), "1,234,567");
    }

    #[test]
    fn the_catalog_applies_adds_and_deletes_to_items() {
        let p = json!({
            "items": [{"id": "a", "label": "A"}, {"id": "b", "label": "B"}, {"label": "no id"}],
            "itemsAdd": [{"id": "b", "label": "B2"}, {"id": "c", "label": "C"}],
            "itemsDel": ["a"],
        });
        let cat = catalog_list(p.as_object().unwrap());
        let ids: Vec<&str> = cat.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids, ["b", "c"]);
        assert_eq!(cat[0].1["label"], "B2");
    }

    #[test]
    fn order_skips_unknown_ids_and_keeps_group_headers() {
        let p = json!({
            "items": [{"id": "a", "label": "A"}, {"id": "b", "label": "B"}],
            "order": [{"group": "g", "label": "G", "count": 2}, "b", "zz", "a", "b"],
        });
        let p = p.as_object().unwrap();
        let cat = catalog_list(p);
        let e = entries(p, &cat);
        let keys: Vec<String> = e
            .iter()
            .map(|e| match e {
                Entry::Group { key, .. } => format!("g:{key}"),
                Entry::Item(id, _) => id.clone(),
            })
            .collect();
        assert_eq!(keys, ["g:g", "b", "a"]);
    }

    #[test]
    fn utf16_offsets_map_to_chars() {
        assert_eq!(utf16_to_char("a😀b", 0), 0);
        assert_eq!(utf16_to_char("a😀b", 1), 1);
        assert_eq!(utf16_to_char("a😀b", 3), 2);
        assert_eq!(utf16_to_char("a😀b", 9), 3);
    }

    #[test]
    fn the_escape_action_is_the_one_bound_to_escape() {
        let p = json!({"actions": [{"id": "use", "keys": ["enter"]}, {"id": "quit", "keys": ["escape"]}]});
        assert_eq!(escape_action(p.as_object().unwrap()), "quit");
        assert_eq!(escape_action(json!({}).as_object().unwrap()), "close");
    }
}
