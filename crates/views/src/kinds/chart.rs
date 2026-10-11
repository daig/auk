//! `chart` (T2): series data drawn as DOM+CSS, a heatmap (a calendar-style grid of
//! cells), a bar chart or a sparkline of thin bars.
//!
//! The whole chart is rebuilt when any prop changes, so its arrivals (`omp-cell`,
//! `omp-bar`, built-in CSS) replay then; a render with the same props (a theme or settings
//! change) keeps its elements, so nothing replays.

use super::{begin, part};
use crate::common::{fmt_num, str_prop};
use crate::spans;
use crate::view::{Cx, Extra, Look, Mount};
use auk_tsp::NodeRef;
use serde_json::{Map, Value};
use std::hash::{Hash, Hasher};

/// The chart kinds; any other value draws a heatmap.
pub fn chart_kind(p: &Map<String, Value>) -> &'static str {
    match str_prop(p, "kind") {
        Some("bars") => "bars",
        Some("spark") => "spark",
        _ => "heatmap",
    }
}

/// The size; any other value draws as `md`.
pub fn chart_size(p: &Map<String, Value>) -> &'static str {
    match str_prop(p, "size") {
        Some("sm") => "sm",
        Some("lg") => "lg",
        _ => "md",
    }
}

/// A heatmap cell's level (`data-l`): `≤ 0` → 0, `≤ 0.25` → 1, `≤ 0.5` → 2, `≤ 0.75` → 3, above
/// → 4; a non-number or a missing cell is `n`.
pub fn level(v: Option<&Value>) -> &'static str {
    match v.and_then(Value::as_f64).filter(|f| f.is_finite()) {
        None => "n",
        Some(f) if f <= 0.0 => "0",
        Some(f) if f <= 0.25 => "1",
        Some(f) if f <= 0.5 => "2",
        Some(f) if f <= 0.75 => "3",
        Some(_) => "4",
    }
}

/// The fill color a `token` names, as `--chart-c`: `accent` (and no
/// token) uses the program palette's accent if it has one; a semantic token naming a color is
/// `var(--sf-c-<t>)`; any other token (a program token, or a semantic one without a color such
/// as `strong`) is `var(--sf-p-<t>, <the accent>)`. A token that isn't a
/// plain name is ignored.
pub fn fill(token: Option<&str>) -> String {
    const ACCENT: &str = "var(--sf-p-accent, var(--accent))";
    let Some(t) = token.filter(|t| {
        !t.is_empty()
            && t.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    }) else {
        return ACCENT.to_owned();
    };
    if t == "accent" {
        ACCENT.to_owned()
    } else if spans::COLOR_TOKENS.contains(&t) {
        format!("var(--sf-c-{t})")
    } else {
        format!("var(--sf-p-{t}, var(--accent))")
    }
}

/// Bar heights relative to the largest value (0–1); a zero, negative or missing value is 0 (a
/// 1 px stub).
pub fn bar_heights(series: &[Value]) -> Vec<f64> {
    let vals: Vec<f64> = series
        .iter()
        .map(|e| {
            e.get("value")
                .and_then(Value::as_f64)
                .filter(|f| f.is_finite() && *f > 0.0)
                .unwrap_or(0.0)
        })
        .collect();
    let max = vals.iter().copied().fold(0.0, f64::max);
    vals.iter()
        .map(|v| if max > 0.0 { v / max } else { 0.0 })
        .collect()
}

fn props_hash(p: &Map<String, Value>) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_string(p).unwrap_or_default().hash(&mut h);
    h.finish()
}

fn text_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

pub(crate) fn chart(cx: &mut Cx, node: NodeRef, m: &mut Mount, look: &mut Look) {
    let p = node.props();
    let kind = chart_kind(p);
    let size = chart_size(p);
    look.attr("data-kind", kind);
    look.attr("data-size", size);
    look.attr("role", "figure");
    look.style(&format!("--chart-c: {};", fill(str_prop(p, "token"))));
    let summary = spans::parse_spans(p.get("summary").filter(|s| !s.is_null()));
    let sum_text = spans::spans_text(&summary);
    if str_prop(p, "aria").is_none() && !sum_text.trim().is_empty() {
        look.attr("aria-label", sum_text.clone());
    }
    let hash = props_hash(p);
    if matches!(m.extra, Extra::Chart(h) if h == hash) && !m.generated.is_empty() {
        return;
    }
    begin(cx, m);
    m.extra = Extra::Chart(hash);
    if kind == "heatmap" {
        heatmap(cx, m, p);
    } else {
        bars(cx, m, p);
    }
    if !sum_text.trim().is_empty() {
        let s = part(cx, m, "div", "sum");
        spans::render(&mut cx.dom, s, &summary);
    }
}

fn heatmap(cx: &mut Cx, m: &mut Mount, p: &Map<String, Value>) {
    let rows_of = |k: &str| -> Vec<Vec<Value>> {
        p.get(k)
            .and_then(Value::as_array)
            .map(|rs| {
                rs.iter()
                    .map(|r| r.as_array().cloned().unwrap_or_default())
                    .collect()
            })
            .unwrap_or_default()
    };
    let cells = rows_of("cells");
    let tips = rows_of("tips");
    let labels: Vec<String> = p
        .get("rows")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(|v| text_of(v).unwrap_or_default()).collect())
        .unwrap_or_default();
    let ncols = cells.iter().map(Vec::len).max().unwrap_or(0);
    let hm = part(cx, m, "div", "hm");
    cx.dom.attr(hm, "aria-hidden", "true");
    // Wider than the pane, it scrolls sideways and shows its newest columns first.
    cx.st.scroll_end_x.push(hm);
    // Row labels take the widest label's room; month labels sit past it.
    let widest = labels.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    if widest > 0 {
        cx.dom.set_style(
            hm,
            &format!("--rlw: calc({}em + 4px)", fmt_num(widest as f64 * 0.62)),
        );
    }
    let mls = cx.dom.child(hm, "div", "mls");
    if let Some(cols) = p.get("cols").and_then(Value::as_array) {
        for c in cols {
            let at = c
                .get("at")
                .and_then(Value::as_f64)
                .filter(|f| f.is_finite() && *f >= 0.0 && f.fract() == 0.0);
            let label = c.get("label").and_then(text_of);
            let (Some(at), Some(label)) = (at, label) else {
                continue;
            };
            let ml = cx.dom.child_text(mls, "span", "ml", &label);
            cx.dom.set_style(ml, &format!("--at: {}", fmt_num(at)));
        }
    }
    let grid = cx.dom.child(hm, "div", "grid");
    if widest > 0 {
        let rls = cx.dom.child(grid, "div", "rls");
        for i in 0..cells.len().max(labels.len()) {
            let l = labels.get(i).map(String::as_str).unwrap_or("");
            cx.dom.child_text(rls, "span", "rl", l);
        }
    }
    for x in 0..ncols {
        let wk = cx.dom.child(grid, "div", "wk");
        cx.dom.set_style(wk, &format!("--x: {x}"));
        for (r, row) in cells.iter().enumerate() {
            let c = cx.dom.child(wk, "span", "c");
            cx.dom.attr(c, "data-l", level(row.get(x)));
            if let Some(t) = tips
                .get(r)
                .and_then(|tr| tr.get(x))
                .and_then(Value::as_str)
                .filter(|t| !t.is_empty())
            {
                cx.dom.attr(c, "title", t);
            }
        }
    }
}

fn bars(cx: &mut Cx, m: &mut Mount, p: &Map<String, Value>) {
    let series: Vec<Value> = p
        .get("series")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let heights = bar_heights(&series);
    let b = part(cx, m, "div", "bars");
    cx.dom.attr(b, "aria-hidden", "true");
    for (x, (e, v)) in series.iter().zip(heights).enumerate() {
        let bar = cx.dom.child(b, "span", "b");
        cx.dom
            .set_style(bar, &format!("--v: {}; --x: {x}", fmt_num(v)));
        let tip = e
            .get("title")
            .and_then(text_of)
            .or_else(|| e.get("label").and_then(text_of))
            .filter(|t| !t.is_empty());
        if let Some(t) = tip {
            cx.dom.attr(bar, "title", &t);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn levels_follow_the_book() {
        let l = |v: Value| level(Some(&v));
        assert_eq!(l(json!(-1)), "0");
        assert_eq!(l(json!(0)), "0");
        assert_eq!(l(json!(0.1)), "1");
        assert_eq!(l(json!(0.25)), "1");
        assert_eq!(l(json!(0.5)), "2");
        assert_eq!(l(json!(0.75)), "3");
        assert_eq!(l(json!(0.76)), "4");
        assert_eq!(l(json!(3)), "4");
        assert_eq!(l(json!("x")), "n");
        assert_eq!(l(Value::Null), "n");
        assert_eq!(level(None), "n");
    }

    #[test]
    fn tokens_resolve_like_the_other_token_kinds() {
        assert_eq!(fill(None), "var(--sf-p-accent, var(--accent))");
        assert_eq!(fill(Some("accent")), "var(--sf-p-accent, var(--accent))");
        assert_eq!(fill(Some("success")), "var(--sf-c-success)");
        // Semantic tokens without a color have no `--sf-c-*`: the palette's, else the accent.
        for t in ["strong", "em", "mono", "mark", "typo", "icon", "hide"] {
            assert_eq!(
                fill(Some(t)),
                format!("var(--sf-p-{t}, var(--accent))"),
                "{t}"
            );
        }
        assert_eq!(
            fill(Some("statusLineCost")),
            "var(--sf-p-statusLineCost, var(--accent))"
        );
        assert_eq!(
            fill(Some("x); color: red")),
            "var(--sf-p-accent, var(--accent))"
        );
    }

    #[test]
    fn bars_are_relative_to_the_largest_value() {
        let s = vec![
            json!({"value": 2}),
            json!({"value": 4}),
            json!({"value": 0}),
            json!({"value": -3}),
            json!({}),
            json!({"value": "x"}),
        ];
        assert_eq!(bar_heights(&s), vec![0.5, 1.0, 0.0, 0.0, 0.0, 0.0]);
        assert_eq!(bar_heights(&[json!({"value": 0})]), vec![0.0]);
    }
}
