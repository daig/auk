//! The `tern.*` API surface (§9.9, research/plugins.md §4.9).
//!
//! [`install`] builds the `tern` table for one VM: the shared members first ([`shared`]), then
//! the context's own ([`host`] or [`window`]). A member of the other context is absent (nil), so
//! using it raises Luau's own error. Later milestones add their namespaces here (`json`, `kv`,
//! `fs`, `ui`, `pane` in M7; lens running in M8; commands, binds and chrome in M9), each as a
//! module with an `install` that fills the same table before the VM is sandboxed.

pub mod effects;
mod host;
mod shared;
mod window;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use mlua::{Function, Lua, Table};

use crate::vm::{Context, LogSink, PluginInfo};
use effects::{Effect, EffectTarget};

/// The handlers of one `tern.block.define`.
#[derive(Debug, Clone)]
pub struct BlockHandlers {
    /// `init(cx, args, saved) -> state`.
    pub init: Function,
    /// `view(state, cx) -> {main?, dock?, layer?}`.
    pub view: Function,
    /// `title(state) -> string?`.
    pub title: Option<Function>,
    /// `key(state, key, cx) -> boolean?`.
    pub key: Option<Function>,
    /// `event(state, ev, cx)`.
    pub event: Option<Function>,
    /// `resize(state, cols, rows, cx)`.
    pub resize: Option<Function>,
    /// `save(state) -> json`.
    pub save: Option<Function>,
}

/// The handlers of one `tern.lens.define`.
#[derive(Debug, Clone)]
pub struct LensHandlers {
    /// `open(run, cx) -> state`.
    pub open: Function,
    /// `line(state, line)`.
    pub line: Function,
    /// `finish(state, status)`.
    pub finish: Function,
    /// `view(state) -> node?`.
    pub view: Function,
    /// `event(state, ev, cx)`.
    pub event: Option<Function>,
}

/// What a VM's code has registered.
#[derive(Debug, Default)]
pub(crate) struct Registry {
    /// `tern.on` handlers by event name, in registration order.
    pub events: BTreeMap<String, Vec<Function>>,
    /// Block types by id.
    pub blocks: BTreeMap<String, BlockHandlers>,
    /// Lenses by id.
    pub lenses: BTreeMap<String, LensHandlers>,
}

/// Pending `tern.timer` callbacks, by due time then creation order.
#[derive(Debug, Default)]
pub(crate) struct Timers {
    next_id: u64,
    due: BTreeMap<(Instant, u64), Function>,
}

impl Timers {
    fn add(&mut self, at: Instant, f: Function) -> (Instant, u64) {
        self.next_id += 1;
        let key = (at, self.next_id);
        self.due.insert(key, f);
        key
    }

    fn cancel(&mut self, key: (Instant, u64)) {
        self.due.remove(&key);
    }

    /// When the next timer is due.
    pub(crate) fn next_due(&self) -> Option<Instant> {
        self.due.keys().next().map(|(at, _)| *at)
    }

    /// Removes and returns the oldest timer due at `now`.
    pub(crate) fn pop_due(&mut self, now: Instant) -> Option<Function> {
        let (&key, _) = self.due.iter().next().filter(|((at, _), _)| *at <= now)?;
        self.due.remove(&key)
    }
}

/// The state behind one VM's `tern` table, shared by the API closures and the [`crate::Vm`].
pub(crate) struct ApiState {
    pub context: Context,
    pub plugin: PluginInfo,
    pub jit: bool,
    pub log: LogSink,
    /// Block ids the manifest declares (host).
    pub declared_blocks: Vec<String>,
    /// Lens ids the manifest declares (host).
    pub declared_lenses: Vec<String>,
    pub registry: RefCell<Registry>,
    pub timers: RefCell<Timers>,
    effects: RefCell<Vec<(EffectTarget, Effect)>>,
    /// Raised once a `spawn` handler is registered, so the daemon knows to ask this worker.
    pub has_spawn: Arc<AtomicBool>,
}

impl std::fmt::Debug for ApiState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiState")
            .field("context", &self.context)
            .field("plugin", &self.plugin.id)
            .finish_non_exhaustive()
    }
}

impl ApiState {
    pub(crate) fn new(
        context: Context,
        plugin: PluginInfo,
        jit: bool,
        log: LogSink,
        declared_blocks: Vec<String>,
        declared_lenses: Vec<String>,
    ) -> Self {
        ApiState {
            context,
            plugin,
            jit,
            log,
            declared_blocks,
            declared_lenses,
            registry: RefCell::default(),
            timers: RefCell::default(),
            effects: RefCell::default(),
            has_spawn: Arc::default(),
        }
    }

    pub(crate) fn push_effect(&self, target: EffectTarget, effect: Effect) {
        self.effects.borrow_mut().push((target, effect));
    }

    pub(crate) fn take_effects(&self) -> Vec<(EffectTarget, Effect)> {
        std::mem::take(&mut *self.effects.borrow_mut())
    }

    pub(crate) fn register_event(&self, name: &str, f: Function) {
        if name == "spawn" {
            self.has_spawn.store(true, Ordering::Release);
        }
        self.registry
            .borrow_mut()
            .events
            .entry(name.to_owned())
            .or_default()
            .push(f);
    }
}

/// Builds the `tern` table for `state`'s context.
pub(crate) fn install(lua: &Lua, state: &std::rc::Rc<ApiState>) -> mlua::Result<Table> {
    let tern = lua.create_table()?;
    shared::install(lua, &tern, state)?;
    match state.context {
        Context::Host => host::install(lua, &tern, state)?,
        Context::Window => window::install(lua, &tern, state)?,
    }
    Ok(tern)
}
