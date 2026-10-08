//! The grid pass's input: a resolved, renderer-side copy of the visible rows.
//!
//! A [`GridSnapshot`] holds final colors and decorations per cell, so the renderer never looks at
//! terminal modes or themes. [`GridSnapshot::from_terminal`] builds one from an
//! `auk_term::Terminal`; tests and benchmarks build synthetic ones with [`GridSnapshot::new`] and
//! [`GridSnapshot::write`].

use std::sync::Arc;

use auk_term::{CursorShape, Flags, Line, Terminal};
use unicode_width::UnicodeWidthStr;

use crate::theme::{Rgba, Theme};

/// A cell's text.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum CellText {
    #[default]
    Empty,
    Char(char),
    /// A base character with combining marks, or an emoji sequence.
    Cluster(Arc<str>),
}

impl CellText {
    pub fn new(text: &str) -> CellText {
        let mut chars = text.chars();
        match (chars.next(), chars.next()) {
            (None, _) => CellText::Empty,
            (Some(c), None) => CellText::Char(c),
            _ => CellText::Cluster(text.into()),
        }
    }

    /// Whether the cell draws no glyph.
    pub fn is_blank(&self) -> bool {
        match self {
            CellText::Empty => true,
            CellText::Char(c) => *c == ' ',
            CellText::Cluster(_) => false,
        }
    }

    /// Writes the text into `buf` and returns it.
    pub fn as_str<'a>(&'a self, buf: &'a mut [u8; 4]) -> &'a str {
        match self {
            CellText::Empty => "",
            CellText::Char(c) => c.encode_utf8(buf),
            CellText::Cluster(s) => s,
        }
    }
}

/// Per-cell attribute bits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Attrs(pub u16);

impl Attrs {
    pub const BOLD: u16 = 1 << 0;
    pub const ITALIC: u16 = 1 << 1;
    pub const UNDERLINE: u16 = 1 << 2;
    pub const DOUBLE_UNDERLINE: u16 = 1 << 3;
    pub const CURLY_UNDERLINE: u16 = 1 << 4;
    pub const DOTTED_UNDERLINE: u16 = 1 << 5;
    pub const DASHED_UNDERLINE: u16 = 1 << 6;
    pub const STRIKE: u16 = 1 << 7;
    pub const OVERLINE: u16 = 1 << 8;
    /// The left half of a wide character: its glyph spans this cell and the next.
    pub const WIDE: u16 = 1 << 9;
    /// The right half of a wide character: draws its background only.
    pub const SPACER: u16 = 1 << 10;

    pub const ANY_UNDERLINE: u16 = Self::UNDERLINE
        | Self::DOUBLE_UNDERLINE
        | Self::CURLY_UNDERLINE
        | Self::DOTTED_UNDERLINE
        | Self::DASHED_UNDERLINE;

    pub fn has(self, bit: u16) -> bool {
        self.0 & bit != 0
    }
}

/// One cell, resolved.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct SnapCell {
    pub text: CellText,
    pub fg: Rgba,
    pub bg: Rgba,
    /// Underline, strikethrough and overline color.
    pub decoration: Rgba,
    pub attrs: Attrs,
}

/// How the cursor is drawn. A focused block cursor is drawn by the cells it covers (their
/// colors are swapped in the snapshot), so it needs no shape of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CursorStyle {
    Block,
    /// The outline of a block: the cursor of an unfocused pane.
    HollowBlock,
    Underline,
    Bar,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SnapCursor {
    pub col: u16,
    pub row: u16,
    /// 2 over a wide character.
    pub width: u16,
    pub style: CursorStyle,
    pub color: Rgba,
}

/// A selection in buffer coordinates (`auk_term::Line`, column), both ends inclusive, in either
/// order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: (Line, u16),
    pub head: (Line, u16),
    /// A rectangular (block) selection instead of a stream of text.
    pub block: bool,
}

impl Selection {
    /// Whether the cell at buffer line `line`, column `col` is selected.
    pub fn contains(&self, line: Line, col: u16) -> bool {
        let (start, end) = if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        };
        if line < start.0 || line > end.0 {
            return false;
        }
        if self.block {
            let (c0, c1) = (
                self.anchor.1.min(self.head.1),
                self.anchor.1.max(self.head.1),
            );
            return (c0..=c1).contains(&col);
        }
        (line > start.0 || col >= start.1) && (line < end.0 || col <= end.1)
    }
}

/// How the window views a terminal.
#[derive(Clone, Debug, PartialEq)]
pub struct View {
    /// Rows scrolled back from the bottom (0 shows the live screen).
    pub scroll: usize,
    pub selection: Option<Selection>,
    /// The pane has keyboard focus: the cursor is solid.
    pub focused: bool,
    /// The cursor's blink phase is on (always true when not blinking).
    pub cursor_on: bool,
}

impl Default for View {
    fn default() -> Self {
        View {
            scroll: 0,
            selection: None,
            focused: true,
            cursor_on: true,
        }
    }
}

/// The visible grid, resolved for drawing.
#[derive(Clone, Debug, PartialEq)]
pub struct GridSnapshot {
    pub cols: u16,
    pub rows: u16,
    /// `rows * cols` cells, row-major.
    pub cells: Vec<SnapCell>,
    pub cursor: Option<SnapCursor>,
    /// The color around the grid (the theme's background).
    pub background: Rgba,
}

impl GridSnapshot {
    /// A blank grid in the theme's colors.
    pub fn new(cols: u16, rows: u16, theme: &Theme) -> GridSnapshot {
        let blank = SnapCell {
            fg: theme.foreground,
            bg: theme.background,
            decoration: theme.foreground,
            ..SnapCell::default()
        };
        GridSnapshot {
            cols,
            rows,
            cells: vec![blank; usize::from(cols) * usize::from(rows)],
            cursor: None,
            background: theme.background,
        }
    }

    pub fn row(&self, row: u16) -> &[SnapCell] {
        let start = usize::from(row) * usize::from(self.cols);
        &self.cells[start..start + usize::from(self.cols)]
    }

    pub fn row_mut(&mut self, row: u16) -> &mut [SnapCell] {
        let start = usize::from(row) * usize::from(self.cols);
        &mut self.cells[start..start + usize::from(self.cols)]
    }

    pub fn cell_mut(&mut self, col: u16, row: u16) -> &mut SnapCell {
        &mut self.cells[usize::from(row) * usize::from(self.cols) + usize::from(col)]
    }

    /// Writes `text` at (`col`, `row`) in the given colors and attributes, one grapheme cluster
    /// per cell (two for wide ones), clipped at the row's end. For synthetic grids.
    pub fn write(&mut self, col: u16, row: u16, text: &str, fg: Rgba, bg: Rgba, attrs: u16) {
        let mut col = col;
        for cluster in clusters(text) {
            let width = cluster.width().clamp(1, 2) as u16;
            if col + width > self.cols {
                break;
            }
            let wide = if width == 2 { Attrs::WIDE } else { 0 };
            *self.cell_mut(col, row) = SnapCell {
                text: CellText::new(cluster),
                fg,
                bg,
                decoration: fg,
                attrs: Attrs(attrs | wide),
            };
            if width == 2 {
                *self.cell_mut(col + 1, row) = SnapCell {
                    fg,
                    bg,
                    decoration: fg,
                    attrs: Attrs(Attrs::SPACER),
                    ..SnapCell::default()
                };
            }
            col += width;
        }
    }

    /// The visible rows of `term` under `view`, in `theme`'s colors (with the program's color
    /// overrides applied).
    pub fn from_terminal(term: &Terminal, theme: &Theme, view: &View) -> GridSnapshot {
        let history = term.history_len();
        let top = history - view.scroll.min(history);
        GridSnapshot::from_lines(term, theme, view, top, term.rows())
    }

    /// `rows` buffer lines of `term` starting at line `first` (lines past the end are blank),
    /// for a mixed layout that shows a different run of lines than the screen. `view.scroll` is
    /// ignored; the selection and cursor apply where they fall.
    pub fn from_lines(
        term: &Terminal,
        theme: &Theme,
        view: &View,
        first: Line,
        rows: u16,
    ) -> GridSnapshot {
        let theme = theme.with_overrides(term.color_overrides());
        let cols = term.cols();
        let mut snap = GridSnapshot::new(cols, rows, &theme);
        for r in 0..rows {
            let line = first + usize::from(r);
            let Some(row) = term.line(line) else { continue };
            for (c, cell) in row.cells.iter().take(usize::from(cols)).enumerate() {
                let col = c as u16;
                let selected = view.selection.is_some_and(|s| s.contains(line, col));
                *snap.cell_mut(col, r) = resolve_cell(cell, &theme, selected);
            }
        }

        let cursor = term.cursor();
        let modes = term.modes();
        let cursor_line = term.history_len() + usize::from(cursor.row);
        if cursor.visible
            && modes.cursor_visible
            && view.cursor_on
            && (first..first + usize::from(rows)).contains(&cursor_line)
        {
            let row = (cursor_line - first) as u16;
            let col = cursor.col.min(cols.saturating_sub(1));
            let width = if snap.row(row)[usize::from(col)].attrs.has(Attrs::WIDE) {
                2
            } else {
                1
            };
            let style = match (view.focused, cursor.shape) {
                (false, _) => CursorStyle::HollowBlock,
                (true, CursorShape::Block) => CursorStyle::Block,
                (true, CursorShape::Underline) => CursorStyle::Underline,
                (true, CursorShape::Bar) => CursorStyle::Bar,
            };
            snap.set_cursor(
                SnapCursor {
                    col,
                    row,
                    width,
                    style,
                    color: theme.cursor,
                },
                &theme,
            );
        }
        snap
    }

    /// Places the cursor. A block cursor recolors the cells it covers.
    pub fn set_cursor(&mut self, cursor: SnapCursor, theme: &Theme) {
        if cursor.style == CursorStyle::Block {
            for dc in 0..cursor.width {
                if cursor.col + dc < self.cols {
                    let cell = self.cell_mut(cursor.col + dc, cursor.row);
                    cell.bg = cursor.color;
                    cell.fg = theme.cursor_text;
                    cell.decoration = theme.cursor_text;
                }
            }
        }
        self.cursor = Some(cursor);
    }
}

fn resolve_cell(cell: &auk_term::Cell, theme: &Theme, selected: bool) -> SnapCell {
    let f = cell.flags;
    let bold = f.has(Flags::BOLD);
    let mut fg = theme.resolve(cell.fg, true, bold);
    let mut bg = theme.resolve(cell.bg, false, false);
    if f.has(Flags::INVERSE) {
        std::mem::swap(&mut fg, &mut bg);
    }
    if f.has(Flags::DIM) {
        fg = fg.mix(bg, 0.4);
    }
    let mut decoration = match cell.underline_color {
        auk_term::Color::Default => fg,
        c => theme.resolve(c, true, false),
    };
    if f.has(Flags::HIDDEN) {
        fg = bg;
        decoration = bg;
    }
    if selected {
        bg = theme.selection_background;
        if let Some(sel_fg) = theme.selection_foreground {
            fg = sel_fg;
            decoration = sel_fg;
        }
    }
    let mut attrs = 0;
    for (from, to) in [
        (Flags::BOLD, Attrs::BOLD),
        (Flags::ITALIC, Attrs::ITALIC),
        (Flags::UNDERLINE, Attrs::UNDERLINE),
        (Flags::DOUBLE_UNDERLINE, Attrs::DOUBLE_UNDERLINE),
        (Flags::CURLY_UNDERLINE, Attrs::CURLY_UNDERLINE),
        (Flags::DOTTED_UNDERLINE, Attrs::DOTTED_UNDERLINE),
        (Flags::DASHED_UNDERLINE, Attrs::DASHED_UNDERLINE),
        (Flags::STRIKE, Attrs::STRIKE),
        (Flags::OVERLINE, Attrs::OVERLINE),
        (Flags::WIDE, Attrs::WIDE),
        (Flags::SPACER, Attrs::SPACER),
    ] {
        if f.has(from) {
            attrs |= to;
        }
    }
    let text = if f.has(Flags::SPACER) || f.has(Flags::HIDDEN) {
        CellText::Empty
    } else {
        CellText::new(&cell.text)
    };
    SnapCell {
        text,
        fg,
        bg,
        decoration,
        attrs: Attrs(attrs),
    }
}

/// Splits text into extended grapheme clusters, approximately: a base character followed by
/// combining marks, variation selectors, ZWJ-joined characters and skin-tone modifiers. Only
/// synthetic grids use this; real grids come from the terminal core's own segmentation.
fn clusters(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = None;
    let mut joined = false;
    for (i, c) in text.char_indices() {
        let extends = joined
            || c == '\u{200D}'
            || ('\u{FE00}'..='\u{FE0F}').contains(&c)
            || ('\u{1F3FB}'..='\u{1F3FF}').contains(&c)
            || ('\u{0300}'..='\u{036F}').contains(&c)
            || c == '\u{20E3}';
        joined = c == '\u{200D}';
        if !extends {
            if let Some(s) = start {
                out.push(&text[s..i]);
            }
            start = Some(i);
        }
    }
    if let Some(s) = start {
        out.push(&text[s..]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clusters_split_like_graphemes() {
        assert_eq!(clusters("ab"), ["a", "b"]);
        assert_eq!(clusters("e\u{301}x"), ["e\u{301}", "x"]);
        assert_eq!(clusters("👩\u{200D}💻!"), ["👩\u{200D}💻", "!"]);
        assert_eq!(clusters("❤\u{FE0F}"), ["❤\u{FE0F}"]);
    }

    #[test]
    fn write_marks_wide_cells() {
        let theme = Theme::dark();
        let mut s = GridSnapshot::new(6, 1, &theme);
        s.write(0, 0, "a漢b", theme.foreground, theme.background, 0);
        let row = s.row(0);
        assert_eq!(row[0].text, CellText::Char('a'));
        assert!(row[1].attrs.has(Attrs::WIDE));
        assert!(row[2].attrs.has(Attrs::SPACER));
        assert_eq!(row[3].text, CellText::Char('b'));
        // Clipped at the end of the row: a wide character never straddles it.
        s.write(5, 0, "漢", theme.foreground, theme.background, 0);
        assert_eq!(s.row(0)[5].text, CellText::Empty);
    }

    #[test]
    fn selection_ranges() {
        let s = Selection {
            anchor: (3, 5),
            head: (1, 2),
            block: false,
        };
        assert!(!s.contains(1, 1));
        assert!(s.contains(1, 2));
        assert!(s.contains(2, 0));
        assert!(s.contains(3, 5));
        assert!(!s.contains(3, 6));
        let b = Selection { block: true, ..s };
        assert!(!b.contains(2, 0));
        assert!(b.contains(2, 3));
    }

    #[test]
    fn cell_attributes_resolve() {
        let theme = Theme::dark();
        let mut cell = auk_term::Cell {
            text: "x".into(),
            fg: auk_term::Color::Indexed(1),
            bg: auk_term::Color::Indexed(4),
            flags: Flags(Flags::INVERSE | Flags::UNDERLINE | Flags::BOLD),
            ..auk_term::Cell::default()
        };
        let s = resolve_cell(&cell, &theme, false);
        assert_eq!((s.fg, s.bg), (theme.palette[4], theme.palette[1]));
        assert!(s.attrs.has(Attrs::UNDERLINE) && s.attrs.has(Attrs::BOLD));
        cell.flags = Flags(Flags::HIDDEN);
        let s = resolve_cell(&cell, &theme, true);
        assert_eq!(s.text, CellText::Empty);
        assert_eq!(s.bg, theme.selection_background);
    }
}
