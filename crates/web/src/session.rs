//! The browser client's connection state: the handshake, a follower replica per pane of
//! the attached window key, the layout, and what the client sends. Pure, so it runs and is
//! tested natively; the wasm glue feeds it WebSocket messages and drains its outbox.

use std::collections::{BTreeMap, BTreeSet};

use auk_mirror::layout::{Node, SessionId, Tab, Window};
use auk_mirror::{
    ClientFacts, ClientKind, ClientMsg, DaemonMsg, Effect, Frame, Hello, Message, PaneId,
    WireError, frame::split_frame,
};
use auk_replica::{Item, Replica, Role};
use auk_term::input::{KeyEvent, encode_key, encode_paste};
use auk_views::chrome::{BarModel, SessionItem, TabItem};

/// Where the connection is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// The hello is out; no answer yet.
    Connecting,
    /// Welcomed and attached to the window key.
    Attached,
    /// The daemon runs another version: reload after upgrading.
    Refused(String),
    /// The connection ended.
    Closed(String),
}

impl Status {
    pub fn name(&self) -> &'static str {
        match self {
            Status::Connecting => "connecting",
            Status::Attached => "attached",
            Status::Refused(_) => "refused",
            Status::Closed(_) => "closed",
        }
    }
}

/// One pane as this client follows it.
#[derive(Debug)]
pub struct PaneState {
    pub replica: Replica,
    /// Whether this client is the pane's primary.
    pub primary: bool,
    /// The process's exit status, once it ended.
    pub exit: Option<i32>,
    /// The size last asked for.
    pub asked: Option<(u16, u16)>,
    /// The last frame acked per surface.
    pub acked: BTreeMap<String, u64>,
    /// TSP changes the client hasn't drawn yet, in order ([`Session::take_changes`]).
    pub changes: Vec<auk_tsp::Changes>,
    /// A snapshot replaced the replica: the client's view of its surfaces starts over.
    pub reset: bool,
    /// Blobs already asked for.
    pub blobs_asked: BTreeSet<String>,
}

impl PaneState {
    /// Keeps the replica's TSP changes for the client to draw, bounded: past 256 batches they
    /// merge into one, in order.
    fn keep_changes(&mut self) {
        let changes = self.replica.take_tsp_changes();
        if changes.is_empty() {
            return;
        }
        self.changes.push(changes);
        if self.changes.len() > 256 {
            let mut merged = auk_tsp::Changes::default();
            for c in self.changes.drain(..) {
                merged.surfaces.retain(|s| !c.removed.contains(&s.key));
                merged.surfaces.extend(c.surfaces);
                merged.removed.extend(c.removed);
                merged.blobs.extend(c.blobs);
                merged.pane |= c.pane;
            }
            self.changes.push(merged);
        }
    }
}

/// The client's state.
#[derive(Debug)]
pub struct Session {
    pub window_key: String,
    pub status: Status,
    pub client_id: Option<u64>,
    pub panes: BTreeMap<PaneId, PaneState>,
    /// The attached window key's sessions.
    pub window: Window,
    /// The plugin catalog: the web client runs no Lua and learns block types, lens claims
    /// and plugin styles from it.
    pub catalog: serde_json::Value,
    /// The Ready plugins' manifest styles from the catalog, as `(plugin id, css)` in id order:
    /// the client installs them as plugin sheets in every pane's document.
    pub styles: Vec<(String, String)>,
    /// Bumped whenever [`Session::styles`] changes.
    pub styles_serial: u64,
    /// Effects the serving host asked this window to perform, oldest first
    /// ([`Session::take_effects`]).
    pub effects: Vec<Effect>,
    /// Protocol errors: items out of order, undecodable frames. A correct daemon causes none.
    pub errors: Vec<String>,
    /// Something visible changed since the last [`Session::take_dirty`].
    dirty: bool,
    outbox: Vec<u8>,
    /// Panes whose primary status we asked for by focusing.
    focused: Option<PaneId>,
    /// Lens views that arrived before their block (replicas retry an early view for 5 s):
    /// pane, block, view, arrival time.
    early_views: Vec<(PaneId, u64, Option<serde_json::Value>, f64)>,
    /// The clock, in seconds ([`Session::set_time`]).
    now: f64,
    /// Facts that changed before the welcome, sent once it comes.
    pending_facts: Option<ClientFacts>,
}

/// How long an early lens view waits for its block.
pub const EARLY_VIEW_SECS: f64 = 5.0;

impl Session {
    /// A session about to say hello with `facts`.
    pub fn new(window_key: &str, facts: ClientFacts) -> Session {
        let mut s = Session {
            window_key: window_key.to_owned(),
            status: Status::Connecting,
            client_id: None,
            panes: BTreeMap::new(),
            window: Window::default(),
            catalog: serde_json::Value::Null,
            styles: Vec::new(),
            styles_serial: 0,
            effects: Vec::new(),
            errors: Vec::new(),
            dirty: true,
            outbox: Vec::new(),
            focused: None,
            early_views: Vec::new(),
            now: 0.0,
            pending_facts: None,
        };
        s.send(&ClientMsg::Hello(Hello::new(ClientKind::Web, facts)));
        s
    }

    fn send(&mut self, msg: &ClientMsg) {
        msg.encode(&mut self.outbox);
    }

    /// Frames to send, one per message, in order.
    pub fn take_outbox(&mut self) -> Vec<Vec<u8>> {
        let bytes = std::mem::take(&mut self.outbox);
        let mut out = Vec::new();
        let mut rest = &bytes[..];
        while let Ok(Some((_, used))) = split_frame(rest) {
            out.push(rest[..used].to_vec());
            rest = &rest[used..];
        }
        out
    }

    /// Whether to draw again; clears the flag.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Sets the clock (seconds) that times early lens views, and drops those past their wait.
    pub fn set_time(&mut self, now: f64) {
        self.now = now;
        self.retry_views(None);
    }

    /// Effects to perform, oldest first.
    pub fn take_effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.effects)
    }

    /// Lens views still waiting for their block.
    pub fn early_views(&self) -> usize {
        self.early_views.len()
    }

    /// Takes a new catalog: its Ready plugins' styles replace the installed ones.
    fn set_catalog(&mut self, catalog: serde_json::Value) {
        let styles: Vec<(String, String)> = if catalog.is_null() {
            Vec::new()
        } else {
            match serde_json::from_value::<auk_plugins::Catalog>(catalog.clone()) {
                Ok(c) => c
                    .styles()
                    .map(|(id, css)| (id.to_owned(), css.to_owned()))
                    .collect(),
                Err(err) => {
                    self.error(format!("bad catalog: {err}"));
                    Vec::new()
                }
            }
        };
        if styles != self.styles {
            self.styles = styles;
            self.styles_serial += 1;
        }
        self.catalog = catalog;
    }

    /// Applies early lens views whose block has appeared (only `pane`'s, when given) and drops
    /// those past their wait.
    fn retry_views(&mut self, pane: Option<PaneId>) {
        let now = self.now;
        let panes = &mut self.panes;
        self.early_views.retain(|(p, block, view, at)| {
            if pane.is_none_or(|want| want == *p)
                && let Some(state) = panes.get_mut(p)
                && state.replica.set_lens_view(*block, view.clone())
            {
                return false;
            }
            now - *at < EARLY_VIEW_SECS
        });
    }

    /// A view event from a pane's surface or command block (`lens:<block>`), sent as the
    /// daemon's view-event message.
    pub fn view_event(&mut self, pane: PaneId, sf: &str, event: serde_json::Value) {
        if self.status == Status::Attached && self.panes.contains_key(&pane) {
            self.send(&ClientMsg::ViewEvent {
                pane,
                sf: sf.to_owned(),
                event,
            });
        }
    }

    /// One WebSocket message: exactly one frame.
    pub fn on_message(&mut self, bytes: &[u8]) {
        match split_frame(bytes) {
            Ok(Some((Frame { kind, payload }, used))) if used == bytes.len() => {
                match DaemonMsg::decode(Frame { kind, payload }) {
                    Ok(msg) => self.on_msg(msg),
                    Err(err) => self.error(format!("bad frame: {err}")),
                }
            }
            Ok(_) => self.error(format!(
                "a message of {} bytes isn't one frame",
                bytes.len()
            )),
            Err(err) => self.error(format!("bad frame: {err}")),
        }
    }

    fn error(&mut self, err: String) {
        self.errors.push(err);
        self.dirty = true;
    }

    /// The connection closed.
    pub fn on_close(&mut self, reason: &str) {
        if !matches!(self.status, Status::Refused(_)) {
            self.status = Status::Closed(if reason.is_empty() {
                "the connection to the server closed".to_owned()
            } else {
                reason.to_owned()
            });
        }
        self.dirty = true;
    }

    fn on_msg(&mut self, msg: DaemonMsg) {
        self.dirty = true;
        match msg {
            DaemonMsg::Welcome {
                catalog, client, ..
            } => {
                self.client_id = Some(client);
                self.set_catalog(catalog);
                self.status = Status::Attached;
                if let Some(facts) = self.pending_facts.take() {
                    self.send(&ClientMsg::Facts(facts));
                }
                self.send(&ClientMsg::Attach {
                    window_key: self.window_key.clone(),
                });
            }
            DaemonMsg::Refused(refusal) => {
                self.status = Status::Refused(format!(
                    "{}. Upgrade this client to Auk {} (reload the page after `auk web serve` \
                     restarts from the daemon's build).",
                    refusal.reason, refusal.daemon.auk
                ));
            }
            // The browser client sends no requests.
            DaemonMsg::Reply { .. } => {}
            DaemonMsg::Snapshot { pane, snapshot } => {
                // The views' sheet checker, as a drawing window's replica has.
                let tsp = auk_tsp::TspPane::with(
                    auk_replica::tsp_config(&snapshot.settings),
                    auk_views::sheet_checker(),
                    Box::new(auk_tsp::MemoryBlobs::default()),
                );
                let replica = Replica::from_snapshot(&snapshot, Role::Follower, Some(tsp));
                let exit = snapshot.exit;
                let old = self.panes.remove(&pane);
                // The daemon sends the blobs the snapshot names right after it.
                let blobs_asked = replica.tsp().missing_blobs().into_iter().collect();
                let mut p = PaneState {
                    replica,
                    primary: old.as_ref().is_some_and(|p| p.primary),
                    exit,
                    asked: None,
                    acked: BTreeMap::new(),
                    changes: Vec::new(),
                    reset: true,
                    blobs_asked,
                };
                p.keep_changes();
                self.panes.insert(pane, p);
                self.retry_views(Some(pane));
            }
            DaemonMsg::Output { pane, seq, bytes } => self.apply(pane, seq, Item::Output(&bytes)),
            DaemonMsg::Record { pane, seq, record } => self.apply(pane, seq, Item::Record(&record)),
            DaemonMsg::Layout { window_key, window } => {
                if window_key == self.window_key {
                    let panes: BTreeSet<PaneId> = window.panes().into_iter().collect();
                    self.panes.retain(|id, _| panes.contains(id));
                    self.window = window;
                }
            }
            DaemonMsg::Catalog { catalog } => self.set_catalog(catalog),
            DaemonMsg::PaneExit { pane, status } => {
                if let Some(p) = self.panes.get_mut(&pane) {
                    p.exit = Some(status);
                }
            }
            DaemonMsg::Primary { pane, you } => {
                if let Some(p) = self.panes.get_mut(&pane) {
                    p.primary = you;
                }
            }
            DaemonMsg::Blob {
                pane,
                id,
                mime,
                bytes,
            } => {
                if let Some(p) = self.panes.get_mut(&pane) {
                    p.replica.deliver_blob(&id, mime.as_deref(), bytes);
                    p.keep_changes();
                }
            }
            DaemonMsg::LensView { pane, block, node } => {
                // A view replaces the block's last one whole; `null` shows the raw output.
                let Ok(block) = block.parse::<u64>() else {
                    self.error(format!("pane {pane}: lens view for block `{block}`"));
                    return;
                };
                let view = (!node.is_null()).then_some(node);
                let shown = self
                    .panes
                    .get_mut(&pane)
                    .is_some_and(|p| p.replica.set_lens_view(block, view.clone()));
                if !shown {
                    self.early_views
                        .retain(|(p, b, ..)| (*p, *b) != (pane, block));
                    self.early_views.push((pane, block, view, self.now));
                }
            }
            DaemonMsg::Effect(effect) => self.effects.push(effect),
            DaemonMsg::Presence { .. } => {}
        }
    }

    fn apply(&mut self, pane: PaneId, seq: u64, item: Item<'_>) {
        let Some(p) = self.panes.get_mut(&pane) else {
            self.errors
                .push(format!("pane {pane}: item {seq} before its snapshot"));
            return;
        };
        if let Err(err) = p.replica.apply(seq, item) {
            self.errors.push(format!("pane {pane}: {err}"));
        }
        p.keep_changes();
        let waiting = self.early_views.iter().any(|(id, ..)| *id == pane);
        // Blobs a document names that this client lacks.
        let ask: Vec<String> = p
            .replica
            .tsp()
            .missing_blobs()
            .into_iter()
            .filter(|id| p.blobs_asked.insert(id.clone()))
            .collect();
        if !ask.is_empty() {
            self.send(&ClientMsg::Blobs { pane, ids: ask });
        }
        if waiting {
            self.retry_views(Some(pane));
        }
    }

    /// A pane's undrawn TSP changes, and whether its surfaces start over (a snapshot replaced the
    /// replica since the last call).
    pub fn take_changes(&mut self, pane: PaneId) -> (bool, Vec<auk_tsp::Changes>) {
        match self.panes.get_mut(&pane) {
            Some(p) => (std::mem::take(&mut p.reset), std::mem::take(&mut p.changes)),
            None => (false, Vec::new()),
        }
    }

    /// The tab to show: the one holding `pane`, else the first session's first tab.
    pub fn tab(&self, pane: Option<PaneId>) -> Option<&Tab> {
        let tabs = || self.window.sessions.iter().flat_map(|s| s.tabs.iter());
        pane.and_then(|p| tabs().find(|t| t.root.contains(p)))
            .or_else(|| tabs().next())
    }

    /// The pane a click on a tab shows and focuses: its zoomed pane, else `focused` when the
    /// tab holds it, else its first pane.
    pub fn tab_pane(tab: &Tab, focused: Option<PaneId>) -> Option<PaneId> {
        tab.zoom
            .or(focused.filter(|p| tab.root.contains(*p)))
            .or_else(|| tab.root.panes().first().copied())
    }

    /// The pane to show for a session: `remembered` (the one last shown there) while the
    /// session still holds it, else its first tab's pane.
    pub fn session_pane(&self, session: SessionId, remembered: Option<PaneId>) -> Option<PaneId> {
        let s = self.window.sessions.iter().find(|s| s.id == session)?;
        remembered
            .filter(|p| s.tabs.iter().any(|t| t.root.contains(*p)))
            .or_else(|| s.tabs.first().and_then(|t| Self::tab_pane(t, None)))
    }

    /// A pane's title: the program's (OSC 0/2), else its cwd's last component, else `shell`,
    /// as the app titles tabs.
    fn pane_title(&self, pane: PaneId) -> Option<String> {
        let p = self.panes.get(&pane)?;
        let title = p.replica.terminal().title().trim().to_owned();
        if !title.is_empty() {
            return Some(title);
        }
        p.replica
            .cwd()
            .and_then(|c| {
                c.trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .map(str::to_owned)
            })
            .filter(|s| !s.is_empty())
    }

    /// The tab bar the shared chrome draws for the tab holding `pane` (else the
    /// first): every session of the window key (the one shown marked, for the session chip and
    /// its menu) and the shown session's tabs, each named by its own title, else its pane's, with
    /// the agent icon and the program's badge while a listening surface is open.
    /// The browser views and switches only: it creates nothing (no `+`). Sizes, dividers
    /// and the focus ring are the caller's.
    pub fn chrome_bar(&self, pane: Option<PaneId>, focused: Option<PaneId>) -> BarModel {
        let shown = self.tab(pane).map(|t| t.id);
        let shown_session = self
            .window
            .sessions
            .iter()
            .find(|s| s.tabs.iter().any(|t| Some(t.id) == shown));
        let sessions = self
            .window
            .sessions
            .iter()
            .map(|s| SessionItem {
                id: s.id,
                name: s.name.clone(),
                active: shown_session.is_some_and(|x| x.id == s.id),
                locked: s.locked,
                named: s.named,
                tabs: s.tabs.len(),
            })
            .collect();
        let tabs = shown_session
            .map(|s| {
                s.tabs
                    .iter()
                    .map(|t| {
                        let pane = Self::tab_pane(t, focused);
                        let title = t
                            .title
                            .clone()
                            .filter(|t| !t.is_empty())
                            .or_else(|| pane.and_then(|p| self.pane_title(p)))
                            .unwrap_or_else(|| "shell".to_owned());
                        TabItem {
                            id: t.id,
                            title,
                            active: Some(t.id) == shown,
                            zoomed: t.zoom.is_some(),
                            app: pane
                                .and_then(|p| self.panes.get(&p))
                                .and_then(|p| p.replica.tsp().app().map(str::to_owned)),
                            agent: pane.is_some_and(|p| {
                                self.window.kind(p) == auk_mirror::layout::PaneTag::Agent
                            }),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        BarModel {
            sessions,
            tabs,
            can_create: false,
            ..BarModel::default()
        }
    }

    /// Sends input bytes to a pane's PTY.
    pub fn input(&mut self, pane: PaneId, bytes: Vec<u8>) {
        if !bytes.is_empty() && self.status == Status::Attached {
            self.send(&ClientMsg::Input { pane, bytes });
        }
    }

    /// Encodes a key for the pane's current modes and sends it.
    pub fn key(&mut self, pane: PaneId, ev: &KeyEvent) {
        let Some(p) = self.panes.get(&pane) else {
            return;
        };
        let bytes = encode_key(ev, &p.replica.terminal().modes());
        self.input(pane, bytes);
    }

    /// Text an input method or dead key committed.
    pub fn text(&mut self, pane: PaneId, text: &str) {
        self.input(pane, text.as_bytes().to_vec());
    }

    /// A paste, bracketed when the program asked for it.
    pub fn paste(&mut self, pane: PaneId, text: &str) {
        let Some(p) = self.panes.get(&pane) else {
            return;
        };
        let bytes = encode_paste(text, &p.replica.terminal().modes());
        self.input(pane, bytes);
    }

    /// The size this client shows the pane at; sent when it changes.
    pub fn resize(&mut self, pane: PaneId, cols: u16, rows: u16) {
        let Some(p) = self.panes.get_mut(&pane) else {
            return;
        };
        if p.asked != Some((cols, rows)) {
            p.asked = Some((cols, rows));
            self.send(&ClientMsg::Resize { pane, cols, rows });
        }
    }

    /// Focuses a pane, which makes this client its primary.
    pub fn focus(&mut self, pane: PaneId) {
        if self.panes.contains_key(&pane) && self.status == Status::Attached {
            self.focused = Some(pane);
            self.send(&ClientMsg::Focus { pane });
        }
    }

    pub fn focused(&self) -> Option<PaneId> {
        self.focused
    }

    /// Updated client facts (appearance, cell size).
    pub fn facts(&mut self, facts: ClientFacts) {
        if self.status == Status::Attached {
            self.send(&ClientMsg::Facts(facts));
        } else {
            self.pending_facts = Some(facts);
        }
    }

    /// After a frame that drew `pane` was presented: acks each live listening surface's last
    /// frame, as the pane's primary only.
    pub fn drew(&mut self, pane: PaneId) {
        let Some(p) = self.panes.get_mut(&pane) else {
            return;
        };
        if !p.primary {
            return;
        }
        let mut acks = Vec::new();
        for s in p.replica.tsp().surfaces() {
            if s.listens()
                && s.is_live()
                && let Some(last) = s.last_s()
                && p.acked.get(s.id()) != Some(&last)
            {
                p.acked.insert(s.id().to_owned(), last);
                acks.push(ClientMsg::Ack {
                    pane,
                    sf: s.id().to_owned(),
                    s: last,
                });
            }
        }
        for ack in &acks {
            self.send(ack);
        }
    }
}

/// A rectangle in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Lays a tab's split tree out in `area`, `gap` pixels between panes; a zoomed pane fills it.
pub fn layout(tab: &Tab, area: Rect, gap: f32) -> Vec<(PaneId, Rect)> {
    let mut out = Vec::new();
    match tab.zoom {
        Some(pane) => out.push((pane, area)),
        None => split(&tab.root, area, gap, &mut out, &mut Vec::new()),
    }
    out
}

/// The gaps between a tab's split panes laid out as [`layout`] does: where the chrome draws its
/// dividers. None while a pane is zoomed.
pub fn dividers(tab: &Tab, area: Rect, gap: f32) -> Vec<Rect> {
    let mut lines = Vec::new();
    if tab.zoom.is_none() {
        split(&tab.root, area, gap, &mut Vec::new(), &mut lines);
    }
    lines
}

fn split(node: &Node, area: Rect, gap: f32, out: &mut Vec<(PaneId, Rect)>, lines: &mut Vec<Rect>) {
    match node {
        Node::Pane { pane } => out.push((*pane, area)),
        Node::Split {
            dir,
            ratio,
            first,
            second,
        } => {
            let r = f32::from(*ratio) / 1000.0;
            let (a, b) = match dir {
                auk_mirror::layout::SplitDir::Right => {
                    let w = ((area.w - gap) * r).floor().max(0.0);
                    lines.push(Rect {
                        x: area.x + w,
                        w: gap,
                        ..area
                    });
                    (
                        Rect { w, ..area },
                        Rect {
                            x: area.x + w + gap,
                            w: (area.w - w - gap).max(0.0),
                            ..area
                        },
                    )
                }
                auk_mirror::layout::SplitDir::Down => {
                    let h = ((area.h - gap) * r).floor().max(0.0);
                    lines.push(Rect {
                        y: area.y + h,
                        h: gap,
                        ..area
                    });
                    (
                        Rect { h, ..area },
                        Rect {
                            y: area.y + h + gap,
                            h: (area.h - h - gap).max(0.0),
                            ..area
                        },
                    )
                }
            };
            split(first, a, gap, out, lines);
            split(second, b, gap, out, lines);
        }
    }
}

/// Decodes every frame in `bytes` (a test helper for streams of several frames).
pub fn decode_all<M: Message>(mut bytes: &[u8]) -> Result<Vec<M>, WireError> {
    let mut out = Vec::new();
    while let Some((frame, used)) = split_frame(bytes)? {
        out.push(M::decode(frame)?);
        bytes = &bytes[used..];
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use auk_mirror::layout::{Layout, SplitDir};
    use auk_mirror::{Refusal, Versions};
    use auk_replica::Settings;

    fn welcome() -> Vec<u8> {
        DaemonMsg::Welcome {
            versions: Versions::current(),
            catalog: serde_json::json!({"plugins": [], "problems": [], "jit": false}),
            sessions: Layout::default(),
            client: 7,
        }
        .to_bytes()
    }

    /// Facts that change while the hello is out (the page's clock or look) reach the
    /// daemon once it welcomes the client; later changes go at once.
    #[test]
    fn facts_changed_before_the_welcome_are_sent_with_it() {
        let mut s = Session::new("main", ClientFacts::default());
        s.take_outbox();
        let twelve = ClientFacts {
            hour12: true,
            ..ClientFacts::default()
        };
        s.facts(twelve.clone());
        assert!(s.take_outbox().is_empty());
        s.on_message(&welcome());
        let out: Vec<ClientMsg> = decode_all(&s.take_outbox().concat()).unwrap();
        assert_eq!(
            out,
            vec![
                ClientMsg::Facts(twelve),
                ClientMsg::Attach {
                    window_key: "main".into()
                }
            ]
        );
        s.facts(ClientFacts::default());
        let out: Vec<ClientMsg> = decode_all(&s.take_outbox().concat()).unwrap();
        assert_eq!(out, vec![ClientMsg::Facts(ClientFacts::default())]);
    }

    #[test]
    fn hello_then_attach_then_replicas_follow_the_stream() {
        let mut s = Session::new("main", ClientFacts::default());
        let out = s.take_outbox();
        assert_eq!(out.len(), 1);
        match ClientMsg::decode(split_frame(&out[0]).unwrap().unwrap().0).unwrap() {
            ClientMsg::Hello(h) => {
                assert_eq!(h.kind, ClientKind::Web);
                assert_eq!(h.versions, Versions::current());
            }
            other => panic!("{other:?}"),
        }
        s.on_message(&welcome());
        assert_eq!(s.status, Status::Attached);
        assert_eq!(s.catalog["plugins"], serde_json::json!([]));
        let out: Vec<ClientMsg> = decode_all(&s.take_outbox().concat()).unwrap();
        assert_eq!(
            out,
            vec![ClientMsg::Attach {
                window_key: "main".into()
            }]
        );

        let mut layout = Layout::default();
        layout.new_session("main", None, 3);
        s.on_message(
            &DaemonMsg::Layout {
                window_key: "main".into(),
                window: layout.window("main").unwrap().clone(),
            }
            .to_bytes(),
        );
        let mut daemon = Replica::new(20, 5, Settings::default(), Role::Authoritative);
        daemon.push(Item::Output(b"hello"));
        s.on_message(
            &DaemonMsg::Snapshot {
                pane: 3,
                snapshot: daemon.snapshot(),
            }
            .to_bytes(),
        );
        let (seq, _) = daemon.push(Item::Output(b" world\r\n\x1b[?2004h"));
        s.on_message(
            &DaemonMsg::Output {
                pane: 3,
                seq,
                bytes: b" world\r\n\x1b[?2004h".to_vec(),
            }
            .to_bytes(),
        );
        assert!(s.errors.is_empty(), "{:?}", s.errors);
        let p = &s.panes[&3];
        assert_eq!(p.replica.state_hash(), daemon.state_hash());
        assert!(p.replica.screen_text().starts_with("hello world"));
        assert_eq!(s.tab(None).unwrap().root, Node::Pane { pane: 3 });

        // Paste follows the replica's bracketed-paste mode; resize is sent once per size.
        s.paste(3, "x");
        s.resize(3, 80, 24);
        s.resize(3, 80, 24);
        s.focus(3);
        let out: Vec<ClientMsg> = decode_all(&s.take_outbox().concat()).unwrap();
        assert_eq!(
            out,
            vec![
                ClientMsg::Input {
                    pane: 3,
                    bytes: b"\x1b[200~x\x1b[201~".to_vec()
                },
                ClientMsg::Resize {
                    pane: 3,
                    cols: 80,
                    rows: 24
                },
                ClientMsg::Focus { pane: 3 },
            ]
        );
    }

    #[test]
    fn a_refusal_is_reported_and_stays() {
        let mut s = Session::new("main", ClientFacts::default());
        s.on_message(
            &DaemonMsg::Refused(Refusal {
                reason: "parser x differs from the daemon's y".into(),
                daemon: Versions::current(),
                upgrading: false,
            })
            .to_bytes(),
        );
        s.on_close("");
        match &s.status {
            Status::Refused(msg) => assert!(msg.contains("parser"), "{msg}"),
            other => panic!("{other:?}"),
        }
        s.input(1, b"x".to_vec());
        assert!(s.take_outbox().len() == 1, "only the hello");
    }

    fn attached() -> Session {
        let mut s = Session::new("main", ClientFacts::default());
        s.on_message(&welcome());
        s.take_outbox();
        s
    }

    fn plugin(id: &str, css: &str, status: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "id": id, "name": id, "version": "1", "description": "", "icon": "plug",
            "css": css, "blocks": [], "lenses": [], "host": true, "window": false,
            "status": status,
        })
    }

    #[test]
    fn the_catalog_brings_the_ready_plugins_styles() {
        let mut s = Session::new("main", ClientFacts::default());
        let catalog = serde_json::json!({"plugins": [
            plugin("a", ".a{}", serde_json::json!("ready")),
            plugin("b", ".b{}", serde_json::json!("disabled")),
            plugin("c", ".c{}", serde_json::json!({"failed": "boom"})),
            plugin("d", "", serde_json::json!("ready")),
        ], "problems": [], "jit": false});
        s.on_message(
            &DaemonMsg::Welcome {
                versions: Versions::current(),
                catalog: catalog.clone(),
                sessions: Layout::default(),
                client: 1,
            }
            .to_bytes(),
        );
        assert_eq!(s.styles, vec![("a".to_owned(), ".a{}".to_owned())]);
        let serial = s.styles_serial;
        // The same styles again change nothing; a reload's new catalog replaces them.
        s.on_message(&DaemonMsg::Catalog { catalog }.to_bytes());
        assert_eq!(s.styles_serial, serial);
        let reloaded = serde_json::json!({"plugins": [
            plugin("a", ".a{color:red}", serde_json::json!("ready")),
            plugin("b", ".b{}", serde_json::json!("ready")),
        ], "problems": [], "jit": false});
        s.on_message(&DaemonMsg::Catalog { catalog: reloaded }.to_bytes());
        assert!(s.styles_serial > serial);
        assert_eq!(
            s.styles,
            vec![
                ("a".to_owned(), ".a{color:red}".to_owned()),
                ("b".to_owned(), ".b{}".to_owned())
            ]
        );
        assert!(s.errors.is_empty(), "{:?}", s.errors);
    }

    #[test]
    fn effects_queue_for_the_page_in_order() {
        let mut s = attached();
        let toast = Effect::Toast {
            level: "info".into(),
            text: "Saved".into(),
            sub: Some("plan.tf".into()),
        };
        let copy = Effect::Copy {
            text: "aws_instance.old".into(),
        };
        s.on_message(&DaemonMsg::Effect(toast.clone()).to_bytes());
        s.on_message(&DaemonMsg::Effect(copy.clone()).to_bytes());
        assert_eq!(s.take_effects(), vec![toast, copy]);
        assert!(s.take_effects().is_empty());
    }

    /// A replica of a pane that ran a command `tf` claims, with the claim catalog in its stream.
    fn lensed_daemon() -> Replica {
        use auk_plugins::catalog::{LensRule, PluginEntry};
        let catalog = auk_plugins::Catalog {
            plugins: vec![PluginEntry {
                id: "tf".into(),
                name: "TF".into(),
                version: "1".into(),
                description: String::new(),
                icon: "stack".into(),
                css: String::new(),
                blocks: Vec::new(),
                lenses: vec![LensRule {
                    id: "plan".into(),
                    patterns: vec!["tf".into()],
                }],
                host: true,
                window: false,
                status: auk_plugins::Status::Ready,
            }],
            problems: Vec::new(),
            jit: false,
        };
        let mut daemon = Replica::new(40, 10, Settings::default(), Role::Authoritative);
        let record = auk_replica::Record::Catalog {
            catalog: auk_replica::claim_catalog(&catalog),
        };
        daemon.push(Item::Record(&record));
        daemon
    }

    #[test]
    fn lens_views_reach_the_replica_and_early_ones_wait_up_to_5_s() {
        let mut s = attached();
        let mut daemon = lensed_daemon();
        s.on_message(
            &DaemonMsg::Snapshot {
                pane: 2,
                snapshot: daemon.snapshot(),
            }
            .to_bytes(),
        );
        let view = serde_json::json!({"k": "text", "p": {"text": "1 to add"}});
        let lens_view = |node: serde_json::Value| {
            DaemonMsg::LensView {
                pane: 2,
                block: "0".into(),
                node,
            }
            .to_bytes()
        };
        // The view outruns the command's output: it waits for the block.
        s.set_time(100.0);
        s.on_message(&lens_view(view.clone()));
        assert_eq!(s.early_views(), 1);
        for bytes in [
            &b"$ tf\r\n\x1b]133;C;cmdline_url=tf\x07"[..],
            b"out\r\n\x1b]133;D;0\x07",
        ] {
            let (seq, _) = daemon.push(Item::Output(bytes));
            s.on_message(
                &DaemonMsg::Output {
                    pane: 2,
                    seq,
                    bytes: bytes.to_vec(),
                }
                .to_bytes(),
            );
        }
        assert!(s.errors.is_empty(), "{:?}", s.errors);
        assert_eq!(s.early_views(), 0);
        let blocks = s.panes[&2].replica.lens_blocks();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].view.as_ref(), Some(&view));
        // `null` shows the raw output again.
        s.on_message(&lens_view(serde_json::Value::Null));
        assert_eq!(s.panes[&2].replica.lens_blocks()[0].view, None);

        // A view for a block that never comes is dropped after 5 s.
        s.on_message(
            &DaemonMsg::LensView {
                pane: 2,
                block: "9".into(),
                node: view,
            }
            .to_bytes(),
        );
        s.set_time(104.0);
        assert_eq!(s.early_views(), 1);
        s.set_time(105.5);
        assert_eq!(s.early_views(), 0);
    }

    #[test]
    fn view_events_go_to_the_daemon_for_known_panes() {
        let mut s = attached();
        let daemon = Replica::new(20, 5, Settings::default(), Role::Authoritative);
        s.on_message(
            &DaemonMsg::Snapshot {
                pane: 4,
                snapshot: daemon.snapshot(),
            }
            .to_bytes(),
        );
        let ev = serde_json::json!({"ev": "action", "id": "go", "act": "deploy"});
        s.view_event(4, "lens:0", ev.clone());
        s.view_event(5, "s1", ev.clone());
        let out: Vec<ClientMsg> = decode_all(&s.take_outbox().concat()).unwrap();
        assert_eq!(
            out,
            vec![ClientMsg::ViewEvent {
                pane: 4,
                sf: "lens:0".into(),
                event: ev
            }]
        );
    }

    /// The browser's tab bar is the shared chrome's model: the shown session's tabs (the
    /// one holding the pane active), every session for the chip and its menu, and no `+`.
    #[test]
    fn the_chrome_bar_shows_the_shown_sessions_tabs_and_every_session() {
        let mut s = attached();
        let mut l = Layout::default();
        let session = l.new_session("main", None, 1);
        l.new_tab(session, 2).unwrap();
        l.rename_tab(
            l.window("main").unwrap().sessions[0].tabs[1].id,
            Some("logs"),
        )
        .unwrap();
        let other = l.new_session("main", Some("ops"), 3);
        // Pane 2 is an agent pane: its tab shows the agent icon.
        assert!(l.set_kind(2, auk_mirror::layout::PaneTag::Agent));
        s.on_message(
            &DaemonMsg::Layout {
                window_key: "main".into(),
                window: l.window("main").unwrap().clone(),
            }
            .to_bytes(),
        );
        let bar = s.chrome_bar(Some(2), None);
        assert!(!bar.can_create);
        assert_eq!(bar.tabs.len(), 2, "{bar:?}");
        assert_eq!((bar.tabs[0].agent, bar.tabs[1].agent), (false, true));
        assert_eq!(
            (bar.tabs[0].active, bar.tabs[0].title.as_str()),
            (false, "shell")
        );
        assert_eq!(
            (bar.tabs[1].title.as_str(), bar.tabs[1].active),
            ("logs", true)
        );
        let names: Vec<(&str, bool, usize)> = bar
            .sessions
            .iter()
            .map(|s| (s.name.as_str(), s.active, s.tabs))
            .collect();
        assert_eq!(names[1], ("ops", false, 1));
        assert!(names[0].1 && names[0].2 == 2, "{names:?}");
        // The other session: its own tab only.
        let bar = s.chrome_bar(Some(3), None);
        assert_eq!(bar.tabs.len(), 1);
        assert!(bar.sessions[1].active && bar.sessions[1].named);
        assert_eq!(s.session_pane(other, None), Some(3));
        assert_eq!(
            s.session_pane(session, Some(2)),
            Some(2),
            "the remembered pane"
        );
        assert_eq!(
            s.session_pane(session, Some(3)),
            Some(1),
            "not in the session"
        );
        assert!(
            s.chrome_bar(None, None).tabs[0].active,
            "the first tab by default"
        );
    }

    #[test]
    fn splits_lay_out_by_ratio() {
        let mut l = Layout::default();
        l.new_session("w", None, 1);
        l.split(1, SplitDir::Right, 2).unwrap();
        l.split(2, SplitDir::Down, 3).unwrap();
        let tab = &l.window("w").unwrap().sessions[0].tabs[0];
        let rects = layout(
            tab,
            Rect {
                x: 0.0,
                y: 0.0,
                w: 1002.0,
                h: 602.0,
            },
            2.0,
        );
        assert_eq!(rects.len(), 3);
        assert_eq!(rects[0].1.w, 500.0);
        assert_eq!(rects[1].1.x, 502.0);
        assert_eq!(rects[1].1.h, 300.0);
        assert_eq!(rects[2].1.y, 302.0);
        let lines = dividers(
            tab,
            Rect {
                x: 0.0,
                y: 0.0,
                w: 1002.0,
                h: 602.0,
            },
            2.0,
        );
        assert_eq!(
            lines,
            [
                Rect {
                    x: 500.0,
                    y: 0.0,
                    w: 2.0,
                    h: 602.0
                },
                Rect {
                    x: 502.0,
                    y: 300.0,
                    w: 500.0,
                    h: 2.0
                },
            ]
        );
    }
}
