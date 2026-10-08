//! Window-only registration. Now: `tern.on` with the window event names. Commands, binds,
//! overrides, routes, chrome formatters and `tern.css` come with M9.

use std::rc::Rc;

use mlua::{Function, Lua, Table};

use super::ApiState;
use crate::events::WINDOW_EVENTS;

pub(super) fn install(lua: &Lua, tern: &Table, state: &Rc<ApiState>) -> mlua::Result<()> {
    let on_state = Rc::clone(state);
    tern.set(
        "on",
        lua.create_function(move |_, (name, f): (String, Function)| {
            if !WINDOW_EVENTS.contains(&name.as_str()) {
                return Err(mlua::Error::runtime(format!(
                    "tern.on: unknown window event {name:?} ({})",
                    WINDOW_EVENTS.join(", ")
                )));
            }
            on_state.register_event(&name, f);
            Ok(())
        })?,
    )?;
    Ok(())
}
