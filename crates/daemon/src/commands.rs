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
//! | `capture TARGET [--scrollback]` | the pane's text |
//! | `wait TARGET --until prompt\|exit\|idle=MS [--timeout S]` | waits (`"timeout":true` on timeout) |
//! | `state-hash TARGET` | the authoritative replica's hash and sequence number |
//! | `marks TARGET` | the shell's command marks (§5.3) |
//! | `tty-mode TARGET` | the mode gate's reading (§12.1) |
//! | `resize TARGET COLS ROWS`, `clear-scrollback TARGET`, `reset TARGET` | sequenced records |
//! | `clients` | attached clients and the panes they are primary for |
//! | `kill TARGET` | hangs up and closes a pane |
//! | `shutdown` | stops the daemon |
//!
//! Targets: `@focused`, a pane id, an exact title, a title in any case, or a program name.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use auk_mirror::PaneId;
use auk_mirror::layout::SplitDir;
use auk_replica::Record;
use auk_term::input::{Key, KeyEvent, Mods, encode_key, encode_paste};
use serde_json::{Value, json};

use crate::control::{self, error, ok};
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
            "new" => self.cmd_new(args),
            "split" => self.cmd_split(args),
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
            "kill" => self.target(args.first()).and_then(|id| self.kill(id)).map(|()| Value::Null),
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
        let find =
            |pred: &dyn Fn(&Candidate) -> bool| candidates.iter().find(|c| pred(c)).map(|c| c.id);
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
        let scrollback = args.iter().skip(1).any(|a| a == "--scrollback");
        self.with_pane(args, |p| {
            let text = if scrollback {
                p.replica.text()
            } else {
                p.replica.screen_text()
            };
            Ok(json!({"text": text, "seq": p.replica.seq()}))
        })
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
        let mut it = args.iter();
        while let Some(arg) = it.next() {
            if arg == "--" {
                opts.command = it.cloned().collect();
                break;
            }
            match arg.strip_prefix("--") {
                Some(name) => match name.split_once('=') {
                    Some((name, value)) => opts.flags.push((name.into(), value.into())),
                    None if name == "scrollback" => opts.flags.push((name.into(), String::new())),
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
