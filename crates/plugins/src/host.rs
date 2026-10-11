//! The daemon side of the plugin runtime: loading, the catalog, events, the spawn filter, blocks
//! and lens captures.
//!
//! The daemon waits on workers in only two places: while plugins load or reload, and for
//! the spawn filter (at most 50 ms per plugin, counted from when the request is queued).
//! Everything else is a channel send, and worker output arrives on [`PluginHost::outputs`].

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use serde_json::{Map, Value as Json};

use crate::blocks::{BlockOutput, BlockRecord, BlockStart};
use crate::capture::LensLine;
use crate::catalog::{Catalog, Status};
use crate::claim::{Claim, LensRun};
use crate::discovery::{self, PluginSettings};
use crate::events::{HostEvent, PaneDirectory, PaneId};
use crate::lenses::{LensId, LensMsg, LensRecord};
use crate::paths;
use crate::spawn::SpawnSpec;
use crate::vm::{LogSink, tracing_log_sink};
use crate::worker::{HostOutput, Late, Request, Shared, Starting, Waker, WorkerHandle};

/// How long the daemon waits for each plugin's spawn filter.
pub const SPAWN_WAIT: Duration = Duration::from_millis(50);
/// How long a retiring worker gets to let go of its blocks.
pub const RETIRE_WAIT: Duration = Duration::from_secs(3);
/// Lens blocks whose owning plugin the host remembers.
pub const LENS_OWNERS: usize = 4096;

/// Where a host finds plugins, and what it reports to.
#[derive(Clone)]
pub struct HostConfig {
    /// `<config>/plugins/`.
    pub plugins_dir: PathBuf,
    /// `<state>`, which holds `plugin-data/`.
    pub state_dir: PathBuf,
    /// `plugins` and `plugins_disabled`.
    pub settings: PluginSettings,
    /// Called after a worker sends output, to wake the daemon's loop.
    pub waker: Option<Waker>,
    /// Where `tern.log` and `print` go.
    pub log: LogSink,
    /// The host's panes, for `tern.pane.list`.
    pub panes: Option<Arc<dyn PaneDirectory>>,
}

impl std::fmt::Debug for HostConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostConfig")
            .field("plugins_dir", &self.plugins_dir)
            .field("state_dir", &self.state_dir)
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

impl HostConfig {
    /// The standard locations, default settings, and logs to tracing.
    pub fn standard() -> Self {
        HostConfig {
            plugins_dir: paths::plugins_dir(&paths::config_dir()),
            state_dir: paths::state_dir(),
            settings: PluginSettings::default(),
            waker: None,
            log: tracing_log_sink(),
            panes: None,
        }
    }
}

/// A block to start: on a new pane, or restored after a daemon restart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockLaunch {
    /// The pane (kept across a restart).
    pub pane: PaneId,
    /// `<plugin>.<block>`.
    pub kind: String,
    /// Launch arguments.
    pub args: Vec<String>,
    /// The last `save` result (JSON text), or `None` for a new block.
    pub saved: Option<String>,
    /// The pane's directory.
    pub cwd: Option<String>,
    /// Size in cells.
    pub cols: u16,
    /// Size in cells.
    pub rows: u16,
}

/// A worker kept after a reload until its running lens captures finish.
struct Draining {
    worker: WorkerHandle,
    running: usize,
}

/// The host halves of every plugin on this machine.
pub struct PluginHost {
    config: HostConfig,
    /// Ready workers, in id order.
    workers: Vec<WorkerHandle>,
    /// Workers of earlier loads finishing their running lens captures.
    draining: Vec<Draining>,
    catalog: Catalog,
    shared: Shared,
    rx: Receiver<HostOutput>,
    /// Which plugin (and load) owns each lens block, oldest first.
    lens_owners: HashMap<LensId, (String, u64)>,
    lens_order: VecDeque<LensId>,
    /// Lens blocks whose command is still running, by owner load.
    lens_running: HashMap<LensId, u64>,
    /// Bumped by every load.
    generation: u64,
}

impl std::fmt::Debug for PluginHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginHost")
            .field("config", &self.config)
            .field("catalog", &self.catalog)
            .finish_non_exhaustive()
    }
}

impl PluginHost {
    /// Scans the plugins folder and loads every allowed host half: all workers start at once,
    /// each entry gets its 2 s budget, and this waits for each to report.
    pub fn start(config: HostConfig) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        let shared = Shared {
            tx,
            waker: config.waker.clone(),
            log: Arc::clone(&config.log),
            panes: config.panes.clone(),
            blocks: Arc::default(),
        };
        let mut host = PluginHost {
            config,
            workers: Vec::new(),
            draining: Vec::new(),
            catalog: Catalog::default(),
            shared,
            rx,
            lens_owners: HashMap::new(),
            lens_order: VecDeque::new(),
            lens_running: HashMap::new(),
            generation: 0,
        };
        host.load();
        host
    }

    fn load(&mut self) {
        self.generation += 1;
        let scan = discovery::scan(&self.config.plugins_dir);
        let settings = &self.config.settings;
        let mut starting: Vec<(String, Result<Starting, String>)> = Vec::new();
        for package in &scan.packages {
            let m = &package.manifest;
            if m.host.is_none() || !settings.allows(&m.id) {
                continue;
            }
            let data = paths::data_dir(&self.config.state_dir, &m.id);
            let worker = WorkerHandle::start(package, data, self.shared.clone());
            starting.push((m.id.clone(), worker));
        }
        let mut statuses = Vec::with_capacity(starting.len());
        let mut jit = false;
        for (id, worker) in starting {
            let status = match worker {
                Ok(starting) => {
                    let (result, worker_jit) = starting.wait();
                    match result {
                        Ok(handle) => {
                            jit |= worker_jit;
                            self.workers.push(handle);
                            Status::Ready
                        }
                        Err(error) => Status::Failed(error),
                    }
                }
                Err(error) => Status::Failed(error),
            };
            statuses.push((id, status));
        }
        self.catalog = Catalog::build(&scan, settings, jit, |id| {
            statuses.iter().find(|(s, _)| s == id).map_or(
                Status::Failed("the plugin's worker stopped".into()),
                |(_, s)| s.clone(),
            )
        });
        for problem in &self.catalog.problems {
            tracing::warn!(dir = %problem.dir, error = %problem.error, "not a usable plugin");
        }
    }

    /// Retires every worker (each gets [`RETIRE_WAIT`] to let go) and loads again from a fresh
    /// scan. Running blocks move to the new workers and re-run `init` from their last `save`; a
    /// block whose type is gone ends with `no block type KIND on this host`. Running lens captures
    /// finish on the old workers. Returns the new catalog, which the caller resends to every
    /// window if it changed.
    pub fn reload(&mut self) -> &Catalog {
        self.retire_all();
        self.load();
        self.restart_blocks();
        &self.catalog
    }

    fn retire_all(&mut self) {
        let old_generation = self.generation;
        let deadline = Instant::now() + RETIRE_WAIT;
        let mut pending = Vec::new();
        for mut worker in std::mem::take(&mut self.workers) {
            worker.send(Request::DropBlocks);
            let running = self
                .lens_running
                .iter()
                .filter(|(block, g)| {
                    **g == old_generation
                        && self
                            .lens_owners
                            .get(block)
                            .is_some_and(|(p, _)| *p == worker.id)
                })
                .count();
            if running > 0 {
                self.draining.push(Draining { worker, running });
            } else {
                pending.push((worker.id.clone(), worker.retire(deadline)));
            }
        }
        for (id, pending) in pending {
            if pending.wait() == Err(Late::Timeout) {
                tracing::warn!(plugin = %id, "plugin worker didn't retire in time");
            }
        }
    }

    fn restart_blocks(&mut self) {
        let records: Vec<(PaneId, BlockRecord)> = match self.shared.blocks.lock() {
            Ok(b) => b.iter().map(|(p, r)| (*p, r.clone())).collect(),
            Err(_) => return,
        };
        for (pane, record) in records {
            let launch = BlockLaunch {
                pane,
                kind: record.kind.clone(),
                args: record.args.clone(),
                saved: record.saved.clone(),
                cwd: record.cwd.clone(),
                cols: record.cols,
                rows: record.rows,
            };
            if let Err(message) = self.launch(launch, Some(record.generation)) {
                if let Ok(mut b) = self.shared.blocks.lock() {
                    b.remove(&pane);
                }
                let _ = self.shared.tx.send(HostOutput::Block(BlockOutput::Exited {
                    pane,
                    status: 1,
                    message: Some(message),
                }));
                if let Some(waker) = &self.shared.waker {
                    waker();
                }
            }
        }
    }

    /// Replaces the `plugins` and `plugins_disabled` settings; the next [`reload`](Self::reload)
    /// applies them (the daemon re-reads its settings file before each reload).
    pub fn set_settings(&mut self, settings: PluginSettings) {
        self.config.settings = settings;
    }

    /// The catalog every replica receives.
    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    /// Everything workers hand the daemon: effects, pane writes, block output and lens views.
    pub fn outputs(&self) -> &Receiver<HostOutput> {
        &self.rx
    }

    fn worker(&self, plugin: &str) -> Option<&WorkerHandle> {
        self.workers.iter().find(|w| w.id == plugin)
    }

    /// Sends a pane event to every Ready worker. Never blocks.
    pub fn dispatch(&self, event: HostEvent) {
        let event = Arc::new(event);
        for worker in &self.workers {
            worker.send_event(Arc::clone(&event));
        }
    }

    /// Runs the spawn filter: plugins with a `spawn` handler, in id order, each given the
    /// previous result and at most [`SPAWN_WAIT`] from when its request is queued. A late answer
    /// is discarded and the spec stays as it was before that plugin (fail open). Never called for
    /// blocks.
    pub fn filter_spawn(&self, mut spec: SpawnSpec) -> SpawnSpec {
        for worker in self.workers.iter().filter(|w| w.has_spawn()) {
            let pending = worker.request_spawn(spec.clone(), Instant::now() + SPAWN_WAIT);
            match pending.wait() {
                Ok(filtered) => spec = filtered,
                Err(Late::Timeout) => {
                    tracing::warn!(plugin = %worker.id, "plugin spawn filter took over 50 ms; skipped")
                }
                Err(Late::Stopped) => {}
            }
        }
        spec
    }

    // -----------------------------------------------------------------------------------------
    // Blocks

    /// Starts a block pane. Its output arrives as
    /// [`HostOutput::Block`]. Fails with `no block type KIND on this host` when no Ready plugin
    /// defines the kind; the window shows "Could not start the block".
    pub fn start_block(&mut self, launch: BlockLaunch) -> Result<(), String> {
        self.launch(launch, None)
    }

    fn launch(&mut self, launch: BlockLaunch, previous: Option<u32>) -> Result<(), String> {
        let missing = || format!("no block type {} on this host", launch.kind);
        let (entry, block) = self.catalog.block_type(&launch.kind).ok_or_else(missing)?;
        let (plugin, block, title) = (entry.id.clone(), block.id.clone(), block.title.clone());
        let worker = self.worker(&plugin).ok_or_else(missing)?;
        let generation = previous.map_or(1, |g| g + 1);
        let record = BlockRecord {
            kind: launch.kind.clone(),
            args: launch.args.clone(),
            saved: launch.saved.clone(),
            cwd: launch.cwd.clone(),
            cols: launch.cols,
            rows: launch.rows,
            generation,
        };
        if let Ok(mut b) = self.shared.blocks.lock() {
            b.insert(launch.pane, record);
        }
        worker.send(Request::StartBlock(BlockStart {
            pane: launch.pane,
            kind: launch.kind,
            block,
            title,
            plugin,
            args: launch.args,
            saved: launch.saved,
            cwd: launch.cwd,
            cols: launch.cols,
            rows: launch.rows,
            surface: format!("s{generation}"),
            previous_surface: previous.map(|g| format!("s{g}")),
        }));
        Ok(())
    }

    fn block_worker(&self, pane: PaneId) -> Option<&WorkerHandle> {
        let kind = self.shared.blocks.lock().ok()?.get(&pane)?.kind.clone();
        let plugin = kind.split_once('.')?.0.to_owned();
        self.worker(&plugin)
    }

    /// Bytes written to a block pane's input: keys, and the terminal's TSP replies and events.
    pub fn block_input(&self, pane: PaneId, bytes: &[u8]) {
        if let Some(w) = self.block_worker(pane) {
            w.send(Request::BlockInput {
                pane,
                bytes: bytes.to_vec(),
            });
        }
    }

    /// The block pane was resized.
    pub fn block_resize(&self, pane: PaneId, cols: u16, rows: u16) {
        if let Some(w) = self.block_worker(pane) {
            w.send(Request::BlockResize { pane, cols, rows });
        }
    }

    /// The block pane was closed; the worker lets go of it (there is no close handler).
    pub fn block_closed(&self, pane: PaneId) {
        if let Some(w) = self.block_worker(pane) {
            w.send(Request::BlockClosed { pane });
        }
        if let Ok(mut b) = self.shared.blocks.lock() {
            b.remove(&pane);
        }
    }

    /// What the host knows of each running block (kind, args, last save, size).
    pub fn blocks(&self) -> BTreeMap<PaneId, BlockRecord> {
        self.shared
            .blocks
            .lock()
            .map(|b| b.clone())
            .unwrap_or_default()
    }

    // -----------------------------------------------------------------------------------------
    // Lenses

    fn remember(&mut self, block: LensId, plugin: &str) {
        if self
            .lens_owners
            .insert(block, (plugin.to_owned(), self.generation))
            .is_none()
        {
            self.lens_order.push_back(block);
        }
        while self.lens_order.len() > LENS_OWNERS {
            if let Some(old) = self.lens_order.pop_front() {
                self.lens_owners.remove(&old);
            }
        }
    }

    /// The handle of the worker (current or draining) that owns `block`'s capture.
    fn lens_worker(&self, block: LensId) -> Option<&WorkerHandle> {
        let (plugin, generation) = self.lens_owners.get(&block)?;
        if *generation == self.generation {
            return self.worker(plugin);
        }
        self.draining
            .iter()
            .find(|d| d.worker.id == *plugin)
            .map(|d| &d.worker)
    }

    /// A claimed command started: the claiming plugin's lens `open`s a capture.
    pub fn lens_open(&mut self, block: LensId, pane: PaneId, claim: &Claim, run: LensRun) {
        let Some(worker) = self.worker(&claim.plugin) else {
            return;
        };
        worker.send(Request::Lens(LensMsg::Open {
            block,
            pane,
            lens: claim.lens.clone(),
            run,
        }));
        self.remember(block, &claim.plugin);
        self.lens_running.insert(block, self.generation);
    }

    /// Lines that scrolled off the capture's scratch screen.
    pub fn lens_lines(&self, block: LensId, lines: Vec<LensLine>) {
        if let Some(w) = self.lens_worker(block) {
            w.send(Request::Lens(LensMsg::Lines { block, lines }));
        }
    }

    /// The command finished (after the scratch screen's last lines).
    pub fn lens_finish(&mut self, block: LensId, status: i32) {
        if let Some(w) = self.lens_worker(block) {
            w.send(Request::Lens(LensMsg::Finish { block, status }));
        }
        self.capture_done(block);
    }

    /// The capture aborted (it turned interactive, or passed 16 MiB): its state is dropped.
    pub fn lens_abort(&mut self, block: LensId) {
        if let Some(w) = self.lens_worker(block) {
            w.send(Request::Lens(LensMsg::Abort { block }));
        }
        self.capture_done(block);
    }

    fn capture_done(&mut self, block: LensId) {
        let Some(generation) = self.lens_running.remove(&block) else {
            return;
        };
        if generation == self.generation {
            return;
        }
        let Some((plugin, _)) = self.lens_owners.get(&block).cloned() else {
            return;
        };
        if let Some(i) = self.draining.iter().position(|d| d.worker.id == plugin) {
            self.draining[i].running = self.draining[i].running.saturating_sub(1);
            if self.draining[i].running == 0 {
                let mut d = self.draining.remove(i);
                drop(d.worker.retire(Instant::now()));
            }
        }
        // Later events go to the current load, which rehydrates.
        self.lens_owners.remove(&block);
    }

    /// An event from a lens view. When the worker lost the capture's state it is rebuilt from
    /// `record`, provided the current globs still claim the command; otherwise the event is
    /// dropped.
    pub fn lens_event(
        &mut self,
        block: LensId,
        pane: PaneId,
        event: Map<String, Json>,
        record: LensRecord,
    ) {
        let claim = self.catalog.claim_command(&record.line);
        let owner = self.lens_owners.get(&block).map(|(p, _)| p.clone());
        let plugin = match (&owner, &claim) {
            (Some(p), _) => p.clone(),
            (None, Some(c)) => c.plugin.clone(),
            (None, None) => return,
        };
        let rehydrate = claim
            .filter(|c| c.plugin == plugin)
            .map(|c| (c.lens, record));
        let Some(worker) = self.lens_worker(block).or_else(|| self.worker(&plugin)) else {
            return;
        };
        worker.send(Request::Lens(LensMsg::Event {
            block,
            pane,
            event,
            rehydrate,
        }));
        if owner.is_none() {
            self.remember(block, &plugin);
        }
    }
}

impl Drop for PluginHost {
    fn drop(&mut self) {
        // Workers exit when they see the request; nobody waits for them here.
        let deadline = Instant::now();
        for worker in &mut self.workers {
            drop(worker.retire(deadline));
        }
        for d in &mut self.draining {
            drop(d.worker.retire(deadline));
        }
    }
}
