//! The daemon's control-endpoint commands (§11.1, §11.3). One line in, one JSON line out; words
//! are split like a shell's (`shlex`).
//!
//! | Command | Does |
//! | --- | --- |
//! | `ping`, `version` | liveness and the daemon's versions |
//! | `ls` | panes and the layout |
//! | `new [session\|tab] [--window K] [--session ID] [--name N] [--cwd DIR] [--size CxR] [-- CMD…]` | spawns a pane |
//! | `split TARGET right\|down [--cwd DIR] [-- CMD…]` | splits a pane |
//! | `run TARGET WORD…` | types the words and Enter |
//! | `send TARGET text\|paste WORD…`, `send TARGET keys KEY…` | sends input |
//! | `capture TARGET [--ansi\|--html] [--scrollback] [--surfaces]` | the pane's content (`capture.rs`) |
//! | `wait TARGET --until prompt\|exit\|idle=MS [--timeout S]` | waits (`"timeout":true` on timeout) |
//! | `state-hash TARGET` | the authoritative replica's hash and sequence number |
//! | `marks TARGET` | the shell's command marks (§5.3) |
//! | `tty-mode TARGET` | the mode gate's reading (§12.1) |
//! | `resize TARGET COLS ROWS`, `clear-scrollback TARGET`, `reset TARGET` | sequenced records |
//! | `clients` | attached clients and the panes they are primary for |
//! | `snapshot TARGET` | the pane's snapshot (§15.3): its stream position and bytes, base64 |
//! | `tsp TARGET` | the authoritative replica's TSP state (§7) and the mode gate's counts |
//! | `settings [--native-surfaces on\|off] [--command-lenses on\|off] [--scrollback N] [--retention BYTES]` | host settings (§15.2); changes reach every pane as a record |
//! | `surface-play [--pane TARGET] FILE [from N] [until N] [paced [TIMES]]` | plays a TSP recording into a pane (§7.15) |
//! | `effect [--window K] toast\|open\|copy TEXT` | asks the windows showing a window key to perform an effect |
//! | `kill TARGET`, `close TARGET` | hangs up and closes a pane |
//! | `open PATH [--window K]` | asks the windows showing the window key to open a file (their `route.open`, then their opener); `delivered` 0 means no window took it |
//! | `events [--filter k,…]` | turns the connection into an event stream (`events.rs`) |
//! | `checkpoint` | writes the state file now (§16) |
//! | `state` | the state file, its last checkpoint, and the in-place upgrades so far |
//! | `upgrade [--exe PATH]` | hands the daemon to a new build in place (§15.7) |
//! | `shutdown` | writes the state file and stops the daemon |
//!
//! Targets: `@focused`, a pane id, an exact title, a title in any case, or a program name. Among
//! several matches, one in the focused pane's tab wins.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use auk_mirror::PaneId;
use auk_mirror::layout::SplitDir;
use auk_replica::Record;
use auk_term::input::{Key, KeyEvent, Mods, encode_key, encode_paste};
use serde_json::{Value, json};

use crate::control::{self, error, ok};
use crate::events::{Actor, as_actor};
use crate::pane::{Pane, PaneKind};
use crate::server::{Daemon, NewPane, Place, lock};

type Reply = Result<Value, String>;

impl Daemon {
    /// Runs one control command.
    pub async fn command(self: &Arc<Self>, line: &str) -> Value {
        let Some(words) = shlex::split(line) else {
            return error("unbalanced quotes");
        };
        let Some((cmd, args)) = words.split_first() else {
            return error("empty command");
        };
        let result = match cmd.as_str() {
            "ping" => return control::handle("ping"),
            "version" => Ok(json!(self.versions)),
            "ls" => Ok(self.ls()),
            "new" => as_actor(Actor::Cli, || self.cmd_new(args)),
            "split" => as_actor(Actor::Cli, || self.cmd_split(args)),
            "open" => self.cmd_open(args),
            "run" => self.cmd_run(args),
            "send" => self.cmd_send(args),
            "capture" => self.cmd_capture(args),
            "wait" => return self.cmd_wait(args).await,
            "state-hash" => self.with_pane(args, |p| {
                let hash: String =
                    p.replica.state_hash().iter().map(|b| format!("{b:02x}")).collect();
                Ok(json!({"hash": hash, "seq": p.replica.seq()}))
            }),
            "marks" => self.with_pane(args, |p| Ok(json!(p.shell.marks))),
            "tty-mode" => self.with_pane(args, |p| match p.tty_mode() {
                Some(m) => Ok(json!({"canonical": m.canonical, "echo": m.echo, "tsp_input": m.accepts_tsp_input()})),
                None => Err("the pane's process has exited".into()),
            }),
            "resize" => self.cmd_resize(args),
            "clear-scrollback" => self.record(args, Record::ClearScrollback),
            "reset" => self.record(args, Record::Reset),
            "clients" => Ok(self.clients()),
            "tsp" => self.with_pane(args, |p| {
                let mut v = crate::tsp::state_json(p.replica.tsp(), p.replica.terminal());
                if let Some((written, dropped)) = p.gate_stats() {
                    v["gate_stats"] = json!({"written": written, "dropped": dropped});
                }
                v["stand_in"] = json!(p.stand_in);
                Ok(v)
            }),
            "settings" => self.cmd_settings(args),
            "snapshot" => self.with_pane(args, |p| {
                use base64::Engine as _;
                let snap = p.snapshot();
                let mut v = json!(snap);
                v["bytes"] = json!(base64::engine::general_purpose::STANDARD.encode(&snap.bytes));
                Ok(v)
            }),
            "surface-play" => return self.cmd_play(args).await,
            "effect" => self.cmd_effect(args),
            "kill" | "close" => as_actor(Actor::Cli, || {
                self.target(args.first()).and_then(|id| self.kill(id))
            })
            .map(|()| Value::Null),
            "plugin" | "block" | "blocks" | "pane-list" => {
                as_actor(Actor::Cli, || self.plugin_command(cmd, args))
            }
            "lens" => self.with_pane(args, |p| {
                let blocks: Vec<Value> = p
                    .replica
                    .lens_blocks()
                    .iter()
                    .map(|b| {
                        let mut v = json!(b);
                        let raw = p.replica.lens_raw(b.id).unwrap_or_default();
                        v["raw"] = json!(String::from_utf8_lossy(raw));
                        v
                    })
                    .collect();
                Ok(json!({"blocks": blocks, "running": p.replica.lens_running(), "catalog": p.replica.lens_catalog()}))
            }),
            "checkpoint" => self.checkpoint().map_err(|e| format!("checkpoint: {e}")),
            "state" => Ok(self.persist_info()),
            "upgrade" => self.cmd_upgrade(args),
            "shutdown" => {
                self.stop();
                Ok(Value::Null)
            }
            other => Err(format!("unknown command `{other}`")),
        };
        match result {
            Ok(Value::Null) => json!({"ok": true}),
            Ok(value) => ok(value),
            Err(message) => error(message),
        }
    }

    fn ls(&self) -> Value {
        let st = self.state();
        let panes: Vec<Value> = st
            .panes
            .values()
            .map(|p| {
                let p = lock(p);
                let term = p.replica.terminal();
                json!({
                    "id": p.id,
                    "kind": p.kind,
                    "title": term.title(),
                    "cwd": p.cwd(),
                    "cols": term.cols(),
                    "rows": term.rows(),
                    "pid": p.pid,
                    "exit": p.exit,
                    "window": p.window_key,
                    "session": st.layout.session_of(p.id),
                    "at_prompt": p.shell.at_prompt,
                    "running": p.shell.running,
                    "primary": p.primary(),
                })
            })
            .collect();
        json!({
            "panes": panes,
            "layout": st.layout.windows,
            "focused": st.focused,
            "daemon_pid": std::process::id(),
            "event_subscribers": crate::events::subscribers(),
        })
    }

    fn clients(&self) -> Value {
        let st = self.state();
        let clients: Vec<Value> = st
            .clients
            .iter()
            .map(|(id, c)| {
                let primary_for: Vec<PaneId> = st
                    .panes
                    .values()
                    .filter_map(|p| {
                        let p = lock(p);
                        (p.primary() == Some(*id)).then_some(p.id)
                    })
                    .collect();
                json!({"id": id, "kind": c.kind, "window": c.window_key, "primary_for": primary_for})
            })
            .collect();
        json!(clients)
    }

    /// Resolves a target (§11.1).
    fn target(&self, word: Option<&String>) -> Result<PaneId, String> {
        let word = word.ok_or("missing target")?;
        let st = self.state();
        if word == "@focused" {
            return st.focused.ok_or_else(|| "no pane is focused".into());
        }
        if let Ok(id) = word.parse::<PaneId>() {
            return st
                .panes
                .contains_key(&id)
                .then_some(id)
                .ok_or_else(|| format!("no pane {id}"));
        }
        struct Candidate {
            id: PaneId,
            title: String,
            program: Option<String>,
        }
        let candidates: Vec<Candidate> = st
            .panes
            .values()
            .map(|p| {
                let p = lock(p);
                let line = match &p.kind {
                    PaneKind::Command { line } => Some(line.clone()),
                    PaneKind::Shell if p.shell.running => {
                        p.shell.marks.last().and_then(|m| m.cmdline.clone())
                    }
                    PaneKind::Shell => None,
                    PaneKind::Block { block, .. } => Some(block.clone()),
                };
                let program = line
                    .and_then(|l| l.split_whitespace().next().map(str::to_owned))
                    .map(|w| w.rsplit('/').next().unwrap_or(&w).to_owned());
                Candidate {
                    id: p.id,
                    title: p.replica.terminal().title().to_owned(),
                    program,
                }
            })
            .collect();
        // Ties go to the active tab: the focused pane's.
        let active: Vec<PaneId> = st
            .focused
            .and_then(|f| tab_panes(&st.layout, f))
            .unwrap_or_default();
        let find = |pred: &dyn Fn(&Candidate) -> bool| {
            candidates
                .iter()
                .filter(|c| pred(c))
                .min_by_key(|c| !active.contains(&c.id))
                .map(|c| c.id)
        };
        find(&|c| c.title == *word)
            .or_else(|| find(&|c| c.title.eq_ignore_ascii_case(word)))
            .or_else(|| find(&|c| c.program.as_deref() == Some(word.as_str())))
            .ok_or_else(|| format!("no pane matches `{word}`"))
    }

    fn pane_arc(&self, args: &[String]) -> Result<Arc<Mutex<Pane>>, String> {
        let id = self.target(args.first())?;
        self.pane(id).ok_or_else(|| format!("no pane {id}"))
    }

    fn with_pane(&self, args: &[String], f: impl FnOnce(&mut Pane) -> Reply) -> Reply {
        let pane = self.pane_arc(args)?;
        let mut p = lock(&pane);
        f(&mut p)
    }

    fn cmd_new(self: &Arc<Self>, args: &[String]) -> Reply {
        let mut opts = Opts::parse(args)?;
        let what = opts
            .positional
            .first()
            .map(String::as_str)
            .unwrap_or("session");
        let place = match what {
            "session" => Place::NewSession {
                name: opts.take("name"),
            },
            "tab" => {
                let session = match opts.take("session") {
                    Some(s) => s.parse().map_err(|_| format!("bad session `{s}`"))?,
                    None => {
                        let st = self.state();
                        let key = opts
                            .get("window")
                            .unwrap_or(crate::server::DEFAULT_WINDOW_KEY);
                        st.layout
                            .window(key)
                            .and_then(|w| w.sessions.last())
                            .map(|s| s.id)
                            .ok_or_else(|| format!("window `{key}` has no session"))?
                    }
                };
                Place::NewTab { session }
            }
            other => return Err(format!("new: expected `session` or `tab`, got `{other}`")),
        };
        let id = self.new_pane(NewPane {
            window_key: opts.take("window"),
            place,
            cwd: opts.take("cwd").map(PathBuf::from),
            command: opts.command(),
            size: opts.size()?,
        })?;
        Ok(json!({"pane": id}))
    }

    fn cmd_split(self: &Arc<Self>, args: &[String]) -> Reply {
        let at = self.target(args.first())?;
        let mut opts = Opts::parse(&args[1..])?;
        let dir = match opts.positional.first().map(String::as_str) {
            Some("right") => SplitDir::Right,
            Some("down") => SplitDir::Down,
            other => return Err(format!("split: expected `right` or `down`, got {other:?}")),
        };
        let cwd = match opts.take("cwd") {
            Some(dir) => Some(PathBuf::from(dir)),
            None => self.pane(at).map(|p| lock(&p).cwd()),
        };
        let id = self.new_pane(NewPane {
            window_key: None,
            place: Place::Split { pane: at, dir },
            cwd,
            command: opts.command(),
            size: opts.size()?,
        })?;
        Ok(json!({"pane": id}))
    }

    fn cmd_run(&self, args: &[String]) -> Reply {
        if args.len() < 2 {
            return Err("run: expected a target and a command".into());
        }
        let mut line = args[1..].join(" ").into_bytes();
        line.push(b'\r');
        self.with_pane(args, |p| write(p, line))
    }

    fn cmd_send(&self, args: &[String]) -> Reply {
        let (mode, rest) = match args.get(1..) {
            Some([mode, rest @ ..]) => (mode.as_str(), rest),
            _ => return Err("send: expected a target, then text, paste or keys".into()),
        };
        self.with_pane(args, |p| {
            let modes = p.replica.terminal().modes();
            let bytes = match mode {
                "text" => rest.join(" ").into_bytes(),
                "paste" => encode_paste(&rest.join(" "), &modes),
                "keys" => {
                    let mut out = Vec::new();
                    for name in rest {
                        let key = parse_key(name).ok_or_else(|| format!("unknown key `{name}`"))?;
                        out.extend(encode_key(&key, &modes));
                    }
                    out
                }
                other => return Err(format!("send: expected text, paste or keys, got `{other}`")),
            };
            write(p, bytes)
        })
    }

    fn cmd_capture(&self, args: &[String]) -> Reply {
        let opts = crate::capture::Options::parse(args.get(1..).unwrap_or_default())?;
        self.with_pane(args, |p| {
            let text = crate::capture::capture(p.replica.terminal(), p.replica.tsp(), opts);
            let term = p.replica.terminal();
            Ok(json!({
                "pane": p.id,
                "text": text,
                "seq": p.replica.seq(),
                "cols": term.cols(),
                "rows": term.rows(),
                "title": term.title(),
                "cursor": {"row": term.cursor().row, "col": term.cursor().col},
            }))
        })
    }

    /// `open PATH [--window K]`: the windows showing the window key route it (`route.open`,
    /// then their opener). Returns how many windows it went to.
    fn cmd_open(&self, args: &[String]) -> Reply {
        let mut opts = Opts::parse(args)?;
        let key = opts
            .take("window")
            .unwrap_or_else(|| crate::server::DEFAULT_WINDOW_KEY.to_owned());
        let [path] = opts.positional.as_slice() else {
            return Err("open: expected one path".into());
        };
        let delivered = self.send_effect(
            &key,
            auk_mirror::Effect::Open {
                target: path.clone(),
                origin: None,
            },
        );
        Ok(json!({"delivered": delivered, "window": key}))
    }

    fn cmd_settings(&self, args: &[String]) -> Reply {
        let opts = Opts::parse(args)?;
        let mut settings = self.settings();
        let on_off = |name: &str| -> Result<Option<bool>, String> {
            match opts.get(name) {
                None => Ok(None),
                Some("on") => Ok(Some(true)),
                Some("off") => Ok(Some(false)),
                Some(other) => Err(format!("--{name}: expected on or off, got `{other}`")),
            }
        };
        let number = |name: &str| -> Result<Option<u64>, String> {
            opts.get(name)
                .map(|v| v.parse().map_err(|_| format!("--{name}: bad number `{v}`")))
                .transpose()
        };
        if let Some(on) = on_off("native-surfaces")? {
            settings.native_surfaces = on;
        }
        if let Some(on) = on_off("command-lenses")? {
            settings.command_lenses = on;
        }
        if let Some(n) = number("scrollback")? {
            settings.scrollback = n as usize;
        }
        if let Some(n) = number("retention")? {
            settings.retention = n;
        }
        if let Some(word) = opts.positional.first() {
            return Err(format!("settings: unexpected `{word}`"));
        }
        self.set_settings(settings.clone());
        Ok(json!(settings))
    }

    async fn cmd_play(self: &Arc<Self>, args: &[String]) -> Value {
        let result = async {
            let (target, rest) = match args.split_first() {
                Some((flag, rest)) if flag == "--pane" => {
                    let (t, rest) = rest.split_first().ok_or("--pane needs a target")?;
                    (t.clone(), rest)
                }
                _ => ("@focused".to_owned(), args),
            };
            let (file, rest) = rest
                .split_first()
                .ok_or("surface-play: expected a recording")?;
            let opts = crate::play::PlayOptions::parse(rest)?;
            let pane = self.pane_arc(std::slice::from_ref(&target))?;
            let apc = lock(&pane).replica.tsp().config().chunk_size() as usize;
            let steps = crate::play::load(std::path::Path::new(file), apc)?;
            self.play(pane, steps, opts).await
        }
        .await;
        match result {
            Ok(value) => ok(value),
            Err(message) => error(message),
        }
    }

    fn cmd_effect(&self, args: &[String]) -> Reply {
        use auk_mirror::Effect;
        let mut opts = Opts::parse(args)?;
        let key = opts
            .take("window")
            .unwrap_or_else(|| crate::server::DEFAULT_WINDOW_KEY.to_owned());
        let (kind, text) = match opts.positional.split_first() {
            Some((kind, text)) if !text.is_empty() => (kind.as_str(), text.join(" ")),
            _ => return Err("effect: expected toast, open or copy, then text".into()),
        };
        let effect = match kind {
            "toast" => Effect::Toast {
                level: "info".into(),
                text,
                sub: None,
            },
            "open" => Effect::Open { target: text, origin: None },
            "copy" => Effect::Copy { text },
            other => return Err(format!("effect: unknown effect `{other}`")),
        };
        Ok(json!({"delivered": self.send_effect(&key, effect)}))
    }

    fn cmd_resize(&self, args: &[String]) -> Reply {
        let (cols, rows) = match args.get(1..3) {
            Some([c, r]) => (c.parse::<u16>(), r.parse::<u16>()),
            _ => return Err("resize: expected a target, columns and rows".into()),
        };
        let (Ok(cols @ 1..), Ok(rows @ 1..)) = (cols, rows) else {
            return Err("resize: bad size".into());
        };
        self.with_pane(args, |p| {
            p.resize(cols, rows);
            Ok(Value::Null)
        })
    }

    fn record(&self, args: &[String], record: Record) -> Reply {
        self.with_pane(args, |p| {
            p.push_record(record);
            Ok(Value::Null)
        })
    }

    async fn cmd_wait(self: &Arc<Self>, args: &[String]) -> Value {
        match self.wait(args).await {
            Ok(Some(value)) => ok(value),
            Ok(None) => json!({"ok": false, "error": "timeout", "timeout": true}),
            Err(message) => error(message),
        }
    }

    /// Waits for a pane condition; `Ok(None)` on timeout.
    async fn wait(self: &Arc<Self>, args: &[String]) -> Result<Option<Value>, String> {
        let pane = self.pane_arc(args)?;
        let opts = Opts::parse(&args[1..])?;
        let until = opts.get("until").ok_or("wait: missing --until")?;
        let cond = match until {
            "prompt" => Until::Prompt,
            "exit" => Until::Exit,
            idle => match idle.strip_prefix("idle=").map(str::parse::<u64>) {
                Some(Ok(ms)) => Until::Idle(Duration::from_millis(ms)),
                _ => return Err(format!("wait: bad --until `{until}`")),
            },
        };
        let timeout = match opts.get("timeout") {
            Some(s) => Some(Duration::from_secs_f64(
                s.parse::<f64>()
                    .ok()
                    .filter(|t| *t >= 0.0)
                    .ok_or("wait: bad --timeout")?,
            )),
            None => None,
        };
        let wait = async {
            loop {
                let (mut changed, idle_left) = {
                    let p = lock(&pane);
                    let rx = p.watch();
                    let met = match cond {
                        Until::Prompt => p.shell.at_prompt && !p.shell.awaiting_prompt,
                        Until::Exit => p.exit.is_some(),
                        Until::Idle(ms) => p.idle_for() >= ms,
                    };
                    if met {
                        return json!({
                            "pane": p.id,
                            "until": until,
                            "exit": p.exit,
                            "prompts": p.shell.prompts,
                            "status": p.shell.marks.last().and_then(|m| m.status),
                        });
                    }
                    let idle_left = match cond {
                        Until::Idle(ms) => Some(ms.saturating_sub(p.idle_for())),
                        _ => None,
                    };
                    (rx, idle_left)
                };
                match idle_left {
                    Some(left) => {
                        let _ = tokio::time::timeout(left, changed.changed()).await;
                    }
                    None => {
                        if changed.changed().await.is_err() {
                            // The pane is gone from the table; wait on the next check.
                            tokio::time::sleep(Duration::from_millis(50)).await;
                        }
                    }
                }
            }
        };
        Ok(match timeout {
            Some(t) => tokio::time::timeout(t, wait).await.ok(),
            None => Some(wait.await),
        })
    }
}

/// The panes of the tab that holds `pane`.
fn tab_panes(layout: &auk_mirror::layout::Layout, pane: PaneId) -> Option<Vec<PaneId>> {
    layout
        .windows
        .values()
        .flat_map(|w| &w.sessions)
        .flat_map(|s| &s.tabs)
        .find(|t| t.root.contains(pane))
        .map(|t| t.root.panes())
}

#[derive(Clone, Copy)]
enum Until {
    Prompt,
    Exit,
    Idle(Duration),
}

fn write(p: &mut Pane, bytes: Vec<u8>) -> Reply {
    if p.write_input(bytes) {
        Ok(Value::Null)
    } else {
        Err(format!("pane {} has exited", p.id))
    }
}

/// `--flag value` options, positional words, and a command after `--`.
struct Opts {
    flags: Vec<(String, String)>,
    positional: Vec<String>,
    command: Vec<String>,
}

impl Opts {
    fn parse(args: &[String]) -> Result<Opts, String> {
        let mut opts = Opts {
            flags: Vec::new(),
            positional: Vec::new(),
            command: Vec::new(),
        };
        let mut it = args.iter().peekable();
        while let Some(arg) = it.next() {
            if arg == "--" {
                opts.command = it.cloned().collect();
                break;
            }
            match arg.strip_prefix("--") {
                Some(name) => match name.split_once('=') {
                    Some((name, value)) => opts.flags.push((name.into(), value.into())),
                    // `capture --scrollback` is a switch; `settings --scrollback N` takes a value.
                    None if name == "scrollback"
                        && !it.peek().is_some_and(|v| v.parse::<u64>().is_ok()) =>
                    {
                        opts.flags.push((name.into(), String::new()))
                    }
                    None => {
                        let value = it.next().ok_or_else(|| format!("--{name} needs a value"))?;
                        opts.flags.push((name.into(), value.clone()));
                    }
                },
                None => opts.positional.push(arg.clone()),
            }
        }
        Ok(opts)
    }

    fn get(&self, name: &str) -> Option<&str> {
        self.flags
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    fn take(&mut self, name: &str) -> Option<String> {
        let value = self.get(name).map(str::to_owned);
        self.flags.retain(|(n, _)| n != name);
        value
    }

    fn command(&self) -> Option<String> {
        (!self.command.is_empty()).then(|| self.command.join(" "))
    }

    fn size(&self) -> Result<Option<(u16, u16)>, String> {
        let Some(size) = self.get("size") else {
            return Ok(None);
        };
        let parsed = size
            .split_once('x')
            .and_then(|(c, r)| Some((c.parse::<u16>().ok()?, r.parse::<u16>().ok()?)))
            .filter(|(c, r)| *c > 0 && *r > 0);
        parsed
            .map(Some)
            .ok_or_else(|| format!("bad --size `{size}` (expected COLSxROWS)"))
    }
}

/// Parses a key name: `Enter`, `Tab`, `Esc`, `Up`, `F5`, `Space`, a single character, with
/// `C-`, `M-` (Alt) and `S-` prefixes.
pub fn parse_key(name: &str) -> Option<KeyEvent> {
    let mut mods = Mods::default();
    let mut rest = name;
    loop {
        match rest.get(..2) {
            Some("C-") if rest.len() > 2 => mods.ctrl = true,
            Some("M-") if rest.len() > 2 => mods.alt = true,
            Some("S-") if rest.len() > 2 => mods.shift = true,
            _ => break,
        }
        rest = &rest[2..];
    }
    let key = match rest.to_ascii_lowercase().as_str() {
        "enter" | "return" => Key::Enter,
        "tab" => Key::Tab,
        "bspace" | "backspace" => Key::Backspace,
        "esc" | "escape" => Key::Escape,
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" | "ppage" => Key::PageUp,
        "pagedown" | "npage" => Key::PageDown,
        "insert" | "ic" => Key::Insert,
        "delete" | "dc" => Key::Delete,
        "space" => Key::Char(' '),
        f if f.len() > 1 && f.starts_with('f') => {
            Key::F(f[1..].parse().ok().filter(|n| (1..=24).contains(n))?)
        }
        _ => {
            let mut chars = rest.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            Key::Char(c.to_ascii_lowercase())
        }
    };
    let text = match key {
        Key::Char(c) if !mods.ctrl && !mods.alt => {
            let typed = if rest.chars().count() == 1 {
                rest.to_owned()
            } else {
                c.to_string()
            };
            Some(if mods.shift {
                typed.to_uppercase()
            } else {
                typed
            })
        }
        _ => None,
    };
    Some(KeyEvent { key, mods, text })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_key_names() {
        let k = parse_key("C-c").unwrap();
        assert_eq!((k.key, k.mods.ctrl, k.text), (Key::Char('c'), true, None));
        assert_eq!(parse_key("Enter").unwrap().key, Key::Enter);
        assert_eq!(parse_key("F12").unwrap().key, Key::F(12));
        assert_eq!(parse_key("x").unwrap().text.as_deref(), Some("x"));
        assert_eq!(parse_key("S-a").unwrap().text.as_deref(), Some("A"));
        assert_eq!(parse_key("Space").unwrap().text.as_deref(), Some(" "));
        assert_eq!(parse_key("Q").unwrap().text.as_deref(), Some("Q"));
        let k = parse_key("M-S-Up").unwrap();
        assert!(k.mods.alt && k.mods.shift && k.key == Key::Up);
        assert!(parse_key("F99").is_none());
        assert!(parse_key("nonsense").is_none());
    }

    #[test]
    fn parses_options() {
        let args: Vec<String> = ["tab", "--cwd", "/tmp", "--size=100x30", "--", "echo", "hi"]
            .map(String::from)
            .into();
        let mut o = Opts::parse(&args).unwrap();
        assert_eq!(o.positional, ["tab"]);
        assert_eq!(o.take("cwd").as_deref(), Some("/tmp"));
        assert_eq!(o.size().unwrap(), Some((100, 30)));
        assert_eq!(o.command().as_deref(), Some("echo hi"));
        assert!(Opts::parse(&["--cwd".to_owned()]).is_err());
        let bad = Opts::parse(&["--size".to_owned(), "0x3".to_owned()]).unwrap();
        assert!(bad.size().is_err());
    }
}
