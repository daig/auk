//! Framing (§15.6): a u32 little-endian payload length, one type byte, then the payload.

use std::fmt;
use std::io::{self, Read, Write};

/// The largest payload accepted, matching the pane socket's limit (§11.2).
pub const MAX_PAYLOAD: usize = 256 << 20;

/// Bytes before the payload: the length and the type byte.
pub const HEADER_LEN: usize = 5;

/// Frame types. Output, input, snapshots and blobs carry raw bytes; everything else is JSON.
pub mod kind {
    /// A control message as JSON.
    pub const CONTROL: u8 = 0x01;
    /// Daemon to client: `pane` u64 LE, `seq` u64 LE, then the pane's output bytes.
    pub const OUTPUT: u8 = 0x02;
    /// Daemon to client: a u32 LE header length, the JSON header, then the snapshot bytes.
    pub const SNAPSHOT: u8 = 0x03;
    /// Client to daemon: `pane` u64 LE, then the bytes for the pane's PTY.
    pub const INPUT: u8 = 0x04;
    /// Daemon to client: a u32 LE header length, the JSON header (`pane`, `id`, `mime`), then the
    /// blob's bytes.
    pub const BLOB: u8 = 0x05;
}

/// Why bytes couldn't be read as a message.
#[derive(Debug)]
pub enum WireError {
    Io(io::Error),
    TooLarge(usize),
    UnknownKind(u8),
    Truncated(&'static str),
    Json(serde_json::Error),
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WireError::Io(err) => write!(f, "{err}"),
            WireError::TooLarge(n) => write!(f, "frame of {n} bytes exceeds {MAX_PAYLOAD}"),
            WireError::UnknownKind(k) => write!(f, "unknown frame type {k:#04x}"),
            WireError::Truncated(what) => write!(f, "truncated {what} frame"),
            WireError::Json(err) => write!(f, "bad control message: {err}"),
        }
    }
}

impl std::error::Error for WireError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            WireError::Io(err) => Some(err),
            WireError::Json(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for WireError {
    fn from(err: io::Error) -> Self {
        WireError::Io(err)
    }
}

impl From<serde_json::Error> for WireError {
    fn from(err: serde_json::Error) -> Self {
        WireError::Json(err)
    }
}

impl From<WireError> for io::Error {
    fn from(err: WireError) -> Self {
        match err {
            WireError::Io(err) => err,
            other => io::Error::new(io::ErrorKind::InvalidData, other),
        }
    }
}

/// One frame, borrowed from a buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame<'a> {
    pub kind: u8,
    pub payload: &'a [u8],
}

/// Appends a frame to `out`.
pub fn write_frame(out: &mut Vec<u8>, kind: u8, payload_parts: &[&[u8]]) {
    let len: usize = payload_parts.iter().map(|p| p.len()).sum();
    out.reserve(HEADER_LEN + len);
    out.extend_from_slice(&(len as u32).to_le_bytes());
    out.push(kind);
    for part in payload_parts {
        out.extend_from_slice(part);
    }
}

/// Splits the first complete frame off `buf`. Returns the frame and the bytes it used, or `None`
/// when more bytes are needed.
pub fn split_frame(buf: &[u8]) -> Result<Option<(Frame<'_>, usize)>, WireError> {
    if buf.len() < HEADER_LEN {
        return Ok(None);
    }
    let len = u32::from_le_bytes(buf[..4].try_into().expect("four bytes")) as usize;
    if len > MAX_PAYLOAD {
        return Err(WireError::TooLarge(len));
    }
    if buf.len() < HEADER_LEN + len {
        return Ok(None);
    }
    let frame = Frame {
        kind: buf[4],
        payload: &buf[HEADER_LEN..HEADER_LEN + len],
    };
    Ok(Some((frame, HEADER_LEN + len)))
}

/// Reads one frame into `buf` (replacing its contents) and returns its type, or `None` at a clean
/// end of stream.
pub fn read_frame(r: &mut impl Read, buf: &mut Vec<u8>) -> Result<Option<u8>, WireError> {
    let mut header = [0u8; HEADER_LEN];
    let mut filled = 0;
    while filled < HEADER_LEN {
        match r.read(&mut header[filled..]) {
            Ok(0) if filled == 0 => return Ok(None),
            Ok(0) => return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into()),
            Ok(n) => filled += n,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err.into()),
        }
    }
    let len = u32::from_le_bytes(header[..4].try_into().expect("four bytes")) as usize;
    if len > MAX_PAYLOAD {
        return Err(WireError::TooLarge(len));
    }
    buf.clear();
    buf.resize(len, 0);
    r.read_exact(buf)?;
    Ok(Some(header[4]))
}

/// Writes pre-encoded frames and flushes.
pub fn write_all(w: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    w.write_all(bytes)?;
    w.flush()
}

/// Little-endian u64 at the start of `bytes`.
pub(crate) fn take_u64<'a>(
    bytes: &'a [u8],
    what: &'static str,
) -> Result<(u64, &'a [u8]), WireError> {
    if bytes.len() < 8 {
        return Err(WireError::Truncated(what));
    }
    let (head, rest) = bytes.split_at(8);
    Ok((
        u64::from_le_bytes(head.try_into().expect("eight bytes")),
        rest,
    ))
}

/// Little-endian u32 at the start of `bytes`.
pub(crate) fn take_u32<'a>(
    bytes: &'a [u8],
    what: &'static str,
) -> Result<(u32, &'a [u8]), WireError> {
    if bytes.len() < 4 {
        return Err(WireError::Truncated(what));
    }
    let (head, rest) = bytes.split_at(4);
    Ok((
        u32::from_le_bytes(head.try_into().expect("four bytes")),
        rest,
    ))
}
