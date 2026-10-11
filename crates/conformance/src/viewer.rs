//! A test window on the daemon's wire: it attaches to a window key like the app does and
//! keeps a follower replica of each pane it is sent, so a test can read cells, links and modes
//! exactly as a client sees them after snapshot and live output.

use std::collections::BTreeMap;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex, MutexGuard};

use auk_mirror::{Client, ClientFacts, ClientKind, ClientMsg, DaemonMsg, Hello, PaneId};
use auk_replica::{Item, Replica, Role};

use crate::harness::{TestEnv, wait_for};

#[derive(Default)]
struct State {
    replicas: BTreeMap<PaneId, Replica>,
    /// Out-of-order items or a lost connection; a correct daemon causes none.
    errors: Vec<String>,
}

/// An attached client holding a replica per pane.
pub struct Viewer {
    state: Arc<Mutex<State>>,
    stream: UnixStream,
}

impl std::fmt::Debug for Viewer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Viewer").finish_non_exhaustive()
    }
}

impl Viewer {
    /// Connects to the test daemon and attaches to `window_key` (`main` holds panes `new` makes).
    pub fn attach(env: &TestEnv, window_key: &str) -> Viewer {
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
        let state = Arc::new(Mutex::new(State::default()));
        let shared = state.clone();
        std::thread::spawn(move || {
            loop {
                let msg = match client.recv() {
                    Ok(Some(msg)) => msg,
                    Ok(None) => break,
                    Err(e) => {
                        lock(&shared).errors.push(format!("read: {e}"));
                        break;
                    }
                };
                let mut guard = lock(&shared);
                let st = &mut *guard;
                let (pane, seq, item) = match &msg {
                    DaemonMsg::Snapshot { pane, snapshot } => {
                        st.replicas.insert(
                            *pane,
                            Replica::from_snapshot(snapshot, Role::Follower, None),
                        );
                        continue;
                    }
                    DaemonMsg::Output { pane, seq, bytes } => (*pane, *seq, Item::Output(bytes)),
                    DaemonMsg::Record { pane, seq, record } => (*pane, *seq, Item::Record(record)),
                    _ => continue,
                };
                if let Some(replica) = st.replicas.get_mut(&pane) {
                    if let Err(e) = replica.apply(seq, item) {
                        st.errors.push(format!("pane {pane} seq {seq}: {e:?}"));
                    }
                    let _ = replica.take_replies();
                }
            }
        });
        Viewer { state, stream }
    }

    /// Runs `f` on the pane's replica, once there is one.
    pub fn with<T>(&self, pane: PaneId, f: impl FnOnce(&Replica) -> T) -> Option<T> {
        lock(&self.state).replicas.get(&pane).map(f)
    }

    /// Waits until the pane's replica is at the daemon's stream position with the daemon's state
    /// hash, and returns that position.
    pub fn converge(&self, env: &TestEnv, pane: PaneId) -> u64 {
        wait_for("the viewer's replica to match the daemon's", || {
            let (hash, seq) = env.daemon_hash(pane);
            let ours = self.with(pane, |r| {
                let h: String = r.state_hash().iter().map(|b| format!("{b:02x}")).collect();
                (h, r.seq())
            })?;
            (ours == (hash, seq)).then_some(seq)
        })
    }

    /// Stream errors seen so far.
    pub fn errors(&self) -> Vec<String> {
        lock(&self.state).errors.clone()
    }
}

impl Drop for Viewer {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(|e| e.into_inner())
}
