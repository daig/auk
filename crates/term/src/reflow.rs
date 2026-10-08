//! Resizing a screen, with reflow of soft-wrapped lines.
//!
//! A port of `alacritty_terminal` 0.26's `grid/resize.rs` (Apache-2.0, Copyright (c) 2020 The
//! Alacritty Project), so that reflow matches the reference grid. Two changes:
//!
//! - **Anchor rows** (§7.4, §17) never join a wrapped line: before reflowing, an anchor row and
//!   the row above it lose their wrap flags, so cells never move into or out of an anchor row;
//!   an anchor row that is too long is cut, not wrapped. Rows that leave the buffer end their
//!   anchors.
//! - Rows are top-first here (the reference stores them bottom-first), and the wrap flag is a
//!   row flag rather than a flag on the row's last cell.

use std::mem;

use crate::screen::{Events, Screen, Slot, blank_row, cell_is_empty, end_anchor, fix_wide};
use crate::{Cell, Color, Flags, Row};

/// The cursor during a resize: its screen line may briefly leave the screen.
struct Cursor {
    line: i64,
    col: usize,
    pending: bool,
}

fn leading_spacer() -> Cell {
    Cell { flags: Flags(Flags::LEADING_SPACER), ..Cell::default() }
}

/// `Point::sub` with `Boundary::Cursor`: moves back `rhs` cells in reading order, then clamps to
/// the screen.
fn point_sub(line: i64, col: usize, cols: usize, lines: usize, rhs: usize) -> (i64, usize) {
    let line_changes = (rhs + cols - 1).saturating_sub(col) / cols;
    let line = line - line_changes as i64;
    let col = (cols + col - rhs % cols) % cols;
    clamp(line, col, cols, lines)
}

fn clamp(line: i64, col: usize, cols: usize, lines: usize) -> (i64, usize) {
    let col = col.min(cols - 1);
    if line < 0 {
        (0, 0)
    } else if line > lines as i64 - 1 {
        (lines as i64 - 1, cols - 1)
    } else {
        (line, col)
    }
}

/// Whether a row is blank and doesn't continue onto the next one.
fn is_clear(row: &Row) -> bool {
    !row.wrapped && row.cells.iter().all(cell_is_empty)
}

/// Splits off the cells past `columns`, dropping trailing blanks (`Row::shrink`). Returns the
/// cells and whether the last of them carries the row's wrap flag.
fn shrink(row: &mut Row, columns: usize) -> Option<(Vec<Cell>, bool)> {
    if row.cells.len() <= columns {
        return None;
    }
    let mut split = row.cells.split_off(columns);
    let wrapped = mem::take(&mut row.wrapped);
    let last = split.len() - 1;
    let keep = split
        .iter()
        .enumerate()
        .rposition(|(i, c)| !cell_is_empty(c) || (i == last && wrapped))
        .map_or(0, |i| i + 1);
    split.truncate(keep);
    let tail = wrapped && keep == last + 1;
    if split.is_empty() { None } else { Some((split, tail)) }
}

impl Screen {
    /// Resizes to `cols` × `rows`, reflowing when `reflow` (the main screen). Rows lost off the
    /// bottom or past the scrollback `limit` end their anchors.
    pub(crate) fn resize(
        &mut self,
        reflow: bool,
        cols: usize,
        rows: usize,
        limit: usize,
        events: &mut Events,
    ) {
        let old_rows = self.rows();
        if rows > old_rows {
            self.grow_lines(rows);
        } else if rows < old_rows {
            self.shrink_lines(rows, limit, events);
        }
        let old_cols = self.buf[0].cells.len();
        if cols > old_cols {
            self.grow_columns(reflow, cols, rows);
        } else if cols < old_cols {
            self.shrink_columns(reflow, cols, rows, limit, events);
        }
    }

    fn grow_lines(&mut self, target: usize) {
        let added = target - self.rows();
        let cols = self.buf[0].cells.len();
        let from_history = self.history.min(added);
        self.history -= from_history;
        for _ in 0..added - from_history {
            self.buf.push_back(Slot::blank(cols));
        }
        self.cursor.row += from_history;
        self.saved.row += from_history;
    }

    fn shrink_lines(&mut self, target: usize, limit: usize, events: &mut Events) {
        let lines = self.rows();
        let required = (self.cursor.row + 1).saturating_sub(target);
        if required > 0 {
            self.scroll_up(0, lines, required, Color::Default, limit, events);
            self.cursor.row = self.cursor.row.min(target - 1);
        }
        self.saved.row = self.saved.row.min(target - 1);
        for _ in 0..lines - target {
            let mut row = self.buf.pop_back().expect("rows exist");
            end_anchor(&mut row, events);
        }
    }

    /// Takes the rows out, top-first, with anchor rows and the rows above them unwrapped.
    fn take_rows(&mut self) -> Vec<Row> {
        let mut rows: Vec<Row> = self.buf.drain(..).map(|slot| slot.row).collect();
        for i in 0..rows.len() {
            if rows[i].anchor.is_some() {
                rows[i].wrapped = false;
                fix_wide(&mut rows[i]);
                if i > 0 {
                    rows[i - 1].wrapped = false;
                    fix_wide(&mut rows[i - 1]);
                }
            }
        }
        rows
    }

    fn grow_columns(&mut self, reflow: bool, columns: usize, lines: usize) {
        let should_reflow =
            |row: &Row| reflow && !row.cells.is_empty() && row.cells.len() < columns && row.wrapped;

        let mut cursor = Cursor {
            line: self.cursor.row as i64,
            col: self.cursor.col,
            pending: self.cursor.pending_wrap,
        };
        if cursor.pending && reflow {
            cursor.pending = false;
            cursor.col += 1;
        }

        let rows = self.take_rows();
        let total = rows.len();
        let mut out: Vec<Row> = Vec::with_capacity(total);
        let mut cursor_line_delta: usize = 0;

        for (t, mut row) in rows.into_iter().enumerate() {
            let i = total - 1 - t;
            if !out.last().is_some_and(should_reflow) {
                out.push(row);
                continue;
            }
            let last_row = out.last_mut().expect("checked above");

            last_row.wrapped = false;

            // A leading spacer goes when its wide character can join the line.
            let mut last_len = last_row.cells.len();
            if last_len >= 1 && last_row.cells[last_len - 1].flags.has(Flags::LEADING_SPACER) {
                last_row.cells.truncate(last_len - 1);
                last_len -= 1;
            }

            let mut num_wrapped = columns - last_len;
            let len = row.cells.len().min(num_wrapped);

            // A wide character that doesn't fit leaves a leading spacer instead.
            let mut cells = if row.cells[len - 1].flags.has(Flags::WIDE) {
                num_wrapped -= 1;
                let rest = row.cells.split_off(len - 1);
                let mut front = mem::replace(&mut row.cells, rest);
                front.push(leading_spacer());
                front
            } else {
                let rest = row.cells.split_off(len);
                mem::replace(&mut row.cells, rest)
            };
            last_row.cells.append(&mut cells);
            if row.cells.is_empty() {
                // The wrap flag travels with the row's last cell.
                last_row.wrapped = mem::take(&mut row.wrapped);
            }

            let cursor_buffer_line = (lines as i64 - cursor.line - 1) as usize;
            let clear = is_clear(&row);
            if i == cursor_buffer_line && reflow {
                let (mut tl, mut tc) = point_sub(cursor.line, cursor.col, columns, lines, num_wrapped);
                if tc == 0 && clear {
                    cursor.pending = true;
                    (tl, tc) = point_sub(tl, tc, columns, lines, 1);
                }
                cursor.col = tc;
                let line_delta = cursor.line - tl;
                if line_delta != 0 && clear {
                    continue;
                }
                cursor_line_delta += line_delta as usize;
            } else if clear {
                if i < cursor_buffer_line {
                    cursor.line += 1;
                }
                continue;
            }

            last_row.wrapped = true;
            out.push(row);
        }

        // At least a screenful.
        if out.len() < lines {
            let delta = (lines - out.len()) as i64;
            cursor.line = (cursor.line - delta).max(0);
            out.resize_with(lines, || blank_row(columns));
        }

        // Pull content down to the cursor, or move the cursor up when nothing is below it.
        if cursor_line_delta != 0 {
            let cursor_buffer_line = (lines as i64 - cursor.line - 1) as usize;
            let available = cursor_buffer_line.min(out.len() - lines);
            let overflow = cursor_line_delta.saturating_sub(available);
            out.truncate(out.len() + overflow - cursor_line_delta);
            cursor.line = (cursor.line - overflow as i64).max(0);
        }

        for row in &mut out {
            if row.cells.len() < columns {
                row.cells.resize(columns, Cell::default());
                // The wrap flag belonged to the old last column.
                row.wrapped = false;
                fix_wide(row);
            }
        }

        self.history = out.len() - lines;
        self.buf = out.into_iter().map(|row| Slot { occ: columns, row }).collect();
        self.cursor.row = cursor.line as usize;
        self.cursor.col = cursor.col;
        self.cursor.pending_wrap = cursor.pending;
    }

    fn shrink_columns(&mut self, reflow: bool, columns: usize, lines: usize, limit: usize, events: &mut Events) {
        let mut cursor = Cursor {
            line: self.cursor.row as i64,
            col: self.cursor.col,
            pending: self.cursor.pending_wrap,
        };
        if cursor.pending && reflow {
            cursor.pending = false;
            cursor.col += 1;
        }

        let rows = self.take_rows();
        let total = rows.len();
        let mut out: Vec<Row> = Vec::with_capacity(total);
        let mut buffered: Option<Vec<Cell>> = None;

        for (t, mut row) in rows.into_iter().enumerate() {
            let i = total - 1 - t;

            if row.anchor.is_some() {
                // Anchor rows are cut, never wrapped.
                row.cells.truncate(columns);
                fix_wide(&mut row);
                out.push(row);
                continue;
            }

            if let Some(buffered) = buffered.take() {
                let cursor_buffer_line = (lines as i64 - cursor.line - 1) as usize;
                if i == cursor_buffer_line {
                    cursor.col += buffered.len();
                }
                row.cells.splice(0..0, buffered);
            }

            loop {
                let (mut wrapped, mut tail) = match shrink(&mut row, columns) {
                    Some(split) if reflow => split,
                    _ => {
                        let cursor_buffer_line = (lines as i64 - cursor.line - 1) as usize;
                        if reflow && i == cursor_buffer_line && cursor.col > columns {
                            // Blanks before the cursor count as content and wrap.
                            (Vec::new(), false)
                        } else {
                            if row.cells.len() > columns {
                                row.cells.truncate(columns);
                            }
                            fix_wide(&mut row);
                            out.push(row);
                            break;
                        }
                    }
                };

                // A wide character in the last column moves down, leaving a leading spacer.
                if row.cells.len() >= columns && row.cells[columns - 1].flags.has(Flags::WIDE) {
                    let wide = mem::replace(&mut row.cells[columns - 1], leading_spacer());
                    if wrapped.is_empty() {
                        tail = false;
                    }
                    wrapped.insert(0, wide);
                }

                // A leading spacer at the end of the cut cells isn't needed.
                let len = wrapped.len();
                if len > 0 && wrapped[len - 1].flags.has(Flags::LEADING_SPACER) {
                    if len == 1 {
                        row.wrapped = true;
                        out.push(row);
                        break;
                    }
                    tail = true;
                    wrapped.truncate(len - 1);
                }

                row.wrapped = true;
                out.push(row);

                if tail && i >= 1 && wrapped.len() < columns {
                    // The rest joins the start of the next row.
                    buffered = Some(wrapped);
                    break;
                }

                let cursor_buffer_line = (lines as i64 - cursor.line - 1) as usize;
                if (i == cursor_buffer_line && cursor.col < columns) || i < cursor_buffer_line {
                    cursor.line = (cursor.line - 1).max(0);
                }
                if i == cursor_buffer_line && cursor.col >= columns {
                    cursor.col -= columns;
                }

                let occ = wrapped.len();
                // The wrap flag stays only if its cell is still the last one.
                let wrapped_flag = tail && occ >= columns;
                if occ < columns {
                    wrapped.resize(columns, Cell::default());
                }
                row = Row { cells: wrapped, wrapped: wrapped_flag, anchor: None };
            }
        }

        // Rows past the scrollback limit go, oldest first.
        let keep = limit + lines;
        if out.len() > keep {
            for mut row in out.drain(..out.len() - keep) {
                end_anchor(&mut row, events);
            }
        }

        self.history = out.len() - lines;
        self.buf = out.into_iter().map(|row| Slot { occ: columns, row }).collect();

        if !reflow {
            cursor.col = cursor.col.min(columns - 1);
        } else if cursor.col == columns && !self.row(cursor.line as usize).wrapped {
            cursor.pending = true;
            cursor.col -= 1;
        } else {
            let (line, col) = clamp(cursor.line, cursor.col, columns, lines);
            cursor.line = line;
            cursor.col = col;
        }
        self.cursor.row = cursor.line as usize;
        self.cursor.col = cursor.col;
        self.cursor.pending_wrap = cursor.pending;
        self.saved.col = self.saved.col.min(columns - 1);
    }
}
