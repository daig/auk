//! Kinds on rich sub-engines (§8.5, §18.4): `md code diff ansi rows`, and the T2 `math` and
//! `image` drawn as plain fallbacks.

use super::layout::preview_lines;
use super::{begin, icon, part};
use crate::ansi::AnsiState;
use crate::common::{bool_prop, num_prop, str_prop};
use crate::diff::{self, DLine, Hunk, Op};
use crate::dom::Dom;
use crate::highlight::{self, Highlighter, Runs};
use crate::spans;
use crate::state::{SurfaceState, state_key};
use crate::view::{Cx, Extra, Look, Mount};
use auk_tsp::NodeRef;
use auk_ui::NodeId;
use serde_json::Value;

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
    st.update(&mut cx.dom, m.el, &text, stream, marks);
    // Blocks are the node's generated parts.
    m.generated = st.blocks.iter().map(|b| b.el).collect();
    // Many blocks: virtualize (§8.3).
    let many = st.blocks.len() > 64;
    super::virtualize(cx, node.id(), m.el, many.then_some(60.0));
    m.clock = stream && !cx.set.reduce_motion;
}

/// The streaming caret pulses.
pub(crate) fn tick_md(cx: &mut Cx, _node: NodeRef, m: &mut Mount) -> bool {
    let Extra::Md(st) = &m.extra else {
        return false;
    };
    let Some(c) = st.caret else { return false };
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
    pub hl: Option<Highlighter>,
    pub key: String,
    pub body: Option<NodeId>,
    pub lines: Vec<NodeId>,
    pub text: String,
    /// Lines re-rendered by the last update.
    pub redrawn: usize,
}

/// A line's runs with emphasized byte ranges split out.
fn line_into(
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
            hl: highlight::find(lang, path).map(Highlighter::new),
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
    let first = match &mut st.hl {
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
        let runs = st.hl.as_ref().map(|h| h.line(i).1);
        let em: &[std::ops::Range<usize>] = mark.map(|m| m.2.as_slice()).unwrap_or(&[]);
        line_into(&mut cx.dom, lt, line, runs, em);
        st.lines.push(ln);
    }
    st.redrawn = new_lines.len() - first;
    st.text = text;
    // Long code virtualizes (§8.3: over 400 lines).
    let long = st.lines.len() > 400;
    super::virtualize(cx, node.id(), body, long.then_some(17.0));
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
    let side = |op_keep: Op| -> (Vec<String>, Option<Highlighter>) {
        let lines: Vec<String> = hunks
            .iter()
            .flat_map(|h| h.lines.iter())
            .filter(|l| l.op == Op::Ctx || l.op == op_keep)
            .map(|l| l.text.clone())
            .collect();
        let hl = syn.map(|s| {
            let mut h = Highlighter::new(s);
            h.update(&lines.join("\n"));
            h
        });
        (lines, hl)
    };
    let (_, old_hl) = side(Op::Del);
    let (_, new_hl) = side(Op::Add);
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
                diff_line(cx, r, old, &old_hl, true, true);
                diff_line(cx, r, new, &new_hl, true, false);
            } else {
                match (old, new) {
                    (Some(o), Some(_)) => diff_line(cx, body, Some(o), &old_hl, false, true),
                    (Some(o), None) => diff_line(cx, body, Some(o), &old_hl, false, true),
                    (None, Some(n)) => diff_line(cx, body, Some(n), &new_hl, false, false),
                    (None, None) => {}
                }
            }
        }
        let old_len = h.lines.iter().filter(|l| l.op != Op::Add).count() as u64;
        prev_end = Some(h.old_start + old_len);
    }
}

/// One side of a diff row: the line and its index in that side's highlighted text.
type Side<'a> = Option<(&'a DLine, usize)>;

fn diff_line(
    cx: &mut Cx,
    parent: NodeId,
    l: Option<(&DLine, usize)>,
    hl: &Option<Highlighter>,
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
    let runs = hl
        .as_ref()
        .filter(|h| idx < h.line_count())
        .map(|h| h.line(idx).1.clone());
    line_into(&mut cx.dom, t, &l.text, runs.as_ref(), &l.em);
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

pub(crate) fn math(cx: &mut Cx, node: NodeRef, m: &mut Mount, look: &mut Look) {
    begin(cx, m);
    let display = bool_prop(node.props(), "display");
    look.class_if(display, "display");
    let tex = text_prop(node);
    let md = part(cx, m, "div", "md");
    let el = cx.dom.child(
        md,
        if display { "div" } else { "span" },
        if display { "math-display" } else { "math" },
    );
    cx.dom.attr(el, "data-tex", &tex);
    cx.dom.text(el, &tex);
}

pub(crate) fn image(cx: &mut Cx, node: NodeRef, m: &mut Mount, look: &mut Look) {
    begin(cx, m);
    let p = node.props();
    look.class("missing");
    let alt = str_prop(p, "alt").unwrap_or("Image").to_owned();
    let card = part(cx, m, "div", "sf-img-card");
    cx.dom.attr(card, "role", "img");
    cx.dom.attr(card, "aria-label", &alt);
    if let Some(w) = num_prop(p, "w") {
        look.style(&format!("max-width: {}px;", crate::common::fmt_num(w)));
    }
    let ic = cx.dom.child(card, "span", "sf-img-ic");
    icon(cx, ic, "image");
    cx.dom.child_text(card, "span", "sf-img-alt", &alt);
    let over = super::slot(cx, m, 0, "div", "sf-img-over");
    cx.dom.doc.append_child(m.el, over);
    m.generated.push(over);
}
