//! Members both halves have: `context`, `plugin`, `runtime`, `log`, `now`, `getenv`, `timer`,
//! `sleep`, `json`, `base64`, `kv`, `fs`, `process`, `fetch`, `ui` and `parse`.

use std::rc::Rc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use mlua::{Function, Lua, MultiValue, Table, UserData, UserDataMethods};

use super::ApiState;
use super::effects::display;
use crate::vm::{Context, LogLevel, LogRecord};

/// The Luau half of the API: `tern.ui` and `tern.parse`.
const PRELUDE: &str = include_str!("ui.luau");

/// `tern.now()`: monotonic milliseconds, fractional, from the first call in the process.
pub(crate) fn now_ms() -> f64 {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    ORIGIN.get_or_init(Instant::now).elapsed().as_secs_f64() * 1000.0
}

/// A `tern.timer` handle; `cancel` does nothing once the timer fired.
struct TimerHandle {
    key: (Instant, u64),
    state: Rc<ApiState>,
}

impl UserData for TimerHandle {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("cancel", |_, this, ()| {
            this.state.tasks.borrow_mut().cancel_timer(this.key);
            Ok(())
        });
    }
}

/// The `tern.log.<level>` function: arguments through `tostring`, joined with tabs. Never raises.
pub(super) fn log_function(
    lua: &Lua,
    state: &Rc<ApiState>,
    level: LogLevel,
) -> mlua::Result<Function> {
    let state = Rc::clone(state);
    lua.create_function(move |_, args: MultiValue| {
        let message = args.iter().map(display).collect::<Vec<_>>().join("\t");
        (state.log)(&LogRecord {
            plugin: &state.plugin.id,
            level,
            message: &message,
        });
        Ok(())
    })
}

fn delay(ms: f64) -> Duration {
    if ms.is_finite() && ms > 0.0 {
        Duration::from_secs_f64(ms / 1000.0)
    } else {
        Duration::ZERO
    }
}

pub(super) fn install(lua: &Lua, tern: &Table, state: &Rc<ApiState>) -> mlua::Result<()> {
    tern.set(
        "context",
        match state.context {
            Context::Host => "host",
            Context::Window => "window",
        },
    )?;

    let plugin = lua.create_table()?;
    plugin.set("id", state.plugin.id.as_str())?;
    plugin.set("name", state.plugin.name.as_str())?;
    plugin.set("dir", state.plugin.dir.to_string_lossy().as_ref())?;
    plugin.set("data", state.plugin.data.to_string_lossy().as_ref())?;
    plugin.set_readonly(true);
    tern.set("plugin", plugin)?;

    let runtime = lua.create_table()?;
    runtime.set(
        "os",
        if cfg!(target_os = "macos") {
            "macos"
        } else {
            std::env::consts::OS
        },
    )?;
    runtime.set("jit", state.jit)?;
    runtime.set_readonly(true);
    tern.set("runtime", runtime)?;

    let log = lua.create_table()?;
    for level in [
        LogLevel::Debug,
        LogLevel::Info,
        LogLevel::Warn,
        LogLevel::Error,
    ] {
        log.set(level.name(), log_function(lua, state, level)?)?;
    }
    log.set_readonly(true);
    tern.set("log", log)?;

    tern.set("now", lua.create_function(|_, ()| Ok(now_ms()))?)?;

    tern.set(
        "getenv",
        lua.create_function(|_, name: String| Ok(std::env::var(name).ok()))?,
    )?;

    let timers = Rc::clone(state);
    tern.set(
        "timer",
        lua.create_function(move |_, (ms, f): (f64, Function)| {
            let key = timers
                .tasks
                .borrow_mut()
                .add_timer(Instant::now() + delay(ms), f);
            Ok(TimerHandle {
                key,
                state: Rc::clone(&timers),
            })
        })?,
    )?;

    let sleeper = Rc::clone(state);
    tern.set(
        "sleep",
        lua.create_function(move |lua, ms: f64| {
            if !ms.is_finite() || ms < 0.0 {
                return Err(mlua::Error::runtime(
                    "tern.sleep: ms must be a finite, nonnegative number",
                ));
            }
            let id = sleeper.tasks.borrow_mut().sleep(Instant::now() + delay(ms));
            super::awaitable(lua, &sleeper, "sleep", id)
        })?,
    )?;

    super::json::install_json(lua, tern)?;
    super::storage::install_kv(lua, tern, state)?;
    super::storage::install_fs(lua, tern, state)?;

    let process = lua.create_table()?;
    let s = Rc::clone(state);
    process.set(
        "run",
        lua.create_function(move |lua, args: MultiValue| super::process::run(lua, &s, args))?,
    )?;
    process.set_readonly(true);
    tern.set("process", process)?;

    let s = Rc::clone(state);
    tern.set(
        "fetch",
        lua.create_function(move |lua, args: MultiValue| super::fetch::fetch(lua, &s, args))?,
    )?;

    let native = lua.create_table()?;
    native.set("json_tree", lua.create_function(super::json::json_tree)?)?;
    let (ui, parse): (Table, Table) = lua
        .load(PRELUDE)
        .set_name("=tern.ui")
        .call(native)?;
    ui.set_readonly(true);
    parse.set_readonly(true);
    tern.set("ui", ui)?;
    tern.set("parse", parse)?;
    Ok(())
}
