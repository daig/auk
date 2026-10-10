//! `editor` and `input`: program-owned text fields (§7.9, research/elements.md §6). The view
//! draws text, caret, selection, decorations, ghost, placeholder and IME preedit; keys stay keys.

use super::{begin, part};
use crate::common::{bool_prop, num_prop, str_prop};
use crate::highlight::{self, Highlighter};
use crate::spans::{self, SpanStyle};
use crate::state::Selection;
use crate::view::{Cx, Extra, Look, Mount};
use auk_tsp::NodeRef;
use auk_ui::NodeId;
use serde_json::Value;

/// Rendered field state.
#[derive(Debug, Default)]
pub(crate) struct FieldState {
    pub text: String,
    pub caret: Option<NodeId>,
    pub focused: bool,
    /// When the caret last moved (blink restarts).
    pub epoch: f64,
    pub scroll: Option<NodeId>,
    pub text_el: Option<NodeId>,
    pub hl: Option<Highlighter>,
    pub hl_key: String,
    pub sig: String,
}

/// UTF-16 offset → byte offset in `s` (inside a surrogate pair rounds down; past the end clamps).
pub fn utf16_to_byte(s: &str, off: u64) -> usize {
    let mut units = 0u64;
    for (i, c) in s.char_indices() {
        let n = c.len_utf16() as u64;
        if units + n > off {
            return i;
        }
        units += n;
    }
    s.len()
}

pub fn byte_to_utf16(s: &str, byte: usize) -> u64 {
    s[..byte.min(s.len())].encode_utf16().count() as u64
}

/// The field's text and caret as the program last sent them.
pub(crate) fn field_text(node: NodeRef) -> String {
    match node.prop("text") {
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}

pub(crate) fn field_cursor(node: NodeRef) -> u64 {
    let text = field_text(node);
    let len = byte_to_utf16(&text, text.len());
    num_prop(node.props(), "cursor")
        .map(|c| (c.max(0.0) as u64).min(len))
        .unwrap_or(len)
}

#[derive(Default, Clone)]
struct Seg {
    sel: bool,
    tokens: Vec<&'static str>,
    color: Option<String>,
    tk: Option<&'static str>,
}

pub(crate) fn field(cx: &mut Cx, node: NodeRef, m: &mut Mount, look: &mut Look) {
    let p = node.props();
    let is_editor = node.kind() == "editor";
    let text = field_text(node);
    let cursor_u = field_cursor(node);
    let readonly = bool_prop(p, "readonly");
    let focused = cx.focus() == Some(node.id());
    let lang = str_prop(p, "lang");
    let code = lang.is_some();
    let highlight_on = p.get("highlight").and_then(Value::as_bool).unwrap_or(true);
    let max_lines = num_prop(p, "maxLines").filter(|n| *n > 0.0 && is_editor);
    let nowrap = bool_prop(p, "nowrap") || !is_editor;
    let mode = str_prop(p, "mode");
    // omp's composer draws its Markdown inline (book:9198-9201); a code field stays code.
    let markdown = is_editor && !code && in_omp_editor(node);
    look.class_if(markdown, "markdown");
    look.class_if(focused, "focused");
    look.class_if(readonly, "readonly");
    look.class_if(text.is_empty(), "empty");
    look.class_if(code, "code");
    look.class_if(max_lines.is_some(), "capped");
    look.class_if(nowrap && is_editor, "nowrap");
    look.attr("role", "textbox");
    look.attr("aria-multiline", if is_editor { "true" } else { "false" });
    if readonly {
        look.attr("aria-readonly", "true");
    }
    if let Some(ph) = str_prop(p, "placeholder") {
        look.attr("aria-placeholder", ph);
    }
    if str_prop(p, "aria").is_none() {
        let label = if is_editor {
            "Message".to_owned()
        } else {
            str_prop(p, "placeholder").unwrap_or("Search").to_owned()
        };
        look.attr("aria-label", label);
    }
    if let Some(md) = mode {
        look.attr("data-mode", md.to_ascii_lowercase());
    }
    if let Some(n) = max_lines {
        look.style(&format!("--sf-ed-max: {};", crate::common::fmt_num(n)));
    }
    // Selection: the view's native one, else the program's anchor..cursor.
    let local = cx.st.selection.get(node.id()).copied();
    let program = num_prop(p, "anchor").map(|a| Selection {
        anchor: a.max(0.0) as u64,
        cursor: cursor_u,
    });
    let sel = local.or(program).filter(|s| !s.is_empty());
    look.class_if(local.is_some_and(|s| !s.is_empty()), "selecting");
    let caret_u = local.map(|s| s.cursor).unwrap_or(cursor_u);
    let preedit = cx
        .st
        .preedit
        .as_ref()
        .filter(|pe| pe.id == node.id() && focused)
        .cloned();
    // Highlighting.
    let hl_key = format!("{lang:?}|{highlight_on}");
    if !matches!(m.extra, Extra::Field(_)) {
        begin(cx, m);
        let scroll = part(cx, m, "div", "sf-ed-scroll");
        let t = cx.dom.child(scroll, "div", "sf-ed-text");
        let chip = part(cx, m, "span", "sf-ed-mode");
        let _ = chip;
        m.extra = Extra::Field(Box::new(FieldState {
            scroll: Some(scroll),
            text_el: Some(t),
            ..Default::default()
        }));
    }
    let now = cx.now;
    let chip = m.generated[1];
    let Extra::Field(st) = &mut m.extra else {
        unreachable!()
    };
    if st.hl_key != hl_key {
        st.hl = lang
            .filter(|_| highlight_on)
            .and_then(|l| highlight::find(Some(l), None))
            .map(Highlighter::new);
        st.hl_key = hl_key;
    }
    if let Some(h) = &mut st.hl {
        h.update(&text);
    }
    let sig = format!(
        "{text}\u{0}{caret_u}\u{0}{sel:?}\u{0}{focused}\u{0}{preedit:?}\u{0}{:?}\u{0}{:?}\u{0}{:?}\u{0}{:?}\u{0}{:?}\u{0}{:?}\u{0}{readonly}\u{0}{markdown}",
        p.get("decor"),
        p.get("ghost"),
        p.get("placeholder"),
        p.get("prompt"),
        p.get("gutter"),
        p.get("virt"),
    );
    if st.sig != sig {
        if st.focused != focused || st.text != text || st.sig.is_empty() {
            st.epoch = now;
        }
        st.epoch = now;
        st.sig = sig;
        let t = st.text_el.expect("text");
        cx.dom.clear(t);
        st.caret = None;
        // Prompt.
        if p.get("prompt").is_some_and(|v| !v.is_null()) {
            let pr = cx.dom.child(t, "span", "sf-ed-prompt");
            spans::render(&mut cx.dom, pr, &spans::parse_spans(p.get("prompt")));
        }
        let caret_b = utf16_to_byte(&text, caret_u);
        let sel_b = sel.map(|s| {
            let (a, b) = s.range();
            (utf16_to_byte(&text, a), utf16_to_byte(&text, b))
        });
        // Decorations: (from, to, style) in bytes.
        let decor: Vec<(usize, usize, SpanStyle)> = p
            .get("decor")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|d| {
                        let o = d.as_object()?;
                        let f = o.get("from")?.as_f64()?.max(0.0) as u64;
                        let to = o.get("to")?.as_f64()?.max(0.0) as u64;
                        let (f, to) = (utf16_to_byte(&text, f), utf16_to_byte(&text, to));
                        (f < to).then(|| {
                            (
                                f,
                                to,
                                SpanStyle::parse(o.get("s").and_then(Value::as_str).unwrap_or("")),
                            )
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let gutter: Vec<Value> = p
            .get("gutter")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let virt: Vec<Value> = p
            .get("virt")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let lines: Vec<&str> = text.split('\n').collect();
        let md_runs = if markdown {
            super::edmd::runs(&text)
        } else {
            Vec::new()
        };
        let mut line_start = 0usize;
        let show_caret = !readonly || focused;
        for (li, line) in lines.iter().enumerate() {
            if let Some(g) = gutter.get(li).filter(|g| !g.is_null()) {
                let ge = cx.dom.child(t, "span", "sf-ed-gutter");
                cx.dom.attr(ge, "aria-hidden", "true");
                spans::render(&mut cx.dom, ge, &spans::parse_spans(Some(g)));
            }
            let line_end = line_start + line.len();
            // Cut points within the line.
            let mut cuts = vec![line_start, line_end];
            if let Some((a, b)) = sel_b {
                cuts.push(a.clamp(line_start, line_end));
                cuts.push(b.clamp(line_start, line_end));
            }
            for d in &decor {
                cuts.push(d.0.clamp(line_start, line_end));
                cuts.push(d.1.clamp(line_start, line_end));
            }
            if (line_start..=line_end).contains(&caret_b) {
                cuts.push(caret_b);
            }
            let runs = st
                .hl
                .as_ref()
                .filter(|h| li < h.line_count())
                .map(|h| h.line(li).1.clone());
            if let Some(r) = &runs {
                for (_, rr) in r {
                    cuts.push(line_start + rr.start);
                    cuts.push(line_start + rr.end);
                }
            }
            let md_line = md_runs.get(li).map(Vec::as_slice).unwrap_or(&[]);
            for (rr, _) in md_line {
                cuts.push(line_start + rr.start);
                cuts.push(line_start + rr.end);
            }
            cuts.sort_unstable();
            cuts.dedup();
            for w in cuts
                .windows(2)
                .map(|w| (w[0], w[1]))
                .chain(std::iter::once((line_end, line_end)))
            {
                let (a, b) = w;
                if a == caret_b && show_caret && st.caret.is_none() {
                    match &preedit {
                        Some(pe) => {
                            let pe_el = cx.dom.child(t, "span", "sf-ed-preedit");
                            let chars: Vec<char> = pe.text.chars().collect();
                            let at = pe.cursor.min(chars.len());
                            let before: String = chars[..at].iter().collect();
                            let after: String = chars[at..].iter().collect();
                            if !before.is_empty() {
                                cx.dom.text(pe_el, &before);
                            }
                            let c = cx.dom.child(pe_el, "span", "sf-caret");
                            st.caret = Some(c);
                            if !after.is_empty() {
                                cx.dom.text(pe_el, &after);
                            }
                        }
                        None => {
                            let c = cx.dom.child(t, "span", "sf-caret");
                            cx.dom.attr(c, "aria-hidden", "true");
                            st.caret = Some(c);
                            if text.is_empty() {
                                let ghost = str_prop(p, "ghost").unwrap_or("");
                                if !ghost.is_empty() {
                                    cx.dom.child_text(t, "span", "sf-ed-ghost", ghost);
                                } else if let Some(ph) = str_prop(p, "placeholder") {
                                    cx.dom.child_text(t, "span", "sf-ed-ph", ph);
                                }
                            } else if let Some(g) =
                                str_prop(p, "ghost").filter(|g| !g.is_empty() && focused)
                            {
                                cx.dom.child_text(t, "span", "sf-ed-ghost", g);
                            }
                        }
                    }
                }
                if a >= b {
                    continue;
                }
                let piece = &text[a..b];
                let mut seg = Seg::default();
                if let Some((sa, sb)) = sel_b {
                    seg.sel = sa <= a && b <= sb;
                }
                for d in &decor {
                    if d.0 <= a && b <= d.1 {
                        for tkn in &d.2.tokens {
                            if !seg.tokens.contains(tkn) {
                                seg.tokens.push(tkn);
                            }
                        }
                        if seg.color.is_none() {
                            seg.color = d.2.color.clone();
                        }
                    }
                }
                if let Some(r) = &runs {
                    seg.tk = r
                        .iter()
                        .find(|(_, rr)| line_start + rr.start <= a && a < line_start + rr.end)
                        .and_then(|(tk, _)| tk.map(|t| t.class()));
                }
                let host = if seg.sel {
                    cx.dom.child(t, "span", "sf-ed-sel")
                } else {
                    t
                };
                let mut class: Vec<String> =
                    seg.tokens.iter().map(|x| format!("sf-t-{x}")).collect();
                if let Some(tk) = seg.tk {
                    class.push(tk.to_owned());
                }
                if let Some((_, c)) = md_line
                    .iter()
                    .find(|(rr, _)| line_start + rr.start <= a && a < line_start + rr.end)
                {
                    class.push(c.clone());
                }
                if class.is_empty() && seg.color.is_none() {
                    spans::render_plain(&mut cx.dom, host, piece);
                } else {
                    let s = cx.dom.child(host, "span", &class.join(" "));
                    if let Some(c) = &seg.color {
                        cx.dom.set_style(s, &format!("color: {c}"));
                    }
                    cx.dom.text(s, piece);
                }
            }
            if let Some(v) = virt.get(li).filter(|g| !g.is_null()) {
                let ve = cx.dom.child(t, "span", "sf-ed-virt");
                cx.dom.attr(ve, "aria-hidden", "true");
                spans::render(&mut cx.dom, ve, &spans::parse_spans(Some(v)));
            }
            if li + 1 < lines.len() {
                cx.dom.text(t, "\n");
            }
            line_start = line_end + 1;
        }
        // The folded composer's caption shows the draft.
        if st.text != text {
            crate::deco::field_changed(cx, node);
        }
        st.text = text;
        st.focused = focused;
    }
    // The mode chip.
    match mode {
        Some(md) => {
            let lower = md.to_ascii_lowercase();
            cx.dom.set_class(chip, "sf-ed-mode on");
            cx.dom.attr(chip, "data-mode", &lower);
            cx.dom
                .attr(chip, "title", &format!("Mode: {}", md.to_uppercase()));
            cx.dom.set_only_text(chip, &md.to_uppercase());
        }
        None => {
            cx.dom.set_class(chip, "sf-ed-mode");
            cx.dom.set_only_text(chip, "");
        }
    }
    m.clock = focused && st.caret.is_some();
    if let Some(top) = num_prop(p, "topline") {
        cx.st.topline.insert(
            node.id().to_owned(),
            (st.scroll.expect("scroll"), top.max(0.0)),
        );
    } else {
        cx.st.topline.remove(node.id());
    }
    if focused
        && max_lines.is_some()
        && p.get("followCursor").and_then(Value::as_bool) != Some(false)
    {
        cx.st
            .caret_follow
            .insert(node.id().to_owned(), (st.scroll.expect("scroll"), st.caret));
    } else {
        cx.st.caret_follow.remove(node.id());
    }
    tick(cx, node, m);
}

/// Whether a field sits in omp's composer: it or an ancestor has an `omp.editor*` role.
pub(crate) fn in_omp_editor(node: NodeRef) -> bool {
    std::iter::successors(Some(node), |n| n.parent())
        .any(|n| str_prop(n.props(), "role").is_some_and(|r| r.starts_with("omp.editor")))
}

/// Caret blink: only the focused field, steady under Reduce Motion, hidden or unfocused panes.
pub(crate) fn tick(cx: &mut Cx, _node: NodeRef, m: &mut Mount) -> bool {
    let Extra::Field(st) = &m.extra else {
        return false;
    };
    let Some(c) = st.caret else { return false };
    if !st.focused {
        cx.dom.set_style(c, "");
        return false;
    }
    if cx.set.reduce_motion {
        cx.dom.set_style(c, "");
        return false;
    }
    // Steady for the first 500 ms after a change, then 1 s blinks.
    let t = cx.now - st.epoch;
    let on = t < 500.0 || ((t - 500.0).rem_euclid(1000.0)) < 500.0;
    cx.dom.set_style(c, if on { "" } else { "opacity: 0" });
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_offsets() {
        let s = "a😀b";
        assert_eq!(utf16_to_byte(s, 0), 0);
        assert_eq!(utf16_to_byte(s, 1), 1);
        assert_eq!(
            utf16_to_byte(s, 2),
            1,
            "inside a surrogate pair rounds down"
        );
        assert_eq!(utf16_to_byte(s, 3), 5);
        assert_eq!(utf16_to_byte(s, 99), 6);
        assert_eq!(byte_to_utf16(s, 5), 3);
    }
}
