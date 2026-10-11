//! `tern.process.run`: an argv, no shell, run off the owner
//! thread; at most 16 MiB per output stream; an optional timeout.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use mlua::{Function, Lua, Table, Value};

use super::ApiState;
use super::tasks::{Completer, Outcome};

/// Largest captured output per stream.
pub const MAX_OUTPUT: usize = 16 * 1024 * 1024;

/// A finished process.
#[derive(Debug, Clone, Default)]
pub struct ProcessResult {
    /// Exit code; -1 when killed or ended by a signal.
    pub status: i64,
    /// Captured stdout.
    pub stdout: Vec<u8>,
    /// Captured stderr.
    pub stderr: Vec<u8>,
    /// Whether the timeout killed it.
    pub timed_out: bool,
}

impl ProcessResult {
    pub(crate) fn to_lua(&self, lua: &Lua) -> mlua::Result<Value> {
        let t = lua.create_table()?;
        t.set("status", self.status as f64)?;
        t.set("stdout", lua.create_string(&self.stdout)?)?;
        t.set("stderr", lua.create_string(&self.stderr)?)?;
        t.set("timed_out", self.timed_out)?;
        Ok(Value::Table(t))
    }
}

struct Options {
    cwd: Option<String>,
    env: Vec<(String, String)>,
    stdin: Option<Vec<u8>>,
    timeout: Option<Duration>,
}

fn options(t: Option<Table>) -> mlua::Result<Options> {
    let mut o = Options {
        cwd: None,
        env: Vec::new(),
        stdin: None,
        timeout: None,
    };
    let Some(t) = t else { return Ok(o) };
    o.cwd = t.get("cwd")?;
    if let Some(env) = t.get::<Option<Table>>("env")? {
        for pair in env.pairs::<String, String>() {
            o.env.push(pair?);
        }
        o.env.sort();
    }
    o.stdin = t
        .get::<Option<mlua::LuaString>>("stdin")?
        .map(|s| s.as_bytes().to_vec());
    if let Some(ms) = t.get::<Option<f64>>("timeout_ms")?
        && ms > 0.0
        && ms.is_finite()
    {
        o.timeout = Some(Duration::from_secs_f64(ms / 1000.0));
    }
    Ok(o)
}

/// Reads up to [`MAX_OUTPUT`] bytes, then reads and discards the rest.
fn read_capped(mut r: impl Read) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        match r.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let room = MAX_OUTPUT.saturating_sub(out.len());
                out.extend_from_slice(&buf[..n.min(room)]);
            }
        }
    }
    out
}

fn resolve(dir: &Path, path: &str) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        p.into()
    } else {
        dir.join(p)
    }
}

/// Starts `argv` now (spawn failures raise) and finishes it on its own thread.
fn spawn(
    state: &ApiState,
    argv: &[String],
    opts: Options,
    completer: Completer,
) -> mlua::Result<()> {
    let program = &argv[0];
    let mut cmd = Command::new(program);
    cmd.args(&argv[1..]);
    let cwd = opts.cwd.as_deref().map_or_else(
        || state.plugin.dir.clone(),
        |c| resolve(&state.plugin.dir, c),
    );
    cmd.current_dir(cwd);
    cmd.env("TERN_PLUGIN_DATA", &state.plugin.data);
    cmd.env("AUK_PLUGIN_DATA", &state.plugin.data);
    for (k, v) in &opts.env {
        cmd.env(k, v);
    }
    cmd.stdin(if opts.stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| mlua::Error::runtime(format!("tern.process.run {program}: {e}")))?;
    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let input = opts.stdin;
    let timeout = opts.timeout;
    std::thread::Builder::new()
        .name("plugin process".into())
        .spawn(move || {
            if let (Some(mut pipe), Some(bytes)) = (stdin, input) {
                std::thread::spawn(move || {
                    let _ = pipe.write_all(&bytes);
                });
            }
            let out = stdout.map(|p| std::thread::spawn(move || read_capped(p)));
            let err = stderr.map(|p| std::thread::spawn(move || read_capped(p)));
            let started = Instant::now();
            let mut timed_out = false;
            let status = loop {
                match child.try_wait() {
                    Ok(Some(status)) => break status.code().map_or(-1, i64::from),
                    Ok(None) => {}
                    Err(_) => break -1,
                }
                if let Some(limit) = timeout
                    && started.elapsed() >= limit
                {
                    let _ = child.kill();
                    let _ = child.wait();
                    timed_out = true;
                    break -1;
                }
                std::thread::sleep(Duration::from_millis(2));
            };
            let stdout = out.and_then(|h| h.join().ok()).unwrap_or_default();
            let stderr = err.and_then(|h| h.join().ok()).unwrap_or_default();
            completer.complete(Outcome::Process(ProcessResult {
                status,
                stdout,
                stderr,
                timed_out,
            }));
        })
        .map_err(|e| mlua::Error::runtime(format!("tern.process.run {program}: {e}")))?;
    Ok(())
}

/// `tern.process.run(argv, opts?, cb?)`.
pub(super) fn run(
    lua: &Lua,
    state: &std::rc::Rc<ApiState>,
    args: mlua::MultiValue,
) -> mlua::Result<Value> {
    const BAD: &str = "tern.process.run(argv, opts?, cb): bad arguments";
    let args: Vec<Value> = args.into_iter().collect();
    let (argv, opts, cb): (Table, Option<Table>, Option<Function>) = match args.as_slice() {
        [Value::Table(a)] => (a.clone(), None, None),
        [Value::Table(a), Value::Nil] => (a.clone(), None, None),
        [Value::Table(a), Value::Table(o)] => (a.clone(), Some(o.clone()), None),
        [Value::Table(a), Value::Function(f)] => (a.clone(), None, Some(f.clone())),
        [Value::Table(a), Value::Table(o), Value::Function(f)] => {
            (a.clone(), Some(o.clone()), Some(f.clone()))
        }
        [Value::Table(a), Value::Nil, Value::Function(f)] => (a.clone(), None, Some(f.clone())),
        [Value::Table(a), Value::Table(o), Value::Nil] => (a.clone(), Some(o.clone()), None),
        _ => return Err(mlua::Error::runtime(BAD)),
    };
    let mut words = Vec::new();
    for word in argv.sequence_values::<Value>() {
        match word? {
            Value::String(s) => words.push(s.to_str()?.to_owned()),
            Value::Number(n) => words.push(n.to_string()),
            Value::Integer(n) => words.push(n.to_string()),
            _ => return Err(mlua::Error::runtime(BAD)),
        }
    }
    if words.is_empty() {
        return Err(mlua::Error::runtime("tern.process.run: argv is empty"));
    }
    let opts = options(opts).map_err(|_| mlua::Error::runtime(BAD))?;
    let callback = cb.map(|f| ("process", f));
    let awaitable = callback.is_none();
    let (id, completer) = state.tasks.borrow_mut().start(callback);
    if let Err(e) = spawn(state, &words, opts, completer) {
        state.tasks.borrow_mut().forget(id);
        return Err(e);
    }
    if awaitable {
        super::awaitable(lua, state, "process", id)
    } else {
        Ok(Value::Nil)
    }
}
