//! The terminal core (spec §17): VT parser, grid, scrollback, main and alternate screens, anchor
//! rows, modes and reports. Deterministic: the same bytes and calls give the same state on every
//! platform, wasm32 included (§15.2).
//!
//! The public API below is the contract other crates build against.
//!
//! Built on a parser vendored from `vte` (with APC strings for TSP, §6.1) and a grid whose
//! semantics follow `alacritty_terminal`, the reference grid (§21); `tests/differential.rs`
//! checks the two against each other and lists the deliberate differences.

mod core;
mod dispatch;
mod hash;
pub mod input;
mod osc;
mod parser;
mod reflow;
mod screen;
mod snapshot;

/// Replicated settings that change parsing (§15.2). They change only through sequenced records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// Scrollback length in rows (a host setting, §5.5).
    pub scrollback: usize,
}

impl Default for Config {
    fn default() -> Self {
        Config { scrollback: 10_000 }
    }
}

/// Answers that depend on a client (§14.2, §15.5): colors for OSC 4/10/11/12 queries and the
/// cell size for `CSI 14/16/18 t`. Not replicated state; only the answering replica uses them.
/// `Facts::default()` is xterm's colors and an 8×16 cell, for before any client has said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Facts {
    /// The cell's width and height in pixels.
    pub cell_px: (u16, u16),
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Rgb,
    /// The ANSI colors; queries for 16–255 answer xterm's cube and gray ramp.
    pub palette: [Rgb; 16],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rgb(pub u8, pub u8, pub u8);

/// A cell color.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Color {
    #[default]
    Default,
    /// 0–255; 0–15 are the theme's ANSI colors.
    Indexed(u8),
    Rgb(Rgb),
}

/// OSC 133 marks (§5.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mark {
    PromptStart,
    PromptEnd,
    /// `C`, with kitty's `cmdline_url` decoded when present, and the line as typed
    /// (`typed_url`, Auk's zsh integration) when the shell reports it too.
    CommandStart {
        cmdline: Option<String>,
        typed: Option<String>,
    },
    CommandEnd {
        status: Option<i32>,
    },
}

/// Something the caller must see in stream order. `Terminal::advance` stops right after the byte
/// that produced it, so the caller can act (open an anchor, discard surfaces) before later bytes
/// are parsed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// A complete `ESC _ tsp;…` string: the bytes after `ESC _`, without the terminator. Other
    /// APC strings are consumed and dropped (§6.1).
    Tsp(Vec<u8>),
    Mark(Mark),
    /// OSC 7, as sent (usually a `file://host/path` URL).
    Cwd(String),
    /// OSC 0 or 2.
    Title(String),
    /// OSC 1337 `SetUserVar=<name>=<base64>`, decoded; empty for `SetUserVar=<name>` (unset).
    UserVar {
        name: String,
        value: Vec<u8>,
    },
    Bell,
    /// RIS (`ESC c`). The grid is already reset when this arrives.
    Reset,
    /// The alternate screen was entered (`true`) or left.
    AltScreen(bool),
    /// The active screen was cleared by `ED 2`/`ED 3` (the screen surface rule, §7.3).
    ScreenCleared,
    /// An anchor row ended: erased, scrolled out of a scroll region or out of the scrollback,
    /// cleared with the scrollback, or dropped by RIS (§7.4).
    AnchorGone(AnchorKey),
    /// The tty's mode-dependent answer queue (`take_replies`) became non-empty.
    Replies,
}

/// The identity of an anchor row (§7.4, §15.3 `key`). Chosen by the caller, unique per pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AnchorKey(pub u64);

/// Modes the input encoder and the renderer need.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Modes {
    pub app_cursor: bool,
    pub app_keypad: bool,
    pub bracketed_paste: bool,
    /// The top of the kitty keyboard flag stack for the active screen (0 when empty).
    pub kitty_flags: u8,
    pub mouse: MouseMode,
    pub mouse_sgr: bool,
    pub focus_reports: bool,
    pub alt_screen: bool,
    pub cursor_visible: bool,
    pub origin: bool,
    pub autowrap: bool,
    pub insert: bool,
    /// LNM: Enter sends CR LF.
    pub newline: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MouseMode {
    #[default]
    Off,
    Press,
    PressRelease,
    ButtonMotion,
    AnyMotion,
}

/// One cell. A wide character occupies its cell plus a following `Spacer` cell.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Cell {
    /// The base character plus combining marks; empty for a blank cell.
    pub text: String,
    pub fg: Color,
    pub bg: Color,
    pub underline_color: Color,
    pub flags: Flags,
    /// OSC 8 link, as an index into `Terminal::links`; 0 for none.
    pub link: u32,
}

/// SGR attributes and cell kind bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash)]
pub struct Flags(pub u16);

impl Flags {
    pub const BOLD: u16 = 1 << 0;
    pub const DIM: u16 = 1 << 1;
    pub const ITALIC: u16 = 1 << 2;
    pub const UNDERLINE: u16 = 1 << 3;
    pub const DOUBLE_UNDERLINE: u16 = 1 << 4;
    pub const CURLY_UNDERLINE: u16 = 1 << 5;
    pub const DOTTED_UNDERLINE: u16 = 1 << 6;
    pub const DASHED_UNDERLINE: u16 = 1 << 7;
    pub const BLINK: u16 = 1 << 8;
    pub const INVERSE: u16 = 1 << 9;
    pub const HIDDEN: u16 = 1 << 10;
    pub const STRIKE: u16 = 1 << 11;
    pub const OVERLINE: u16 = 1 << 12;
    /// The left half of a wide character.
    pub const WIDE: u16 = 1 << 13;
    /// The right half of a wide character; draws nothing.
    pub const SPACER: u16 = 1 << 14;
    /// A blank in the last column of a wrapped row, standing in for a wide character that
    /// didn't fit and went to the next row; draws nothing.
    pub const LEADING_SPACER: u16 = 1 << 15;

    pub fn has(self, bit: u16) -> bool {
        self.0 & bit != 0
    }
}

/// A row of the grid or the scrollback.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Row {
    pub cells: Vec<Cell>,
    /// The row continues on the next one (soft wrap).
    pub wrapped: bool,
    /// Set when the row is an anchor row (§7.4).
    pub anchor: Option<AnchorKey>,
    /// A prompt started on this row (OSC 133 `A`, §5.3): prompt navigation jumps between these.
    /// Erasing the whole row clears it.
    pub prompt: bool,
}

/// Where a row is: an index into the whole buffer of the active screen, counted from the oldest
/// scrollback row (0) to the bottom of the screen (`history_len + rows - 1`).
pub type Line = usize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Cursor {
    pub col: u16,
    /// Screen row, 0 at the top of the screen.
    pub row: u16,
    pub visible: bool,
    pub shape: CursorShape,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    Bar,
}

/// A pane's terminal state machine.
#[derive(Debug)]
pub struct Terminal {
    parser: parser::Parser,
    core: core::Core,
}

impl Terminal {
    /// A blank terminal. Sizes below 2 columns or 1 row are raised to that.
    pub fn new(cols: u16, rows: u16, config: Config) -> Self {
        let (cols, rows) = clamp_size(cols, rows);
        Terminal {
            parser: parser::Parser::new(),
            core: core::Core::new(cols, rows, config),
        }
    }

    /// Parses `bytes` until they run out or an `Event` arises. Returns how many bytes were
    /// consumed and the event, if any. Callers loop until all bytes are consumed.
    ///
    /// When one byte raises several events (RIS, or `ED 2` ending anchors), they come one per
    /// call, the later ones with 0 bytes consumed; events queued by records (`resize`,
    /// `clear_scrollback`, `reset`, `set_config`) also come first. `pending_events` tells whether
    /// any are waiting, and `take_events` drains them.
    pub fn advance(&mut self, bytes: &[u8]) -> (usize, Option<Event>) {
        if let Some(event) = self.core.events.pop_front() {
            return (0, Some(event));
        }
        let n = self.parser.advance(&mut self.core, bytes);
        (n, self.core.events.pop_front())
    }

    /// Parses all of `bytes`, collecting events. For callers that don't act between events.
    pub fn feed(&mut self, mut bytes: &[u8]) -> Vec<Event> {
        let mut events = Vec::new();
        while !bytes.is_empty() {
            let (n, ev) = self.advance(bytes);
            events.extend(ev);
            bytes = &bytes[n..];
        }
        events.extend(self.take_events());
        events
    }

    /// Whether events are queued (see `advance`).
    pub fn pending_events(&self) -> bool {
        !self.core.events.is_empty()
    }

    /// Drains queued events, in order: the rest of a byte's events, or those a record raised.
    pub fn take_events(&mut self) -> Vec<Event> {
        self.core.events.drain(..).collect()
    }

    /// Answers the program asked for (DA1, DA2, DSR, CPR, DECRQM, kitty flag queries, XTVERSION,
    /// OSC color queries, window size reports), in order. Only the answering replica forwards
    /// them to the PTY (§14.2); others drain and drop them.
    pub fn take_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.core.replies)
    }

    /// Client facts for answers. Not part of the replicated state.
    pub fn set_facts(&mut self, facts: Facts) {
        self.core.facts = facts;
    }

    /// Whether `OSC 6464 ; anchor` (which snapshots use to rebuild anchor rows) is honored. On by
    /// default, so snapshots replay on a fresh terminal; a terminal fed a program's output
    /// directly can turn it off so the program can't forge anchors. Not replicated state.
    pub fn set_accept_anchor_sequences(&mut self, accept: bool) {
        self.core.accept_anchor_sequences = accept;
    }

    // Records (§15.2): replicated changes that arrive in-band, in stream order.

    /// Resizes with reflow (§17: anchor rows never join a wrapped line). The main screen
    /// reflows; the alternate screen is cut or padded. Rows lost end their anchors (queued as
    /// `AnchorGone`).
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let (cols, rows) = clamp_size(cols, rows);
        let c = &mut self.core;
        if (cols, rows) == (c.cols, c.rows) {
            return;
        }
        let limit = c.config.scrollback;
        c.main.resize(true, cols, rows, limit, &mut c.events);
        c.alt.resize(false, cols, rows, 0, &mut c.events);
        if cols != c.cols {
            let old = c.tabs.len();
            c.tabs.resize(cols, false);
            for (i, t) in c.tabs.iter_mut().enumerate().skip(old) {
                *t = i % 8 == 0;
            }
        }
        c.cols = cols;
        c.rows = rows;
        c.top = 0;
        c.bot = rows;
    }

    /// Clears the scrollback; ends every anchor in it (queued as `AnchorGone`).
    pub fn clear_scrollback(&mut self) {
        self.core.clear_scrollback();
    }

    /// A user-initiated reset: the same as RIS. Anchors it ends are queued as `AnchorGone`;
    /// `Event::Reset` is not queued, since the caller made the reset.
    pub fn reset(&mut self) {
        self.core.full_reset(false);
        self.parser = parser::Parser::new();
    }

    pub fn set_config(&mut self, config: Config) {
        self.core.set_config(config);
    }

    // Anchors (§7.4).

    /// Opens an anchor at the cursor per §7.4: ends the cursor's row if it holds anything, makes
    /// the row the anchor, and moves the cursor to the start of the next row. Fails on the
    /// alternate screen.
    pub fn open_anchor(&mut self, key: AnchorKey) -> Result<Line, AnchorError> {
        self.core.open_anchor(key)
    }

    /// Undoes an anchor row if nothing was written below it (the prompt rule, §7.3): the row is
    /// removed and the cursor returns to it. Returns whether it was undone.
    pub fn undo_anchor(&mut self, key: AnchorKey) -> bool {
        self.core.undo_anchor(key)
    }

    /// Ends an anchor without touching the grid (its surface was discarded).
    pub fn drop_anchor(&mut self, key: AnchorKey) {
        self.core.drop_anchor(key);
    }

    /// The anchor's current line in the main screen's buffer, if it still exists.
    pub fn anchor_line(&self, key: AnchorKey) -> Option<Line> {
        self.core.main.find_anchor(key)
    }

    // Reading state.

    pub fn cols(&self) -> u16 {
        self.core.cols as u16
    }

    pub fn rows(&self) -> u16 {
        self.core.rows as u16
    }

    /// Scrollback rows above the active screen (0 on the alternate screen).
    pub fn history_len(&self) -> usize {
        self.core.screen().history
    }

    /// A row of the active screen's buffer (see `Line`).
    pub fn line(&self, line: Line) -> Option<&Row> {
        self.core.screen().buf.get(line).map(|slot| &slot.row)
    }

    /// The active screen's lines where a prompt started (OSC 133 `A`), oldest first (§5.3 prompt
    /// navigation).
    pub fn prompt_lines(&self) -> Vec<Line> {
        self.core
            .screen()
            .buf
            .iter()
            .enumerate()
            .filter(|(_, slot)| slot.row.prompt)
            .map(|(i, _)| i)
            .collect()
    }

    /// The main screen's scrollback length, whichever screen is active.
    pub fn main_history_len(&self) -> usize {
        self.core.main.history
    }

    /// A row of the main screen's buffer, whichever screen is active (anchor lines index it).
    pub fn main_line(&self, line: Line) -> Option<&Row> {
        self.core.main.buf.get(line).map(|slot| &slot.row)
    }

    pub fn cursor(&self) -> Cursor {
        let c = &self.core.screen().cursor;
        Cursor {
            col: c.col as u16,
            row: c.row as u16,
            visible: self.core.modes & core::mode::SHOW_CURSOR != 0,
            shape: self.core.cursor_style.map_or(CursorShape::Block, |s| s.0),
        }
    }

    /// Whether the cursor blinks (DECSCUSR or mode 12); `None` when the program left the
    /// default.
    pub fn cursor_blinks(&self) -> Option<bool> {
        self.core.cursor_style.map(|s| s.1)
    }

    /// Whether the cursor waits to wrap: the last column was written and the next character
    /// starts a new row.
    pub fn pending_wrap(&self) -> bool {
        self.core.screen().cursor.pending_wrap
    }

    /// The scroll region (DECSTBM), as the screen rows `top..bottom`.
    pub fn scroll_region(&self) -> std::ops::Range<u16> {
        self.core.top as u16..self.core.bot as u16
    }

    /// Whether synchronized output (mode 2026) is on: the renderer may hold frames until it ends.
    pub fn synchronized_output(&self) -> bool {
        self.core.modes & core::mode::SYNC != 0
    }

    pub fn modes(&self) -> Modes {
        use core::mode;
        let m = self.core.modes;
        Modes {
            app_cursor: m & mode::APP_CURSOR != 0,
            app_keypad: m & mode::APP_KEYPAD != 0,
            bracketed_paste: m & mode::BRACKETED_PASTE != 0,
            kitty_flags: self.core.screen().kitty_flags(),
            mouse: self.core.mouse,
            mouse_sgr: self.core.mouse_encoding == core::MouseEncoding::Sgr,
            focus_reports: m & mode::FOCUS != 0,
            alt_screen: self.core.alt_active,
            cursor_visible: m & mode::SHOW_CURSOR != 0,
            origin: m & mode::ORIGIN != 0,
            autowrap: m & mode::AUTOWRAP != 0,
            insert: m & mode::INSERT != 0,
            newline: m & mode::NEWLINE != 0,
        }
    }

    pub fn title(&self) -> &str {
        &self.core.title
    }

    /// The icon title (OSC 0 or 1).
    pub fn icon_title(&self) -> &str {
        &self.core.icon
    }

    /// OSC 8 targets; index 0 is unused.
    pub fn links(&self) -> &[String] {
        &self.core.links.uris
    }

    /// Theme colors the program set with OSC 4/10/11/12 (replicated state): palette indexes
    /// 0–255, then 256 for the default foreground, 257 for the background and 258 for the
    /// cursor, sorted by index.
    pub fn color_overrides(&self) -> &[(u16, Rgb)] {
        &self.core.overrides
    }

    /// The text of a range of lines, trailing blanks trimmed, wrapped rows joined.
    pub fn text(&self, lines: std::ops::Range<Line>) -> String {
        let buf = &self.core.screen().buf;
        let mut out = String::new();
        let end = lines.end.min(buf.len());
        let start = lines.start.min(end);
        for (index, row) in buf
            .range(start..end)
            .enumerate()
            .map(|(i, r)| (start + i, r))
        {
            let mut line = String::new();
            for cell in &row.cells {
                if cell.flags.has(Flags::SPACER) || cell.flags.has(Flags::LEADING_SPACER) {
                    continue;
                }
                if cell.text.is_empty() {
                    line.push(' ');
                } else {
                    line.push_str(&cell.text);
                }
            }
            if row.wrapped && index + 1 < end {
                out.push_str(&line);
            } else {
                out.push_str(line.trim_end_matches(' '));
                if index + 1 < end {
                    out.push('\n');
                }
            }
        }
        out
    }

    /// A hash of all replicated state: both screens, scrollback, anchors, cursor and saved
    /// cursors, modes and mode stacks, charsets, tab stops, scroll region, SGR pen, title,
    /// links, color overrides, parser state. Platform-independent (wasm32 included).
    pub fn state_hash(&self) -> [u8; 32] {
        hash::state_hash(&self.core, &self.parser)
    }

    /// VT bytes that rebuild this state (§15.3): fed to `Terminal::new(cols, rows, config)`
    /// they give an equal `state_hash`, anchors (keys included) as well. Anchor rows are
    /// written with Auk's private `OSC 6464 ; anchor ; <key> ST`; a few other facts plain VT
    /// can't set use other `OSC 6464` sequences.
    pub fn snapshot(&self) -> Vec<u8> {
        let mut out = self.snapshot_state();
        self.snapshot_parser(&mut out);
        out
    }

    /// The snapshot's first part: everything but the sequence the parser is in the middle of.
    /// It leaves a fresh terminal's parser in the ground state, so a caller can append its own
    /// sequences (a replica's OSC and TSP state) before [`snapshot_parser`](Self::snapshot_parser).
    pub fn snapshot_state(&self) -> Vec<u8> {
        snapshot::snapshot(&self.core)
    }

    /// The snapshot's last part: bytes that bring a parser in the ground state into this one's
    /// state (a half-parsed escape sequence or OSC, DCS or APC string, a partial UTF-8
    /// character). Nothing may follow them in a snapshot; the live stream continues after them.
    pub fn snapshot_parser(&self, out: &mut Vec<u8>) {
        self.parser.replay_bytes(out);
    }
}

fn clamp_size(cols: u16, rows: u16) -> (usize, usize) {
    (usize::from(cols.max(2)), usize::from(rows.max(1)))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnchorError {
    AltScreen,
    DuplicateKey,
}
