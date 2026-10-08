//! Text selection in a pane (§6.2): by cell in buffer coordinates, extended by word or line on
//! double and triple clicks, and the selected text for the clipboard.

use auk_term::{Flags, Line, Terminal};

/// How a selection grows as the mouse drags.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    Cell,
    Word,
    Line,
}

/// A selection: where it started and where the pointer is, in (buffer line, column).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: (Line, u16),
    pub head: (Line, u16),
    pub unit: Unit,
    /// Option-drag: a rectangle.
    pub block: bool,
}

impl Selection {
    /// The selected range, start before end, both inclusive, widened to the unit.
    pub fn range(&self, term: &Terminal) -> ((Line, u16), (Line, u16)) {
        let (start, end) = if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        };
        let last_col = term.cols().saturating_sub(1);
        match self.unit {
            Unit::Cell => (start, end),
            Unit::Word => (
                (start.0, word_bounds(term, start.0, start.1).0),
                (end.0, word_bounds(term, end.0, end.1).1),
            ),
            Unit::Line => ((start.0, 0), (end.0, last_col)),
        }
    }

    /// The renderer's view of the selection.
    pub fn for_render(&self, term: &Terminal) -> auk_render::Selection {
        let (start, end) = self.range(term);
        auk_render::Selection {
            anchor: start,
            head: end,
            block: self.block,
        }
    }

    /// The selected text: rows joined by newlines, except where a row soft-wraps into the next;
    /// trailing blanks of each row dropped.
    pub fn text(&self, term: &Terminal) -> String {
        let (start, end) = self.range(term);
        let mut out = String::new();
        for line in start.0..=end.0 {
            let Some(row) = term.line(line) else { break };
            let (c0, c1) = if self.block {
                (
                    self.anchor.1.min(self.head.1),
                    self.anchor.1.max(self.head.1),
                )
            } else {
                (
                    if line == start.0 { start.1 } else { 0 },
                    if line == end.0 { end.1 } else { u16::MAX },
                )
            };
            let mut text = String::new();
            for (c, cell) in row.cells.iter().enumerate() {
                let c = c as u16;
                if c < c0 || c > c1 {
                    continue;
                }
                if cell.flags.has(Flags::SPACER) || cell.flags.has(Flags::LEADING_SPACER) {
                    continue;
                }
                if cell.text.is_empty() {
                    text.push(' ');
                } else {
                    text.push_str(&cell.text);
                }
            }
            let joined = !self.block && row.wrapped && line < end.0 && c1 == u16::MAX;
            if joined {
                out.push_str(&text);
            } else {
                out.push_str(text.trim_end_matches(' '));
                if line < end.0 {
                    out.push('\n');
                }
            }
        }
        out
    }
}

/// The columns of the word around (`line`, `col`): a run of word characters, or a run of the
/// same other character.
pub fn word_bounds(term: &Terminal, line: Line, col: u16) -> (u16, u16) {
    let Some(row) = term.line(line) else {
        return (col, col);
    };
    let class = |c: usize| -> u8 {
        let Some(cell) = row.cells.get(c) else {
            return 0;
        };
        let ch = cell.text.chars().next().unwrap_or(' ');
        if ch == ' ' {
            0
        } else if ch.is_alphanumeric() || "_-./~:@%+=".contains(ch) || cell.text.len() > 1 {
            1
        } else {
            2
        }
    };
    let c = usize::from(col).min(row.cells.len().saturating_sub(1));
    let k = class(c);
    let mut lo = c;
    while lo > 0 && class(lo - 1) == k {
        lo -= 1;
    }
    let mut hi = c;
    while hi + 1 < row.cells.len() && class(hi + 1) == k {
        hi += 1;
    }
    (lo as u16, hi as u16)
}

#[cfg(test)]
mod tests {
    use super::*;
    use auk_term::Config;

    fn term(text: &str) -> Terminal {
        let mut t = Terminal::new(20, 4, Config::default());
        t.feed(text.as_bytes());
        t
    }

    #[test]
    fn stream_selection_text() {
        let t = term("hello world\r\nsecond line");
        let s = Selection {
            anchor: (0, 6),
            head: (1, 5),
            unit: Unit::Cell,
            block: false,
        };
        assert_eq!(s.text(&t), "world\nsecond");
        let reversed = Selection {
            anchor: (1, 5),
            head: (0, 6),
            ..s
        };
        assert_eq!(reversed.text(&t), "world\nsecond");
    }

    #[test]
    fn words_and_lines() {
        let t = term("ls /usr/bin; echo");
        let w = Selection {
            anchor: (0, 6),
            head: (0, 6),
            unit: Unit::Word,
            block: false,
        };
        assert_eq!(w.text(&t), "/usr/bin");
        let l = Selection {
            unit: Unit::Line,
            ..w
        };
        assert_eq!(l.text(&t), "ls /usr/bin; echo");
    }

    #[test]
    fn soft_wrapped_rows_join() {
        let t = term("abcdefghijklmnopqrstuvwxyz");
        let s = Selection {
            anchor: (0, 0),
            head: (1, 19),
            unit: Unit::Cell,
            block: false,
        };
        assert_eq!(s.text(&t), "abcdefghijklmnopqrstuvwxyz");
    }

    #[test]
    fn block_selection() {
        let t = term("abcd\r\nefgh");
        let s = Selection {
            anchor: (0, 1),
            head: (1, 2),
            unit: Unit::Cell,
            block: true,
        };
        assert_eq!(s.text(&t), "bc\nfg");
    }
}
