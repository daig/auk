//! `capture` (§11.1): a pane's content as plain text, ANSI (SGR attributes kept) or HTML, the
//! screen or the whole buffer, optionally with its surfaces' text in place of their anchor rows.
//!
//! With surfaces, an inline or flow surface's text replaces its anchor row, docks follow the
//! grid, and a screen surface replaces the grid. A surface's text is its document's text: every
//! string a node shows (`text`, `label`, `title`, `value`, span runs), block nodes on their own
//! lines.

use auk_term::{Cell, Color, Flags, Line, Terminal};
use auk_tsp::{Mode, NodeRef, Surface, TspPane};
use serde_json::Value;

/// The output format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Text,
    Ansi,
    Html,
}

/// What to capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    pub format: Format,
    pub scrollback: bool,
    pub surfaces: bool,
}

impl Options {
    /// Parses `--ansi`, `--html`, `--scrollback` and `--surfaces` (other words are left alone).
    pub fn parse(args: &[String]) -> Result<Options, String> {
        let mut o = Options {
            format: Format::Text,
            scrollback: false,
            surfaces: false,
        };
        for a in args {
            match a.as_str() {
                "--ansi" | "--html" if o.format != Format::Text => {
                    return Err("capture: --ansi and --html exclude each other".into());
                }
                "--ansi" => o.format = Format::Ansi,
                "--html" => o.format = Format::Html,
                "--scrollback" => o.scrollback = true,
                "--surfaces" => o.surfaces = true,
                _ => {}
            }
        }
        Ok(o)
    }
}

/// One piece of output: a grid line or a surface's text.
enum Piece<'a> {
    Line(Line),
    Surface(&'a Surface),
}

/// Captures a pane.
pub fn capture(term: &Terminal, tsp: &TspPane, opts: Options) -> String {
    let total = term.history_len() + usize::from(term.rows());
    let start = if opts.scrollback {
        0
    } else {
        term.history_len()
    };
    let mut pieces: Vec<Piece<'_>> = Vec::new();
    let screen = opts
        .surfaces
        .then(|| {
            tsp.surfaces()
                .iter()
                .rev()
                .find(|s| s.mode() == Mode::Screen && s.is_live() && !tsp.covered(s.key()))
        })
        .flatten();
    if let Some(s) = screen {
        pieces.push(Piece::Surface(s));
    } else {
        // Anchored surfaces by line (the main screen's lines; only on the main screen).
        let anchored: Vec<(Line, &Surface)> = if opts.surfaces && !term.modes().alt_screen {
            tsp.surfaces()
                .iter()
                .filter(|s| s.mode() != Mode::Screen)
                .filter_map(|s| Some((term.anchor_line(s.anchor()?)?, s)))
                .collect()
        } else {
            Vec::new()
        };
        for line in start..total {
            let here: Vec<&Surface> = anchored
                .iter()
                .filter(|(l, _)| *l == line)
                .map(|(_, s)| *s)
                .collect();
            if here.is_empty() {
                pieces.push(Piece::Line(line));
            } else {
                pieces.extend(here.into_iter().map(Piece::Surface));
            }
        }
    }
    let mut out = String::new();
    if opts.format == Format::Html {
        out.push_str("<pre class=\"auk-capture\">");
    }
    let n = pieces.len();
    for (i, piece) in pieces.iter().enumerate() {
        let last = i + 1 == n;
        match piece {
            Piece::Line(line) => {
                let Some(row) = term.line(*line) else { continue };
                let text = match opts.format {
                    Format::Text => plain_row(&row.cells),
                    Format::Ansi => ansi_row(&row.cells),
                    Format::Html => html_row(&row.cells, term.links()),
                };
                out.push_str(&text);
                if !(row.wrapped && opts.format == Format::Text && !last) && !last {
                    out.push('\n');
                }
            }
            Piece::Surface(s) => {
                let text = surface_text(s);
                match opts.format {
                    Format::Html => {
                        out.push_str(&format!(
                            "<div class=\"auk-surface\" data-sf=\"{}\" data-mode=\"{}\">{}</div>",
                            escape(s.id()),
                            s.mode().as_str(),
                            escape(&text)
                        ));
                    }
                    _ => out.push_str(&text),
                }
                if !last && opts.format != Format::Html {
                    out.push('\n');
                }
            }
        }
    }
    if opts.surfaces && screen.is_none() {
        for s in tsp.surfaces() {
            if !s.is_live() || s.mode() == Mode::Screen {
                continue;
            }
            let dock = s.document().get("dock").map(|n| node_text(n));
            if let Some(text) = dock.filter(|t| !t.is_empty()) {
                if opts.format == Format::Html {
                    out.push_str(&format!(
                        "<div class=\"auk-dock\" data-sf=\"{}\">{}</div>",
                        escape(s.id()),
                        escape(&text)
                    ));
                } else {
                    out.push('\n');
                    out.push_str(&text);
                }
            }
        }
    }
    if opts.format == Format::Html {
        out.push_str("</pre>");
    }
    out
}

fn visible(cell: &Cell) -> bool {
    !cell.flags.has(Flags::SPACER) && !cell.flags.has(Flags::LEADING_SPACER)
}

fn plain_row(cells: &[Cell]) -> String {
    let mut line = String::new();
    for c in cells.iter().filter(|c| visible(c)) {
        line.push_str(if c.text.is_empty() { " " } else { &c.text });
    }
    line.trim_end_matches(' ').to_owned()
}

/// The cells up to the last one that shows something (text or a background).
fn used(cells: &[Cell]) -> &[Cell] {
    let end = cells
        .iter()
        .rposition(|c| !c.text.is_empty() || c.bg != Color::Default || c.flags.has(Flags::INVERSE))
        .map_or(0, |i| i + 1);
    &cells[..end]
}

fn sgr(c: &Cell) -> String {
    let mut p: Vec<String> = vec!["0".into()];
    let f = c.flags;
    for (bit, code) in [
        (Flags::BOLD, "1"),
        (Flags::DIM, "2"),
        (Flags::ITALIC, "3"),
        (Flags::UNDERLINE, "4"),
        (Flags::DOUBLE_UNDERLINE, "21"),
        (Flags::CURLY_UNDERLINE, "4:3"),
        (Flags::DOTTED_UNDERLINE, "4:4"),
        (Flags::DASHED_UNDERLINE, "4:5"),
        (Flags::BLINK, "5"),
        (Flags::INVERSE, "7"),
        (Flags::HIDDEN, "8"),
        (Flags::STRIKE, "9"),
        (Flags::OVERLINE, "53"),
    ] {
        if f.has(bit) {
            p.push(code.into());
        }
    }
    color_sgr(c.fg, 30, 90, 38, &mut p);
    color_sgr(c.bg, 40, 100, 48, &mut p);
    format!("\x1b[{}m", p.join(";"))
}

fn color_sgr(color: Color, base: u8, bright: u8, ext: u8, p: &mut Vec<String>) {
    match color {
        Color::Default => {}
        Color::Indexed(n @ 0..=7) => p.push((base + n).to_string()),
        Color::Indexed(n @ 8..=15) => p.push((bright + n - 8).to_string()),
        Color::Indexed(n) => p.push(format!("{ext};5;{n}")),
        Color::Rgb(rgb) => p.push(format!("{ext};2;{};{};{}", rgb.0, rgb.1, rgb.2)),
    }
}

fn style_key(c: &Cell) -> (Color, Color, u16) {
    (c.fg, c.bg, c.flags.0 & !(Flags::WIDE | Flags::SPACER | Flags::LEADING_SPACER))
}

fn ansi_row(cells: &[Cell]) -> String {
    let mut out = String::new();
    let mut current = (Color::Default, Color::Default, 0);
    for c in used(cells).iter().filter(|c| visible(c)) {
        let key = style_key(c);
        if key != current {
            out.push_str(&sgr(c));
            current = key;
        }
        out.push_str(if c.text.is_empty() { " " } else { &c.text });
    }
    if current != (Color::Default, Color::Default, 0) {
        out.push_str("\x1b[0m");
    }
    out
}

fn html_row(cells: &[Cell], links: &[String]) -> String {
    let mut out = String::new();
    let mut run = String::new();
    let mut current: Option<(Color, Color, u16, u32)> = None;
    let flush = |out: &mut String, run: &mut String, key: Option<(Color, Color, u16, u32)>| {
        if run.is_empty() {
            return;
        }
        let Some((fg, bg, flags, link)) = key else {
            out.push_str(&escape(run));
            run.clear();
            return;
        };
        let css = html_style(fg, bg, Flags(flags));
        let text = escape(run);
        let span = if css.is_empty() {
            text
        } else {
            format!("<span style=\"{css}\">{text}</span>")
        };
        match links.get(link as usize).filter(|_| link != 0) {
            Some(url) => out.push_str(&format!("<a href=\"{}\">{span}</a>", escape(url))),
            None => out.push_str(&span),
        }
        run.clear();
    };
    for c in used(cells).iter().filter(|c| visible(c)) {
        let (fg, bg, flags) = style_key(c);
        let key = Some((fg, bg, flags, c.link));
        if key != current {
            flush(&mut out, &mut run, current);
            current = key;
        }
        run.push_str(if c.text.is_empty() { " " } else { &c.text });
    }
    flush(&mut out, &mut run, current);
    out
}

fn html_style(fg: Color, bg: Color, flags: Flags) -> String {
    let (fg, bg) = if flags.has(Flags::INVERSE) {
        (
            if bg == Color::Default { None } else { Some(bg) },
            Some(fg),
        )
    } else {
        (Some(fg), Some(bg))
    };
    let mut css = Vec::new();
    match fg.map(css_color) {
        Some(Some(c)) => css.push(format!("color:{c}")),
        _ if flags.has(Flags::INVERSE) => css.push("color:var(--bg,#fff)".into()),
        _ => {}
    }
    match bg.map(css_color) {
        Some(Some(c)) => css.push(format!("background-color:{c}")),
        _ if flags.has(Flags::INVERSE) => css.push("background-color:var(--fg,#000)".into()),
        _ => {}
    }
    if flags.has(Flags::BOLD) {
        css.push("font-weight:bold".into());
    }
    if flags.has(Flags::DIM) {
        css.push("opacity:0.6".into());
    }
    if flags.has(Flags::ITALIC) {
        css.push("font-style:italic".into());
    }
    let mut lines = Vec::new();
    if flags.0
        & (Flags::UNDERLINE
            | Flags::DOUBLE_UNDERLINE
            | Flags::CURLY_UNDERLINE
            | Flags::DOTTED_UNDERLINE
            | Flags::DASHED_UNDERLINE)
        != 0
    {
        lines.push("underline");
    }
    if flags.has(Flags::STRIKE) {
        lines.push("line-through");
    }
    if flags.has(Flags::OVERLINE) {
        lines.push("overline");
    }
    if !lines.is_empty() {
        css.push(format!("text-decoration:{}", lines.join(" ")));
    }
    if flags.has(Flags::HIDDEN) {
        css.push("visibility:hidden".into());
    }
    css.join(";")
}

/// A CSS color for a terminal color (xterm's default palette); `None` for the default.
fn css_color(color: Color) -> Option<String> {
    let (r, g, b) = match color {
        Color::Default => return None,
        Color::Rgb(c) => (c.0, c.1, c.2),
        Color::Indexed(n) => xterm(n),
    };
    Some(format!("#{r:02x}{g:02x}{b:02x}"))
}

fn xterm(n: u8) -> (u8, u8, u8) {
    const BASE: [(u8, u8, u8); 16] = [
        (0, 0, 0),
        (205, 0, 0),
        (0, 205, 0),
        (205, 205, 0),
        (0, 0, 238),
        (205, 0, 205),
        (0, 205, 205),
        (229, 229, 229),
        (127, 127, 127),
        (255, 0, 0),
        (0, 255, 0),
        (255, 255, 0),
        (92, 92, 255),
        (255, 0, 255),
        (0, 255, 255),
        (255, 255, 255),
    ];
    match n {
        0..=15 => BASE[n as usize],
        16..=231 => {
            let i = n - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + 40 * v };
            (level(i / 36), level((i / 6) % 6), level(i % 6))
        }
        _ => {
            let v = 8 + 10 * (n - 232);
            (v, v, v)
        }
    }
}

/// HTML-escapes text.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

/// A surface's text: its `main` region (or the whole document when it has no regions).
pub fn surface_text(s: &Surface) -> String {
    let doc = s.document();
    match doc.get("main") {
        Some(main) => node_text(main),
        None => node_text(doc.root()),
    }
}

/// Kinds whose content is inline: they continue the current line.
const INLINE: &[&str] = &[
    "span", "badge", "kbd", "code", "link", "icon", "spinner", "elapsed", "seg",
];

/// A node's text, block children on their own lines.
pub fn node_text(n: NodeRef<'_>) -> String {
    let mut lines: Vec<String> = vec![String::new()];
    collect(n, &mut lines);
    let out: Vec<String> = lines
        .into_iter()
        .map(|l| l.trim_end().to_owned())
        .filter(|l| !l.is_empty())
        .collect();
    out.join("\n")
}

/// Kinds whose children share one line.
const ROWS: &[&str] = &["row", "status"];

fn own_text(n: NodeRef<'_>) -> String {
    let mut own = String::new();
    for key in ["title", "label", "text", "value", "placeholder"] {
        if let Some(v) = n.prop(key) {
            spans(v, &mut own);
            if !own.is_empty() && !own.ends_with(' ') {
                own.push(' ');
            }
        }
    }
    own.trim_end().to_owned()
}

fn flatten(n: NodeRef<'_>, parts: &mut Vec<String>) {
    let own = own_text(n);
    if !own.is_empty() {
        parts.push(own);
    }
    for c in n.children() {
        flatten(c, parts);
    }
}

fn collect(n: NodeRef<'_>, lines: &mut Vec<String>) {
    let row = ROWS.contains(&n.kind())
        || (n.kind() == "el" && n.prop("tag").and_then(Value::as_str) == Some("tr"));
    if row {
        let mut parts = Vec::new();
        flatten(n, &mut parts);
        if !lines.last().is_some_and(String::is_empty) {
            lines.push(String::new());
        }
        lines.push(parts.join(" "));
        lines.push(String::new());
        return;
    }
    let inline = INLINE.contains(&n.kind())
        || (n.kind() == "el"
            && matches!(
                n.prop("tag").and_then(Value::as_str),
                Some("span" | "b" | "i" | "em" | "strong" | "code" | "a" | "kbd" | "label" | "td" | "th")
            ));
    if !inline && !lines.last().is_some_and(String::is_empty) {
        lines.push(String::new());
    }
    let own = own_text(n);
    let last = lines.last_mut().expect("never empty");
    if !own.is_empty() {
        if !last.is_empty() && !last.ends_with(' ') {
            last.push(' ');
        }
        last.push_str(&own);
    }
    for c in n.children() {
        collect(c, lines);
    }
    if !inline && !lines.last().is_some_and(String::is_empty) {
        lines.push(String::new());
    }
}

/// Text from a prop value: a string, a number, or span runs (`[{"t": …}, …]`).
fn spans(v: &Value, out: &mut String) {
    match v {
        Value::String(s) => out.push_str(s),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::Array(items) => {
            for item in items {
                match item {
                    Value::Object(o) => {
                        if let Some(t) = o.get("t") {
                            spans(t, out);
                        }
                    }
                    other => spans(other, out),
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn feed(t: &mut Terminal, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            let (n, _) = t.advance(bytes);
            bytes = &bytes[n..];
        }
    }

    fn term_with(bytes: &[u8]) -> (Terminal, TspPane) {
        let mut t = Terminal::new(20, 3, auk_term::Config::default());
        feed(&mut t, bytes);
        (t, TspPane::new(auk_tsp::Config::default()))
    }

    #[test]
    fn ansi_keeps_attributes_and_html_escapes() {
        let (t, p) = term_with(b"\x1b[1;31mred\x1b[0m <b>\r\n\x1b[42mgreen\x1b[0m");
        let o = |format| Options {
            format,
            scrollback: false,
            surfaces: false,
        };
        assert_eq!(capture(&t, &p, o(Format::Text)), "red <b>\ngreen\n");
        let ansi = capture(&t, &p, o(Format::Ansi));
        assert!(ansi.starts_with("\x1b[0;1;31mred\x1b[0m <b>\n\x1b[0;42mgreen\x1b[0m"), "{ansi:?}");
        let html = capture(&t, &p, o(Format::Html));
        assert!(html.contains("<span style=\"color:#cd0000;font-weight:bold\">red</span> &lt;b&gt;"), "{html}");
        assert!(html.contains("background-color:#00cd00"), "{html}");
    }

    #[test]
    fn surfaces_replace_their_anchor_rows() {
        let mut t = Terminal::new(30, 4, auk_term::Config::default());
        let mut p = TspPane::new(auk_tsp::Config::default());
        let msg = |verb: &str, body: serde_json::Value| format!("tsp;{verb};{body}").into_bytes();
        feed(&mut t, b"$ run\r\n");
        p.on_apc(&mut t, &msg("o", json!({"id": "s1", "mode": "inline"})));
        p.on_apc(
            &mut t,
            &msg(
                "f",
                json!({"sf": "s1", "s": 1, "ops": [["add", "main", "s1", null, {"id": "main", "k": "col", "c": [
                    {"id": "h", "k": "text", "p": {"text": "Files"}},
                    {"id": "r", "k": "row", "c": [{"id": "a", "k": "badge", "p": {"label": "3"}}, {"id": "b", "k": "text", "p": {"text": [{"t": "new"}]}}]}
                ]}]]}),
            ),
        );
        let text = capture(
            &t,
            &p,
            Options {
                format: Format::Text,
                scrollback: false,
                surfaces: true,
            },
        );
        assert!(text.starts_with("$ run\nFiles\n3 new"), "{text:?}");
    }
}
