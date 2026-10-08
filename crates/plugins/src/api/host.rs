//! Host-only members: `tern.on` (host events), `tern.block.define`, `tern.lens.define` and
//! `tern.pane`. Nothing here names a window (AC14): effects target a pane or the whole host.

use std::rc::Rc;

use mlua::{Function, Lua, Table, Value};

use super::{ApiState, BlockHandlers, LensHandlers};
use crate::events::HOST_EVENTS;

pub(super) fn install(lua: &Lua, tern: &Table, state: &Rc<ApiState>) -> mlua::Result<()> {
    let on_state = Rc::clone(state);
    tern.set(
        "on",
        lua.create_function(move |_, (name, f): (String, Function)| {
            if !HOST_EVENTS.contains(&name.as_str()) {
                return Err(mlua::Error::runtime(format!(
                    "tern.on: unknown host event {name:?} ({})",
                    HOST_EVENTS.join(", ")
                )));
            }
            on_state.register_event(&name, f);
            Ok(())
        })?,
    )?;

    let block = lua.create_table()?;
    let block_state = Rc::clone(state);
    block.set(
        "define",
        lua.create_function(move |_, (id, def): (String, Table)| {
            if !block_state.declared_blocks.contains(&id) {
                return Err(mlua::Error::runtime(format!(
                    "tern.block.define: block {id:?} is not declared in plugin.toml"
                )));
            }
            let handlers = BlockHandlers {
                init: required(&def, "tern.block.define", &id, "init")?,
                view: required(&def, "tern.block.define", &id, "view")?,
                title: optional(&def, "tern.block.define", &id, "title")?,
                key: optional(&def, "tern.block.define", &id, "key")?,
                event: optional(&def, "tern.block.define", &id, "event")?,
                resize: optional(&def, "tern.block.define", &id, "resize")?,
                save: optional(&def, "tern.block.define", &id, "save")?,
            };
            block_state
                .registry
                .borrow_mut()
                .blocks
                .insert(id, handlers);
            Ok(())
        })?,
    )?;
    tern.set("block", &block)?;

    let lens = lua.create_table()?;
    let lens_state = Rc::clone(state);
    lens.set(
        "define",
        lua.create_function(move |_, (id, def): (String, Table)| {
            if !lens_state.declared_lenses.contains(&id) {
                return Err(mlua::Error::runtime(format!(
                    "tern.lens.define: lens {id:?} is not declared in plugin.toml"
                )));
            }
            let handlers = LensHandlers {
                open: required(&def, "tern.lens.define", &id, "open")?,
                line: required(&def, "tern.lens.define", &id, "line")?,
                finish: required(&def, "tern.lens.define", &id, "finish")?,
                view: required(&def, "tern.lens.define", &id, "view")?,
                event: optional(&def, "tern.lens.define", &id, "event")?,
            };
            lens_state.registry.borrow_mut().lenses.insert(id, handlers);
            Ok(())
        })?,
    )?;
    tern.set("lens", &lens)?;

    let pane = lua.create_table()?;
    let writes = Rc::clone(state);
    pane.set(
        "write",
        lua.create_function(move |_, (pane, text): (f64, mlua::LuaString)| {
            if pane >= 0.0 && pane.fract() == 0.0 {
                writes
                    .pane_writes
                    .borrow_mut()
                    .push((pane as u64, text.to_string_lossy()));
            }
            Ok(())
        })?,
    )?;
    let lister = Rc::clone(state);
    pane.set(
        "list",
        lua.create_function(move |lua, ()| {
            let panes = lister.panes.as_ref().map(|p| p.list()).unwrap_or_default();
            let out = lua.create_table_with_capacity(panes.len(), 0)?;
            for (i, p) in panes.iter().enumerate() {
                let t = lua.create_table()?;
                t.set("pane", p.pane as f64)?;
                t.set("cwd", p.cwd.as_deref())?;
                t.set("program", p.program.as_str())?;
                t.set("title", p.title.as_str())?;
                t.set("busy", p.busy)?;
                out.raw_set(i + 1, t)?;
            }
            Ok(out)
        })?,
    )?;
    pane.set_readonly(true);
    tern.set("pane", pane)?;
    block.set_readonly(true);
    lens.set_readonly(true);
    Ok(())
}

fn required(def: &Table, api: &str, id: &str, key: &str) -> mlua::Result<Function> {
    optional(def, api, id, key)?
        .ok_or_else(|| mlua::Error::runtime(format!("{api}: {id}: `{key}` must be a function")))
}

fn optional(def: &Table, api: &str, id: &str, key: &str) -> mlua::Result<Option<Function>> {
    match def.raw_get::<Value>(key)? {
        Value::Nil => Ok(None),
        Value::Function(f) => Ok(Some(f)),
        _ => Err(mlua::Error::runtime(format!(
            "{api}: {id}: `{key}` must be a function"
        ))),
    }
}
