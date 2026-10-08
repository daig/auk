//! One pane replica (spec §15.1, §22): the pane's terminal plus the sequenced stream that drives
//! it. The stream is the pane's output bytes with records (§15.2) inserted at exact positions;
//! every item carries a sequence number, and a replica applies items strictly in order.
//!
//! The daemon's replica is authoritative: it numbers the items and is the only one whose answers
//! (DA1, CPR, TSP replies) reach the program (§14.2). Client replicas follow: they apply the same
//! items, start from a snapshot taken at a sequence number (§15.3), and drop every answer.
//!
//! TSP is a seam: complete `ESC _ tsp;…` strings go to an optional [`TspHandler`], which the
//! TSP crate provides. Without one, TSP strings are consumed and dropped.
//!
//! Builds for wasm32: the browser client runs the same replica.

use std::collections::BTreeMap;
use std::fmt;

use auk_term::{Config, Terminal};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub use auk_term::Event;

/// The parsing behavior every replica of a host must share (§15.2). Clients whose parser version
/// differs from the daemon's are refused at attach (X9). It names this crate's version, which the
/// whole workspace shares (so it covers `auk-term`, its Unicode width tables and `auk-tsp`), plus
/// a revision bumped whenever parsing changes between releases.
pub const PARSER_VERSION: &str = concat!("auk-replica/", env!("CARGO_PKG_VERSION"), "+r1");

/// Host settings that change parsing (§15.2, §5.5). They change only through
/// [`Record::Settings`], so every replica switches at the same stream position.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// Scrollback length in rows.
    pub scrollback: usize,
    /// TSP on or off (`native_surfaces`).
    pub native_surfaces: bool,
    /// Lens claims on or off (`command_lenses`).
    pub command_lenses: bool,
    /// The per-pane retention budget for surface documents, in bytes (§7.10).
    pub retention: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            scrollback: Config::default().scrollback,
            native_surfaces: true,
            command_lenses: true,
            retention: 64 << 20,
        }
    }
}

impl Settings {
    fn term_config(&self) -> Config {
        Config {
            scrollback: self.scrollback,
        }
    }
}

/// A sequenced record (§15.2): a change to replicated state that isn't pane output.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Record {
    Resize {
        cols: u16,
        rows: u16,
    },
    ClearScrollback,
    Reset,
    Settings {
        settings: Settings,
    },
    /// The pane's process ended (its code, or 128 plus the signal). Inline and flow surfaces
    /// close as if kept (§7.3), at this position on every replica.
    Exit {
        status: i32,
    },
}

/// One item of a pane's stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Item<'a> {
    Output(&'a [u8]),
    Record(&'a Record),
}

/// Whose answers reach the program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// The daemon's replica: answers queue for [`Replica::take_replies`].
    Authoritative,
    /// A client's replica: answers are dropped.
    Follower,
}

/// A replica's state as a byte stream replayed through the normal parser (§15.3), at the stream
/// position `seq`. Replaying it into a new replica of the same size and settings gives an equal
/// [`Replica::state_hash`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    /// The sequence number of the last item the snapshot includes; 0 for none.
    pub seq: u64,
    pub cols: u16,
    pub rows: u16,
    pub settings: Settings,
    /// The process's exit status, once it has ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<i32>,
    /// VT bytes, then the replica's own state as OSC sequences, then TSP (when a handler exists).
    #[serde(skip)]
    pub bytes: Vec<u8>,
}

/// An item arrived out of order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeqError {
    pub expected: u64,
    pub got: u64,
}

impl fmt::Display for SeqError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "stream item {} arrived; expected {}",
            self.got, self.expected
        )
    }
}

impl std::error::Error for SeqError {}

/// What a [`TspHandler`] may touch while handling a TSP string or a terminal event.
#[derive(Debug)]
pub struct TspContext<'a> {
    pub term: &'a mut Terminal,
    pub settings: &'a Settings,
    /// True while a snapshot is being replayed: nothing may reach the program.
    pub replaying: bool,
    /// Answers for the program, in order with the terminal's own. Dropped unless the replica is
    /// authoritative and not replaying.
    pub replies: &'a mut Vec<u8>,
}

/// The TSP document model behind a replica (§7). The `auk-tsp` crate implements it.
pub trait TspHandler: Send + fmt::Debug {
    /// A complete TSP string: the bytes after `ESC _`, without the terminator.
    fn on_tsp(&mut self, payload: &[u8], ctx: TspContext<'_>);

    /// A terminal event the surface rules depend on: marks, reset, the alternate screen, cleared
    /// screens and ended anchors.
    fn on_event(&mut self, event: &Event, ctx: TspContext<'_>) {
        let _ = (event, ctx);
    }

    /// A record, after the terminal applied it: resizes, settings, process exit.
    fn on_record(&mut self, record: &Record, ctx: TspContext<'_>) {
        let _ = (record, ctx);
    }

    /// TSP bytes that rebuild the documents (§15.3, steps 1–5), appended to the VT snapshot.
    fn snapshot(&self) -> Vec<u8> {
        Vec::new()
    }

    /// A hash of the replicated document state.
    fn state_hash(&self) -> [u8; 32] {
        [0; 32]
    }
}

/// One pane replica.
#[derive(Debug)]
pub struct Replica {
    term: Terminal,
    role: Role,
    seq: u64,
    settings: Settings,
    exit: Option<i32>,
    cwd: Option<String>,
    user_vars: BTreeMap<String, Vec<u8>>,
    replies: Vec<u8>,
    tsp: Option<Box<dyn TspHandler>>,
}

impl Replica {
    /// An empty replica at stream position 0.
    pub fn new(cols: u16, rows: u16, settings: Settings, role: Role) -> Self {
        Replica {
            term: Terminal::new(cols, rows, settings.term_config()),
            role,
            seq: 0,
            settings,
            exit: None,
            cwd: None,
            user_vars: BTreeMap::new(),
            replies: Vec::new(),
            tsp: None,
        }
    }

    /// Rebuilds a replica from a snapshot. Live items continue at `snapshot.seq + 1`. Nothing the
    /// replay would answer is kept (§15.3).
    pub fn from_snapshot(
        snapshot: &Snapshot,
        role: Role,
        tsp: Option<Box<dyn TspHandler>>,
    ) -> Self {
        let mut replica = Replica::new(
            snapshot.cols,
            snapshot.rows,
            snapshot.settings.clone(),
            role,
        );
        replica.tsp = tsp;
        replica.exit = snapshot.exit;
        let mut events = Vec::new();
        replica.feed(&snapshot.bytes, true, &mut events);
        replica.term.take_replies();
        replica.replies.clear();
        replica.seq = snapshot.seq;
        replica
    }

    /// Installs the TSP handler. Set it before the first item; documents aren't rebuilt.
    pub fn set_tsp_handler(&mut self, handler: Box<dyn TspHandler>) {
        self.tsp = Some(handler);
    }

    /// The sequence number of the last item applied; 0 before any.
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// Applies the item numbered `seq`, which must be the next one. Returns the terminal events
    /// it raised, in order, apart from TSP strings and reply notifications, which the replica
    /// handles itself.
    pub fn apply(&mut self, seq: u64, item: Item<'_>) -> Result<Vec<Event>, SeqError> {
        let expected = self.seq + 1;
        if seq != expected {
            return Err(SeqError { expected, got: seq });
        }
        let mut events = Vec::new();
        match item {
            Item::Output(bytes) => self.feed(bytes, false, &mut events),
            Item::Record(record) => self.apply_record(record, &mut events),
        }
        self.seq = seq;
        Ok(events)
    }

    /// Applies `item` as the next one, for the replica that numbers the stream. Returns its
    /// sequence number and events.
    pub fn push(&mut self, item: Item<'_>) -> (u64, Vec<Event>) {
        let seq = self.seq + 1;
        let events = self
            .apply(seq, item)
            .expect("the next sequence number is always accepted");
        (seq, events)
    }

    /// Answers for the program, in order. Always empty for a follower.
    pub fn take_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.replies)
    }

    pub fn role(&self) -> Role {
        self.role
    }

    pub fn terminal(&self) -> &Terminal {
        &self.term
    }

    /// Client facts for answers (§15.5). Not replicated.
    pub fn set_facts(&mut self, facts: auk_term::Facts) {
        self.term.set_facts(facts);
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// The process's exit status, once a [`Record::Exit`] arrived.
    pub fn exit(&self) -> Option<i32> {
        self.exit
    }

    /// The last OSC 7 cwd, as sent.
    pub fn cwd(&self) -> Option<&str> {
        self.cwd.as_deref()
    }

    /// OSC 1337 user variables (§5.4 `resume`), decoded.
    pub fn user_vars(&self) -> &BTreeMap<String, Vec<u8>> {
        &self.user_vars
    }

    /// The whole screen buffer as text: scrollback, then the screen.
    pub fn text(&self) -> String {
        self.term
            .text(0..self.term.history_len() + usize::from(self.term.rows()))
    }

    /// The visible screen as text.
    pub fn screen_text(&self) -> String {
        let top = self.term.history_len();
        self.term.text(top..top + usize::from(self.term.rows()))
    }

    /// A hash of all replicated state: the terminal's, the stream position, the settings, the
    /// cwd and user variables, and the TSP documents. Equal on every platform for equal streams.
    pub fn state_hash(&self) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(b"auk-replica state v1\0");
        h.update(self.term.state_hash());
        h.update(self.seq.to_le_bytes());
        let s = &self.settings;
        h.update((s.scrollback as u64).to_le_bytes());
        h.update([u8::from(s.native_surfaces), u8::from(s.command_lenses)]);
        h.update(s.retention.to_le_bytes());
        match self.exit {
            None => h.update([0]),
            Some(status) => {
                h.update([1]);
                h.update(status.to_le_bytes());
            }
        }
        hash_bytes(&mut h, self.cwd.as_deref().map(str::as_bytes));
        h.update((self.user_vars.len() as u64).to_le_bytes());
        for (name, value) in &self.user_vars {
            hash_bytes(&mut h, Some(name.as_bytes()));
            hash_bytes(&mut h, Some(value));
        }
        h.update(self.tsp.as_ref().map_or([0; 32], |t| t.state_hash()));
        h.finalize().into()
    }

    /// The snapshot at the current stream position (§15.3).
    pub fn snapshot(&self) -> Snapshot {
        let mut bytes = self.term.snapshot();
        if let Some(cwd) = &self.cwd {
            write_osc(&mut bytes, &format!("7;{cwd}"));
        }
        let b64 = base64::engine::general_purpose::STANDARD;
        for (name, value) in &self.user_vars {
            write_osc(
                &mut bytes,
                &format!("1337;SetUserVar={name}={}", b64.encode(value)),
            );
        }
        if let Some(tsp) = &self.tsp {
            bytes.extend(tsp.snapshot());
        }
        Snapshot {
            seq: self.seq,
            cols: self.term.cols(),
            rows: self.term.rows(),
            settings: self.settings.clone(),
            exit: self.exit,
            bytes,
        }
    }

    fn apply_record(&mut self, record: &Record, events: &mut Vec<Event>) {
        match record {
            Record::Resize { cols, rows } => self.term.resize(*cols, *rows),
            Record::ClearScrollback => self.term.clear_scrollback(),
            Record::Reset => {
                self.term.reset();
                self.observe(Event::Reset, false, events);
            }
            Record::Settings { settings } => {
                self.term.set_config(settings.term_config());
                self.settings = settings.clone();
            }
            Record::Exit { status } => self.exit = Some(*status),
        }
        self.collect_replies(false);
        if let Some(tsp) = &mut self.tsp {
            let mut replies = Vec::new();
            tsp.on_record(
                record,
                TspContext {
                    term: &mut self.term,
                    settings: &self.settings,
                    replaying: false,
                    replies: &mut replies,
                },
            );
            self.keep_replies(replies, false);
        }
    }

    fn feed(&mut self, mut bytes: &[u8], replaying: bool, events: &mut Vec<Event>) {
        while !bytes.is_empty() {
            let (n, event) = self.term.advance(bytes);
            bytes = &bytes[n.min(bytes.len())..];
            match event {
                Some(event) => self.observe(event, replaying, events),
                // The terminal consumes at least one byte unless it stops on an event.
                None if n == 0 => break,
                None => {}
            }
        }
        self.collect_replies(replaying);
    }

    fn observe(&mut self, event: Event, replaying: bool, events: &mut Vec<Event>) {
        match event {
            Event::Replies => self.collect_replies(replaying),
            Event::Tsp(payload) => {
                // Answers already queued go first, so replies keep stream order.
                self.collect_replies(replaying);
                if let Some(tsp) = &mut self.tsp {
                    let mut replies = Vec::new();
                    tsp.on_tsp(
                        &payload,
                        TspContext {
                            term: &mut self.term,
                            settings: &self.settings,
                            replaying,
                            replies: &mut replies,
                        },
                    );
                    self.keep_replies(replies, replaying);
                }
            }
            event => {
                match &event {
                    Event::Cwd(cwd) => self.cwd = Some(cwd.clone()),
                    Event::UserVar { name, value } => {
                        self.user_vars.insert(name.clone(), value.clone());
                    }
                    _ => {}
                }
                if let Some(tsp) = &mut self.tsp {
                    let mut replies = Vec::new();
                    tsp.on_event(
                        &event,
                        TspContext {
                            term: &mut self.term,
                            settings: &self.settings,
                            replaying,
                            replies: &mut replies,
                        },
                    );
                    self.keep_replies(replies, replaying);
                }
                if !replaying {
                    events.push(event);
                }
            }
        }
    }

    fn collect_replies(&mut self, replaying: bool) {
        let replies = self.term.take_replies();
        self.keep_replies(replies, replaying);
    }

    fn keep_replies(&mut self, replies: Vec<u8>, replaying: bool) {
        if self.role == Role::Authoritative && !replaying {
            self.replies.extend(replies);
        }
    }
}

fn hash_bytes(h: &mut Sha256, bytes: Option<&[u8]>) {
    match bytes {
        None => h.update([0]),
        Some(bytes) => {
            h.update([1]);
            h.update((bytes.len() as u64).to_le_bytes());
            h.update(bytes);
        }
    }
}

fn write_osc(out: &mut Vec<u8>, body: &str) {
    out.extend_from_slice(b"\x1b]");
    out.extend_from_slice(body.as_bytes());
    out.extend_from_slice(b"\x1b\\");
}

#[cfg(test)]
mod tests;
