//! A window's connection to the daemon (§15.6): the handshake, `attach` for the window key, a
//! reader thread that applies the stream to the window's follower replicas, and the sending
//! side. Each window is one client, so the primary-replica policy (§15.5) ranks windows.
//!
//! The replicas live in [`Shared`], behind a mutex: the reader thread applies output as it
//! arrives (so a fast producer never waits for a frame), and the main thread reads them to draw.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;

use auk_mirror::layout::Window as Sessions;
use auk_mirror::{ClientFacts, ClientKind, ClientMsg, DaemonMsg, Effect, Hello, PaneId, Welcome};
use auk_remote::{Link, LinkReader, Target};
use auk_replica::{Item, Replica, Role};

/// One pane as this window follows it.
#[derive(Debug)]
pub struct PaneData {
    pub replica: Replica,
    /// The process's exit status, once it ended.
    pub exit: Option<i32>,
    /// Whether this window is the pane's primary (§15.5).
    pub primary: bool,
    /// Bumped whenever the replica changes, for redraws.
    pub generation: u64,
    /// The terminal rang the bell since the window last looked.
    pub bell: bool,
    /// A snapshot replaced the replica: the window's view of its surfaces starts over.
    pub reset: bool,
    /// Blobs already asked for (§7.12).
    pub asked: std::collections::BTreeSet<String>,
}

/// What the reader thread and the main thread share.
#[derive(Debug, Default)]
pub struct Shared {
    pub panes: BTreeMap<PaneId, PaneData>,
    /// The attached window key's sessions.
    pub sessions: Sessions,
    /// A layout message has arrived since attaching.
    pub layout_seen: bool,
    /// Bumped on every layout message.
    pub layout_generation: u64,
    /// Stream errors (items out of order or before a snapshot); a correct daemon causes none.
    pub errors: Vec<String>,
    /// Why the connection ended, once it has.
    pub closed: Option<String>,
    /// Effects the daemon asked this window to perform, oldest first.
    pub effects: Vec<Effect>,
}

impl Shared {
    fn apply(&mut self, pane: PaneId, seq: u64, item: Item<'_>) {
        let Some(data) = self.panes.get_mut(&pane) else {
            self.errors
                .push(format!("pane {pane}: item {seq} before a snapshot"));
            return;
        };
        match data.replica.apply(seq, item) {
            Ok(events) => {
                data.generation += 1;
                if events.iter().any(|e| matches!(e, auk_replica::Event::Bell)) {
                    data.bell = true;
                }
            }
            Err(err) => self.errors.push(format!("pane {pane}: {err}")),
        }
    }

    fn on_message(&mut self, msg: DaemonMsg) {
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
                let asked = replica.tsp().missing_blobs().into_iter().collect();
                let exit = snapshot.exit;
                let previous = self.panes.remove(&pane);
                self.panes.insert(
                    pane,
                    PaneData {
                        replica,
                        exit,
                        primary: previous.as_ref().is_some_and(|p| p.primary),
                        generation: previous.map_or(0, |p| p.generation + 1),
                        bell: false,
                        reset: true,
                        asked,
                    },
                );
            }
            DaemonMsg::Output { pane, seq, bytes } => self.apply(pane, seq, Item::Output(&bytes)),
            DaemonMsg::Record { pane, seq, record } => self.apply(pane, seq, Item::Record(&record)),
            DaemonMsg::Layout { window, .. } => {
                // Panes that left the layout are no longer shown here.
                let shown = window.panes();
                self.panes.retain(|id, _| shown.contains(id));
                self.sessions = window;
                self.layout_seen = true;
                self.layout_generation += 1;
            }
            DaemonMsg::PaneExit { pane, status } => {
                if let Some(p) = self.panes.get_mut(&pane) {
                    p.exit = Some(status);
                    p.generation += 1;
                }
            }
            DaemonMsg::Primary { pane, you } => {
                if let Some(p) = self.panes.get_mut(&pane) {
                    p.primary = you;
                    p.generation += 1;
                }
            }
            DaemonMsg::Effect(effect) => self.effects.push(effect),
            // A blob the pane's documents name, sent after a snapshot (§7.12); nodes naming it
            // redraw from the replica's changes.
            DaemonMsg::Blob {
                pane,
                id,
                mime,
                bytes,
            } => {
                if let Some(p) = self.panes.get_mut(&pane)
                    && p.replica.deliver_blob(&id, mime.as_deref(), bytes)
                {
                    p.generation += 1;
                }
            }
            DaemonMsg::Welcome { .. }
            | DaemonMsg::Refused(_)
            | DaemonMsg::LensView { .. }
            | DaemonMsg::Catalog { .. }
            | DaemonMsg::Presence { .. } => {}
        }
    }
}

/// A window's connection: an `auk-remote` link to the local daemon, which reconnects and
/// resyncs after the daemon upgrades in place (§15.7).
#[derive(Debug)]
pub struct Connection {
    pub window_key: String,
    pub client_id: u64,
    shared: Arc<Mutex<Shared>>,
    link: Link,
    reader: Mutex<Option<LinkReader>>,
}

impl Connection {
    /// Connects and says hello. The welcome lists every window key's sessions; then
    /// [`Connection::attach`] picks one. Nothing is read until [`Connection::start`].
    pub fn open(socket: &Path, facts: ClientFacts) -> io::Result<(Connection, Welcome)> {
        let hello = Hello::new(ClientKind::App, facts);
        let (link, welcome, reader) = Link::connect(Target::Local(socket.to_owned()), hello)
            .map_err(|e| io::Error::other(format!("{}: {e}", socket.display())))?;
        Ok((
            Connection {
                window_key: String::new(),
                client_id: welcome.client,
                shared: Arc::new(Mutex::new(Shared::default())),
                link,
                reader: Mutex::new(Some(reader)),
            },
            welcome,
        ))
    }

    /// Shows `window_key`'s sessions on this connection: the daemon answers with the layout, a
    /// snapshot per pane, then live output (§15.3).
    pub fn attach(&mut self, window_key: &str) -> io::Result<()> {
        self.window_key = window_key.to_owned();
        self.send(&ClientMsg::Attach {
            window_key: window_key.to_owned(),
        })
    }

    /// Starts the reader thread. `wake` runs after each batch of messages (from that thread).
    pub fn start(&self, wake: impl Fn() + Send + 'static) {
        let Some(mut reader) = self.reader.lock().unwrap_or_else(|e| e.into_inner()).take() else {
            return;
        };
        let shared = self.shared.clone();
        let spawned = thread::Builder::new()
            .name(format!("auk window {}", self.window_key))
            .spawn(move || {
                let reason = loop {
                    match reader.recv() {
                        Ok(Some(msg)) => {
                            lock(&shared).on_message(msg);
                            wake();
                        }
                        Ok(None) => break "the window detached".to_owned(),
                        Err(err) => break err.to_string(),
                    }
                };
                lock(&shared).closed = Some(reason);
                wake();
            });
        if let Err(err) = spawned {
            lock(&self.shared).closed = Some(format!("reader thread: {err}"));
        }
    }

    pub fn shared(&self) -> MutexGuard<'_, Shared> {
        lock(&self.shared)
    }

    /// Sends a message; while the link resyncs after a reconnect it holds input and drops acks.
    pub fn send(&self, msg: &ClientMsg) -> io::Result<()> {
        self.link.send(msg).map(|_| ())
    }

    /// The link's state (`connected`, `reconnecting(n)`, …).
    pub fn link_state(&self) -> String {
        self.link.state().to_string()
    }

    /// Sends input for a pane. A failure means the connection is gone, which the reader
    /// reports; the input is dropped.
    pub fn input(&self, pane: PaneId, bytes: Vec<u8>) {
        if !bytes.is_empty() {
            let _ = self.send(&ClientMsg::Input { pane, bytes });
        }
    }

    /// Says goodbye; the daemon drops this client.
    pub fn detach(&self) {
        let _ = self.send(&ClientMsg::Detach);
    }
}

fn lock(shared: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    shared.lock().unwrap_or_else(|e| e.into_inner())
}
