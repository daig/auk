//! The `ansi` and `rows` sub-engine (§8.5): our own terminal core (auk-term) in a box, for SGR,
//! OSC 8, `\r`, `\b`, reflow on width change and follow-tail. Appends feed only the new tail.

use crate::dom::Dom;
use auk_term::{Cell, Color, Config, Flags, Terminal};
use auk_ui::NodeId;

/// Scrollback of the boxed terminal: long outputs keep this many lines.
const SCROLLBACK: usize = 100_000;

/// A boxed terminal and the lines drawn from it.
pub(crate) struct AnsiState {
    pub term: Terminal,
    /// The text fed so far.
    pub fed: String,
    pub cols: u16,
    /// Each drawn line's signature and element.
    pub lines: Vec<(u64, NodeId)>,
    pub holder: Option<NodeId>,
}

impl std::fmt::Debug for AnsiState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnsiState")
            .field("cols", &self.cols)
            .field("lines", &self.lines.len())
            .finish()
    }
}

/// `\n` as a pty with `onlcr` would deliver it.
fn onlcr(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 16);
    let mut prev = '\0';
    for c in s.chars() {
        if c == '\n' && prev != '\r' {
            out.push('\r');
        }
        out.push(c);
        prev = c;
    }
    out
}

impl AnsiState {
    pub fn new(cols: u16) -> AnsiState {
        AnsiState {
            term: Self::fresh(cols),
            fed: String::new(),
            cols,
            lines: Vec::new(),
            holder: None,
        }
    }

    fn fresh(cols: u16) -> Terminal {
        let mut t = Terminal::new(
            cols.max(2),
            1,
            Config {
                scrollback: SCROLLBACK,
            },
        );
        t.set_accept_anchor_sequences(false);
        t
    }

    /// Bring the terminal up to `text` at `cols`: append feeds the tail, anything else restarts.
    pub fn set(&mut self, text: &str, cols: u16) {
        if cols != self.cols {
            self.term.resize(cols.max(2), 1);
            self.cols = cols;
        }
        if let Some(tail) = text.strip_prefix(self.fed.as_str()) {
            if !tail.is_empty() {
                self.feed(tail);
                self.fed.push_str(tail);
            }
            return;
        }
        self.term = Self::fresh(self.cols);
        self.feed(text);
        self.fed = text.to_owned();
    }

    fn feed(&mut self, s: &str) {
        let bytes = onlcr(s);
        let _ = self.term.feed(bytes.as_bytes());
        let _ = self.term.take_replies();
    }

    /// The content lines (trailing blank lines dropped).
    pub fn line_count(&self) -> usize {
        let total = self.term.history_len() + self.term.rows() as usize;
        let mut n = total;
        while n > 0 {
            let row = self.term.line(n - 1);
            if row.is_some_and(|r| r.cells.iter().any(|c| !c.text.is_empty() && c.text != " ")) {
                break;
            }
            n -= 1;
        }
        n
    }

    /// Draw lines `range` into `holder`, reusing the elements of lines that didn't change.
    pub fn draw(&mut self, dom: &mut Dom, holder: NodeId, range: std::ops::Range<usize>) {
        if self.holder != Some(holder) {
            self.lines.clear();
            self.holder = Some(holder);
        }
        let links = self.term.links().to_vec();
        let mut want = Vec::with_capacity(range.len());
        let mut old: std::collections::HashMap<u64, Vec<NodeId>> = Default::default();
        for (sig, el) in self.lines.drain(..) {
            old.entry(sig).or_default().push(el);
        }
        for i in range {
            let Some(row) = self.term.line(i) else {
                continue;
            };
            let sig = row_sig(&row.cells);
            let el = match old.get_mut(&sig).and_then(|v| v.pop()) {
                Some(el) => el,
                None => {
                    let el = dom.el("div", "ln");
                    draw_row(dom, el, &row.cells, &links);
                    el
                }
            };
            want.push(el);
            self.lines.push((sig, el));
        }
        for els in old.into_values() {
            for el in els {
                if dom.doc.try_node(el).is_some() {
                    dom.doc.remove(el);
                }
            }
        }
        dom.reconcile(holder, &want);
    }

    /// The plain text.
    #[cfg(test)]
    pub fn text(&self) -> String {
        let n = self.line_count();
        self.term.text(0..n)
    }
}

fn row_sig(cells: &[Cell]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let end = cells
        .iter()
        .rposition(|c| !c.text.is_empty() || c.bg != Color::Default)
        .map_or(0, |i| i + 1);
    for c in &cells[..end] {
        c.text.hash(&mut h);
        c.fg.hash(&mut h);
        c.bg.hash(&mut h);
        c.flags.hash(&mut h);
        c.link.hash(&mut h);
    }
    h.finish()
}

/// A cell color as CSS: the theme's 16 ANSI colors as variables, the rest computed.
pub(crate) fn css_color(c: Color) -> Option<String> {
    match c {
        Color::Default => None,
        Color::Indexed(i) if i < 16 => Some(format!("var(--ansi{i})")),
        Color::Indexed(i) => {
            let (r, g, b) = xterm_rgb(i);
            Some(format!("#{r:02x}{g:02x}{b:02x}"))
        }
        Color::Rgb(rgb) => Some(format!("#{:02x}{:02x}{:02x}", rgb.0, rgb.1, rgb.2)),
    }
}

fn xterm_rgb(i: u8) -> (u8, u8, u8) {
    if i >= 232 {
        let v = 8 + (i - 232) * 10;
        return (v, v, v);
    }
    let i = i - 16;
    let step = |n: u8| if n == 0 { 0 } else { 55 + n * 40 };
    (step(i / 36), step((i / 6) % 6), step(i % 6))
}

#[derive(PartialEq, Clone)]
struct Attr {
    fg: Color,
    bg: Color,
    flags: u16,
    link: u32,
}

fn draw_row(dom: &mut Dom, el: NodeId, cells: &[Cell], links: &[String]) {
    let end = cells
        .iter()
        .rposition(|c| !c.text.is_empty() || c.bg != Color::Default)
        .map_or(0, |i| i + 1);
    let mut run = String::new();
    let mut attr: Option<Attr> = None;
    let flush = |dom: &mut Dom, run: &mut String, attr: &Option<Attr>| {
        if run.is_empty() {
            return;
        }
        let a = attr.as_ref().unwrap();
        let plain = a.fg == Color::Default && a.bg == Color::Default && a.flags == 0 && a.link == 0;
        if plain {
            dom.text(el, run);
        } else {
            let f = Flags(a.flags);
            let mut class = Vec::new();
            for (bit, c) in [
                (Flags::BOLD, "b"),
                (Flags::DIM, "d"),
                (Flags::ITALIC, "i"),
                (Flags::UNDERLINE, "u"),
                (Flags::DOUBLE_UNDERLINE, "u"),
                (Flags::CURLY_UNDERLINE, "u"),
                (Flags::STRIKE, "s"),
                (Flags::HIDDEN, "h"),
            ] {
                if f.has(bit) && !class.contains(&c) {
                    class.push(c);
                }
            }
            if a.link != 0 {
                class.push("sf-link");
            }
            let s = dom.child(el, "span", &class.join(" "));
            let (mut fg, mut bg) = (css_color(a.fg), css_color(a.bg));
            if f.has(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
                fg = fg.or(Some("var(--tv-bg, var(--page))".into()));
                bg = bg.or(Some("var(--tv-fg, var(--t1))".into()));
            }
            let mut css = String::new();
            if let Some(c) = fg {
                css.push_str(&format!("color: {c}; "));
            }
            if let Some(c) = bg {
                css.push_str(&format!("background-color: {c}; "));
            }
            if !css.is_empty() {
                dom.set_style(s, css.trim_end());
            }
            if a.link != 0
                && let Some(h) = links.get(a.link as usize)
            {
                dom.attr(s, "data-href", h);
                dom.attr(s, "title", &format!("{h}  ⌘-click to open"));
            }
            dom.text(s, run);
        }
        run.clear();
    };
    for c in &cells[..end] {
        if Flags(c.flags.0).has(Flags::SPACER) || Flags(c.flags.0).has(Flags::LEADING_SPACER) {
            continue;
        }
        let a = Attr {
            fg: c.fg,
            bg: c.bg,
            flags: c.flags.0 & !(Flags::WIDE | Flags::SPACER | Flags::LEADING_SPACER),
            link: c.link,
        };
        if attr.as_ref() != Some(&a) {
            flush(dom, &mut run, &attr);
            attr = Some(a);
        }
        if c.text.is_empty() {
            run.push(' ');
        } else {
            run.push_str(&c.text);
        }
    }
    flush(dom, &mut run, &attr);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sgr_cr_backspace_and_reflow() {
        let mut a = AnsiState::new(20);
        a.set("one\n\x1b[31mred\x1b[0m two\nab\rX\nxy\x08Z", 20);
        assert_eq!(a.line_count(), 4);
        assert_eq!(a.term.line(2).unwrap().cells[0].text, "X");
        assert_eq!(a.text(), "one\nred two\nXb\nxZ");
        // Append feeds only the tail.
        a.set("one\n\x1b[31mred\x1b[0m two\nab\rX\nxy\x08Z and more", 20);
        assert!(a.text().ends_with("xZ and more"));
        // Reflow at a narrower width.
        a.set("0123456789abcdefghij", 10);
        assert_eq!(a.line_count(), 2);
        assert_eq!(xterm_rgb(196), (255, 0, 0));
        assert_eq!(
            css_color(Color::Indexed(2)).as_deref(),
            Some("var(--ansi2)")
        );
    }
}
