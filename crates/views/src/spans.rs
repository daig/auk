//! Spans `{t, s?, href?, fx?}` (§8.1, book:4845-4960): style tokens, omp program tokens, links,
//! effects, keycaps, paths and Nerd Font icon runs.

use crate::dom::Dom;
use crate::icons;
use auk_ui::NodeId;
use serde_json::Value;

/// The semantic span tokens; each draws as `.sf-t-<token>`.
pub const SEMANTIC: [&str; 21] = [
    "muted", "dim", "strong", "em", "accent", "success", "warning", "error", "info", "code",
    "mono", "path", "key", "link", "num", "ins", "del", "mark", "typo", "icon", "hide",
];

/// Semantic tokens that name a color (`--sf-c-<token>`).
pub(crate) const COLOR_TOKENS: [&str; 14] = [
    "muted", "dim", "accent", "success", "warning", "error", "info", "link", "num", "path", "key",
    "ins", "del", "code",
];

/// omp's program tokens and the semantic token each maps to (`""` for plain), book:4896-4912.
pub fn program_token(t: &str) -> Option<&'static str> {
    Some(match t {
        "text" | "customMessageText" | "syntaxVariable" | "toolOutput" | "userMessageText" => "",
        "borderAccent" | "customMessageLabel" | "mdListBullet" | "statusLineModel"
        | "syntaxFunction" | "thinkingMedium" => "accent",
        "borderMuted" | "statusLineSep" | "thinkingOff" => "dim",
        "border"
        | "mdCodeBlockBorder"
        | "mdHr"
        | "mdLinkUrl"
        | "mdQuote"
        | "mdQuoteBorder"
        | "statusLineOutput"
        | "statusLineSpend"
        | "statusLineUntracked"
        | "syntaxComment"
        | "syntaxOperator"
        | "syntaxPunctuation"
        | "thinkingText"
        | "thinkingMinimal"
        | "toolDiffContext" => "muted",
        "thinkingXhigh" | "thinkingMax" | "toolDiffRemoved" => "error",
        "statusLineCost" | "statusLineGitClean" | "statusLineStaged" | "syntaxString" => "success",
        "bashMode" | "statusLineDirty" | "statusLineGitDirty" | "thinkingHigh" => "warning",
        "pythonMode"
        | "statusLineContext"
        | "statusLineSubagents"
        | "syntaxKeyword"
        | "syntaxType"
        | "thinkingLow" => "info",
        "mdCode" => "code",
        "mdCodeBlock" => "mono",
        "mdHeading" | "toolTitle" => "strong",
        "mdLink" => "link",
        "statusLinePath" => "path",
        "syntaxNumber" => "num",
        "toolDiffAdded" => "ins",
        _ => return None,
    })
}

fn token_ok(t: &str) -> bool {
    !t.is_empty()
        && t.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// What a span's `s` resolves to: classes, and an inline color for a program token.
#[derive(Debug, Default, PartialEq)]
pub struct SpanStyle {
    pub tokens: Vec<&'static str>,
    pub color: Option<String>,
}

impl SpanStyle {
    pub fn parse(s: &str) -> SpanStyle {
        let mut out = SpanStyle::default();
        for t in s.split_ascii_whitespace() {
            if let Some(&sem) = SEMANTIC.iter().find(|&&x| x == t) {
                if !out.tokens.contains(&sem) {
                    out.tokens.push(sem);
                }
                continue;
            }
            if t.ends_with("Bg") || !token_ok(t) {
                continue;
            }
            let mapped = program_token(t);
            if let Some(m) = mapped
                && let Some(&sem) = SEMANTIC.iter().find(|&&x| x == m)
                && !out.tokens.contains(&sem)
            {
                out.tokens.push(sem);
            }
            if out.color.is_none() {
                let fallback = match mapped {
                    Some(m) if COLOR_TOKENS.contains(&m) => format!("var(--sf-c-{m})"),
                    _ => "inherit".to_owned(),
                };
                out.color = Some(format!("var(--sf-p-{t}, {fallback})"));
            }
        }
        out
    }

    pub fn class(&self) -> String {
        self.tokens
            .iter()
            .map(|t| format!("sf-t-{t}"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn has(&self, t: &str) -> bool {
        self.tokens.contains(&t)
    }
}

/// One span, normalized.
#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub t: String,
    pub s: String,
    pub href: Option<String>,
    pub fx: Option<String>,
}

/// `Spans = string | [Span | string]`.
pub fn parse_spans(v: Option<&Value>) -> Vec<Span> {
    let one = |v: &Value| -> Option<Span> {
        match v {
            Value::String(s) => Some(Span {
                t: s.clone(),
                s: String::new(),
                href: None,
                fx: None,
            }),
            Value::Object(o) => Some(Span {
                t: match o.get("t") {
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Number(n)) => n.to_string(),
                    _ => String::new(),
                },
                s: o.get("s").and_then(Value::as_str).unwrap_or("").to_owned(),
                href: o
                    .get("href")
                    .and_then(Value::as_str)
                    .filter(|h| !h.is_empty())
                    .map(str::to_owned),
                fx: o
                    .get("fx")
                    .and_then(Value::as_str)
                    .filter(|f| matches!(*f, "shimmer" | "pulse"))
                    .map(str::to_owned),
            }),
            Value::Number(n) => Some(Span {
                t: n.to_string(),
                s: String::new(),
                href: None,
                fx: None,
            }),
            _ => None,
        }
    };
    match v {
        Some(Value::Array(a)) => a.iter().filter_map(one).collect(),
        Some(v @ (Value::String(_) | Value::Object(_) | Value::Number(_))) => {
            one(v).into_iter().collect()
        }
        _ => Vec::new(),
    }
}

/// The plain text of spans.
pub fn spans_text(spans: &[Span]) -> String {
    spans.iter().map(|s| s.t.as_str()).collect()
}

/// Split a string into runs of Private Use Area characters and ordinary text.
fn pua_runs(t: &str) -> Vec<(bool, &str)> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut cur: Option<bool> = None;
    for (i, c) in t.char_indices() {
        let p = icons::is_private_use(c);
        if cur.is_some_and(|x| x != p) {
            out.push((cur.unwrap(), &t[start..i]));
            start = i;
        }
        cur = Some(p);
    }
    if let Some(p) = cur {
        out.push((p, &t[start..]));
    }
    out
}

/// Whether the shimmer/pulse effect spans need the clock.
pub fn has_fx(spans: &[Span]) -> bool {
    spans.iter().any(|s| s.fx.is_some())
}

/// Append spans to `parent`. Returns whether any effect span was drawn (it needs the clock).
pub(crate) fn render(dom: &mut Dom, parent: NodeId, spans: &[Span]) -> bool {
    let mut fx = false;
    // Pieces after splitting icon runs: (text, style, href, fx).
    let mut pieces: Vec<(String, SpanStyle, Option<&str>, Option<&str>)> = Vec::new();
    for sp in spans {
        if sp.t.is_empty() {
            continue;
        }
        let style = SpanStyle::parse(&sp.s);
        if style.has("icon") {
            pieces.push((sp.t.clone(), style, sp.href.as_deref(), sp.fx.as_deref()));
            continue;
        }
        for (pua, run) in pua_runs(&sp.t) {
            let mut st = SpanStyle {
                tokens: style.tokens.clone(),
                color: style.color.clone(),
            };
            if pua {
                st.tokens.push("icon");
            }
            pieces.push((run.to_owned(), st, sp.href.as_deref(), sp.fx.as_deref()));
        }
    }
    for i in 0..pieces.len() {
        let (text, style, href, effect) = &pieces[i];
        let plain = style.tokens.is_empty() && style.color.is_none() && href.is_none();
        if plain && effect.is_none() {
            dom.text(parent, text);
            continue;
        }
        let mut class = style.class();
        if style.has("icon") {
            // `.gap` when the next non-empty piece doesn't start with whitespace.
            let next = pieces[i + 1..].iter().find(|p| !p.0.is_empty());
            if next.is_some_and(|n| !n.0.starts_with(char::is_whitespace)) {
                class.push_str(" gap");
            }
        }
        if href.is_some() {
            if !class.is_empty() {
                class.push(' ');
            }
            class.push_str("sf-link");
        }
        if let Some(e) = effect {
            if !class.is_empty() {
                class.push(' ');
            }
            class.push_str(&format!("sf-fx-{e}"));
            fx = true;
        }
        let span = dom.child(parent, "span", &class);
        if let Some(c) = &style.color {
            dom.set_style(span, &format!("color: {c}"));
        }
        if let Some(h) = href {
            dom.attr(span, "data-href", h);
            dom.attr(span, "title", &format!("{h}  ⌘-click to open"));
        }
        if style.has("icon") {
            dom.attr(span, "aria-hidden", "true");
        }
        if *effect == Some("shimmer") {
            for (k, ch) in text.chars().enumerate() {
                let c = dom.child(span, "span", "sf-sh");
                dom.set_style(c, &format!("--i: {k}"));
                dom.text(c, &ch.to_string());
            }
            continue;
        }
        if style.has("key")
            && let Some(caps) = icons::key_id(text)
        {
            dom.attr(span, "title", text);
            dom.text(span, &caps);
            continue;
        }
        if style.has("path")
            && let Some(slash) = text.rfind('/')
            && slash + 1 < text.len()
        {
            dom.child_text(span, "span", "dir", &text[..=slash]);
            dom.text(span, &text[slash + 1..]);
            continue;
        }
        dom.text(span, text);
    }
    fx
}

/// Plain text with Nerd Font runs split into icon spans (for `md` prose and text fields).
pub(crate) fn render_plain(dom: &mut Dom, parent: NodeId, text: &str) {
    for (pua, run) in pua_runs(text) {
        if pua {
            let s = dom.child(parent, "span", "sf-t-icon");
            dom.attr(s, "aria-hidden", "true");
            dom.text(s, run);
        } else {
            dom.text(parent, run);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tokens_and_program_tokens() {
        let s = SpanStyle::parse("strong error strong");
        assert_eq!(s.tokens, vec!["strong", "error"]);
        assert_eq!(s.color, None);
        let s = SpanStyle::parse("toolTitle");
        assert_eq!(s.tokens, vec!["strong"]);
        assert_eq!(s.color.as_deref(), Some("var(--sf-p-toolTitle, inherit)"));
        let s = SpanStyle::parse("syntaxString");
        assert_eq!(
            s.color.as_deref(),
            Some("var(--sf-p-syntaxString, var(--sf-c-success))")
        );
        let s = SpanStyle::parse("myTone userMessageBg");
        assert!(s.tokens.is_empty());
        assert_eq!(s.color.as_deref(), Some("var(--sf-p-myTone, inherit)"));
        assert_eq!(SpanStyle::parse("bogus!").color, None);
    }

    #[test]
    fn spans_parse() {
        let v = json!(["a", {"t": "b", "s": "muted", "fx": "glow"}, 3, null]);
        let s = parse_spans(Some(&v));
        assert_eq!(s.len(), 3);
        assert_eq!(s[1].fx, None);
        assert_eq!(spans_text(&s), "ab3");
        assert_eq!(
            pua_runs("a\u{e0a0}\u{e0a1}b"),
            vec![(false, "a"), (true, "\u{e0a0}\u{e0a1}"), (false, "b")]
        );
    }
}
