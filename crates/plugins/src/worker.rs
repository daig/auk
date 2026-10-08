//! Host-half workers: one OS thread per plugin, named `plugin <id>`, owning that plugin's VM
//! (§9.2, §20). The daemon talks to a worker only over channels and never runs Lua itself.
//!
//! A worker handles its plugin's requests, events, block input, lens captures and callbacks one
//! at a time, in arrival order; workers run in parallel. Work requested through a cx is applied
//! after the handler returns: effects, pane writes and block requests are flushed after each
//! handler.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Select, Sender};
use mlua::{Function, Table, Value};
use serde_json::Value as Json;

use crate::api::block::{BlockRequest, Geometry};
use crate::api::effects::{Effect, EffectTarget, ToastLevel};
use crate::blocks::{BlockOutput, BlockRecord, BlockRun, BlockStart};
use crate::budget::Budget;
use crate::discovery::Package;
use crate::events::{HostEvent, PaneDirectory, PaneId};
use crate::lenses::{LensCaptures, LensId, LensMsg};
use crate::spawn::{SpawnPatch, SpawnSpec};
use crate::vm::{CallError, Context, LogSink, Vm, VmOptions};

/// Wakes the daemon's event loop after a worker sends output.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// An effect a worker asks of the windows, tagged with its plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectOutput {
    /// The plugin id.
    pub plugin: String,
    /// Which windows receive it.
    pub target: EffectTarget,
    /// The effect (a handler's own, or the toast for a failed handler).
    pub effect: Effect,
}

/// Everything workers hand the daemon, in the order each worker produced it.
#[derive(Debug, Clone, PartialEq)]
pub enum HostOutput {
    /// A toast, open or copy for the windows.
    Effect(EffectOutput),
    /// `tern.pane.write`: type `text` into the pane's PTY (an unknown pane is ignored).
    PaneWrite {
        /// The plugin id.
        plugin: String,
        /// The pane.
        pane: PaneId,
        /// The text.
        text: String,
    },
    /// Output of a running block.
    Block(BlockOutput),
    /// A lens view, sent out of band to every replica; `None` leaves the raw output showing.
    LensView {
        /// The lens block.
        block: LensId,
        /// Its pane.
        pane: PaneId,
        /// The view node, whole.
        view: Option<Json>,
    },
}

impl HostOutput {
    /// The effect, when this is one.
    pub fn effect(&self) -> Option<&EffectOutput> {
        match self {
            HostOutput::Effect(e) => Some(e),
            _ => None,
        }
    }
}

/// The answer to a request, due by a deadline. The daemon never waits past it.
#[derive(Debug)]
pub struct Pending<T> {
    rx: Receiver<T>,
    deadline: Instant,
}

/// Why a [`Pending`] has no answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Late {
    /// The deadline passed first; a later answer is discarded.
    Timeout,
    /// The worker stopped without answering.
    Stopped,
}

impl<T> Pending<T> {
    /// When the answer is due.
    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    /// Blocks until the answer arrives or the deadline passes.
    pub fn wait(&self) -> Result<T, Late> {
        self.rx.recv_deadline(self.deadline).map_err(|e| match e {
            RecvTimeoutError::Timeout => Late::Timeout,
            RecvTimeoutError::Disconnected => Late::Stopped,
        })
    }

    /// The answer, if it has arrived.
    pub fn try_take(&self) -> Option<T> {
        self.rx.try_recv().ok()
    }
}

pub(crate) enum Request {
    Event(Arc<HostEvent>),
    Spawn {
        spec: SpawnSpec,
        reply: Sender<SpawnSpec>,
    },
    Retire {
        done: Sender<()>,
    },
    /// Lets go of every block (the host moved them to a new worker).
    DropBlocks,
    StartBlock(BlockStart),
    BlockInput {
        pane: PaneId,
        bytes: Vec<u8>,
    },
    BlockResize {
        pane: PaneId,
        cols: u16,
        rows: u16,
    },
    BlockClosed {
        pane: PaneId,
    },
    Lens(LensMsg),
}

/// How a worker's entry went.
pub(crate) struct LoadReport {
    pub result: Result<(), String>,
    pub jit: bool,
    pub has_spawn: Arc<AtomicBool>,
}

/// What every worker of a host shares.
#[derive(Clone)]
pub(crate) struct Shared {
    pub tx: Sender<HostOutput>,
    pub waker: Option<Waker>,
    pub log: LogSink,
    pub panes: Option<Arc<dyn PaneDirectory>>,
    /// Running blocks, by pane: the host's record of kind, args and last save.
    pub blocks: Arc<Mutex<BTreeMap<PaneId, BlockRecord>>>,
}

/// The daemon's handle on one worker.
pub(crate) struct WorkerHandle {
    pub id: String,
    tx: Sender<Request>,
    has_spawn: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// A worker that is running its entry.
pub(crate) struct Starting {
    id: String,
    tx: Sender<Request>,
    loaded: Receiver<LoadReport>,
    thread: JoinHandle<()>,
}

impl Starting {
    /// Waits for the entry to finish (allowed by §4.1: the daemon waits while plugins load).
    /// Returns the handle when the plugin is Ready, else the Failed error.
    pub(crate) fn wait(self) -> (Result<WorkerHandle, String>, bool) {
        match self.loaded.recv() {
            Ok(LoadReport {
                result: Ok(()),
                jit,
                has_spawn,
            }) => (
                Ok(WorkerHandle {
                    id: self.id,
                    tx: self.tx,
                    has_spawn,
                    thread: Some(self.thread),
                }),
                jit,
            ),
            Ok(LoadReport {
                result: Err(e),
                jit,
                ..
            }) => (Err(e), jit),
            Err(_) => (Err("the plugin's worker stopped".into()), false),
        }
    }
}

impl WorkerHandle {
    /// Starts the worker for `package`: it creates the data folder and the VM, then runs the
    /// host entry under its 2 s budget.
    pub(crate) fn start(package: &Package, data: PathBuf, shared: Shared) -> Result<Starting, String> {
        let (tx, rx) = crossbeam_channel::unbounded();
        let (loaded_tx, loaded) = crossbeam_channel::bounded(1);
        let id = package.manifest.id.clone();
        let package = package.clone();
        let thread = std::thread::Builder::new()
            .name(format!("plugin {id}"))
            .spawn(move || run(package, data, shared, rx, loaded_tx))
            .map_err(|e| format!("cannot start the plugin's worker: {e}"))?;
        Ok(Starting {
            id,
            tx,
            loaded,
            thread,
        })
    }

    pub(crate) fn send(&self, request: Request) {
        let _ = self.tx.send(request);
    }

    /// Queues a host event; never blocks.
    pub(crate) fn send_event(&self, event: Arc<HostEvent>) {
        self.send(Request::Event(event));
    }

    /// Whether the plugin registered a `spawn` handler.
    pub(crate) fn has_spawn(&self) -> bool {
        self.has_spawn.load(Ordering::Acquire)
    }

    /// Queues the spawn filter; the answer is due by `deadline`.
    pub(crate) fn request_spawn(&self, spec: SpawnSpec, deadline: Instant) -> Pending<SpawnSpec> {
        let (reply, rx) = crossbeam_channel::bounded(1);
        self.send(Request::Spawn { spec, reply });
        Pending { rx, deadline }
    }

    /// Asks the worker to drop its VM and exit; the acknowledgement is due by `deadline`.
    pub(crate) fn retire(&mut self, deadline: Instant) -> Pending<()> {
        let (done, rx) = crossbeam_channel::bounded(1);
        self.send(Request::Retire { done });
        // The thread exits on its own; a retiring worker is never joined.
        drop(self.thread.take());
        Pending { rx, deadline }
    }
}

/// How long undecided key bytes (a lone ESC) wait before they are decided.
const KEY_FLUSH: Duration = Duration::from_millis(30);

pub(crate) struct Worker {
    pub vm: Vm,
    pub id: String,
    pub name: String,
    pub shared: Shared,
    /// Running blocks by pane.
    pub blocks: HashMap<PaneId, BlockRun>,
    /// Lens captures.
    pub lenses: LensCaptures,
}

fn run(
    package: Package,
    data: PathBuf,
    shared: Shared,
    rx: Receiver<Request>,
    loaded: Sender<LoadReport>,
) {
    let manifest = &package.manifest;
    if let Err(error) = std::fs::create_dir_all(&data) {
        tracing::warn!(plugin = %manifest.id, %error, "cannot create the plugin data directory");
    }
    let mut options = VmOptions::for_manifest(Context::Host, manifest, &package.dir, &data);
    options.log = Arc::clone(&shared.log);
    options.panes = shared.panes.clone();
    let vm = match Vm::new(options) {
        Ok(vm) => vm,
        Err(e) => {
            let _ = loaded.send(LoadReport {
                result: Err(CallError::Lua(e).message()),
                jit: false,
                has_spawn: Arc::default(),
            });
            return;
        }
    };
    let entry = manifest.host.as_deref().unwrap_or_default();
    let result = vm.run_entry(entry);
    if let Err(error) = &result {
        tracing::warn!(plugin = %manifest.id, %error, "plugin failed to load");
    }
    let ready = result.is_ok();
    let report = LoadReport {
        result,
        jit: vm.jit(),
        has_spawn: vm.spawn_flag(),
    };
    let mut worker = Worker {
        vm,
        id: manifest.id.clone(),
        name: manifest.name.clone(),
        shared,
        blocks: HashMap::new(),
        lenses: LensCaptures::default(),
    };
    worker.after_handler();
    if loaded.send(report).is_err() || !ready {
        return;
    }
    worker.serve(&rx);
}

impl Worker {
    fn next_deadline(&self) -> Option<Instant> {
        let mut at = self.vm.next_timer();
        let mut consider = |t: Option<Instant>| {
            if let Some(t) = t {
                at = Some(at.map_or(t, |a: Instant| a.min(t)));
            }
        };
        consider(self.lenses.next_view_due());
        for b in self.blocks.values() {
            consider(b.flush_at);
        }
        at
    }

    fn serve(&mut self, rx: &Receiver<Request>) {
        let completions = self.vm.completion_receiver();
        loop {
            let deadline = self.next_deadline();
            let mut select = Select::new();
            let requests = select.recv(rx);
            let done = select.recv(&completions);
            let ready = if self.vm.has_callbacks() {
                select.try_ready().ok()
            } else {
                match deadline {
                    Some(d) => select.ready_deadline(d).ok(),
                    None => Some(select.ready()),
                }
            };
            if ready == Some(requests) {
                match rx.try_recv() {
                    Ok(request) => {
                        if !self.handle(request) {
                            return;
                        }
                    }
                    Err(crossbeam_channel::TryRecvError::Disconnected) => return,
                    Err(crossbeam_channel::TryRecvError::Empty) => {}
                }
            }
            let _ = done;
            self.run_callbacks();
            let now = Instant::now();
            self.flush_keys(now);
            self.run_lens_views(now);
        }
    }

    /// Handles one request; false once the worker should exit.
    fn handle(&mut self, request: Request) -> bool {
        match request {
            Request::Event(event) => self.dispatch(&event),
            Request::Spawn { spec, reply } => {
                let spec = self.filter_spawn(spec);
                let _ = reply.send(spec);
            }
            Request::Retire { done } => {
                let _ = done.send(());
                return false;
            }
            Request::DropBlocks => self.blocks.clear(),
            Request::StartBlock(start) => self.start_block(start),
            Request::BlockInput { pane, bytes } => self.block_input(pane, &bytes),
            Request::BlockResize { pane, cols, rows } => self.block_resize(pane, cols, rows),
            Request::BlockClosed { pane } => {
                self.blocks.remove(&pane);
            }
            Request::Lens(msg) => self.lens(msg),
        }
        true
    }

    /// Runs `handler` as `hook`; a failure toasts `target`. Returns false once the hook is
    /// disabled, so the remaining handlers of that hook are skipped. Does not flush.
    pub(crate) fn call<R: mlua::FromLuaMulti>(
        &self,
        hook: &str,
        target: EffectTarget,
        handler: &Function,
        args: impl mlua::IntoLuaMulti,
    ) -> (Option<R>, bool) {
        let outcome = self.vm.call::<R>(hook, Budget::Host, handler, args);
        match outcome {
            Ok(value) => (Some(value), true),
            Err(CallError::Disabled { .. }) => (None, false),
            Err(error) => {
                let disabled = matches!(error, CallError::Exceeded { .. });
                if !disabled {
                    tracing::warn!(plugin = %self.id, hook, error = %error.message(), "plugin handler failed");
                }
                self.toast_failure(hook, target, &error);
                (None, !disabled)
            }
        }
    }

    /// [`Worker::call`] followed by [`Worker::after_handler`].
    fn run_handler<R: mlua::FromLuaMulti>(
        &mut self,
        hook: &str,
        target: EffectTarget,
        handler: &Function,
        args: impl mlua::IntoLuaMulti,
    ) -> (Option<R>, bool) {
        let result = self.call(hook, target, handler, args);
        self.after_handler();
        result
    }

    pub(crate) fn toast_failure(&self, hook: &str, target: EffectTarget, error: &CallError) {
        self.send(HostOutput::Effect(EffectOutput {
            plugin: self.id.clone(),
            target,
            effect: Effect::Toast {
                level: ToastLevel::Error,
                text: format!("Plugin {}: {hook} failed", self.name),
                sub: Some(error.first_line()),
            },
        }));
    }

    fn dispatch(&mut self, event: &HostEvent) {
        let name = event.name();
        let handlers = self.vm.handlers(name);
        if handlers.is_empty() {
            return;
        }
        let target = EffectTarget::Pane(event.pane());
        let args = self
            .vm
            .to_lua(&event.payload())
            .and_then(|payload| Ok((payload, self.vm.effect_cx(target)?)));
        let (payload, cx) = match args {
            Ok(args) => args,
            Err(e) => {
                self.toast_failure(name, target, &CallError::Lua(e));
                self.after_handler();
                return;
            }
        };
        // Handlers within one plugin share the payload table and the cx.
        for handler in &handlers {
            let (_, go_on) =
                self.run_handler::<()>(name, target, handler, (payload.clone(), cx.clone()));
            if !go_on {
                break;
            }
        }
    }

    fn filter_spawn(&mut self, mut spec: SpawnSpec) -> SpawnSpec {
        let target = EffectTarget::Host;
        for handler in self.vm.handlers("spawn") {
            let table = match spec_table(&self.vm, &spec) {
                Ok(table) => table,
                Err(e) => {
                    self.toast_failure("spawn", target, &CallError::Lua(e));
                    break;
                }
            };
            let (answer, go_on) = self.call::<Value>("spawn", target, &handler, table);
            match answer {
                Some(Value::Nil) | None => {}
                Some(Value::Table(t)) => match spawn_patch(&t) {
                    Ok(patch) => spec.apply(patch),
                    Err(e) => self.toast_failure("spawn", target, &CallError::Lua(e)),
                },
                Some(other) => self.toast_failure(
                    "spawn",
                    target,
                    &CallError::Lua(mlua::Error::runtime(format!(
                        "spawn must return a spec table or nil, got {}",
                        other.type_name()
                    ))),
                ),
            }
            self.after_handler();
            if !go_on {
                break;
            }
        }
        spec
    }

    /// Runs queued callbacks: completed processes and requests, due timers, awaitables.
    fn run_callbacks(&mut self) {
        while let Some(cb) = self.vm.next_callback(Instant::now()) {
            // A failing host callback toasts every window of the host.
            self.run_handler::<()>(cb.hook, EffectTarget::Host, &cb.f, mlua::MultiValue::from_iter(cb.args));
        }
    }

    pub(crate) fn send(&self, output: HostOutput) {
        // The channel is unbounded; a send only fails once the host is gone.
        let _ = self.shared.tx.send(output);
        if let Some(waker) = &self.shared.waker {
            waker();
        }
    }

    /// Applies what handlers asked for: effects, pane writes, and block requests (at most 8
    /// rounds of renders that ask for more).
    pub(crate) fn after_handler(&mut self) {
        let mut rounds = 0;
        loop {
            // Block requests first, so a blob is sent before an effect that names it.
            let requests = self.vm.take_block_requests();
            let more = !requests.is_empty();
            if more {
                rounds += 1;
                if rounds > 8 {
                    tracing::warn!(plugin = %self.id, "plugin keeps re-rendering; requests dropped");
                } else {
                    self.apply_block_requests(requests);
                }
            }
            for (target, effect) in self.vm.take_effects() {
                self.send(HostOutput::Effect(EffectOutput {
                    plugin: self.id.clone(),
                    target,
                    effect,
                }));
            }
            for (pane, text) in self.vm.take_pane_writes() {
                self.send(HostOutput::PaneWrite {
                    plugin: self.id.clone(),
                    pane,
                    text,
                });
            }
            if !more || rounds > 8 {
                break;
            }
        }
    }

    fn apply_block_requests(&mut self, requests: Vec<(PaneId, BlockRequest)>) {
        // Consecutive renders of one block collapse into one.
        let mut last_render: Option<PaneId> = None;
        for (pane, request) in requests {
            if !self.blocks.contains_key(&pane) {
                continue; // the block ended: ignored (the check is per pane)
            }
            match request {
                BlockRequest::Render => {
                    if last_render == Some(pane) {
                        continue;
                    }
                    last_render = Some(pane);
                    self.cycle(pane, true);
                }
                BlockRequest::Save => {
                    last_render = None;
                    self.save(pane);
                }
                BlockRequest::Exit(code) => {
                    last_render = None;
                    self.end_block(pane, code, None);
                }
                BlockRequest::Frame(ops) => {
                    last_render = None;
                    if let Some(run) = self.blocks.get_mut(&pane) {
                        let bytes = run.raw_frame(ops);
                        self.send(HostOutput::Block(BlockOutput::Bytes { pane, bytes }));
                    }
                }
                BlockRequest::Blob { bytes, mime } => {
                    last_render = None;
                    if let Some(run) = self.blocks.get_mut(&pane) {
                        let out = run.blob(&bytes, &mime);
                        self.send(HostOutput::Block(BlockOutput::Bytes { pane, bytes: out }));
                    }
                }
            }
        }
    }

    // -----------------------------------------------------------------------------------------
    // Blocks

    fn start_block(&mut self, start: BlockStart) {
        let pane = start.pane;
        let Some(handlers) = self.vm.block(&start.block) else {
            let message = format!("no block type {} on this host", start.kind);
            self.send(HostOutput::Block(BlockOutput::Exited {
                pane,
                status: 1,
                message: Some(message),
            }));
            return;
        };
        let geometry = std::rc::Rc::new(std::cell::RefCell::new(Geometry {
            cwd: start.cwd.clone(),
            cols: start.cols,
            rows: start.rows,
        }));
        let cx = match self.vm.block_cx(pane, std::rc::Rc::clone(&geometry)) {
            Ok(cx) => cx,
            Err(e) => {
                self.block_failed(pane, &start.block, "init", &CallError::Lua(e));
                return;
            }
        };
        let args = self
            .vm
            .lua()
            .create_sequence_from(start.args.iter().map(String::as_str));
        let saved = match &start.saved {
            Some(text) => serde_json::from_str::<Json>(text)
                .ok()
                .map(|j| self.vm.to_lua(&j)),
            None => None,
        };
        let (args, saved) = match (args, saved.transpose()) {
            (Ok(a), Ok(s)) => (a, s.unwrap_or(Value::Nil)),
            (Err(e), _) | (_, Err(e)) => {
                self.block_failed(pane, &start.block, "init", &CallError::Lua(e));
                return;
            }
        };
        let hook = format!("{}.init", start.block);
        let target = EffectTarget::Pane(pane);
        let state = match self
            .vm
            .call::<Value>(&hook, Budget::Host, &handlers.init, (cx.clone(), args, saved))
        {
            Ok(state) => state,
            Err(error) => {
                self.toast_failure(&hook, target, &error);
                self.block_failed(pane, &start.block, "init", &error);
                self.after_handler();
                return;
            }
        };
        let mut run = BlockRun::new(&start, handlers, state, cx, geometry);
        let opening = run.opening(start.previous_surface.as_deref());
        self.blocks.insert(pane, run);
        self.send(HostOutput::Block(BlockOutput::Bytes {
            pane,
            bytes: opening,
        }));
        self.cycle(pane, true);
        self.after_handler();
    }

    /// `init` raised: the error is printed in the pane, which exits with status 1.
    fn block_failed(&mut self, pane: PaneId, block: &str, handler: &str, error: &CallError) {
        let message = format!("{block}.{handler}: {}", error.message());
        self.blocks.remove(&pane);
        if let Ok(mut blocks) = self.shared.blocks.lock() {
            blocks.remove(&pane);
        }
        self.send(HostOutput::Block(BlockOutput::Exited {
            pane,
            status: 1,
            message: Some(message),
        }));
    }

    fn end_block(&mut self, pane: PaneId, code: i32, message: Option<String>) {
        let Some(mut run) = self.blocks.remove(&pane) else {
            return;
        };
        let bytes = run.closing();
        if let Ok(mut blocks) = self.shared.blocks.lock() {
            blocks.remove(&pane);
        }
        self.send(HostOutput::Block(BlockOutput::Bytes { pane, bytes }));
        self.send(HostOutput::Block(BlockOutput::Exited {
            pane,
            status: code,
            message,
        }));
    }

    /// The render cycle: `view` diffed into one frame, `title` sent when it changed, and (when
    /// `with_save`) `save` stored when its JSON changed.
    pub(crate) fn cycle(&mut self, pane: PaneId, with_save: bool) {
        let Some(run) = self.blocks.get(&pane) else {
            return;
        };
        let target = EffectTarget::Pane(pane);
        let view_hook = format!("{}.view", run.block);
        let view_fn = run.handlers.view.clone();
        let state = run.state.clone();
        let cx = run.cx.clone();
        let (view, _) = self.call::<Value>(&view_hook, target, &view_fn, (state.clone(), cx));
        if let Some(view) = view {
            let parsed = self
                .vm
                .to_json(&view)
                .map_err(|e| format!("view: {e}"))
                .and_then(|json| {
                    tern_sdk::reconcile::Doc::from_json(&json).map_err(|e| e.to_string())
                });
            match parsed {
                Ok(doc) => {
                    if let Some(run) = self.blocks.get_mut(&pane) {
                        let bytes = run.render(doc);
                        if !bytes.is_empty() {
                            self.send(HostOutput::Block(BlockOutput::Bytes { pane, bytes }));
                        }
                    }
                }
                Err(message) => {
                    // The last good view stays on screen.
                    self.toast_failure(
                        &view_hook,
                        target,
                        &CallError::Lua(mlua::Error::runtime(message)),
                    );
                }
            }
        }
        let Some(run) = self.blocks.get(&pane) else {
            return;
        };
        if let Some(title_fn) = run.handlers.title.clone() {
            let hook = format!("{}.title", run.block);
            let (title, _) = self.call::<Option<String>>(&hook, target, &title_fn, state);
            if let Some(Some(title)) = title
                && let Some(run) = self.blocks.get_mut(&pane)
                && let Some(bytes) = run.retitle(&title)
            {
                self.send(HostOutput::Block(BlockOutput::Bytes { pane, bytes }));
            }
        }
        if with_save {
            self.save(pane);
        }
    }

    fn save(&mut self, pane: PaneId) {
        let Some(run) = self.blocks.get(&pane) else {
            return;
        };
        let Some(save_fn) = run.handlers.save.clone() else {
            return;
        };
        let hook = format!("{}.save", run.block);
        let state = run.state.clone();
        let target = EffectTarget::Pane(pane);
        let (value, _) = self.call::<Value>(&hook, target, &save_fn, state);
        let Some(value) = value else { return };
        let text = match self.vm.to_json(&value) {
            Ok(json) => json.to_string(),
            Err(e) => {
                self.toast_failure(&hook, target, &CallError::Lua(mlua::Error::runtime(e)));
                return;
            }
        };
        let Some(run) = self.blocks.get_mut(&pane) else {
            return;
        };
        if run.saved.as_deref() == Some(text.as_str()) {
            return;
        }
        run.saved = Some(text.clone());
        if let Ok(mut blocks) = self.shared.blocks.lock()
            && let Some(record) = blocks.get_mut(&pane)
        {
            record.saved = Some(text.clone());
        }
        self.send(HostOutput::Block(BlockOutput::Saved { pane, saved: text }));
    }

    fn block_input(&mut self, pane: PaneId, bytes: &[u8]) {
        let Some(run) = self.blocks.get_mut(&pane) else {
            return;
        };
        let inputs = run.feed(bytes, Instant::now() + KEY_FLUSH);
        self.handle_block_inputs(pane, inputs);
    }

    fn flush_keys(&mut self, now: Instant) {
        let due: Vec<PaneId> = self
            .blocks
            .iter()
            .filter(|(_, b)| b.flush_at.is_some_and(|t| t <= now))
            .map(|(p, _)| *p)
            .collect();
        for pane in due {
            if let Some(run) = self.blocks.get_mut(&pane) {
                let inputs = run.flush();
                self.handle_block_inputs(pane, inputs);
            }
        }
    }

    fn handle_block_inputs(&mut self, pane: PaneId, inputs: Vec<crate::blocks::BlockInput>) {
        use crate::blocks::BlockInput;
        for input in inputs {
            let Some(run) = self.blocks.get(&pane) else {
                return;
            };
            let target = EffectTarget::Pane(pane);
            let block = run.block.clone();
            let state = run.state.clone();
            let cx = run.cx.clone();
            match input {
                BlockInput::Key(key) => {
                    let Some(key_fn) = run.handlers.key.clone() else {
                        continue;
                    };
                    let table = match key_table(&self.vm, &key) {
                        Ok(t) => t,
                        Err(_) => continue,
                    };
                    let hook = format!("{block}.key");
                    let (ret, _) = self.call::<Value>(&hook, target, &key_fn, (state, table, cx));
                    // Exactly `false` means the key changed nothing: no render, no save.
                    if !matches!(ret, Some(Value::Boolean(false))) {
                        self.cycle(pane, true);
                    }
                    self.after_handler();
                }
                BlockInput::Event(ev) => {
                    let name = ev.get("ev").and_then(Json::as_str).unwrap_or_default();
                    if !matches!(
                        name,
                        "action" | "select" | "activate" | "toggle" | "change" | "focus"
                    ) {
                        continue;
                    }
                    if let Some(event_fn) = run.handlers.event.clone() {
                        let hook = format!("{block}.event");
                        if let Ok(table) = self.vm.to_lua(&Json::Object(ev)) {
                            self.call::<()>(&hook, target, &event_fn, (state, table, cx));
                        }
                    }
                    self.cycle(pane, true);
                    self.after_handler();
                }
            }
        }
    }

    fn block_resize(&mut self, pane: PaneId, cols: u16, rows: u16) {
        let Some(run) = self.blocks.get(&pane) else {
            return;
        };
        {
            let mut g = run.geometry.borrow_mut();
            g.cols = cols;
            g.rows = rows;
        }
        if let Ok(mut blocks) = self.shared.blocks.lock()
            && let Some(record) = blocks.get_mut(&pane)
        {
            record.cols = cols;
            record.rows = rows;
        }
        match run.handlers.resize.clone() {
            Some(resize_fn) => {
                let hook = format!("{}.resize", run.block);
                let args = (run.state.clone(), f64::from(cols), f64::from(rows), run.cx.clone());
                self.call::<()>(&hook, EffectTarget::Pane(pane), &resize_fn, args);
                self.cycle(pane, true);
            }
            // Without a handler the block still re-renders (no save).
            None => self.cycle(pane, false),
        }
        self.after_handler();
    }

    // -----------------------------------------------------------------------------------------
    // Lenses

    fn lens(&mut self, msg: LensMsg) {
        crate::lenses::handle(self, msg);
        self.after_handler();
    }

    fn run_lens_views(&mut self, now: Instant) {
        if crate::lenses::run_due_views(self, now) {
            self.after_handler();
        }
    }
}

fn key_table(vm: &Vm, key: &tern_sdk::keys::Key) -> mlua::Result<Table> {
    let t = vm.lua().create_table()?;
    t.set("name", key.name.as_str())?;
    if let Some(text) = &key.text {
        t.set("text", text.as_str())?;
    }
    t.set("ctrl", key.ctrl)?;
    t.set("alt", key.alt)?;
    t.set("shift", key.shift)?;
    t.set("meta", key.meta)?;
    Ok(t)
}

fn spec_table(vm: &Vm, spec: &SpawnSpec) -> mlua::Result<Table> {
    let lua = vm.lua();
    let t = lua.create_table()?;
    t.set("program", spec.program.as_str())?;
    t.set(
        "args",
        lua.create_sequence_from(spec.args.iter().map(String::as_str))?,
    )?;
    t.set("cwd", spec.cwd.as_str())?;
    let env = lua.create_table()?;
    for (k, v) in &spec.env {
        env.set(k.as_str(), v.as_str())?;
    }
    t.set("env", env)?;
    Ok(t)
}

fn spawn_patch(t: &Table) -> mlua::Result<SpawnPatch> {
    Ok(SpawnPatch {
        program: t.get("program")?,
        args: t.get("args")?,
        cwd: t.get("cwd")?,
        env: t.get("env")?,
    })
}
