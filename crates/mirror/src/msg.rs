//! The protocol's messages and their frames.

use std::collections::BTreeMap;

use auk_replica::{Record, Snapshot};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::frame::{Frame, WireError, kind, take_u32, take_u64, write_frame};
use crate::layout::{Layout, SessionId, Side, SplitDir, TabId, Window};
use crate::{PaneId, Versions};

/// What kind of client is attaching.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientKind {
    /// A macOS app window.
    App,
    /// A headless window.
    Headless,
    /// A browser tab.
    Web,
    /// A window attached over the remote transport.
    Remote,
}

/// Facts a program sees from its pane's primary client.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientFacts {
    /// Cell size in pixels: width, height.
    pub cell_px: [u16; 2],
    pub dark: bool,
    pub reduce_motion: bool,
    pub hour12: bool,
    /// The client's theme, which OSC 4/10/11/12 queries answer with. A client that doesn't say
    /// gets Auk's dark or light colors, following `dark`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colors: Option<ThemeColors>,
}

impl Default for ClientFacts {
    fn default() -> Self {
        ClientFacts {
            cell_px: [8, 16],
            dark: false,
            reduce_motion: false,
            hour12: false,
            colors: None,
        }
    }
}

impl ClientFacts {
    /// The terminal's answers from these facts: the cell size and the theme's colors. Colors
    /// whose background disagrees with `dark` give way to Auk's for that appearance, so OSC 11
    /// and the TSP hello's `dark` never tell a program different things.
    pub fn term_facts(&self) -> auk_term::Facts {
        let mut facts = self
            .colors
            .as_ref()
            .map(ThemeColors::term_facts)
            .filter(|f| f.is_dark() == self.dark)
            .unwrap_or_else(|| auk_term::Facts::for_appearance(self.dark));
        facts.cell_px = (self.cell_px[0], self.cell_px[1]);
        facts
    }
}

/// A client's theme colors: default foreground, background and cursor, and the 16 ANSI
/// colors, as RGB.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemeColors {
    pub foreground: [u8; 3],
    pub background: [u8; 3],
    pub cursor: [u8; 3],
    pub palette: [[u8; 3]; 16],
}

impl ThemeColors {
    fn term_facts(&self) -> auk_term::Facts {
        let rgb = |[r, g, b]: [u8; 3]| auk_term::Rgb(r, g, b);
        auk_term::Facts {
            cell_px: (8, 16),
            foreground: rgb(self.foreground),
            background: rgb(self.background),
            cursor: rgb(self.cursor),
            palette: self.palette.map(rgb),
        }
    }
}

impl From<&auk_term::Facts> for ThemeColors {
    fn from(f: &auk_term::Facts) -> Self {
        let rgb = |auk_term::Rgb(r, g, b): auk_term::Rgb| [r, g, b];
        ThemeColors {
            foreground: rgb(f.foreground),
            background: rgb(f.background),
            cursor: rgb(f.cursor),
            palette: f.palette.map(rgb),
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
    /// The client's own binary. A daemon of an older release execs it to upgrade in place;
    /// `None` where there is none (the browser).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exe: Option<std::path::PathBuf>,
    /// How the client reached the daemon, for presence: a remote link names its SSH hop.
    /// Empty for a client on the daemon's machine, which the daemon shows as a `local` hop.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub identity: Vec<Hop>,
}

impl Hello {
    /// A hello carrying this build's versions and, where there is one, its binary.
    pub fn new(kind: ClientKind, facts: ClientFacts) -> Self {
        Hello {
            versions: Versions::current(),
            kind,
            facts,
            exe: std::env::current_exe().ok(),
            identity: Vec::new(),
        }
    }
}

/// The daemon's answer to a client whose versions differ. The client offers an upgrade.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refusal {
    pub reason: String,
    /// The daemon's versions, so the client can say what to upgrade to.
    pub daemon: Versions,
    /// The daemon is upgrading itself to this client's release: connect again shortly.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub upgrading: bool,
}

/// A layout edit a window asks for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "edit", rename_all = "snake_case")]
pub enum LayoutEdit {
    NewSession {
        name: Option<String>,
    },
    NewTab {
        session: SessionId,
    },
    Split {
        pane: PaneId,
        dir: SplitDir,
    },
    Close {
        pane: PaneId,
    },
    Zoom {
        tab: TabId,
        pane: Option<PaneId>,
    },
    RenameSession {
        session: SessionId,
        name: String,
    },
    RenameTab {
        tab: TabId,
        title: Option<String>,
    },
    /// Locks or unlocks a session (`session lock` toggles: the window sends the opposite of
    /// what its layout shows).
    LockSession {
        session: SessionId,
        locked: bool,
    },
    /// Opens an agent pane: `agent_command` through the login shell, split right of
    /// `pane` (the window's focused pane), or in a new session when there is none.
    Agent {
        pane: Option<PaneId>,
    },
    /// Opens a plugin block pane of `kind` (`<plugin>.<block>`) with launch `args`: split
    /// off `split`'s pane, else a new tab in `session`, else a new session in the client's window
    /// key. A remote window opens its host's block types this way; a local one may too.
    Block {
        kind: String,
        args: Vec<String>,
        split: Option<(PaneId, SplitDir)>,
        session: Option<SessionId>,
        cwd: Option<String>,
    },
    /// Opens a pane running `launch` at `place` (`layout:new_tab`, `split`, `tab` and
    /// `cx:new_block`). Sent as a [`ClientMsg::Request`], it is answered with `{pane}`, plus
    /// `failed` when a block couldn't start.
    Open {
        place: OpenPlace,
        launch: Launch,
    },
    /// Moves `pane` to `side` of `target`, or swaps the two when `side` is absent
    /// (`layout:move`). Answered with whether anything moved: not for a missing pane, or one in
    /// another window key.
    Move {
        pane: PaneId,
        target: PaneId,
        side: Option<Side>,
    },
    /// Moves `pane`'s nearest divider on `side` by `delta` thousandths of its split, toward
    /// `side` when positive (`layout:resize`). Answered with whether it moved.
    Resize {
        pane: PaneId,
        side: Side,
        delta: i32,
    },
}

/// Where [`LayoutEdit::Open`] puts its pane.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "at", rename_all = "snake_case")]
pub enum OpenPlace {
    /// A new session in the client's window key.
    Session { name: Option<String> },
    /// A new last tab of a session.
    Tab { session: SessionId },
    /// Beside `pane`, on `side`; `share` is `pane`'s share of the split in thousandths (half by
    /// default).
    Split {
        pane: PaneId,
        side: Side,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        share: Option<u16>,
    },
}

/// What a pane opened with [`LayoutEdit::Open`] runs (`Launch`): a block of `block` with
/// `args`, else `command` through the login shell, else an interactive shell. `cwd` defaults to
/// the split target's directory, or home.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Launch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
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
    /// Bytes for the pane's PTY, already encoded for its modes. Sent as a binary frame.
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
    /// The client focused the pane, which makes it the pane's primary.
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
    /// A layout edit whose result the client waits for: the daemon answers with a
    /// [`DaemonMsg::Reply`] carrying the same `id`, after the layout change it made.
    Request {
        id: u64,
        edit: LayoutEdit,
    },
    /// Blobs the pane's documents name that this client doesn't hold: the daemon answers
    /// with a [`DaemonMsg::Blob`] for each one it has.
    Blobs {
        pane: PaneId,
        ids: Vec<String>,
    },
    /// Clears the pane's scrollback: a pane operation the daemon records in-band, so it
    /// reaches every replica and ends every surface anchored in the scrollback. Taken only from a
    /// client that shows the pane.
    ClearScrollback {
        pane: PaneId,
    },
    Detach,
}

/// An effect a host asks a window to perform.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "effect", rename_all = "snake_case")]
pub enum Effect {
    /// Two lines; `level` is `info`, `success` or `error`.
    Toast {
        #[serde(default = "info")]
        level: String,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sub: Option<String>,
    },
    /// Open a path or URL. `origin` is absent for `auk open` (the windows route it with origin
    /// `cli`), and `link` for a host plugin's `cx:open` (`route.link`, then `route.open` with
    /// origin `link`).
    Open {
        target: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<String>,
    },
    Copy {
        text: String,
    },
}

fn info() -> String {
    "info".to_owned()
}

/// An attached client, as presence reports it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientInfo {
    pub id: u64,
    pub kind: ClientKind,
    pub window_key: Option<String>,
    /// Who the client is: its identity chain, nearest hop last. A local client has one
    /// `local` hop.
    #[serde(default)]
    pub identity: Vec<Hop>,
}

/// One hop of an identity chain: a principal reached by a method from a machine, acting
/// as a user, as in "can@github via tailscale from laptop as can@build-box". The same shape as
/// `auk_remote::Hop`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hop {
    pub principal: String,
    /// `local`, `ssh-key` or `tailscale`.
    pub method: String,
    /// The machine the hop started from.
    pub origin: String,
    /// The user it acts as on the host, `user@host`.
    pub acting: String,
    /// The key's type and fingerprint, for a key hop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

/// Daemon to client.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DaemonMsg {
    /// The answer to an accepted hello.
    Welcome {
        versions: Versions,
        /// The plugin catalog; `null` until plugins exist.
        catalog: Value,
        /// Every window key's sessions.
        sessions: Layout,
        /// This connection's client id.
        client: u64,
    },
    /// The answer to a hello with other versions; the connection closes after it.
    Refused(Refusal),
    /// A pane's state at a stream position. Sent as a binary frame.
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
    /// A lens block's current view: it replaces the last one whole; `null` shows the raw
    /// output. `block` is the block's number in the pane.
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
    /// Whether this client is now the pane's primary.
    Primary {
        pane: PaneId,
        you: bool,
    },
    /// The answer to a [`ClientMsg::Request`]: its value, or why it failed.
    Reply {
        id: u64,
        #[serde(default)]
        value: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// A blob of the pane, outside the stream: snapshots don't carry blobs, so the daemon
    /// sends those the pane's documents name after a snapshot, and any a client asks for. Blob
    /// bytes are storage, not replicated state. Sent as a binary frame.
    #[serde(skip)]
    Blob {
        pane: PaneId,
        /// Lowercase hex SHA-256 of `bytes`.
        id: String,
        mime: Option<String>,
        bytes: Vec<u8>,
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

/// The JSON header of a blob frame.
#[derive(Serialize, Deserialize)]
struct BlobHeader {
    pane: PaneId,
    id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mime: Option<String>,
}

/// Encodes a blob frame without cloning its bytes.
pub fn encode_blob(out: &mut Vec<u8>, pane: PaneId, id: &str, mime: Option<&str>, bytes: &[u8]) {
    let header = serde_json::to_vec(&BlobHeader {
        pane,
        id: id.to_owned(),
        mime: mime.map(str::to_owned),
    })
    .expect("blob headers serialize");
    write_frame(
        out,
        kind::BLOB,
        &[&(header.len() as u32).to_le_bytes(), &header, bytes],
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
            DaemonMsg::Blob {
                pane,
                id,
                mime,
                bytes,
            } => encode_blob(out, *pane, id, mime.as_deref(), bytes),
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
            kind::BLOB => {
                let (len, rest) = take_u32(frame.payload, "blob")?;
                let len = len as usize;
                if rest.len() < len {
                    return Err(WireError::Truncated("blob"));
                }
                let (header, bytes) = rest.split_at(len);
                let BlobHeader { pane, id, mime } = serde_json::from_slice(header)?;
                Ok(DaemonMsg::Blob {
                    pane,
                    id,
                    mime,
                    bytes: bytes.to_vec(),
                })
            }
            other => Err(WireError::UnknownKind(other)),
        }
    }
}
