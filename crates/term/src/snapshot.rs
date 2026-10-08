//! Snapshots (§15.3): VT bytes that rebuild the state on a fresh terminal of the same size and
//! configuration, so that its `state_hash` is equal.
//!
//! Plain VT rebuilds the cells, modes and reports; a few facts plain VT can't set go through
//! Auk's private `OSC 6464 ; … ST` sequences (see `Core::private_osc`): anchor rows, row wrap
//! flags and leading spacers, exact cursor placement (with its pending-wrap flag), the character
//! REP repeats, and zero-width characters that must not trigger emoji widening.
//!
//! Order: the alternate screen's cells, saved cursor and kitty stack (entered with mode 47,
//! which neither clears nor saves); the main screen's scrollback and rows, streamed so that rows
//! scroll into the scrollback; its saved cursor and kitty stack; tab stops, titles, colors and
//! modes; the cursors; and the REP character. The bytes of any sequence the parser is in the
//! middle of come last, after anything the caller appends (see `Terminal::snapshot_parser`).

use std::fmt::Write as _;

use unicode_width::UnicodeWidthChar;

use crate::core::{Core, MouseEncoding, default_tabs, mode};
use crate::screen::{Charset, CursorState, Pen, Screen};
use crate::{Cell, Color, CursorShape, Flags, MouseMode, Row};

const KIND: u16 = Flags::WIDE | Flags::SPACER | Flags::LEADING_SPACER;

struct Writer<'a> {
    core: &'a Core,
    out: Vec<u8>,
    /// The rebuilt terminal's pen (on its active screen), to skip redundant SGR.
    pen: Pen,
}

impl Writer<'_> {
    fn csi(&mut self, body: &str) {
        self.out.extend_from_slice(b"\x1b[");
        self.out.extend_from_slice(body.as_bytes());
    }

    fn osc(&mut self, body: &str) {
        self.out.extend_from_slice(b"\x1b]");
        self.out.extend_from_slice(body.as_bytes());
        self.out.extend_from_slice(b"\x1b\\");
    }

    fn private_mode(&mut self, number: u16, on: bool) {
        self.csi(&format!("?{number}{}", if on { 'h' } else { 'l' }));
    }

    /// Sets the pen's SGR attributes and link, if they differ.
    fn set_pen(&mut self, pen: &Pen) {
        if self.pen.fg != pen.fg
            || self.pen.bg != pen.bg
            || self.pen.underline_color != pen.underline_color
            || self.pen.flags != pen.flags
        {
            let sgr = sgr(pen);
            self.csi(&sgr);
        }
        if self.pen.link != pen.link {
            self.link(pen.link);
        }
        self.pen = pen.clone();
    }

    fn link(&mut self, index: u32) {
        if index == 0 {
            self.osc("8;;");
            return;
        }
        let i = index as usize;
        let id = match &self.core.links.ids[i] {
            Some(id) => format!("id={id}"),
            None => String::new(),
        };
        let uri = self.core.links.uris[i].clone();
        self.osc(&format!("8;{id};{uri}"));
    }

    /// Writes one row's cells from column 0 of the cursor's row, then its flags and anchor.
    fn row(&mut self, row: &Row) {
        let cols = row.cells.len();
        let last = row
            .cells
            .iter()
            .rposition(|c| *c != Cell::default())
            .map_or(0, |i| i + 1);
        let mut col = 0;
        while col < last {
            let cell = &row.cells[col];
            if cell.flags.has(Flags::SPACER) {
                // Written with its wide character.
                col += 1;
                continue;
            }
            self.set_pen(&cell_pen(cell));
            self.cell_text(cell);
            col += 1;
        }
        let mut bits = 0;
        if row.wrapped {
            bits |= 1;
        }
        if row.cells[cols - 1].flags.has(Flags::LEADING_SPACER) {
            bits |= 2;
        }
        if row.prompt {
            bits |= 4;
        }
        if bits != 0 {
            self.osc(&format!("6464;row;{bits}"));
        }
        if let Some(key) = row.anchor {
            self.osc(&format!("6464;anchor;{}", key.0));
        }
    }

    fn cell_text(&mut self, cell: &Cell) {
        let mut chars = cell.text.chars();
        let Some(base) = chars.next() else {
            self.out.push(b' ');
            return;
        };
        let mut buf = [0; 4];
        self.out
            .extend_from_slice(base.encode_utf8(&mut buf).as_bytes());
        let rest = chars.as_str();
        if rest.is_empty() {
            return;
        }
        // Zero-width characters print normally, except where a VS16 would widen a cell that
        // stayed narrow.
        if !cell.flags.has(Flags::WIDE) && rest.contains('\u{FE0F}') && base.width() == Some(1) {
            let cps: Vec<String> = rest.chars().map(|c| u32::from(c).to_string()).collect();
            for chunk in cps.chunks(14) {
                self.osc(&format!("6464;zw;{}", chunk.join(";")));
            }
        } else {
            self.out.extend_from_slice(rest.as_bytes());
        }
    }

    /// Sets the whole cursor state of the active screen: pen, link, charsets and position.
    fn cursor(&mut self, c: &CursorState) {
        let sgr = sgr(&c.pen);
        self.csi(&sgr);
        self.link(c.pen.link);
        self.pen = c.pen.clone();
        for (slot, cs) in c.charsets.iter().enumerate() {
            let designator = b"()*+"[slot];
            let set = match cs {
                Charset::Ascii => b'B',
                Charset::DecGraphics => b'0',
            };
            self.out.extend_from_slice(&[0x1b, designator, set]);
        }
        self.osc(&format!(
            "6464;cursor;{};{};{}",
            c.row,
            c.col,
            u8::from(c.pending_wrap)
        ));
    }

    fn kitty(&mut self, s: &Screen) {
        for &flags in &s.kitty {
            self.csi(&format!(">{flags}u"));
        }
    }
}

/// The pen that writes `cell` (its attributes without the cell-kind bits).
fn cell_pen(cell: &Cell) -> Pen {
    Pen {
        fg: cell.fg,
        bg: cell.bg,
        underline_color: cell.underline_color,
        flags: cell.flags.0 & !KIND,
        link: cell.link,
    }
}

/// A full SGR sequence body (after `CSI`) for `pen`, starting from a reset.
fn sgr(pen: &Pen) -> String {
    let mut s = String::from("0");
    let f = pen.flags;
    let bits: [(u16, &str); 13] = [
        (Flags::BOLD, "1"),
        (Flags::DIM, "2"),
        (Flags::ITALIC, "3"),
        (Flags::UNDERLINE, "4"),
        (Flags::DOUBLE_UNDERLINE, "4:2"),
        (Flags::CURLY_UNDERLINE, "4:3"),
        (Flags::DOTTED_UNDERLINE, "4:4"),
        (Flags::DASHED_UNDERLINE, "4:5"),
        (Flags::BLINK, "5"),
        (Flags::INVERSE, "7"),
        (Flags::HIDDEN, "8"),
        (Flags::STRIKE, "9"),
        (Flags::OVERLINE, "53"),
    ];
    for (bit, code) in bits {
        if f & bit != 0 {
            s.push(';');
            s.push_str(code);
        }
    }
    color(&mut s, pen.fg, 30, 90, 38);
    color(&mut s, pen.bg, 40, 100, 48);
    match pen.underline_color {
        Color::Default => {}
        Color::Indexed(i) => {
            let _ = write!(s, ";58;5;{i}");
        }
        Color::Rgb(c) => {
            let _ = write!(s, ";58;2;{};{};{}", c.0, c.1, c.2);
        }
    }
    s.push('m');
    s
}

fn color(s: &mut String, c: Color, base: u8, bright: u8, extended: u8) {
    match c {
        Color::Default => {}
        Color::Indexed(i @ 0..=7) => {
            let _ = write!(s, ";{}", base + i);
        }
        Color::Indexed(i @ 8..=15) => {
            let _ = write!(s, ";{}", bright + i - 8);
        }
        Color::Indexed(i) => {
            let _ = write!(s, ";{extended};5;{i}");
        }
        Color::Rgb(c) => {
            let _ = write!(s, ";{extended};2;{};{};{}", c.0, c.1, c.2);
        }
    }
}

fn screen_is_blank(s: &Screen) -> bool {
    s.buf.iter().all(|r| {
        !r.wrapped
            && r.anchor.is_none()
            && !r.prompt
            && r.cells.iter().all(|c| *c == Cell::default())
    })
}

pub(crate) fn snapshot(core: &Core) -> Vec<u8> {
    let mut w = Writer {
        core,
        out: Vec::new(),
        pen: Pen::default(),
    };
    let rows = core.rows;

    // The alternate screen.
    let alt = &core.alt;
    if !screen_is_blank(alt) || alt.saved != CursorState::default() || !alt.kitty.is_empty() {
        w.private_mode(47, true);
        for r in 0..rows {
            let row = alt.row(r);
            if !row.wrapped
                && row.anchor.is_none()
                && row.cells.iter().all(|c| *c == Cell::default())
            {
                continue;
            }
            w.csi(&format!("{};1H", r + 1));
            w.row(row);
        }
        w.cursor(&alt.saved);
        w.out.extend_from_slice(b"\x1b7");
        w.kitty(alt);
        w.private_mode(47, false);
        w.pen = Pen::default();
    }

    // The main screen, streamed top to bottom so that rows scroll into the scrollback.
    let main = &core.main;
    let total = main.buf.len();
    for (k, row) in main.buf.iter().enumerate() {
        w.row(row);
        if k + 1 < total {
            if w.pen.bg != Color::Default {
                // Rows created by scrolling take the pen's background.
                let pen = Pen {
                    link: w.pen.link,
                    ..Pen::default()
                };
                w.set_pen(&pen);
            }
            w.out.extend_from_slice(b"\r\n");
        }
    }
    w.cursor(&main.saved);
    w.out.extend_from_slice(b"\x1b7");
    w.kitty(main);

    // Tab stops (before origin mode can affect CUP).
    if core.tabs != default_tabs(core.cols) {
        w.csi("3g");
        for (col, _) in core.tabs.iter().enumerate().filter(|&(_, &t)| t) {
            w.csi(&format!("1;{}H", col + 1));
            w.out.extend_from_slice(b"\x1bH");
        }
    }

    // Titles.
    for title in &core.title_stack {
        w.osc(&format!("2;{title}"));
        w.csi("22t");
    }
    if !core.icon.is_empty() {
        w.osc(&format!("1;{}", core.icon));
    }
    if !core.title.is_empty() || !core.title_stack.is_empty() {
        w.osc(&format!("2;{}", core.title));
    }

    // Color overrides.
    for &(index, rgb) in &core.overrides {
        let value = format!("rgb:{:02x}/{:02x}/{:02x}", rgb.0, rgb.1, rgb.2);
        match index {
            0..=255 => w.osc(&format!("4;{index};{value}")),
            _ => w.osc(&format!("{};{value}", index - 256 + 10)),
        }
    }

    // Cursor style.
    if let Some((shape, blink)) = core.cursor_style {
        let base = match shape {
            CursorShape::Block => 1,
            CursorShape::Underline => 3,
            CursorShape::Bar => 5,
        };
        w.csi(&format!("{} q", base + u8::from(!blink)));
    }

    // Modes.
    match core.mouse {
        MouseMode::Off => {}
        MouseMode::Press => w.private_mode(9, true),
        MouseMode::PressRelease => w.private_mode(1000, true),
        MouseMode::ButtonMotion => w.private_mode(1002, true),
        MouseMode::AnyMotion => w.private_mode(1003, true),
    }
    match core.mouse_encoding {
        MouseEncoding::Default => {}
        MouseEncoding::Utf8 => w.private_mode(1005, true),
        MouseEncoding::Sgr => w.private_mode(1006, true),
    }
    let private_bits: [(u32, u16); 8] = [
        (mode::APP_CURSOR, 1),
        (mode::AUTOWRAP, 7),
        (mode::SHOW_CURSOR, 25),
        (mode::FOCUS, 1004),
        (mode::ALT_SCROLL, 1007),
        (mode::URGENCY, 1042),
        (mode::BRACKETED_PASTE, 2004),
        (mode::SYNC, 2026),
    ];
    for (bit, number) in private_bits {
        if (core.modes ^ mode::DEFAULT) & bit != 0 {
            w.private_mode(number, core.modes & bit != 0);
        }
    }
    for (bit, number) in [(mode::INSERT, 4), (mode::NEWLINE, 20)] {
        if core.modes & bit != 0 {
            w.csi(&format!("{number}h"));
        }
    }
    if core.modes & mode::APP_KEYPAD != 0 {
        w.out.extend_from_slice(b"\x1b=");
    }
    if (core.top, core.bot) != (0, rows) {
        // DECSTBM needs top < bottom; a region clamped to the last row needs a larger bottom.
        let bottom = core.bot.max(core.top + 2);
        w.csi(&format!("{};{bottom}r", core.top + 1));
    }
    if core.modes & mode::ORIGIN != 0 {
        w.private_mode(6, true);
    }

    // The cursors: the main screen's, then the alternate screen's if it is active.
    w.cursor(&main.cursor);
    if core.alt_active {
        w.private_mode(47, true);
        w.cursor(&alt.cursor);
    }
    match core.active_charset {
        0 => {}
        1 => w.out.push(0x0E),
        // G2 and G3 can't be invoked; they are never active.
        _ => {}
    }

    // What REP repeats.
    match core.preceding {
        Some(c) => w.osc(&format!("6464;rep;{}", u32::from(c))),
        None => w.osc("6464;rep"),
    }

    w.out
}
