//! Lua ⇄ JSON, with Tern's rules. The same conversion carries view
//! trees, block state, `tern.kv` values, events and `tern.json`.
//!
//! - A table with keys exactly 1..n (n > 0), or one tagged with `tern.json.array`, is an array;
//!   any other non-empty table is an object with sorted keys (number keys become strings); an
//!   empty table is `{}` unless tagged.
//! - Integral numbers within ±2^53 are JSON integers.
//! - `tern.json.null` is null; decoding keeps nulls in arrays as `tern.json.null` and drops null
//!   members.

use mlua::{LuaSerdeExt, Table, Value};
use serde_json::{Map, Number, Value as Json};

/// Deepest nesting `encode` accepts.
pub const MAX_DEPTH: usize = 128;
/// Largest integer JSON keeps exact.
const EXACT: f64 = 9_007_199_254_740_992.0;

/// Formats a number the way Luau's `tostring` does for error messages.
fn number_text(n: f64) -> String {
    if n.is_nan() {
        "nan".into()
    } else if n.is_infinite() {
        if n > 0.0 { "inf".into() } else { "-inf".into() }
    } else {
        n.to_string()
    }
}

/// A JSON number for `n`, or the encode error.
pub fn json_number(n: f64) -> Result<Json, String> {
    if !n.is_finite() {
        return Err(format!("cannot encode {} as JSON", number_text(n)));
    }
    if n.fract() == 0.0 && n.abs() <= EXACT {
        // Exact within ±2^53, so the cast is lossless.
        #[allow(clippy::cast_possible_truncation, reason = "integral and within ±2^53")]
        return Ok(Json::Number(Number::from(n as i64)));
    }
    Ok(Number::from_f64(n).map_or(Json::Null, Json::Number))
}

fn key_text(key: &Value) -> Result<String, String> {
    match key {
        Value::String(s) => Ok(s.to_string_lossy()),
        Value::Integer(i) => Ok(i.to_string()),
        Value::Number(n) => match json_number(*n)? {
            Json::Number(num) => Ok(num.to_string()),
            _ => Err(format!("cannot encode {} as JSON", number_text(*n))),
        },
        other => Err(format!("cannot encode a {} key as JSON", other.type_name())),
    }
}

/// Whether `table` carries the array tag.
pub fn is_array_tagged(lua: &mlua::Lua, table: &Table) -> bool {
    table
        .metatable()
        .is_some_and(|mt| mt == lua.array_metatable())
}

/// Converts a Lua value to JSON.
pub fn to_json(lua: &mlua::Lua, value: &Value) -> Result<Json, String> {
    to_json_at(lua, value, 0)
}

fn to_json_at(lua: &mlua::Lua, value: &Value, depth: usize) -> Result<Json, String> {
    match value {
        Value::Nil => Ok(Json::Null),
        Value::Boolean(b) => Ok(Json::Bool(*b)),
        Value::Integer(i) => json_number(*i as f64),
        Value::Number(n) => json_number(*n),
        Value::String(s) => Ok(Json::String(s.to_string_lossy())),
        Value::LightUserData(p) if p.0.is_null() => Ok(Json::Null),
        Value::Table(t) => {
            if depth >= MAX_DEPTH {
                return Err("cannot encode tables nested this deep (or cyclic) as JSON".into());
            }
            table_to_json(lua, t, depth + 1)
        }
        other => Err(format!("cannot encode a {} as JSON", other.type_name())),
    }
}

fn table_to_json(lua: &mlua::Lua, t: &Table, depth: usize) -> Result<Json, String> {
    let tagged = is_array_tagged(lua, t);
    let mut entries: Vec<(Value, Value)> = Vec::new();
    for pair in t.pairs::<Value, Value>() {
        entries.push(pair.map_err(|e| e.to_string())?);
    }
    let n = entries.len();
    let is_seq = n > 0
        && entries.iter().all(|(k, _)| match k {
            Value::Integer(i) => *i >= 1 && (*i as usize) <= n,
            Value::Number(f) => f.fract() == 0.0 && *f >= 1.0 && *f <= n as f64,
            _ => false,
        });
    if tagged || is_seq {
        let len = if tagged { t.raw_len() } else { n };
        let mut out = Vec::with_capacity(len);
        for i in 1..=len {
            let v: Value = t.raw_get(i).map_err(|e| e.to_string())?;
            out.push(to_json_at(lua, &v, depth)?);
        }
        return Ok(Json::Array(out));
    }
    let mut keyed: Vec<(String, Value)> = Vec::with_capacity(n);
    for (k, v) in entries {
        keyed.push((key_text(&k)?, v));
    }
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    let mut map = Map::new();
    for (k, v) in keyed {
        map.insert(k, to_json_at(lua, &v, depth)?);
    }
    Ok(Json::Object(map))
}

/// Converts JSON to a Lua value: arrays come back tagged, nulls in arrays as `tern.json.null`,
/// null members dropped, numbers as doubles.
pub fn to_lua(lua: &mlua::Lua, json: &Json) -> mlua::Result<Value> {
    Ok(match json {
        Json::Null => Value::NULL,
        Json::Bool(b) => Value::Boolean(*b),
        Json::Number(n) => Value::Number(n.as_f64().unwrap_or(0.0)),
        Json::String(s) => Value::String(lua.create_string(s)?),
        Json::Array(items) => {
            let t = lua.create_table_with_capacity(items.len(), 0)?;
            for (i, item) in items.iter().enumerate() {
                t.raw_set(i + 1, to_lua(lua, item)?)?;
            }
            t.set_metatable(Some(lua.array_metatable()))?;
            Value::Table(t)
        }
        Json::Object(map) => {
            let t = lua.create_table_with_capacity(0, map.len())?;
            for (k, v) in map {
                if !v.is_null() {
                    t.raw_set(k.as_str(), to_lua(lua, v)?)?;
                }
            }
            Value::Table(t)
        }
    })
}
