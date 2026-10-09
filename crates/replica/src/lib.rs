//! One pane replica (spec §15.1, §22): the pane's terminal plus the sequenced stream that drives
//! it. The stream is the pane's output bytes with records (§15.2) inserted at exact positions;
//! every item carries a sequence number, and a replica applies items strictly in order.
//!
//! The daemon's replica is authoritative: it numbers the items and is the only one whose answers
//! (DA1, CPR, TSP replies) reach the program (§14.2). Client replicas follow: they apply the same
//! items, start from a snapshot taken at a sequence number (§15.3), and drop every answer.
//!
//! Every replica runs the same TSP engine (`auk-tsp`, §7) on the same stream: TSP strings, the
//! prompt mark, RIS, the alternate screen, cleared screens and ended anchors reach it in stream
//! order, as do the records it depends on (process exit, the chunk timeout, settings). Only the
//! authoritative replica's answers reach the program; acks, view events and client facts come
//! from the daemon on behalf of the pane's primary client (§15.5) and touch no replicated state.
//!
//! Builds for wasm32: the browser client runs the same replica.

use std::collections::BTreeMap;
use std::fmt;

use auk_term::{Config, Mark, Terminal};
pub use auk_tsp::{
    Changes as TspChanges, Config as TspConfig, Delivery, Facts as TspFacts, Refused, Rewrite,
    TspPane, ViewEvent,
};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub use auk_term::Event;

mod lens;
pub use lens::{ANCHOR_BASE, LensBlock, LensNote, LensSnapshot, SF_PREFIX, claim_catalog};

/// The parsing behavior every replica of a host must share (§15.2). Clients whose parser version
/// differs from the daemon's are refused at attach (X9). It names this crate's version, which the
/// whole workspace shares (so it covers `auk-term`, its Unicode width tables and `auk-tsp`), plus
/// a revision bumped whenever parsing changes between releases.
pub const PARSER_VERSION: &str = concat!("auk-replica/", env!("CARGO_PKG_VERSION"), "+r3");

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
    /// The hello advertises the T2 kinds drawn (A23). Off, it lists only T0 and T1 (and no
    /// `aside`), which is how a program's T2 fallback is checked.
    #[serde(default = "on")]
    pub t2_kinds: bool,
}

fn on() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            scrollback: Config::default().scrollback,
            native_surfaces: true,
            command_lenses: true,
            retention: 64 << 20,
            t2_kinds: true,
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
    /// The daemon's 1 s chunk timeout passed (§7.1): the chunk sequence being joined is dropped
    /// here on every replica.
    ChunkTimeout,
    /// The plugin catalog changed (§15.2): lens claims at later command starts use this one.
    /// It carries only what claims read ([`claim_catalog`]).
    Catalog {
        catalog: auk_plugins::Catalog,
    },
    /// The daemon measured a command block's duration, from `C` to `D` (§5.3).
    LensTook {
        block: u64,
        took_ms: u64,
    },
    /// The pane came back from the state file after a daemon restart, with a new process (§5.4):
    /// inline and flow surfaces become closed, read-only and restored, screen surfaces go, and
    /// an earlier exit is forgotten.
    Restore,
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
    /// VT bytes, then the replica's own state as OSC sequences, then TSP, then the bytes of any
    /// sequence the parser is in the middle of (§15.3).
    #[serde(skip)]
    pub bytes: Vec<u8>,
    /// The lens claim catalog, command blocks with their current views, and a running capture
    /// (§15.3).
    #[serde(default, skip_serializing_if = "LensSnapshot::is_empty")]
    pub lens: LensSnapshot,
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

/// An answer for the program, in stream order. The daemon writes terminal answers as they come
/// and TSP messages only through the tty mode gate (§12.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    /// Terminal answers: DA1, CPR, color queries and the like.
    Term(Vec<u8>),
    /// One TSP reply or event: `ESC _ tsp;<r|e>;<json> ESC \`.
    Tsp(Vec<u8>),
}

impl Answer {
    pub fn bytes(&self) -> &[u8] {
        match self {
            Answer::Term(b) | Answer::Tsp(b) => b,
        }
    }
}

/// The TSP engine's replicated config for these settings (§15.2).
pub fn tsp_config(settings: &Settings) -> TspConfig {
    let mut config = TspConfig {
        enabled: settings.native_surfaces,
        retention: settings.retention,
        ..TspConfig::default()
    };
    if !settings.t2_kinds {
        config.kinds.retain(|k| !auk_tsp::T2_KINDS.contains(&k.as_str()));
        config.features.retain(|f| f != "aside");
    }
    config
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
    replies: Vec<Answer>,
    tsp: TspPane,
    lens: lens::Lenses,
}

impl Replica {
    /// An empty replica at stream position 0, with the TSP engine's in-memory blob store.
    pub fn new(cols: u16, rows: u16, settings: Settings, role: Role) -> Self {
        let tsp = TspPane::new(tsp_config(&settings));
        Self::with_tsp(cols, rows, settings, role, tsp)
    }

    /// An empty replica whose TSP engine was built by the caller (the daemon's has the CSS
    /// checker and the disk blob store). Its config must be [`tsp_config`] of `settings`.
    pub fn with_tsp(cols: u16, rows: u16, settings: Settings, role: Role, tsp: TspPane) -> Self {
        let mut term = Terminal::new(cols, rows, settings.term_config());
        // A program can't forge anchor rows; only snapshot replay rebuilds them.
        term.set_accept_anchor_sequences(false);
        Replica {
            term,
            role,
            seq: 0,
            settings,
            exit: None,
            cwd: None,
            user_vars: BTreeMap::new(),
            replies: Vec::new(),
            tsp,
            lens: lens::Lenses::default(),
        }
    }

    /// Rebuilds a replica from a snapshot. Live items continue at `snapshot.seq + 1`. Nothing the
    /// replay would answer is kept (§15.3). `tsp` is the engine to rebuild into (its config is
    /// replaced by the snapshot's settings); `None` builds the default one.
    pub fn from_snapshot(snapshot: &Snapshot, role: Role, tsp: Option<TspPane>) -> Self {
        let settings = snapshot.settings.clone();
        let mut tsp = tsp.unwrap_or_else(|| TspPane::new(tsp_config(&settings)));
        let mut term = Terminal::new(snapshot.cols, snapshot.rows, settings.term_config());
        tsp.set_config(&mut term, tsp_config(&settings));
        let mut replica = Replica::with_tsp(snapshot.cols, snapshot.rows, settings, role, tsp);
        replica.exit = snapshot.exit;
        replica.term.set_accept_anchor_sequences(true);
        replica.tsp.begin_replay();
        let mut events = Vec::new();
        replica.feed(&snapshot.bytes, true, &mut events);
        replica.tsp.end_replay();
        replica.term.set_accept_anchor_sequences(false);
        replica.term.take_replies();
        replica.tsp.take_output();
        replica.replies.clear();
        replica.lens = lens::Lenses::restore(&snapshot.lens);
        // The engine counts the finished command blocks from the start, as the live replica's
        // does (§7.10); the state fits its budget already, so nothing is evicted here.
        replica.enforce_retention();
        replica.seq = snapshot.seq;
        replica
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
        self.enforce_retention();
        self.drain_events(false, &mut events);
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
    pub fn take_replies(&mut self) -> Vec<Answer> {
        std::mem::take(&mut self.replies)
    }

    pub fn role(&self) -> Role {
        self.role
    }

    pub fn terminal(&self) -> &Terminal {
        &self.term
    }

    /// The pane's TSP state (§7): surfaces, documents, the program's hello.
    pub fn tsp(&self) -> &TspPane {
        &self.tsp
    }

    /// A blob the daemon sent outside the stream ([`TspPane::deliver_blob`]): storage, not
    /// replicated state. Returns whether it was stored.
    pub fn deliver_blob(&mut self, id: &str, mime: Option<&str>, bytes: Vec<u8>) -> bool {
        self.tsp.deliver_blob(id, mime, bytes)
    }

    /// What changed in the TSP state since the last call, for views.
    pub fn take_tsp_changes(&mut self) -> TspChanges {
        self.tsp.take_changes()
    }

    /// Client facts for answers (§15.5). Not replicated.
    pub fn set_facts(&mut self, facts: auk_term::Facts) {
        self.term.set_facts(facts);
    }

    /// The primary client's facts for TSP (§15.5): the hello's `cell`, `dark`, `reduceMotion`
    /// and `hour12`, and `theme`, `motion`, `resize` and `visible` events as they change.
    pub fn set_tsp_facts(&mut self, facts: TspFacts) {
        self.tsp.set_facts(&self.term, facts);
        self.collect_tsp(false);
    }

    /// The primary drew surface `sf` up to frame `s` (§7.10): queues the `ack` (and the
    /// first-draw `resize`) for the program.
    pub fn ack(&mut self, sf: &str, s: u64) {
        self.tsp.ack(&self.term, sf, s);
        self.collect_tsp(false);
    }

    /// An event a client built from its view (§15.4), checked against the documents.
    pub fn view_event(&mut self, sf: &str, event: ViewEvent) -> Result<Delivery, Refused> {
        let result = self.tsp.view_event(&self.term, sf, event);
        self.collect_tsp(false);
        result
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
    /// cwd and user variables, and the TSP state. Equal on every platform for equal streams.
    pub fn state_hash(&self) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(b"auk-replica state v1\0");
        h.update(self.term.state_hash());
        h.update(self.seq.to_le_bytes());
        let s = &self.settings;
        h.update((s.scrollback as u64).to_le_bytes());
        h.update([u8::from(s.native_surfaces), u8::from(s.command_lenses)]);
        h.update(s.retention.to_le_bytes());
        if !s.t2_kinds {
            h.update(b"t1");
        }
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
        h.update(self.tsp.state_hash());
        self.lens.hash(&mut h);
        h.finalize().into()
    }

    /// The snapshot at the current stream position (§15.3).
    pub fn snapshot(&self) -> Snapshot {
        self.snapshot_with(&mut |_, _, _| {})
    }

    /// The snapshot, with each TSP node's props passed through `rewrite` (the daemon rewrites
    /// ages to snapshot time, §7.13).
    pub fn snapshot_with(&self, rewrite: &mut Rewrite<'_>) -> Snapshot {
        // The terminal's state leaves the parser in the ground state, so the replica's own
        // sequences and TSP's parse as such; the sequence the parser is in the middle of comes
        // last, and the live stream continues it.
        let mut bytes = self.term.snapshot_state();
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
        bytes.extend(self.tsp.snapshot_with(rewrite));
        self.term.snapshot_parser(&mut bytes);
        Snapshot {
            seq: self.seq,
            cols: self.term.cols(),
            rows: self.term.rows(),
            settings: self.settings.clone(),
            exit: self.exit,
            bytes,
            lens: self.lens.snapshot(),
        }
    }

    fn apply_record(&mut self, record: &Record, events: &mut Vec<Event>) {
        match record {
            Record::Resize { cols, rows } => self.term.resize(*cols, *rows),
            Record::ClearScrollback => self.term.clear_scrollback(),
            Record::Reset => self.term.reset(),
            Record::Settings { settings } => {
                self.term.set_config(settings.term_config());
                self.settings = settings.clone();
            }
            Record::Exit { status } => self.exit = Some(*status),
            Record::Restore => self.exit = None,
            Record::ChunkTimeout => {}
            Record::Catalog { catalog } => self.lens.catalog = Some(catalog.clone()),
            Record::LensTook { block, took_ms } => {
                if let Some(i) = self.lens.index(*block) {
                    self.lens.blocks[i].took_ms = Some(*took_ms);
                }
            }
        }
        // The process is gone: a running capture ends where it is, unfinished.
        if matches!(record, Record::Exit { .. } | Record::Restore) {
            self.end_capture();
        }
        self.collect_replies(false);
        // A reset clears every surface silently, before the anchors it ended are reported.
        if matches!(record, Record::Reset) {
            self.observe(Event::Reset, false, events);
        }
        // Anchors the record ended come next, then the record's own TSP rule.
        self.drain_events(false, events);
        match record {
            Record::Resize { .. } => self.tsp.on_resize(&self.term),
            Record::Reset => {}
            Record::Settings { settings } => {
                self.tsp.set_config(&mut self.term, tsp_config(settings))
            }
            Record::Exit { .. } => self.tsp.on_exit(&mut self.term),
            Record::ChunkTimeout => self.tsp.expire_chunks(),
            Record::Restore => self.tsp.on_restore(&mut self.term),
            Record::ClearScrollback | Record::Catalog { .. } | Record::LensTook { .. } => {}
        }
        self.collect_tsp(false);
        self.drain_events(false, events);
    }

    fn feed(&mut self, mut bytes: &[u8], replaying: bool, events: &mut Vec<Event>) {
        while !bytes.is_empty() {
            if self.lens.running.is_some() && !replaying {
                let n = self.feed_capture(bytes, events);
                bytes = &bytes[n.min(bytes.len())..];
                continue;
            }
            let (n, event) = self.term.advance(bytes);
            bytes = &bytes[n.min(bytes.len())..];
            match event {
                Some(event) => {
                    // The rest of this byte's events. RIS ends anchors as it resets; surfaces are
                    // cleared silently by the reset, not ended one by one with `gone` (§7.3).
                    let mut batch = vec![event];
                    batch.extend(self.term.take_events());
                    if let Some(i) = batch.iter().position(|e| *e == Event::Reset) {
                        let reset = batch.remove(i);
                        batch.insert(0, reset);
                    }
                    for event in batch {
                        self.observe(event, replaying, events);
                    }
                }
                // The terminal consumes at least one byte unless it stops on an event.
                None if n == 0 => break,
                None => {}
            }
        }
        self.drain_events(replaying, events);
        self.collect_replies(replaying);
    }

    /// Handles events the terminal queued without consuming bytes (a record's, or the rest of a
    /// byte's), until none are left.
    fn drain_events(&mut self, replaying: bool, events: &mut Vec<Event>) {
        while self.term.pending_events() {
            for event in self.term.take_events() {
                self.observe(event, replaying, events);
            }
        }
    }

    fn observe(&mut self, event: Event, replaying: bool, events: &mut Vec<Event>) {
        match event {
            Event::Replies => self.collect_replies(replaying),
            Event::Tsp(payload) => {
                // Answers already queued go first, so replies keep stream order.
                self.collect_replies(replaying);
                self.tsp.on_apc(&mut self.term, &payload);
                self.collect_tsp(replaying);
            }
            event => {
                self.collect_replies(replaying);
                match &event {
                    Event::Cwd(cwd) => self.cwd = Some(cwd.clone()),
                    // An empty value unsets the variable.
                    Event::UserVar { name, value } if value.is_empty() => {
                        self.user_vars.remove(name);
                    }
                    Event::UserVar { name, value } => {
                        self.user_vars.insert(name.clone(), value.clone());
                    }
                    Event::Mark(Mark::PromptStart) => self.tsp.on_prompt(&mut self.term),
                    Event::Mark(Mark::CommandStart { cmdline, typed }) if !replaying => {
                        self.claim(cmdline.as_deref(), typed.as_deref());
                    }
                    Event::AnchorGone(key) if key.0 >= ANCHOR_BASE => self.lens_anchor_gone(*key),
                    Event::Reset => self.tsp.on_reset(&mut self.term),
                    Event::AltScreen(entered) => self.tsp.on_alt_screen(&mut self.term, *entered),
                    Event::ScreenCleared => self.tsp.on_screen_cleared(&mut self.term),
                    Event::AnchorGone(key) => self.tsp.on_anchor_gone(&mut self.term, *key),
                    Event::Title(_) => self.tsp.on_title(),
                    _ => {}
                }
                self.collect_tsp(replaying);
                if !replaying {
                    events.push(event);
                }
            }
        }
    }

    fn collect_replies(&mut self, replaying: bool) {
        let replies = self.term.take_replies();
        if !replies.is_empty() && self.answers(replaying) {
            match self.replies.last_mut() {
                Some(Answer::Term(last)) => last.extend(replies),
                _ => self.replies.push(Answer::Term(replies)),
            }
        }
    }

    fn collect_tsp(&mut self, replaying: bool) {
        let out = self.tsp.take_output();
        if self.answers(replaying) {
            self.replies
                .extend(out.iter().map(|msg| Answer::Tsp(msg.encode())));
        }
    }

    fn answers(&self, replaying: bool) -> bool {
        self.role == Role::Authoritative && !replaying
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
