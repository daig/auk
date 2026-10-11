//! A window's mirror connection over a local socket or an SSH link.
//!
//! [`Link::connect`] opens the transport and says hello; it returns the sending half ([`Link`],
//! cheap to clone and shared by threads) and the receiving half ([`LinkReader`]), which a
//! window's reader thread drains as it would a bare [`auk_mirror::Client`].
//!
//! Over SSH the link survives drops. When the stream ends, the reader reruns `ssh` with a
//! backoff, says hello again with the window's latest facts, reattaches the window key and
//! hands on what the daemon sends: the layout, then a fresh snapshot of every pane,
//! which replaces the window's replica. Until every pane's snapshot has arrived, typing and
//! other user input is held, then sent in order; acks are dropped, since the window
//! redraws and acks what the snapshots bring.
//!
//! A local link reconnects the same way when its daemon was upgraded in place: the new
//! daemon keeps the old one's listening socket, so connecting again succeeds. A local daemon
//! that went away refuses the connection, and the link fails at once. A daemon upgrading itself
//! to this client's release refuses the hello with `upgrading` set; the link says hello again
//! until the new daemon welcomes it.
//!
//! Connection states: `connecting` (opening, or attaching for the first time),
//! `connected`, `stalled` (input sent and nothing heard for [`STALL_AFTER`]), `reconnecting(n)`
//! (attempt n, through the resync) and `failed`. Tern's `verify` state for unknown host keys
//! is SSH's own prompt here.

use std::collections::{BTreeSet, VecDeque};
use std::fmt;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::Child;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::thread;
use std::time::{Duration, Instant};

use auk_mirror::{
    Client, ClientMsg, DaemonMsg, HandshakeError, Hello, PaneId, Welcome, WireError, read_msg,
    write_msg,
};
use serde::Serialize;

use crate::bridge::BridgeInfo;
use crate::identity::Hop;
use crate::ssh::{OpenError, SshTarget};

/// Input sent with nothing heard back for this long shows the link as stalled.
pub const STALL_AFTER: Duration = Duration::from_secs(3);
/// Reconnect attempts before the link fails.
pub const MAX_ATTEMPTS: u32 = 10;
/// How long a resync waits for the snapshots of the panes in the layout.
pub const RESYNC_TIMEOUT: Duration = Duration::from_secs(10);
/// The most input bytes held while the link is down.
pub const MAX_HELD_BYTES: usize = 1 << 20;

/// The delay before reconnect attempt `n` (from 1): 250 ms, doubling, at most 5 s.
pub fn backoff(attempt: u32) -> Duration {
    let ms = 250u64.saturating_mul(1 << attempt.saturating_sub(1).min(5));
    Duration::from_millis(ms.min(5_000))
}

/// Where a link goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// The daemon socket on this machine.
    Local(PathBuf),
    /// A remote host's daemon, through `ssh <host> auk attach --stdio`.
    Ssh(SshTarget),
}

/// A link's connection state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum State {
    Connecting,
    Connected,
    Stalled,
    Reconnecting { attempt: u32 },
    Failed { reason: String },
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            State::Connecting => f.write_str("connecting"),
            State::Connected => f.write_str("connected"),
            State::Stalled => f.write_str("stalled"),
            State::Reconnecting { attempt } => write!(f, "reconnecting({attempt})"),
            State::Failed { reason } => write!(f, "failed: {reason}"),
        }
    }
}

/// What [`Link::send`] did with a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    /// Written to the connection.
    Sent,
    /// Held until the link has resynced.
    Held,
    /// Not needed after a reconnect (an ack, or facts the next hello carries).
    Dropped,
}

/// A snapshot of a link for display and tests.
#[derive(Clone, Debug, Serialize)]
pub struct Status {
    pub state: State,
    /// `connected`, `reconnecting(2)`, …
    pub label: String,
    /// Recent states, oldest first.
    pub history: Vec<String>,
    /// The host's identity chain.
    pub identity: Vec<Hop>,
    /// What the remote bridge said about the host.
    pub host: Option<BridgeInfo>,
    /// The running `ssh` process.
    pub ssh_pid: Option<u32>,
    /// Successful reconnects.
    pub reconnects: u32,
    /// Messages held until the resync completes.
    pub held: usize,
    /// This connection's client id on the daemon.
    pub client: u64,
}

/// Why a link couldn't connect, or ended.
#[derive(Debug)]
pub enum LinkError {
    /// The transport didn't open.
    Open(String),
    /// The daemon refused the hello, or answered out of turn.
    Handshake(HandshakeError),
    /// The local daemon closed the connection, or the stream broke.
    Closed(String),
    /// Reconnecting gave up.
    Failed(String),
}

impl fmt::Display for LinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LinkError::Open(reason) => write!(f, "couldn't connect: {reason}"),
            LinkError::Handshake(err) => write!(f, "{err}"),
            LinkError::Closed(reason) => write!(f, "{reason}"),
            LinkError::Failed(reason) => write!(f, "the link failed: {reason}"),
        }
    }
}

impl std::error::Error for LinkError {}

impl From<LinkError> for io::Error {
    fn from(err: LinkError) -> Self {
        match err {
            LinkError::Closed(reason) => io::Error::new(io::ErrorKind::ConnectionAborted, reason),
            other => io::Error::other(other.to_string()),
        }
    }
}

type Reader = Box<dyn Read + Send>;
type Writer = Box<dyn Write + Send>;

/// An open transport.
struct Opened {
    reader: Reader,
    writer: Writer,
    child: Option<Child>,
    hop: Hop,
    bridge: Option<BridgeInfo>,
}

fn open(target: &Target) -> Result<Opened, OpenError> {
    match target {
        Target::Local(socket) => {
            let stream = UnixStream::connect(socket)
                .map_err(|e| OpenError::Fatal(format!("{}: {e}", socket.display())))?;
            let reader = stream
                .try_clone()
                .map_err(|e| OpenError::Fatal(e.to_string()))?;
            Ok(Opened {
                reader: Box::new(reader),
                writer: Box::new(stream),
                child: None,
                hop: Hop::local(),
                bridge: None,
            })
        }
        Target::Ssh(target) => {
            let session = target.open()?;
            let hop = session.hop();
            Ok(Opened {
                reader: Box::new(session.stdout),
                writer: Box::new(session.stdin),
                child: Some(session.child),
                hop,
                bridge: Some(session.bridge),
            })
        }
    }
}

/// A reader and a writer as one stream, for the handshake.
struct Duplex {
    reader: Reader,
    writer: Writer,
}

impl Read for Duplex {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.reader.read(buf)
    }
}

impl Write for Duplex {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.writer.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

/// How long a client waits for a daemon that is upgrading itself to this client's release.
const UPGRADE_WAIT: Duration = Duration::from_secs(20);

/// Opens the transport and says hello. A daemon that answers that it is upgrading to this
/// client's release is asked again until the new one welcomes the client.
fn greet(target: &Target, hello: &Hello) -> Result<(Opened, Welcome), LinkError> {
    let deadline = Instant::now() + UPGRADE_WAIT;
    loop {
        let opened = open(target).map_err(|e| LinkError::Open(e.to_string()))?;
        match handshake(opened, hello) {
            Err(HandshakeError::Refused(r)) if r.upgrading && Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(100));
            }
            result => return result.map_err(LinkError::Handshake),
        }
    }
}

fn handshake(opened: Opened, hello: &Hello) -> Result<(Opened, Welcome), HandshakeError> {
    let Opened {
        reader,
        writer,
        child,
        hop,
        bridge,
    } = opened;
    // The daemon shows this link's hop to every client attached to the host (presence).
    let mut hello = hello.clone();
    hello.identity = vec![hop.clone().into()];
    let (client, welcome) = Client::handshake(Duplex { reader, writer }, &hello)?;
    let Duplex { reader, writer } = client.into_inner();
    Ok((
        Opened {
            reader,
            writer,
            child,
            hop,
            bridge,
        },
        welcome,
    ))
}

/// Where the resync after an attach stands.
#[derive(Debug)]
enum Sync {
    Done,
    /// Waiting for the attached window key's layout.
    Layout {
        deadline: Instant,
    },
    /// Waiting for these panes' snapshots.
    Snapshots {
        pending: BTreeSet<PaneId>,
        deadline: Instant,
    },
}

struct Shared {
    state: State,
    history: VecDeque<String>,
    hello: Hello,
    window_key: Option<String>,
    detached: bool,
    writer: Option<Writer>,
    sync: Sync,
    held: Vec<ClientMsg>,
    held_bytes: usize,
    identity: Vec<Hop>,
    bridge: Option<BridgeInfo>,
    ssh_pid: Option<u32>,
    reconnects: u32,
    client: u64,
    /// When the oldest input still unanswered was sent.
    awaiting: Option<Instant>,
    /// Told after every state change, so a window can redraw its connection state.
    on_change: Option<Arc<dyn Fn() + Send + std::marker::Sync>>,
}

impl Shared {
    fn set_state(&mut self, state: State) {
        if self.state == state {
            return;
        }
        self.history.push_back(state.to_string());
        if self.history.len() > 64 {
            self.history.pop_front();
        }
        self.state = state;
        if let Some(f) = &self.on_change {
            f();
        }
    }

    fn synced(&self) -> bool {
        matches!(self.sync, Sync::Done)
    }

    /// Writes a message; on failure the connection is gone and the reader will notice.
    fn write(&mut self, msg: &ClientMsg) -> bool {
        let Some(writer) = self.writer.as_mut() else {
            return false;
        };
        if write_msg(writer, msg).is_ok() {
            if expects_answer(msg) && self.awaiting.is_none() {
                self.awaiting = Some(Instant::now());
            }
            true
        } else {
            self.writer = None;
            false
        }
    }

    fn hold(&mut self, msg: &ClientMsg) -> io::Result<Delivery> {
        let size = match msg {
            ClientMsg::Input { bytes, .. } => bytes.len(),
            _ => 64,
        };
        if self.held_bytes + size > MAX_HELD_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!(
                    "the link is {} and {} bytes of input are already held",
                    self.state, self.held_bytes
                ),
            ));
        }
        self.held_bytes += size;
        self.held.push(msg.clone());
        Ok(Delivery::Held)
    }

    /// The resync is complete: the link is connected, and held input goes out in order.
    fn finish_sync(&mut self) {
        self.sync = Sync::Done;
        self.set_state(State::Connected);
        let held = std::mem::take(&mut self.held);
        self.held_bytes = 0;
        let mut rest = held.into_iter();
        for msg in rest.by_ref() {
            if !self.write(&msg) {
                self.held.push(msg);
                break;
            }
        }
        self.held.extend(rest);
    }

    /// Starts the resync an attach begins.
    fn start_sync(&mut self) {
        self.sync = Sync::Layout {
            deadline: Instant::now() + RESYNC_TIMEOUT,
        };
    }
}

/// Messages whose effect the user waits to see.
fn expects_answer(msg: &ClientMsg) -> bool {
    matches!(
        msg,
        ClientMsg::Input { .. }
            | ClientMsg::ViewEvent { .. }
            | ClientMsg::LayoutEdit(_)
            | ClientMsg::Request { .. }
    )
}

struct Inner {
    target: Target,
    shared: Mutex<Shared>,
}

/// The sending half of a link; clones share it.
#[derive(Clone)]
pub struct Link {
    inner: Arc<Inner>,
}

impl fmt::Debug for Link {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Link")
            .field("target", &self.inner.target)
            .field("state", &self.lock().state)
            .finish()
    }
}

impl Link {
    /// Opens the transport and says hello. Nothing is attached yet: send
    /// [`ClientMsg::Attach`], then drain the [`LinkReader`].
    pub fn connect(target: Target, hello: Hello) -> Result<(Link, Welcome, LinkReader), LinkError> {
        let (opened, welcome) = greet(&target, &hello)?;
        let Opened {
            reader,
            writer,
            child,
            hop,
            bridge,
        } = opened;
        let mut shared = Shared {
            state: State::Connecting,
            history: VecDeque::from(["connecting".to_owned()]),
            hello,
            window_key: None,
            detached: false,
            writer: Some(writer),
            sync: Sync::Done,
            held: Vec::new(),
            held_bytes: 0,
            identity: vec![hop],
            bridge,
            ssh_pid: child.as_ref().map(Child::id),
            reconnects: 0,
            client: welcome.client,
            awaiting: None,
            on_change: None,
        };
        shared.set_state(State::Connected);
        let link = Link {
            inner: Arc::new(Inner {
                target,
                shared: Mutex::new(shared),
            }),
        };
        let weak = Arc::downgrade(&link.inner);
        thread::Builder::new()
            .name("auk link watchdog".into())
            .spawn(move || watchdog(weak))
            .map_err(|e| LinkError::Open(e.to_string()))?;
        let reader = LinkReader {
            link: link.clone(),
            reader,
            child,
            buf: Vec::new(),
        };
        Ok((link, welcome, reader))
    }

    fn lock(&self) -> MutexGuard<'_, Shared> {
        self.inner.shared.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Sends a message, or holds it while the link is down or resyncing.
    pub fn send(&self, msg: &ClientMsg) -> io::Result<Delivery> {
        let mut st = self.lock();
        if matches!(st.state, State::Failed { .. }) {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                st.state.to_string(),
            ));
        }
        match msg {
            ClientMsg::Hello(_) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the link says hello itself",
            )),
            ClientMsg::Attach { window_key } => {
                st.window_key = Some(window_key.clone());
                st.start_sync();
                if st.state == State::Connected {
                    st.set_state(State::Connecting);
                }
                // Sent now, or by the next reconnect.
                Ok(if st.write(msg) {
                    Delivery::Sent
                } else {
                    Delivery::Held
                })
            }
            ClientMsg::Facts(facts) => {
                st.hello.facts = facts.clone();
                Ok(if st.write(msg) {
                    Delivery::Sent
                } else {
                    Delivery::Dropped
                })
            }
            ClientMsg::Ack { .. } => Ok(if st.synced() && st.write(msg) {
                Delivery::Sent
            } else {
                Delivery::Dropped
            }),
            ClientMsg::Detach => {
                st.detached = true;
                st.write(msg);
                Ok(Delivery::Sent)
            }
            _ => {
                if st.synced() && st.write(msg) {
                    Ok(Delivery::Sent)
                } else {
                    st.hold(msg)
                }
            }
        }
    }

    pub fn state(&self) -> State {
        self.lock().state.clone()
    }

    /// Runs `f` after every change of [`Link::state`] (from whichever thread made it, with the
    /// link locked: `f` must not call back into the link). A window wakes itself with it.
    pub fn on_change(&self, f: impl Fn() + Send + std::marker::Sync + 'static) {
        self.lock().on_change = Some(Arc::new(f));
    }

    pub fn identity(&self) -> Vec<Hop> {
        self.lock().identity.clone()
    }

    pub fn status(&self) -> Status {
        let st = self.lock();
        Status {
            state: st.state.clone(),
            label: st.state.to_string(),
            history: st.history.iter().cloned().collect(),
            identity: st.identity.clone(),
            host: st.bridge.clone(),
            ssh_pid: st.ssh_pid,
            reconnects: st.reconnects,
            held: st.held.len(),
            client: st.client,
        }
    }

    /// Notes a message from the daemon: it ends a stall and advances a resync.
    fn received(&self, msg: &DaemonMsg) {
        let mut st = self.lock();
        st.awaiting = None;
        if st.state == State::Stalled {
            st.set_state(State::Connected);
        }
        let window_key = st.window_key.clone();
        let finished = match (&mut st.sync, msg) {
            (
                Sync::Layout { deadline },
                DaemonMsg::Layout {
                    window_key: key,
                    window,
                },
            ) if Some(key) == window_key.as_ref() => {
                let pending: BTreeSet<PaneId> = window.panes().into_iter().collect();
                let empty = pending.is_empty();
                let deadline = *deadline;
                st.sync = Sync::Snapshots { pending, deadline };
                empty
            }
            (Sync::Snapshots { pending, .. }, DaemonMsg::Snapshot { pane, .. }) => {
                pending.remove(pane);
                pending.is_empty()
            }
            _ => false,
        };
        if finished {
            st.finish_sync();
        }
    }

    /// The stream ended; returns whether the link should reconnect.
    fn lost(&self) -> bool {
        let mut st = self.lock();
        st.writer = None;
        st.ssh_pid = None;
        if st.detached {
            return false;
        }
        // A local daemon that still accepts connections was upgraded in place;
        // reconnecting to one that went away fails at once.
        true
    }

    fn fail(&self, reason: String) -> LinkError {
        self.lock().set_state(State::Failed {
            reason: reason.clone(),
        });
        LinkError::Failed(reason)
    }
}

/// Times out resyncs and notices stalls, until the link is dropped.
fn watchdog(inner: Weak<Inner>) {
    loop {
        thread::sleep(Duration::from_millis(100));
        let Some(inner) = inner.upgrade() else { return };
        let mut st = inner.shared.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        let overdue = match &st.sync {
            Sync::Layout { deadline } | Sync::Snapshots { deadline, .. } => {
                *deadline <= now && st.writer.is_some()
            }
            Sync::Done => false,
        };
        if overdue {
            // Some pane in the layout sent no snapshot; don't hold typing forever.
            st.finish_sync();
        }
        if st.state == State::Connected && st.awaiting.is_some_and(|t| now - t >= STALL_AFTER) {
            st.set_state(State::Stalled);
        }
    }
}

/// The receiving half of a link: one thread drains it.
pub struct LinkReader {
    link: Link,
    reader: Reader,
    child: Option<Child>,
    buf: Vec<u8>,
}

impl fmt::Debug for LinkReader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LinkReader")
            .field("link", &self.link)
            .finish()
    }
}

impl LinkReader {
    /// The next message from the daemon, reconnecting an SSH link as needed. `Ok(None)` after
    /// this window detached; an error once a local daemon went away or reconnecting gave up.
    pub fn recv(&mut self) -> Result<Option<DaemonMsg>, LinkError> {
        loop {
            let reason = match read_msg::<DaemonMsg>(&mut self.reader, &mut self.buf) {
                Ok(Some(msg)) => {
                    self.link.received(&msg);
                    return Ok(Some(msg));
                }
                Ok(None) => "the daemon closed the connection".to_owned(),
                Err(WireError::Io(err)) => format!("the connection broke: {err}"),
                Err(err) => format!("the daemon sent a bad frame: {err}"),
            };
            self.reap();
            if !self.link.lost() {
                return if self.link.lock().detached {
                    Ok(None)
                } else {
                    Err(LinkError::Closed(reason))
                };
            }
            self.reconnect(reason)?;
        }
    }

    pub fn link(&self) -> &Link {
        &self.link
    }

    fn reap(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// Reconnects with backoff, says hello and reattaches; the resync then completes as the
    /// snapshots arrive through [`LinkReader::recv`].
    fn reconnect(&mut self, mut last: String) -> Result<(), LinkError> {
        for attempt in 1..=MAX_ATTEMPTS {
            self.link.lock().set_state(State::Reconnecting { attempt });
            thread::sleep(backoff(attempt));
            if self.link.lock().detached {
                return Err(self.link.fail("the window detached".into()));
            }
            let opened = match open(&self.link.inner.target) {
                Ok(opened) => opened,
                Err(OpenError::Retry(reason)) => {
                    last = reason;
                    continue;
                }
                Err(OpenError::Fatal(reason)) => return Err(self.link.fail(reason)),
            };
            let hello = self.link.lock().hello.clone();
            let (opened, welcome) = match handshake(opened, &hello) {
                Ok(done) => done,
                Err(HandshakeError::Refused(r)) if r.upgrading => {
                    last = r.reason;
                    continue;
                }
                Err(err @ HandshakeError::Refused(_)) => {
                    return Err(self.link.fail(err.to_string()));
                }
                Err(err) => {
                    last = err.to_string();
                    continue;
                }
            };
            let Opened {
                reader,
                writer,
                child,
                hop,
                bridge,
            } = opened;
            let mut st = self.link.lock();
            st.writer = Some(writer);
            st.client = welcome.client;
            st.identity = vec![hop];
            st.bridge = bridge;
            st.ssh_pid = child.as_ref().map(Child::id);
            st.reconnects += 1;
            st.awaiting = None;
            match st.window_key.clone() {
                Some(window_key) => {
                    st.start_sync();
                    st.write(&ClientMsg::Attach { window_key });
                }
                None => st.finish_sync(),
            }
            drop(st);
            self.reader = reader;
            self.child = child;
            return Ok(());
        }
        Err(self.link.fail(format!(
            "gave up after {MAX_ATTEMPTS} reconnect attempts: {last}"
        )))
    }
}

impl Drop for LinkReader {
    fn drop(&mut self) {
        self.reap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_to_a_cap() {
        assert_eq!(backoff(1), Duration::from_millis(250));
        assert_eq!(backoff(2), Duration::from_millis(500));
        assert_eq!(backoff(5), Duration::from_secs(4));
        assert_eq!(backoff(6), Duration::from_secs(5));
        assert_eq!(backoff(40), Duration::from_secs(5));
    }

    #[test]
    fn states_read_as_tern_names_them() {
        assert_eq!(
            State::Reconnecting { attempt: 3 }.to_string(),
            "reconnecting(3)"
        );
        assert_eq!(State::Stalled.to_string(), "stalled");
        assert_eq!(
            serde_json::to_value(State::Reconnecting { attempt: 1 }).expect("json"),
            serde_json::json!({"state": "reconnecting", "attempt": 1})
        );
    }
}
