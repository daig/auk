//! Window-only registration (§9.8): `tern.on` with the window events, `tern.command`,
//! `tern.bind`, `tern.override`, `tern.route.open`/`link`, `tern.chrome.*` and `tern.css`.
//! Nothing here can reach a block's frames (AC14): blocks live in host workers.

use std::rc::Rc;

use mlua::{Function, Lua, Table, Value};

use super::ApiState;
use crate::events::WINDOW_EVENTS;

/// A `tern.command`.
#[derive(Debug, Clone)]
pub struct CommandDef {
    /// Id; the action is `plugin.<plugin>.<id>`.
    pub id: String,
    /// Palette label.
    pub title: String,
    /// Icon name (an unknown or absent one is `puzzle`).
    pub icon: String,
    /// Palette group (`None`: the plugin's name).
    pub group: Option<String>,
    /// Default chords.
    pub keys: Vec<String>,
    /// `available(cx) -> boolean` (4 ms).
    pub available: Option<Function>,
    /// `run(cx)`.
    pub run: Function,
}

/// What a `tern.bind` runs.
#[derive(Debug, Clone)]
pub enum BindTarget {
    /// An action name.
    Action(String),
    /// A function bind, numbered `n` among this plugin's function binds (action
    /// `plugin.<plugin>.bind.<n>`).
    Function(Function, usize),
}

/// A `tern.bind` (or a command's `keys`).
#[derive(Debug, Clone)]
pub struct Bind {
    /// The chord as written.
    pub chord: String,
    /// What it runs.
    pub target: BindTarget,
}

/// The window registrations of one VM, in registration order.
#[derive(Debug, Default)]
pub struct WindowRegistry {
    /// Commands by registration; re-registering an id replaces its entry in place.
    pub commands: Vec<CommandDef>,
    /// Binds, including commands' `keys`.
    pub binds: Vec<Bind>,
    /// How many function binds there are.
    pub function_binds: usize,
    /// `(command id, fn)`.
    pub overrides: Vec<(String, Function)>,
    /// `tern.route.open` handlers.
    pub route_open: Vec<Function>,
    /// `tern.route.link` handlers.
    pub route_link: Vec<Function>,
    /// `tern.chrome.tab_title`.
    pub tab_title: Option<Function>,
    /// `tern.chrome.window_title`.
    pub window_title: Option<Function>,
    /// `tern.chrome.status`.
    pub status: Option<Function>,
    /// `tern.css` sheets `(name, source)`, in first-install order.
    pub css: Vec<(String, String)>,
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

pub(super) fn install(lua: &Lua, tern: &Table, state: &Rc<ApiState>) -> mlua::Result<()> {
    let s = Rc::clone(state);
    tern.set(
        "on",
        lua.create_function(move |_, (name, f): (String, Function)| {
            if !WINDOW_EVENTS.contains(&name.as_str()) {
                return Err(mlua::Error::runtime(format!(
                    "tern.on: unknown window event {name:?} ({})",
                    WINDOW_EVENTS.join(", ")
                )));
            }
            s.register_event(&name, f);
            s.registrations_changed.set(true);
            Ok(())
        })?,
    )?;

    let s = Rc::clone(state);
    tern.set(
        "command",
        lua.create_function(move |_, def: Table| {
            let id: String = def.get("id")?;
            if !valid_id(&id) || id == "bind" {
                return Err(mlua::Error::runtime(format!(
                    "tern.command: bad id `{id}` (letters, digits, `_` and `-`; not `bind`)"
                )));
            }
            let title: String = def.get("title")?;
            let run: Function = def.get("run")?;
            let icon =
                crate::icons::resolve(def.get::<Option<String>>("icon")?.as_deref()).to_owned();
            let keys: Vec<String> = def.get::<Option<Vec<String>>>("keys")?.unwrap_or_default();
            let command = CommandDef {
                id: id.clone(),
                title,
                icon,
                group: def.get("group")?,
                keys: keys.clone(),
                available: def.get("available")?,
                run,
            };
            let mut reg = s.registry.borrow_mut();
            let w = &mut reg.window;
            match w.commands.iter_mut().find(|c| c.id == id) {
                Some(existing) => *existing = command,
                None => w.commands.push(command),
            }
            let action = format!("plugin.{}.{id}", s.plugin.id);
            for chord in keys {
                w.binds.push(Bind {
                    chord,
                    target: BindTarget::Action(action.clone()),
                });
            }
            s.registrations_changed.set(true);
            Ok(())
        })?,
    )?;

    let s = Rc::clone(state);
    tern.set(
        "bind",
        lua.create_function(move |_, (chord, action): (String, Value)| {
            let mut reg = s.registry.borrow_mut();
            let w = &mut reg.window;
            let target = match action {
                Value::String(name) => BindTarget::Action(name.to_str()?.to_owned()),
                Value::Function(f) => {
                    let n = w.function_binds;
                    w.function_binds += 1;
                    BindTarget::Function(f, n)
                }
                _ => {
                    return Err(mlua::Error::runtime(
                        "tern.bind: the action is a name or a function",
                    ));
                }
            };
            w.binds.push(Bind { chord, target });
            s.registrations_changed.set(true);
            Ok(())
        })?,
    )?;

    let s = Rc::clone(state);
    tern.set(
        "override",
        lua.create_function(move |_, (id, f): (String, Function)| {
            if let Some(known) = &s.builtin_commands
                && !known.contains(&id)
            {
                return Err(mlua::Error::runtime(format!(
                    "tern.override: unknown command `{id}`"
                )));
            }
            s.registry.borrow_mut().window.overrides.push((id, f));
            s.registrations_changed.set(true);
            Ok(())
        })?,
    )?;

    let route = lua.create_table()?;
    let s = Rc::clone(state);
    route.set(
        "open",
        lua.create_function(move |_, f: Function| {
            s.registry.borrow_mut().window.route_open.push(f);
            s.registrations_changed.set(true);
            Ok(())
        })?,
    )?;
    let s = Rc::clone(state);
    route.set(
        "link",
        lua.create_function(move |_, f: Function| {
            s.registry.borrow_mut().window.route_link.push(f);
            s.registrations_changed.set(true);
            Ok(())
        })?,
    )?;
    route.set_readonly(true);
    tern.set("route", route)?;

    let chrome = lua.create_table()?;
    for slot in ["tab_title", "window_title", "status"] {
        let s = Rc::clone(state);
        chrome.set(
            slot,
            lua.create_function(move |_, f: Function| {
                let mut reg = s.registry.borrow_mut();
                let w = &mut reg.window;
                match slot {
                    "tab_title" => w.tab_title = Some(f),
                    "window_title" => w.window_title = Some(f),
                    _ => w.status = Some(f),
                }
                s.registrations_changed.set(true);
                Ok(())
            })?,
        )?;
    }
    let s = Rc::clone(state);
    chrome.set(
        "refresh",
        lua.create_function(move |_, ()| {
            if s.in_formatter.get() {
                return Err(mlua::Error::runtime(
                    "tern.chrome.refresh: not from a formatter",
                ));
            }
            s.chrome_refresh.set(true);
            Ok(())
        })?,
    )?;
    chrome.set_readonly(true);
    tern.set("chrome", chrome)?;

    let s = Rc::clone(state);
    tern.set(
        "css",
        lua.create_function(move |_, (name, source): (String, String)| {
            if !valid_id(&name) || name == "styles" {
                return Err(mlua::Error::runtime(format!(
                    "tern.css: bad name `{name}` (letters, digits, `_` and `-`; not `styles`)"
                )));
            }
            let mut reg = s.registry.borrow_mut();
            let css = &mut reg.window.css;
            match css.iter_mut().find(|(n, _)| *n == name) {
                Some(entry) => entry.1 = source,
                None => css.push((name, source)),
            }
            s.registrations_changed.set(true);
            Ok(())
        })?,
    )?;
    Ok(())
}
