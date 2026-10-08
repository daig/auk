//! The plugin host in the daemon (§4.1, §9, §14.2, §15.1–§15.3, §20): host halves load at start,
//! before panes are restored; host events, the spawn filter, block panes and lens captures go to
//! the workers of `auk-plugins`; their output (block bytes, saves, lens views, effects, pane
//! writes) comes back into the pane table.
//!
//! **The event loop never runs Lua.** One thread owns the [`PluginHost`] and handles requests in
//! order; the host itself only trades messages with one worker thread per plugin. Pane readers,
//! clients and the control endpoint send requests and never wait, except:
//! - the spawn filter, before each process pane starts (at most 50 ms per plugin, fail open);
//! - loading and reloading (`auk plugin reload` waits for the new catalog).
//!
//! Another thread applies worker output to the panes as it arrives, with no window open too.
//!
//! **Lenses.** Every replica claims commands itself from the catalog the daemon sequences into
//! each pane's stream ([`Record::Catalog`]); the daemon's replica tells this module what it
//! claimed and captured ([`LensNote`]), which goes to the claiming plugin's worker. Views come
//! back as side messages: kept in the daemon's replica (so snapshots carry them) and sent to
//! every client showing the pane.
//!
//! For the state file (M6): a block pane's kind, args and last `save` are in its [`PaneKind`],
//! and [`Daemon::restore_block`] restarts one; [`Daemon::sync_catalog`] brings restored panes'
//! claim catalog up to date.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use auk_mirror::layout::SplitDir;
use auk_mirror::{DaemonMsg, Effect, Message, PaneId};
use auk_plugins::capture::LensLine;
use auk_plugins::{
    BlockLaunch, BlockOutput, BlockRecord, Catalog, Claim, EffectTarget, HostConfig, HostEvent,
    HostOutput, LensRecord, LensRun, PaneInfo, PaneTable, PluginHost, PluginSettings, SpawnSpec,
    ToastLevel, Watcher,
};
use auk_replica::{LensNote, Record, Replica, Role, claim_catalog};
use bytes::Bytes;
use serde_json::{Map, Value, json};

use crate::client::ClientTx;
use crate::pane::{Pane, PaneKind, default_facts, osc7_path};
use crate::server::{DEFAULT_SIZE, DEFAULT_WINDOW_KEY, Daemon, lock};
use crate::{pty, tsp};

/// The pane-identity variables (§5.2) whose `AUK_` names the spawn filter sees as `TERN_`.
const IDENTITY: [&str; 8] = [
    "IDENTITY",
    "PANE",
    "PANE_SOCKET",
    "WINDOW_KEY",
    "WINDOW_SOCKET",
    "LENSES",
    "COMPLETE",
    "BLOB_DIR",
];

/// The longest the daemon waits for the whole spawn filter: 50 ms per plugin is the rule, and
/// this bounds the wait while a reload holds the plugin thread.
const SPAWN_LIMIT: Duration = Duration::from_secs(10);

/// How long `auk plugin reload` may wait (§9.11: 124 after 60 s).
const RELOAD_LIMIT: Duration = Duration::from_secs(60);

/// A host-unique lens block id: the pane in the high half, the block's number in the low half.
pub fn lens_id(pane: PaneId, block: u64) -> u64 {
    (pane << 32) | (block & 0xffff_ffff)
}

fn lens_parts(id: u64) -> (PaneId, u64) {
    (id >> 32, id & 0xffff_ffff)
}

/// Requests for the plugin thread.
enum Cmd {
    Event(HostEvent),
    Spawn(SpawnSpec, Sender<SpawnSpec>),
    StartBlock(BlockLaunch, Sender<Result<(), String>>),
    BlockInput(PaneId, Vec<u8>),
    BlockResize(PaneId, u16, u16),
    BlockClosed(PaneId),
    LensOpen(u64, PaneId, Box<Claim>, Box<LensRun>),
    LensLines(u64, Vec<LensLine>),
    LensFinish(u64, i32),
    LensAbort(u64),
    LensEvent(u64, PaneId, Map<String, Value>, Box<LensRecord>),
    /// Reloads; the reply says whether the catalog changed. Without a reply (the watcher) the
    /// plugin thread hands the change to a thread of its own: it never takes daemon locks, so a
    /// spawn waiting on the filter under the state lock can't stall it.
    Reload(Option<Sender<(Catalog, bool)>>),
    Blocks(Sender<BTreeMap<PaneId, BlockRecord>>),
}

/// The daemon's side of the plugin host.
#[derive(Debug)]
pub struct Plugins {
    tx: Mutex<Sender<Cmd>>,
    /// The current catalog; replaced by the plugin thread after each reload.
    catalog: Arc<Mutex<Catalog>>,
    /// What `tern.pane.list` reads.
    panes: Arc<PaneTable>,
    plugins_dir: PathBuf,
    /// Taken once the daemon exists: the host, its output and the request queue.
    pending: Mutex<Option<Pending>>,
    watcher: Mutex<Option<Watcher>>,
}

struct Pending {
    host: PluginHost,
    rx: Receiver<Cmd>,
}

impl std::fmt::Debug for Pending {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pending").finish_non_exhaustive()
    }
}

/// What a pane keeps to reach the plugin host.
#[derive(Clone, Debug)]
pub struct PaneLink {
    tx: Sender<Cmd>,
    panes: Arc<PaneTable>,
    /// The last cwd and title reported, so hooks hear only changes.
    cwd: Option<String>,
    title: Option<String>,
}

impl std::fmt::Debug for Cmd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Cmd")
    }
}

/// Where plugins live: `<config>/plugins` (§9.1; `$AUK_CONFIG_DIR` overrides the config dir).
pub fn plugins_dir() -> PathBuf {
    auk_plugins::paths::plugins_dir(&auk_plugins::paths::config_dir())
}

impl Plugins {
    /// Loads every allowed host half (§4.1 start order: before panes are restored). All workers
    /// start at once and this waits for each entry, as §9.3 allows.
    pub fn start(state_dir: PathBuf) -> Plugins {
        let plugins_dir = plugins_dir();
        let panes = Arc::new(PaneTable::default());
        let config = HostConfig {
            plugins_dir: plugins_dir.clone(),
            state_dir,
            settings: PluginSettings::default(),
            waker: None,
            log: Arc::new(|r: &auk_plugins::LogRecord<'_>| {
                eprintln!("plugin {} [{:?}]: {}", r.plugin, r.level, r.message);
            }),
            panes: Some(panes.clone() as Arc<dyn auk_plugins::PaneDirectory>),
        };
        let host = PluginHost::start(config);
        for p in &host.catalog().plugins {
            if let auk_plugins::Status::Failed(e) = &p.status {
                eprintln!("auk daemon: plugin {} failed: {e}", p.id);
            }
        }
        let catalog = Arc::new(Mutex::new(host.catalog().clone()));
        let (tx, rx) = mpsc::channel();
        Plugins {
            tx: Mutex::new(tx),
            catalog,
            panes,
            plugins_dir,
            pending: Mutex::new(Some(Pending { host, rx })),
            watcher: Mutex::new(None),
        }
    }

    /// Starts the plugin thread, the output pump and the folder watcher once the daemon exists.
    pub fn run(daemon: &Arc<Daemon>) {
        let plugins = &daemon.plugins;
        let Some(Pending { host, rx }) = lock(&plugins.pending).take() else {
            return;
        };
        let outputs = host.outputs().clone();
        let weak = Arc::downgrade(daemon);
        let catalog = plugins.catalog.clone();
        let _ = std::thread::Builder::new()
            .name("plugin host".into())
            .spawn(move || serve_host(host, rx, weak, catalog));
        let weak = Arc::downgrade(daemon);
        let _ = std::thread::Builder::new()
            .name("plugin output".into())
            .spawn(move || {
                while let Ok(out) = outputs.recv() {
                    let Some(daemon) = weak.upgrade() else { return };
                    daemon.plugin_output(out);
                }
            });
        let tx = plugins.sender();
        let watcher = Watcher::start(plugins.plugins_dir.clone(), move || {
            let _ = tx.send(Cmd::Reload(None));
        });
        *lock(&plugins.watcher) = Some(watcher);
    }

    fn sender(&self) -> Sender<Cmd> {
        lock(&self.tx).clone()
    }

    fn send(&self, cmd: Cmd) {
        let _ = lock(&self.tx).send(cmd);
    }

    /// The catalog every client receives (§9.4).
    pub fn catalog(&self) -> Catalog {
        lock(&self.catalog).clone()
    }

    /// The catalog's JSON form (`auk plugin list --json`), for `welcome` and `catalog`.
    pub fn catalog_json(&self) -> Value {
        lock(&self.catalog).to_json()
    }

    /// What a new pane links to.
    pub fn link(&self) -> PaneLink {
        PaneLink {
            tx: self.sender(),
            panes: self.panes.clone(),
            cwd: None,
            title: None,
        }
    }

    /// Runs the spawn filter (§9.7) over a process pane's spec: plugins see the pane-identity set
    /// under `TERN_` names, in order, and their answer replaces the program, args, directory and
    /// that set. Fails open.
    pub fn filter_spawn(&self, spec: &mut pty::SpawnSpec) {
        let identity: Vec<(String, String)> = IDENTITY
            .iter()
            .filter_map(|n| {
                spec.env
                    .get(&format!("AUK_{n}"))
                    .map(|v| (format!("TERN_{n}"), v.clone()))
            })
            .collect();
        let before = SpawnSpec {
            program: spec.program.to_string_lossy().into_owned(),
            args: spec
                .args
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect(),
            cwd: spec.cwd.to_string_lossy().into_owned(),
            env: identity,
        };
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::Spawn(before.clone(), tx));
        let Ok(after) = rx.recv_timeout(SPAWN_LIMIT) else {
            return;
        };
        if after == before {
            return;
        }
        spec.program = after.program.into();
        spec.args = after.args.into_iter().map(Into::into).collect();
        spec.cwd = after.cwd.into();
        for n in IDENTITY {
            spec.env.remove(&format!("AUK_{n}"));
        }
        for (name, value) in after.env {
            let name = match name.strip_prefix("TERN_") {
                Some(rest) if IDENTITY.contains(&rest) => format!("AUK_{rest}"),
                _ => name,
            };
            spec.env.insert(name, value);
        }
        if let Some(pwd) = spec.env.get_mut("PWD") {
            *pwd = spec.cwd.to_string_lossy().into_owned();
        }
    }

    /// Asks for a reload and waits for it (`None` if the plugin thread is gone or took over a
    /// minute). A changed catalog has reached every client and pane when this returns.
    pub fn reload(daemon: &Daemon) -> Option<Catalog> {
        let (tx, rx) = mpsc::channel();
        daemon.plugins.send(Cmd::Reload(Some(tx)));
        let (catalog, changed) = rx.recv_timeout(RELOAD_LIMIT).ok()?;
        if changed {
            daemon.catalog_changed(&catalog);
        }
        Some(catalog)
    }

    /// What the host knows of each running block.
    pub fn blocks(&self) -> BTreeMap<PaneId, BlockRecord> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::Blocks(tx));
        rx.recv_timeout(Duration::from_secs(5)).unwrap_or_default()
    }

    fn start_block(&self, launch: BlockLaunch) -> Result<(), String> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::StartBlock(launch, tx));
        rx.recv_timeout(SPAWN_LIMIT)
            .unwrap_or_else(|_| Err("the plugin host didn't answer".into()))
    }
}

/// The plugin thread: owns the host and handles requests one at a time.
fn serve_host(
    mut host: PluginHost,
    rx: Receiver<Cmd>,
    daemon: Weak<Daemon>,
    catalog: Arc<Mutex<Catalog>>,
) {
    while let Ok(cmd) = rx.recv() {
        match cmd {
            Cmd::Event(event) => host.dispatch(event),
            Cmd::Spawn(spec, reply) => {
                let _ = reply.send(host.filter_spawn(spec));
            }
            Cmd::StartBlock(launch, reply) => {
                let _ = reply.send(host.start_block(launch));
            }
            Cmd::BlockInput(pane, bytes) => host.block_input(pane, &bytes),
            Cmd::BlockResize(pane, cols, rows) => host.block_resize(pane, cols, rows),
            Cmd::BlockClosed(pane) => host.block_closed(pane),
            Cmd::LensOpen(block, pane, claim, run) => host.lens_open(block, pane, &claim, *run),
            Cmd::LensLines(block, lines) => host.lens_lines(block, lines),
            Cmd::LensFinish(block, status) => host.lens_finish(block, status),
            Cmd::LensAbort(block) => host.lens_abort(block),
            Cmd::LensEvent(block, pane, event, record) => {
                host.lens_event(block, pane, event, *record)
            }
            Cmd::Reload(reply) => {
                let new = host.reload().clone();
                for p in &new.plugins {
                    if let auk_plugins::Status::Failed(e) = &p.status {
                        eprintln!("auk daemon: plugin {} failed: {e}", p.id);
                    }
                }
                let changed = {
                    let mut c = lock(&catalog);
                    let changed = *c != new;
                    *c = new.clone();
                    changed
                };
                match reply {
                    Some(reply) => {
                        let _ = reply.send((new, changed));
                    }
                    None if changed => {
                        if let Some(d) = daemon.upgrade() {
                            let _ = std::thread::Builder::new()
                                .name("plugin catalog".into())
                                .spawn(move || d.catalog_changed(&new));
                        }
                    }
                    None => {}
                }
            }
            Cmd::Blocks(reply) => {
                let _ = reply.send(host.blocks());
            }
        }
    }
}

/// Where a new block pane goes.
#[derive(Clone, Debug)]
pub enum BlockPlace {
    NewSession { window_key: Option<String> },
    Split { pane: PaneId, dir: SplitDir },
}

/// A request for a block pane (§9.5 "Opening").
#[derive(Clone, Debug)]
pub struct NewBlock {
    /// `<plugin>.<block>`.
    pub kind: String,
    pub args: Vec<String>,
    pub place: BlockPlace,
    pub cwd: Option<PathBuf>,
    pub size: Option<(u16, u16)>,
}

impl Daemon {
    /// A replica for a new pane, with the current claim catalog sequenced first (§15.2).
    fn block_replica(&self, cols: u16, rows: u16) -> Replica {
        let settings = self.settings();
        let mut replica = Replica::with_tsp(
            cols,
            rows,
            settings.clone(),
            Role::Authoritative,
            tsp::engine(&settings, &self.blob_dir()),
        );
        replica.set_facts(default_facts());
        replica
    }

    /// Opens a block pane: a daemon pane with no process whose output is the block worker's TSP
    /// bytes (§9.5, §15.1). Fails with `no block type KIND on this host` when no Ready plugin
    /// defines it.
    pub fn new_block_pane(self: &Arc<Self>, req: NewBlock) -> Result<PaneId, String> {
        if self.plugins.catalog().block_type(&req.kind).is_none() {
            return Err(format!("no block type {} on this host", req.kind));
        }
        let cwd = match &req.cwd {
            Some(dir) if dir.is_dir() => dir.clone(),
            Some(dir) => return Err(format!("{} is not a directory", dir.display())),
            None => self.login.home.clone(),
        };
        let (cols, rows) = req.size.unwrap_or(DEFAULT_SIZE);
        let mut st = self.state();
        let window_key = match &req.place {
            BlockPlace::Split { pane, .. } => st
                .layout
                .window_of(*pane)
                .ok_or_else(|| format!("pane {pane} is not in a window"))?
                .to_owned(),
            BlockPlace::NewSession { window_key } => window_key
                .clone()
                .unwrap_or_else(|| DEFAULT_WINDOW_KEY.to_owned()),
        };
        let id = st.next_pane + 1;
        let kind = PaneKind::Block {
            block: req.kind.clone(),
            args: req.args.clone(),
            saved: None,
        };
        let pane = Arc::new(Mutex::new(Pane::new(
            id,
            kind,
            cwd.clone(),
            window_key.clone(),
            self.block_replica(cols, rows),
        )));
        {
            let mut p = lock(&pane);
            p.plugin = Some(self.plugins.link());
            self.sync_catalog(&mut p);
        }
        match req.place {
            BlockPlace::NewSession { .. } => {
                st.layout.new_session(&window_key, None, id);
            }
            BlockPlace::Split { pane: at, dir } => {
                st.layout.split(at, dir, id).map_err(|e| e.to_string())?;
            }
        }
        st.next_pane = id;
        st.panes.insert(id, pane.clone());
        let viewers: Vec<(u64, ClientTx, auk_mirror::ClientFacts)> = st
            .clients
            .iter()
            .filter(|(_, c)| c.window_key.as_deref() == Some(window_key.as_str()))
            .map(|(id, c)| (*id, c.tx.clone(), c.facts.clone()))
            .collect();
        self.broadcast_layout_of(&st, &window_key);
        drop(st);
        let started = self.plugins.start_block(BlockLaunch {
            pane: id,
            kind: req.kind,
            args: req.args,
            saved: None,
            cwd: Some(cwd.to_string_lossy().into_owned()),
            cols,
            rows,
        });
        if let Err(e) = started {
            let generation = lock(&pane).generation;
            self.pane_exited(id, generation, 1);
            return Err(e);
        }
        let mut p = lock(&pane);
        p.link_info();
        for (client, tx, facts) in viewers {
            p.add_viewer(client, tx, facts);
        }
        Ok(id)
    }

    /// Restarts a block pane from the state file (§5.4, §9.5 "Survival"): same pane id, same
    /// args, from its last `save`, at its last size, on a blank screen. Not placed in the layout
    /// (the restored layout already names it). Fails, and nothing is added, when no Ready plugin
    /// defines the block type.
    pub fn restore_block(self: &Arc<Self>, record: &crate::pane::PaneRecord) -> Result<(), String> {
        let PaneKind::Block {
            block: kind,
            args,
            saved,
        } = &record.kind
        else {
            return Err(format!("pane {} is not a plugin block", record.id));
        };
        if self.plugins.catalog().block_type(kind).is_none() {
            return Err(format!("cannot restore plugin block: no block type {kind} on this host"));
        }
        let pane = Arc::new(Mutex::new(Pane::new(
            record.id,
            record.kind.clone(),
            record.cwd.clone(),
            record.window_key.clone(),
            self.block_replica(record.cols, record.rows),
        )));
        {
            let mut p = lock(&pane);
            p.plugin = Some(self.plugins.link());
            self.sync_catalog(&mut p);
        }
        {
            let mut st = self.state();
            st.next_pane = st.next_pane.max(record.id);
            st.panes.insert(record.id, pane.clone());
        }
        self.plugins.start_block(BlockLaunch {
            pane: record.id,
            kind: kind.clone(),
            args: args.clone(),
            saved: saved.clone(),
            cwd: Some(record.cwd.to_string_lossy().into_owned()),
            cols: record.cols,
            rows: record.rows,
        })?;
        lock(&pane).link_info();
        Ok(())
    }

    /// Brings a pane's claim catalog up to date with a sequenced record (§15.2). New panes get it
    /// at creation; a catalog change reaches every live pane; restored panes should be synced once
    /// they are in the table.
    pub fn sync_catalog(&self, p: &mut Pane) {
        if p.exit.is_some() {
            return;
        }
        let claims = claim_catalog(&self.plugins.catalog());
        let current = p.replica.lens_catalog();
        let empty = claims.plugins.is_empty();
        if current == Some(&claims) || (current.is_none() && empty) {
            return;
        }
        p.push_record(Record::Catalog { catalog: claims });
    }

    /// [`sync_catalog`](Self::sync_catalog) for every pane in the table.
    pub fn sync_catalog_all(&self) {
        let panes: Vec<_> = self.state().panes.values().cloned().collect();
        for pane in panes {
            let mut p = lock(&pane);
            if p.plugin.is_none() {
                p.plugin = Some(self.plugins.link());
            }
            self.sync_catalog(&mut p);
        }
    }

    /// The catalog changed after a reload: every client gets it (§9.3), and every live pane gets
    /// the claim part in its stream.
    fn catalog_changed(&self, catalog: &Catalog) {
        let frame = Bytes::from(
            DaemonMsg::Catalog {
                catalog: catalog.to_json(),
            }
            .to_bytes(),
        );
        for client in self.state().clients.values() {
            client.tx.send(frame.clone());
        }
        self.sync_catalog_all();
    }

    /// Applies one piece of worker output.
    fn plugin_output(&self, out: HostOutput) {
        match out {
            HostOutput::Block(BlockOutput::Bytes { pane, bytes }) => {
                if let Some(p) = self.pane(pane) {
                    let mut p = lock(&p);
                    if p.exit.is_none() {
                        p.on_output(&bytes);
                    }
                }
            }
            HostOutput::Block(BlockOutput::Saved { pane, saved: value }) => {
                if let Some(p) = self.pane(pane)
                    && let PaneKind::Block { saved, .. } = &mut lock(&p).kind
                {
                    *saved = Some(value);
                }
            }
            HostOutput::Block(BlockOutput::Exited {
                pane,
                status,
                message,
            }) => {
                let Some(p) = self.pane(pane) else { return };
                let generation = {
                    let mut p = lock(&p);
                    if p.exit.is_some() {
                        return;
                    }
                    if let Some(message) = message {
                        let text = message.replace('\n', "\r\n");
                        p.on_output(format!("\r\n{text}\r\n").as_bytes());
                    }
                    p.generation
                };
                self.pane_exited(pane, generation, status);
            }
            HostOutput::PaneWrite { pane, text, .. } => {
                if let Some(p) = self.pane(pane) {
                    lock(&p).write_input(text.into_bytes());
                }
            }
            HostOutput::LensView { block, pane, view } => {
                let (_, n) = lens_parts(block);
                if let Some(p) = self.pane(pane) {
                    let mut p = lock(&p);
                    if p.replica.set_lens_view(n, view.clone()) {
                        let msg = DaemonMsg::LensView {
                            pane,
                            block: n.to_string(),
                            node: view.unwrap_or(Value::Null),
                        };
                        p.broadcast(Bytes::from(msg.to_bytes()));
                    }
                }
            }
            HostOutput::Effect(e) => {
                let effect = match e.effect {
                    auk_plugins::Effect::Toast { level, text, sub } => Effect::Toast {
                        level: match level {
                            ToastLevel::Success => "success",
                            ToastLevel::Info => "info",
                            ToastLevel::Error => "error",
                        }
                        .into(),
                        text,
                        sub,
                    },
                    auk_plugins::Effect::Open(target) => Effect::Open { target },
                    auk_plugins::Effect::Copy(text) => Effect::Copy { text },
                };
                match e.target {
                    EffectTarget::Pane(pane) => {
                        let key = self.pane(pane).map(|p| lock(&p).window_key.clone());
                        if let Some(key) = key {
                            self.send_effect(&key, effect);
                        }
                    }
                    EffectTarget::Host => {
                        let frame = Bytes::from(DaemonMsg::Effect(effect).to_bytes());
                        for c in self.state().clients.values() {
                            c.tx.send(frame.clone());
                        }
                    }
                }
            }
        }
    }

    /// A view event on a command block (`lens:<n>`) goes to the lens's worker, with the block's
    /// record for rehydration (§9.6).
    pub(crate) fn lens_view_event(
        &self,
        client: crate::client::ClientId,
        pane: PaneId,
        block: &str,
        event: &Value,
    ) -> Result<(), String> {
        let n: u64 = block.parse().map_err(|_| format!("bad lens block `{block}`"))?;
        let p = self.pane(pane).ok_or_else(|| format!("no pane {pane}"))?;
        let p = lock(&p);
        if !p.has_viewer(client) {
            return Err(format!("the client doesn't show pane {pane}"));
        }
        let b = p
            .replica
            .lens_blocks()
            .iter()
            .find(|b| b.id == n)
            .ok_or_else(|| format!("no lens block {n} in pane {pane}"))?;
        let Value::Object(event) = event.clone() else {
            return Err("a view event is a JSON object".into());
        };
        let record = LensRecord {
            line: b.line.clone(),
            typed: b.typed.clone(),
            cwd: b.cwd.clone(),
            raw: p.replica.lens_raw(n).unwrap_or_default().to_vec(),
            cols: b.cols,
            status: if b.done { Some(b.status.unwrap_or(-1)) } else { None },
        };
        if let Some(link) = &p.plugin {
            let _ = link.tx.send(Cmd::LensEvent(
                lens_id(pane, n),
                pane,
                event,
                Box::new(record),
            ));
        }
        Ok(())
    }

    /// A block pane was closed by a user: the worker lets go (there is no close handler) and
    /// the pane ends. Returns false for other panes.
    pub(crate) fn close_block(&self, id: PaneId) -> bool {
        let Some(pane) = self.pane(id) else {
            return false;
        };
        let generation = {
            let p = lock(&pane);
            if !matches!(p.kind, PaneKind::Block { .. }) || p.exit.is_some() {
                return false;
            }
            if let Some(link) = &p.plugin {
                let _ = link.tx.send(Cmd::BlockClosed(id));
            }
            p.generation
        };
        self.pane_exited(id, generation, 0);
        true
    }

    fn broadcast_layout_of(&self, st: &crate::server::State, key: &str) {
        let window = st.layout.window(key).cloned().unwrap_or_default();
        let frame = Bytes::from(
            DaemonMsg::Layout {
                window_key: key.to_owned(),
                window,
            }
            .to_bytes(),
        );
        for client in st
            .clients
            .values()
            .filter(|c| c.window_key.as_deref() == Some(key))
        {
            client.tx.send(frame.clone());
        }
    }

    /// The control endpoint's plugin commands:
    ///
    /// | Command | Does |
    /// | --- | --- |
    /// | `plugin list` | the catalog (JSON form) |
    /// | `plugin reload` | reloads every host half and answers with the new catalog |
    /// | `block KIND [--window K] [--split TARGET right\|down] [--cwd DIR] [--size CxR] [-- ARG…]` | opens a block pane |
    /// | `blocks` | the running blocks: kind, args, last save, size |
    /// | `lens TARGET` | the pane's command blocks and their views |
    /// | `pane-list` | what `tern.pane.list` returns |
    pub(crate) fn plugin_command(self: &Arc<Self>, cmd: &str, args: &[String]) -> Result<Value, String> {
        match (cmd, args.first().map(String::as_str)) {
            ("plugin", Some("list")) => Ok(self.plugins.catalog_json()),
            ("plugin", Some("reload")) => tokio::task::block_in_place(|| Plugins::reload(self))
                .map(|c| c.to_json())
                .ok_or_else(|| "the session daemon did not answer".into()),
            ("plugin", _) => Err("plugin: expected list or reload".into()),
            ("block", _) => self.cmd_block(args),
            ("blocks", _) => {
                let blocks: BTreeMap<String, Value> = self
                    .plugins
                    .blocks()
                    .into_iter()
                    .map(|(pane, r)| {
                        (pane.to_string(), json!({
                            "kind": r.kind, "args": r.args, "saved": r.saved, "cwd": r.cwd,
                            "cols": r.cols, "rows": r.rows,
                        }))
                    })
                    .collect();
                Ok(json!(blocks))
            }
            ("pane-list", _) => {
                use auk_plugins::PaneDirectory as _;
                let list: Vec<Value> = self
                    .plugins
                    .panes
                    .list()
                    .into_iter()
                    .map(|p| json!({"pane": p.pane, "cwd": p.cwd, "program": p.program, "title": p.title, "busy": p.busy}))
                    .collect();
                Ok(json!(list))
            }
            _ => Err(format!("unknown command `{cmd}`")),
        }
    }

    fn cmd_block(self: &Arc<Self>, args: &[String]) -> Result<Value, String> {
        let (words, rest) = match args.iter().position(|a| a == "--") {
            Some(i) => (&args[..i], args[i + 1..].to_vec()),
            None => (args, Vec::new()),
        };
        let mut kind = None;
        let mut window = None;
        let mut split = None;
        let mut cwd = None;
        let mut size = None;
        let mut it = words.iter();
        while let Some(w) = it.next() {
            let mut value = |name: &str| it.next().cloned().ok_or_else(|| format!("{name} needs a value"));
            match w.as_str() {
                "--window" => window = Some(value("--window")?),
                "--cwd" => cwd = Some(PathBuf::from(value("--cwd")?)),
                "--size" => {
                    let s = value("--size")?;
                    size = Some(
                        s.split_once('x')
                            .and_then(|(c, r)| Some((c.parse::<u16>().ok()?, r.parse::<u16>().ok()?)))
                            .filter(|(c, r)| *c > 0 && *r > 0)
                            .ok_or_else(|| format!("bad --size `{s}`"))?,
                    );
                }
                "--split" => {
                    let target = value("--split")?;
                    let dir = match it.next().map(String::as_str) {
                        Some("right") => SplitDir::Right,
                        Some("down") => SplitDir::Down,
                        other => return Err(format!("--split: expected right or down, got {other:?}")),
                    };
                    let pane: PaneId = target.parse().map_err(|_| format!("bad pane `{target}`"))?;
                    split = Some((pane, dir));
                }
                other if other.starts_with("--") => return Err(format!("block: unknown flag {other}")),
                other if kind.is_none() => kind = Some(other.to_owned()),
                other => return Err(format!("block: unexpected `{other}`")),
            }
        }
        let kind = kind.ok_or("block: expected a block kind (<plugin>.<block>)")?;
        let place = match split {
            Some((pane, dir)) => BlockPlace::Split { pane, dir },
            None => BlockPlace::NewSession { window_key: window },
        };
        let id = self.new_block_pane(NewBlock {
            kind,
            args: rest,
            place,
            cwd,
            size,
        })?;
        Ok(json!({"pane": id}))
    }
}

impl Pane {
    /// After the replica took an item: host events from shell integration (§9.7), the pane
    /// directory, and the replica's lens notes for the plugin host.
    pub(crate) fn plugin_after(&mut self, events: &[auk_replica::Event], record: Option<&Record>) {
        let Some(link) = self.plugin.as_mut() else {
            return;
        };
        let id = self.id;
        let mut changed = false;
        for event in events {
            let host = match event {
                auk_replica::Event::Mark(auk_term::Mark::CommandStart { cmdline, typed }) => {
                    changed = true;
                    let line = typed.clone().or_else(|| cmdline.clone()).unwrap_or_default();
                    let cwd = self
                        .replica
                        .cwd()
                        .and_then(osc7_path)
                        .map(|p| p.to_string_lossy().into_owned());
                    Some(HostEvent::CommandStarted { pane: id, line, cwd })
                }
                auk_replica::Event::Mark(auk_term::Mark::CommandEnd { status }) => {
                    changed = true;
                    let mark = self.shell.marks.last();
                    let line = mark.and_then(|m| m.cmdline.clone()).unwrap_or_default();
                    let took_ms = mark.and_then(|m| m.duration_ms).unwrap_or(0) as f64;
                    Some(HostEvent::CommandFinished {
                        pane: id,
                        line,
                        status: status.unwrap_or(-1),
                        took_ms,
                    })
                }
                auk_replica::Event::Cwd(url) => {
                    let path = osc7_path(url).map(|p| p.to_string_lossy().into_owned());
                    match path {
                        Some(path) if link.cwd.as_deref() != Some(path.as_str()) => {
                            link.cwd = Some(path.clone());
                            changed = true;
                            Some(HostEvent::Cwd { pane: id, path })
                        }
                        _ => None,
                    }
                }
                auk_replica::Event::Title(title) if link.title.as_deref() != Some(title) => {
                    link.title = Some(title.clone());
                    changed = true;
                    Some(HostEvent::Title {
                        pane: id,
                        title: title.clone(),
                    })
                }
                _ => None,
            };
            if let Some(host) = host {
                let _ = link.tx.send(Cmd::Event(host));
            }
        }
        // A block pane's resize reaches its worker.
        if let (Some(Record::Resize { cols, rows }), PaneKind::Block { .. }) = (record, &self.kind) {
            let _ = link.tx.send(Cmd::BlockResize(id, *cols, *rows));
        }
        let notes = self.replica.take_lens_notes();
        let link = self.plugin.clone();
        if let Some(link) = &link {
            for note in notes {
                match note {
                    LensNote::Open {
                        block, claim, run, ..
                    } => {
                        let _ = link.tx.send(Cmd::LensOpen(
                            lens_id(id, block),
                            id,
                            Box::new(claim),
                            Box::new(run),
                        ));
                    }
                    LensNote::Lines { block, lines } => {
                        let _ = link.tx.send(Cmd::LensLines(lens_id(id, block), lines));
                    }
                    LensNote::Finish { block, status } => {
                        let _ = link
                            .tx
                            .send(Cmd::LensFinish(lens_id(id, block), status.unwrap_or(-1)));
                        // The duration the daemon measured joins the block in every replica.
                        if let Some(took_ms) = self.shell.marks.last().and_then(|m| m.duration_ms) {
                            self.push_record(Record::LensTook { block, took_ms });
                        }
                    }
                    LensNote::Drop { block } => {
                        let _ = link.tx.send(Cmd::LensAbort(lens_id(id, block)));
                    }
                }
            }
        }
        if changed {
            self.link_info();
        }
    }

    /// Updates the pane directory (`tern.pane.list`).
    pub(crate) fn link_info(&self) {
        let Some(link) = &self.plugin else { return };
        if self.exit.is_some() {
            link.panes.remove(self.id);
            return;
        }
        let program = match &self.kind {
            PaneKind::Block { block, .. } => block.clone(),
            PaneKind::Command { line } => line.split_whitespace().next().unwrap_or("").to_owned(),
            PaneKind::Shell if self.shell.running => self
                .shell
                .marks
                .last()
                .and_then(|m| m.cmdline.as_deref())
                .and_then(|l| l.split_whitespace().next())
                .unwrap_or("")
                .to_owned(),
            PaneKind::Shell => std::path::Path::new(&self.login_shell())
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        };
        link.panes.upsert(PaneInfo {
            pane: self.id,
            cwd: Some(self.cwd().to_string_lossy().into_owned()),
            program,
            title: self.replica.terminal().title().to_owned(),
            busy: self.shell.running,
        });
    }

    fn login_shell(&self) -> String {
        std::env::var("SHELL").unwrap_or_else(|_| "zsh".into())
    }

    /// The pane's process (or block) ended: `pane_exited` for the hooks.
    pub(crate) fn plugin_exited(&mut self, status: i32) {
        let Some(link) = &self.plugin else { return };
        let _ = link.tx.send(Cmd::Event(HostEvent::PaneExited {
            pane: self.id,
            status,
        }));
        link.panes.remove(self.id);
    }

    /// Input for a pane without a PTY: a block pane's keys and terminal answers go to its
    /// worker (§15.4). Returns false when there is no block to take them.
    pub(crate) fn block_input(&self, bytes: Vec<u8>) -> bool {
        match (&self.kind, &self.plugin, self.exit) {
            (PaneKind::Block { .. }, Some(link), None) => {
                link.tx.send(Cmd::BlockInput(self.id, bytes)).is_ok()
            }
            _ => false,
        }
    }

    /// Sends each command block's current view to a client that just got the snapshot (§15.3
    /// attach order).
    pub(crate) fn send_lens_views(&self, tx: &ClientTx) {
        for b in self.replica.lens_blocks() {
            if let Some(view) = &b.view {
                let msg = DaemonMsg::LensView {
                    pane: self.id,
                    block: b.id.to_string(),
                    node: view.clone(),
                };
                tx.send(Bytes::from(msg.to_bytes()));
            }
        }
    }
}

impl Daemon {
    /// Links panes that joined the table without a plugin link (a restore) and syncs their claim
    /// catalog. Runs on the daemon's ticker.
    pub(crate) fn link_unlinked(&self) {
        let panes: Vec<_> = self.state().panes.values().cloned().collect();
        for pane in panes {
            let mut p = lock(&pane);
            if p.plugin.is_none() && p.exit.is_none() {
                p.plugin = Some(self.plugins.link());
                self.sync_catalog(&mut p);
                p.link_info();
            }
        }
    }
}
