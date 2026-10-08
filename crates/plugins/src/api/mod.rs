//! The `tern.*` API surface (§9.9, research/plugins.md §4.9).
//!
//! [`install`] builds the `tern` table for one VM: the shared members first ([`shared`]), then
//! the context's own ([`host`] or [`window`]). A member of the other context is absent (nil), so
//! using it raises Luau's own error.

pub mod block;
pub(crate) mod convert;
pub mod effects;
pub(crate) mod fetch;
mod host;
mod json;
pub(crate) mod process;
mod shared;
mod storage;
pub(crate) mod tasks;
pub mod window;

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use mlua::{Function, Lua, Table, Value};

use crate::events::{PaneDirectory, PaneId};
use crate::vm::{Context, LogSink, PluginInfo};
use block::BlockRequest;
use effects::{Effect, EffectTarget};
use storage::KvCache;
use tasks::Tasks;
use window::WindowRegistry;

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
    /// Window registrations.
    pub window: WindowRegistry,
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
    pub tasks: RefCell<Tasks>,
    effects: RefCell<Vec<(EffectTarget, Effect)>>,
    /// Raised once a `spawn` handler is registered, so the daemon knows to ask this worker.
    pub has_spawn: Arc<AtomicBool>,
    pub kv: RefCell<KvCache>,
    fetch_agent: RefCell<Option<ureq::Agent>>,
    /// `tern.pane.write` requests, applied after the handler returns.
    pub pane_writes: RefCell<Vec<(PaneId, String)>>,
    /// The host's panes, for `tern.pane.list`.
    pub panes: Option<Arc<dyn PaneDirectory>>,
    /// Block cx requests, applied after the handler returns.
    pub block_requests: RefCell<Vec<(PaneId, BlockRequest)>>,
    /// Built-in palette command ids `tern.override` accepts (window); `None` accepts any.
    pub builtin_commands: Option<Arc<Vec<String>>>,
    /// Set while a chrome formatter runs (`tern.chrome.refresh` then raises).
    pub in_formatter: Cell<bool>,
    /// `tern.chrome.refresh` was called.
    pub chrome_refresh: Cell<bool>,
    /// A window registration changed (keymap, palette, formatters or sheets need rebuilding).
    pub registrations_changed: Cell<bool>,
}

impl std::fmt::Debug for ApiState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiState")
            .field("context", &self.context)
            .field("plugin", &self.plugin.id)
            .finish_non_exhaustive()
    }
}

/// Everything a VM's API needs besides the context and plugin.
pub(crate) struct ApiOptions {
    pub context: Context,
    pub plugin: PluginInfo,
    pub jit: bool,
    pub log: LogSink,
    pub declared_blocks: Vec<String>,
    pub declared_lenses: Vec<String>,
    pub panes: Option<Arc<dyn PaneDirectory>>,
    pub builtin_commands: Option<Arc<Vec<String>>>,
}

impl ApiState {
    pub(crate) fn new(o: ApiOptions) -> Self {
        ApiState {
            context: o.context,
            plugin: o.plugin,
            jit: o.jit,
            log: o.log,
            declared_blocks: o.declared_blocks,
            declared_lenses: o.declared_lenses,
            registry: RefCell::default(),
            tasks: RefCell::default(),
            effects: RefCell::default(),
            has_spawn: Arc::default(),
            kv: RefCell::default(),
            fetch_agent: RefCell::default(),
            pane_writes: RefCell::default(),
            panes: o.panes,
            block_requests: RefCell::default(),
            builtin_commands: o.builtin_commands,
            in_formatter: Cell::new(false),
            chrome_refresh: Cell::new(false),
            registrations_changed: Cell::new(false),
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

    /// The VM's HTTP agent (connections are kept alive per VM).
    pub(crate) fn fetch_agent(&self) -> ureq::Agent {
        self.fetch_agent
            .borrow_mut()
            .get_or_insert_with(fetch::agent)
            .clone()
    }
}

/// An awaitable `{kind, id, next}` for pending operation `id`.
pub(crate) fn awaitable(
    lua: &Lua,
    state: &std::rc::Rc<ApiState>,
    kind: &str,
    id: u64,
) -> mlua::Result<Value> {
    let t = lua.create_table()?;
    t.set("kind", kind)?;
    t.set("id", id as f64)?;
    let s = std::rc::Rc::clone(state);
    t.set(
        "next",
        lua.create_function(move |_, (_this, f): (Value, Function)| {
            s.tasks.borrow_mut().observe(id, f);
            Ok(())
        })?,
    )?;
    t.set_readonly(true);
    Ok(Value::Table(t))
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
