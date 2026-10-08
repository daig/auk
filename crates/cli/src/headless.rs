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
//! It also draws (`draw.rs`) and takes the scenario commands of §11.3 (`script.rs`): harness
//! commands (`size`, `appearance`, `wait`, `freeze-anims`, `thaw`, `motion-at`, `key`, `type`,
//! `paste`, `shot`), control commands (`tree`, `state`, `css`, `dump`, `perf`) and terminal
//! commands (`ready`, `run`, `expect`, `split`, `new`, `close`, `focus`, `feed`, `surface-play`,
//! `plugins …`). Each settles before it answers: the window's replicas reach the daemon's stream
//! position, the shown panes have the size the window gives them, and the stream goes quiet.
//!
//! With [`Options::remote`] the window attaches to a remote host's daemon over SSH
//! (`auk-remote`, §10.2): the link reconnects and resyncs after a drop, holding typing meanwhile,
//! and `remote` reports its state and the host's identity chain.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Instant;

pub mod draw;
mod gesture;
mod script;

pub use script::fixture_root;

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
    /// Deterministic scenario mode (`auk serve`, `auk shot`); `None` for a plain headless window.
    pub scenario: Option<Scenario>,
}

/// A deterministic headless window's setup (§11.3).
#[derive(Clone)]
pub struct Scenario {
    pub spec: draw::WindowSpec,
    /// Where `shot` writes.
    pub out: PathBuf,
    /// Environment for `auk plugin` runs against the window's daemon.
    pub plugin_env: Vec<(String, String)>,
    /// Runs before `quit` exits (stops the private daemon).
    pub on_quit: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl std::fmt::Debug for Scenario {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scenario")
            .field("spec", &self.spec)
            .field("out", &self.out)
            .finish_non_exhaustive()
    }
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
    /// TSP changes not yet drawn, per pane, in order (the draw thread's views take them).
    pending: BTreeMap<PaneId, Vec<auk_tsp::Changes>>,
    /// The pane this window has focused.
    focused: Option<PaneId>,
    /// When the daemon last sent something.
    last_message: Option<Instant>,
    /// Panes whose replica was rebuilt from a snapshot: their drawn views start over.
    reset: std::collections::BTreeSet<PaneId>,
    /// Blobs asked of the daemon, or sent by it after a snapshot (§7.12), per pane.
    asked: BTreeMap<PaneId, std::collections::BTreeSet<String>>,
}

/// Work for the draw thread.
type DrawJob = Box<dyn FnOnce(&Headless, &mut Option<draw::Drawer>) + Send>;

struct Headless {
    view: Mutex<View>,
    /// The link to the host; `remote loopback` replaces it.
    link: std::sync::RwLock<auk_remote::Link>,
    /// Which link's reader may apply messages (an old reader stops when this moves on).
    generation: std::sync::atomic::AtomicU64,
    size: Option<(u16, u16)>,
    /// The host's control endpoint, socket and plugin environment.
    host: std::sync::RwLock<Host>,
    window_key: String,
    exe: PathBuf,
    /// Drawing, clocks and scenario state.
    script: Mutex<script::WindowState>,
    /// The draw thread, which owns the GPU and the documents (they stay on one thread).
    draw_tx: std::sync::OnceLock<std::sync::mpsc::Sender<DrawJob>>,
    deterministic: bool,
    on_quit: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Daemons this window started (`remote loopback`), stopped on `quit`.
    owned: Mutex<Vec<crate::scenario::Private>>,
}

/// Where the window's host takes control commands.
#[derive(Clone, Debug)]
struct Host {
    /// The daemon's control endpoint (none for a host over SSH).
    daemon: Option<PathBuf>,
    socket: PathBuf,
    /// Environment for `auk plugin` runs against the host's daemon.
    plugin_env: Vec<(String, String)>,
}

impl std::fmt::Debug for Headless {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Headless")
            .field("window_key", &self.window_key)
            .finish_non_exhaustive()
    }
}

impl Headless {
    fn link(&self) -> std::sync::RwLockReadGuard<'_, auk_remote::Link> {
        self.link.read().unwrap_or_else(|e| e.into_inner())
    }

    fn host(&self) -> Host {
        self.host.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Stops the daemons this window started.
    fn stop_owned(&self) {
        for mut p in std::mem::take(&mut *self.owned.lock().unwrap_or_else(|e| e.into_inner())) {
            p.stop();
        }
    }

    /// Attaches to another host: a new link, a new reader, and the views start over.
    fn switch_host(self: &Arc<Self>, target: auk_remote::Target, host: Host) -> io::Result<Value> {
        let hello = Hello::new(ClientKind::Headless, ClientFacts::default());
        let (link, welcome, reader) = auk_remote::Link::connect(target, hello)?;
        link.send(&ClientMsg::Attach {
            window_key: self.window_key.clone(),
        })?;
        let generation = self
            .generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        {
            let mut view = self.view();
            let old: Vec<PaneId> = view.replicas.keys().copied().collect();
            *view = View {
                catalog: welcome.catalog,
                ..View::default()
            };
            view.reset.extend(old);
        }
        let status = json!(link.status());
        *self.link.write().unwrap_or_else(|e| e.into_inner()) = link;
        *self.host.write().unwrap_or_else(|e| e.into_inner()) = host;
        spawn_reader(self.clone(), reader, generation)?;
        Ok(status)
    }

    fn view(&self) -> MutexGuard<'_, View> {
        self.view.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn send(&self, msg: &ClientMsg) -> io::Result<()> {
        self.link().send(msg).map(|_| ())
    }

    fn on_message(&self, msg: DaemonMsg) {
        let mut view = self.view();
        let mut draw = None;
        match msg {
            DaemonMsg::Snapshot { pane, snapshot } => {
                // The views' sheet checker, as a drawing window's replica has (§8.4).
                let tsp = auk_tsp::TspPane::with(
                    auk_replica::tsp_config(&snapshot.settings),
                    auk_views::sheet_checker(),
                    Box::new(auk_tsp::MemoryBlobs::default()),
                );
                let replica = Replica::from_snapshot(&snapshot, Role::Follower, Some(tsp));
                // The daemon sends the blobs the snapshot names right after it.
                view.asked
                    .insert(pane, replica.tsp().missing_blobs().into_iter().collect());
                view.replicas.insert(pane, replica);
                // A new replica starts its views over.
                view.pending.insert(pane, Vec::new());
                view.reset.insert(pane);
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
            DaemonMsg::Layout { window, .. } => {
                view.window = window;
                // Focus stays in the window; it moves when the focused pane leaves.
                let panes = view.window.panes();
                if view.focused.is_none_or(|f| !panes.contains(&f))
                    && let Some(&first) = panes.first()
                {
                    view.focused = Some(first);
                    // A scenario window focuses like an app window; a plain headless window
                    // only focuses when told (focus makes it the pane's primary, §15.5).
                    if self.deterministic {
                        let _ = self.send(&ClientMsg::Focus { pane: first });
                    }
                }
            }
            DaemonMsg::PaneExit { pane, status } => {
                view.exited.insert(pane, status);
            }
            DaemonMsg::Primary { pane, you } => {
                view.primary.insert(pane, you);
                // A new primary draws the current documents and acks them (E4).
                draw = Some((pane, true));
            }
            DaemonMsg::Effect(effect) => view.effects.push(json!(effect)),
            DaemonMsg::Blob {
                pane,
                id,
                mime,
                bytes,
            } => {
                if let Some(replica) = view.replicas.get_mut(&pane) {
                    if !replica.deliver_blob(&id, mime.as_deref(), bytes) {
                        view.errors.push(format!("pane {pane}: bad blob {id}"));
                    }
                    draw = Some((pane, false));
                }
            }
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
        view.last_message = Some(Instant::now());
        retry_views(&mut view);
        if let Some((pane, all)) = draw {
            let acks = draw_pane(&mut view, pane, all);
            drop(view);
            for msg in acks {
                let _ = self.send(&msg);
            }
        }
    }

    fn handle(self: &Arc<Self>, line: &str) -> Value {
        // Lines parse as script lines (`scenario::tokenize`): shell-style quoting, plus `\n`,
        // `\t` and `\e` escapes in double quotes.
        match crate::scenario::tokenize(line) {
            Ok(Some(words)) => self.handle_words(&words),
            Ok(None) => error("empty command"),
            Err(e) => error(e),
        }
    }

    /// Runs one command, given as words.
    fn handle_words(self: &Arc<Self>, words: &[String]) -> Value {
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
            "remote" => match args.first().map(String::as_str) {
                None => Ok(json!(self.link().status())),
                Some(_) => self
                    .scenario_command("remote", args)
                    .unwrap_or_else(|| Err("remote: unknown subcommand".into())),
            },
            "focus" if args.first().is_some_and(|a| a.parse::<PaneId>().is_ok()) => {
                pane_id(args).and_then(|pane| self.set_focus(pane).map(|()| Value::Null))
            }
            "resize" => match (pane_id(args), args.get(1..3)) {
                (Ok(pane), Some([c, r])) => match (c.parse(), r.parse()) {
                    (Ok(cols), Ok(rows)) => self.sent(&ClientMsg::Resize { pane, cols, rows }),
                    _ => Err("resize: bad size".into()),
                },
                _ => Err("resize: expected a pane, columns and rows".into()),
            },
            "quit" => {
                let _ = self.send(&ClientMsg::Detach);
                self.stop_owned();
                if let Some(f) = &self.on_quit {
                    f();
                }
                std::process::exit(0);
            }
            other => match self.scenario_command(other, args) {
                Some(result) => result,
                None => Err(format!("unknown command `{other}`")),
            },
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
        match self.link().send(msg) {
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
/// drawn. Headless rendering of surfaces arrives with the views; the ack point stays here. Blobs
/// the pane's documents name and the window lacks are asked for here too (§7.12).
fn draw_pane(view: &mut View, pane: PaneId, all: bool) -> Vec<ClientMsg> {
    let primary = view.primary.get(&pane).copied().unwrap_or(false);
    let View {
        replicas,
        surfaces,
        pending,
        asked,
        ..
    } = view;
    let Some(replica) = replicas.get_mut(&pane) else {
        return Vec::new();
    };
    // Blobs a document names that this window lacks (they arrived before its snapshot).
    let asked = asked.entry(pane).or_default();
    let ask: Vec<String> = replica
        .tsp()
        .missing_blobs()
        .into_iter()
        .filter(|id| asked.insert(id.clone()))
        .collect();
    let mut out = Vec::new();
    if !ask.is_empty() {
        out.push(ClientMsg::Blobs { pane, ids: ask });
    }
    let changes = replica.take_tsp_changes();
    if !changes.is_empty() {
        let pending = pending.entry(pane).or_default();
        pending.push(changes.clone());
        compact(pending);
    }
    let pv = surfaces.entry(pane).or_default();
    pv.forget(&changes);
    if !primary {
        return out;
    }
    out.extend(
        pv.acks(replica.tsp(), &changes, all)
            .into_iter()
            .map(|(sf, s)| ClientMsg::Ack { pane, sf, s }),
    );
    out
}

/// Keeps a pane's undrawn changes bounded: past 256 entries they merge into one, in order, and
/// surfaces that ended lose their earlier entries.
fn compact(pending: &mut Vec<auk_tsp::Changes>) {
    if pending.len() <= 256 {
        return;
    }
    let mut merged = auk_tsp::Changes::default();
    for c in pending.drain(..) {
        merged.surfaces.retain(|s| !c.removed.contains(&s.key));
        merged.surfaces.extend(c.surfaces);
        merged.removed.extend(c.removed);
        merged.blobs.extend(c.blobs);
        merged.pane |= c.pane;
    }
    pending.push(merged);
}

fn pane_id(args: &[String]) -> Result<PaneId, String> {
    let word = args.first().ok_or("missing pane")?;
    word.parse().map_err(|_| format!("bad pane `{word}`"))
}

/// A started headless window: commands go to [`Window::command`]; the daemon's messages arrive
/// on a thread until it goes away.
pub struct Started {
    headless: Arc<Headless>,
    reader: thread::JoinHandle<io::Result<()>>,
}

impl std::fmt::Debug for Started {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Started").finish_non_exhaustive()
    }
}

impl Started {
    /// Runs one command given as words, returning its JSON reply.
    pub fn command(&self, words: &[String]) -> Value {
        self.headless.handle_words(words)
    }

    /// Serves the control endpoint at `path` on a thread.
    pub fn serve(&self, path: &Path) -> io::Result<()> {
        let listener = control::bind(path)?;
        let headless = self.headless.clone();
        thread::spawn(move || {
            control::serve_listener_with(listener, Arc::new(move |line| headless.handle(line)))
        });
        Ok(())
    }

    /// Waits until the daemon goes away.
    pub fn join(self) -> io::Result<()> {
        self.reader
            .join()
            .unwrap_or_else(|_| Err(io::Error::other("the reader panicked")))
    }

    /// Detaches from the daemon and stops the daemons the window started.
    pub fn detach(&self) {
        let _ = self.headless.send(&ClientMsg::Detach);
        self.headless.stop_owned();
    }
}

/// Starts a headless window: attaches and starts taking the daemon's messages.
pub fn start(opts: &Options) -> io::Result<Started> {
    let target = match &opts.remote {
        Some(remote) => auk_remote::Target::Ssh(remote.clone()),
        None => {
            auk_daemon::spawn::ensure(&opts.exe, &opts.socket, &opts.state_dir)?;
            auk_remote::Target::Local(opts.socket.clone())
        }
    };
    let hello = Hello::new(ClientKind::Headless, ClientFacts::default());
    let (link, welcome, reader) = auk_remote::Link::connect(target, hello)?;
    link.send(&ClientMsg::Attach {
        window_key: opts.window_key.clone(),
    })?;

    let scenario = opts.scenario.clone();
    let headless = Arc::new(Headless {
        view: Mutex::new(View {
            catalog: welcome.catalog,
            ..View::default()
        }),
        link: std::sync::RwLock::new(link),
        generation: std::sync::atomic::AtomicU64::new(0),
        size: opts.size,
        host: std::sync::RwLock::new(Host {
            daemon: opts
                .remote
                .is_none()
                .then(|| auk_daemon::paths::control_socket(&opts.socket)),
            socket: opts.socket.clone(),
            plugin_env: scenario
                .as_ref()
                .map(|s| s.plugin_env.clone())
                .unwrap_or_default(),
        }),
        window_key: opts.window_key.clone(),
        exe: opts.exe.clone(),
        script: Mutex::new(script::WindowState::new(
            scenario.as_ref().map(|s| s.spec).unwrap_or_default(),
            scenario
                .as_ref()
                .map(|s| s.out.clone())
                .unwrap_or_else(|| PathBuf::from("target/shots/auk")),
            scenario.is_some(),
        )),
        draw_tx: std::sync::OnceLock::new(),
        deterministic: scenario.is_some(),
        on_quit: scenario.and_then(|s| s.on_quit),
        owned: Mutex::new(Vec::new()),
    });
    {
        let (tx, rx) = std::sync::mpsc::channel::<DrawJob>();
        let _ = headless.draw_tx.set(tx);
        let headless = headless.clone();
        thread::Builder::new()
            .name("headless draw".into())
            .spawn(move || {
                let mut drawer = None;
                while let Ok(job) = rx.recv() {
                    job(&headless, &mut drawer);
                }
            })?;
    }
    let reader = spawn_reader(headless.clone(), reader, 0)?;
    Ok(Started { headless, reader })
}

/// Applies a link's messages until it ends or a newer link replaces it.
fn spawn_reader(
    headless: Arc<Headless>,
    mut reader: auk_remote::LinkReader,
    generation: u64,
) -> io::Result<thread::JoinHandle<io::Result<()>>> {
    thread::Builder::new()
        .name("headless reader".into())
        .spawn(move || {
            let current = || {
                headless
                    .generation
                    .load(std::sync::atomic::Ordering::SeqCst)
                    == generation
            };
            loop {
                match reader.recv() {
                    Ok(Some(_)) if !current() => break Ok(()),
                    Ok(Some(msg)) => headless.on_message(msg),
                    Ok(None) => break Ok(()),
                    Err(err) => break Err(err.into()),
                }
            }
        })
}

/// Runs a headless window until the daemon goes away.
pub fn run(opts: &Options) -> io::Result<()> {
    let window = start(opts)?;
    window.serve(&opts.control)?;
    println!("ready {}", opts.control.display());
    let result = window.join();
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
