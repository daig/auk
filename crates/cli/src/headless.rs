//! The headless window (§11.3, §21): a client that attaches to the daemon like an app window,
//! keeps a follower replica per pane, and serves its own control endpoint so tests and scripts
//! can read what it shows. It draws nothing.
//!
//! Control commands: `ping`, `version`, `panes`, `layout`, `state-hash PANE`,
//! `capture PANE [--scrollback]`, `send PANE text|paste WORD…`, `send PANE keys KEY…`,
//! `focus PANE`, `resize PANE COLS ROWS`, `quit`.

use std::collections::BTreeMap;
use std::io;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;

use auk_daemon::commands::parse_key;
use auk_daemon::control::{self, error, ok};
use auk_mirror::layout::Window;
use auk_mirror::{Client, ClientFacts, ClientKind, ClientMsg, DaemonMsg, Hello, PaneId, write_msg};
use auk_replica::{Item, Replica, Role};
use auk_term::input::{encode_key, encode_paste};
use serde_json::{Value, json};

/// How a headless window starts.
#[derive(Clone, Debug)]
pub struct Options {
    pub socket: PathBuf,
    pub control: PathBuf,
    pub window_key: String,
    /// The size this window asks for each pane; it applies while the window is primary.
    pub size: Option<(u16, u16)>,
    /// The `auk` binary to start a daemon with when none answers.
    pub exe: PathBuf,
    pub state_dir: PathBuf,
}

#[derive(Debug, Default)]
struct View {
    replicas: BTreeMap<PaneId, Replica>,
    window: Window,
    exited: BTreeMap<PaneId, i32>,
    primary: BTreeMap<PaneId, bool>,
    /// Stream errors seen (out-of-order items); a correct daemon never causes any.
    errors: Vec<String>,
}

#[derive(Debug)]
struct Headless {
    view: Mutex<View>,
    out: Mutex<UnixStream>,
    size: Option<(u16, u16)>,
}

impl Headless {
    fn view(&self) -> MutexGuard<'_, View> {
        self.view.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn send(&self, msg: &ClientMsg) -> io::Result<()> {
        write_msg(
            &mut *self.out.lock().unwrap_or_else(|e| e.into_inner()),
            msg,
        )
    }

    fn on_message(&self, msg: DaemonMsg) {
        let mut view = self.view();
        match msg {
            DaemonMsg::Snapshot { pane, snapshot } => {
                view.replicas.insert(
                    pane,
                    Replica::from_snapshot(&snapshot, Role::Follower, None),
                );
                drop(view);
                if let Some((cols, rows)) = self.size {
                    let _ = self.send(&ClientMsg::Resize { pane, cols, rows });
                }
            }
            DaemonMsg::Output { pane, seq, bytes } => {
                apply(&mut view, pane, seq, Item::Output(&bytes))
            }
            DaemonMsg::Record { pane, seq, record } => {
                apply(&mut view, pane, seq, Item::Record(&record))
            }
            DaemonMsg::Layout { window, .. } => view.window = window,
            DaemonMsg::PaneExit { pane, status } => {
                view.exited.insert(pane, status);
            }
            DaemonMsg::Primary { pane, you } => {
                view.primary.insert(pane, you);
            }
            _ => {}
        }
    }

    fn handle(&self, line: &str) -> Value {
        let Some(words) = shlex::split(line) else {
            return error("unbalanced quotes");
        };
        let Some((cmd, args)) = words.split_first() else {
            return error("empty command");
        };
        let result = match cmd.as_str() {
            "ping" | "version" => return control::handle(cmd),
            "panes" => Ok(self.panes()),
            "layout" => Ok(json!(self.view().window)),
            "state-hash" => self.with_replica(args, |r| {
                let hash: String = r.state_hash().iter().map(|b| format!("{b:02x}")).collect();
                Ok(json!({"hash": hash, "seq": r.seq()}))
            }),
            "capture" => {
                let scrollback = args.iter().any(|a| a == "--scrollback");
                self.with_replica(args, |r| {
                    let text = if scrollback {
                        r.text()
                    } else {
                        r.screen_text()
                    };
                    Ok(json!({"text": text, "seq": r.seq()}))
                })
            }
            "send" => self.cmd_send(args),
            "focus" => pane_id(args).and_then(|pane| self.sent(&ClientMsg::Focus { pane })),
            "resize" => match (pane_id(args), args.get(1..3)) {
                (Ok(pane), Some([c, r])) => match (c.parse(), r.parse()) {
                    (Ok(cols), Ok(rows)) => self.sent(&ClientMsg::Resize { pane, cols, rows }),
                    _ => Err("resize: bad size".into()),
                },
                _ => Err("resize: expected a pane, columns and rows".into()),
            },
            "quit" => {
                let _ = self.send(&ClientMsg::Detach);
                std::process::exit(0);
            }
            other => Err(format!("unknown command `{other}`")),
        };
        match result {
            Ok(Value::Null) => json!({"ok": true}),
            Ok(value) => ok(value),
            Err(message) => error(message),
        }
    }

    fn panes(&self) -> Value {
        let view = self.view();
        let panes: Vec<Value> = view
            .replicas
            .iter()
            .map(|(id, r)| {
                json!({
                    "id": id,
                    "seq": r.seq(),
                    "cols": r.terminal().cols(),
                    "rows": r.terminal().rows(),
                    "exit": view.exited.get(id),
                    "primary": view.primary.get(id).copied().unwrap_or(false),
                })
            })
            .collect();
        json!({"panes": panes, "errors": view.errors})
    }

    fn with_replica(
        &self,
        args: &[String],
        f: impl FnOnce(&Replica) -> Result<Value, String>,
    ) -> Result<Value, String> {
        let pane = pane_id(args)?;
        let view = self.view();
        f(view
            .replicas
            .get(&pane)
            .ok_or_else(|| format!("no replica of pane {pane}"))?)
    }

    /// Encodes input from this window's replica's modes (§15.4) and sends it.
    fn cmd_send(&self, args: &[String]) -> Result<Value, String> {
        let pane = pane_id(args)?;
        let (mode, rest) = match args.get(1..) {
            Some([mode, rest @ ..]) => (mode.as_str(), rest),
            _ => return Err("send: expected a pane, then text, paste or keys".into()),
        };
        let modes = {
            let view = self.view();
            view.replicas
                .get(&pane)
                .ok_or_else(|| format!("no replica of pane {pane}"))?
                .terminal()
                .modes()
        };
        let bytes = match mode {
            "text" => rest.join(" ").into_bytes(),
            "paste" => encode_paste(&rest.join(" "), &modes),
            "keys" => {
                let mut out = Vec::new();
                for name in rest {
                    out.extend(encode_key(
                        &parse_key(name).ok_or_else(|| format!("unknown key `{name}`"))?,
                        &modes,
                    ));
                }
                out
            }
            other => return Err(format!("send: expected text, paste or keys, got `{other}`")),
        };
        self.sent(&ClientMsg::Input { pane, bytes })
    }

    fn sent(&self, msg: &ClientMsg) -> Result<Value, String> {
        self.send(msg)
            .map(|()| Value::Null)
            .map_err(|e| e.to_string())
    }
}

fn apply(view: &mut View, pane: PaneId, seq: u64, item: Item<'_>) {
    let Some(replica) = view.replicas.get_mut(&pane) else {
        view.errors
            .push(format!("pane {pane}: item {seq} before a snapshot"));
        return;
    };
    if let Err(err) = replica.apply(seq, item) {
        view.errors.push(format!("pane {pane}: {err}"));
    }
}

fn pane_id(args: &[String]) -> Result<PaneId, String> {
    let word = args.first().ok_or("missing pane")?;
    word.parse().map_err(|_| format!("bad pane `{word}`"))
}

/// Runs a headless window until the daemon goes away.
pub fn run(opts: &Options) -> io::Result<()> {
    auk_daemon::spawn::ensure(&opts.exe, &opts.socket, &opts.state_dir)?;
    let stream = UnixStream::connect(&opts.socket)?;
    let hello = Hello::new(ClientKind::Headless, ClientFacts::default());
    let (mut client, _welcome) =
        Client::handshake(stream, &hello).map_err(|e| io::Error::other(e.to_string()))?;
    let out = client.get_ref().try_clone()?;
    client.send(&ClientMsg::Attach {
        window_key: opts.window_key.clone(),
    })?;

    let headless = Arc::new(Headless {
        view: Mutex::new(View::default()),
        out: Mutex::new(out),
        size: opts.size,
    });
    let listener = control::bind(&opts.control)?;
    {
        let headless = headless.clone();
        thread::spawn(move || {
            control::serve_listener_with(listener, Arc::new(move |line| headless.handle(line)))
        });
    }
    println!("ready {}", opts.control.display());

    let result = loop {
        match client.recv() {
            Ok(Some(msg)) => headless.on_message(msg),
            Ok(None) => {
                break Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "the daemon closed the connection",
                ));
            }
            Err(err) => break Err(err.into()),
        }
    };
    let _ = std::fs::remove_file(&opts.control);
    result
}

/// The default control endpoint for a headless window: beside the daemon socket, by pid.
pub fn default_control(socket: &Path) -> PathBuf {
    socket
        .parent()
        .unwrap_or(Path::new("."))
        .join(format!("headless-{}.ctl", std::process::id()))
}
