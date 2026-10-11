//! The reference grid for the `terminal` suite: recordings of real programs
//! (`fixtures/terminal/*.vt.jsonl`, made by `tools/vt_record.py`), and `alacritty_terminal` fed
//! the same bytes and resizes as an Auk grid, compared cell by cell.
//!
//! The comparison follows `crates/term/tests/common/reference.rs`, and the intended differences
//! listed at the top of `crates/term/tests/differential.rs` apply here too. Those that real
//! programs reach are handled as that file says: the reference is repaired after every step for
//! broken wide characters, stale wrap flags and row occupancy (6, 11, 12), and tab cells compare
//! as blank (8). The rest are listed with the tests that meet them.

use std::fs;
use std::path::{Path, PathBuf};

use alacritty_terminal::Term;
use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::Flags as AFlags;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::vte::ansi::{self, Processor};
use auk_term::{Color, Config, Flags, Terminal};
use base64::Engine;
use serde_json::Value;

/// The recordings, by name (`fixtures/terminal/<name>.vt.jsonl`).
pub const RECORDINGS: [&str; 8] = [
    "vim", "less", "htop", "gitlog", "tmux", "wide", "emoji", "resize",
];

/// The directory of terminal recordings and the files they use.
pub fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/terminal")
}

/// One step of a recording.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Bytes the program wrote, as one read.
    Output(Vec<u8>),
    /// The terminal was resized here.
    Resize(u16, u16),
}

/// A recording and its metadata.
#[derive(Clone, Debug)]
pub struct Recording {
    pub name: String,
    pub cols: u16,
    pub rows: u16,
    pub steps: Vec<Step>,
    pub meta: Value,
}

impl Recording {
    /// All output bytes, in order.
    pub fn bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for step in &self.steps {
            if let Step::Output(b) = step {
                out.extend_from_slice(b);
            }
        }
        out
    }

    /// The output between resizes: `(size, bytes)` per segment, the first at the starting size.
    pub fn segments(&self) -> Vec<((u16, u16), Vec<u8>)> {
        let mut segs = vec![((self.cols, self.rows), Vec::new())];
        for step in &self.steps {
            match step {
                Step::Output(b) => segs.last_mut().unwrap().1.extend_from_slice(b),
                Step::Resize(c, r) => segs.push(((*c, *r), Vec::new())),
            }
        }
        segs
    }
}

/// Loads `fixtures/terminal/<name>.vt.jsonl` and its `.meta.json`, panicking with the path on a
/// missing or malformed file.
pub fn load(name: &str) -> Recording {
    let dir = fixtures_dir();
    let stream = dir.join(format!("{name}.vt.jsonl"));
    let meta_path = dir.join(format!("{name}.meta.json"));
    let read = |p: &Path| {
        fs::read_to_string(p).unwrap_or_else(|e| panic!("recording {}: {e}", p.display()))
    };
    let meta: Value = serde_json::from_str(&read(&meta_path))
        .unwrap_or_else(|e| panic!("{}: {e}", meta_path.display()));
    let b64 = base64::engine::general_purpose::STANDARD;
    let mut steps = Vec::new();
    for (n, line) in read(&stream).lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("{}:{}: {e}", stream.display(), n + 1));
        if let Some(out) = v["out"].as_str() {
            let bytes = b64
                .decode(out)
                .unwrap_or_else(|e| panic!("{}:{}: {e}", stream.display(), n + 1));
            steps.push(Step::Output(bytes));
        } else if let Some([c, r]) = v["resize"].as_array().map(Vec::as_slice) {
            let size = |x: &Value| x.as_u64().and_then(|x| u16::try_from(x).ok()).unwrap();
            steps.push(Step::Resize(size(c), size(r)));
        }
    }
    let size = |key: &str| {
        meta[key]
            .as_u64()
            .and_then(|x| u16::try_from(x).ok())
            .unwrap_or_else(|| panic!("{}: no `{key}`", meta_path.display()))
    };
    Recording {
        name: name.to_owned(),
        cols: size("cols"),
        rows: size("rows"),
        steps,
        meta,
    }
}

/// `alacritty_terminal`, the reference grid.
pub struct Reference {
    pub term: Term<VoidListener>,
    processor: Processor,
    /// Whether insert mode is on, so printing is fed a byte at a time (see `feed`).
    insert: bool,
    /// The start of a UTF-8 character the last `feed` ended in, held back (see `feed`).
    partial: Vec<u8>,
}

impl std::fmt::Debug for Reference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reference").finish_non_exhaustive()
    }
}

impl Reference {
    pub fn new(cols: u16, rows: u16, scrollback: usize) -> Reference {
        let config = alacritty_terminal::term::Config {
            scrolling_history: scrollback,
            kitty_keyboard: true,
            ..Default::default()
        };
        let size = TermSize::new(usize::from(cols), usize::from(rows));
        Reference {
            term: Term::new(config, &size, VoidListener),
            processor: Processor::new(),
            insert: false,
            partial: Vec::new(),
        }
    }

    /// Feeds bytes. A UTF-8 character split across two calls is held back until it is whole:
    /// `vte` 0.15 drops the ASCII byte after such a character when a non-ASCII one follows it
    /// (`"γ\xce"` then `"\xb4 А"` prints `γδА`), a bug in the reference, not a difference.
    /// Holding the bytes back changes nothing visible, since a partial character prints nothing.
    pub fn feed(&mut self, bytes: &[u8]) {
        let mut buf = std::mem::take(&mut self.partial);
        buf.extend_from_slice(bytes);
        let keep = partial_utf8_len(&buf);
        self.partial = buf.split_off(buf.len() - keep);
        let bytes = &buf[..];
        if self.insert || self.term.mode().contains(TermMode::INSERT) {
            // Insert-mode printing can break wide characters, and the reference misbehaves
            // around the broken halves at the very next character (difference 6), so it is fed
            // and repaired a byte at a time.
            for b in bytes {
                self.processor
                    .advance(&mut self.term, std::slice::from_ref(b));
                repair(&mut self.term);
            }
        } else {
            self.processor.advance(&mut self.term, bytes);
        }
        self.insert = self.term.mode().contains(TermMode::INSERT);
        self.flush();
    }

    /// Ends a synchronized update the reference is buffering (it would time out on its own),
    /// and applies Auk's rules for the intended differences 6, 11 and 12.
    pub fn flush(&mut self) {
        if self.processor.sync_bytes_count() > 0 {
            self.processor.stop_sync(&mut self.term);
        }
        repair(&mut self.term);
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.flush();
        self.term
            .resize(TermSize::new(usize::from(cols), usize::from(rows)));
        repair(&mut self.term);
    }

    /// A row's text, for messages.
    fn row_text(&self, line: Line) -> String {
        let grid = self.term.grid();
        let text: String = (0..grid.columns())
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
        text.trim_end().to_owned()
    }
}

/// How many bytes at the end of `bytes` are the start of a UTF-8 character still missing its
/// continuation bytes.
fn partial_utf8_len(bytes: &[u8]) -> usize {
    for back in 1..=3.min(bytes.len()) {
        let b = bytes[bytes.len() - back];
        if b & 0xc0 == 0x80 {
            continue;
        }
        let need = match b {
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => return 0,
        };
        return if back < need { back } else { 0 };
    }
    0
}

/// Auk and the reference, fed the same input.
#[derive(Debug)]
pub struct Pair {
    pub auk: Terminal,
    pub reference: Reference,
}

impl Pair {
    pub fn new(cols: u16, rows: u16, scrollback: usize) -> Pair {
        Pair {
            auk: Terminal::new(cols, rows, Config { scrollback }),
            reference: Reference::new(cols, rows, scrollback),
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.reference.feed(bytes);
        self.auk.feed(bytes);
        let _ = self.auk.take_replies();
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.reference.resize(cols, rows);
        self.auk.resize(cols, rows);
        let _ = self.auk.take_events();
    }

    pub fn compare(&self) -> Result<usize, String> {
        compare(&self.auk, &self.reference)
    }
}

/// Compares an Auk grid with the reference: size, scrollback, every cell of the scrollback and
/// screen (text, colors, attributes, link, wide-character kind), wrap flags, the cursor, a
/// pending wrap and the modes both track. Returns a description of the first difference, or the
/// number of rows where cells from a VS16 sequence on were left out.
///
/// Intended difference 7 (VS16 widens an emoji-presentation character to two cells in Auk; the
/// reference keeps it narrow): from the first cell holding U+FE0F, in either grid, to the end of
/// that row, cells aren't compared, and neither is the cursor when it is there. Everything else
/// in such a row (cells to the left, the wrap flag) still is. When the row is wrapped, the
/// rows its line continues on are left out too, since the widened cells push text onto them.
pub fn compare(a: &Terminal, reference: &Reference) -> Result<usize, String> {
    let r = &reference.term;
    let grid = r.grid();
    let rows = usize::from(a.rows());
    let cols = usize::from(a.cols());
    if grid.screen_lines() != rows || grid.columns() != cols {
        return Err(format!(
            "size {cols}x{rows} vs reference {}x{}",
            grid.columns(),
            grid.screen_lines()
        ));
    }
    if grid.history_size() != a.history_len() {
        return Err(format!(
            "history {} vs reference {}\n{}",
            a.history_len(),
            grid.history_size(),
            dump(a, reference)
        ));
    }
    let history = a.history_len();
    let mut skipped = 0;
    let mut cursor_skip = usize::MAX;
    let mut continues_vs16 = false;
    for index in 0..history + rows {
        let line = Line(index as i32 - history as i32);
        let row = a.line(index).expect("line exists");
        let rrow = &grid[line];
        let rwrapped = rrow[Column(cols - 1)].flags.contains(AFlags::WRAPLINE);
        if row.wrapped != rwrapped {
            return Err(format!(
                "line {index}: wrapped {} vs reference {rwrapped}\n{}",
                row.wrapped,
                dump(a, reference)
            ));
        }
        let vs16 = if continues_vs16 {
            Some(0)
        } else {
            (0..cols).find(|&c| {
                row.cells[c].text.contains('\u{fe0f}')
                    || rrow[Column(c)]
                        .zerowidth()
                        .is_some_and(|z| z.contains(&'\u{fe0f}'))
            })
        };
        continues_vs16 = vs16.is_some() && row.wrapped;
        if let Some(c) = vs16 {
            skipped += 1;
            if index == history + usize::from(a.cursor().row) {
                cursor_skip = c;
            }
        }
        for col in 0..vs16.unwrap_or(cols) {
            let ours = describe(&row.cells[col], a);
            let theirs = describe_reference(&rrow[Column(col)]);
            if ours != theirs {
                return Err(format!(
                    "line {index} col {col}: {ours:?} vs reference {theirs:?}\n{}",
                    dump(a, reference)
                ));
            }
        }
    }
    let cursor = a.cursor();
    let rc = grid.cursor.point;
    let skip_cursor = usize::from(cursor.col).max(rc.column.0) >= cursor_skip
        && usize::from(cursor.row) == rc.line.0 as usize;
    if !skip_cursor
        && (usize::from(cursor.row), usize::from(cursor.col)) != (rc.line.0 as usize, rc.column.0)
    {
        return Err(format!(
            "cursor {},{} vs reference {},{}\n{}",
            cursor.row,
            cursor.col,
            rc.line.0,
            rc.column.0,
            dump(a, reference)
        ));
    }
    if !skip_cursor && a.pending_wrap() != grid.cursor.input_needs_wrap {
        return Err(format!(
            "pending wrap {} vs reference {}",
            a.pending_wrap(),
            grid.cursor.input_needs_wrap
        ));
    }
    let modes = a.modes();
    let rmode = *r.mode();
    let pairs = [
        ("alt screen", modes.alt_screen, TermMode::ALT_SCREEN),
        ("app cursor", modes.app_cursor, TermMode::APP_CURSOR),
        ("app keypad", modes.app_keypad, TermMode::APP_KEYPAD),
        ("origin", modes.origin, TermMode::ORIGIN),
        ("autowrap", modes.autowrap, TermMode::LINE_WRAP),
        ("insert", modes.insert, TermMode::INSERT),
        (
            "cursor visible",
            modes.cursor_visible,
            TermMode::SHOW_CURSOR,
        ),
        (
            "bracketed paste",
            modes.bracketed_paste,
            TermMode::BRACKETED_PASTE,
        ),
        ("sgr mouse", modes.mouse_sgr, TermMode::SGR_MOUSE),
        ("focus", modes.focus_reports, TermMode::FOCUS_IN_OUT),
    ];
    for (name, ours, flag) in pairs {
        let theirs = rmode.contains(flag);
        if ours != theirs {
            return Err(format!("mode {name}: {ours} vs reference {theirs}"));
        }
    }
    Ok(skipped)
}

/// Both grids as text, side by side.
pub fn dump(a: &Terminal, reference: &Reference) -> String {
    let history = a.history_len();
    let mut s = String::from("auk | reference\n");
    for index in 0..history + usize::from(a.rows()) {
        let line = Line(index as i32 - history as i32);
        let ours = a.text(index..index + 1);
        let theirs = if index < history + reference.term.grid().screen_lines() {
            reference.row_text(line)
        } else {
            String::new()
        };
        s += &format!("{index:3} {ours:<40}| {theirs}\n");
    }
    s
}

/// A cell as compared: text, colors, attributes, link.
type Described = (String, Color, Color, Color, u16, String);

/// Blink and overline are kept by Auk and dropped by alacritty (difference 9).
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
    // Tabs compare as blank (difference 8).
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

/// Applies Auk's rules to the reference grid: an unpaired wide character or spacer becomes a
/// blank, a leading spacer only stays in the last column of a wrapped row, and a wrap flag only
/// counts in the last column (differences 6 and 12); every row is marked fully occupied
/// (difference 11).
fn repair(term: &mut Term<VoidListener>) {
    let grid = term.grid_mut();
    let cols = grid.columns();
    let top = -(grid.history_size() as i32);
    let bottom = grid.screen_lines() as i32;
    for l in top..bottom {
        let row = &mut grid[Line(l)];
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

/// A small deterministic generator, so fuzz failures reproduce from the seed.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// A number in `0..n`.
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n.max(1)
    }

    /// A number in `lo..=hi`.
    pub fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.below(hi - lo + 1)
    }
}

/// Splits a stream into the units `allowed` judges: escape sequences (CSI, OSC and other
/// strings, two- and three-byte escapes) and runs of other bytes. Approximate at broken
/// sequences, which is fine for its use: deciding what a fuzzed stream may contain.
pub fn tokens(bytes: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        if bytes[i] != 0x1b {
            while i < bytes.len() && bytes[i] != 0x1b {
                i += 1;
            }
        } else if i + 1 >= bytes.len() {
            i += 1;
        } else {
            match bytes[i + 1] {
                b'[' => {
                    i += 2;
                    while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) && bytes[i] != 0x1b
                    {
                        i += 1;
                    }
                    if i < bytes.len() && bytes[i] != 0x1b {
                        i += 1;
                    }
                }
                b']' | b'P' | b'_' | b'^' | b'X' => {
                    i += 2;
                    while i < bytes.len() {
                        if bytes[i] == 0x07 {
                            i += 1;
                            break;
                        }
                        if bytes[i] == 0x1b {
                            if bytes.get(i + 1) == Some(&b'\\') {
                                i += 2;
                            }
                            break;
                        }
                        i += 1;
                    }
                }
                _ => {
                    i += 1;
                    while i < bytes.len() && (0x20..=0x2f).contains(&bytes[i]) {
                        i += 1;
                    }
                    if i < bytes.len() {
                        i += 1;
                    }
                }
            }
        }
        out.push(&bytes[start..i]);
    }
    out
}

/// Whether a token is outside the intended differences given the terminal's state, for fuzzed
/// streams built from pieces of recordings (the differences in `crates/term/tests/differential.rs`):
/// 1 DCH past the end of the row, 2 ED 1 on row 1, 3 vertical moves inside a scroll region,
/// 4 horizontal absolute moves in origin mode under a region, 5 CBT with a wrap pending, 9 the
/// modes alacritty lacks, 11 RIS on the alternate screen.
pub fn allowed(t: &Terminal, token: &[u8]) -> bool {
    if token == b"\x1bc" {
        return !t.modes().alt_screen;
    }
    let Some(body) = token.strip_prefix(b"\x1b[") else {
        return true;
    };
    let Some((&fin, params)) = body.split_last() else {
        return true;
    };
    let params = std::str::from_utf8(params).unwrap_or("");
    if let Some(private) = params.strip_prefix('?') {
        return !(matches!(fin, b'h' | b'l')
            && private
                .split(';')
                .any(|p| matches!(p, "47" | "1047" | "1048"))
            || fin == b'W');
    }
    if params.starts_with(['>', '<', '=']) {
        return true;
    }
    if params.ends_with('!') || params.contains(['!', '$', '"', ' ', '\'']) {
        return false;
    }
    if matches!(fin, b'h' | b'l') && params.split(';').any(|p| p == "20") {
        return false;
    }
    if fin == b'm' && params.split([';', ':']).any(|p| p == "21") {
        return false;
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
        b'P' => count < t.cols() - cursor.col,
        b'J' if first == 1 => cursor.row != 1,
        b'A' | b'B' | b'e' | b'E' | b'F' => full_region,
        b'G' | b'`' => !(t.modes().origin && region.start > 0),
        b'Z' => !t.pending_wrap(),
        b'b' => !t.modes().insert,
        _ => true,
    }
}
