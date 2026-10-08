//! Lens capture (§9.6 "Aborting" and the line rules of `LensDef.line`): a claimed command's
//! output is diverted into an 8-row scratch screen, and each row that scrolls off becomes a
//! [`LensLine`]: colors stripped, `\r` rewrites resolved, soft wraps joined, trailing blanks
//! trimmed, with its OSC 8 links.
//!
//! Deterministic and free of Luau: every replica runs the same capture over the same bytes, so
//! each one aborts at the same byte and replays the same raw output into its grid. Only the host
//! hands the lines to the plugin.

use auk_term::{Config, Event, Flags, Mark, MouseMode, Row, Terminal};

/// Rows of the scratch screen.
pub const SCRATCH_ROWS: u16 = 8;
/// A capture past this many bytes aborts.
pub const MAX_CAPTURE: usize = 16 * 1024 * 1024;
/// Bytes fed to the scratch screen per step; with a scrollback as long, no row is lost.
const STEP: usize = 4096;

/// An OSC 8 hyperlink on a lens line: 1-based inclusive byte offsets into the text.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LensLink {
    /// First byte.
    pub from: usize,
    /// Last byte.
    pub to: usize,
    /// Target.
    pub href: String,
}

/// One output line, as a lens's `line` receives it.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct LensLine {
    /// The text.
    pub text: String,
    /// Its links.
    pub links: Vec<LensLink>,
}

/// Why a capture stopped early.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Abort {
    /// The program entered the alternate screen.
    AltScreen,
    /// The program turned on keypad or mouse modes.
    Modes,
    /// The program asked for the cursor position.
    CursorQuery,
    /// The output passed [`MAX_CAPTURE`].
    TooLarge,
}

/// What [`Capture::feed_stream`] did with a piece of the pane's stream.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Step {
    /// Bytes of the input the capture took: all of it, or up to and including the mark that
    /// ended the command, or up to the end of the piece that aborted it. The rest is the grid's.
    pub consumed: usize,
    /// Lines that scrolled off the scratch screen.
    pub lines: Vec<LensLine>,
    /// The shell-integration mark that ended the command (OSC 133 `D`, or an `A` without one).
    pub end: Option<Mark>,
    /// Terminal answers the captured program asked for (other than a cursor position report,
    /// which aborts), for the answering replica to forward.
    pub replies: Vec<u8>,
    /// Why the capture aborted, if it did; the caller replays [`Capture::raw`] into the grid.
    pub abort: Option<Abort>,
}

/// A running capture.
#[derive(Debug)]
pub struct Capture {
    term: Terminal,
    raw: Vec<u8>,
    /// Scrollback rows already handed out.
    taken: usize,
    /// A soft-wrapped line whose continuation hasn't scrolled off yet.
    partial: Option<LensLine>,
    aborted: Option<Abort>,
}

impl Capture {
    /// A capture at `cols` columns (the pane's width when the command started).
    pub fn new(cols: u16) -> Self {
        let mut term = Terminal::new(cols.max(2), SCRATCH_ROWS, Config { scrollback: STEP + 16 });
        term.set_accept_anchor_sequences(false);
        Capture {
            term,
            raw: Vec::new(),
            taken: 0,
            partial: None,
            aborted: None,
        }
    }

    /// The capture width.
    pub fn cols(&self) -> u16 {
        self.term.cols()
    }

    /// Everything captured, to replay into the grid on abort or show as Raw.
    pub fn raw(&self) -> &[u8] {
        &self.raw
    }

    /// Why the capture aborted, once it has.
    pub fn aborted(&self) -> Option<Abort> {
        self.aborted
    }

    /// Feeds output. Returns the lines that scrolled off, or the abort reason; after an abort the
    /// caller replays [`raw`](Self::raw) into the grid and drops the capture.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<LensLine>, Abort> {
        if let Some(why) = self.aborted {
            return Err(why);
        }
        let mut lines = Vec::new();
        for piece in bytes.chunks(STEP) {
            if self.raw.len() + piece.len() > MAX_CAPTURE {
                self.raw.extend_from_slice(piece);
                return Err(self.abort(Abort::TooLarge));
            }
            self.raw.extend_from_slice(piece);
            for event in self.term.feed(piece) {
                if event == Event::AltScreen(true) {
                    return Err(self.abort(Abort::AltScreen));
                }
            }
            let modes = self.term.modes();
            if modes.app_keypad || modes.mouse != MouseMode::Off {
                return Err(self.abort(Abort::Modes));
            }
            let replies = self.term.take_replies();
            if is_cursor_report(&replies) {
                return Err(self.abort(Abort::CursorQuery));
            }
            self.drain_history(&mut lines);
        }
        Ok(lines)
    }

    /// Feeds a piece of the pane's stream while a claimed command runs, stopping after the mark
    /// that ends the command (`D`, or `A` when the shell skipped `D`). Unlike [`feed`](Self::feed)
    /// it looks at every event, so a replica can hand the bytes after the mark back to its grid.
    ///
    /// Deterministic in the bytes and in how they are split into calls: every replica of a pane
    /// gets the same stream items, so each one stops and aborts at the same byte.
    pub fn feed_stream(&mut self, bytes: &[u8]) -> Step {
        let mut step = Step::default();
        if let Some(why) = self.aborted {
            step.abort = Some(why);
            return step;
        }
        for piece in bytes.chunks(STEP) {
            if self.raw.len() + piece.len() > MAX_CAPTURE {
                self.raw.extend_from_slice(piece);
                step.consumed += piece.len();
                step.abort = Some(self.abort(Abort::TooLarge));
                return step;
            }
            let mut rest = piece;
            while !rest.is_empty() {
                let (n, event) = self.term.advance(rest);
                let n = n.min(rest.len());
                self.raw.extend_from_slice(&rest[..n]);
                step.consumed += n;
                rest = &rest[n..];
                let mut batch: Vec<Event> = event.into_iter().collect();
                batch.extend(self.term.take_events());
                for event in batch {
                    match event {
                        Event::AltScreen(true) => {
                            self.take_rest(rest, &mut step);
                            step.abort = Some(self.abort(Abort::AltScreen));
                            return step;
                        }
                        Event::Mark(mark @ (Mark::CommandEnd { .. } | Mark::PromptStart)) => {
                            step.end = Some(mark);
                        }
                        _ => {}
                    }
                }
                let replies = self.term.take_replies();
                if is_cursor_report(&replies) {
                    self.take_rest(rest, &mut step);
                    step.abort = Some(self.abort(Abort::CursorQuery));
                    return step;
                }
                step.replies.extend(replies);
                if step.end.is_some() {
                    self.drain_history(&mut step.lines);
                    return step;
                }
                if n == 0 && event_is_none(&self.term) {
                    break;
                }
            }
            let modes = self.term.modes();
            if modes.app_keypad || modes.mouse != MouseMode::Off {
                step.abort = Some(self.abort(Abort::Modes));
                return step;
            }
            self.drain_history(&mut step.lines);
        }
        step
    }

    /// An aborting piece belongs to the capture whole: its rest joins the raw output.
    fn take_rest(&mut self, rest: &[u8], step: &mut Step) {
        self.raw.extend_from_slice(rest);
        step.consumed += rest.len();
    }

    /// Rebuilds a running capture from its raw output so far (a replica built from a snapshot
    /// taken while the command ran). Nothing is checked or handed out: the capture was running
    /// when the snapshot was taken.
    pub fn resume(cols: u16, raw: &[u8]) -> Self {
        let mut capture = Capture::new(cols);
        let mut discard = Vec::new();
        for piece in raw.chunks(STEP) {
            capture.raw.extend_from_slice(piece);
            capture.term.feed(piece);
            capture.term.take_replies();
            capture.drain_history(&mut discard);
            discard.clear();
        }
        capture
    }

    fn abort(&mut self, why: Abort) -> Abort {
        self.aborted = Some(why);
        why
    }

    /// Hands out the rows that scrolled into the scrollback since the last call.
    fn drain_history(&mut self, out: &mut Vec<LensLine>) {
        let history = self.term.history_len();
        for line in self.taken..history {
            if let Some(row) = self.term.line(line) {
                let row = row.clone();
                self.push_row(&row, out);
            }
        }
        self.taken = history;
        if history > STEP {
            self.term.clear_scrollback();
            self.taken = 0;
        }
    }

    fn push_row(&mut self, row: &Row, out: &mut Vec<LensLine>) {
        let mut line = self.partial.take().unwrap_or_default();
        append_row(&self.term, row, &mut line);
        if row.wrapped {
            self.partial = Some(line);
        } else {
            trim_end(&mut line);
            out.push(line);
        }
    }

    /// The command finished: the rows still on the scratch screen, up to the last one written.
    pub fn finish(&mut self) -> Vec<LensLine> {
        let mut out = Vec::new();
        if self.aborted.is_some() {
            return out;
        }
        self.drain_history(&mut out);
        let history = self.term.history_len();
        let rows = usize::from(self.term.rows());
        // Rows up to the last one holding text.
        let mut last: Option<usize> = None;
        for r in 0..rows {
            if let Some(row) = self.term.line(history + r)
                && row.cells.iter().any(|c| !c.text.is_empty() && c.text != " ")
            {
                last = Some(r);
            }
        }
        let end = match last {
            Some(l) => l,
            None => {
                if let Some(partial) = self.partial.take() {
                    let mut partial = partial;
                    trim_end(&mut partial);
                    out.push(partial);
                }
                return out;
            }
        };
        for r in 0..=end {
            if let Some(row) = self.term.line(history + r) {
                let row = row.clone();
                let mut row = row;
                if r == end {
                    row.wrapped = false;
                }
                self.push_row(&row, &mut out);
            }
        }
        out
    }
}

/// Whether the parser stopped without consuming bytes and without an event (never expected;
/// guards the loop).
fn event_is_none(term: &Terminal) -> bool {
    !term.pending_events()
}

/// Whether the terminal's answers include a cursor position report (`CSI row ; col R`).
fn is_cursor_report(replies: &[u8]) -> bool {
    let mut i = 0;
    while i + 1 < replies.len() {
        if replies[i] == 0x1b && replies[i + 1] == b'[' {
            let mut j = i + 2;
            while j < replies.len() && (replies[j].is_ascii_digit() || replies[j] == b';' || replies[j] == b'?') {
                j += 1;
            }
            if replies.get(j) == Some(&b'R') {
                return true;
            }
            i = j;
        } else {
            i += 1;
        }
    }
    false
}

fn append_row(term: &Terminal, row: &Row, line: &mut LensLine) {
    let links = term.links();
    for cell in &row.cells {
        if cell.flags.has(Flags::SPACER) || cell.flags.has(Flags::LEADING_SPACER) {
            continue;
        }
        let text = if cell.text.is_empty() { " " } else { cell.text.as_str() };
        let from = line.text.len() + 1;
        line.text.push_str(text);
        let to = line.text.len();
        if cell.link != 0
            && let Some(href) = links.get(cell.link as usize)
        {
            match line.links.last_mut() {
                Some(last) if last.href == *href && last.to + 1 == from => last.to = to,
                _ => line.links.push(LensLink {
                    from,
                    to,
                    href: href.clone(),
                }),
            }
        }
    }
}

fn trim_end(line: &mut LensLine) {
    let trimmed = line.text.trim_end_matches(' ').len();
    line.text.truncate(trimmed);
    line.links.retain_mut(|l| {
        if l.from > trimmed {
            return false;
        }
        l.to = l.to.min(trimmed);
        true
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(lines: &[LensLine]) -> Vec<&str> {
        lines.iter().map(|l| l.text.as_str()).collect()
    }

    #[test]
    fn lines_scroll_off_and_finish_delivers_the_rest() {
        let mut c = Capture::new(20);
        let mut got = Vec::new();
        for i in 0..10 {
            got.extend(c.feed(format!("line {i}\r\n").as_bytes()).expect("no abort"));
        }
        // 10 lines and the cursor on an 8-row screen: three rows scrolled off.
        assert_eq!(texts(&got), ["line 0", "line 1", "line 2"]);
        got.extend(c.finish());
        assert_eq!(got.len(), 10);
        assert_eq!(got[9].text, "line 9");
    }

    #[test]
    fn colors_stripped_rewrites_resolved_wraps_joined() {
        let mut c = Capture::new(20);
        c.feed(b"\x1b[31mred\x1b[0m text   \r\n").expect("ok");
        c.feed(b"progress 10%\rprogress 100%\r\n").expect("ok");
        c.feed(b"0123456789abcdefghijKLMN\r\n").expect("ok");
        let lines = c.finish();
        assert_eq!(
            texts(&lines),
            ["red text", "progress 100%", "0123456789abcdefghijKLMN"]
        );
    }

    #[test]
    fn links_are_byte_offsets() {
        let mut c = Capture::new(40);
        c.feed(b"see \x1b]8;;https://x.test\x1b\\h\xc3\xa9re\x1b]8;;\x1b\\ ok\r\n").expect("ok");
        let lines = c.finish();
        assert_eq!(lines[0].text, "see h\u{e9}re ok");
        assert_eq!(
            lines[0].links,
            [LensLink {
                from: 5,
                to: 9,
                href: "https://x.test".into()
            }]
        );
    }

    #[test]
    fn the_stream_ends_at_the_mark_and_aborts_at_the_same_byte_however_it_is_split() {
        let stream = b"one\r\ntwo\r\n\x1b]133;D;3\x07\x1b]133;A\x07$ ";
        let mut c = Capture::new(20);
        let step = c.feed_stream(stream);
        let end = stream.len() - b"\x1b]133;A\x07$ ".len();
        assert_eq!(step.consumed, end);
        assert_eq!(step.end, Some(Mark::CommandEnd { status: Some(3) }));
        assert_eq!(texts(&c.finish()), ["one", "two"]);
        // Split byte by byte: the same stop.
        let mut c = Capture::new(20);
        let mut at = 0;
        loop {
            let step = c.feed_stream(&stream[at..at + 1]);
            at += step.consumed;
            if step.end.is_some() {
                break;
            }
        }
        assert_eq!(at, end);
        // An abort takes the rest of its piece.
        let mut c = Capture::new(20);
        let step = c.feed_stream(b"x\x1b[?1049hmore");
        assert_eq!((step.abort, step.consumed), (Some(Abort::AltScreen), 13));
        assert_eq!(c.raw(), b"x\x1b[?1049hmore");
        let step = Capture::new(20).feed_stream(b"a\x1b[cb");
        assert_eq!(step.replies.is_empty(), false, "DA1 is answered");
        assert_eq!(step.abort, None);
        let step = Capture::new(20).feed_stream(b"a\x1b[6nb");
        assert_eq!(step.abort, Some(Abort::CursorQuery));
        let step = Capture::new(20).feed_stream(b"\x1b[?1000h");
        assert_eq!(step.abort, Some(Abort::Modes));
    }

    #[test]
    fn a_resumed_capture_continues_where_the_raw_output_left_off() {
        let mut live = Capture::new(20);
        let mut lines = Vec::new();
        for i in 0..12 {
            lines.extend(live.feed_stream(format!("l{i}\r\n").as_bytes()).lines);
        }
        let mut resumed = Capture::resume(20, live.raw());
        let a = live.feed_stream(b"tail\r\n").lines;
        let b = resumed.feed_stream(b"tail\r\n").lines;
        assert_eq!(a, b);
        assert_eq!(live.finish(), resumed.finish());
        assert_eq!(live.raw(), resumed.raw());
    }

    #[test]
    fn interactive_programs_abort() {
        let mut c = Capture::new(20);
        assert_eq!(c.feed(b"x\x1b[?1049h"), Err(Abort::AltScreen));
        assert_eq!(c.raw(), b"x\x1b[?1049h");
        assert_eq!(Capture::new(20).feed(b"\x1b="), Err(Abort::Modes));
        assert_eq!(Capture::new(20).feed(b"\x1b[?1000h"), Err(Abort::Modes));
        assert_eq!(Capture::new(20).feed(b"\x1b[6n"), Err(Abort::CursorQuery));
        let mut big = Capture::new(20);
        let chunk = vec![b'a'; 1024 * 1024];
        let mut result = Ok(Vec::new());
        for _ in 0..17 {
            result = big.feed(&chunk);
            if result.is_err() {
                break;
            }
        }
        assert_eq!(result, Err(Abort::TooLarge));
    }
}
