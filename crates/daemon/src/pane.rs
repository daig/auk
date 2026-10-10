//! A pane in the pane table (§14.2): its process, its authoritative replica, the clients that
//! show it, the primary-replica policy (§15.5) and the shell's command marks (§5.3).

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use auk_mirror::msg::{encode_blob, encode_output, encode_snapshot};
use auk_mirror::{ClientFacts, DaemonMsg, Message, PaneId};
use auk_replica::{Answer, Delivery, Event, Item, Record, Replica, Snapshot, TspFacts};
use auk_term::Mark;
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};

use crate::client::{ClientId, ClientTx};
use crate::pty::{Master, TtyMode};
use crate::tsp::{Ages, parse_view_event};

/// How long a chunk sequence may wait for its next chunk (§7.1).
pub const CHUNK_TIMEOUT: Duration = Duration::from_secs(1);

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
    /// An agent pane (§5.1): a command pane running `agent_command` (default `omp`) through the
    /// login shell, which windows badge with the agent icon.
    Agent { line: String },
    /// A plugin block (§9.5): its kind `<plugin>.<block>`, its launch args and its last `save`
    /// (JSON text), which a restore starts it from.
    Block {
        block: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        saved: Option<String>,
    },
}

impl PaneKind {
    /// The kind every window sees in the layout (§5.1).
    pub fn tag(&self) -> auk_mirror::layout::PaneTag {
        use auk_mirror::layout::PaneTag;
        match self {
            PaneKind::Shell => PaneTag::Shell,
            PaneKind::Command { .. } => PaneTag::Command,
            PaneKind::Agent { .. } => PaneTag::Agent,
            PaneKind::Block { .. } => PaneTag::Block,
        }
    }

    /// The command line a command or agent pane runs.
    pub fn line(&self) -> Option<&str> {
        match self {
            PaneKind::Command { line } | PaneKind::Agent { line } => Some(line),
            PaneKind::Shell | PaneKind::Block { .. } => None,
        }
    }
}

/// One `C`…`D` span (§5.3): the daemon measures the duration itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandMark {
    /// The command line, as the shell reported it at `C` (alias-expanded when the shell
    /// reports that).
    pub cmdline: Option<String>,
    /// The line as typed, when the shell reports it too (Auk's zsh integration).
    #[serde(default)]
    pub typed: Option<String>,
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
            Mark::CommandStart { cmdline, typed } => {
                if self.running {
                    self.finish(None);
                }
                self.running = true;
                self.at_prompt = false;
                self.marks.push(CommandMark {
                    cmdline: cmdline.clone(),
                    typed: typed.clone(),
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

/// One write to a pane's PTY, in the order the single writer (§14.2) makes them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PtyWrite {
    /// Client input: keys, pastes.
    Input(Vec<u8>),
    /// Terminal answers (DA1, CPR, …).
    Answer(Vec<u8>),
    /// One TSP reply or event: written only through the mode gate (§12.1).
    Tsp(Vec<u8>),
}

/// What the mode gate let through and held back (§12.1), for diagnostics.
#[derive(Debug, Default)]
pub struct GateStats {
    pub written: AtomicU64,
    /// Dropped because the tty was canonical or echoing.
    pub dropped: AtomicU64,
}

/// The pane's PTY while its process runs.
#[derive(Debug)]
pub struct PaneIo {
    pub master: Arc<Master>,
    /// The pane socket's server (§11.2); it stops with the process.
    pub pane_socket: Option<tokio::task::AbortHandle>,
    /// The single writer to the PTY (§14.2): client input and the replica's answers go through
    /// this queue, in order.
    pub input: mpsc::UnboundedSender<PtyWrite>,
    pub gate: Arc<GateStats>,
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
    /// The facts TSP sees: the primary's, else the last primary's (§15.5).
    facts: ClientFacts,
    changed: watch::Sender<u64>,
    /// When ages arrived, for snapshots (§7.13).
    ages: Ages,
    /// A chunk sequence being joined: its size and when it last grew (§7.1).
    chunk: Option<(usize, Instant)>,
    /// Bumped when the pane's process is replaced (`surface-play`): output and exits of an older
    /// process are ignored.
    pub generation: u64,
    /// The pane runs `surface-play`'s stand-in program.
    pub stand_in: bool,
    /// The plugin host: host events, lens captures, and a block pane's worker (§9).
    pub plugin: Option<crate::plugins::PaneLink>,
    /// The resume variables set since the last prompt (§5.4, D19), for the state file.
    pub resume: crate::persist::ResumeVars,
    /// The pane stays in the layout when its program exits 0 (`--keep-open`, §5.1).
    pub keep_open: bool,
    /// A close was asked for: the pane leaves the layout when its process ends, whatever its
    /// status.
    pub closing: bool,
    /// A block pane whose block couldn't start shows a card with Retry (§9.5).
    pub block_failed: Option<crate::plugins::FailedBlock>,
}

impl Pane {
    pub fn new(
        id: PaneId,
        kind: PaneKind,
        spawn_cwd: PathBuf,
        window_key: String,
        replica: Replica,
    ) -> Self {
        let mut pane = Pane {
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
            facts: ClientFacts::default(),
            changed: watch::channel(0).0,
            ages: Ages::default(),
            chunk: None,
            generation: 0,
            stand_in: false,
            plugin: None,
            resume: Default::default(),
            keep_open: false,
            closing: false,
            block_failed: None,
        };
        // No client shows the pane yet.
        pane.apply_primary_facts();
        pane
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
        self.plugin_after(&events, None);
        self.after_tsp();
        self.last_output = Instant::now();
        self.bump();
    }

    /// Bookkeeping after the replica's TSP state may have changed: ages for snapshots, and the
    /// chunk timeout.
    fn after_tsp(&mut self) {
        let changes = self.replica.take_tsp_changes();
        if !changes.is_empty() {
            self.ages.observe(self.replica.tsp(), &changes);
        }
        self.chunk = match (self.replica.tsp().chunk_pending(), self.chunk) {
            (None, _) => None,
            (Some(n), Some((m, at))) if n == m => Some((m, at)),
            (Some(n), _) => Some((n, Instant::now())),
        };
    }

    /// Whether a chunk sequence has waited past the timeout; the daemon then sequences
    /// [`Record::ChunkTimeout`].
    ///
    /// Only the program's silence counts: while output is still queued in the PTY or held by the
    /// reader, the sequence hasn't timed out.
    ///
    /// A chunk still arriving counts as a chunk: a 64 KiB chunk can take longer than the timeout
    /// to reach the replica when the machine is loaded, so the clock also waits for 1 s with no
    /// output at all.
    pub fn chunk_expired(&self) -> bool {
        self.chunk
            .is_some_and(|(_, at)| at.elapsed() >= CHUNK_TIMEOUT)
            && self.last_output.elapsed() >= CHUNK_TIMEOUT
            && !self
                .io
                .as_ref()
                .is_some_and(|io| io.master.output_pending())
    }

    /// The snapshot at the current position, ages rewritten to now (§15.3).
    pub fn snapshot(&self) -> Snapshot {
        let ages = &self.ages;
        self.replica
            .snapshot_with(&mut |s, node, props| ages.rewrite(s, node, props))
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
        let resize = matches!(record, Record::Resize { .. }).then(|| record.clone());
        let msg = DaemonMsg::Record {
            pane: self.id,
            seq,
            record,
        };
        self.broadcast(Bytes::from(msg.to_bytes()));
        self.on_events(&events);
        self.plugin_after(&events, resize.as_ref());
        self.after_tsp();
        self.bump();
    }

    /// Sends the replica's answers to the PTY writer: terminal answers as they are, TSP through
    /// the mode gate.
    fn forward_replies(&mut self) {
        let replies = self.replica.take_replies();
        let Some(io) = &self.io else {
            // A block pane's answers go to its worker (§9.5).
            for answer in replies {
                self.block_input(answer.bytes().to_vec());
            }
            return;
        };
        for answer in replies {
            let _ = io.input.send(match answer {
                Answer::Term(bytes) => PtyWrite::Answer(bytes),
                Answer::Tsp(bytes) => PtyWrite::Tsp(bytes),
            });
        }
    }

    /// An ack from a client (§7.10): only the primary's counts.
    pub fn client_ack(&mut self, client: ClientId, sf: &str, s: u64) {
        if self.primary != Some(client) {
            return;
        }
        self.replica.ack(sf, s);
        self.forward_replies();
    }

    /// A view event from a client showing the pane (§15.4), checked against the documents and
    /// written subject to §7.7 and the mode gate. Pointer input makes the client primary.
    pub fn client_view_event(
        &mut self,
        client: ClientId,
        sf: &str,
        event: &serde_json::Value,
    ) -> Result<Delivery, String> {
        if !self.viewers.contains_key(&client) {
            return Err(format!("the client doesn't show pane {}", self.id));
        }
        let event = parse_view_event(event)?;
        self.touch(client);
        let result = self.replica.view_event(sf, event);
        self.forward_replies();
        result.map_err(|r| format!("refused: {r:?}"))
    }

    fn on_events(&mut self, events: &[Event]) {
        crate::events::pane_events(self.id, events);
        for event in events {
            self.resume.on_event(event);
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
            Some(io) => io.input.send(PtyWrite::Input(bytes)).is_ok(),
            None => self.block_input(bytes),
        }
    }

    /// The mode gate's counts: TSP messages written and dropped.
    pub fn gate_stats(&self) -> Option<(u64, u64)> {
        self.io.as_ref().map(|io| {
            (
                io.gate.written.load(Ordering::Relaxed),
                io.gate.dropped.load(Ordering::Relaxed),
            )
        })
    }

    /// Whether the pane leaves the layout now that its program ended with `status` (§5.1): a
    /// program that exits 0 closes its pane unless it was opened with `--keep-open`; any other
    /// status keeps the pane open, showing how it ended, until it is closed. A pane being closed,
    /// and a plugin block, always leave.
    pub fn closes_on_exit(&self, status: i32) -> bool {
        self.closing
            || matches!(self.kind, PaneKind::Block { .. })
            || (status == 0 && !self.keep_open)
    }

    /// The process ended.
    pub fn on_exit(&mut self, status: i32) {
        self.io = None;
        self.push_record(Record::Exit { status });
        self.exit = Some(status);
        self.plugin_exited(status);
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

    pub(crate) fn broadcast(&self, frame: Bytes) {
        for viewer in self.viewers.values() {
            viewer.tx.send(frame.clone());
        }
    }

    // Viewers and the primary replica (§15.5).

    /// Starts showing the pane to a client: queues a snapshot at the current position, then
    /// everything after it. The client ranks below every client that has been active here.
    pub fn add_viewer(&mut self, client: ClientId, tx: ClientTx, facts: ClientFacts) {
        let mut frame = Vec::new();
        encode_snapshot(&mut frame, self.id, &self.snapshot());
        tx.send(Bytes::from(frame));
        // Snapshots don't carry blobs; the window gets the ones its documents name (§7.12).
        let named = self.replica.tsp().referenced_blobs();
        self.send_blobs(&tx, named.iter().map(String::as_str));
        self.send_lens_views(&tx);
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

    /// Sends the blobs among `ids` that the pane holds (in memory or on disk), each as a
    /// [`DaemonMsg::Blob`]. Ids it doesn't hold are skipped: they may still arrive in the stream.
    fn send_blobs<'a>(&self, tx: &ClientTx, ids: impl Iterator<Item = &'a str>) {
        for id in ids {
            if let Some(blob) = self.replica.tsp().blobs().load(&id.to_ascii_lowercase()) {
                let mut frame = Vec::new();
                encode_blob(
                    &mut frame,
                    self.id,
                    &blob.id,
                    blob.mime.as_deref(),
                    &blob.bytes,
                );
                tx.send(Bytes::from(frame));
            }
        }
    }

    /// A window asks for blobs its documents name and it lacks (§7.12).
    pub fn client_blobs(&self, client: ClientId, ids: &[String]) {
        if let Some(v) = self.viewers.get(&client) {
            self.send_blobs(&v.tx, ids.iter().map(String::as_str));
        }
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
        self.apply_primary_facts();
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

    /// Facts to answer with before any client is primary: a new pane takes those of the client
    /// that will be, so a program querying colors as it starts sees that client's theme.
    pub fn seed_facts(&mut self, facts: ClientFacts) {
        if self.primary.is_none() {
            self.facts = facts;
            self.apply_primary_facts();
        }
    }

    /// The facts the pane answers with: its primary's, or the last ones it had.
    pub fn facts(&self) -> &ClientFacts {
        &self.facts
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
            // Still update `visible`: the last viewer may have gone.
            self.apply_primary_facts();
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

    /// Client facts from the primary (§15.5): the terminal's answers and TSP's hello fields and
    /// events. With no client attached, the last primary's facts stay, and the pane is hidden.
    fn apply_primary_facts(&mut self) {
        if let Some(viewer) = self.primary.and_then(|c| self.viewers.get(&c)) {
            self.facts = viewer.facts.clone();
        }
        self.replica.set_facts(self.facts.term_facts());
        let f = &self.facts;
        let tsp = TspFacts {
            cell: (f64::from(f.cell_px[0]), f64::from(f.cell_px[1])),
            dark: f.dark,
            reduce_motion: f.reduce_motion,
            hour12: f.hour12,
            visible: !self.viewers.is_empty(),
        };
        if *self.replica.tsp().facts() != tsp {
            self.replica.set_tsp_facts(tsp);
            self.forward_replies();
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
            snapshot: self.snapshot(),
        }
    }

    /// How long since the last output.
    pub fn idle_for(&self) -> Duration {
        self.last_output.elapsed()
    }
}

/// Answers for color and size queries until a client supplies its own: those of
/// `ClientFacts::default()`, which the TSP hello's facts start from too (light).
pub fn default_facts() -> auk_term::Facts {
    ClientFacts::default().term_facts()
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
                typed: None,
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
        s.on_mark(
            &Mark::CommandStart {
                cmdline: None,
                typed: None,
            },
            None,
            0,
        );
        s.on_mark(&Mark::PromptStart, None, 1);
        assert!(!s.running);
        assert_eq!(s.marks[0].status, None);
        assert!(s.marks[0].duration_ms.is_some());
    }
}
