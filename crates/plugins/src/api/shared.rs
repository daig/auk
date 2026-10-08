//! Members both halves have: `context`, `plugin`, `runtime`, `log`, `now`, `getenv`, `timer`.

use std::rc::Rc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use mlua::{Function, Lua, MultiValue, Table, UserData, UserDataMethods};

use super::ApiState;
use super::effects::display;
use crate::vm::{Context, LogLevel, LogRecord};

/// `tern.now()`: monotonic milliseconds, fractional, from the first call in the process.
fn now_ms() -> f64 {
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
            this.state.timers.borrow_mut().cancel(this.key);
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
            let delay = if ms.is_finite() && ms > 0.0 {
                Duration::from_secs_f64(ms / 1000.0)
            } else {
                Duration::ZERO
            };
            let key = timers.timers.borrow_mut().add(Instant::now() + delay, f);
            Ok(TimerHandle {
                key,
                state: Rc::clone(&timers),
            })
        })?,
    )?;
    Ok(())
}
