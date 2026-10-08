//! The browser client's connection state (§15): the handshake, a follower replica per pane of
//! the attached window key, the layout, and what the client sends. Pure, so it runs and is
//! tested natively; the wasm glue feeds it WebSocket messages and drains its outbox.

use std::collections::{BTreeMap, BTreeSet};

use auk_mirror::layout::{Node, Tab, Window};
use auk_mirror::{
    ClientFacts, ClientKind, ClientMsg, DaemonMsg, Frame, Hello, Message, PaneId, WireError,
    frame::split_frame,
};
use auk_replica::{Item, Replica, Role};
use auk_term::input::{KeyEvent, encode_key, encode_paste};

/// Where the connection is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// The hello is out; no answer yet.
    Connecting,
    /// Welcomed and attached to the window key.
    Attached,
    /// The daemon runs another version (X9): reload after upgrading.
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
    /// Whether this client is the pane's primary (§15.5).
    pub primary: bool,
    /// The process's exit status, once it ended.
    pub exit: Option<i32>,
    /// The size last asked for.
    pub asked: Option<(u16, u16)>,
    /// The last frame acked per surface.
    pub acked: BTreeMap<String, u64>,
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
    /// The plugin catalog (§9.4): the web client runs no Lua and learns block types, lens claims
    /// and plugin styles from it (§10.3).
    pub catalog: serde_json::Value,
    /// Protocol errors: items out of order, undecodable frames. A correct daemon causes none.
    pub errors: Vec<String>,
    /// Something visible changed since the last [`Session::take_dirty`].
    dirty: bool,
    outbox: Vec<u8>,
    /// Panes whose primary status we asked for by focusing.
    focused: Option<PaneId>,
}

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
            errors: Vec::new(),
            dirty: true,
            outbox: Vec::new(),
            focused: None,
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
                self.catalog = catalog;
                self.status = Status::Attached;
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
            DaemonMsg::Snapshot { pane, snapshot } => {
                let replica = Replica::from_snapshot(&snapshot, Role::Follower, None);
                let exit = snapshot.exit;
                let old = self.panes.remove(&pane);
                self.panes.insert(
                    pane,
                    PaneState {
                        replica,
                        primary: old.as_ref().is_some_and(|p| p.primary),
                        exit,
                        asked: None,
                        acked: BTreeMap::new(),
                    },
                );
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
            DaemonMsg::Catalog { catalog } => self.catalog = catalog,
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
            DaemonMsg::LensView { .. } | DaemonMsg::Effect(_) | DaemonMsg::Presence { .. } => {}
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
        // The browser draws the grid; surface changes are drained so they don't pile up.
        let _ = p.replica.take_tsp_changes();
    }

    /// The tab to show: the one holding `pane`, else the first session's first tab.
    pub fn tab(&self, pane: Option<PaneId>) -> Option<&Tab> {
        let tabs = || self.window.sessions.iter().flat_map(|s| s.tabs.iter());
        pane.and_then(|p| tabs().find(|t| t.root.contains(p)))
            .or_else(|| tabs().next())
    }

    /// Sends input bytes to a pane's PTY.
    pub fn input(&mut self, pane: PaneId, bytes: Vec<u8>) {
        if !bytes.is_empty() && self.status == Status::Attached {
            self.send(&ClientMsg::Input { pane, bytes });
        }
    }

    /// Encodes a key for the pane's current modes and sends it (§15.4).
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

    /// Focuses a pane, which makes this client its primary (§15.5).
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
        }
    }

    /// After a frame that drew `pane` was presented: acks each live listening surface's last
    /// frame, as the pane's primary only (§7.10, §15.5).
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
        None => split(&tab.root, area, gap, &mut out),
    }
    out
}

fn split(node: &Node, area: Rect, gap: f32, out: &mut Vec<(PaneId, Rect)>) {
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
            split(first, a, gap, out);
            split(second, b, gap, out);
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
            catalog: serde_json::json!({"plugins": []}),
            sessions: Layout::default(),
            client: 7,
        }
        .to_bytes()
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
    }
}
