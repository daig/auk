//! Host-half workers: one OS thread per plugin, named `plugin <id>`, owning that plugin's VM
//! (§9.2, §20). The daemon talks to a worker only over channels and never runs Lua itself.
//!
//! A worker handles its plugin's requests, events and timer callbacks one at a time, in
//! arrival order; workers run in parallel. Work requested through a cx is applied after the
//! handler returns: effects are flushed to the host's output channel after each handler.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Instant;

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use mlua::{Function, Table, Value};

use crate::api::effects::{Effect, EffectTarget, ToastLevel};
use crate::budget::Budget;
use crate::discovery::Package;
use crate::events::HostEvent;
use crate::spawn::{SpawnPatch, SpawnSpec};
use crate::vm::{CallError, Context, LogSink, Vm, VmOptions};

/// Wakes the daemon's event loop after a worker sends output.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// Something a worker asks of the windows, tagged with its plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostOutput {
    /// The plugin id.
    pub plugin: String,
    /// Which windows receive it.
    pub target: EffectTarget,
    /// The effect (a handler's own, or the toast for a failed handler).
    pub effect: Effect,
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
}

/// How a worker's entry went.
pub(crate) struct LoadReport {
    pub result: Result<(), String>,
    pub jit: bool,
    pub has_spawn: Arc<AtomicBool>,
}

/// The output side every worker of a host shares.
#[derive(Clone)]
pub(crate) struct Outputs {
    pub tx: Sender<HostOutput>,
    pub waker: Option<Waker>,
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
    pub(crate) fn start(
        package: &Package,
        data: PathBuf,
        outputs: Outputs,
        log: LogSink,
    ) -> Result<Starting, String> {
        let (tx, rx) = crossbeam_channel::unbounded();
        let (loaded_tx, loaded) = crossbeam_channel::bounded(1);
        let id = package.manifest.id.clone();
        let package = package.clone();
        let thread = std::thread::Builder::new()
            .name(format!("plugin {id}"))
            .spawn(move || run(package, data, outputs, log, rx, loaded_tx))
            .map_err(|e| format!("cannot start the plugin's worker: {e}"))?;
        Ok(Starting {
            id,
            tx,
            loaded,
            thread,
        })
    }

    /// Queues a host event; never blocks.
    pub(crate) fn send_event(&self, event: Arc<HostEvent>) {
        let _ = self.tx.send(Request::Event(event));
    }

    /// Whether the plugin registered a `spawn` handler.
    pub(crate) fn has_spawn(&self) -> bool {
        self.has_spawn.load(Ordering::Acquire)
    }

    /// Queues the spawn filter; the answer is due by `deadline`.
    pub(crate) fn request_spawn(&self, spec: SpawnSpec, deadline: Instant) -> Pending<SpawnSpec> {
        let (reply, rx) = crossbeam_channel::bounded(1);
        let _ = self.tx.send(Request::Spawn { spec, reply });
        Pending { rx, deadline }
    }

    /// Asks the worker to drop its VM and exit; the acknowledgement is due by `deadline`.
    pub(crate) fn retire(&mut self, deadline: Instant) -> Pending<()> {
        let (done, rx) = crossbeam_channel::bounded(1);
        let _ = self.tx.send(Request::Retire { done });
        // The thread exits on its own; a retiring worker is never joined.
        drop(self.thread.take());
        Pending { rx, deadline }
    }
}

struct Worker {
    vm: Vm,
    id: String,
    name: String,
    outputs: Outputs,
}

fn run(
    package: Package,
    data: PathBuf,
    outputs: Outputs,
    log: LogSink,
    rx: Receiver<Request>,
    loaded: Sender<LoadReport>,
) {
    let manifest = &package.manifest;
    if let Err(error) = std::fs::create_dir_all(&data) {
        tracing::warn!(plugin = %manifest.id, %error, "cannot create the plugin data directory");
    }
    let mut options = VmOptions::for_manifest(Context::Host, manifest, &package.dir, &data);
    options.log = log;
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
    let worker = Worker {
        vm,
        id: manifest.id.clone(),
        name: manifest.name.clone(),
        outputs,
    };
    worker.flush();
    if loaded.send(report).is_err() || !ready {
        return;
    }
    worker.serve(&rx);
}

impl Worker {
    fn serve(&self, rx: &Receiver<Request>) {
        loop {
            let request = match self.vm.next_timer() {
                Some(due) => match rx.recv_deadline(due) {
                    Ok(request) => Some(request),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => return,
                },
                None => match rx.recv() {
                    Ok(request) => Some(request),
                    Err(_) => return,
                },
            };
            match request {
                Some(Request::Event(event)) => self.dispatch(&event),
                Some(Request::Spawn { spec, reply }) => {
                    let spec = self.filter_spawn(spec);
                    let _ = reply.send(spec);
                }
                Some(Request::Retire { done }) => {
                    let _ = done.send(());
                    return;
                }
                None => {}
            }
            self.run_due_timers();
        }
    }

    /// Runs `handler` as `hook`; a failure toasts `target`. Returns false once the hook is
    /// disabled, so the remaining handlers of that hook are skipped.
    fn run_handler<R: mlua::FromLuaMulti>(
        &self,
        hook: &str,
        target: EffectTarget,
        handler: &Function,
        args: impl mlua::IntoLuaMulti,
    ) -> (Option<R>, bool) {
        let outcome = self.vm.call::<R>(hook, Budget::Host, handler, args);
        let result = match outcome {
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
        };
        self.flush();
        result
    }

    fn toast_failure(&self, hook: &str, target: EffectTarget, error: &CallError) {
        self.send(
            target,
            Effect::Toast {
                level: ToastLevel::Error,
                text: format!("Plugin {}: {hook} failed", self.name),
                sub: Some(error.first_line()),
            },
        );
    }

    fn dispatch(&self, event: &HostEvent) {
        let name = event.name();
        let handlers = self.vm.handlers(name);
        if handlers.is_empty() {
            return;
        }
        let target = EffectTarget::Pane(event.pane());
        let lua = self.vm.lua();
        let args = mlua::LuaSerdeExt::to_value(lua, &event.payload())
            .and_then(|payload| Ok((payload, self.vm.effect_cx(target)?)));
        let (payload, cx) = match args {
            Ok(args) => args,
            Err(e) => {
                self.toast_failure(name, target, &CallError::Lua(e));
                self.flush();
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

    fn filter_spawn(&self, mut spec: SpawnSpec) -> SpawnSpec {
        let target = EffectTarget::Host;
        for handler in self.vm.handlers("spawn") {
            let table = match spec_table(&self.vm, &spec) {
                Ok(table) => table,
                Err(e) => {
                    self.toast_failure("spawn", target, &CallError::Lua(e));
                    break;
                }
            };
            let (answer, go_on) = self.run_handler::<Value>("spawn", target, &handler, table);
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
            self.flush();
            if !go_on {
                break;
            }
        }
        spec
    }

    fn run_due_timers(&self) {
        let now = Instant::now();
        while let Some(callback) = self.vm.pop_due_timer(now) {
            // A failing host timer toasts every window of the host.
            self.run_handler::<()>("timer", EffectTarget::Host, &callback, ());
        }
    }

    fn send(&self, target: EffectTarget, effect: Effect) {
        // The channel is unbounded; a send only fails once the host is gone.
        let _ = self.outputs.tx.send(HostOutput {
            plugin: self.id.clone(),
            target,
            effect,
        });
        if let Some(waker) = &self.outputs.waker {
            waker();
        }
    }

    fn flush(&self) {
        for (target, effect) in self.vm.take_effects() {
            self.send(target, effect);
        }
    }
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
