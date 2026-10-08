//! The reference-grid harness: Auk and `alacritty_terminal` fed the same input, compared cell
//! by cell. The intended differences it accounts for are listed in `tests/differential.rs`.

use alacritty_terminal::Term;
use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags as AFlags;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::vte::ansi::{self, Processor};
use auk_term::{Color, Config, Flags, Terminal};

/// Auk and the reference, fed the same input.
pub struct Pair {
    pub auk: Terminal,
    pub reference: Term<VoidListener>,
    processor: Processor,
    /// Whether the main grid reflowed while the alternate screen was active (difference 12).
    pub resized_in_alt: bool,
}

impl Pair {
    pub fn new(cols: u16, rows: u16, scrollback: usize) -> Pair {
        let config = alacritty_terminal::term::Config {
            scrolling_history: scrollback,
            kitty_keyboard: true,
            ..Default::default()
        };
        let size = TermSize::new(usize::from(cols), usize::from(rows));
        Pair {
            auk: Terminal::new(cols, rows, Config { scrollback }),
            reference: Term::new(config, &size, VoidListener),
            processor: Processor::new(),
            resized_in_alt: false,
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        if self.auk.modes().insert {
            // Insert-mode printing can break wide characters, and the reference misbehaves
            // around the broken halves at the very next character (difference 6), so it is fed
            // and repaired a byte at a time.
            for b in bytes {
                self.processor
                    .advance(&mut self.reference, std::slice::from_ref(b));
                repair_reference(&mut self.reference);
            }
        } else {
            self.processor.advance(&mut self.reference, bytes);
        }
        self.auk.feed(bytes);
        self.flush();
    }

    /// Ends a synchronized update the reference is buffering (it would time out on its own).
    fn flush(&mut self) {
        if self.processor.sync_bytes_count() > 0 {
            self.processor.stop_sync(&mut self.reference);
        }
        repair_reference(&mut self.reference);
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.flush();
        self.auk.resize(cols, rows);
        self.reference
            .resize(TermSize::new(usize::from(cols), usize::from(rows)));
        repair_reference(&mut self.reference);
    }

    /// Compares the two, returning a description of the first difference.
    pub fn compare(&self) -> Result<(), String> {
        let a = &self.auk;
        let r = &self.reference;
        let grid = r.grid();
        let rows = usize::from(a.rows());
        let cols = usize::from(a.cols());
        if grid.screen_lines() != rows || grid.columns() != cols {
            return Err(format!(
                "size {}x{} vs {}x{}",
                cols,
                rows,
                grid.columns(),
                grid.screen_lines()
            ));
        }
        if grid.history_size() != a.history_len() {
            return Err(format!(
                "history {} vs reference {}",
                a.history_len(),
                grid.history_size()
            ));
        }
        let history = a.history_len();
        for index in 0..history + rows {
            let line = Line(index as i32 - history as i32);
            let row = a.line(index).expect("line exists");
            let rrow = &grid[line];
            let rwrapped = rrow[Column(cols - 1)].flags.contains(AFlags::WRAPLINE);
            if row.wrapped != rwrapped {
                return Err(format!(
                    "line {index}: wrapped {} vs reference {rwrapped}\n{}",
                    row.wrapped,
                    self.dump()
                ));
            }
            for col in 0..cols {
                let cell = &row.cells[col];
                let rcell = &rrow[Column(col)];
                let ours = describe(cell, a);
                let theirs = describe_reference(rcell);
                if ours != theirs {
                    return Err(format!(
                        "line {index} col {col}: {ours:?} vs reference {theirs:?}\n{}",
                        self.dump()
                    ));
                }
            }
        }
        let cursor = a.cursor();
        let rc = grid.cursor.point;
        if (usize::from(cursor.row), usize::from(cursor.col)) != (rc.line.0 as usize, rc.column.0) {
            return Err(format!(
                "cursor {},{} vs reference {},{}\n{}",
                cursor.row,
                cursor.col,
                rc.line.0,
                rc.column.0,
                self.dump()
            ));
        }
        if a.pending_wrap() != grid.cursor.input_needs_wrap {
            return Err(format!(
                "pending wrap {} vs reference {}",
                a.pending_wrap(),
                grid.cursor.input_needs_wrap
            ));
        }
        let modes = a.modes();
        let rmode = *r.mode();
        use alacritty_terminal::term::TermMode;
        let pairs = [
            (
                "alt screen",
                modes.alt_screen,
                rmode.contains(TermMode::ALT_SCREEN),
            ),
            (
                "app cursor",
                modes.app_cursor,
                rmode.contains(TermMode::APP_CURSOR),
            ),
            (
                "app keypad",
                modes.app_keypad,
                rmode.contains(TermMode::APP_KEYPAD),
            ),
            ("origin", modes.origin, rmode.contains(TermMode::ORIGIN)),
            (
                "autowrap",
                modes.autowrap,
                rmode.contains(TermMode::LINE_WRAP),
            ),
            ("insert", modes.insert, rmode.contains(TermMode::INSERT)),
            (
                "cursor visible",
                modes.cursor_visible,
                rmode.contains(TermMode::SHOW_CURSOR),
            ),
            (
                "bracketed paste",
                modes.bracketed_paste,
                rmode.contains(TermMode::BRACKETED_PASTE),
            ),
            (
                "sgr mouse",
                modes.mouse_sgr,
                rmode.contains(TermMode::SGR_MOUSE),
            ),
            (
                "focus",
                modes.focus_reports,
                rmode.contains(TermMode::FOCUS_IN_OUT),
            ),
        ];
        for (name, ours, theirs) in pairs {
            if ours != theirs {
                return Err(format!("mode {name}: {ours} vs reference {theirs}"));
            }
        }
        Ok(())
    }

    pub fn dump(&self) -> String {
        let a = &self.auk;
        let grid = self.reference.grid();
        let history = a.history_len();
        let mut s = String::from("auk | reference\n");
        for index in 0..history + usize::from(a.rows()) {
            let line = Line(index as i32 - history as i32);
            let ours = a.text(index..index + 1);
            let theirs: String = (0..grid.columns())
                .filter(|&c| {
                    !grid[line][Column(c)]
                        .flags
                        .intersects(AFlags::WIDE_CHAR_SPACER | AFlags::LEADING_WIDE_CHAR_SPACER)
                })
                .map(|c| {
                    let cell = &grid[line][Column(c)];
                    let mut t = if cell.c == '\t' {
                        ' '.to_string()
                    } else {
                        cell.c.to_string()
                    };
                    t.extend(cell.zerowidth().unwrap_or(&[]));
                    t
                })
                .collect();
            s += &format!("{index:3} {ours:<30}| {}\n", theirs.trim_end());
        }
        s
    }
}

/// A cell as compared: text, colors, attributes, link.
type Described = (String, Color, Color, Color, u16, String);

const COMPARED_FLAGS: u16 = !(Flags::BLINK | Flags::OVERLINE);

fn describe(cell: &auk_term::Cell, t: &Terminal) -> Described {
    let link = t
        .links()
        .get(cell.link as usize)
        .cloned()
        .unwrap_or_default();
    (
        cell.text.clone(),
        cell.fg,
        cell.bg,
        cell.underline_color,
        cell.flags.0 & COMPARED_FLAGS,
        link,
    )
}

fn reference_color(c: ansi::Color) -> Color {
    match c {
        ansi::Color::Named(n) => match n as usize {
            i @ 0..=15 => Color::Indexed(i as u8),
            _ => Color::Default,
        },
        ansi::Color::Indexed(i) => Color::Indexed(i),
        ansi::Color::Spec(rgb) => Color::Rgb(auk_term::Rgb(rgb.r, rgb.g, rgb.b)),
    }
}

fn describe_reference(cell: &alacritty_terminal::term::cell::Cell) -> Described {
    let mut text = String::new();
    let zerowidth = cell.zerowidth().unwrap_or(&[]);
    if !(matches!(cell.c, ' ' | '\t') && zerowidth.is_empty()) {
        text.push(if cell.c == '\t' { ' ' } else { cell.c });
        text.extend(zerowidth);
    }
    let map = [
        (AFlags::INVERSE, Flags::INVERSE),
        (AFlags::BOLD, Flags::BOLD),
        (AFlags::ITALIC, Flags::ITALIC),
        (AFlags::UNDERLINE, Flags::UNDERLINE),
        (AFlags::WIDE_CHAR, Flags::WIDE),
        (AFlags::WIDE_CHAR_SPACER, Flags::SPACER),
        (AFlags::DIM, Flags::DIM),
        (AFlags::HIDDEN, Flags::HIDDEN),
        (AFlags::STRIKEOUT, Flags::STRIKE),
        (AFlags::LEADING_WIDE_CHAR_SPACER, Flags::LEADING_SPACER),
        (AFlags::DOUBLE_UNDERLINE, Flags::DOUBLE_UNDERLINE),
        (AFlags::UNDERCURL, Flags::CURLY_UNDERLINE),
        (AFlags::DOTTED_UNDERLINE, Flags::DOTTED_UNDERLINE),
        (AFlags::DASHED_UNDERLINE, Flags::DASHED_UNDERLINE),
    ];
    let flags = map
        .iter()
        .filter(|(a, _)| cell.flags.contains(*a))
        .fold(0, |f, (_, b)| f | b);
    let ul = cell
        .underline_color()
        .map_or(Color::Default, reference_color);
    let link = cell
        .hyperlink()
        .map(|h| h.uri().to_owned())
        .unwrap_or_default();
    (
        text,
        reference_color(cell.fg),
        reference_color(cell.bg),
        ul,
        flags,
        link,
    )
}

/// Applies Auk's wide-character rule (difference 6) to the reference grid: an unpaired wide
/// character or spacer becomes a blank, a leading spacer only stays in the last column of a
/// wrapped row, and a wrap flag only counts in the last column.
fn repair_reference(term: &mut Term<VoidListener>) {
    let grid = term.grid_mut();
    let cols = grid.columns();
    let top = -(grid.history_size() as i32);
    let bottom = grid.screen_lines() as i32;
    for l in top..bottom {
        let row = &mut grid[Line(l)];
        // Marks the whole row occupied (difference 11).
        let _ = &mut row[..];
        for c in 0..cols - 1 {
            row[Column(c)].flags.remove(AFlags::WRAPLINE);
        }
        let wrapped = row[Column(cols - 1)].flags.contains(AFlags::WRAPLINE);
        for c in 0..cols {
            let flags = row[Column(c)].flags;
            if flags.contains(AFlags::WIDE_CHAR) {
                let paired =
                    c + 1 < cols && row[Column(c + 1)].flags.contains(AFlags::WIDE_CHAR_SPACER);
                if !paired {
                    row[Column(c)].clear_wide();
                }
            } else if flags.contains(AFlags::WIDE_CHAR_SPACER) {
                let paired = c > 0 && row[Column(c - 1)].flags.contains(AFlags::WIDE_CHAR);
                if !paired {
                    row[Column(c)].flags.remove(AFlags::WIDE_CHAR_SPACER);
                }
            } else if flags.contains(AFlags::LEADING_WIDE_CHAR_SPACER)
                && (c + 1 != cols || !wrapped)
            {
                row[Column(c)]
                    .flags
                    .remove(AFlags::LEADING_WIDE_CHAR_SPACER);
            }
        }
    }
}

/// Whether a token is outside the compared behavior given the current state (the intended
/// differences 1–5 in the module docs).
pub fn allowed(pair: &Pair, bytes: &[u8]) -> bool {
    let t = &pair.auk;
    if bytes == b"\x1bc" {
        // 11. RIS would reset the reference's inactive main grid with stale occupancy.
        return !t.modes().alt_screen;
    }
    let Some(body) = bytes.strip_prefix(b"\x1b[") else {
        return true;
    };
    let Some((&fin, params)) = body.split_last() else {
        return true;
    };
    let params = std::str::from_utf8(params).unwrap_or("");
    if params.starts_with(['?', '>', '<', '=']) {
        return true;
    }
    let first: u16 = params
        .split(';')
        .next()
        .and_then(|p| p.parse().ok())
        .unwrap_or(0);
    let count = first.max(1);
    let cursor = t.cursor();
    let region = t.scroll_region();
    let full_region = region == (0..t.rows());
    match fin {
        // 1. DCH past the end of the row.
        b'P' => count < t.cols() - cursor.col,
        // 2. ED 1 on row 1.
        b'J' if first == 1 => cursor.row != 1,
        // 3. Vertical moves with a scroll region.
        b'A' | b'B' | b'e' | b'E' | b'F' => full_region,
        // 4. Horizontal absolute moves in origin mode with the region's top below row 0.
        b'G' | b'`' => !(t.modes().origin && region.start > 0),
        // 5. CBT with a wrap pending.
        b'Z' => !t.pending_wrap(),
        // 6. REP in insert mode repeats inside one sequence, so the reference can't be repaired
        //    between the characters (see `Pair::feed`).
        b'b' => !t.modes().insert,
        _ => true,
    }
}
