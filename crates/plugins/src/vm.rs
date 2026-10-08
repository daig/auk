//! One plugin VM: Luau in sandbox mode with the 256 MiB cap, per-call budgets, native codegen,
//! a package-confined `require`, and the `tern` API (§9.2, §20).
//!
//! A [`Vm`] is not `Send`: a host half's VM lives on its worker thread (`worker.rs`), a window
//! half's on the window's UI thread.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use mlua::{FromLuaMulti, Function, IntoLuaMulti, Lua, Table, Value, ffi};

use crate::api::effects::{Effect, EffectCx, EffectTarget};
use crate::api::tasks::{Callback, Outcome, Wake};
use crate::api::window::WindowRegistry;
use crate::api::{self, ApiOptions, ApiState, BlockHandlers, LensHandlers};
use crate::events::{PaneDirectory, PaneId};
use crate::budget::{Budget, CallClock, FrameGuard};
use crate::manifest::Manifest;
use crate::require::{PackageRequirer, chunk_name};

/// The memory cap of every VM.
pub const MEMORY_LIMIT: usize = 256 * 1024 * 1024;

/// Which half a VM runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Context {
    /// The host half, on a worker thread in the daemon.
    Host,
    /// The window half, on a window's UI thread.
    Window,
}

impl Context {
    /// The budget of an ordinary call (and the entry) in this context.
    pub fn budget(self) -> Budget {
        match self {
            Context::Host => Budget::Host,
            Context::Window => Budget::Window,
        }
    }
}

/// `tern.plugin`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginInfo {
    /// Manifest id.
    pub id: String,
    /// Manifest name.
    pub name: String,
    /// The package directory (the link target for a linked package).
    pub dir: PathBuf,
    /// `<state>/plugin-data/<id>`.
    pub data: PathBuf,
}

/// A `tern.log` level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    /// `tern.log.debug`.
    Debug,
    /// `tern.log.info` and `print`.
    Info,
    /// `tern.log.warn`.
    Warn,
    /// `tern.log.error`.
    Error,
}

impl LogLevel {
    /// The `tern.log` member name.
    pub fn name(self) -> &'static str {
        match self {
            LogLevel::Debug => "debug",
            LogLevel::Info => "info",
            LogLevel::Warn => "warn",
            LogLevel::Error => "error",
        }
    }
}

/// One `tern.log` line.
#[derive(Debug, Clone, Copy)]
pub struct LogRecord<'a> {
    /// The plugin id.
    pub plugin: &'a str,
    /// The level.
    pub level: LogLevel,
    /// The arguments through `tostring`, joined with tabs.
    pub message: &'a str,
}

/// Where `tern.log` and `print` go.
pub type LogSink = Arc<dyn Fn(&LogRecord<'_>) + Send + Sync>;

/// The default sink: the `tern::plugin` tracing target with a `plugin` field.
pub fn tracing_log_sink() -> LogSink {
    Arc::new(|r: &LogRecord<'_>| match r.level {
        LogLevel::Debug => {
            tracing::debug!(target: "tern::plugin", plugin = r.plugin, "{}", r.message)
        }
        LogLevel::Info => {
            tracing::info!(target: "tern::plugin", plugin = r.plugin, "{}", r.message)
        }
        LogLevel::Warn => {
            tracing::warn!(target: "tern::plugin", plugin = r.plugin, "{}", r.message)
        }
        LogLevel::Error => {
            tracing::error!(target: "tern::plugin", plugin = r.plugin, "{}", r.message)
        }
    })
}

/// How to build a [`Vm`].
#[derive(Clone)]
pub struct VmOptions {
    /// Host or window half.
    pub context: Context,
    /// `tern.plugin`; `dir` is the package `require` is confined to.
    pub plugin: PluginInfo,
    /// Block ids the manifest declares.
    pub declared_blocks: Vec<String>,
    /// Lens ids the manifest declares.
    pub declared_lenses: Vec<String>,
    /// The memory cap; [`MEMORY_LIMIT`] unless a test lowers it.
    pub memory_limit: usize,
    /// Whether to compile chunks to native code (when the CPU supports it).
    pub jit: bool,
    /// Where `tern.log` and `print` go.
    pub log: LogSink,
    /// The host's panes for `tern.pane.list` (host halves).
    pub panes: Option<Arc<dyn PaneDirectory>>,
    /// Built-in palette command ids `tern.override` accepts (window halves); `None` accepts any.
    pub builtin_commands: Option<Arc<Vec<String>>>,
}

impl fmt::Debug for VmOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VmOptions")
            .field("context", &self.context)
            .field("plugin", &self.plugin)
            .field("memory_limit", &self.memory_limit)
            .field("jit", &self.jit)
            .finish_non_exhaustive()
    }
}

impl VmOptions {
    /// Options for `manifest`'s package at `dir`, with its data folder at `data`.
    pub fn for_manifest(context: Context, manifest: &Manifest, dir: &Path, data: &Path) -> Self {
        VmOptions {
            context,
            plugin: PluginInfo {
                id: manifest.id.clone(),
                name: manifest.name.clone(),
                dir: dir.into(),
                data: data.into(),
            },
            declared_blocks: manifest.blocks.iter().map(|b| b.id.clone()).collect(),
            declared_lenses: manifest.lenses.iter().map(|l| l.id.clone()).collect(),
            memory_limit: MEMORY_LIMIT,
            jit: true,
            log: tracing_log_sink(),
            panes: None,
            builtin_commands: None,
        }
    }
}

/// Whether Luau's native code generator supports this CPU.
pub fn codegen_supported() -> bool {
    // SAFETY: a pure query with no preconditions.
    unsafe { ffi::luau_codegen_supported() != 0 }
}

/// Why a call into a VM produced no result.
#[derive(Debug, Clone)]
pub enum CallError {
    /// The hook was disabled by an earlier budget trip; the call was skipped.
    Disabled {
        /// The hook.
        hook: String,
    },
    /// The call ran past its budget; the hook is now disabled until reload.
    Exceeded {
        /// The hook.
        hook: String,
        /// Its budget in milliseconds.
        budget_ms: u64,
    },
    /// The call raised (a memory error included).
    Lua(mlua::Error),
}

impl CallError {
    /// The error's message without mlua's wrapping (callback tracebacks are dropped; Luau's own
    /// `file:line:` prefix stays).
    pub fn message(&self) -> String {
        match self {
            CallError::Lua(e) => lua_message(e),
            other => other.to_string(),
        }
    }

    /// The first line of the message, as toasts show it.
    pub fn first_line(&self) -> String {
        self.message().lines().next().unwrap_or_default().to_owned()
    }
}

fn lua_message(e: &mlua::Error) -> String {
    match e {
        mlua::Error::RuntimeError(m) | mlua::Error::MemoryError(m) => m.clone(),
        mlua::Error::SyntaxError { message, .. } => message.clone(),
        mlua::Error::CallbackError { cause, .. } => lua_message(cause),
        mlua::Error::WithContext { cause, .. } => lua_message(cause),
        other => other.to_string(),
    }
}

impl fmt::Display for CallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CallError::Disabled { hook } => {
                write!(f, "tern: {hook} is disabled after exceeding its budget")
            }
            CallError::Exceeded { hook, budget_ms } => {
                write!(f, "tern: {hook} exceeded {budget_ms} ms")
            }
            CallError::Lua(e) => f.write_str(&lua_message(e)),
        }
    }
}

impl std::error::Error for CallError {}

/// A plugin VM.
pub struct Vm {
    // Declared first so the Lua state is closed before anything below it is dropped.
    lua: Lua,
    clock: Rc<CallClock>,
    api: Rc<ApiState>,
    disabled: RefCell<BTreeSet<String>>,
}

impl fmt::Debug for Vm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Vm")
            .field("plugin", &self.api.plugin.id)
            .field("context", &self.api.context)
            .finish_non_exhaustive()
    }
}

impl Vm {
    /// Builds a sandboxed VM with the `tern` API installed (and `auk` as its alias).
    pub fn new(options: VmOptions) -> mlua::Result<Self> {
        let lua = Lua::new();
        lua.set_memory_limit(options.memory_limit)?;
        let jit = options.jit && codegen_supported();
        lua.enable_jit(jit);

        let mut plugin = options.plugin;
        if let Ok(dir) = plugin.dir.canonicalize() {
            plugin.dir = dir;
        }
        let requirer = PackageRequirer::new(&plugin.dir).map_err(|e| {
            mlua::Error::runtime(format!("cannot read {}: {e}", plugin.dir.display()))
        })?;
        let api = Rc::new(ApiState::new(ApiOptions {
            context: options.context,
            plugin,
            jit,
            log: options.log,
            declared_blocks: options.declared_blocks,
            declared_lenses: options.declared_lenses,
            panes: options.panes,
            builtin_commands: options.builtin_commands,
        }));

        let tern = api::install(&lua, &api)?;
        let globals = lua.globals();
        globals.raw_set("tern", &tern)?;
        globals.raw_set("auk", &tern)?;
        let log: Table = tern.raw_get("log")?;
        globals.raw_set("print", log.raw_get::<Function>("info")?)?;
        globals.raw_set("require", lua.create_require_function(requirer)?)?;
        register_module(&lua, "tern", &tern)?;
        register_module(&lua, "auk", &tern)?;
        // `os` keeps only clock, date, difftime and time (Luau has nothing else); there is no
        // `io`, `package`, `dofile` or `loadfile` in Luau to begin with.
        lua.sandbox(true)?;

        let clock = CallClock::install(&lua)?;
        Ok(Vm {
            lua,
            clock,
            api,
            disabled: RefCell::default(),
        })
    }

    /// The underlying Lua state, for API modules and tests.
    pub fn lua(&self) -> &Lua {
        &self.lua
    }

    /// Host or window.
    pub fn context(&self) -> Context {
        self.api.context
    }

    /// `tern.plugin` (with the canonical package directory).
    pub fn plugin(&self) -> &PluginInfo {
        &self.api.plugin
    }

    /// Whether chunks are compiled to native code (`tern.runtime.jit`). Like Tern's, it does
    /// not report whether the OS granted executable memory.
    pub fn jit(&self) -> bool {
        self.api.jit
    }

    /// Bytes the VM has allocated.
    pub fn used_memory(&self) -> usize {
        self.lua.used_memory()
    }

    /// Compiles a chunk as if it were the file `path` (chunk name `@<path>`), without running it.
    pub fn compile(&self, source: &str, path: &Path) -> mlua::Result<Function> {
        self.lua
            .load(source)
            .set_name(chunk_name(path))
            .into_function()
    }

    /// Runs the entry at `relative` (inside the package) under the `load` hook and the
    /// context's budget, then checks that every declared block and lens was defined. The error
    /// is the full text a Failed status keeps.
    pub fn run_entry(&self, relative: &str) -> Result<(), String> {
        let path = self.api.plugin.dir.join(relative);
        let source =
            std::fs::read_to_string(&path).map_err(|e| format!("cannot read {relative}: {e}"))?;
        let entry = self
            .compile(&source, &path)
            .map_err(|e| CallError::Lua(e).message())?;
        self.call::<()>("load", self.context().budget(), &entry, ())
            .map_err(|e| e.message())?;
        let registry = self.api.registry.borrow();
        if let Some(id) = self
            .api
            .declared_blocks
            .iter()
            .find(|id| !registry.blocks.contains_key(*id))
        {
            return Err(format!(
                "block {id} is declared in plugin.toml but not defined with tern.block.define"
            ));
        }
        if let Some(id) = self
            .api
            .declared_lenses
            .iter()
            .find(|id| !registry.lenses.contains_key(*id))
        {
            return Err(format!(
                "lens {id} is declared in plugin.toml but not defined with tern.lens.define"
            ));
        }
        Ok(())
    }

    /// Calls `f` as `hook` under `budget`.
    ///
    /// - A disabled hook is skipped ([`CallError::Disabled`]).
    /// - Nested calls keep the earlier deadline.
    /// - A call that passes its deadline raises at every later interrupt check, and returns
    ///   [`CallError::Exceeded`] even if Lua swallowed the error; the hook is then disabled in
    ///   this VM until it is rebuilt.
    pub fn call<R: FromLuaMulti>(
        &self,
        hook: &str,
        budget: Budget,
        f: &Function,
        args: impl IntoLuaMulti,
    ) -> Result<R, CallError> {
        if self.is_disabled(hook) {
            tracing::debug!(plugin = %self.api.plugin.id, hook, "disabled plugin hook skipped");
            return Err(CallError::Disabled {
                hook: hook.to_owned(),
            });
        }
        let frame = FrameGuard::enter(&self.clock, hook, budget);
        let result = f.call::<R>(args);
        if let Err(mlua::Error::MemoryError(_)) = &result {
            // Luau collects no garbage when an allocation fails; do it now so the VM recovers.
            let _ = self.lua.gc_collect();
        }
        if frame.finish() {
            self.disabled.borrow_mut().insert(hook.to_owned());
            tracing::warn!(plugin = %self.api.plugin.id, hook, "plugin hook exceeded its budget; disabled until reload");
            return Err(CallError::Exceeded {
                hook: hook.to_owned(),
                budget_ms: budget.millis(),
            });
        }
        result.map_err(CallError::Lua)
    }

    /// Whether `hook` was disabled by a budget trip.
    pub fn is_disabled(&self, hook: &str) -> bool {
        self.disabled.borrow().contains(hook)
    }

    /// The `tern.on` handlers for `event`, in registration order.
    pub fn handlers(&self, event: &str) -> Vec<Function> {
        self.api
            .registry
            .borrow()
            .events
            .get(event)
            .cloned()
            .unwrap_or_default()
    }

    /// The handlers of block type `id`, once defined.
    pub fn block(&self, id: &str) -> Option<BlockHandlers> {
        self.api.registry.borrow().blocks.get(id).cloned()
    }

    /// The handlers of lens `id`, once defined.
    pub fn lens(&self, id: &str) -> Option<LensHandlers> {
        self.api.registry.borrow().lenses.get(id).cloned()
    }

    /// An effect cx aimed at `target`, as host event handlers receive.
    pub fn effect_cx(&self, target: EffectTarget) -> mlua::Result<Value> {
        self.lua
            .create_userdata(EffectCx::new(target, Rc::clone(&self.api)))
            .map(Value::UserData)
    }

    /// Effects queued by handlers since the last call, oldest first.
    pub fn take_effects(&self) -> Vec<(EffectTarget, Effect)> {
        self.api.take_effects()
    }

    /// When the next timer (`tern.timer`, `tern.sleep`) is due.
    pub fn next_timer(&self) -> Option<Instant> {
        self.api.tasks.borrow().next_due()
    }

    /// Wakes the owner thread when a process or request completes. Set it before the entry runs.
    pub fn set_waker(&self, wake: Option<Wake>) {
        self.api.tasks.borrow_mut().set_wake(wake);
    }

    /// The channel completions arrive on, for an owner that selects on it (it must not
    /// receive from it: [`Vm::next_callback`] does).
    pub(crate) fn completion_receiver(&self) -> crossbeam_channel::Receiver<crate::api::tasks::Completion> {
        self.api.tasks.borrow().receiver().clone()
    }

    /// The block cx of `pane`.
    pub(crate) fn block_cx(
        &self,
        pane: PaneId,
        geometry: Rc<RefCell<crate::api::block::Geometry>>,
    ) -> mlua::Result<Value> {
        self.lua
            .create_userdata(crate::api::block::BlockCx {
                pane,
                geometry,
                state: Rc::clone(&self.api),
            })
            .map(Value::UserData)
    }

    /// Whether callbacks are waiting (completions arrived or callbacks are queued).
    pub fn has_callbacks(&self) -> bool {
        self.api.tasks.borrow().has_ready()
    }

    /// The next callback to run on the owner thread, oldest first: completed processes and
    /// requests, due timers and settled awaitables. The caller runs it under its hook with
    /// [`Vm::call`] (a window adds a fresh cx as the last argument).
    pub fn next_callback(&self, now: Instant) -> Option<Callback> {
        let completions = self.api.tasks.borrow().take_completions();
        for c in completions {
            let (hook, result, err) = match &c.outcome {
                Outcome::Process(r) => ("process", r.to_lua(&self.lua), Value::Nil),
                Outcome::Fetch(r) => ("fetch", r.to_lua(&self.lua), Value::Nil),
                Outcome::Value(Ok(v)) => ("await", api::convert::to_lua(&self.lua, v), Value::Nil),
                Outcome::Value(Err(e)) => (
                    "await",
                    Ok(Value::Nil),
                    self.lua.create_string(e).map_or(Value::Nil, Value::String),
                ),
            };
            match result {
                Ok(v) => self.api.tasks.borrow_mut().settle(c.id, hook, v, err),
                Err(error) => {
                    tracing::warn!(plugin = %self.api.plugin.id, %error, "cannot hand a process result to Lua");
                }
            }
        }
        let sleeps = self.api.tasks.borrow_mut().take_due(now);
        for id in sleeps {
            self.api
                .tasks
                .borrow_mut()
                .settle(id, "await", Value::Nil, Value::Nil);
        }
        self.api.tasks.borrow_mut().pop_ready()
    }

    /// `tern.pane.write` requests since the last call, in order.
    pub fn take_pane_writes(&self) -> Vec<(PaneId, String)> {
        std::mem::take(&mut *self.api.pane_writes.borrow_mut())
    }

    /// Block cx requests since the last call, in order.
    pub fn take_block_requests(&self) -> Vec<(PaneId, api::block::BlockRequest)> {
        std::mem::take(&mut *self.api.block_requests.borrow_mut())
    }

    /// Converts JSON to a Lua value with Tern's rules (arrays tagged, null members dropped).
    pub fn to_lua(&self, json: &serde_json::Value) -> mlua::Result<Value> {
        api::convert::to_lua(&self.lua, json)
    }

    /// Converts a Lua value to JSON with Tern's rules (sorted keys, integral numbers as integers).
    pub fn to_json(&self, value: &Value) -> Result<serde_json::Value, String> {
        api::convert::to_json(&self.lua, value)
    }

    /// The window registrations (commands, binds, overrides, routes, formatters, sheets).
    pub fn window_registry(&self) -> std::cell::Ref<'_, WindowRegistry> {
        std::cell::Ref::map(self.api.registry.borrow(), |r| &r.window)
    }

    /// Whether a window registration changed since the last call (and clears the flag).
    pub fn take_registrations_changed(&self) -> bool {
        self.api.registrations_changed.replace(false)
    }

    /// Whether `tern.chrome.refresh` was called since the last call (and clears the flag).
    pub fn take_chrome_refresh(&self) -> bool {
        self.api.chrome_refresh.replace(false)
    }

    /// Marks that a chrome formatter is running (so `tern.chrome.refresh` raises).
    pub(crate) fn set_in_formatter(&self, on: bool) {
        self.api.in_formatter.set(on);
    }

    pub(crate) fn api(&self) -> &Rc<ApiState> {
        &self.api
    }

    /// The flag raised once a `spawn` handler is registered.
    pub fn spawn_flag(&self) -> Arc<std::sync::atomic::AtomicBool> {
        Arc::clone(&self.api.has_spawn)
    }
}

/// Makes `require(name)` return `value`. mlua's `register_module` insists on `@` names; plain
/// names are what `require("tern")` asks for, and mlua's `require` checks this table first.
fn register_module(lua: &Lua, name: &str, value: &Table) -> mlua::Result<()> {
    // SAFETY: pushes the registered-modules table and sets one string key on it; the stack is
    // balanced by `exec_raw`'s guard.
    unsafe {
        lua.exec_raw::<()>(value, |state| {
            ffi::luaL_getsubtable(
                state,
                ffi::LUA_REGISTRYINDEX,
                ffi::LUA_REGISTERED_MODULES_TABLE,
            );
            ffi::lua_pushlstring(state, name.as_ptr().cast(), name.len());
            ffi::lua_pushvalue(state, -3);
            ffi::lua_rawset(state, -3);
        })
    }
}
