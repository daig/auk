//! Kinds on rich sub-engines (§8.5, §18.4): `md code diff ansi rows image`, and the T2 `math`
//! (typeset by [`crate::math`], as md math is).

use super::layout::preview_lines;
use super::{begin, icon, part};
use crate::ansi::AnsiState;
use crate::common::{bool_prop, num_prop, str_prop};
use crate::diff::{self, DLine, Hunk, Op};
use crate::dom::Dom;
use crate::highlight::{self, Runs};
use crate::image::Picture;
use crate::spans;
use crate::state::{SurfaceState, state_key};
use crate::view::{Cx, Extra, Look, Mount};
use auk_tsp::NodeRef;
use auk_ui::NodeId;
use serde_json::Value;
use std::sync::Arc;

fn text_prop(node: NodeRef) -> String {
    match node.prop("text") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

pub(crate) fn md(cx: &mut Cx, node: NodeRef, m: &mut Mount, _look: &mut Look) {
    let p = node.props();
    let text = text_prop(node);
    let stream = bool_prop(p, "stream");
    let marks = spans::parse_spans(p.get("marks"));
    if !matches!(m.extra, Extra::Md(_)) {
        begin(cx, m);
        m.extra = Extra::Md(Box::default());
    }
    let Extra::Md(st) = &mut m.extra else {
        unreachable!()
    };
    st.math_em = math_em(cx.set.font_px, cx.dom.doc.device_scale());
    st.update(&mut cx.dom, m.el, &text, stream, marks);
    let math_pending = !st.math_pending.is_empty();
    // Blocks are the node's generated parts.
    m.generated = st.blocks.iter().map(|b| b.el).collect();
    // Many blocks: virtualize (§8.3).
    let many = st.blocks.len() > 64;
    super::virtualize(cx, node.id(), m.el, many.then_some(60.0));
    m.clock = stream && !cx.set.reduce_motion || math_pending;
}

/// Math's device pixels per em in a region at terminal font `font_px` (regions zoom by
/// `font_px / 13`, §8.3; body text is 13px).
pub(crate) fn math_em(font_px: f32, device_scale: f32) -> u32 {
    crate::math::em_px(13.0, font_px / 13.0, device_scale)
}

/// The streaming caret pulses; formulas typeset in the background are filled in.
pub(crate) fn tick_md(cx: &mut Cx, _node: NodeRef, m: &mut Mount) -> bool {
    let Extra::Md(st) = &mut m.extra else {
        return false;
    };
    let mut math = false;
    if !st.math_pending.is_empty() {
        let pending = std::mem::take(&mut st.math_pending);
        st.math_pending = crate::math::fill(&mut cx.dom, pending);
        math = !st.math_pending.is_empty();
    }
    let Some(c) = st.caret else { return math };
    if cx.set.reduce_motion {
        cx.dom.set_style(c, "");
        return false;
    }
    let t = (cx.now.rem_euclid(1000.0)) / 1000.0;
    let o = if t < 0.5 { 1.0 } else { 0.25 };
    cx.dom.set_style(c, &format!("opacity: {o}"));
    true
}

/// A `code` node's incremental state.
#[derive(Debug, Default)]
pub(crate) struct CodeState {
    /// Highlighting, done in the background past the first screenful (§8.5).
    pub hl: Option<highlight::Background>,
    pub key: String,
    pub body: Option<NodeId>,
    pub lines: Vec<NodeId>,
    /// Each line's text element and emphasized ranges, for drawing highlighting in later.
    pub texts: Vec<(NodeId, Vec<std::ops::Range<usize>>)>,
    /// Lines drawn with their highlighting; the rest are drawn plain until it arrives.
    pub hl_drawn: usize,
    pub text: String,
    /// Lines re-rendered by the last update.
    pub redrawn: usize,
}

/// A line's runs with emphasized byte ranges split out.
pub(crate) fn line_into(
    dom: &mut Dom,
    parent: NodeId,
    line: &str,
    runs: Option<&Runs>,
    em: &[std::ops::Range<usize>],
) {
    let whole: Runs = vec![(None, 0..line.len())];
    let runs = runs.unwrap_or(&whole);
    let mut cuts: Vec<usize> = vec![0, line.len()];
    for r in runs {
        cuts.push(r.1.start);
        cuts.push(r.1.end);
    }
    for e in em {
        cuts.push(e.start.min(line.len()));
        cuts.push(e.end.min(line.len()));
    }
    cuts.sort_unstable();
    cuts.dedup();
    // The open emphasis span and the range it belongs to.
    let mut em_open: Option<(usize, NodeId)> = None;
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        if a >= b || !line.is_char_boundary(a) || !line.is_char_boundary(b) {
            continue;
        }
        let tk = runs
            .iter()
            .find(|r| r.1.start <= a && a < r.1.end)
            .and_then(|r| r.0);
        let host = match em.iter().position(|e| e.start <= a && a < e.end) {
            Some(i) => match em_open {
                Some((j, h)) if j == i => h,
                _ => {
                    let h = dom.child(parent, "span", "sf-em");
                    em_open = Some((i, h));
                    h
                }
            },
            None => {
                em_open = None;
                parent
            }
        };
        let piece = &line[a..b];
        match tk {
            Some(t) => {
                dom.child_text(host, "span", t.class(), piece);
            }
            None => {
                dom.text(host, piece);
            }
        }
    }
}

pub(crate) fn code(cx: &mut Cx, node: NodeRef, m: &mut Mount, look: &mut Look) {
    let p = node.props();
    let text = text_prop(node);
    let lang = str_prop(p, "lang");
    let path = str_prop(p, "path");
    let numbers = bool_prop(p, "numbers");
    let wrap = bool_prop(p, "wrap");
    let start = num_prop(p, "start").unwrap_or(1.0).max(0.0) as u64;
    let marks: Vec<(u64, Option<String>, Vec<std::ops::Range<usize>>)> = p
        .get("marks")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|mk| {
                    let o = mk.as_object()?;
                    let line = o.get("line")?.as_u64()?;
                    let tone = o.get("tone").and_then(Value::as_str).map(str::to_owned);
                    let ranges = o
                        .get("ranges")
                        .and_then(Value::as_array)
                        .map(|rs| {
                            rs.iter()
                                .filter_map(|r| {
                                    let r = r.as_array()?;
                                    let s = r.first()?.as_u64()? as usize;
                                    let e = r.get(1)?.as_u64()? as usize;
                                    (s < e).then_some(s..e)
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    Some((line, tone, ranges))
                })
                .collect()
        })
        .unwrap_or_default();
    let line_count = text.split('\n').count();
    look.class_if(numbers, "numbers");
    look.class_if(wrap, "wrap");
    look.class_if(line_count > 400, "virtual");
    let last_no = start + line_count as u64;
    look.style(&format!("--sf-gw: {};", last_no.to_string().len()));
    // Everything but the text in the key: a change rebuilds all lines.
    let key = format!("{lang:?}|{path:?}|{numbers}|{start}|{:?}", p.get("marks"));
    let rebuild = !matches!(&m.extra, Extra::Code(c) if c.key == key);
    if rebuild {
        begin(cx, m);
        let mut st = CodeState {
            key,
            hl: highlight::find(lang, path).map(highlight::Background::new),
            ..Default::default()
        };
        if let Some(path) = path {
            let head = part(cx, m, "div", "sf-code-head");
            icon(cx, head, "file-code");
            let ps = cx.dom.child(head, "span", "sf-code-path");
            cx.dom.attr(ps, "title", path);
            if let Some(i) = path.rfind('/') {
                cx.dom.child_text(ps, "span", "dir", &path[..=i]);
                cx.dom.child_text(ps, "span", "name", &path[i + 1..]);
            } else {
                cx.dom.child_text(ps, "span", "name", path);
            }
        }
        let body = part(cx, m, "div", "sf-code-body");
        let lines = cx.dom.child(body, "div", "sf-code-lines");
        st.body = Some(lines);
        m.extra = Extra::Code(Box::new(st));
    }
    let Extra::Code(st) = &mut m.extra else {
        unreachable!()
    };
    let body = st.body.expect("body");
    let new_lines: Vec<&str> = text.split('\n').collect();
    let first = match &st.hl {
        Some(h) => h.update(&text),
        None => {
            let old: Vec<&str> = st.text.split('\n').collect();
            let same = old
                .iter()
                .zip(&new_lines)
                .take_while(|(a, b)| a == b)
                .count();
            same.min(old.len().saturating_sub(1))
        }
    };
    let first = if rebuild {
        0
    } else {
        first.min(st.lines.len())
    };
    for el in st.lines.drain(first..) {
        if cx.dom.doc.try_node(el).is_some() {
            cx.dom.doc.remove(el);
        }
    }
    st.texts.truncate(first);
    let guard = st.hl.as_ref().map(|h| h.lock());
    for (i, line) in new_lines.iter().enumerate().skip(first) {
        let no = start + i as u64;
        let mark = marks.iter().find(|mk| mk.0 == no);
        let ln = cx.dom.child(
            body,
            "div",
            if mark.is_some() { "sf-ln mk" } else { "sf-ln" },
        );
        if let Some((_, Some(t), _)) = mark {
            cx.dom.attr(ln, "data-tone", t);
        }
        if numbers {
            let g = cx.dom.child_text(ln, "span", "sf-gut", &no.to_string());
            cx.dom.attr(g, "aria-hidden", "true");
        }
        let lt = cx.dom.child(ln, "span", "sf-lt");
        let runs = guard
            .as_ref()
            .filter(|h| i < h.highlighted())
            .map(|h| h.line(i).1);
        let em: Vec<std::ops::Range<usize>> = mark.map(|m| m.2.clone()).unwrap_or_default();
        line_into(&mut cx.dom, lt, line, runs, &em);
        st.lines.push(ln);
        st.texts.push((lt, em));
    }
    st.hl_drawn = guard.as_ref().map_or(new_lines.len(), |h| h.highlighted());
    let pending = guard.is_some_and(|h| !h.done()) || st.hl_drawn < new_lines.len();
    st.redrawn = new_lines.len() - first;
    st.text = text;
    // Long code virtualizes (§8.3: over 400 lines).
    let long = st.lines.len() > 400;
    super::virtualize(cx, node.id(), body, long.then_some(17.0));
    // Highlighting still on its way: the clock draws it in.
    m.clock = pending;
}

/// Draws in the highlighting the background has finished since the last render or tick.
/// Returns whether more is on its way.
pub(crate) fn tick_code(cx: &mut Cx, _node: NodeRef, m: &mut Mount) -> bool {
    let Extra::Code(st) = &mut m.extra else {
        return false;
    };
    let Some(bg) = st.hl.clone() else {
        return false;
    };
    let more = bg.pump();
    let h = bg.lock();
    let ready = h.highlighted().min(st.texts.len());
    for i in st.hl_drawn..ready {
        let (lt, em) = &st.texts[i];
        let lt = *lt;
        if cx.dom.doc.try_node(lt).is_none() {
            continue;
        }
        cx.dom.clear(lt);
        let (line, runs) = h.line(i);
        line_into(&mut cx.dom, lt, line, Some(runs), em);
    }
    st.hl_drawn = st.hl_drawn.max(ready);
    more || st.hl_drawn < st.texts.len()
}

/// Whether the nearest collapsible card or section up the tree is open (previews give way).
fn inside_open_fold(st: &mut SurfaceState, node: NodeRef) -> bool {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if matches!(n.kind(), "card" | "section" | "tool") && bool_prop(n.props(), "collapsible") {
            return !st.fold(n, false);
        }
        cur = n.parent();
    }
    false
}

pub(crate) fn diff(cx: &mut Cx, node: NodeRef, m: &mut Mount, look: &mut Look) {
    begin(cx, m);
    let p = node.props();
    let hunks: Vec<Hunk> = match p.get("hunks") {
        Some(h @ Value::Array(_)) => diff::parse_hunks(h),
        _ => diff::parse_unified(str_prop(p, "text").unwrap_or("")),
    };
    let split = match str_prop(p, "mode") {
        Some("split") => true,
        Some("unified") => false,
        _ => cx.set.split_diffs && cx.cols > 112,
    };
    look.class_if(split, "split");
    let lang = str_prop(p, "lang");
    let path = str_prop(p, "path");
    let maxno = hunks
        .iter()
        .flat_map(|h| h.lines.iter())
        .flat_map(|l| [l.old, l.new])
        .flatten()
        .max()
        .unwrap_or(1);
    look.style(&format!("--sf-gw: {};", maxno.to_string().len()));
    let (add, del) = diff::counts(&hunks);
    look.attr("aria-label", format!("Diff, {add} added, {del} removed"));
    let body = part(cx, m, "div", "sf-diff-body");
    // Highlight each side as a whole so multi-line constructs color right.
    let syn = highlight::find(lang, path);
    let side = |op_keep: Op| -> Option<highlight::Later> {
        let lines: Vec<String> = hunks
            .iter()
            .flat_map(|h| h.lines.iter())
            .filter(|l| l.op == Op::Ctx || l.op == op_keep)
            .map(|l| l.text.clone())
            .collect();
        syn.map(|s| highlight::later(s, &lines.join("\n")))
    };
    let mut old_hl = side(Op::Del);
    let mut new_hl = side(Op::Add);
    let (mut oi, mut ni) = (0usize, 0usize);
    let mut prev_end: Option<u64> = None;
    for h in &hunks {
        let gap = match prev_end {
            Some(e) => h.old_start.saturating_sub(e),
            None => h.old_start.saturating_sub(1),
        };
        if gap > 0 {
            let sep = cx.dom.child(body, "div", "sf-dsep");
            cx.dom.text(
                sep,
                &format!("⋯ {gap} unchanged line{}", if gap == 1 { "" } else { "s" }),
            );
            if let Some(f) = &h.func {
                cx.dom.child_text(sep, "span", "fn", f);
            }
        }
        let mut rows: Vec<(Side, Side)> = Vec::new();
        let mut k = 0;
        while k < h.lines.len() {
            let l = &h.lines[k];
            match l.op {
                Op::Ctx => {
                    rows.push((Some((l, oi)), Some((l, ni))));
                    oi += 1;
                    ni += 1;
                    k += 1;
                }
                _ => {
                    let ds = k;
                    while k < h.lines.len() && h.lines[k].op == Op::Del {
                        k += 1;
                    }
                    let as_ = k;
                    while k < h.lines.len() && h.lines[k].op == Op::Add {
                        k += 1;
                    }
                    let dels: Vec<(&DLine, usize)> = h.lines[ds..as_]
                        .iter()
                        .map(|l| {
                            oi += 1;
                            (l, oi - 1)
                        })
                        .collect();
                    let adds: Vec<(&DLine, usize)> = h.lines[as_..k]
                        .iter()
                        .map(|l| {
                            ni += 1;
                            (l, ni - 1)
                        })
                        .collect();
                    if split {
                        for j in 0..dels.len().max(adds.len()) {
                            rows.push((dels.get(j).copied(), adds.get(j).copied()));
                        }
                    } else {
                        for d in dels {
                            rows.push((Some(d), None));
                        }
                        for a in adds {
                            rows.push((None, Some(a)));
                        }
                    }
                }
            }
        }
        for (old, new) in rows {
            if split {
                let r = cx.dom.child(body, "div", "sf-dr");
                diff_line(cx, r, old, &mut old_hl, true, true);
                diff_line(cx, r, new, &mut new_hl, true, false);
            } else {
                match (old, new) {
                    (Some(o), Some(_)) => diff_line(cx, body, Some(o), &mut old_hl, false, true),
                    (Some(o), None) => diff_line(cx, body, Some(o), &mut old_hl, false, true),
                    (None, Some(n)) => diff_line(cx, body, Some(n), &mut new_hl, false, false),
                    (None, None) => {}
                }
            }
        }
        let old_len = h.lines.iter().filter(|l| l.op != Op::Add).count() as u64;
        prev_end = Some(h.old_start + old_len);
    }
    // Each side is highlighted in the background and drawn in when in view.
    for hl in [old_hl, new_hl].into_iter().flatten() {
        hl.finish();
    }
}

/// One side of a diff row: the line and its index in that side's highlighted text.
type Side<'a> = Option<(&'a DLine, usize)>;

fn diff_line(
    cx: &mut Cx,
    parent: NodeId,
    l: Option<(&DLine, usize)>,
    hl: &mut Option<highlight::Later>,
    half: bool,
    old_side: bool,
) {
    let Some((l, idx)) = l else {
        cx.dom.child(parent, "div", "sf-dl half empty");
        return;
    };
    let op = match l.op {
        Op::Ctx => "ctx",
        Op::Del => "del",
        Op::Add => "add",
    };
    let class = if half {
        format!("sf-dl half {op}")
    } else {
        format!("sf-dl {op}")
    };
    let row = cx.dom.child(parent, "div", &class);
    let num = |n: Option<u64>| n.map(|n| n.to_string()).unwrap_or_default();
    if half {
        let n = if old_side { l.old } else { l.new };
        cx.dom.child_text(row, "span", "sf-dg", &num(n));
    } else {
        cx.dom.child_text(row, "span", "sf-dg", &num(l.old));
        cx.dom.child_text(row, "span", "sf-dg", &num(l.new));
    }
    let mark = match l.op {
        Op::Ctx => " ",
        Op::Del => "-",
        Op::Add => "+",
    };
    cx.dom.child_text(row, "span", "sf-dm", mark);
    let t = cx.dom.child(row, "span", "sf-dt");
    line_into(&mut cx.dom, t, &l.text, None, &l.em);
    if let Some(h) = hl.as_mut() {
        h.add(highlight::DeferredLine {
            el: t,
            idx,
            text: l.text.clone(),
            em: l.em.clone(),
            newline: false,
        });
    }
}

pub(crate) fn ansi(cx: &mut Cx, node: NodeRef, m: &mut Mount, look: &mut Look) {
    let p = node.props();
    let text = text_prop(node);
    let cols = num_prop(p, "cols")
        .map(|c| (c as u16).clamp(2, cx.cols.max(2)))
        .unwrap_or(cx.cols.max(2));
    let follow = bool_prop(p, "follow");
    let scroll = p
        .get("max")
        .and_then(|mx| mx.get("h"))
        .and_then(crate::common::extent)
        .is_some();
    let preview = preview_lines(p.get("preview"));
    let key = state_key(node);
    let expanded = cx.st.more.get(&key).copied().unwrap_or(false);
    let preview = if inside_open_fold(cx.st, node) {
        None
    } else {
        preview
    };
    look.class_if(scroll, "scroll");
    look.class_if(follow, "follow");
    if !matches!(m.extra, Extra::Ansi(_)) {
        begin(cx, m);
        let clip = part(cx, m, "div", "sf-term-clip");
        let mini = cx.dom.child(clip, "div", "tv-mini");
        let more = part(cx, m, "button", "sf-ansi-more sf-hidden");
        cx.dom.attr(more, "data-more", "ansi");
        let mut st = AnsiState::new(cols);
        st.holder = Some(mini);
        m.extra = Extra::Ansi(Box::new(st));
    }
    let clip = m.generated[0];
    let more = m.generated[1];
    let Extra::Ansi(st) = &mut m.extra else {
        unreachable!()
    };
    st.set(&text, cols);
    let n = st.line_count();
    let mini = st.holder.expect("holder");
    let range = match preview {
        Some(pv) if !expanded && (pv as usize) < n => {
            if follow {
                n - pv as usize..n
            } else {
                0..pv as usize
            }
        }
        _ => 0..n,
    };
    let hidden = n - range.len();
    st.draw(&mut cx.dom, mini, range);
    cx.dom.set_class(
        clip,
        if hidden > 0 {
            if follow {
                "sf-term-clip clamped fade-top"
            } else {
                "sf-term-clip clamped fade-bottom"
            }
        } else {
            "sf-term-clip"
        },
    );
    if hidden > 0 || (expanded && preview.is_some_and(|pv| (pv as usize) < n)) {
        cx.dom.set_class(more, "sf-ansi-more");
        cx.dom.attr(
            more,
            "aria-expanded",
            if expanded { "true" } else { "false" },
        );
        let label = if expanded {
            "Show less".to_owned()
        } else {
            format!("{hidden} more line{}", if hidden == 1 { "" } else { "s" })
        };
        cx.dom.attr(
            more,
            "title",
            if expanded {
                "Show less"
            } else {
                "Show all output"
            },
        );
        cx.dom.set_only_text(more, &label);
    } else {
        cx.dom.set_class(more, "sf-ansi-more sf-hidden");
        cx.dom.set_only_text(more, "");
    }
    // Long output paints the visible lines (§8.3).
    super::virtualize(cx, node.id(), mini, (n > 48).then_some(17.0));
    if scroll && follow {
        cx.st.follow.insert(node.id().to_owned(), clip);
    } else {
        cx.st.follow.remove(node.id());
    }
}

pub(crate) fn rows(cx: &mut Cx, node: NodeRef, m: &mut Mount, _look: &mut Look) {
    begin(cx, m);
    let p = node.props();
    let cols = num_prop(p, "cols").unwrap_or(80.0).clamp(2.0, 1000.0) as u16;
    let lines: Vec<String> = p
        .get("lines")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let mark = part(cx, m, "span", "sf-rows-mark");
    let label = format!("Fallback rows ({})", lines.len());
    cx.dom.attr(mark, "title", &label);
    cx.dom.attr(mark, "aria-label", &label);
    cx.dom.text(mark, "fallback");
    let clip = part(cx, m, "div", "sf-term-clip");
    let mini = cx.dom.child(clip, "div", "tv-mini");
    cx.dom
        .set_style(mini, &format!("width: calc({cols}*var(--sf-cw))"));
    // Exact width, no rewrap: autowrap off, each row on its own line.
    let mut st = AnsiState::new(cols);
    let mut text = String::from("\x1b[?7l");
    for (i, l) in lines.iter().enumerate() {
        if i > 0 {
            text.push_str("\x1b[0m\r\n");
        }
        text.push_str(l);
    }
    st.set(&text, cols);
    let n = st
        .line_count()
        .max(lines.len())
        .min(st.term.history_len() + 1);
    st.draw(&mut cx.dom, mini, 0..n);
}

/// `math` (book: math): one TeX formula, `div.md > div.math-display` centered in the node's
/// width when `display`, else `div.md > p > span.math` at the node's start, typeset like md math.
/// A formula typesetting in the background shows its TeX until the clock fills it in.
pub(crate) fn math(cx: &mut Cx, node: NodeRef, m: &mut Mount, look: &mut Look) {
    begin(cx, m);
    let display = bool_prop(node.props(), "display");
    look.class_if(display, "display");
    let tex = text_prop(node);
    let md = part(cx, m, "div", "md");
    let parent = if display {
        md
    } else {
        cx.dom.child(md, "p", "")
    };
    let em = math_em(cx.set.font_px, cx.dom.doc.device_scale());
    let (_, pending) = crate::math::mount(&mut cx.dom, parent, &tex, display, em);
    m.clock = pending.is_some();
}

/// A `math` node waiting for the background: drawn again once its formula is ready.
pub(crate) fn tick_math(cx: &mut Cx, node: NodeRef, m: &mut Mount) -> bool {
    crate::math::pump();
    let display = bool_prop(node.props(), "display");
    let tex = crate::math::normalize(&text_prop(node), display);
    let em = math_em(cx.set.font_px, cx.dom.doc.device_scale());
    if matches!(
        crate::math::request(tex.trim(), display, em),
        crate::math::Poll::Pending
    ) {
        return true;
    }
    crate::view::render_node(cx, m, node);
    m.clock
}

/// Per-node state of an `image`: what it shows, its frame element and its animation clock.
#[derive(Debug, Default)]
pub(crate) struct ImageState {
    /// The source and density shown; the clock restarts when it changes.
    key: String,
    picture: Option<Arc<Picture>>,
    /// The box's natural size (from `w`/`h`, completed from the picture's ratio).
    natural: (f32, f32),
    frame: Option<NodeId>,
    /// When the picture first drew (view clock, ms).
    start: f64,
    shown: usize,
    /// The picture is decoding in the background: the clock watches for it.
    decoding: bool,
}

/// What an `image` node can show.
enum ImageSource {
    Ready(Arc<Picture>),
    /// The blob hasn't arrived.
    Loading,
    /// The blob is decoding in the background.
    Decoding,
    /// Why it can't show.
    Unavailable(String),
}

/// `image` (book:5925-5969): a blob (raster or sanitized SVG) or a built-in picture in a replaced
/// box, its children drawn over it in `.sf-img-over`. Until the blob arrives, or when it can't be
/// decoded, a placeholder card with an icon and `alt`. Animations run on the view clock (§7.13),
/// still under Reduce Motion. Blob images zoom on click unless the node has its own `click`.
pub(crate) fn image(cx: &mut Cx, node: NodeRef, m: &mut Mount, look: &mut Look) {
    let prior = match std::mem::take(&mut m.extra) {
        Extra::Image(st) => Some(st),
        _ => None,
    };
    begin(cx, m);
    let p = node.props();
    let alt = str_prop(p, "alt");
    let builtin = str_prop(p, "builtin");
    let blob = str_prop(p, "blob").map(str::to_ascii_lowercase);
    // Vector pictures draw at the region zoom times the display scale (§8.3).
    let density = cx.set.font_px / 13.0 * cx.dom.doc.device_scale();
    let source = if let Some(name) = builtin {
        match crate::image::load_builtin(name, density) {
            Ok(pic) => ImageSource::Ready(pic),
            Err(e) => ImageSource::Unavailable(e),
        }
    } else if let Some(id) = blob.as_deref() {
        let held = cx
            .blobs
            .get(id)
            .map(std::borrow::Cow::Borrowed)
            .or_else(|| cx.blobs.load(id).map(std::borrow::Cow::Owned));
        match held {
            None => ImageSource::Loading,
            // A big picture decodes off the frame path (amendment A9); small ones now.
            Some(b) => match crate::image::request_blob(&b, density) {
                crate::image::Poll::Ready(Ok(pic)) => ImageSource::Ready(pic),
                crate::image::Poll::Ready(Err(e)) => ImageSource::Unavailable(e),
                crate::image::Poll::Pending => ImageSource::Decoding,
            },
        }
    } else {
        ImageSource::Unavailable("the node names no blob".into())
    };
    let size = |k: &str| num_prop(p, k).filter(|v| *v > 0.0).map(|v| v as f32);
    match source {
        ImageSource::Ready(pic) => {
            let (pw, ph) = pic.size;
            let natural = match (size("w"), size("h")) {
                (Some(w), Some(h)) => (w, h),
                (Some(w), None) if pw > 0.0 => (w, w * ph / pw),
                (None, Some(h)) if ph > 0.0 => (h * pw / ph, h),
                _ => (pw, ph),
            };
            let zoom =
                builtin.is_none() && crate::common::Actions::read(p, "image").click.is_none();
            look.class_if(zoom, "sf-zoom");
            let label = alt.unwrap_or("Image");
            let el = if pic.svg {
                let el = part(cx, m, "svg", "sf-img-svg");
                cx.dom.attr(el, "role", "img");
                cx.dom.attr(el, "aria-label", label);
                el
            } else {
                let el = part(cx, m, "img", "sf-img-frame");
                cx.dom.attr(el, "alt", label);
                el
            };
            // `max.h` bounds the picture's height; both axes shrink together.
            if let Some(Value::Object(max)) = p.get("max")
                && let Some(h @ Value::String(_)) = max.get("h")
                && let Some(e) = crate::common::extent(h)
            {
                cx.dom.set_style(el, &format!("max-height: {e}"));
            }
            let key = format!(
                "{}@{density}",
                builtin.map_or_else(
                    || blob.clone().unwrap_or_default(),
                    |b| format!("builtin:{b}")
                )
            );
            let start = prior.filter(|s| s.key == key).map_or(cx.now, |s| s.start);
            let (shown, running) = if pic.animated() && !cx.set.reduce_motion {
                pic.frame_at(cx.now - start)
            } else {
                (0, false)
            };
            let mut content = pic.frames[shown].content.clone();
            content.natural = natural;
            cx.dom.doc.set_image(el, Some(content));
            m.clock = running;
            m.extra = Extra::Image(Box::new(ImageState {
                key,
                picture: Some(pic),
                natural,
                frame: Some(el),
                start,
                shown,
                decoding: false,
            }));
        }
        ImageSource::Loading | ImageSource::Decoding | ImageSource::Unavailable(_) => {
            look.class("missing");
            let loading = matches!(source, ImageSource::Loading | ImageSource::Decoding);
            let (fallback, why) = match &source {
                ImageSource::Loading => {
                    ("Image loading", "The image data has not arrived".to_owned())
                }
                ImageSource::Decoding => ("Image loading", "Decoding the image".to_owned()),
                ImageSource::Unavailable(e) => (
                    "Image unavailable",
                    format!("Not an image format Auk can show ({e})"),
                ),
                ImageSource::Ready(_) => unreachable!("handled above"),
            };
            let text = alt.unwrap_or(fallback);
            let card = part(cx, m, "div", "sf-img-card");
            cx.dom.attr(card, "role", "img");
            cx.dom.attr(card, "title", &why);
            let state = if loading {
                "image loading"
            } else {
                "image unavailable"
            };
            cx.dom
                .attr(card, "aria-label", &format!("{text} ({state})"));
            if loading {
                cx.dom.attr(card, "aria-busy", "true");
            }
            if let Some(w) = size("w") {
                look.style(&format!(
                    "max-width: {}px;",
                    crate::common::fmt_num(f64::from(w))
                ));
            }
            let ic = cx.dom.child(card, "span", "sf-img-ic");
            icon(cx, ic, "image");
            cx.dom.child_text(card, "span", "sf-img-alt", text);
            // The picture draws in when it's decoded: the clock watches for it.
            if matches!(source, ImageSource::Decoding) {
                m.clock = true;
                m.extra = Extra::Image(Box::new(ImageState {
                    decoding: true,
                    ..Default::default()
                }));
            }
        }
    }
    let over = super::slot(cx, m, 0, "div", "sf-img-over");
    cx.dom.doc.append_child(m.el, over);
    m.generated.push(over);
}

/// The blob an `image` node names, as cache keys spell it.
fn node_blob(node: NodeRef) -> String {
    str_prop(node.props(), "blob")
        .map(str::to_ascii_lowercase)
        .unwrap_or_default()
}

/// An animated image's next frame (§7.13), or a picture decoded in the background drawn in.
/// Returns whether it still needs the clock.
pub(crate) fn tick_image(cx: &mut Cx, node: NodeRef, m: &mut Mount) -> bool {
    let Extra::Image(st) = &mut m.extra else {
        return false;
    };
    // Decoding in the background: redraw the node once the picture is ready (§8.5).
    if st.decoding {
        crate::image::pump();
        if crate::image::decoding_blob(&node_blob(node)) {
            return true;
        }
        crate::view::render_node(cx, m, node);
        return m.clock;
    }
    let (Some(pic), Some(el)) = (st.picture.clone(), st.frame) else {
        return false;
    };
    if cx.dom.doc.try_node(el).is_none() {
        return false;
    }
    let (i, running) = if cx.set.reduce_motion {
        (0, false)
    } else {
        pic.frame_at(cx.now - st.start)
    };
    if i != st.shown {
        let mut content = pic.frames[i].content.clone();
        content.natural = st.natural;
        cx.dom.doc.set_image(el, Some(content));
        st.shown = i;
    }
    running
}
