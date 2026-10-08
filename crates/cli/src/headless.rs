//! The headless window (§11.3, §21): a client that attaches to the daemon like an app window,
//! keeps a follower replica per pane (TSP included), and serves its own control endpoint so tests
//! and scripts can read what it shows. When it is a pane's primary it acks the frames it has
//! applied as drawn (§7.10); drawing surfaces headlessly arrives with the views, at the same point.
//!
//! Control commands: `ping`, `version`, `panes`, `layout`, `state-hash PANE`,
//! `capture PANE [--scrollback]`, `send PANE text|paste WORD…`, `send PANE keys KEY…`,
//! `focus PANE`, `resize PANE COLS ROWS`, `tsp PANE`, `event PANE SF JSON`,
//! `click PANE SF NODE [--double] [--menu ACT] [--mods shift,ctrl,alt,meta]`, `facts JSON`,
//! `effects`, `catalog`, `lens PANE`, `remote`, `quit`.
//!
//! With [`Options::remote`] the window attaches to a remote host's daemon over SSH
//! (`auk-remote`, §10.2): the link reconnects and resyncs after a drop, holding typing meanwhile,
//! and `remote` reports its state and the host's identity chain.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;

mod gesture;

use auk_daemon::commands::parse_key;
use auk_daemon::control::{self, error, ok};
use auk_mirror::layout::Window;
use auk_mirror::{ClientFacts, ClientKind, ClientMsg, DaemonMsg, Hello, PaneId};
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
    /// Attach to this remote host's daemon over SSH instead of the local socket.
    pub remote: Option<auk_remote::SshTarget>,
}

#[derive(Debug, Default)]
struct View {
    replicas: BTreeMap<PaneId, Replica>,
    window: Window,
    exited: BTreeMap<PaneId, i32>,
    primary: BTreeMap<PaneId, bool>,
    /// What each pane's surfaces show (view state) and the frames acked.
    surfaces: BTreeMap<PaneId, gesture::PaneView>,
    /// Effects the daemon asked for, in order.
    effects: Vec<Value>,
    /// Stream errors seen (out-of-order items); a correct daemon never causes any.
    errors: Vec<String>,
    /// The host's plugin catalog (§9.4), from the welcome and later `catalog` messages.
    catalog: Value,
    /// Lens views that arrived before this window's replica had their block, retried for 5 s
    /// (§9.6): `(pane, block, view, when)`.
    early_views: Vec<(PaneId, u64, Option<Value>, std::time::Instant)>,
}

#[derive(Debug)]
struct Headless {
    view: Mutex<View>,
    link: auk_remote::Link,
    size: Option<(u16, u16)>,
}

impl Headless {
    fn view(&self) -> MutexGuard<'_, View> {
        self.view.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn send(&self, msg: &ClientMsg) -> io::Result<()> {
        self.link.send(msg).map(|_| ())
    }

    fn on_message(&self, msg: DaemonMsg) {
        let mut view = self.view();
        let mut draw = None;
        match msg {
            DaemonMsg::Snapshot { pane, snapshot } => {
                view.replicas.insert(
                    pane,
                    Replica::from_snapshot(&snapshot, Role::Follower, None),
                );
                view.surfaces.insert(pane, gesture::PaneView::default());
                draw = Some((pane, true));
                if let Some((cols, rows)) = self.size {
                    let _ = self.send(&ClientMsg::Resize { pane, cols, rows });
                }
            }
            DaemonMsg::Output { pane, seq, bytes } => {
                apply(&mut view, pane, seq, Item::Output(&bytes));
                draw = Some((pane, false));
            }
            DaemonMsg::Record { pane, seq, record } => {
                apply(&mut view, pane, seq, Item::Record(&record));
                draw = Some((pane, false));
            }
            DaemonMsg::Layout { window, .. } => view.window = window,
            DaemonMsg::PaneExit { pane, status } => {
                view.exited.insert(pane, status);
            }
            DaemonMsg::Primary { pane, you } => {
                view.primary.insert(pane, you);
                // A new primary draws the current documents and acks them (E4).
                draw = Some((pane, true));
            }
            DaemonMsg::Effect(effect) => view.effects.push(json!(effect)),
            DaemonMsg::Catalog { catalog } => view.catalog = catalog,
            DaemonMsg::LensView { pane, block, node } => {
                if let Ok(block) = block.parse::<u64>() {
                    let node = (!node.is_null()).then_some(node);
                    let shown = view
                        .replicas
                        .get_mut(&pane)
                        .is_some_and(|r| r.set_lens_view(block, node.clone()));
                    if !shown {
                        view.early_views
                            .push((pane, block, node, std::time::Instant::now()));
                    }
                }
            }
            _ => {}
        }
        retry_views(&mut view);
        if let Some((pane, all)) = draw {
            let acks = draw_pane(&mut view, pane, all);
            drop(view);
            for msg in acks {
                let _ = self.send(&msg);
            }
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
            "tsp" => self.with_replica(args, |r| {
                Ok(auk_daemon::tsp::state_json(r.tsp(), r.terminal()))
            }),
            "event" => self.cmd_event(args),
            "click" => self.cmd_click(args),
            "facts" => match args.first().map(|j| serde_json::from_str::<ClientFacts>(j)) {
                Some(Ok(facts)) => self.sent(&ClientMsg::Facts(facts)),
                Some(Err(e)) => Err(format!("facts: {e}")),
                None => Err("facts: expected a JSON object".into()),
            },
            "effects" => Ok(json!(self.view().effects)),
            "catalog" => Ok(self.view().catalog.clone()),
            "lens" => self.with_replica(args, |r| {
                let blocks: Vec<Value> = r
                    .lens_blocks()
                    .iter()
                    .map(|b| {
                        let mut v = json!(b);
                        let raw = r.lens_raw(b.id).unwrap_or_default();
                        v["raw"] = json!(String::from_utf8_lossy(raw));
                        v
                    })
                    .collect();
                Ok(json!({"blocks": blocks, "running": r.lens_running(), "catalog": r.lens_catalog()}))
            }),
            "remote" => Ok(json!(self.link.status())),
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

    /// Sends a view event as the window would build it: `event PANE SF JSON`.
    fn cmd_event(&self, args: &[String]) -> Result<Value, String> {
        let pane = pane_id(args)?;
        let (Some(sf), Some(body)) = (args.get(1), args.get(2)) else {
            return Err("event: expected a pane, a surface and a JSON event".into());
        };
        let event: Value = serde_json::from_str(body).map_err(|e| format!("event: {e}"))?;
        self.sent(&ClientMsg::ViewEvent {
            pane,
            sf: sf.clone(),
            event,
        })
    }

    /// A pointer gesture on a node (§7.8): resolves it as a window does, flips local state, and
    /// sends the event it gives, if any. Returns the event (or null).
    fn cmd_click(&self, args: &[String]) -> Result<Value, String> {
        let pane = pane_id(args)?;
        let (Some(sf), Some(node)) = (args.get(1), args.get(2)) else {
            return Err("click: expected a pane, a surface and a node".into());
        };
        let mut gesture = gesture::Gesture::Click;
        let mut mods = auk_tsp::Mods::NONE;
        let mut rest = args[3..].iter();
        while let Some(arg) = rest.next() {
            match arg.as_str() {
                "--double" => gesture = gesture::Gesture::Double,
                "--menu" => {
                    let act = rest.next().ok_or("--menu needs an action")?;
                    gesture = gesture::Gesture::Menu(act.clone());
                }
                "--mods" => {
                    for m in rest.next().ok_or("--mods needs a list")?.split(',') {
                        match m {
                            "shift" => mods.shift = true,
                            "ctrl" => mods.ctrl = true,
                            "alt" => mods.alt = true,
                            "meta" => mods.meta = true,
                            other => return Err(format!("unknown modifier `{other}`")),
                        }
                    }
                }
                other => return Err(format!("click: unexpected `{other}`")),
            }
        }
        let event = {
            let mut view = self.view();
            let View {
                replicas, surfaces, ..
            } = &mut *view;
            let replica = replicas
                .get(&pane)
                .ok_or_else(|| format!("no replica of pane {pane}"))?;
            let pv = surfaces.entry(pane).or_default();
            pv.gesture(replica.tsp(), sf, node, gesture, mods)?
        };
        match event {
            Some(event) => {
                let body = gesture::event_json(&event);
                self.sent(&ClientMsg::ViewEvent {
                    pane,
                    sf: sf.clone(),
                    event: body.clone(),
                })?;
                Ok(body)
            }
            None => Ok(json!({"event": null})),
        }
    }

    /// Sends a message; the reply says when the link held it until a resync (§10.1).
    fn sent(&self, msg: &ClientMsg) -> Result<Value, String> {
        match self.link.send(msg) {
            Ok(auk_remote::Delivery::Held) => Ok(json!({"held": true})),
            Ok(_) => Ok(Value::Null),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// Applies early lens views whose block has appeared; drops those older than 5 s.
fn retry_views(view: &mut View) {
    let View {
        replicas,
        early_views,
        ..
    } = view;
    early_views.retain(|(pane, block, node, at)| {
        let Some(r) = replicas.get_mut(pane) else {
            return at.elapsed() < std::time::Duration::from_secs(5);
        };
        !r.set_lens_view(*block, node.clone()) && at.elapsed() < std::time::Duration::from_secs(5)
    });
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

/// Where a window draws a pane after the replica changed, and then acks what it drew (§7.10):
/// only the pane's primary acks, each live listening surface's last frame once it has been
/// drawn. Headless rendering of surfaces arrives with the views; the ack point stays here.
fn draw_pane(view: &mut View, pane: PaneId, all: bool) -> Vec<ClientMsg> {
    let primary = view.primary.get(&pane).copied().unwrap_or(false);
    let View {
        replicas, surfaces, ..
    } = view;
    let Some(replica) = replicas.get_mut(&pane) else {
        return Vec::new();
    };
    let changes = replica.take_tsp_changes();
    let pv = surfaces.entry(pane).or_default();
    pv.forget(&changes);
    if !primary {
        return Vec::new();
    }
    pv.acks(replica.tsp(), &changes, all)
        .into_iter()
        .map(|(sf, s)| ClientMsg::Ack { pane, sf, s })
        .collect()
}

fn pane_id(args: &[String]) -> Result<PaneId, String> {
    let word = args.first().ok_or("missing pane")?;
    word.parse().map_err(|_| format!("bad pane `{word}`"))
}

/// Runs a headless window until the daemon goes away.
pub fn run(opts: &Options) -> io::Result<()> {
    let target = match &opts.remote {
        Some(remote) => auk_remote::Target::Ssh(remote.clone()),
        None => {
            auk_daemon::spawn::ensure(&opts.exe, &opts.socket, &opts.state_dir)?;
            auk_remote::Target::Local(opts.socket.clone())
        }
    };
    let hello = Hello::new(ClientKind::Headless, ClientFacts::default());
    let (link, welcome, mut reader) = auk_remote::Link::connect(target, hello)?;
    link.send(&ClientMsg::Attach {
        window_key: opts.window_key.clone(),
    })?;

    let headless = Arc::new(Headless {
        view: Mutex::new(View {
            catalog: welcome.catalog,
            ..View::default()
        }),
        link,
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
        match reader.recv() {
            Ok(Some(msg)) => headless.on_message(msg),
            Ok(None) => break Ok(()),
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
