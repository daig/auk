//! Callbacks that run later on the VM's owner thread (§9.2 "No re-entrancy"): `tern.timer`,
//! `tern.sleep`, `tern.process.run` and `tern.fetch` callbacks, and awaitables.
//!
//! Work on other threads (processes, requests) sends a [`Completion`] over a channel and wakes
//! the owner. The owner drains completions and due timers into one queue and runs it oldest first,
//! one callback at a time ([`crate::Vm::next_callback`]).

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Arc;
use std::time::Instant;

use crossbeam_channel::{Receiver, Sender};
use mlua::{Function, Value};

/// Wakes a VM's owner thread after a completion arrives.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// A result computed off the owner thread, as plain data.
#[derive(Debug, Clone)]
pub(crate) enum Outcome {
    /// `tern.process.run`.
    Process(super::process::ProcessResult),
    /// `tern.fetch`.
    Fetch(super::fetch::FetchResult),
    /// A host-provided async answer (`cx.session:settle`): a value, or the error string.
    Value(Result<serde_json::Value, String>),
}

/// A finished operation for pending operation `id`.
#[derive(Debug)]
pub(crate) struct Completion {
    pub id: u64,
    pub outcome: Outcome,
}

/// What a due timer does.
#[derive(Debug)]
enum TimerAction {
    /// Calls a `tern.timer` function.
    Call(Function),
    /// Resolves a `tern.sleep` awaitable.
    Resolve(u64),
}

/// A callback ready to run: `f(args…)` under `hook` (a window caller appends a fresh cx).
#[derive(Debug, Clone)]
pub struct Callback {
    /// The hook it runs as (`timer`, `process`, `fetch`, `await`).
    pub hook: &'static str,
    /// The function.
    pub f: Function,
    /// Its arguments.
    pub args: Vec<Value>,
}

/// One pending operation: a callback-style call, or an awaitable.
#[derive(Debug, Default)]
struct Pending {
    /// The callback of the `cb` form, with its hook.
    callback: Option<(&'static str, Function)>,
    /// Observers registered with `next`.
    observers: Vec<Function>,
    /// `(result, err)` once settled.
    settled: Option<(Value, Value)>,
}

/// The owner-thread side of a VM's deferred work.
pub(crate) struct Tasks {
    next_id: u64,
    timers: BTreeMap<(Instant, u64), TimerAction>,
    pending: HashMap<u64, Pending>,
    ready: VecDeque<Callback>,
    tx: Sender<Completion>,
    rx: Receiver<Completion>,
    wake: Option<Wake>,
}

impl std::fmt::Debug for Tasks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tasks")
            .field("timers", &self.timers.len())
            .field("pending", &self.pending.len())
            .field("ready", &self.ready.len())
            .finish_non_exhaustive()
    }
}

impl Default for Tasks {
    fn default() -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        Tasks {
            next_id: 0,
            timers: BTreeMap::new(),
            pending: HashMap::new(),
            ready: VecDeque::new(),
            tx,
            rx,
            wake: None,
        }
    }
}

/// Hands a completion to the owner and wakes it.
#[derive(Clone)]
pub struct Completer {
    id: u64,
    tx: Sender<Completion>,
    wake: Option<Wake>,
}

impl std::fmt::Debug for Completer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Completer").field("id", &self.id).finish_non_exhaustive()
    }
}

impl Completer {
    pub(crate) fn complete(self, outcome: Outcome) {
        self.send(outcome);
    }

    /// Settles a host-provided awaitable with a value (as JSON) or an error.
    pub fn resolve(self, result: Result<serde_json::Value, String>) {
        self.send(Outcome::Value(result));
    }

    fn send(self, outcome: Outcome) {
        // The receiver lives as long as the VM; a send after it is gone is dropped (§9.3: the
        // work runs to completion, its callback never runs).
        if self.tx.send(Completion { id: self.id, outcome }).is_ok()
            && let Some(wake) = &self.wake
        {
            wake();
        }
    }
}

impl Tasks {
    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    pub(crate) fn set_wake(&mut self, wake: Option<Wake>) {
        self.wake = wake;
    }

    /// Adds a `tern.timer` call.
    pub(crate) fn add_timer(&mut self, at: Instant, f: Function) -> (Instant, u64) {
        let key = (at, self.id());
        self.timers.insert(key, TimerAction::Call(f));
        key
    }

    pub(crate) fn cancel_timer(&mut self, key: (Instant, u64)) {
        self.timers.remove(&key);
    }

    /// Starts a `tern.sleep`: an awaitable settled by a timer.
    pub(crate) fn sleep(&mut self, at: Instant) -> u64 {
        let id = self.id();
        self.pending.insert(id, Pending::default());
        let key = (at, self.id());
        self.timers.insert(key, TimerAction::Resolve(id));
        id
    }

    /// Starts an operation completed off-thread. With `callback`, its result goes to that
    /// function under `hook`; otherwise it is an awaitable.
    pub(crate) fn start(&mut self, callback: Option<(&'static str, Function)>) -> (u64, Completer) {
        let id = self.id();
        self.pending.insert(
            id,
            Pending {
                callback,
                ..Pending::default()
            },
        );
        let completer = Completer {
            id,
            tx: self.tx.clone(),
            wake: self.wake.clone(),
        };
        (id, completer)
    }

    /// Drops a pending operation that never started.
    pub(crate) fn forget(&mut self, id: u64) {
        self.pending.remove(&id);
    }

    /// Registers an observer of awaitable `id`. It is queued as hook `await`, never called inline,
    /// even when the awaitable already settled.
    pub(crate) fn observe(&mut self, id: u64, f: Function) {
        let Some(p) = self.pending.get_mut(&id) else {
            return;
        };
        if let Some((result, err)) = &p.settled {
            self.ready.push_back(Callback {
                hook: "await",
                f,
                args: vec![result.clone(), err.clone()],
            });
        } else {
            p.observers.push(f);
        }
    }

    /// Settles operation `id`: queues its callback or its observers.
    pub(crate) fn settle(&mut self, id: u64, hook: &'static str, result: Value, err: Value) {
        let Some(p) = self.pending.get_mut(&id) else {
            return;
        };
        if let Some((cb_hook, f)) = p.callback.take() {
            self.ready.push_back(Callback {
                hook: cb_hook,
                f,
                args: vec![result],
            });
            self.pending.remove(&id);
            return;
        }
        let _ = hook;
        for f in p.observers.drain(..) {
            self.ready.push_back(Callback {
                hook: "await",
                f,
                args: vec![result.clone(), err.clone()],
            });
        }
        p.settled = Some((result, err));
    }

    /// Completions that arrived, oldest first.
    pub(crate) fn take_completions(&self) -> Vec<Completion> {
        self.rx.try_iter().collect()
    }

    /// Moves due timers into the queue, in due order. Returns sleep ids to settle.
    pub(crate) fn take_due(&mut self, now: Instant) -> Vec<u64> {
        let mut sleeps = Vec::new();
        while let Some((&key, _)) = self.timers.iter().next() {
            if key.0 > now {
                break;
            }
            match self.timers.remove(&key) {
                Some(TimerAction::Call(f)) => self.ready.push_back(Callback {
                    hook: "timer",
                    f,
                    args: Vec::new(),
                }),
                Some(TimerAction::Resolve(id)) => sleeps.push(id),
                None => {}
            }
        }
        sleeps
    }

    /// The next queued callback.
    pub(crate) fn pop_ready(&mut self) -> Option<Callback> {
        self.ready.pop_front()
    }

    /// Whether callbacks are queued.
    pub(crate) fn has_ready(&self) -> bool {
        !self.ready.is_empty() || !self.rx.is_empty()
    }

    /// When the next timer is due.
    pub(crate) fn next_due(&self) -> Option<Instant> {
        self.timers.keys().next().map(|(at, _)| *at)
    }

    /// The channel completions arrive on (a worker selects on it).
    pub(crate) fn receiver(&self) -> &Receiver<Completion> {
        &self.rx
    }
}
