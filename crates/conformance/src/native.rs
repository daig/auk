//! A test window that is a pane's primary and edits natively (§7.9, §15.5): it attaches to the
//! daemon like the app, keeps a follower replica per pane, projects each pane's TSP state through
//! `auk-views` into an `auk-ui` document and lays it out, acks what it drew, and routes input the
//! way a window does: keys become PTY bytes encoded for the pane's modes, and native editing in
//! program fields (selection, typing over it, ⌘Z, `send`) goes through [`PaneView`]'s editing
//! entry points, whose `edit`, `undo` and `send` events reach the daemon as view events.
//!
//! The `omp` suite drives a real omp through it; nothing here knows about omp.

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use auk_mirror::{Client, ClientFacts, ClientKind, ClientMsg, DaemonMsg, Hello, PaneId, write_msg};
use auk_replica::{Item, Replica, Role, tsp_config};
use auk_term::input::{Key, KeyEvent, Mods, encode_key};
use auk_tsp::{MemoryBlobs, NodeRef, Surface, TspPane};
use auk_ui::{Document, NodeId, SheetKind};
use auk_views::{Outcome, PaneView, ViewSettings};
use serde_json::Value;

use crate::harness::TestEnv;
use crate::surfaces::{HOST_CSS, PANE_WIDTH};

/// One pane as the window shows it.
struct Shown {
    replica: Replica,
    doc: Document,
    view: PaneView,
    /// The last frame acked per surface key.
    acked: BTreeMap<u64, u64>,
    /// Blobs asked of the daemon, or sent by it after the snapshot (§7.12).
    asked: BTreeSet<String>,
}

/// A window attached to the test daemon that is primary for the panes it focuses.
pub struct NativeWindow {
    rx: Receiver<DaemonMsg>,
    stream: UnixStream,
    panes: BTreeMap<PaneId, Shown>,
    primary: BTreeMap<PaneId, bool>,
    exited: BTreeMap<PaneId, i32>,
    settings: ViewSettings,
    size: (u16, u16),
    start: Instant,
    /// Stream errors and refused sends; a correct daemon causes none.
    errors: Vec<String>,
    /// Every view event this window sent, as sent.
    pub sent_events: Vec<Value>,
}

impl std::fmt::Debug for NativeWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeWindow").finish_non_exhaustive()
    }
}

impl NativeWindow {
    /// Connects to the test daemon and attaches to `window_key`; panes are shown at `size`
    /// (columns, rows) while this window is their primary.
    pub fn attach(env: &TestEnv, window_key: &str, size: (u16, u16)) -> NativeWindow {
        let stream = UnixStream::connect(&env.socket)
            .unwrap_or_else(|e| panic!("connect to {}: {e}", env.socket.display()));
        let hello = Hello::new(ClientKind::App, ClientFacts::default());
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
        NativeWindow {
            rx,
            stream,
            panes: BTreeMap::new(),
            primary: BTreeMap::new(),
            exited: BTreeMap::new(),
            settings: ViewSettings::default(),
            size,
            start: Instant::now(),
            errors: Vec::new(),
            sent_events: Vec::new(),
        }
    }

    fn now(&self) -> f64 {
        self.start.elapsed().as_secs_f64() * 1000.0
    }

    fn send(&mut self, msg: &ClientMsg) {
        if let Err(e) = write_msg(&mut self.stream, msg) {
            self.errors.push(format!("send {msg:?}: {e}"));
        }
    }

    /// Applies everything the daemon has sent so far: projects TSP changes, lays the pane out
    /// and, as its primary, acks each live listening surface's last frame once drawn (§7.10).
    pub fn pump(&mut self) {
        let mut touched = Vec::new();
        while let Ok(msg) = self.rx.try_recv() {
            if let Some(pane) = self.apply(msg) {
                touched.push(pane);
            }
        }
        touched.sort_unstable();
        touched.dedup();
        for pane in touched {
            self.draw(pane);
        }
    }

    fn apply(&mut self, msg: DaemonMsg) -> Option<PaneId> {
        let now = self.now();
        let (pane, seq, item) = match &msg {
            DaemonMsg::Snapshot { pane, snapshot } => {
                let tsp = TspPane::with(
                    tsp_config(&snapshot.settings),
                    auk_views::sheet_checker(),
                    Box::new(MemoryBlobs::default()),
                );
                let mut replica = Replica::from_snapshot(snapshot, Role::Follower, Some(tsp));
                let mut doc = Document::new();
                let errors = auk_views::install_sheets(&mut doc, self.settings.dark);
                assert!(errors.is_empty(), "built-in sheets: {errors:?}");
                doc.set_stylesheet("host", SheetKind::BuiltIn, None, HOST_CSS);
                let root = doc.root();
                let body = doc.create_element("body");
                doc.append_child(root, body);
                doc.set_viewport(PANE_WIDTH, crate::webkit::PAGE_HEIGHT);
                let mut view = PaneView::new(&mut doc, self.settings.clone());
                doc.append_child(body, view.element());
                let changes = replica.take_tsp_changes();
                view.sync(&mut doc, replica.tsp(), &changes, now);
                let _ = replica.take_replies();
                // The daemon sends the blobs the snapshot names right after it.
                let asked = replica.tsp().missing_blobs().into_iter().collect();
                self.panes.insert(
                    *pane,
                    Shown {
                        replica,
                        doc,
                        view,
                        acked: BTreeMap::new(),
                        asked,
                    },
                );
                let (cols, rows) = self.size;
                let pane = *pane;
                self.send(&ClientMsg::Focus { pane });
                self.send(&ClientMsg::Resize { pane, cols, rows });
                return Some(pane);
            }
            DaemonMsg::Output { pane, seq, bytes } => (*pane, *seq, Item::Output(bytes)),
            DaemonMsg::Record { pane, seq, record } => (*pane, *seq, Item::Record(record)),
            DaemonMsg::Primary { pane, you } => {
                self.primary.insert(*pane, *you);
                if let Some(shown) = self.panes.get_mut(pane) {
                    // A new primary draws the current documents and acks them.
                    shown.acked.clear();
                }
                return Some(*pane);
            }
            DaemonMsg::PaneExit { pane, status } => {
                self.exited.insert(*pane, *status);
                return None;
            }
            DaemonMsg::Blob {
                pane,
                id,
                mime,
                bytes,
            } => {
                let shown = self.panes.get_mut(pane)?;
                if !shown
                    .replica
                    .deliver_blob(id, mime.as_deref(), bytes.clone())
                {
                    self.errors.push(format!("pane {pane}: bad blob {id}"));
                }
                let changes = shown.replica.take_tsp_changes();
                shown
                    .view
                    .sync(&mut shown.doc, shown.replica.tsp(), &changes, now);
                return Some(*pane);
            }
            _ => return None,
        };
        let shown = self.panes.get_mut(&pane)?;
        if let Err(e) = shown.replica.apply(seq, item) {
            self.errors.push(format!("pane {pane} seq {seq}: {e:?}"));
        }
        let _ = shown.replica.take_replies();
        let changes = shown.replica.take_tsp_changes();
        if !changes.is_empty() {
            shown
                .view
                .sync(&mut shown.doc, shown.replica.tsp(), &changes, now);
        }
        // Blobs a document names that this window lacks (they arrived before its snapshot).
        let ask: Vec<String> = shown
            .replica
            .tsp()
            .missing_blobs()
            .into_iter()
            .filter(|id| shown.asked.insert(id.clone()))
            .collect();
        if !ask.is_empty() {
            self.send(&ClientMsg::Blobs { pane, ids: ask });
        }
        Some(pane)
    }

    fn draw(&mut self, pane: PaneId) {
        let now = self.now();
        let primary = self.primary.get(&pane).copied().unwrap_or(false);
        let Some(shown) = self.panes.get_mut(&pane) else {
            return;
        };
        // A test's window draws deterministically: highlighting and pictures left to the
        // background are finished first (the tick draws decoded pictures in).
        shown.view.finish_background(&mut shown.doc);
        shown.view.tick(&mut shown.doc, shown.replica.tsp(), now);
        shown.view.layout(&mut shown.doc);
        if !primary {
            return;
        }
        let mut acks = Vec::new();
        for s in shown.replica.tsp().surfaces() {
            if !s.is_live() || !s.listens() {
                continue;
            }
            let Some(last) = s.last_s() else { continue };
            if shown.acked.get(&s.key().0) != Some(&last) {
                shown.acked.insert(s.key().0, last);
                acks.push(ClientMsg::Ack {
                    pane,
                    sf: s.id().to_owned(),
                    s: last,
                });
            }
        }
        for msg in acks {
            self.send(&msg);
        }
    }

    /// Applies what the daemon sent, then runs the view's clock and lays out every pane, as a
    /// window's frame does even when nothing arrived (toasts expire, motion advances).
    pub fn frame(&mut self) {
        self.pump();
        let panes: Vec<PaneId> = self.panes.keys().copied().collect();
        for pane in panes {
            self.draw(pane);
        }
    }

    /// Pumps until `f` returns `Some`, or panics naming `what` after `timeout`.
    pub fn wait<T>(
        &mut self,
        what: &str,
        timeout: Duration,
        mut f: impl FnMut(&NativeWindow) -> Option<T>,
    ) -> T {
        let deadline = Instant::now() + timeout;
        loop {
            self.pump();
            if let Some(v) = f(self) {
                return v;
            }
            if Instant::now() > deadline {
                let screens: Vec<String> = self
                    .panes
                    .iter()
                    .map(|(id, p)| format!("pane {id}:\n{}", p.replica.screen_text()))
                    .collect();
                panic!(
                    "timed out after {timeout:?} waiting for {what}\n{}",
                    screens.join("\n")
                );
            }
            std::thread::sleep(Duration::from_millis(15));
        }
    }

    /// Waits until the pane's replica is at the daemon's stream position with the daemon's state
    /// hash, and lays it out.
    pub fn converge(&mut self, env: &TestEnv, pane: PaneId) {
        self.wait(
            "the window's replica to match the daemon's",
            crate::harness::WAIT,
            |w| {
                let (hash, seq) = env.daemon_hash(pane);
                let r = w.try_replica(pane)?;
                let ours: String = r.state_hash().iter().map(|b| format!("{b:02x}")).collect();
                ((ours, r.seq()) == (hash, seq)).then_some(())
            },
        );
        self.draw(pane);
        assert!(self.errors.is_empty(), "stream errors: {:?}", self.errors);
    }

    /// The text under element `el`, concatenated in document order.
    pub fn text_content(&self, pane: PaneId, el: NodeId) -> String {
        let doc = self.doc(pane);
        let mut out = String::new();
        let mut stack = vec![el];
        while let Some(n) = stack.pop() {
            if let Some(t) = doc.node(n).text() {
                out.push_str(t);
            }
            stack.extend(doc.children(n).iter().rev());
        }
        out
    }

    /// Elements under `el` (itself included) whose written class list has `class`.
    pub fn with_class(&self, pane: PaneId, el: NodeId, class: &str) -> Vec<NodeId> {
        let doc = self.doc(pane);
        let mut out = Vec::new();
        let mut stack = vec![el];
        while let Some(n) = stack.pop() {
            if self.class(pane, n).split(' ').any(|c| c == class) {
                out.push(n);
            }
            stack.extend(doc.children(n).iter().rev());
        }
        out
    }

    /// Stream errors and failed sends seen so far.
    pub fn errors(&self) -> &[String] {
        &self.errors
    }

    /// Whether this window is the pane's primary.
    pub fn is_primary(&self, pane: PaneId) -> bool {
        self.primary.get(&pane).copied().unwrap_or(false)
    }

    /// The pane's exit status, once the daemon reported it.
    pub fn exit_status(&self, pane: PaneId) -> Option<i32> {
        self.exited.get(&pane).copied()
    }

    /// The pane's replica, if the window has its snapshot.
    pub fn try_replica(&self, pane: PaneId) -> Option<&Replica> {
        self.panes.get(&pane).map(|s| &s.replica)
    }

    /// The pane's replica.
    pub fn replica(&self, pane: PaneId) -> &Replica {
        self.try_replica(pane).expect("a replica for the pane")
    }

    /// The pane's TSP state.
    pub fn tsp(&self, pane: PaneId) -> &TspPane {
        self.replica(pane).tsp()
    }

    /// The pane's projected document.
    pub fn doc(&self, pane: PaneId) -> &Document {
        &self.panes[&pane].doc
    }

    /// The pane's view.
    pub fn view(&self, pane: PaneId) -> &PaneView {
        &self.panes[&pane].view
    }

    /// The class text the views wrote on `el`.
    pub fn class(&self, pane: PaneId, el: NodeId) -> String {
        self.view(pane).written().class(el).unwrap_or("").to_owned()
    }

    /// The live surface named `sf`, if any.
    pub fn live_surface(&self, pane: PaneId, sf: &str) -> Option<&Surface> {
        self.try_replica(pane)?
            .tsp()
            .surfaces()
            .iter()
            .rev()
            .find(|s| s.id() == sf && s.is_live())
    }

    /// The element node `id` of `surface` was projected to.
    pub fn element_of(&self, pane: PaneId, surface: &Surface, id: &str) -> Option<NodeId> {
        self.view(pane).node_element(surface.key(), id)
    }

    /// The focused `editor` or `input` of the pane's live surfaces: (surface id, node id).
    pub fn focused_field(&self, pane: PaneId) -> Option<(String, String)> {
        let tsp = self.try_replica(pane)?.tsp();
        tsp.surfaces()
            .iter()
            .rev()
            .filter(|s| s.is_live())
            .find_map(|s| {
                let id = s.document().focus()?;
                let n = s.document().get(id)?;
                matches!(n.kind(), "editor" | "input").then(|| (s.id().to_owned(), id.to_owned()))
            })
    }

    /// A field's node in the newest live surface that has it.
    pub fn field<'a>(&'a self, pane: PaneId, id: &str) -> Option<NodeRef<'a>> {
        let tsp = self.try_replica(pane)?.tsp();
        tsp.surfaces()
            .iter()
            .rev()
            .filter(|s| s.is_live())
            .find_map(|s| s.document().get(id))
    }

    // -----------------------------------------------------------------------------------------
    // Input

    /// Sends raw bytes to the pane's program.
    pub fn input(&mut self, pane: PaneId, bytes: &[u8]) {
        self.send(&ClientMsg::Input {
            pane,
            bytes: bytes.to_vec(),
        });
    }

    /// Types text as keys (each character a key press without modifiers).
    pub fn type_keys(&mut self, pane: PaneId, text: &str) {
        let modes = self.replica(pane).terminal().modes();
        let mut bytes = Vec::new();
        for c in text.chars() {
            let ev = KeyEvent {
                key: Key::Char(c.to_ascii_lowercase()),
                mods: Mods {
                    shift: c.is_ascii_uppercase(),
                    ..Mods::default()
                },
                text: Some(c.to_string()),
            };
            bytes.extend(encode_key(&ev, &modes));
        }
        self.input(pane, &bytes);
    }

    /// Presses one key with modifiers, encoded for the pane's current modes.
    pub fn press(&mut self, pane: PaneId, key: Key, mods: Mods) {
        let modes = self.replica(pane).terminal().modes();
        let text = match key {
            Key::Char(c) if !mods.ctrl && !mods.alt && !mods.meta => Some(c.to_string()),
            _ => None,
        };
        let bytes = encode_key(&KeyEvent { key, mods, text }, &modes);
        self.input(pane, &bytes);
    }

    /// Ctrl plus a letter.
    pub fn ctrl(&mut self, pane: PaneId, c: char) {
        self.press(
            pane,
            Key::Char(c),
            Mods {
                ctrl: true,
                ..Mods::default()
            },
        );
    }

    fn outcome(&mut self, pane: PaneId, outcome: Outcome) -> Outcome {
        if let Outcome::Event { sf, event, .. } = &outcome {
            let mut body = event.body(sf);
            if let Some(obj) = body.as_object_mut() {
                obj.remove("sf");
            }
            self.sent_events.push(event.body(sf));
            let msg = ClientMsg::ViewEvent {
                pane,
                sf: sf.clone(),
                event: body,
            };
            self.send(&msg);
        }
        self.draw(pane);
        outcome
    }

    /// A native selection in the focused field (selecting keys, a drag, ⌘A): local only.
    pub fn select(&mut self, pane: PaneId, anchor: u64, cursor: u64) -> Outcome {
        let shown = self.panes.get_mut(&pane).expect("a shown pane");
        let o = shown
            .view
            .select(&mut shown.doc, shown.replica.tsp(), anchor, cursor);
        self.outcome(pane, o)
    }

    /// Typing (or pasting) `text` in the focused field: an `edit` over a native selection,
    /// otherwise keys.
    pub fn type_in_field(&mut self, pane: PaneId, text: &str) -> Outcome {
        let shown = self.panes.get_mut(&pane).expect("a shown pane");
        let o = shown
            .view
            .type_text(&mut shown.doc, shown.replica.tsp(), text);
        if o == Outcome::Keys {
            self.type_keys(pane, text);
        }
        self.outcome(pane, o)
    }

    /// ⌘Z in the focused field: an `undo` event when the program allows it. Otherwise the result
    /// is [`Outcome::Keys`] and nothing is sent: ⌘Z has no terminal encoding, and the caller
    /// decides whether to send a key instead.
    pub fn undo(&mut self, pane: PaneId) -> Outcome {
        let shown = self.panes.get_mut(&pane).expect("a shown pane");
        let o = shown.view.undo(shown.replica.tsp());
        self.outcome(pane, o)
    }

    /// The window's appearance: its facts (with the theme's colors, as the app sends them) and
    /// the views' theme for every shown pane.
    pub fn set_dark(&mut self, dark: bool) {
        self.settings.dark = dark;
        let facts = ClientFacts {
            dark,
            colors: Some(auk_mirror::ThemeColors::from(
                &auk_term::Facts::for_appearance(dark),
            )),
            ..ClientFacts::default()
        };
        self.send(&ClientMsg::Facts(facts));
        let settings = self.settings.clone();
        for shown in self.panes.values_mut() {
            shown
                .view
                .set_settings(&mut shown.doc, shown.replica.tsp(), settings.clone());
        }
    }

    /// The window gained or lost keyboard focus: the pane's views hear it, as the app's do.
    pub fn set_focused(&mut self, pane: PaneId, focused: bool) {
        let shown = self.panes.get_mut(&pane).expect("a shown pane");
        shown
            .view
            .set_focused(&mut shown.doc, shown.replica.tsp(), focused);
        self.draw(pane);
    }

    /// A primary-button click at a point of the pane's document; events go to the program.
    pub fn click(&mut self, pane: PaneId, x: f32, y: f32) -> Vec<Outcome> {
        let shown = self.panes.get_mut(&pane).expect("a shown pane");
        let out = shown.view.click(
            &mut shown.doc,
            shown.replica.tsp(),
            auk_views::PointerEvent {
                x,
                y,
                clicks: 1,
                ..Default::default()
            },
        );
        out.into_iter().map(|o| self.outcome(pane, o)).collect()
    }

    /// Submits a prompt atomically into field `id` (feature `send`). `None` when the field
    /// can't take one.
    pub fn send_prompt(&mut self, pane: PaneId, id: &str, text: &str) -> Option<Outcome> {
        let shown = self.panes.get(&pane).expect("a shown pane");
        let o = shown.view.send(shown.replica.tsp(), id, text)?;
        Some(self.outcome(pane, o))
    }
}

impl Drop for NativeWindow {
    fn drop(&mut self) {
        let _ = write_msg(&mut self.stream, &ClientMsg::Detach);
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

/// The kinds of every node under `node`, itself included, in document order.
pub fn kinds_under(node: NodeRef<'_>) -> Vec<(String, String)> {
    let mut out = vec![(node.id().to_owned(), node.kind().to_owned())];
    for c in node.children() {
        out.extend(kinds_under(c));
    }
    out
}
