//! A screen buffer (main or alternate): its rows, cursor, saved cursor and kitty keyboard stack,
//! plus the row and scrolling primitives the terminal is built from.

use std::collections::VecDeque;

use crate::{AnchorKey, Cell, Color, Event, Flags, Row};

/// Events queued while a byte is handled; `Terminal::advance` hands them out one by one.
pub(crate) type Events = VecDeque<Event>;

/// The deepest kitty keyboard flag stack, as in kitty.
pub(crate) const KITTY_STACK_DEPTH: usize = 8;

/// SGR attributes the cells written next get, and their OSC 8 link.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Pen {
    pub fg: Color,
    pub bg: Color,
    pub underline_color: Color,
    /// SGR bits only (no cell-kind bits).
    pub flags: u16,
    pub link: u32,
}

/// The four designatable character sets (G0–G3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Charset {
    #[default]
    Ascii,
    /// DEC Special Character and Line Drawing.
    DecGraphics,
}

impl Charset {
    pub(crate) fn map(self, c: char) -> char {
        match self {
            Charset::Ascii => c,
            Charset::DecGraphics => match c {
                '_' => ' ',
                '`' => '◆',
                'a' => '▒',
                'b' => '\u{2409}',
                'c' => '\u{240c}',
                'd' => '\u{240d}',
                'e' => '\u{240a}',
                'f' => '°',
                'g' => '±',
                'h' => '\u{2424}',
                'i' => '\u{240b}',
                'j' => '┘',
                'k' => '┐',
                'l' => '┌',
                'm' => '└',
                'n' => '┼',
                'o' => '⎺',
                'p' => '⎻',
                'q' => '─',
                'r' => '⎼',
                's' => '⎽',
                't' => '├',
                'u' => '┤',
                'v' => '┴',
                'w' => '┬',
                'x' => '│',
                'y' => '≤',
                'z' => '≥',
                '{' => 'π',
                '|' => '≠',
                '}' => '£',
                '~' => '·',
                _ => c,
            },
        }
    }
}

/// The cursor and what DECSC saves with it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CursorState {
    pub row: usize,
    pub col: usize,
    /// The last column was written and the next printable character wraps first.
    pub pending_wrap: bool,
    pub pen: Pen,
    pub charsets: [Charset; 4],
}

/// A row and how much of it may hold anything.
#[derive(Clone, Debug)]
pub(crate) struct Slot {
    pub row: Row,
    /// Every cell from this column on equals `Cell::default()`. Writes raise it; resetting a row
    /// only has to clear the cells before it, which keeps scrolling from touching (cold) blank
    /// cells.
    pub occ: usize,
}

impl Slot {
    pub(crate) fn blank(cols: usize) -> Slot {
        Slot { row: blank_row(cols), occ: 0 }
    }

    /// Notes that cells before `end` may have been written.
    #[inline]
    pub(crate) fn touch(&mut self, end: usize) {
        self.occ = self.occ.max(end);
    }

    /// Notes that any cell may have been written.
    pub(crate) fn touch_all(&mut self) {
        self.occ = self.row.cells.len();
    }
}

impl std::ops::Deref for Slot {
    type Target = Row;

    fn deref(&self) -> &Row {
        &self.row
    }
}

impl std::ops::DerefMut for Slot {
    fn deref_mut(&mut self) -> &mut Row {
        &mut self.row
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Screen {
    /// Scrollback (oldest first), then the visible rows.
    pub buf: VecDeque<Slot>,
    pub history: usize,
    pub cursor: CursorState,
    pub saved: CursorState,
    /// The kitty keyboard flag stack (top last).
    pub kitty: Vec<u8>,
}

/// A blank row of `cols` default cells.
pub(crate) fn blank_row(cols: usize) -> Row {
    Row { cells: vec![Cell::default(); cols], wrapped: false, anchor: None }
}

/// Clears a cell to a blank with background `bg`, keeping its text allocation.
#[inline]
pub(crate) fn clear_cell(cell: &mut Cell, bg: Color) {
    cell.text.clear();
    cell.fg = Color::Default;
    cell.bg = bg;
    cell.underline_color = Color::Default;
    cell.flags = Flags(0);
    cell.link = 0;
}

/// Ends the row's anchor, if it has one.
pub(crate) fn end_anchor(row: &mut Row, events: &mut Events) {
    if let Some(key) = row.anchor.take() {
        events.push_back(Event::AnchorGone(key));
    }
}

/// Blanks a whole row (ending its anchor): the cells get background `bg`.
pub(crate) fn reset_row(slot: &mut Slot, bg: Color, events: &mut Events) {
    end_anchor(&mut slot.row, events);
    let end = if bg == Color::Default { slot.occ } else { slot.row.cells.len() };
    for cell in &mut slot.row.cells[..end] {
        clear_cell(cell, bg);
    }
    slot.occ = if bg == Color::Default { 0 } else { end };
    slot.row.wrapped = false;
}

/// Flags that make a blank cell visible (as `alacritty_terminal` counts them).
const VISIBLE_FLAGS: u16 = Flags::INVERSE
    | Flags::UNDERLINE
    | Flags::DOUBLE_UNDERLINE
    | Flags::CURLY_UNDERLINE
    | Flags::DOTTED_UNDERLINE
    | Flags::DASHED_UNDERLINE
    | Flags::STRIKE
    | Flags::SPACER
    | Flags::LEADING_SPACER;

/// Whether a cell shows nothing at all.
#[inline]
pub(crate) fn cell_is_empty(cell: &Cell) -> bool {
    cell.text.is_empty()
        && cell.bg == Color::Default
        && cell.fg == Color::Default
        && cell.flags.0 & VISIBLE_FLAGS == 0
}

/// Whether a row shows nothing and doesn't continue onto the next one.
pub(crate) fn row_is_clear(row: &Row) -> bool {
    !row.wrapped && row.cells.iter().all(cell_is_empty)
}

/// Restores wide-character pairing in a row after cells moved or were erased: a `WIDE` cell
/// without its `SPACER`, or a `SPACER` without its `WIDE` cell, becomes a blank with the same
/// attributes, and a `LEADING_SPACER` is only kept in the last column of a wrapped row.
pub(crate) fn fix_wide(row: &mut Row) {
    const KIND: u16 = Flags::WIDE | Flags::SPACER | Flags::LEADING_SPACER;
    let n = row.cells.len();
    if !row.cells.iter().any(|c| c.flags.0 & KIND != 0) {
        return;
    }
    for i in 0..n {
        let f = row.cells[i].flags.0;
        if f & Flags::WIDE != 0 {
            let paired = i + 1 < n && row.cells[i + 1].flags.0 & Flags::SPACER != 0;
            if !paired {
                orphan(&mut row.cells[i]);
            }
        } else if f & Flags::SPACER != 0 {
            let paired = i > 0 && row.cells[i - 1].flags.0 & Flags::WIDE != 0;
            if !paired {
                orphan(&mut row.cells[i]);
            }
        } else if f & Flags::LEADING_SPACER != 0 && (i + 1 != n || !row.wrapped) {
            row.cells[i].flags.0 &= !Flags::LEADING_SPACER;
        }
    }
}

fn orphan(cell: &mut Cell) {
    cell.text.clear();
    cell.flags.0 &= !(Flags::WIDE | Flags::SPACER);
}

impl Screen {
    pub(crate) fn new(cols: usize, rows: usize) -> Screen {
        Screen {
            buf: (0..rows).map(|_| Slot::blank(cols)).collect(),
            history: 0,
            cursor: CursorState::default(),
            saved: CursorState::default(),
            kitty: Vec::new(),
        }
    }

    pub(crate) fn rows(&self) -> usize {
        self.buf.len() - self.history
    }

    #[inline]
    pub(crate) fn row(&self, r: usize) -> &Row {
        &self.buf[self.history + r]
    }

    /// A visible row's slot. Writers must `touch` what they write.
    #[inline]
    pub(crate) fn row_mut(&mut self, r: usize) -> &mut Slot {
        &mut self.buf[self.history + r]
    }

    /// The slot of the row the cursor is on.
    #[inline]
    pub(crate) fn cursor_row_mut(&mut self) -> &mut Slot {
        let r = self.cursor.row;
        self.row_mut(r)
    }

    /// The top of the kitty keyboard stack, 0 when empty.
    pub(crate) fn kitty_flags(&self) -> u8 {
        self.kitty.last().copied().unwrap_or(0)
    }

    /// Visible rows `top..bot` move up by `n` (`alacritty_terminal`'s `Grid::scroll_up`). With
    /// `top == 0` the rows leaving the top go into the scrollback (bounded by `limit`, the oldest
    /// rows leaving it); otherwise they are discarded. New rows are blank with background `bg`.
    pub(crate) fn scroll_up(
        &mut self,
        top: usize,
        bot: usize,
        n: usize,
        bg: Color,
        limit: usize,
        events: &mut Events,
    ) {
        if n == 0 || bot <= top {
            return;
        }
        let cols = self.buf[self.history].cells.len();
        if bot - top <= n && top != 0 {
            for r in top..bot {
                reset_row(self.row_mut(r), bg, events);
            }
            return;
        }
        if top == 0 {
            for _ in 0..n {
                self.history += 1;
                let mut row = if self.history > limit {
                    self.history -= 1;
                    let mut row = self.buf.pop_front().expect("rows exist");
                    end_anchor(&mut row, events);
                    row
                } else {
                    Slot::blank(cols)
                };
                reset_row(&mut row, bg, events);
                let index = self.history + bot - 1;
                if index == self.buf.len() {
                    self.buf.push_back(row);
                } else {
                    self.buf.insert(index, row);
                }
            }
        } else {
            let h = self.history;
            for i in top..bot - n {
                self.buf.swap(h + i, h + i + n);
            }
            for r in bot - n..bot {
                reset_row(self.row_mut(r), bg, events);
            }
        }
    }

    /// Visible rows `top..bot` move down by `n`; rows pushed past `bot` are discarded and new
    /// rows at the top are blank with background `bg` (`Grid::scroll_down`).
    pub(crate) fn scroll_down(&mut self, top: usize, bot: usize, n: usize, bg: Color, events: &mut Events) {
        if n == 0 || bot <= top {
            return;
        }
        if bot - top <= n {
            for r in top..bot {
                reset_row(self.row_mut(r), bg, events);
            }
            return;
        }
        let h = self.history;
        for i in (top + n..bot).rev() {
            self.buf.swap(h + i, h + i - n);
        }
        for r in top..top + n {
            reset_row(self.row_mut(r), bg, events);
        }
    }

    /// Drops scrollback rows beyond `limit`, oldest first.
    pub(crate) fn trim_history(&mut self, limit: usize, events: &mut Events) {
        while self.history > limit {
            let mut row = self.buf.pop_front().expect("rows exist");
            end_anchor(&mut row, events);
            self.history -= 1;
        }
    }

    /// Removes all scrollback.
    pub(crate) fn clear_history(&mut self, events: &mut Events) {
        self.trim_history(0, events);
    }

    /// The buffer line of the row anchored with `key`.
    pub(crate) fn find_anchor(&self, key: AnchorKey) -> Option<usize> {
        self.buf.iter().position(|row| row.anchor == Some(key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row_of(cells: &[(&str, u16)], wrapped: bool) -> Row {
        Row {
            cells: cells
                .iter()
                .map(|&(text, flags)| Cell { text: text.into(), flags: Flags(flags), ..Cell::default() })
                .collect(),
            wrapped,
            anchor: None,
        }
    }

    fn kinds(row: &Row) -> Vec<(String, u16)> {
        row.cells.iter().map(|c| (c.text.clone(), c.flags.0)).collect()
    }

    #[test]
    fn fix_wide_blanks_unpaired_halves() {
        let (w, s, l) = (Flags::WIDE, Flags::SPACER, Flags::LEADING_SPACER);
        let mut row = row_of(&[("", s), ("中", w), ("a", 0), ("中", w), ("", s), ("中", w)], false);
        fix_wide(&mut row);
        assert_eq!(
            kinds(&row),
            vec![("".into(), 0), ("".into(), 0), ("a".into(), 0), ("中".into(), w), ("".into(), s), ("".into(), 0)]
        );
        // A leading spacer stays only in the last column of a wrapped row.
        let mut row = row_of(&[("", l), ("", l)], true);
        fix_wide(&mut row);
        assert_eq!(kinds(&row), vec![("".into(), 0), ("".into(), l)]);
        let mut row = row_of(&[("", 0), ("", l)], false);
        fix_wide(&mut row);
        assert_eq!(kinds(&row), vec![("".into(), 0), ("".into(), 0)]);
    }

    #[test]
    fn scrolling_into_a_full_scrollback_drops_the_oldest_row() {
        let mut s = Screen::new(3, 2);
        let mut events = Events::new();
        s.row_mut(0).anchor = Some(AnchorKey(7));
        s.row_mut(0).cells[0].text.push('x');
        s.row_mut(0).touch(1);
        s.scroll_up(0, 2, 1, Color::Default, 1, &mut events);
        assert_eq!((s.history, s.buf.len()), (1, 3));
        assert!(events.is_empty());
        s.scroll_up(0, 2, 1, Color::Default, 1, &mut events);
        assert_eq!((s.history, s.buf.len()), (1, 3));
        assert_eq!(events.pop_front(), Some(Event::AnchorGone(AnchorKey(7))));
        // The recycled row came back blank.
        assert!(s.buf.iter().all(|slot| slot.cells.iter().all(|c| *c == Cell::default())));
    }

    #[test]
    fn scrolling_inside_a_region_discards_rows() {
        let mut s = Screen::new(2, 4);
        let mut events = Events::new();
        for r in 0..4 {
            s.row_mut(r).cells[0].text.push(char::from(b'a' + r as u8));
            s.row_mut(r).touch(1);
        }
        s.row_mut(1).anchor = Some(AnchorKey(1));
        s.scroll_up(1, 3, 1, Color::Indexed(4), 10, &mut events);
        let text: Vec<&str> = (0..4).map(|r| s.row(r).cells[0].text.as_str()).collect();
        assert_eq!(text, ["a", "c", "", "d"]);
        assert_eq!(s.row(2).cells[1].bg, Color::Indexed(4));
        assert_eq!(s.history, 0);
        assert_eq!(events.pop_front(), Some(Event::AnchorGone(AnchorKey(1))));
        s.scroll_down(0, 4, 2, Color::Default, &mut events);
        let text: Vec<&str> = (0..4).map(|r| s.row(r).cells[0].text.as_str()).collect();
        assert_eq!(text, ["", "", "a", "c"]);
    }

    #[test]
    fn reset_clears_only_what_was_written_unless_colored() {
        let mut slot = Slot::blank(4);
        slot.cells[1].text.push('x');
        slot.touch(2);
        let mut events = Events::new();
        reset_row(&mut slot, Color::Default, &mut events);
        assert_eq!(slot.occ, 0);
        assert!(slot.cells.iter().all(|c| *c == Cell::default()));
        reset_row(&mut slot, Color::Indexed(1), &mut events);
        assert_eq!(slot.occ, 4);
        assert!(slot.cells.iter().all(|c| c.bg == Color::Indexed(1)));
    }
}
