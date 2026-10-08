//! One client connection on the daemon socket (§15.6): the handshake, then messages in, and
//! frames out through a queue the pane readers feed.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use auk_mirror::codec::Codec;
use auk_mirror::{ClientMsg, DaemonMsg, Message};
use bytes::Bytes;
use futures_util::StreamExt;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio_util::codec::FramedRead;
use tokio_util::sync::CancellationToken;

use crate::server::Daemon;

pub type ClientId = u64;

/// How long a new connection may take to say hello.
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);

/// How many bytes may wait for a slow client before it is disconnected. It can reattach and
/// catch up from a snapshot.
const MAX_QUEUED: usize = 256 << 20;

/// The sending side of a client's outgoing queue.
#[derive(Clone, Debug)]
pub struct ClientTx {
    tx: mpsc::UnboundedSender<Bytes>,
    queued: Arc<AtomicUsize>,
    closed: CancellationToken,
}

impl ClientTx {
    /// Queues an encoded frame. A client that falls too far behind is disconnected.
    pub fn send(&self, frame: Bytes) {
        if self.closed.is_cancelled() {
            return;
        }
        let len = frame.len();
        if self.queued.fetch_add(len, Ordering::Relaxed) + len > MAX_QUEUED {
            self.closed.cancel();
            return;
        }
        if self.tx.send(frame).is_err() {
            self.closed.cancel();
        }
    }
}

/// Serves one connection until it closes.
pub async fn serve(daemon: Arc<Daemon>, stream: UnixStream) {
    let (read, mut write) = stream.into_split();
    let mut frames = FramedRead::new(read, Codec::<ClientMsg, DaemonMsg>::new());
    let hello = match tokio::time::timeout(HELLO_TIMEOUT, frames.next()).await {
        Ok(Some(Ok(ClientMsg::Hello(hello)))) => hello,
        _ => return,
    };
    if let Err(refusal) = daemon.versions.admit(&hello.versions) {
        eprintln!(
            "auk daemon: refused a {:?} client: {}",
            hello.kind, refusal.reason
        );
        let _ = write
            .write_all(&DaemonMsg::Refused(refusal).to_bytes())
            .await;
        let _ = write.shutdown().await;
        return;
    }

    let (tx, mut rx) = mpsc::unbounded_channel::<Bytes>();
    let queued = Arc::new(AtomicUsize::new(0));
    let closed = CancellationToken::new();
    let client_tx = ClientTx {
        tx,
        queued: queued.clone(),
        closed: closed.clone(),
    };
    let writer = {
        let closed = closed.clone();
        tokio::spawn(async move {
            loop {
                let frame = tokio::select! {
                    frame = rx.recv() => frame,
                    _ = closed.cancelled() => None,
                };
                let Some(frame) = frame else { break };
                if write.write_all(&frame).await.is_err() {
                    break;
                }
                queued.fetch_sub(frame.len(), Ordering::Relaxed);
            }
            closed.cancel();
        })
    };

    let id = daemon.add_client(hello.kind, hello.facts, client_tx);
    loop {
        let msg = tokio::select! {
            msg = frames.next() => msg,
            _ = closed.cancelled() => break,
            _ = daemon.shutdown.cancelled() => break,
        };
        match msg {
            Some(Ok(ClientMsg::Detach)) | None => break,
            Some(Ok(msg)) => on_message(&daemon, id, msg),
            Some(Err(err)) => {
                eprintln!("auk daemon: client {id}: {err}");
                break;
            }
        }
    }
    daemon.remove_client(id);
    closed.cancel();
    let _ = writer.await;
}

fn on_message(daemon: &Arc<Daemon>, id: ClientId, msg: ClientMsg) {
    match msg {
        ClientMsg::Attach { window_key } => daemon.attach(id, window_key),
        ClientMsg::Input { pane, bytes } => daemon.client_input(id, pane, bytes),
        ClientMsg::Resize { pane, cols, rows } => daemon.client_resize(id, pane, cols, rows),
        ClientMsg::Focus { pane } => daemon.client_focus(id, pane),
        ClientMsg::Facts(facts) => daemon.client_facts(id, facts),
        ClientMsg::LayoutEdit(edit) => {
            if let Err(err) = daemon.layout_edit(id, edit) {
                eprintln!("auk daemon: client {id}: layout edit: {err}");
            }
        }
        // Surfaces arrive with TSP (M3); until then no surface exists to ack or act on.
        ClientMsg::ViewEvent { .. } | ClientMsg::Ack { .. } => {}
        // Client environments matter once windows spawn panes with them (M4).
        ClientMsg::Env { .. } => {}
        ClientMsg::Hello(_) | ClientMsg::Detach => {}
    }
}
