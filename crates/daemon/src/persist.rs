//! The state file (§16) and restore after a daemon restart (§5.4).
//!
//! `<state>/daemon.state` is one JSON document ([`DaemonState`]): the host settings, the
//! sessions and layout, and per live pane its kind (with a plugin block's kind, args and last
//! `save`), cwd, size, window key, the pane's snapshot (§15.3, which carries the closed surfaces
//! and the lens records with their last views), the shell's command marks and the program's
//! resume variables (D19). The same document, plus the inherited descriptors, is what an
//! in-place upgrade hands the new daemon ([`crate::upgrade`]).
//!
//! **When it is written.** A checkpointer looks at a cheap fingerprint of that state every
//! [`TICK`]. It writes once the state has stopped changing for a tick, and at least every
//! [`MAX_INTERVAL`] while it keeps changing, so a crash loses at most that interval. It is also
//! written when the daemon stops (SIGTERM, SIGINT, `shutdown`) and on the control command
//! `checkpoint`. Writes are atomic: a temporary file, fsync, rename, fsync of the directory.
//!
//! **Restore** ([`Daemon::restore`]) runs at start, after plugins load (§4.1):
//! - a shell or command pane comes back as a new login shell in its saved cwd, over its old
//!   scrollback. The replica is rebuilt from the snapshot, then [`Record::Restore`] closes its
//!   inline and flow surfaces and command blocks as restored (read-only, past the prompt rule)
//!   and drops screen surfaces; the modes a full-screen program left on are reset, and the new
//!   shell starts below the old content;
//! - a pane whose program set `resume` (or omp's `omp_session_file`) since the last prompt runs
//!   that command at the new shell's first prompt (D19);
//! - a plugin block restarts from its last `save` through the plugin host
//!   ([`Daemon::restore_block`]); one whose block type no Ready plugin defines is dropped;
//! - view state isn't saved, so nothing of it comes back.

use std::collections::BTreeMap;
use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use auk_mirror::PaneId;
use auk_mirror::layout::Layout;
use auk_replica::{Event, Record, Replica, Role, Settings};
use auk_term::Mark;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::pane::{CommandMark, Pane, PaneKind, PaneRecord, default_facts};
use crate::server::{Daemon, lock};
use crate::tsp;

/// The state file's name in the state dir (Tern's, §16).
pub const STATE_FILE: &str = "daemon.state";

/// The state file's format version.
pub const FORMAT: u32 = 1;

/// How often the checkpointer looks for changes.
pub const TICK: Duration = Duration::from_millis(500);

/// The longest a changing state goes unwritten.
pub const MAX_INTERVAL: Duration = Duration::from_secs(5);

/// How long a restored pane waits for its shell's first prompt to run a resume command.
const RESUME_WAIT: Duration = Duration::from_secs(30);

/// What a restored shell writes into its old screen before the new shell starts: leave the
/// alternate screen, soft-reset (DECSTR), and turn off the mouse, focus, paste, cursor-key and
/// keypad modes and kitty keyboard flags a full-screen program may have left on.
const RESET_MODES: &[u8] = b"\x1b[?1049l\x1b[!p\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1004l\
\x1b[?1005l\x1b[?1006l\x1b[?1015l\x1b[?2004l\x1b[?1l\x1b>\x1b[=0;1u\x1b[0 q";

/// The whole state file.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DaemonState {
    pub format: u32,
    /// The Auk release that wrote it.
    pub auk: String,
    /// When it was written, in milliseconds since the Unix epoch.
    pub saved_ms: u64,
    pub next_pane: PaneId,
    pub settings: Settings,
    pub layout: Layout,
    pub panes: Vec<SavedPane>,
    /// In-place upgrades this daemon has been through (§15.7).
    #[serde(default)]
    pub upgrades: u64,
}

/// One live pane in the state file.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedPane {
    /// Kind, cwd, window key, size and snapshot (whose bytes are in `bytes`).
    #[serde(flatten)]
    pub record: PaneRecord,
    /// The snapshot's bytes (§15.3), base64.
    #[serde(with = "b64")]
    pub bytes: Vec<u8>,
    #[serde(default)]
    pub shell: SavedShell,
    /// The resume variables the program set since the last prompt (D19).
    #[serde(default)]
    pub vars: ResumeVars,
    /// An upgrade's inherited process: its pid and PTY master descriptor. Never in the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<Inherited>,
}

impl SavedPane {
    /// The record with the snapshot's bytes back in place.
    pub fn full_record(&self) -> PaneRecord {
        let mut record = self.record.clone();
        record.snapshot.bytes = self.bytes.clone();
        record
    }
}

/// A process an upgrade hands over (§15.7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inherited {
    pub pid: u32,
    pub fd: i32,
}

/// The shell integration's state (§5.3) worth keeping.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SavedShell {
    #[serde(default)]
    pub marks: Vec<CommandMark>,
    #[serde(default)]
    pub prompts: u64,
    #[serde(default)]
    pub at_prompt: bool,
    #[serde(default)]
    pub running: bool,
}

/// The user variables a restore relaunches with (D19), as set since the shell's last prompt:
/// a prompt means the program that set them has ended. An empty or missing value unsets one
/// (omp sends `SetUserVar=omp_session_file` with no value when its session is cleared).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ResumeVars {
    /// `resume`: a command line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<String>,
    /// omp's session file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omp_session_file: Option<String>,
}

impl ResumeVars {
    /// Follows the pane's events.
    pub fn on_event(&mut self, event: &Event) {
        match event {
            Event::UserVar { name, value } => {
                let value =
                    (!value.is_empty()).then(|| String::from_utf8_lossy(value).into_owned());
                match name.as_str() {
                    "resume" => self.resume = value,
                    "omp_session_file" => self.omp_session_file = value,
                    _ => {}
                }
            }
            Event::Mark(Mark::PromptStart) => *self = ResumeVars::default(),
            _ => {}
        }
    }

    /// The command a restored pane runs in its new shell: `resume` as it is, else omp resuming
    /// its session file when that file exists.
    pub fn command(&self) -> Option<String> {
        if let Some(line) = self
            .resume
            .as_deref()
            .map(str::trim)
            .filter(|l| !l.is_empty())
        {
            return Some(line.to_owned());
        }
        let file = self.omp_session_file.as_deref()?;
        if !Path::new(file).is_file() {
            return None;
        }
        let quoted = shlex::try_quote(file).ok()?;
        Some(format!("omp --resume={quoted}"))
    }
}

/// The checkpointer's bookkeeping, and an upgrade's listening sockets.
#[derive(Debug, Default)]
pub struct Persist {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    /// The fingerprint of the state last written.
    written: Option<u64>,
    /// The last checkpoint.
    last: Option<Checkpoint>,
    checkpoints: u64,
    upgrades: u64,
    /// The raw descriptors of the daemon socket and control endpoint listeners.
    listeners: Option<(i32, i32)>,
}

#[derive(Clone, Debug, Serialize)]
struct Checkpoint {
    saved_ms: u64,
    bytes: usize,
    /// Each pane's stream position in it.
    panes: BTreeMap<PaneId, u64>,
    took_ms: u64,
}

impl Persist {
    /// Remembers the listening sockets an upgrade passes on.
    pub(crate) fn set_listeners(&self, socket: i32, control: i32) {
        lock(&self.inner).listeners = Some((socket, control));
    }

    pub(crate) fn listeners(&self) -> Option<(i32, i32)> {
        lock(&self.inner).listeners
    }

    pub(crate) fn upgrades(&self) -> u64 {
        lock(&self.inner).upgrades
    }

    pub(crate) fn set_upgrades(&self, n: u64) {
        lock(&self.inner).upgrades = n;
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// The state file in a state dir.
pub fn state_path(state_dir: &Path) -> PathBuf {
    state_dir.join(STATE_FILE)
}

/// Reads a state file.
pub fn read_state(path: &Path) -> io::Result<DaemonState> {
    let text = fs::read(path)?;
    let state: DaemonState =
        serde_json::from_slice(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    if state.format != FORMAT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "state file format {} (this build reads {FORMAT})",
                state.format
            ),
        ));
    }
    Ok(state)
}

/// Writes `bytes` to `path` atomically: a temporary file beside it, fsync, rename, then an fsync
/// of the directory so the rename itself survives a crash.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    if let Err(err) = written.and_then(|()| fs::rename(&tmp, path)) {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    fs::File::open(dir)?.sync_all()
}

impl Daemon {
    /// The state as the file stores it. With `handoff`, each live process pane also names its
    /// pid and PTY master descriptor, for an upgrade.
    pub(crate) fn collect_state(&self, handoff: bool) -> DaemonState {
        let st = self.state();
        let mut panes = Vec::new();
        for pane in st.panes.values() {
            let p = lock(pane);
            if p.exit.is_some() {
                continue;
            }
            let mut record = p.record();
            let bytes = std::mem::take(&mut record.snapshot.bytes);
            let process = match (&p.io, p.pid) {
                (Some(io), Some(pid)) if handoff => Some(Inherited {
                    pid,
                    fd: std::os::fd::AsRawFd::as_raw_fd(&*io.master),
                }),
                _ => None,
            };
            panes.push(SavedPane {
                record,
                bytes,
                shell: SavedShell {
                    marks: p.shell.marks.clone(),
                    prompts: p.shell.prompts,
                    at_prompt: p.shell.at_prompt,
                    running: p.shell.running,
                },
                vars: p.resume.clone(),
                process,
            });
        }
        DaemonState {
            format: FORMAT,
            auk: self.versions.auk.clone(),
            saved_ms: now_ms(),
            next_pane: st.next_pane,
            settings: self.settings(),
            layout: st.layout.clone(),
            panes,
            upgrades: self.persist.upgrades(),
        }
    }

    /// A cheap summary of what the state file would hold: it changes whenever the file would.
    fn fingerprint(&self) -> u64 {
        let mut h = DefaultHasher::new();
        let st = self.state();
        st.next_pane.hash(&mut h);
        serde_json::to_string(&st.layout)
            .unwrap_or_default()
            .hash(&mut h);
        format!("{:?}", self.settings()).hash(&mut h);
        for pane in st.panes.values() {
            let p = lock(pane);
            if p.exit.is_some() {
                continue;
            }
            p.id.hash(&mut h);
            p.replica.seq().hash(&mut h);
            serde_json::to_string(&p.kind)
                .unwrap_or_default()
                .hash(&mut h);
            p.cwd().hash(&mut h);
            p.shell.marks.len().hash(&mut h);
            p.shell
                .marks
                .last()
                .map(|m| (m.status, m.duration_ms))
                .hash(&mut h);
            p.resume.hash(&mut h);
        }
        h.finish()
    }

    /// Writes the state file now.
    pub fn checkpoint(&self) -> io::Result<Value> {
        let started = Instant::now();
        let fingerprint = self.fingerprint();
        let state = self.collect_state(false);
        let bytes = serde_json::to_vec(&state).map_err(io::Error::other)?;
        write_atomic(&state_path(&self.state_dir), &bytes)?;
        let checkpoint = Checkpoint {
            saved_ms: state.saved_ms,
            bytes: bytes.len(),
            panes: state
                .panes
                .iter()
                .map(|p| (p.record.id, p.record.snapshot.seq))
                .collect(),
            took_ms: started.elapsed().as_millis() as u64,
        };
        let mut inner = lock(&self.persist.inner);
        inner.written = Some(fingerprint);
        inner.checkpoints += 1;
        inner.last = Some(checkpoint.clone());
        Ok(json!(checkpoint))
    }

    /// What the control command `state` reports: the state file, the last checkpoint, and the
    /// upgrades this daemon has been through.
    pub fn persist_info(&self) -> Value {
        let inner = lock(&self.persist.inner);
        json!({
            "path": state_path(&self.state_dir),
            "checkpoints": inner.checkpoints,
            "last": inner.last,
            "upgrades": inner.upgrades,
            "pid": std::process::id(),
            "auk": self.versions.auk,
        })
    }

    /// Writes the state file when the daemon stops.
    pub(crate) fn save_on_stop(&self) {
        if let Err(err) = self.checkpoint() {
            eprintln!("auk daemon: writing the state file: {err}");
        }
    }

    /// Runs the checkpointer until the daemon stops.
    pub(crate) fn spawn_checkpointer(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let daemon = self.clone();
        tokio::spawn(async move {
            let mut every = tokio::time::interval(TICK);
            every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut seen: Option<u64> = None;
            let mut last_write = Instant::now();
            loop {
                every.tick().await;
                let d = daemon.clone();
                let Ok(fingerprint) = tokio::task::spawn_blocking(move || d.fingerprint()).await
                else {
                    continue;
                };
                let written = lock(&daemon.persist.inner).written;
                let idle = seen == Some(fingerprint);
                seen = Some(fingerprint);
                if written == Some(fingerprint) {
                    continue;
                }
                if !idle && last_write.elapsed() < MAX_INTERVAL {
                    continue;
                }
                let d = daemon.clone();
                match tokio::task::spawn_blocking(move || d.checkpoint()).await {
                    Ok(Ok(_)) => {}
                    Ok(Err(err)) => eprintln!("auk daemon: checkpoint: {err}"),
                    Err(err) => eprintln!("auk daemon: checkpoint: {err}"),
                }
                last_write = Instant::now();
            }
        })
    }

    /// Restores the panes of the state file at start (§5.4). Runs after plugins load, before
    /// clients are served.
    pub(crate) fn restore(self: &Arc<Self>) {
        let path = state_path(&self.state_dir);
        let state = match read_state(&path) {
            Ok(state) => state,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return,
            Err(err) => {
                eprintln!("auk daemon: not restoring {}: {err}", path.display());
                return;
            }
        };
        self.persist.set_upgrades(state.upgrades);
        self.set_settings(state.settings.clone());
        {
            let mut st = self.state();
            st.layout = state.layout.clone();
            st.next_pane = st.next_pane.max(state.next_pane);
        }
        let mut restored = Vec::new();
        for saved in &state.panes {
            let id = saved.record.id;
            let result = match &saved.record.kind {
                PaneKind::Block { .. } => self.restore_block(&saved.full_record()),
                PaneKind::Shell | PaneKind::Command { .. } => self.restore_shell(saved),
            };
            match result {
                Ok(()) => restored.push(id),
                Err(err) => eprintln!("auk daemon: pane {id} not restored: {err}"),
            }
        }
        self.drop_missing_from_layout(&restored);
        eprintln!(
            "auk daemon: restored {} of {} panes from {}",
            restored.len(),
            state.panes.len(),
            path.display()
        );
    }

    /// Removes panes that didn't come back from the layout.
    pub(crate) fn drop_missing_from_layout(&self, present: &[PaneId]) {
        let mut st = self.state();
        let named: Vec<PaneId> = st.layout.windows.values().flat_map(|w| w.panes()).collect();
        for id in named {
            if !present.contains(&id) {
                st.layout.remove_pane(id);
            }
        }
    }

    /// A shell or command pane comes back as a new login shell in its saved cwd, over its old
    /// scrollback (§5.4).
    fn restore_shell(self: &Arc<Self>, saved: &SavedPane) -> Result<(), String> {
        let record = saved.full_record();
        let id = record.id;
        let cwd = if record.cwd.is_dir() {
            record.cwd.clone()
        } else {
            self.login.home.clone()
        };
        let mut replica = self.replica_from(&record);
        replica.set_facts(default_facts());
        let pane = Arc::new(Mutex::new(Pane::new(
            id,
            PaneKind::Shell,
            cwd.clone(),
            record.window_key.clone(),
            replica,
        )));
        {
            let mut p = lock(&pane);
            p.shell.marks = saved.shell.marks.clone();
            p.plugin = Some(self.plugins.link());
            self.sync_catalog(&mut p);
            p.push_record(Record::Restore);
            p.on_output(RESET_MODES);
            if p.replica.terminal().cursor().col != 0 {
                p.on_output(b"\r\n");
            }
        }
        let (cols, rows) = (record.cols, record.rows);
        let spec = self.spawn_spec(id, &PaneKind::Shell, &record.window_key, &cwd, cols, rows);
        self.start_process(&pane, id, &spec)?;
        {
            let mut st = self.state();
            st.next_pane = st.next_pane.max(id);
            st.panes.insert(id, pane.clone());
        }
        lock(&pane).link_info();
        if let Some(line) = saved.vars.command() {
            eprintln!("auk daemon: pane {id} resumes with `{line}`");
            type_at_first_prompt(pane, line);
        }
        Ok(())
    }

    /// A replica rebuilt from a saved snapshot, with the daemon's TSP engine.
    pub(crate) fn replica_from(&self, record: &PaneRecord) -> Replica {
        let engine = tsp::engine(&record.snapshot.settings, &self.blob_dir());
        Replica::from_snapshot(&record.snapshot, Role::Authoritative, Some(engine))
    }
}

/// Types `line` and Enter at the pane's first prompt, as a user would (D19).
fn type_at_first_prompt(pane: Arc<Mutex<Pane>>, line: String) {
    tokio::spawn(async move {
        let mut changed = lock(&pane).watch();
        let deadline = tokio::time::Instant::now() + RESUME_WAIT;
        loop {
            {
                let mut p = lock(&pane);
                if p.exit.is_some() {
                    return;
                }
                if p.shell.prompts >= 1 {
                    let mut bytes = line.into_bytes();
                    bytes.push(b'\r');
                    p.write_input(bytes);
                    return;
                }
            }
            match tokio::time::timeout_at(deadline, changed.changed()).await {
                Ok(Ok(())) => {}
                _ => {
                    eprintln!("auk daemon: no prompt to resume `{line}` at");
                    return;
                }
            }
        }
    });
}

/// Base64 for byte fields.
mod b64 {
    use base64::Engine as _;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&base64::engine::general_purpose::STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(d)?;
        base64::engine::general_purpose::STANDARD
            .decode(text)
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn var(name: &str, value: &str) -> Event {
        Event::UserVar {
            name: name.into(),
            value: value.as_bytes().to_vec(),
        }
    }

    #[test]
    fn resume_variables_last_until_the_next_prompt() {
        let mut v = ResumeVars::default();
        v.on_event(&var("resume", "vim notes.md"));
        assert_eq!(v.command().as_deref(), Some("vim notes.md"));
        v.on_event(&Event::Mark(Mark::PromptStart));
        assert_eq!(v.command(), None);
        v.on_event(&var("resume", "x"));
        v.on_event(&var("resume", ""));
        assert_eq!(v, ResumeVars::default());
    }

    #[test]
    fn omp_resumes_an_existing_session_file() {
        let dir = std::env::temp_dir().join(format!("auk-persist-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a session.jsonl");
        let mut v = ResumeVars::default();
        v.on_event(&var("omp_session_file", &file.to_string_lossy()));
        // No such file yet: nothing to resume.
        assert_eq!(v.command(), None);
        fs::write(&file, "{}\n").unwrap();
        assert_eq!(
            v.command(),
            Some(format!("omp --resume='{}'", file.display()))
        );
        // `resume` wins.
        v.on_event(&var("resume", "omp --continue"));
        assert_eq!(v.command().as_deref(), Some("omp --continue"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn writes_are_atomic_and_the_state_round_trips() {
        let dir = std::env::temp_dir().join(format!("auk-persist-w-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = state_path(&dir);
        let mut replica = Replica::new(20, 4, Settings::default(), Role::Authoritative);
        replica.push(auk_replica::Item::Output(
            b"hello\r\n\x1b]7;file:///tmp\x07",
        ));
        let snapshot = replica.snapshot();
        let mut record = PaneRecord {
            id: 3,
            kind: PaneKind::Block {
                block: "p.b".into(),
                args: vec!["x".into()],
                saved: Some("{\"n\":1}".into()),
            },
            cwd: "/tmp".into(),
            window_key: "main".into(),
            cols: 20,
            rows: 4,
            snapshot,
        };
        let bytes = std::mem::take(&mut record.snapshot.bytes);
        let state = DaemonState {
            format: FORMAT,
            auk: "0.0.0".into(),
            saved_ms: 1,
            next_pane: 3,
            settings: Settings::default(),
            layout: Layout::default(),
            panes: vec![SavedPane {
                record,
                bytes,
                shell: SavedShell::default(),
                vars: ResumeVars {
                    resume: Some("top".into()),
                    omp_session_file: None,
                },
                process: None,
            }],
            upgrades: 2,
        };
        write_atomic(&path, &serde_json::to_vec(&state).unwrap()).unwrap();
        let back = read_state(&path).unwrap();
        let full = back.panes[0].full_record();
        assert_eq!(full.kind, state.panes[0].record.kind);
        assert_eq!(full.snapshot, replica.snapshot());
        assert_eq!(back.panes[0].vars.resume.as_deref(), Some("top"));
        let rebuilt = Replica::from_snapshot(&full.snapshot, Role::Follower, None);
        assert_eq!(rebuilt.state_hash(), replica.state_hash());
        // No temporary file is left behind.
        let names: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from(STATE_FILE)]);
        fs::remove_dir_all(&dir).unwrap();
    }
}
