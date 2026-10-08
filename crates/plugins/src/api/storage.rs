//! `tern.kv` and `tern.fs` (§9.9, research/plugins.md §11.1, §11.2).
//!
//! `tern.kv` is one `<data>/kv.json` per plugin per machine, shared by every VM of the plugin:
//! each VM caches the map and reads the file again when its mtime or length changes; every `set`
//! rewrites the whole file through a temp file and a rename (the last writer wins).

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::SystemTime;

use mlua::{Lua, Table, Value};
use serde_json::Value as Json;

use super::ApiState;
use super::convert::{to_json, to_lua};

/// What a VM knows of `kv.json`.
#[derive(Debug, Default)]
pub(crate) struct KvCache {
    /// `(mtime, length)` of the file the map was read from; `None` when it was absent.
    stamp: Option<Option<(SystemTime, u64)>>,
    map: BTreeMap<String, Json>,
}

fn kv_path(state: &ApiState) -> PathBuf {
    state.plugin.data.join("kv.json")
}

fn stamp(path: &Path) -> Result<Option<(SystemTime, u64)>, std::io::Error> {
    match std::fs::metadata(path) {
        Ok(m) => Ok(Some((m.modified()?, m.len()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Brings the cache up to date with the file. Errors keep the last good cache but are raised.
fn refresh(state: &ApiState, verb: &str) -> mlua::Result<()> {
    let path = kv_path(state);
    let now = stamp(&path).map_err(|e| mlua::Error::runtime(format!("tern.kv.{verb}: {e}")))?;
    let mut cache = state.kv.borrow_mut();
    if cache.stamp == Some(now) {
        return Ok(());
    }
    let map = match now {
        None => BTreeMap::new(),
        Some(_) => {
            let text = std::fs::read(&path)
                .map_err(|e| mlua::Error::runtime(format!("tern.kv.{verb}: {e}")))?;
            let json: Json = serde_json::from_slice(&text).map_err(|e| {
                mlua::Error::runtime(format!("tern.kv.{verb}: {}: {e}", path.display()))
            })?;
            match json {
                Json::Object(m) => m.into_iter().collect(),
                _ => {
                    return Err(mlua::Error::runtime(format!(
                        "tern.kv.{verb}: {} does not hold a JSON object",
                        path.display()
                    )));
                }
            }
        }
    };
    cache.map = map;
    cache.stamp = Some(now);
    Ok(())
}

fn write(state: &ApiState) -> mlua::Result<()> {
    let path = kv_path(state);
    let fail = |e: std::io::Error| mlua::Error::runtime(format!("tern.kv.set: {e}"));
    let mut cache = state.kv.borrow_mut();
    let text = serde_json::to_string_pretty(&cache.map)
        .map_err(|e| mlua::Error::runtime(format!("tern.kv.set: {e}")))?;
    std::fs::create_dir_all(&state.plugin.data).map_err(fail)?;
    let tmp = state
        .plugin
        .data
        .join(format!(".kv.json.{}.{:p}.tmp", std::process::id(), &*cache));
    std::fs::write(&tmp, text).map_err(fail)?;
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        fail(e)
    })?;
    cache.stamp = Some(stamp(&path).map_err(fail)?);
    Ok(())
}

pub(super) fn install_kv(lua: &Lua, tern: &Table, state: &Rc<ApiState>) -> mlua::Result<()> {
    let kv = lua.create_table()?;
    let s = Rc::clone(state);
    kv.set(
        "get",
        lua.create_function(move |lua, key: String| {
            refresh(&s, "get")?;
            let cache = s.kv.borrow();
            match cache.map.get(&key) {
                Some(v) => to_lua(lua, v),
                None => Ok(Value::Nil),
            }
        })?,
    )?;
    let s = Rc::clone(state);
    kv.set(
        "set",
        lua.create_function(move |lua, (key, value): (String, Value)| {
            refresh(&s, "set")?;
            let json = to_json(lua, &value).map_err(mlua::Error::runtime)?;
            {
                let mut cache = s.kv.borrow_mut();
                if json.is_null() {
                    cache.map.remove(&key);
                } else {
                    cache.map.insert(key, json);
                }
            }
            write(&s)
        })?,
    )?;
    kv.set_readonly(true);
    tern.set("kv", kv)
}

fn resolve(state: &ApiState, path: &str) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        state.plugin.dir.join(p)
    }
}

fn fs_error(verb: &str, path: &Path, e: impl std::fmt::Display) -> mlua::Error {
    mlua::Error::runtime(format!("tern.fs.{verb} {}: {e}", path.display()))
}

/// Reads a regular, non-symlink file of at most `max` bytes, rejecting larger ones before
/// reading them.
fn read_capped(path: &Path, max: u64) -> Result<Vec<u8>, String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // No final-component symlinks, and never block opening a FIFO.
        options.custom_flags(libc_flags());
    }
    let file = options.open(path).map_err(|e| e.to_string())?;
    let meta = file.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err("not a regular file".into());
    }
    if meta.len() > max {
        return Err(format!("larger than {max} bytes"));
    }
    let mut out = Vec::with_capacity(meta.len() as usize);
    file.take(max + 1)
        .read_to_end(&mut out)
        .map_err(|e| e.to_string())?;
    if out.len() as u64 > max {
        return Err(format!("larger than {max} bytes"));
    }
    Ok(out)
}

#[cfg(unix)]
fn libc_flags() -> i32 {
    // O_NOFOLLOW and O_NONBLOCK; the values are fixed per platform.
    #[cfg(target_os = "macos")]
    {
        0x0100 | 0x0004
    }
    #[cfg(target_os = "linux")]
    {
        0o400_000 | 0o4000
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        0
    }
}

pub(super) fn install_fs(lua: &Lua, tern: &Table, state: &Rc<ApiState>) -> mlua::Result<()> {
    let fs = lua.create_table()?;
    let s = Rc::clone(state);
    fs.set(
        "read",
        lua.create_function(move |lua, (path, max): (String, Option<f64>)| {
            let p = resolve(&s, &path);
            let bytes = match max {
                Some(max) if max >= 0.0 => {
                    read_capped(&p, max as u64).map_err(|e| fs_error("read", &p, e))?
                }
                Some(_) => return Err(fs_error("read", &p, "max_bytes must not be negative")),
                None => std::fs::read(&p).map_err(|e| fs_error("read", &p, e))?,
            };
            lua.create_string(bytes)
        })?,
    )?;
    let s = Rc::clone(state);
    fs.set(
        "write",
        lua.create_function(move |_, (path, text): (String, mlua::LuaString)| {
            let p = resolve(&s, &path);
            std::fs::write(&p, text.as_bytes()).map_err(|e| fs_error("write", &p, e))
        })?,
    )?;
    let s = Rc::clone(state);
    fs.set(
        "list",
        lua.create_function(move |lua, path: String| {
            let p = resolve(&s, &path);
            let mut names: Vec<String> = std::fs::read_dir(&p)
                .map_err(|e| fs_error("list", &p, e))?
                .filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            lua.create_sequence_from(names)
        })?,
    )?;
    let s = Rc::clone(state);
    fs.set(
        "exists",
        lua.create_function(move |_, path: String| Ok(resolve(&s, &path).exists()))?,
    )?;
    let s = Rc::clone(state);
    fs.set(
        "mkdir",
        lua.create_function(move |_, path: String| {
            let p = resolve(&s, &path);
            std::fs::create_dir_all(&p).map_err(|e| fs_error("mkdir", &p, e))
        })?,
    )?;
    let s = Rc::clone(state);
    fs.set(
        "remove",
        lua.create_function(move |_, path: String| {
            let p = resolve(&s, &path);
            let meta = std::fs::symlink_metadata(&p).map_err(|e| fs_error("remove", &p, e))?;
            if meta.is_dir() {
                std::fs::remove_dir_all(&p)
            } else {
                std::fs::remove_file(&p)
            }
            .map_err(|e| fs_error("remove", &p, e))
        })?,
    )?;
    fs.set_readonly(true);
    tern.set("fs", fs)
}
