//! A test window that draws surfaces (§8.3, §18.2): it attaches to the daemon like the app, keeps
//! a follower replica per pane with the views' sheet checker, and projects each pane's TSP state
//! through `auk-views` into an `auk-ui` document, as a window does before painting. Suites read the
//! laid-out boxes, the classes the views wrote, and the page `auk-views` exports for WebKit.

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{self, Receiver};
use std::time::Instant;

use auk_mirror::{Client, ClientFacts, ClientKind, ClientMsg, DaemonMsg, Hello, PaneId, write_msg};
use auk_replica::{Item, Replica, Role, tsp_config};
use auk_tsp::{MemoryBlobs, TspPane};
use auk_ui::{Document, NodeId, SheetKind};
use auk_views::{PaneView, ViewSettings};
use serde_json::Value;

use crate::harness::{TestEnv, wait_for};

/// The host's own CSS around a pane (the window's sheets stand in here).
pub const HOST_CSS: &str = "body { margin: 0; background-color: var(--page); }";

/// The width the window gives a pane, in CSS px.
pub const PANE_WIDTH: f32 = 960.0;

/// One pane's projection: the document, the view and the document's `body`.
pub struct Projection {
    pub doc: Document,
    pub view: PaneView,
    pub body: NodeId,
}

impl std::fmt::Debug for Projection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Projection").finish_non_exhaustive()
    }
}

impl Projection {
    fn new(settings: ViewSettings) -> Projection {
        let mut doc = Document::new();
        let errors = auk_views::install_sheets(&mut doc, settings.dark);
        assert!(errors.is_empty(), "built-in sheets: {errors:?}");
        doc.set_stylesheet("host", SheetKind::BuiltIn, None, HOST_CSS);
        let root = doc.root();
        let body = doc.create_element("body");
        doc.append_child(root, body);
        doc.set_viewport(PANE_WIDTH, crate::webkit::PAGE_HEIGHT);
        let view = PaneView::new(&mut doc, settings);
        let el = view.element();
        doc.append_child(body, el);
        Projection { doc, view, body }
    }

    /// The class text the views wrote on `el`.
    pub fn class(&self, el: NodeId) -> String {
        self.view.written().class(el).unwrap_or("").to_owned()
    }

    /// The page `auk-views` exports for this projection: the same DOM and CSS.
    pub fn html(&self) -> String {
        self.view.to_html(&self.doc, self.body, HOST_CSS)
    }

    /// Every element box under `body`, keyed like `webkit-boxes`, with the views' classes.
    pub fn boxes(&self) -> Vec<crate::webkit::BoxRec> {
        crate::layout::our_boxes(&self.doc, self.body, &|el| self.class(el))
    }
}

/// An attached window drawing every pane it is sent.
pub struct SurfaceWindow {
    rx: Receiver<DaemonMsg>,
    stream: UnixStream,
    replicas: BTreeMap<PaneId, Replica>,
    projections: BTreeMap<PaneId, Projection>,
    /// Blobs asked of the daemon, or sent by it after a snapshot (§7.12), per pane.
    asked: BTreeMap<PaneId, BTreeSet<String>>,
    settings: ViewSettings,
    start: Instant,
    /// Out-of-order items or a lost connection; a correct daemon causes none.
    errors: Vec<String>,
}

impl std::fmt::Debug for SurfaceWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SurfaceWindow").finish_non_exhaustive()
    }
}

impl SurfaceWindow {
    /// Connects to the test daemon and attaches to `window_key` (`main` holds panes `new` makes).
    pub fn attach(env: &TestEnv, window_key: &str) -> SurfaceWindow {
        let stream = UnixStream::connect(&env.socket)
            .unwrap_or_else(|e| panic!("connect to {}: {e}", env.socket.display()));
        let hello = Hello::new(ClientKind::Headless, ClientFacts::default());
        let (mut client, _welcome) =
            Client::handshake(stream.try_clone().expect("clone a stream"), &hello)
                .unwrap_or_else(|e| panic!("handshake with the daemon: {e:?}"));
        client
            .send(&ClientMsg::Attach {
                window_key: window_key.to_owned(),
            })
            .expect("send attach");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            while let Ok(Some(msg)) = client.recv() {
                if tx.send(msg).is_err() {
                    break;
                }
            }
        });
        SurfaceWindow {
            rx,
            stream,
            replicas: BTreeMap::new(),
            projections: BTreeMap::new(),
            asked: BTreeMap::new(),
            settings: ViewSettings::default(),
            start: Instant::now(),
            errors: Vec::new(),
        }
    }

    fn now(&self) -> f64 {
        self.start.elapsed().as_secs_f64() * 1000.0
    }

    /// Applies everything the daemon has sent so far, projecting TSP changes as they come.
    pub fn pump(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            self.apply(msg);
        }
    }

    fn apply(&mut self, msg: DaemonMsg) {
        let now = self.now();
        let (pane, seq, item) = match &msg {
            DaemonMsg::Snapshot { pane, snapshot } => {
                let tsp = TspPane::with(
                    tsp_config(&snapshot.settings),
                    auk_views::sheet_checker(),
                    Box::new(MemoryBlobs::default()),
                );
                let mut replica = Replica::from_snapshot(snapshot, Role::Follower, Some(tsp));
                let mut p = Projection::new(self.settings.clone());
                let changes = replica.take_tsp_changes();
                p.view.sync(&mut p.doc, replica.tsp(), &changes, now);
                // The daemon sends the blobs the snapshot names right after it.
                self.asked
                    .insert(*pane, replica.tsp().missing_blobs().into_iter().collect());
                self.replicas.insert(*pane, replica);
                self.projections.insert(*pane, p);
                return;
            }
            DaemonMsg::Output { pane, seq, bytes } => (*pane, *seq, Item::Output(bytes)),
            DaemonMsg::Record { pane, seq, record } => (*pane, *seq, Item::Record(record)),
            DaemonMsg::Blob {
                pane,
                id,
                mime,
                bytes,
            } => {
                let (Some(replica), Some(p)) =
                    (self.replicas.get_mut(pane), self.projections.get_mut(pane))
                else {
                    return;
                };
                if !replica.deliver_blob(id, mime.as_deref(), bytes.clone()) {
                    self.errors.push(format!("pane {pane}: bad blob {id}"));
                }
                let changes = replica.take_tsp_changes();
                p.view.sync(&mut p.doc, replica.tsp(), &changes, now);
                return;
            }
            _ => return,
        };
        let (Some(replica), Some(p)) = (
            self.replicas.get_mut(&pane),
            self.projections.get_mut(&pane),
        ) else {
            return;
        };
        if let Err(e) = replica.apply(seq, item) {
            self.errors.push(format!("pane {pane} seq {seq}: {e:?}"));
        }
        let _ = replica.take_replies();
        let changes = replica.take_tsp_changes();
        if !changes.is_empty() {
            p.view.sync(&mut p.doc, replica.tsp(), &changes, now);
        }
        // Blobs a document names that this window lacks (they arrived before its snapshot).
        let asked = self.asked.entry(pane).or_default();
        let ask: Vec<String> = replica
            .tsp()
            .missing_blobs()
            .into_iter()
            .filter(|id| asked.insert(id.clone()))
            .collect();
        if !ask.is_empty()
            && let Err(e) = write_msg(&mut self.stream, &ClientMsg::Blobs { pane, ids: ask })
        {
            self.errors.push(format!("ask for blobs: {e}"));
        }
    }

    /// Waits until the pane's replica is at the daemon's stream position with the daemon's state
    /// hash, then runs the view's clock tick and lays the pane out.
    pub fn converge(&mut self, env: &TestEnv, pane: PaneId) -> &Projection {
        wait_for("the window's replica to match the daemon's", || {
            self.pump();
            let (hash, seq) = env.daemon_hash(pane);
            let r = self.replicas.get(&pane)?;
            let ours: String = r.state_hash().iter().map(|b| format!("{b:02x}")).collect();
            ((ours, r.seq()) == (hash, seq)).then_some(())
        });
        assert!(self.errors.is_empty(), "stream errors: {:?}", self.errors);
        let now = self.now();
        let replica = &self.replicas[&pane];
        let p = self.projections.get_mut(&pane).expect("a projection");
        // Compared against WebKit: highlighting and pictures left to the background are
        // finished first (the tick draws decoded pictures in).
        p.view.finish_background(&mut p.doc);
        p.view.tick(&mut p.doc, replica.tsp(), now);
        p.view.layout(&mut p.doc);
        p
    }

    /// The pane's replica.
    pub fn replica(&self, pane: PaneId) -> &Replica {
        self.replicas.get(&pane).expect("a replica for the pane")
    }

    /// The pane's projection, as last laid out.
    pub fn projection(&self, pane: PaneId) -> &Projection {
        self.projections
            .get(&pane)
            .expect("a projection for the pane")
    }

    /// The element of node `id` in the newest surface named `sf`.
    pub fn element(&self, pane: PaneId, sf: &str, id: &str) -> Option<NodeId> {
        let key = self
            .replica(pane)
            .tsp()
            .surfaces()
            .iter()
            .rev()
            .find(|s| s.id() == sf)?
            .key();
        self.projection(pane).view.node_element(key, id)
    }
}

impl Drop for SurfaceWindow {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

/// The kinds test's surface (`probes/kinds.json`): `main` is frame 1's content, a node of every T0
/// and T1 kind but `overlay`; `frame2` is frame 2's ops, which add the overlay in `layer` and focus
/// the editor. Shared by the `layout` suite and `auk-views`' own kinds tests.
pub fn kinds_surface() -> (Value, Value) {
    let path = crate::layout::probes_dir().join("kinds.json");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut v: Value =
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    (v["main"].take(), v["frame2"].take())
}
