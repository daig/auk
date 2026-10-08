//! The messages of §15.6 and their frames.

use std::collections::BTreeMap;

use auk_replica::{Record, Snapshot};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::frame::{Frame, WireError, kind, take_u32, take_u64, write_frame};
use crate::layout::{Layout, SessionId, SplitDir, TabId, Window};
use crate::{PaneId, Versions};

/// What kind of client is attaching.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientKind {
    /// A macOS app window.
    App,
    /// A headless window (§11.3).
    Headless,
    /// A browser tab (§10.3).
    Web,
    /// A window attached over the remote transport (§10.2).
    Remote,
}

/// Facts a program sees from its pane's primary client (§15.5).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientFacts {
    /// Cell size in pixels: width, height.
    pub cell_px: [u16; 2],
    pub dark: bool,
    pub reduce_motion: bool,
    pub hour12: bool,
}

impl Default for ClientFacts {
    fn default() -> Self {
        ClientFacts {
            cell_px: [8, 16],
            dark: false,
            reduce_motion: false,
            hour12: false,
        }
    }
}

/// The client's first message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub versions: Versions,
    pub kind: ClientKind,
    #[serde(default)]
    pub facts: ClientFacts,
}

impl Hello {
    /// A hello carrying this build's versions.
    pub fn new(kind: ClientKind, facts: ClientFacts) -> Self {
        Hello {
            versions: Versions::current(),
            kind,
            facts,
        }
    }
}

/// The daemon's answer to a client whose versions differ (X9). The client offers an upgrade.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refusal {
    pub reason: String,
    /// The daemon's versions, so the client can say what to upgrade to.
    pub daemon: Versions,
}

/// A layout edit a window asks for (§4.3). M4 completes the multi-window editing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "edit", rename_all = "snake_case")]
pub enum LayoutEdit {
    NewSession { name: Option<String> },
    NewTab { session: SessionId },
    Split { pane: PaneId, dir: SplitDir },
    Close { pane: PaneId },
    Zoom { tab: TabId, pane: Option<PaneId> },
    RenameSession { session: SessionId, name: String },
    RenameTab { tab: TabId, title: Option<String> },
}

/// Client to daemon.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    Hello(Hello),
    /// Show the sessions of a window key: the daemon answers with its layout, a snapshot per
    /// visible pane, then live output.
    Attach {
        window_key: String,
    },
    /// Bytes for the pane's PTY, already encoded for its modes (§15.4). Sent as a binary frame.
    #[serde(skip)]
    Input {
        pane: PaneId,
        bytes: Vec<u8>,
    },
    ViewEvent {
        pane: PaneId,
        sf: String,
        event: Value,
    },
    Ack {
        pane: PaneId,
        sf: String,
        s: u64,
    },
    /// The size this client would show the pane at; it applies while the client is primary.
    Resize {
        pane: PaneId,
        cols: u16,
        rows: u16,
    },
    /// The client focused the pane, which makes it the pane's primary (§15.5).
    Focus {
        pane: PaneId,
    },
    /// Updated client facts.
    Facts(ClientFacts),
    /// The client's environment, for panes it asks to spawn.
    Env {
        vars: BTreeMap<String, String>,
    },
    LayoutEdit(LayoutEdit),
    Detach,
}

/// An effect a host asks a window to perform.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "effect", rename_all = "snake_case")]
pub enum Effect {
    Toast { text: String },
    Open { target: String },
    Copy { text: String },
}

/// An attached client, as presence reports it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientInfo {
    pub id: u64,
    pub kind: ClientKind,
    pub window_key: Option<String>,
}

/// Daemon to client.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DaemonMsg {
    /// The answer to an accepted hello.
    Welcome {
        versions: Versions,
        /// The plugin catalog (M7); `null` until plugins exist.
        catalog: Value,
        /// Every window key's sessions.
        sessions: Layout,
        /// This connection's client id.
        client: u64,
    },
    /// The answer to a hello with other versions; the connection closes after it.
    Refused(Refusal),
    /// A pane's state at a stream position (§15.3). Sent as a binary frame.
    #[serde(skip)]
    Snapshot {
        pane: PaneId,
        snapshot: Snapshot,
    },
    /// Pane output numbered `seq`. Sent as a binary frame.
    #[serde(skip)]
    Output {
        pane: PaneId,
        seq: u64,
        bytes: Vec<u8>,
    },
    Record {
        pane: PaneId,
        seq: u64,
        record: Record,
    },
    LensView {
        pane: PaneId,
        block: String,
        node: Value,
    },
    Effect(Effect),
    /// The attached window key's sessions, whenever they change.
    Layout {
        window_key: String,
        window: Window,
    },
    Catalog {
        catalog: Value,
    },
    Presence {
        clients: Vec<ClientInfo>,
    },
    /// The pane's process ended. `status` is its exit code, or 128 plus the signal number.
    PaneExit {
        pane: PaneId,
        status: i32,
    },
    /// Whether this client is now the pane's primary (§15.5).
    Primary {
        pane: PaneId,
        you: bool,
    },
}

/// A message with a frame encoding.
pub trait Message: Sized {
    /// Appends the message's frame to `out`.
    fn encode(&self, out: &mut Vec<u8>);

    /// Reads a message from a frame.
    fn decode(frame: Frame<'_>) -> Result<Self, WireError>;

    /// The message's frame as bytes.
    fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }
}

fn encode_json(msg: &impl Serialize, out: &mut Vec<u8>) {
    let json = serde_json::to_vec(msg).expect("control messages always serialize");
    write_frame(out, kind::CONTROL, &[&json]);
}

impl Message for ClientMsg {
    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            ClientMsg::Input { pane, bytes } => {
                write_frame(out, kind::INPUT, &[&pane.to_le_bytes(), bytes]);
            }
            other => encode_json(other, out),
        }
    }

    fn decode(frame: Frame<'_>) -> Result<Self, WireError> {
        match frame.kind {
            kind::CONTROL => Ok(serde_json::from_slice(frame.payload)?),
            kind::INPUT => {
                let (pane, bytes) = take_u64(frame.payload, "input")?;
                Ok(ClientMsg::Input {
                    pane,
                    bytes: bytes.to_vec(),
                })
            }
            other => Err(WireError::UnknownKind(other)),
        }
    }
}

/// The JSON header of a snapshot frame.
#[derive(Serialize, Deserialize)]
struct SnapshotHeader {
    pane: PaneId,
    #[serde(flatten)]
    snapshot: Snapshot,
}

/// Encodes an output frame without building a message, for the daemon's hot path.
pub fn encode_output(out: &mut Vec<u8>, pane: PaneId, seq: u64, bytes: &[u8]) {
    write_frame(
        out,
        kind::OUTPUT,
        &[&pane.to_le_bytes(), &seq.to_le_bytes(), bytes],
    );
}

/// Encodes a snapshot frame without cloning the snapshot's bytes.
pub fn encode_snapshot(out: &mut Vec<u8>, pane: PaneId, snapshot: &Snapshot) {
    #[derive(Serialize)]
    struct HeaderRef<'a> {
        pane: PaneId,
        #[serde(flatten)]
        snapshot: &'a Snapshot,
    }
    let header =
        serde_json::to_vec(&HeaderRef { pane, snapshot }).expect("snapshot headers serialize");
    write_frame(
        out,
        kind::SNAPSHOT,
        &[
            &(header.len() as u32).to_le_bytes(),
            &header,
            &snapshot.bytes,
        ],
    );
}

impl Message for DaemonMsg {
    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            DaemonMsg::Output { pane, seq, bytes } => encode_output(out, *pane, *seq, bytes),
            DaemonMsg::Snapshot { pane, snapshot } => encode_snapshot(out, *pane, snapshot),
            other => encode_json(other, out),
        }
    }

    fn decode(frame: Frame<'_>) -> Result<Self, WireError> {
        match frame.kind {
            kind::CONTROL => Ok(serde_json::from_slice(frame.payload)?),
            kind::OUTPUT => {
                let (pane, rest) = take_u64(frame.payload, "output")?;
                let (seq, bytes) = take_u64(rest, "output")?;
                Ok(DaemonMsg::Output {
                    pane,
                    seq,
                    bytes: bytes.to_vec(),
                })
            }
            kind::SNAPSHOT => {
                let (len, rest) = take_u32(frame.payload, "snapshot")?;
                let len = len as usize;
                if rest.len() < len {
                    return Err(WireError::Truncated("snapshot"));
                }
                let (header, bytes) = rest.split_at(len);
                let SnapshotHeader { pane, mut snapshot } = serde_json::from_slice(header)?;
                snapshot.bytes = bytes.to_vec();
                Ok(DaemonMsg::Snapshot { pane, snapshot })
            }
            other => Err(WireError::UnknownKind(other)),
        }
    }
}
