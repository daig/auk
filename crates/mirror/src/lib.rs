//! The daemon-to-client replication protocol (spec §15.6).
//!
//! - **Frames** ([`frame`]): a u32 little-endian payload length, a type byte, then the payload.
//!   Pane output, input and snapshots carry raw bytes behind a small binary header; every other
//!   message is JSON.
//! - **Messages** ([`msg`]): [`ClientMsg`] and [`DaemonMsg`], the table of §15.6.
//! - **Handshake**: the client's [`Hello`] carries the protocol version, the parser version
//!   ([`auk_replica::PARSER_VERSION`]), the Auk version, its kind and its facts. The daemon
//!   answers [`DaemonMsg::Welcome`], or [`DaemonMsg::Refused`] when any version differs (X9).
//!   Then [`ClientMsg::Attach`] brings the window's layout, a snapshot per visible pane at a
//!   sequence number, and the live stream after it (§15.3).
//! - **Codecs**: blocking ([`read_msg`], [`write_msg`], [`Client`]) and, with the `tokio`
//!   feature, [`codec::Codec`] for `tokio_util`'s framed streams.
//!
//! Builds for wasm32 without the `tokio` feature: the browser client uses it.

pub mod frame;
pub mod layout;
pub mod msg;

#[cfg(feature = "tokio")]
pub mod codec;

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

pub use frame::{Frame, MAX_PAYLOAD, WireError};
pub use msg::{
    ClientFacts, ClientInfo, ClientKind, ClientMsg, DaemonMsg, Effect, Hello, LayoutEdit, Message,
    Refusal,
};

/// A pane's id, unique for the daemon's lifetime (and kept across restore).
pub type PaneId = u64;

/// The version of this protocol. Bump it with any change to frames or messages.
pub const PROTOCOL_VERSION: u32 = 1;

/// The versions client and daemon must share (§4.5, §15.2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Versions {
    pub protocol: u32,
    /// [`auk_replica::PARSER_VERSION`].
    pub parser: String,
    /// The Auk release.
    pub auk: String,
}

impl Versions {
    /// This build's versions.
    pub fn current() -> Self {
        Versions {
            protocol: PROTOCOL_VERSION,
            parser: auk_replica::PARSER_VERSION.to_owned(),
            auk: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }

    /// Checks a client's versions against the daemon's (`self`). Any difference refuses the
    /// client: replicas must parse identically (X9).
    pub fn admit(&self, client: &Versions) -> Result<(), Refusal> {
        let reason = if client.protocol != self.protocol {
            format!(
                "protocol {} differs from the daemon's {}",
                client.protocol, self.protocol
            )
        } else if client.parser != self.parser {
            format!(
                "parser {} differs from the daemon's {}",
                client.parser, self.parser
            )
        } else if client.auk != self.auk {
            format!("Auk {} differs from the daemon's {}", client.auk, self.auk)
        } else {
            return Ok(());
        };
        Err(Refusal {
            reason,
            daemon: self.clone(),
        })
    }
}

/// Reads one message, or `None` at a clean end of stream.
pub fn read_msg<M: Message>(r: &mut impl Read, buf: &mut Vec<u8>) -> Result<Option<M>, WireError> {
    match frame::read_frame(r, buf)? {
        None => Ok(None),
        Some(kind) => M::decode(Frame { kind, payload: buf }).map(Some),
    }
}

/// Writes one message and flushes.
pub fn write_msg<M: Message>(w: &mut impl Write, msg: &M) -> io::Result<()> {
    frame::write_all(w, &msg.to_bytes())
}

/// Why a handshake failed.
#[derive(Debug)]
pub enum HandshakeError {
    Wire(WireError),
    /// The daemon refused this client's versions (X9).
    Refused(Refusal),
    /// The daemon closed the connection or answered out of turn.
    Unexpected(String),
}

impl std::fmt::Display for HandshakeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HandshakeError::Wire(err) => write!(f, "{err}"),
            HandshakeError::Refused(r) => write!(
                f,
                "the daemon refused this client: {}; upgrade to Auk {} (parser {})",
                r.reason, r.daemon.auk, r.daemon.parser
            ),
            HandshakeError::Unexpected(what) => write!(f, "unexpected handshake answer: {what}"),
        }
    }
}

impl std::error::Error for HandshakeError {}

impl From<WireError> for HandshakeError {
    fn from(err: WireError) -> Self {
        HandshakeError::Wire(err)
    }
}

impl From<io::Error> for HandshakeError {
    fn from(err: io::Error) -> Self {
        HandshakeError::Wire(err.into())
    }
}

/// What the daemon said in its welcome.
#[derive(Clone, Debug, PartialEq)]
pub struct Welcome {
    pub versions: Versions,
    pub catalog: serde_json::Value,
    pub sessions: layout::Layout,
    pub client: u64,
}

/// A blocking client connection over any byte stream (a Unix socket, SSH stdio).
#[derive(Debug)]
pub struct Client<S> {
    stream: S,
    buf: Vec<u8>,
}

impl<S: Read + Write> Client<S> {
    /// Sends `hello` and reads the welcome.
    pub fn handshake(mut stream: S, hello: &Hello) -> Result<(Self, Welcome), HandshakeError> {
        write_msg(&mut stream, &ClientMsg::Hello(hello.clone()))?;
        let mut client = Client {
            stream,
            buf: Vec::new(),
        };
        match client.recv()? {
            Some(DaemonMsg::Welcome {
                versions,
                catalog,
                sessions,
                client: id,
            }) => Ok((
                client,
                Welcome {
                    versions,
                    catalog,
                    sessions,
                    client: id,
                },
            )),
            Some(DaemonMsg::Refused(refusal)) => Err(HandshakeError::Refused(refusal)),
            Some(other) => Err(HandshakeError::Unexpected(format!("{other:?}"))),
            None => Err(HandshakeError::Unexpected("the connection closed".into())),
        }
    }

    pub fn send(&mut self, msg: &ClientMsg) -> io::Result<()> {
        write_msg(&mut self.stream, msg)
    }

    /// The next message, or `None` when the daemon closed the connection.
    pub fn recv(&mut self) -> Result<Option<DaemonMsg>, WireError> {
        read_msg(&mut self.stream, &mut self.buf)
    }

    pub fn get_ref(&self) -> &S {
        &self.stream
    }

    pub fn into_inner(self) -> S {
        self.stream
    }
}

#[cfg(test)]
mod tests;
