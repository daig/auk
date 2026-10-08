//! A pane in the pane table (§14.2): its process, its authoritative replica, the clients that
//! show it, the primary-replica policy (§15.5) and the shell's command marks (§5.3).

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use auk_mirror::msg::{encode_output, encode_snapshot};
use auk_mirror::{ClientFacts, DaemonMsg, Message, PaneId};
use auk_replica::{Event, Item, Record, Replica, Snapshot};
use auk_term::Mark;
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};

use crate::client::{ClientId, ClientTx};
use crate::pty::{Master, TtyMode};

/// How many finished command marks a pane keeps.
const MAX_MARKS: usize = 1000;

/// What a pane runs (§5.1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PaneKind {
    /// The login shell.
    Shell,
    /// `$SHELL -l -c <line>`.
    Command { line: String },
}

/// One `C`…`D` span (§5.3): the daemon measures the duration itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandMark {
    /// The command line, as the shell reported it at `C`.
    pub cmdline: Option<String>,
    /// The cwd at `C` (the last OSC 7).
    pub cwd: Option<String>,
    /// The buffer line the command's output starts on.
    pub line: usize,
    /// Wall-clock start, in milliseconds since the Unix epoch.
    pub started_ms: u64,
    /// The exit status from `D`, once finished.
    pub status: Option<i32>,
    /// Measured from `C` to `D`, once finished.
    pub duration_ms: Option<u64>,
    #[serde(skip)]
    started: Option<Instant>,
}

/// What the shell integration has reported.
#[derive(Clone, Debug, Default)]
pub struct ShellState {
    /// Finished commands, oldest first, plus the running one last.
    pub marks: Vec<CommandMark>,
    /// A command is running (`C` seen, `D` not yet).
    pub running: bool,
    /// Prompts seen (`A`).
    pub prompts: u64,
    /// The shell is at a prompt: `A` seen and no command started since.
    pub at_prompt: bool,
    /// A line was sent since the last prompt, so `wait --until prompt` waits for the next one.
    pub awaiting_prompt: bool,
}

impl ShellState {
    fn on_mark(&mut self, mark: &Mark, cwd: Option<&str>, line: usize) {
        match mark {
            Mark::PromptStart => {
                if self.running {
                    // A prompt without `D` (the command was interrupted): close the span.
                    self.finish(None);
                }
                self.prompts += 1;
                self.at_prompt = true;
                self.awaiting_prompt = false;
            }
            Mark::PromptEnd => {}
            Mark::CommandStart { cmdline } => {
                if self.running {
                    self.finish(None);
                }
                self.running = true;
                self.at_prompt = false;
                self.marks.push(CommandMark {
                    cmdline: cmdline.clone(),
                    cwd: cwd.map(str::to_owned),
                    line,
                    started_ms: SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map_or(0, |d| d.as_millis() as u64),
                    status: None,
                    duration_ms: None,
                    started: Some(Instant::now()),
                });
                if self.marks.len() > MAX_MARKS {
                    self.marks.remove(0);
                }
            }
            Mark::CommandEnd { status } => {
                if self.running {
                    self.finish(*status);
                }
            }
        }
    }

    fn finish(&mut self, status: Option<i32>) {
        self.running = false;
        if let Some(mark) = self.marks.last_mut() {
            mark.status = status;
            mark.duration_ms = mark.started.map(|s| s.elapsed().as_millis() as u64);
        }
    }
}

/// A client showing the pane.
#[derive(Debug)]
struct Viewer {
    tx: ClientTx,
    /// The size the client would show the pane at.
    size: Option<(u16, u16)>,
    facts: ClientFacts,
}

/// The pane's PTY while its process runs.
#[derive(Debug)]
pub struct PaneIo {
    pub master: Arc<Master>,
    /// The pane socket's server (§11.2); it stops with the process.
    pub pane_socket: Option<tokio::task::AbortHandle>,
    /// The single writer to the PTY (§14.2): client input and the replica's answers go through
    /// this queue, in order.
    pub input: mpsc::UnboundedSender<Vec<u8>>,
}

impl Drop for PaneIo {
    fn drop(&mut self) {
        if let Some(task) = &self.pane_socket {
            task.abort();
        }
    }
}

/// A pane's persistent description: what the state file (§16) stores per pane, with the
/// snapshot alongside.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneRecord {
    pub id: PaneId,
    #[serde(flatten)]
    pub kind: PaneKind,
    /// The cwd to restart in: the last OSC 7 path, else the spawn cwd.
    pub cwd: PathBuf,
    pub window_key: String,
    pub cols: u16,
    pub rows: u16,
    pub snapshot: Snapshot,
}

#[derive(Debug)]
pub struct Pane {
    pub id: PaneId,
    pub kind: PaneKind,
    pub spawn_cwd: PathBuf,
    pub window_key: String,
    pub replica: Replica,
    pub shell: ShellState,
    pub io: Option<PaneIo>,
    pub pid: Option<u32>,
    /// The exit status once the process ended: its code, or 128 plus the signal.
    pub exit: Option<i32>,
    pub last_output: Instant,
    viewers: BTreeMap<ClientId, Viewer>,
    /// Clients by recency of input or focus, most recent last (§15.5).
    activity: Vec<ClientId>,
    primary: Option<ClientId>,
    changed: watch::Sender<u64>,
}

impl Pane {
    pub fn new(
        id: PaneId,
        kind: PaneKind,
        spawn_cwd: PathBuf,
        window_key: String,
        replica: Replica,
    ) -> Self {
        Pane {
            id,
            kind,
            spawn_cwd,
            window_key,
            replica,
            shell: ShellState::default(),
            io: None,
            pid: None,
            exit: None,
            last_output: Instant::now(),
            viewers: BTreeMap::new(),
            activity: Vec::new(),
            primary: None,
            changed: watch::channel(0).0,
        }
    }

    /// A receiver that wakes on every change to the pane: output, marks, exit.
    pub fn watch(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    fn bump(&self) {
        self.changed.send_modify(|n| *n = n.wrapping_add(1));
    }

    /// Feeds PTY output through the authoritative replica, forwards its answers to the PTY and
    /// the output to every viewer.
    pub fn on_output(&mut self, bytes: &[u8]) {
        let (seq, events) = self.replica.push(Item::Output(bytes));
        self.forward_replies();
        let mut frame = Vec::with_capacity(bytes.len() + 32);
        encode_output(&mut frame, self.id, seq, bytes);
        self.broadcast(Bytes::from(frame));
        self.on_events(&events);
        self.last_output = Instant::now();
        self.bump();
    }

    /// Inserts a record into the stream at the current position (§15.2).
    pub fn push_record(&mut self, record: Record) {
        let (seq, events) = self.replica.push(Item::Record(&record));
        self.forward_replies();
        if let Record::Resize { cols, rows } = record
            && let Some(io) = &self.io
            && let Err(err) = io.master.resize(cols, rows)
        {
            eprintln!("pane {}: resize: {err}", self.id);
        }
        let msg = DaemonMsg::Record {
            pane: self.id,
            seq,
            record,
        };
        self.broadcast(Bytes::from(msg.to_bytes()));
        self.on_events(&events);
        self.bump();
    }

    fn forward_replies(&mut self) {
        let replies = self.replica.take_replies();
        if !replies.is_empty()
            && let Some(io) = &self.io
        {
            let _ = io.input.send(replies);
        }
    }

    fn on_events(&mut self, events: &[Event]) {
        for event in events {
            if let Event::Mark(mark) = event {
                let term = self.replica.terminal();
                let line = term.history_len() + usize::from(term.cursor().row);
                let cwd = self.replica.cwd().map(str::to_owned);
                self.shell.on_mark(mark, cwd.as_deref(), line);
            }
        }
    }

    /// Writes client input to the PTY. Returns false when the process has exited.
    pub fn write_input(&mut self, bytes: Vec<u8>) -> bool {
        if bytes.contains(&b'\r') || bytes.contains(&b'\n') {
            self.shell.awaiting_prompt = true;
        }
        match &self.io {
            Some(io) => io.input.send(bytes).is_ok(),
            None => false,
        }
    }

    /// The process ended.
    pub fn on_exit(&mut self, status: i32) {
        self.io = None;
        self.push_record(Record::Exit { status });
        self.exit = Some(status);
        self.broadcast(Bytes::from(
            DaemonMsg::PaneExit {
                pane: self.id,
                status,
            }
            .to_bytes(),
        ));
        self.bump();
    }

    /// The tty's mode, for the TSP mode gate (§12.1). `None` once the process has exited.
    pub fn tty_mode(&self) -> Option<TtyMode> {
        self.io.as_ref().and_then(|io| io.master.tty_mode().ok())
    }

    fn broadcast(&self, frame: Bytes) {
        for viewer in self.viewers.values() {
            viewer.tx.send(frame.clone());
        }
    }

    // Viewers and the primary replica (§15.5).

    /// Starts showing the pane to a client: queues a snapshot at the current position, then
    /// everything after it. The client ranks below every client that has been active here.
    pub fn add_viewer(&mut self, client: ClientId, tx: ClientTx, facts: ClientFacts) {
        let mut frame = Vec::new();
        encode_snapshot(&mut frame, self.id, &self.replica.snapshot());
        tx.send(Bytes::from(frame));
        self.viewers.insert(
            client,
            Viewer {
                tx,
                size: None,
                facts,
            },
        );
        if !self.activity.contains(&client) {
            self.activity.insert(0, client);
        }
        self.update_primary();
    }

    pub fn remove_viewer(&mut self, client: ClientId) {
        self.viewers.remove(&client);
        self.activity.retain(|c| *c != client);
        self.update_primary();
    }

    /// Stops showing the pane to every client (it left the layout).
    pub fn clear_viewers(&mut self) {
        self.viewers.clear();
        self.activity.clear();
        self.primary = None;
    }

    pub fn has_viewer(&self, client: ClientId) -> bool {
        self.viewers.contains_key(&client)
    }

    pub fn viewers(&self) -> impl Iterator<Item = ClientId> + '_ {
        self.viewers.keys().copied()
    }

    /// The client sent input to or focused the pane: it becomes the primary.
    pub fn touch(&mut self, client: ClientId) {
        if !self.viewers.contains_key(&client) {
            return;
        }
        self.activity.retain(|c| *c != client);
        self.activity.push(client);
        self.update_primary();
    }

    pub fn primary(&self) -> Option<ClientId> {
        self.primary
    }

    /// A client's preferred size; it applies while that client is primary.
    pub fn set_client_size(&mut self, client: ClientId, cols: u16, rows: u16) {
        let Some(viewer) = self.viewers.get_mut(&client) else {
            return;
        };
        viewer.size = Some((cols.max(1), rows.max(1)));
        if self.primary == Some(client) {
            self.apply_primary_size();
        }
    }

    pub fn set_client_facts(&mut self, client: ClientId, facts: ClientFacts) {
        let Some(viewer) = self.viewers.get_mut(&client) else {
            return;
        };
        viewer.facts = facts;
        if self.primary == Some(client) {
            self.apply_primary_facts();
        }
    }

    /// Resizes the pane unless it already has this size.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let term = self.replica.terminal();
        if (term.cols(), term.rows()) != (cols, rows) && self.exit.is_none() {
            self.push_record(Record::Resize { cols, rows });
        }
    }

    fn update_primary(&mut self) {
        let next = self
            .activity
            .iter()
            .rev()
            .copied()
            .find(|c| self.viewers.contains_key(c));
        if next == self.primary {
            return;
        }
        let previous = std::mem::replace(&mut self.primary, next);
        for (client, you) in [(previous, false), (next, true)] {
            if let Some(viewer) = client.and_then(|c| self.viewers.get(&c)) {
                viewer.tx.send(Bytes::from(
                    DaemonMsg::Primary { pane: self.id, you }.to_bytes(),
                ));
            }
        }
        self.apply_primary_size();
        self.apply_primary_facts();
    }

    fn apply_primary_size(&mut self) {
        if let Some((cols, rows)) = self
            .primary
            .and_then(|c| self.viewers.get(&c))
            .and_then(|v| v.size)
        {
            self.resize(cols, rows);
        }
    }

    fn apply_primary_facts(&mut self) {
        // With no client attached, the last primary's facts stay (§15.5).
        if let Some(viewer) = self.primary.and_then(|c| self.viewers.get(&c)) {
            let [w, h] = viewer.facts.cell_px;
            let mut facts = default_facts();
            facts.cell_px = (w, h);
            self.replica.set_facts(facts);
        }
    }

    /// The cwd to report and restart in: the last OSC 7 path, else the spawn cwd.
    pub fn cwd(&self) -> PathBuf {
        self.replica
            .cwd()
            .and_then(osc7_path)
            .unwrap_or_else(|| self.spawn_cwd.clone())
    }

    pub fn record(&self) -> PaneRecord {
        let term = self.replica.terminal();
        PaneRecord {
            id: self.id,
            kind: self.kind.clone(),
            cwd: self.cwd(),
            window_key: self.window_key.clone(),
            cols: term.cols(),
            rows: term.rows(),
            snapshot: self.replica.snapshot(),
        }
    }

    /// How long since the last output.
    pub fn idle_for(&self) -> Duration {
        self.last_output.elapsed()
    }
}

/// Host-default answers for color and size queries until a client supplies its own.
pub fn default_facts() -> auk_term::Facts {
    use auk_term::Rgb;
    const PALETTE: [Rgb; 16] = [
        Rgb(0x00, 0x00, 0x00),
        Rgb(0xcd, 0x31, 0x31),
        Rgb(0x0d, 0xbc, 0x79),
        Rgb(0xe5, 0xe5, 0x10),
        Rgb(0x24, 0x72, 0xc8),
        Rgb(0xbc, 0x3f, 0xbc),
        Rgb(0x11, 0xa8, 0xcd),
        Rgb(0xe5, 0xe5, 0xe5),
        Rgb(0x66, 0x66, 0x66),
        Rgb(0xf1, 0x4c, 0x4c),
        Rgb(0x23, 0xd1, 0x8b),
        Rgb(0xf5, 0xf5, 0x43),
        Rgb(0x3b, 0x8e, 0xea),
        Rgb(0xd6, 0x70, 0xd6),
        Rgb(0x29, 0xb8, 0xdb),
        Rgb(0xff, 0xff, 0xff),
    ];
    auk_term::Facts {
        cell_px: (8, 16),
        foreground: Rgb(0x1e, 0x1e, 0x1e),
        background: Rgb(0xff, 0xff, 0xff),
        cursor: Rgb(0x1e, 0x1e, 0x1e),
        palette: PALETTE,
    }
}

/// The path of an OSC 7 `file://host/path` URL, percent-decoded.
pub fn osc7_path(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    let path = &rest[rest.find('/')?..];
    let bytes = percent_decode(path)?;
    Some(PathBuf::from(OsString::from_vec(bytes)))
}

fn percent_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len());
    let mut bytes = s.bytes();
    while let Some(b) = bytes.next() {
        if b == b'%' {
            let hi = (bytes.next()? as char).to_digit(16)?;
            let lo = (bytes.next()? as char).to_digit(16)?;
            out.push((hi * 16 + lo) as u8);
        } else {
            out.push(b);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_osc7_urls() {
        assert_eq!(
            osc7_path("file://mac/Users/me/a%20b"),
            Some(PathBuf::from("/Users/me/a b"))
        );
        assert_eq!(osc7_path("file:///tmp"), Some(PathBuf::from("/tmp")));
        assert_eq!(osc7_path("file://host/%C3%BC"), Some(PathBuf::from("/ü")));
        assert_eq!(osc7_path("http://x/y"), None);
        assert_eq!(osc7_path("file://host/%zz"), None);
    }

    #[test]
    fn marks_measure_commands() {
        let mut s = ShellState::default();
        s.on_mark(&Mark::PromptStart, None, 0);
        assert!(s.at_prompt);
        s.awaiting_prompt = true;
        s.on_mark(
            &Mark::CommandStart {
                cmdline: Some("sleep 1".into()),
            },
            Some("file:///tmp"),
            3,
        );
        assert!(!s.at_prompt && s.running);
        std::thread::sleep(Duration::from_millis(20));
        s.on_mark(&Mark::CommandEnd { status: Some(2) }, None, 5);
        s.on_mark(&Mark::PromptStart, None, 6);
        assert!(s.at_prompt && !s.awaiting_prompt);
        assert_eq!(s.prompts, 2);
        let m = &s.marks[0];
        assert_eq!(m.cmdline.as_deref(), Some("sleep 1"));
        assert_eq!(m.cwd.as_deref(), Some("file:///tmp"));
        assert_eq!((m.line, m.status), (3, Some(2)));
        assert!(m.duration_ms.unwrap() >= 20);
    }

    #[test]
    fn an_interrupted_command_closes_at_the_next_prompt() {
        let mut s = ShellState::default();
        s.on_mark(&Mark::CommandStart { cmdline: None }, None, 0);
        s.on_mark(&Mark::PromptStart, None, 1);
        assert!(!s.running);
        assert_eq!(s.marks[0].status, None);
        assert!(s.marks[0].duration_ms.is_some());
    }
}
