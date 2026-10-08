//! Text and data kinds: `text badge kbd icon kv table tree` (research/elements.md §2.2, §2.3).

use super::layout::value_key;
use super::{begin, icon, part, spans_into};
use crate::common::{num_prop, str_prop};
use crate::spans;
use crate::state::SurfaceState;
use crate::view::{Cx, Look, Mount};
use auk_tsp::NodeRef;
use auk_ui::NodeId;
use serde_json::Value;

/// How a `text` node is cut after layout (§8.3 measure-then-layout).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Trunc {
    pub el: NodeId,
    pub full: String,
    pub mode: String,
}

pub(crate) fn text(cx: &mut Cx, node: NodeRef, m: &mut Mount, look: &mut Look) {
    begin(cx, m);
    m.direct = false;
    let p = node.props();
    let wrap = str_prop(p, "wrap").unwrap_or("word");
    let lines = num_prop(p, "lines").filter(|l| *l >= 1.0).map(|l| l as u32);
    let truncate = str_prop(p, "truncate");
    let one_line = wrap == "none" || lines == Some(1);
    look.class_if(one_line, "nowrap");
    look.class_if(wrap == "char", "char");
    look.class_if(str_prop(p, "measure") == Some("prose"), "prose");
    if let Some(n) = lines.filter(|n| *n > 1) {
        look.class("clamp");
        look.style(&format!("line-clamp: {n};"));
    }
    let v = match p.get("spans") {
        Some(a @ Value::Array(_)) => Some(a),
        _ => p.get("text"),
    };
    let sp = spans::parse_spans(v);
    let full = spans::spans_text(&sp);
    let cut = one_line && matches!(truncate, Some("start" | "middle"));
    if cut {
        look.class(&format!("tr-{}", truncate.unwrap()));
    }
    // The full text becomes the title when the text is cut.
    if (one_line || lines.is_some()) && str_prop(p, "title").is_none() && !full.is_empty() {
        look.attr("title", full.clone());
    }
    let holder = m.el;
    let mut fx = false;
    {
        let before: Vec<NodeId> = cx.dom.doc.children(holder).to_vec();
        fx |= spans::render(&mut cx.dom, holder, &sp);
        let after = cx.dom.doc.children(holder).to_vec();
        m.generated
            .extend(after.into_iter().filter(|c| !before.contains(c)));
    }
    m.clock |= fx;
    if cut {
        cx.st.truncs.insert(
            node.id().to_owned(),
            Trunc {
                el: m.el,
                full,
                mode: truncate.unwrap().to_owned(),
            },
        );
    } else {
        cx.st.truncs.remove(node.id());
    }
}

/// Cut text to `cells` characters: `start` keeps the end, `middle` keeps both ends (a path drops
/// directories first).
pub(crate) fn cut(full: &str, cells: usize, mode: &str) -> String {
    let chars: Vec<char> = full.chars().collect();
    if chars.len() <= cells || cells == 0 {
        return full.to_owned();
    }
    let keep = cells.saturating_sub(1);
    match mode {
        "start" => {
            let tail: String = chars[chars.len() - keep..].iter().collect();
            format!("…{tail}")
        }
        _ => {
            if full.contains('/') {
                // Drop leading directories: …/rest.
                let parts: Vec<&str> = full.split('/').collect();
                for i in 1..parts.len() {
                    let rest = parts[i..].join("/");
                    let cand = format!("…/{rest}");
                    if cand.chars().count() <= cells {
                        return cand;
                    }
                }
            }
            let head = keep / 2;
            let tail = keep - head;
            let a: String = chars[..head].iter().collect();
            let b: String = chars[chars.len() - tail..].iter().collect();
            format!("{a}…{b}")
        }
    }
}

pub(crate) fn badge(cx: &mut Cx, node: NodeRef, m: &mut Mount, _look: &mut Look) {
    begin(cx, m);
    let t = match node.prop("text") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    };
    if !t.is_empty() {
        let tn = cx.dom.text(m.el, &t);
        m.generated.push(tn);
    }
}

pub(crate) fn kbd(cx: &mut Cx, node: NodeRef, m: &mut Mount, look: &mut Look) {
    begin(cx, m);
    look.class("kbds");
    let keys: Vec<&str> = node
        .prop("keys")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut spoken = Vec::new();
    for k in &keys {
        let (cap, sp) = crate::icons::keycap(k);
        spoken.push(sp.map(str::to_owned).unwrap_or_else(|| cap.clone()));
        let s = part(cx, m, "span", "kbd");
        cx.dom.text(s, &cap);
    }
    if str_prop(node.props(), "aria").is_none() && !keys.is_empty() {
        look.attr("role", "img");
        look.attr("aria-label", spoken.join(" "));
    }
}

pub(crate) fn icon_kind(cx: &mut Cx, node: NodeRef, m: &mut Mount, look: &mut Look) {
    begin(cx, m);
    if str_prop(node.props(), "aria").is_none() {
        look.attr("aria-hidden", "true");
    }
    let name = str_prop(node.props(), "name").unwrap_or("");
    let el = m.el;
    let s = icon(cx, el, name);
    m.generated.push(s);
}

pub(crate) fn kv(cx: &mut Cx, node: NodeRef, m: &mut Mount, look: &mut Look) {
    begin(cx, m);
    let p = node.props();
    let inline = str_prop(p, "layout") == Some("inline");
    look.class_if(inline, "inline");
    let items = p
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut widest = 0usize;
    for (i, it) in items.iter().enumerate() {
        let Some(o) = it.as_object() else { continue };
        let k = spans::parse_spans(o.get("k"));
        widest = widest.max(spans::spans_text(&k).chars().count());
        let row = part(
            cx,
            m,
            "div",
            if i == 0 {
                "sf-kv-item first"
            } else {
                "sf-kv-item"
            },
        );
        if inline {
            let sep = cx.dom.child_text(row, "span", "sf-kv-sep", "·");
            cx.dom.attr(sep, "aria-hidden", "true");
        }
        let kd = cx.dom.child(row, "div", "sf-kv-k");
        m.clock |= spans::render(&mut cx.dom, kd, &k);
        let vd = cx.dom.child(row, "div", "sf-kv-v");
        m.clock |= spans_into(cx, vd, o.get("v"));
    }
    // Long lists virtualize (§8.3): each item lays out as its own grid row on the shared columns,
    // in a stack only the visible part of which is laid out.
    let virt = !inline && items.len() > VIRTUAL_ROWS;
    look.class_if(virt, "virt");
    if !inline {
        let cols = format!("calc({widest}*var(--sf-cw)) minmax(0, 1fr)");
        look.style(&format!(
            "grid-template-columns: {cols}; --sf-kv-cols: {cols};"
        ));
    }
    super::virtualize(cx, node.id(), m.el, virt.then_some(19.0));
}

/// `kv` items, `table` rows and `tree` items past this many virtualize (§8.3; Tern's `list`
/// threshold).
pub(crate) const VIRTUAL_ROWS: usize = 200;

/// Column sizing input for a table (book:5522).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TableCol {
    pub natural: usize,
    pub priority: f64,
    pub grow: f64,
    pub cells: Vec<NodeId>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TableInfo {
    pub grid: NodeId,
    pub cols: Vec<TableCol>,
}

fn meter_cell(cx: &mut Cx, parent: NodeId, v: &serde_json::Map<String, Value>) {
    let value = v
        .get("value")
        .and_then(Value::as_f64)
        .unwrap_or(0.0)
        .clamp(0.0, 1.0);
    let level = crate::motion::meter_level(value, v.get("thresholds"));
    let mt = cx.dom.child(parent, "div", "sf-meter");
    cx.dom.attr(mt, "data-style", "bar");
    cx.dom.attr(mt, "data-size", "sm");
    if let Some(l) = level {
        cx.dom.attr(mt, "data-level", l);
    }
    cx.dom.attr(mt, "role", "meter");
    cx.dom.attr(mt, "aria-valuemin", "0");
    cx.dom.attr(mt, "aria-valuemax", "100");
    cx.dom
        .attr(mt, "aria-valuenow", &format!("{}", (value * 100.0).round()));
    if let Some(t) = v.get("title").and_then(Value::as_str) {
        cx.dom.attr(mt, "title", t);
    }
    let track = cx.dom.child(mt, "div", "sf-mt-track");
    let fill = cx.dom.child(track, "div", "sf-mt-fill");
    cx.dom.set_style(
        fill,
        &format!("width: {}%", crate::common::fmt_num(value * 100.0)),
    );
}

pub(crate) fn table(cx: &mut Cx, node: NodeRef, m: &mut Mount, _look: &mut Look) {
    begin(cx, m);
    let p = node.props();
    let cols: Vec<serde_json::Map<String, Value>> = p
        .get("cols")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|c| c.as_object().cloned()).collect())
        .unwrap_or_default();
    let rows: Vec<serde_json::Map<String, Value>> = p
        .get("rows")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|c| c.as_object().cloned()).collect())
        .unwrap_or_default();
    let virt = rows.len() > VIRTUAL_ROWS;
    let grid = part(cx, m, "div", if virt { "sf-tbl virt" } else { "sf-tbl" });
    cx.dom.attr(grid, "role", "table");
    cx.dom
        .attr(grid, "aria-rowcount", &(rows.len() + 1).to_string());
    let mut info = TableInfo {
        grid,
        cols: cols
            .iter()
            .map(|c| TableCol {
                natural: 0,
                priority: c.get("priority").and_then(Value::as_f64).unwrap_or(0.0),
                grow: c
                    .get("grow")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0)
                    .max(0.0),
                cells: Vec::new(),
            })
            .collect(),
    };
    let align_class =
        |c: &serde_json::Map<String, Value>| match c.get("align").and_then(Value::as_str) {
            Some("end" | "right") => " ta-end",
            Some("center") => " ta-center",
            _ => "",
        };
    let head = cx.dom.child(grid, "div", "sf-tbl-row head");
    cx.dom.attr(head, "role", "row");
    cx.dom.attr(head, "aria-rowindex", "1");
    for (i, c) in cols.iter().enumerate() {
        let h = cx
            .dom
            .child(head, "div", &format!("sf-tbl-h{}", align_class(c)));
        cx.dom.attr(h, "role", "columnheader");
        let sp = spans::parse_spans(c.get("head").or(c.get("id")));
        info.cols[i].natural = spans::spans_text(&sp).chars().count();
        spans::render(&mut cx.dom, h, &sp);
        info.cols[i].cells.push(h);
    }
    for (r, row) in rows.iter().enumerate() {
        let last = r + 1 == rows.len();
        let re = cx.dom.child(
            grid,
            "div",
            if last {
                "sf-tbl-row last"
            } else {
                "sf-tbl-row"
            },
        );
        cx.dom.attr(re, "role", "row");
        cx.dom.attr(re, "aria-rowindex", &(r + 2).to_string());
        if let Some(id) = row.get("id") {
            cx.dom.attr(re, "data-key", &value_key(id));
        }
        let cells = row.get("cells").and_then(Value::as_object);
        for (i, c) in cols.iter().enumerate() {
            let cid = c.get("id").map(value_key).unwrap_or_default();
            let v = cells.and_then(|cs| cs.get(&cid));
            let trunc = match c.get("truncate").and_then(Value::as_str) {
                Some("start") => " tr-start",
                Some("middle") => " tr-middle",
                _ => "",
            };
            match v.and_then(|v| v.get("meter")).and_then(Value::as_object) {
                Some(mt) => {
                    let ce = cx
                        .dom
                        .child(re, "div", &format!("sf-tbl-c meter{}", align_class(c)));
                    cx.dom.attr(ce, "role", "cell");
                    meter_cell(cx, ce, mt);
                    info.cols[i].natural = info.cols[i].natural.max(8);
                    info.cols[i].cells.push(ce);
                }
                None => {
                    let ce =
                        cx.dom
                            .child(re, "div", &format!("sf-tbl-c{}{}", align_class(c), trunc));
                    cx.dom.attr(ce, "role", "cell");
                    let sp = spans::parse_spans(v);
                    let t = spans::spans_text(&sp);
                    info.cols[i].natural = info.cols[i].natural.max(t.chars().count());
                    if !trunc.is_empty() {
                        cx.dom.attr(ce, "title", &t);
                    }
                    m.clock |= spans::render(&mut cx.dom, ce, &sp);
                    info.cols[i].cells.push(ce);
                }
            }
        }
    }
    // A first sizing without the width: natural widths; the measure pass fits it.
    let tpl: Vec<String> = info
        .cols
        .iter()
        .map(|c| format!("calc({}*var(--sf-cw) + 14px)", c.natural))
        .collect();
    cx.dom.set_style(grid, &table_columns(&tpl.join(" ")));
    // Long tables virtualize (§8.3): rows become their own grids on the shared columns.
    super::virtualize(cx, node.id(), grid, virt.then_some(23.0));
    cx.st.tables.insert(node.id().to_owned(), info);
}

/// A table grid's column template, also as `--sf-tbl-cols` for the rows of a virtualized table.
pub(crate) fn table_columns(tpl: &str) -> String {
    format!("grid-template-columns: {tpl}; --sf-tbl-cols: {tpl}")
}

/// Fit table columns to `width` px with cell width `cw` (book:5522): hide lowest priority first
/// (ties: rightmost), give spare width to `grow` columns, shrink `grow` columns to 24 cells, then
/// all proportionally. Returns (shown, width px) per column.
pub(crate) fn fit_columns(cols: &[TableCol], width: f32, cw: f32) -> Vec<(bool, f32)> {
    let pad = 14.0;
    let nat: Vec<f32> = cols.iter().map(|c| c.natural as f32 * cw + pad).collect();
    let test: Vec<f32> = cols
        .iter()
        .zip(&nat)
        .map(|(c, n)| {
            if c.grow > 0.0 {
                n.min(24.0 * cw + pad)
            } else {
                *n
            }
        })
        .collect();
    let mut shown = vec![true; cols.len()];
    loop {
        let total: f32 = (0..cols.len()).filter(|&i| shown[i]).map(|i| test[i]).sum();
        let count = shown.iter().filter(|s| **s).count();
        if total <= width || count <= 1 {
            break;
        }
        // Lowest priority; ties hide the rightmost.
        let victim = (0..cols.len())
            .filter(|&i| shown[i])
            .min_by(|&a, &b| {
                cols[a]
                    .priority
                    .partial_cmp(&cols[b].priority)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(b.cmp(&a))
            })
            .unwrap();
        shown[victim] = false;
    }
    let mut w: Vec<f32> = nat.clone();
    let total: f32 = (0..cols.len()).filter(|&i| shown[i]).map(|i| w[i]).sum();
    let grow_sum: f64 = (0..cols.len())
        .filter(|&i| shown[i])
        .map(|i| cols[i].grow)
        .sum();
    if total < width && grow_sum > 0.0 {
        let spare = width - total;
        for i in (0..cols.len()).filter(|&i| shown[i]) {
            w[i] += spare * (cols[i].grow / grow_sum) as f32;
        }
    } else if total > width {
        let mut over = total - width;
        for i in (0..cols.len()).filter(|&i| shown[i] && cols[i].grow > 0.0) {
            let floor = 24.0 * cw + pad;
            let give = (w[i] - floor).max(0.0).min(over);
            w[i] -= give;
            over -= give;
        }
        if over > 0.0 {
            let t: f32 = (0..cols.len()).filter(|&i| shown[i]).map(|i| w[i]).sum();
            for i in (0..cols.len()).filter(|&i| shown[i]) {
                w[i] -= over * w[i] / t;
            }
        }
    }
    shown.into_iter().zip(w).collect()
}

pub(crate) fn tree(cx: &mut Cx, node: NodeRef, m: &mut Mount, look: &mut Look) {
    begin(cx, m);
    look.attr("role", "tree");
    let nodes = node
        .prop("nodes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let el = m.el;
    let id = node.id().to_owned();
    // The open groups' virtualization is rebuilt with the items.
    cx.st.forget_virtual_under(&id);
    let mut made = Vec::new();
    for n in &nodes {
        if let Some(e) = tree_item(cx, &id, el, n, 1) {
            made.push(e);
        }
    }
    m.generated.extend(made);
    // Many items at a level virtualize (§8.3), the top level here and open groups below.
    super::virtualize(cx, &id, el, (nodes.len() > VIRTUAL_ROWS).then_some(22.0));
}

fn tree_item(cx: &mut Cx, tree: &str, parent: NodeId, n: &Value, level: u32) -> Option<NodeId> {
    let o = n.as_object()?;
    let item = o.get("id").map(value_key)?;
    let kids = o.get("children").and_then(Value::as_array);
    let leaf = kids.is_none_or(|k| k.is_empty());
    let key = (tree.to_owned(), item.clone());
    let open = if leaf {
        false
    } else {
        let pv = o.get("open").cloned().unwrap_or(Value::Null);
        let from_prop = o.get("open").and_then(Value::as_bool).unwrap_or(false);
        match cx.st.tree.get_mut(&key) {
            Some(f) if f.prop == pv => f.local,
            Some(f) => {
                f.prop = pv;
                f.local = from_prop;
                from_prop
            }
            None => {
                cx.st.tree.insert(
                    key.clone(),
                    crate::state::Flip {
                        prop: pv,
                        local: from_prop,
                    },
                );
                from_prop
            }
        }
    };
    let _ = SurfaceState::default;
    let el = cx.dom.child(parent, "div", "sf-tree-node");
    cx.dom.attr(el, "role", "treeitem");
    cx.dom.attr(el, "aria-level", &level.to_string());
    if !leaf {
        cx.dom
            .attr(el, "aria-expanded", if open { "true" } else { "false" });
    }
    cx.dom.set_style(el, &format!("--d: {}", level - 1));
    let row = cx.dom.child(
        el,
        "div",
        if leaf {
            "sf-tree-row leaf"
        } else {
            "sf-tree-row"
        },
    );
    let tw = cx.dom.child(row, "button", "sf-tree-tw");
    cx.dom.attr(tw, "data-key", &item);
    if !leaf {
        let label = if open { "Collapse" } else { "Expand" };
        cx.dom.attr(tw, "title", label);
        cx.dom.attr(tw, "aria-label", label);
        icon(cx, tw, if open { "chev" } else { "chev-r" });
    }
    if let Some(ic) = o.get("icon").and_then(Value::as_str) {
        let s = cx.dom.child(row, "span", "sf-tree-ic");
        cx.dom.attr(s, "aria-hidden", "true");
        icon(cx, s, ic);
    }
    let lbl = cx.dom.child(row, "div", "sf-tree-lbl");
    spans_into(cx, lbl, o.get("label"));
    let unfold = !leaf && cx.st.unfolding_key(format!("tree:{tree}/{item}"), !open);
    let kd = cx.dom.child(
        el,
        "div",
        match (open, unfold) {
            (true, true) => "sf-tree-kids sf-unfold",
            (true, false) => "sf-tree-kids",
            _ => "sf-tree-kids sf-hidden",
        },
    );
    cx.dom.attr(kd, "role", "group");
    // Closed items mount no descendants (the windowed mode's cheap form).
    if open && let Some(kids) = kids {
        let wrap = cx.dom.child(kd, "div", "sf-tree-group");
        for k in kids {
            tree_item(cx, tree, wrap, k, level + 1);
        }
        if kids.len() > VIRTUAL_ROWS {
            super::virtualize(cx, &format!("{tree}/{item}"), wrap, Some(22.0));
        }
    }
    Some(el)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cutting() {
        assert_eq!(cut("abcdefghij", 5, "start"), "…ghij");
        assert_eq!(cut("abcdefghij", 5, "middle"), "ab…ij");
        assert_eq!(
            cut("src/very/long/path/file.rs", 14, "middle"),
            "…/path/file.rs"
        );
        assert_eq!(cut("src/very/long/path/file.rs", 10, "middle"), "…/file.rs");
        assert_eq!(cut("short", 10, "middle"), "short");
    }

    #[test]
    fn columns_fit() {
        let c = |n, p, g| TableCol {
            natural: n,
            priority: p,
            grow: g,
            cells: vec![],
        };
        let cols = vec![c(10, 1.0, 0.0), c(10, 0.0, 0.0), c(10, 2.0, 1.0)];
        // Everything fits: the grow column takes the spare width.
        let f = fit_columns(&cols, 400.0, 8.0);
        assert!(f.iter().all(|x| x.0));
        assert!((f.iter().map(|x| x.1).sum::<f32>() - 400.0).abs() < 0.01);
        // Too narrow: the lowest priority (column 1) hides first.
        let f = fit_columns(&cols, 200.0, 8.0);
        assert_eq!(
            f.iter().map(|x| x.0).collect::<Vec<_>>(),
            vec![true, false, true]
        );
    }
}
