//! `tern.json`, `tern.base64` and the native half of `tern.parse.json_tree` (§9.9,
//! research/plugins.md §8.8, §11.5).

use base64::Engine as _;
use mlua::{Lua, Table, Value};
use serde_json::{Value as Json, json};

use super::convert::{to_json, to_lua};

pub(super) fn install_json(lua: &Lua, tern: &Table) -> mlua::Result<()> {
    let t = lua.create_table()?;
    t.set(
        "encode",
        lua.create_function(|lua, (value, pretty): (Value, Option<bool>)| {
            let json = to_json(lua, &value).map_err(mlua::Error::runtime)?;
            let text = if pretty == Some(true) {
                serde_json::to_string_pretty(&json)
            } else {
                serde_json::to_string(&json)
            }
            .map_err(|e| mlua::Error::runtime(e.to_string()))?;
            Ok(text)
        })?,
    )?;
    t.set(
        "decode",
        lua.create_function(|lua, text: mlua::LuaString| {
            let json: Json = serde_json::from_slice(&text.as_bytes())
                .map_err(|e| mlua::Error::runtime(format!("tern.json.decode: {e}")))?;
            to_lua(lua, &json)
        })?,
    )?;
    t.set(
        "array",
        lua.create_function(|lua, t: Option<Table>| {
            let t = match t {
                Some(t) => t,
                None => lua.create_table()?,
            };
            t.set_metatable(Some(mlua::LuaSerdeExt::array_metatable(lua)))?;
            Ok(t)
        })?,
    )?;
    t.set("null", Value::NULL)?;
    t.set_readonly(true);
    tern.set("json", t)?;

    let b = lua.create_table()?;
    b.set(
        "encode",
        lua.create_function(|_, (bytes, opts): (mlua::LuaString, Option<Table>)| {
            let url = match opts {
                Some(o) => o.get::<Option<bool>>("url")?.unwrap_or(false),
                None => false,
            };
            Ok(if url {
                base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes.as_bytes())
            } else {
                base64::engine::general_purpose::STANDARD.encode(bytes.as_bytes())
            })
        })?,
    )?;
    b.set(
        "decode",
        lua.create_function(|lua, text: mlua::LuaString| {
            let clean: Vec<u8> = text
                .as_bytes()
                .iter()
                .copied()
                .filter(|c| !c.is_ascii_whitespace())
                .map(|c| match c {
                    b'-' => b'+',
                    b'_' => b'/',
                    c => c,
                })
                .filter(|&c| c != b'=')
                .collect();
            let bytes = base64::engine::general_purpose::STANDARD_NO_PAD
                .decode(&clean)
                .map_err(|e| mlua::Error::runtime(format!("tern.base64.decode: {e}")))?;
            lua.create_string(bytes)
        })?,
    )?;
    b.set_readonly(true);
    tern.set("base64", b)
}

/// Nodes a `json_tree` shows before summarizing the rest.
const TREE_NODES: usize = 5000;

struct TreeBuilder {
    next: usize,
    hidden: usize,
}

fn scalar_span(v: &Json) -> Json {
    match v {
        Json::String(s) => json!({"t": Json::String(s.clone()).to_string(), "s": "success"}),
        Json::Number(n) => json!({"t": n.to_string(), "s": "num"}),
        Json::Bool(b) => json!({"t": b.to_string(), "s": "accent"}),
        _ => json!({"t": "null", "s": "accent"}),
    }
}

impl TreeBuilder {
    fn node(&mut self, key: Option<String>, v: &Json, depth: usize) -> Option<Json> {
        if self.next >= TREE_NODES {
            self.hidden += 1;
            return None;
        }
        let id = format!("t{}", self.next);
        self.next += 1;
        let mut label = Vec::new();
        if let Some(k) = key {
            label.push(json!({"t": k, "s": "info"}));
            label.push(json!({"t": ": ", "s": "muted"}));
        }
        let mut out = json!({"id": id});
        match v {
            Json::Object(map) => {
                label.push(json!({"t": format!("{{{}}}", map.len()), "s": "muted"}));
                let children: Vec<Json> = map
                    .iter()
                    .filter_map(|(k, v)| self.node(Some(k.clone()), v, depth + 1))
                    .collect();
                if !children.is_empty() {
                    out["open"] = Json::Bool(depth < 2);
                    out["children"] = Json::Array(children);
                }
            }
            Json::Array(items) => {
                label.push(json!({"t": format!("[{}]", items.len()), "s": "muted"}));
                let children: Vec<Json> = items
                    .iter()
                    .enumerate()
                    .filter_map(|(i, v)| self.node(Some(i.to_string()), v, depth + 1))
                    .collect();
                if !children.is_empty() {
                    out["open"] = Json::Bool(depth < 2);
                    out["children"] = Json::Array(children);
                }
            }
            scalar => label.push(scalar_span(scalar)),
        }
        out["label"] = Json::Array(label);
        Some(out)
    }
}

/// A JSON document (or a stream of them) as a collapsible `tree` node; nil when the text is not
/// JSON or holds only scalars.
pub(super) fn json_tree(lua: &Lua, text: mlua::LuaString) -> mlua::Result<Value> {
    let bytes = text.as_bytes();
    let docs: Result<Vec<Json>, _> = serde_json::Deserializer::from_slice(&bytes)
        .into_iter::<Json>()
        .collect();
    let Ok(docs) = docs else {
        return Ok(Value::Nil);
    };
    if docs.is_empty() || docs.iter().all(|d| !d.is_object() && !d.is_array()) {
        return Ok(Value::Nil);
    }
    let mut b = TreeBuilder { next: 0, hidden: 0 };
    let mut nodes: Vec<Json> = Vec::new();
    if docs.len() == 1 {
        match &docs[0] {
            Json::Object(map) => {
                for (k, v) in map {
                    nodes.extend(b.node(Some(k.clone()), v, 0));
                }
            }
            Json::Array(items) => {
                for (i, v) in items.iter().enumerate() {
                    nodes.extend(b.node(Some(i.to_string()), v, 0));
                }
            }
            _ => {}
        }
    } else {
        for d in &docs {
            nodes.extend(b.node(None, d, 0));
        }
    }
    if b.hidden > 0 {
        nodes.push(json!({
            "id": format!("t{}", b.next),
            "label": [{"t": format!("… {} more", b.hidden), "s": "muted"}],
        }));
    }
    to_lua(lua, &json!({"k": "tree", "p": {"nodes": nodes}}))
}
