//! The emulator: everything the parser drives, apart from the parser itself.
//!
//! Semantics follow `alacritty_terminal` 0.26 (the reference grid, §21), with the deliberate
//! differences listed in `tests/differential.rs`.

use std::collections::VecDeque;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::screen::{
    Charset, CursorState, Events, KITTY_STACK_DEPTH, Screen, cell_is_empty, clear_cell,
    end_anchor, fix_wide, reset_row,
};
use crate::{
    AnchorError, AnchorKey, Color, Config, CursorShape, Event, Facts, Flags, Line, MouseMode,
    Rgb,
};

/// Terminal-wide mode bits.
pub(crate) mod mode {
    pub const APP_CURSOR: u32 = 1 << 0;
    pub const APP_KEYPAD: u32 = 1 << 1;
    pub const BRACKETED_PASTE: u32 = 1 << 2;
    pub const FOCUS: u32 = 1 << 3;
    pub const SHOW_CURSOR: u32 = 1 << 4;
    pub const ORIGIN: u32 = 1 << 5;
    pub const AUTOWRAP: u32 = 1 << 6;
    pub const INSERT: u32 = 1 << 7;
    /// LNM.
    pub const NEWLINE: u32 = 1 << 8;
    pub const ALT_SCROLL: u32 = 1 << 9;
    pub const URGENCY: u32 = 1 << 10;
    /// Synchronized output (2026).
    pub const SYNC: u32 = 1 << 11;
    pub const DEFAULT: u32 = SHOW_CURSOR | AUTOWRAP | ALT_SCROLL | URGENCY;
}

/// How mouse reports are encoded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum MouseEncoding {
    #[default]
    Default,
    Utf8,
    Sgr,
}


/// Interned OSC 8 links. A link's identity is its `id` parameter (if any) and its URI.
#[derive(Clone, Debug, Default)]
pub(crate) struct Links {
    /// Index 0 is "no link".
    pub uris: Vec<String>,
    pub ids: Vec<Option<String>>,
    lookup: std::collections::BTreeMap<(Option<String>, String), u32>,
}

impl Links {
    pub(crate) fn new() -> Links {
        Links { uris: vec![String::new()], ids: vec![None], lookup: Default::default() }
    }

    pub(crate) fn intern(&mut self, id: Option<String>, uri: String) -> u32 {
        let key = (id, uri);
        if let Some(&index) = self.lookup.get(&key) {
            return index;
        }
        let index = self.uris.len() as u32;
        self.ids.push(key.0.clone());
        self.uris.push(key.1.clone());
        self.lookup.insert(key, index);
        index
    }

    pub(crate) fn len(&self) -> usize {
        self.uris.len()
    }
}

/// All replicated terminal state except the parser.
#[derive(Debug)]
pub(crate) struct Core {
    pub cols: usize,
    pub rows: usize,
    pub config: Config,
    pub main: Screen,
    pub alt: Screen,
    pub alt_active: bool,
    pub modes: u32,
    pub mouse: MouseMode,
    pub mouse_encoding: MouseEncoding,
    /// The scroll region, `top..bot`.
    pub top: usize,
    pub bot: usize,
    pub tabs: Vec<bool>,
    /// Which of G0–G3 is invoked (SI/SO).
    pub active_charset: usize,
    /// DECSCUSR: the shape and whether it blinks; `None` for the default.
    pub cursor_style: Option<(CursorShape, bool)>,
    pub title: String,
    pub icon: String,
    pub title_stack: Vec<String>,
    pub links: Links,
    /// OSC 4/10/11/12 overrides, sorted by index (256–258 for foreground, background, cursor).
    pub overrides: Vec<(u16, Rgb)>,
    /// The last printed character, for REP.
    pub preceding: Option<char>,
    // Not replicated state:
    pub events: Events,
    pub replies: Vec<u8>,
    pub facts: Facts,
    pub accept_anchor_sequences: bool,
}

pub(crate) fn default_tabs(cols: usize) -> Vec<bool> {
    (0..cols).map(|i| i % 8 == 0).collect()
}

impl Core {
    pub(crate) fn new(cols: usize, rows: usize, config: Config) -> Core {
        Core {
            cols,
            rows,
            config,
            main: Screen::new(cols, rows),
            alt: Screen::new(cols, rows),
            alt_active: false,
            modes: mode::DEFAULT,
            mouse: MouseMode::Off,
            mouse_encoding: MouseEncoding::Default,
            top: 0,
            bot: rows,
            tabs: default_tabs(cols),
            active_charset: 0,
            cursor_style: None,
            title: String::new(),
            icon: String::new(),
            title_stack: Vec::new(),
            links: Links::new(),
            overrides: Vec::new(),
            preceding: None,
            events: VecDeque::new(),
            replies: Vec::new(),
            facts: Facts::default(),
            accept_anchor_sequences: true,
        }
    }

    #[inline]
    pub(crate) fn scr(&mut self) -> &mut Screen {
        if self.alt_active { &mut self.alt } else { &mut self.main }
    }

    #[inline]
    pub(crate) fn screen(&self) -> &Screen {
        if self.alt_active { &self.alt } else { &self.main }
    }

    /// The active screen, its scrollback limit and the event queue, borrowed together.
    #[inline]
    pub(crate) fn parts(&mut self) -> (&mut Screen, usize, &mut Events) {
        if self.alt_active {
            (&mut self.alt, 0, &mut self.events)
        } else {
            (&mut self.main, self.config.scrollback, &mut self.events)
        }
    }

    pub(crate) fn has(&self, bit: u32) -> bool {
        self.modes & bit != 0
    }

    pub(crate) fn set_mode_bit(&mut self, bit: u32, on: bool) {
        if on {
            self.modes |= bit;
        } else {
            self.modes &= !bit;
        }
    }

    pub(crate) fn reply(&mut self, bytes: &[u8]) {
        if self.replies.is_empty() {
            self.events.push_back(Event::Replies);
        }
        self.replies.extend_from_slice(bytes);
    }

    // Printing.

    /// Prints a run of text. Stops after the character during which an event arose.
    pub(crate) fn print_text(&mut self, text: &str) -> usize {
        let bytes = text.as_bytes();
        let fast = !self.has(mode::INSERT);
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            if fast && (0x20..0x7F).contains(&b) {
                let charset = self.screen().cursor.charsets[self.active_charset];
                if charset == Charset::Ascii {
                    let end = i + bytes[i..]
                        .iter()
                        .position(|&b| !(0x20..0x7F).contains(&b))
                        .unwrap_or(bytes.len() - i);
                    let done = self.print_ascii_run(&bytes[i..end]);
                    if done < end - i || !self.events.is_empty() {
                        return i + done;
                    }
                    i = end;
                    continue;
                }
            }
            let c = text[i..].chars().next().expect("char boundary");
            self.print_char(c);
            i += c.len_utf8();
            if !self.events.is_empty() {
                return i;
            }
        }
        i
    }

    /// The fast path: printable ASCII with the ASCII charset and insert mode off. Returns the
    /// bytes printed; fewer than all only when an event arose.
    pub(crate) fn print_ascii_run(&mut self, run: &[u8]) -> usize {
        let cols = self.cols;
        let autowrap = self.has(mode::AUTOWRAP);
        let mut i = 0;
        while i < run.len() {
            let (pending, col) = (self.screen().cursor.pending_wrap, self.screen().cursor.col);
            if pending {
                if autowrap {
                    self.wrapline();
                    if !self.events.is_empty() {
                        // The event arose while wrapping for this character: print it, then stop.
                        self.write_ascii(&run[i..=i]);
                        self.preceding = Some(run[i] as char);
                        return i + 1;
                    }
                } else if col + 1 == cols {
                    // Without autowrap every character lands in the last column; the last stays.
                    let last = run.len() - 1;
                    self.write_ascii(&run[last..]);
                    self.preceding = Some(run[last] as char);
                    return run.len();
                }
            }
            let col = self.screen().cursor.col;
            let n = (cols - col).min(run.len() - i);
            self.write_ascii(&run[i..i + n]);
            i += n;
        }
        self.preceding = Some(run[run.len() - 1] as char);
        run.len()
    }

    /// Writes ASCII characters from the cursor onward, within the row, advancing the cursor.
    #[inline]
    pub(crate) fn write_ascii(&mut self, chunk: &[u8]) {
        let cols = self.cols;
        let s = self.scr();
        let start = s.cursor.col;
        let r = s.cursor.row;
        let row_index = s.history + r;
        // Overwriting part of a wide character first unpairs it.
        let kinds = s.buf[row_index].cells[start..start + chunk.len()]
            .iter()
            .any(|c| c.flags.0 & (Flags::WIDE | Flags::SPACER) != 0);
        if kinds {
            for k in 0..chunk.len() {
                unpair_for_write(s, row_index, start + k, cols);
            }
        }
        let pen = &s.cursor.pen;
        let (fg, bg, ul, flags, link) = (pen.fg, pen.bg, pen.underline_color, pen.flags, pen.link);
        let row = &mut s.buf[row_index];
        row.touch(start + chunk.len());
        for (cell, &b) in row.cells[start..start + chunk.len()].iter_mut().zip(chunk) {
            cell.text.clear();
            if b != b' ' {
                cell.text.push(b as char);
            }
            cell.fg = fg;
            cell.bg = bg;
            cell.underline_color = ul;
            cell.flags = Flags(flags);
            cell.link = link;
        }
        let end = start + chunk.len();
        if end == cols {
            row.wrapped = false;
            s.cursor.col = cols - 1;
            s.cursor.pending_wrap = true;
        } else {
            s.cursor.col = end;
        }
    }

    /// One character (`alacritty_terminal`'s `Term::input`).
    pub(crate) fn print_char(&mut self, c: char) {
        self.preceding = Some(c);
        let Some(width) = c.width() else { return };
        if width == 0 {
            self.print_zero_width(c);
            return;
        }
        if self.screen().cursor.pending_wrap {
            self.wrapline();
        }
        let cols = self.cols;
        let insert = self.has(mode::INSERT) && self.screen().cursor.col + width < cols;
        if insert {
            let s = self.scr();
            let col = s.cursor.col;
            let row = s.cursor_row_mut();
            // The same swaps as `alacritty_terminal`, so the cells landing under the cursor (and
            // the wide-character fixups they trigger) are the same.
            for c in (col..cols - width).rev() {
                row.cells.swap(c + width, c);
            }
            row.touch_all();
            row.wrapped = false;
        }
        if width == 1 {
            self.write_at_cursor(c, 0);
        } else {
            if self.screen().cursor.col + 1 >= cols {
                if self.has(mode::AUTOWRAP) {
                    self.write_at_cursor(' ', Flags::LEADING_SPACER);
                    self.wrapline();
                } else {
                    self.scr().cursor.pending_wrap = true;
                    return;
                }
            }
            self.write_at_cursor(c, Flags::WIDE);
            self.scr().cursor.col += 1;
            self.write_at_cursor(' ', Flags::SPACER);
        }
        let s = self.scr();
        if s.cursor.col + 1 < cols {
            s.cursor.col += 1;
        } else {
            s.cursor.pending_wrap = true;
        }
        if insert {
            fix_wide(self.scr().cursor_row_mut());
        }
    }

    /// A combining mark or other zero-width character joins the previous cell.
    pub(crate) fn print_zero_width(&mut self, c: char) {
        let cols = self.cols;
        let s = self.scr();
        let mut col = s.cursor.col;
        if !s.cursor.pending_wrap {
            col = col.saturating_sub(1);
        }
        let r = s.cursor.row;
        if s.row(r).cells[col].flags.has(Flags::SPACER) {
            col = col.saturating_sub(1);
        }
        let slot = s.row_mut(r);
        slot.touch(col + 1);
        let cell = &mut slot.cells[col];
        if cell.text.is_empty() {
            cell.text.push(' ');
        }
        cell.text.push(c);
        // Emoji presentation (VS16) widens a narrow character that has an emoji form.
        let widen = c == '\u{FE0F}'
            && !cell.flags.has(Flags::WIDE)
            && col + 1 < cols
            && cell.text.chars().next().and_then(|b| b.width()) == Some(1)
            && cell.text.width() == 2;
        if widen {
            self.widen(r, col);
        }
    }

    /// Turns the narrow cell at `col` into a wide one, taking the next cell as its spacer.
    pub(crate) fn widen(&mut self, r: usize, col: usize) {
        let cols = self.cols;
        let s = self.scr();
        let row_index = s.history + r;
        unpair_for_write(s, row_index, col + 1, cols);
        let row = &mut s.buf[row_index];
        let (fg, bg, ul, flags, link) = {
            let cell = &row.cells[col];
            (cell.fg, cell.bg, cell.underline_color, cell.flags.0, cell.link)
        };
        row.touch(col + 2);
        row.cells[col].flags.0 |= Flags::WIDE;
        let spacer = &mut row.cells[col + 1];
        spacer.text.clear();
        spacer.fg = fg;
        spacer.bg = bg;
        spacer.underline_color = ul;
        spacer.flags = Flags(flags | Flags::SPACER);
        spacer.link = link;
        if col + 2 == cols {
            row.wrapped = false;
        }
        if s.cursor.row == r && s.cursor.col == col + 1 && !s.cursor.pending_wrap {
            if col + 2 < cols {
                s.cursor.col = col + 2;
            } else {
                s.cursor.pending_wrap = true;
            }
        }
    }

    /// Writes `c` (through the active charset) at the cursor with the pen, plus `kind` bits.
    pub(crate) fn write_at_cursor(&mut self, c: char, kind: u16) {
        let cols = self.cols;
        let charset = self.screen().cursor.charsets[self.active_charset];
        let c = charset.map(c);
        let s = self.scr();
        let col = s.cursor.col;
        let row_index = s.history + s.cursor.row;
        unpair_for_write(s, row_index, col, cols);
        let pen = s.cursor.pen.clone();
        let row = &mut s.buf[row_index];
        row.touch(col + 1);
        let cell = &mut row.cells[col];
        cell.text.clear();
        if c != ' ' {
            cell.text.push(c);
        }
        cell.fg = pen.fg;
        cell.bg = pen.bg;
        cell.underline_color = pen.underline_color;
        cell.flags = Flags(pen.flags | kind);
        cell.link = pen.link;
        if col + 1 == cols {
            row.wrapped = false;
        }
    }

    // Cursor motion and line control.

    pub(crate) fn wrapline(&mut self) {
        if !self.has(mode::AUTOWRAP) {
            return;
        }
        let cols = self.cols;
        let bot = self.bot;
        let s = self.scr();
        if s.cursor.col + 1 == cols {
            s.cursor_row_mut().wrapped = true;
        }
        if s.cursor.row + 1 >= bot {
            self.linefeed();
        } else {
            s.cursor.row += 1;
        }
        let s = self.scr();
        s.cursor.col = 0;
        s.cursor.pending_wrap = false;
    }

    pub(crate) fn linefeed(&mut self) {
        let next = self.screen().cursor.row + 1;
        if next == self.bot {
            self.scroll_up_relative(self.top, 1);
        } else if next < self.rows {
            self.scr().cursor.row = next;
        }
    }

    pub(crate) fn carriage_return(&mut self) {
        let s = self.scr();
        s.cursor.col = 0;
        s.cursor.pending_wrap = false;
    }

    pub(crate) fn backspace(&mut self) {
        let s = self.scr();
        if s.cursor.col > 0 {
            s.cursor.col -= 1;
            s.cursor.pending_wrap = false;
        }
    }

    pub(crate) fn put_tab(&mut self, mut count: u16) {
        if self.screen().cursor.pending_wrap {
            self.wrapline();
            return;
        }
        let cols = self.cols;
        while self.screen().cursor.col < cols && count != 0 {
            count -= 1;
            loop {
                let col = self.screen().cursor.col;
                if col + 1 == cols {
                    break;
                }
                self.scr().cursor.col = col + 1;
                if self.tabs[col + 1] {
                    break;
                }
            }
        }
    }

    pub(crate) fn reverse_index(&mut self) {
        let row = self.screen().cursor.row;
        if row == self.top {
            self.scroll_down_relative(self.top, 1);
        } else {
            self.scr().cursor.row = row.saturating_sub(1);
        }
    }

    /// CUP: to `line`, `col` (0-based), relative to the scroll region in origin mode.
    pub(crate) fn goto(&mut self, line: i64, col: usize) {
        let (offset, max) = if self.has(mode::ORIGIN) {
            (self.top as i64, self.bot as i64 - 1)
        } else {
            (0, self.rows as i64 - 1)
        };
        let row = (line + offset).min(max).max(0) as usize;
        let col = col.min(self.cols - 1);
        let s = self.scr();
        s.cursor.row = row;
        s.cursor.col = col;
        s.cursor.pending_wrap = false;
    }

    pub(crate) fn goto_line(&mut self, line: i64) {
        let col = self.screen().cursor.col;
        self.goto(line, col);
    }

    pub(crate) fn goto_col(&mut self, col: usize) {
        let row = self.screen().cursor.row as i64;
        let line = if self.has(mode::ORIGIN) { row - self.top as i64 } else { row };
        self.goto(line, col);
    }

    /// CUU: up, stopping at the top margin when starting at or below it.
    pub(crate) fn move_up(&mut self, n: usize) {
        let top = self.top;
        let s = self.scr();
        let floor = if s.cursor.row >= top { top } else { 0 };
        s.cursor.row = s.cursor.row.saturating_sub(n).max(floor);
        s.cursor.pending_wrap = false;
    }

    /// CUD: down, stopping at the bottom margin when starting at or above it.
    pub(crate) fn move_down(&mut self, n: usize) {
        let ceiling = if self.screen().cursor.row < self.bot { self.bot - 1 } else { self.rows - 1 };
        let s = self.scr();
        s.cursor.row = s.cursor.row.saturating_add(n).min(ceiling);
        s.cursor.pending_wrap = false;
    }

    pub(crate) fn move_forward(&mut self, n: usize) {
        let cols = self.cols;
        let s = self.scr();
        s.cursor.col = s.cursor.col.saturating_add(n).min(cols - 1);
        s.cursor.pending_wrap = false;
    }

    pub(crate) fn move_backward(&mut self, n: usize) {
        let s = self.scr();
        s.cursor.col = s.cursor.col.saturating_sub(n);
        s.cursor.pending_wrap = false;
    }

    pub(crate) fn move_forward_tabs(&mut self, count: u16) {
        let cols = self.cols;
        for _ in 0..count {
            let mut col = self.screen().cursor.col;
            if col == cols - 1 {
                break;
            }
            for i in col + 1..cols {
                col = i;
                if self.tabs[i] {
                    break;
                }
            }
            self.scr().cursor.col = col;
        }
    }

    pub(crate) fn move_backward_tabs(&mut self, count: u16) {
        for _ in 0..count {
            let mut col = self.screen().cursor.col;
            if col == 0 {
                break;
            }
            for i in (0..col).rev() {
                if self.tabs[i] {
                    col = i;
                    break;
                }
            }
            let s = self.scr();
            s.cursor.col = col;
            s.cursor.pending_wrap = false;
        }
    }

    // Scrolling.

    pub(crate) fn scroll_up_relative(&mut self, origin: usize, n: usize) {
        let n = n.min(self.bot.saturating_sub(self.top));
        let bot = self.bot;
        let (s, limit, events) = self.parts();
        let bg = s.cursor.pen.bg;
        s.scroll_up(origin, bot, n, bg, limit, events);
    }

    pub(crate) fn scroll_down_relative(&mut self, origin: usize, n: usize) {
        let n = n.min(self.bot.saturating_sub(self.top)).min(self.bot.saturating_sub(origin));
        let bot = self.bot;
        let (s, _, events) = self.parts();
        let bg = s.cursor.pen.bg;
        s.scroll_down(origin, bot, n, bg, events);
    }

    pub(crate) fn insert_lines(&mut self, n: usize) {
        let row = self.screen().cursor.row;
        if (self.top..self.bot).contains(&row) {
            self.scroll_down_relative(row, n);
        }
    }

    pub(crate) fn delete_lines(&mut self, n: usize) {
        let row = self.screen().cursor.row;
        let n = n.min(self.rows - row);
        if n > 0 && (self.top..self.bot).contains(&row) {
            self.scroll_up_relative(row, n);
        }
    }

    pub(crate) fn set_scrolling_region(&mut self, top: usize, bottom: Option<usize>) {
        let bottom = bottom.unwrap_or(self.rows);
        if top >= bottom {
            return;
        }
        self.top = (top - 1).min(self.rows);
        self.bot = bottom.min(self.rows);
        self.goto(0, 0);
    }

    // Erasing.

    /// Clears cells `from..to` of the cursor row with the pen's background.
    pub(crate) fn erase_in_row(&mut self, from: usize, to: usize) {
        let cols = self.cols;
        let (s, _, events) = self.parts();
        let bg = s.cursor.pen.bg;
        let row = s.cursor_row_mut();
        if from >= to {
            return;
        }
        end_anchor(row, events);
        if bg != Color::Default {
            row.touch(to);
        }
        for cell in &mut row.cells[from..to] {
            clear_cell(cell, bg);
        }
        if to == cols {
            row.wrapped = false;
        }
        fix_wide(row);
    }

    pub(crate) fn insert_blank(&mut self, count: usize) {
        let cols = self.cols;
        let s = self.scr();
        let bg = s.cursor.pen.bg;
        let col = s.cursor.col;
        let count = count.min(cols - col);
        let row = s.cursor_row_mut();
        row.touch_all();
        row.cells[col..].rotate_right(count);
        for cell in &mut row.cells[col..col + count] {
            clear_cell(cell, bg);
        }
        row.wrapped = false;
        fix_wide(row);
    }

    pub(crate) fn delete_chars(&mut self, count: usize) {
        let cols = self.cols;
        let s = self.scr();
        let bg = s.cursor.pen.bg;
        let col = s.cursor.col;
        let count = count.min(cols - col);
        let row = s.cursor_row_mut();
        row.touch_all();
        row.cells[col..].rotate_left(count);
        for cell in &mut row.cells[cols - count..] {
            clear_cell(cell, bg);
        }
        row.wrapped = false;
        fix_wide(row);
    }

    pub(crate) fn erase_chars(&mut self, count: usize) {
        let col = self.screen().cursor.col;
        let end = col.saturating_add(count).min(self.cols);
        self.erase_in_row(col, end);
    }

    pub(crate) fn clear_line(&mut self, mode: u16) {
        let cursor = self.screen().cursor.clone();
        match mode {
            0 if cursor.pending_wrap => {}
            0 => self.erase_in_row(cursor.col, self.cols),
            1 => self.erase_in_row(0, (cursor.col + 1).min(self.cols)),
            2 => self.erase_in_row(0, self.cols),
            _ => {}
        }
    }

    pub(crate) fn clear_screen(&mut self, mode: u16) {
        let cursor = self.screen().cursor.clone();
        let rows = self.rows;
        match mode {
            0 => {
                self.erase_in_row(cursor.col, self.cols);
                let (s, _, events) = self.parts();
                let bg = s.cursor.pen.bg;
                for r in cursor.row + 1..rows {
                    reset_row(s.row_mut(r), bg, events);
                }
            }
            1 => {
                {
                    let (s, _, events) = self.parts();
                    let bg = s.cursor.pen.bg;
                    for r in 0..cursor.row {
                        reset_row(s.row_mut(r), bg, events);
                    }
                }
                self.erase_in_row(0, (cursor.col + 1).min(self.cols));
            }
            2 => {
                if self.alt_active {
                    let (s, _, events) = self.parts();
                    let bg = s.cursor.pen.bg;
                    for r in 0..rows {
                        reset_row(s.row_mut(r), bg, events);
                    }
                } else {
                    self.clear_viewport();
                }
                self.events.push_back(Event::ScreenCleared);
            }
            3 => {
                let (s, _, events) = self.parts();
                s.clear_history(events);
                self.events.push_back(Event::ScreenCleared);
            }
            _ => {}
        }
    }

    /// ED 2 on the main screen: rows up to the last one with content go into the scrollback,
    /// then the screen is blank (as `alacritty_terminal` does). Anchor rows on screen end.
    pub(crate) fn clear_viewport(&mut self) {
        let rows = self.rows;
        let (s, limit, events) = self.parts();
        for r in 0..rows {
            end_anchor(s.row_mut(r), events);
        }
        let last = (0..rows).rev().find(|&r| {
            let row = s.row(r);
            row.wrapped || row.cells.iter().any(|c| !cell_is_empty(c))
        });
        let positions = match last {
            Some(r) => r + 1,
            // `alacritty_terminal` scans into the scrollback; with none, it stops on row 0.
            None if s.history > 0 => 0,
            None => 1,
        };
        let bg = s.cursor.pen.bg;
        s.scroll_up(0, rows, positions, bg, limit, events);
        for r in 0..rows {
            reset_row(s.row_mut(r), bg, events);
        }
    }

    pub(crate) fn decaln(&mut self) {
        let rows = self.rows;
        let (s, _, events) = self.parts();
        for r in 0..rows {
            let row = s.row_mut(r);
            end_anchor(row, events);
            row.touch_all();
            for cell in &mut row.cells {
                clear_cell(cell, Color::Default);
                cell.text.push('E');
            }
            row.wrapped = false;
        }
    }

    // Saved cursor, screens and resets.

    pub(crate) fn save_cursor(&mut self) {
        let s = self.scr();
        s.saved = s.cursor.clone();
    }

    pub(crate) fn restore_cursor(&mut self) {
        let s = self.scr();
        s.cursor = s.saved.clone();
    }

    pub(crate) fn enter_alt(&mut self, clear: bool) {
        if self.alt_active {
            return;
        }
        self.alt.cursor = self.main.cursor.clone();
        if clear {
            let bg = self.alt.cursor.pen.bg;
            for r in 0..self.rows {
                reset_row(self.alt.row_mut(r), bg, &mut self.events);
            }
        }
        self.alt_active = true;
        self.events.push_back(Event::AltScreen(true));
    }

    pub(crate) fn leave_alt(&mut self, clear: bool) {
        if !self.alt_active {
            return;
        }
        if clear {
            let bg = self.alt.cursor.pen.bg;
            for r in 0..self.rows {
                reset_row(self.alt.row_mut(r), bg, &mut self.events);
            }
        }
        // The alternate screen's cursor is taken from the main screen's on every entry.
        self.alt.cursor = CursorState::default();
        self.alt_active = false;
        self.events.push_back(Event::AltScreen(false));
    }

    /// RIS. Ends every anchor; `announce` queues `Event::Reset` after them.
    pub(crate) fn full_reset(&mut self, announce: bool) {
        for screen in [&mut self.main, &mut self.alt] {
            for row in &mut screen.buf {
                end_anchor(row, &mut self.events);
            }
        }
        self.main = Screen::new(self.cols, self.rows);
        self.alt = Screen::new(self.cols, self.rows);
        self.alt_active = false;
        self.modes = mode::DEFAULT;
        self.mouse = MouseMode::Off;
        self.mouse_encoding = MouseEncoding::Default;
        self.top = 0;
        self.bot = self.rows;
        self.tabs = default_tabs(self.cols);
        self.active_charset = 0;
        self.cursor_style = None;
        self.title.clear();
        self.icon.clear();
        self.title_stack.clear();
        self.links = Links::new();
        self.overrides.clear();
        if announce {
            self.events.push_back(Event::Reset);
        }
    }

    /// DECSTR.
    pub(crate) fn soft_reset(&mut self) {
        self.modes &= !(mode::INSERT | mode::ORIGIN | mode::APP_CURSOR | mode::APP_KEYPAD);
        self.modes |= mode::SHOW_CURSOR | mode::AUTOWRAP;
        self.top = 0;
        self.bot = self.rows;
        self.active_charset = 0;
        let s = self.scr();
        s.cursor.pen = Default::default();
        s.cursor.charsets = Default::default();
        s.saved = CursorState::default();
    }

    // Modes.

    pub(crate) fn set_private_mode(&mut self, mode_number: u16, on: bool) {
        match mode_number {
            1 => self.set_mode_bit(mode::APP_CURSOR, on),
            3 => {
                // DECCOLM: no column change, but its side effects (as `alacritty_terminal`).
                self.set_scrolling_region(1, None);
                let (s, _, events) = self.parts();
                let bg = s.cursor.pen.bg;
                for r in 0..s.rows() {
                    reset_row(s.row_mut(r), bg, events);
                }
            }
            6 => {
                self.set_mode_bit(mode::ORIGIN, on);
                if on {
                    self.goto(0, 0);
                }
            }
            7 => self.set_mode_bit(mode::AUTOWRAP, on),
            12 => {
                let shape = self.cursor_style.map_or(CursorShape::Block, |s| s.0);
                self.cursor_style = Some((shape, on));
            }
            25 => self.set_mode_bit(mode::SHOW_CURSOR, on),
            9 | 1000 | 1002 | 1003 => {
                let m = match mode_number {
                    9 => MouseMode::Press,
                    1000 => MouseMode::PressRelease,
                    1002 => MouseMode::ButtonMotion,
                    _ => MouseMode::AnyMotion,
                };
                if on {
                    self.mouse = m;
                } else if self.mouse == m {
                    self.mouse = MouseMode::Off;
                }
            }
            1004 => self.set_mode_bit(mode::FOCUS, on),
            1005 | 1006 => {
                let e = if mode_number == 1005 { MouseEncoding::Utf8 } else { MouseEncoding::Sgr };
                if on {
                    self.mouse_encoding = e;
                } else if self.mouse_encoding == e {
                    self.mouse_encoding = MouseEncoding::Default;
                }
            }
            1007 => self.set_mode_bit(mode::ALT_SCROLL, on),
            1042 => self.set_mode_bit(mode::URGENCY, on),
            47 => {
                if on {
                    self.enter_alt(false);
                } else {
                    self.leave_alt(false);
                }
            }
            1047 => {
                if on {
                    self.enter_alt(false);
                } else {
                    self.leave_alt(true);
                }
            }
            1048 => {
                if on {
                    self.save_cursor();
                } else {
                    self.restore_cursor();
                }
            }
            1049 => {
                if on {
                    if !self.alt_active {
                        self.main.saved = self.main.cursor.clone();
                        self.enter_alt(true);
                    }
                } else {
                    self.leave_alt(false);
                }
            }
            2004 => self.set_mode_bit(mode::BRACKETED_PASTE, on),
            2026 => self.set_mode_bit(mode::SYNC, on),
            _ => {}
        }
    }

    pub(crate) fn set_ansi_mode(&mut self, mode_number: u16, on: bool) {
        match mode_number {
            4 => self.set_mode_bit(mode::INSERT, on),
            20 => self.set_mode_bit(mode::NEWLINE, on),
            _ => {}
        }
    }

    /// DECRQM: 0 not recognized, 1 set, 2 reset.
    pub(crate) fn private_mode_state(&self, mode_number: u16) -> u8 {
        let flag = |on: bool| if on { 1 } else { 2 };
        match mode_number {
            1 => flag(self.has(mode::APP_CURSOR)),
            6 => flag(self.has(mode::ORIGIN)),
            7 => flag(self.has(mode::AUTOWRAP)),
            12 => flag(self.cursor_style.is_some_and(|s| s.1)),
            25 => flag(self.has(mode::SHOW_CURSOR)),
            9 => flag(self.mouse == MouseMode::Press),
            1000 => flag(self.mouse == MouseMode::PressRelease),
            1002 => flag(self.mouse == MouseMode::ButtonMotion),
            1003 => flag(self.mouse == MouseMode::AnyMotion),
            1004 => flag(self.has(mode::FOCUS)),
            1005 => flag(self.mouse_encoding == MouseEncoding::Utf8),
            1006 => flag(self.mouse_encoding == MouseEncoding::Sgr),
            1007 => flag(self.has(mode::ALT_SCROLL)),
            1042 => flag(self.has(mode::URGENCY)),
            47 | 1047 | 1049 => flag(self.alt_active),
            2004 => flag(self.has(mode::BRACKETED_PASTE)),
            2026 => flag(self.has(mode::SYNC)),
            _ => 0,
        }
    }

    // Kitty keyboard protocol (separate stacks per screen, as in kitty).

    pub(crate) fn kitty_push(&mut self, flags: u8) {
        let s = self.scr();
        if s.kitty.len() >= KITTY_STACK_DEPTH {
            s.kitty.remove(0);
        }
        s.kitty.push(flags & 0x1F);
    }

    pub(crate) fn kitty_pop(&mut self, n: usize) {
        let s = self.scr();
        let len = s.kitty.len().saturating_sub(n);
        s.kitty.truncate(len);
    }

    pub(crate) fn kitty_set(&mut self, flags: u8, how: u16) {
        let s = self.scr();
        if s.kitty.is_empty() {
            s.kitty.push(0);
        }
        let top = s.kitty.last_mut().expect("non-empty");
        let flags = flags & 0x1F;
        match how {
            2 => *top |= flags,
            3 => *top &= !flags,
            _ => *top = flags,
        }
    }

    // Reports.

    pub(crate) fn device_status(&mut self, arg: u16, private: bool) {
        match (arg, private) {
            (5, false) => self.reply(b"\x1b[0n"),
            (6, _) => {
                let c = self.screen().cursor.clone();
                let row = if self.has(mode::ORIGIN) { c.row.saturating_sub(self.top) } else { c.row };
                let text = if private {
                    format!("\x1b[?{};{};1R", row + 1, c.col + 1)
                } else {
                    format!("\x1b[{};{}R", row + 1, c.col + 1)
                };
                self.reply(text.as_bytes());
            }
            _ => {}
        }
    }

    pub(crate) fn window_report(&mut self, op: u16) {
        let (w, h) = self.facts.cell_px;
        let text = match op {
            14 => format!("\x1b[4;{};{}t", self.rows * usize::from(h), self.cols * usize::from(w)),
            16 => format!("\x1b[6;{h};{w}t"),
            18 => format!("\x1b[8;{};{}t", self.rows, self.cols),
            _ => return,
        };
        self.reply(text.as_bytes());
    }

    // Anchors (§7.4).

    pub(crate) fn open_anchor(&mut self, key: AnchorKey) -> Result<Line, AnchorError> {
        if self.alt_active {
            return Err(AnchorError::AltScreen);
        }
        if self.main.find_anchor(key).is_some() {
            return Err(AnchorError::DuplicateKey);
        }
        let s = &self.main;
        let row = s.row(s.cursor.row);
        let holds = s.cursor.col > 0
            || s.cursor.pending_wrap
            || row.anchor.is_some()
            || row.cells.iter().any(|c| !c.text.is_empty());
        if holds {
            self.carriage_return();
            self.linefeed();
        }
        let s = &mut self.main;
        let index = s.history + s.cursor.row;
        if index > 0 {
            s.buf[index - 1].wrapped = false;
            fix_wide(&mut s.buf[index - 1]);
        }
        let row = &mut s.buf[index];
        for cell in &mut row.cells {
            clear_cell(cell, Color::Default);
        }
        row.wrapped = false;
        row.anchor = Some(key);
        self.carriage_return();
        self.linefeed();
        Ok(self.main.find_anchor(key).unwrap_or(index))
    }

    pub(crate) fn undo_anchor(&mut self, key: AnchorKey) -> bool {
        let s = &mut self.main;
        let Some(index) = s.find_anchor(key) else { return false };
        if index < s.history {
            return false;
        }
        let r = index - s.history;
        let rows = s.rows();
        let below_clear = (r + 1..rows).all(|i| crate::screen::row_is_clear(s.row(i)) && s.row(i).anchor.is_none());
        if self.alt_active || s.cursor.row <= r || !below_clear {
            return false;
        }
        let mut row = s.buf.remove(index).expect("index in range");
        row.touch_all();
        row.anchor = None;
        for cell in &mut row.cells {
            clear_cell(cell, Color::Default);
        }
        row.wrapped = false;
        s.buf.push_back(row);
        s.cursor.row = r;
        s.cursor.col = 0;
        s.cursor.pending_wrap = false;
        true
    }

    pub(crate) fn drop_anchor(&mut self, key: AnchorKey) {
        for screen in [&mut self.main, &mut self.alt] {
            if let Some(index) = screen.find_anchor(key) {
                screen.buf[index].anchor = None;
            }
        }
    }

    /// Marks the cursor row as the anchor `key` (the snapshot's `OSC 6464 ; anchor`).
    pub(crate) fn mark_anchor(&mut self, key: AnchorKey) {
        if self.alt_active || !self.accept_anchor_sequences || self.main.find_anchor(key).is_some() {
            return;
        }
        let s = &mut self.main;
        let r = s.cursor.row;
        s.row_mut(r).anchor = Some(key);
    }

    // Records.

    pub(crate) fn clear_scrollback(&mut self) {
        self.main.clear_history(&mut self.events);
    }

    pub(crate) fn set_config(&mut self, config: Config) {
        self.config = config;
        let limit = self.config.scrollback;
        self.main.trim_history(limit, &mut self.events);
    }
}

/// Before a write to column `col` of buffer row `row_index`: if it holds half of a wide
/// character, the other half becomes a blank (`alacritty_terminal`'s `write_at_cursor`).
fn unpair_for_write(s: &mut Screen, row_index: usize, col: usize, cols: usize) {
    let flags = s.buf[row_index].cells[col].flags.0;
    if flags & (Flags::WIDE | Flags::SPACER) == 0 {
        return;
    }
    let row = &mut s.buf[row_index];
    if flags & Flags::WIDE != 0 && col + 1 < cols {
        row.cells[col + 1].flags.0 &= !Flags::SPACER;
    } else if col > 0 {
        let prev = &mut row.cells[col - 1];
        prev.flags.0 &= !Flags::WIDE;
        prev.text.clear();
    }
    if col <= 1 && row_index > 0 {
        s.buf[row_index - 1].cells[cols - 1].flags.0 &= !Flags::LEADING_SPACER;
    }
}
