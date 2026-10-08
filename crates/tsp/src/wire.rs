//! The wire (§7.1): parsing a program's APC payloads into verb, parameters and body; encoding
//! terminal-to-program messages; the chunker and the chunk joiner.
//!
//! Nothing here reads a clock. The 1 s chunk timeout is decided by the daemon, which sequences it
//! into the stream so every replica drops the sequence at the same point
//! ([`Joiner::expire`]).

use serde_json::Value;

/// What follows `ESC _` in every TSP string.
pub const PREFIX: &[u8] = b"tsp;";
/// The longest APC string, counted after `ESC _`, parameters included (§7.14).
pub const MAX_APC: usize = 262_144;
/// The longest joined chunked body (§7.1).
pub const MAX_JOINED: usize = 25_165_824;

/// One program-to-terminal message: `tsp;<verb>[;<key>=<value>]*;<body>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    /// The verb as sent. TSP verbs are one case-sensitive letter; anything else is unknown.
    pub verb: String,
    /// Parameters in the order sent.
    pub params: Vec<(String, String)>,
    /// The body: JSON, or base64 for `b`.
    pub body: Vec<u8>,
}

impl Message {
    /// The first value of parameter `key`.
    pub fn param(&self, key: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// A byte of a parameter key: `[A-Za-z0-9_-]`.
pub const fn key_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// A byte of a parameter value: printable ASCII other than `;` (and so no space).
pub const fn value_byte(b: u8) -> bool {
    b >= 0x21 && b <= 0x7e && b != b';'
}

/// Splits `segment` (the bytes up to a `;`) as `key=value` when it has that form.
fn split_param(segment: &[u8]) -> Option<(&[u8], &[u8])> {
    let key = segment.iter().take_while(|&&b| key_byte(b)).count();
    if key == 0 || segment.get(key) != Some(&b'=') {
        return None;
    }
    let value = &segment[key + 1..];
    value
        .iter()
        .all(|&b| value_byte(b))
        .then_some((&segment[..key], value))
}

/// Whether `rest` begins with a parameter-shaped segment: `key=value` followed by `;`.
pub fn leads_with_parameter(rest: &[u8]) -> bool {
    rest.iter()
        .position(|&b| b == b';')
        .is_some_and(|end| split_param(&rest[..end]).is_some())
}

/// Parses an APC payload (the bytes after `ESC _`, without the terminator). Returns `None` when
/// it isn't a TSP string.
///
/// A segment is a parameter only if it is `key=value` and another `;` follows it; the first
/// segment that isn't one starts the body (§7.1).
pub fn parse(payload: &[u8]) -> Option<Message> {
    let rest = payload.strip_prefix(PREFIX)?;
    let (verb, mut at) = match rest.iter().position(|&b| b == b';') {
        Some(end) => (&rest[..end], end + 1),
        None => (rest, rest.len()),
    };
    let mut params = Vec::new();
    while let Some(end) = rest[at..].iter().position(|&b| b == b';') {
        let Some((k, v)) = split_param(&rest[at..at + end]) else {
            break;
        };
        // Both halves are ASCII by construction.
        params.push((ascii(k), ascii(v)));
        at += end + 1;
    }
    Some(Message {
        verb: String::from_utf8_lossy(verb).into_owned(),
        params,
        body: rest[at..].to_vec(),
    })
}

fn ascii(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| char::from(b)).collect()
}

/// Appends one APC string `ESC _ tsp;<verb>;<k>=<v>;…<body> ESC \`.
pub fn push_apc(out: &mut Vec<u8>, verb: &str, params: &[(&str, &str)], body: &[u8]) {
    out.extend_from_slice(b"\x1b_tsp;");
    out.extend_from_slice(verb.as_bytes());
    out.push(b';');
    for (k, v) in params {
        out.extend_from_slice(k.as_bytes());
        out.push(b'=');
        out.extend_from_slice(v.as_bytes());
        out.push(b';');
    }
    out.extend_from_slice(body);
    out.extend_from_slice(b"\x1b\\");
}

/// Encodes one terminal-to-program message: `ESC _ tsp;<r|e>;<json> ESC \`, compact JSON, no
/// parameters, never chunked, ST only (§7.1, F1).
pub fn encode_to_program(verb: ToProgram, body: &Value) -> Vec<u8> {
    let json = body.to_string();
    let mut out = Vec::with_capacity(json.len() + 12);
    push_apc(&mut out, verb.as_str(), &[], json.as_bytes());
    out
}

/// The two terminal-to-program verbs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ToProgram {
    /// `r`: a reply to a query.
    Reply,
    /// `e`: an event.
    Event,
}

impl ToProgram {
    pub const fn as_str(self) -> &'static str {
        match self {
            ToProgram::Reply => "r",
            ToProgram::Event => "e",
        }
    }
}

/// Where the chunk of `body` starting at `start` ends: at most `limit` bytes on, at the last
/// position that is a UTF-8 boundary and doesn't begin a parameter-shaped segment; failing that,
/// at the first such position after it.
fn chunk_end(body: &[u8], start: usize, limit: usize) -> usize {
    let target = start.saturating_add(limit.max(1));
    if target >= body.len() {
        return body.len();
    }
    let safe = |at: usize| body[at] & 0xc0 != 0x80 && !leads_with_parameter(&body[at..]);
    (start + 1..=target)
        .rev()
        .find(|&at| safe(at))
        .or_else(|| (target + 1..body.len()).find(|&at| safe(at)))
        .unwrap_or(body.len())
}

/// Splits a body into chunk bodies the way Tern's chunker does: each at most `limit` bytes where
/// possible, cut only where the next piece starts on a UTF-8 boundary and not with a
/// parameter-shaped segment. Since a parameter needs a `;` before any byte outside
/// `[A-Za-z0-9_-]=` and value bytes, this cuts only right before a `;` or a byte that can't be
/// read as a parameter (§7.1). An empty body has no chunks.
pub fn chunks(body: &[u8], limit: usize) -> impl Iterator<Item = &[u8]> {
    let mut start = 0;
    std::iter::from_fn(move || {
        (start < body.len()).then(|| {
            let end = chunk_end(body, start, limit);
            let piece = &body[start..end];
            start = end;
            piece
        })
    })
}

/// Encodes a program-style message, chunked under `limit` with chunk id `chunk` when its body is
/// longer. Used for snapshots, which replay through the normal parser.
pub fn encode_chunked(
    out: &mut Vec<u8>,
    verb: &str,
    params: &[(&str, &str)],
    body: &[u8],
    limit: usize,
    chunk: &str,
) {
    if body.len() <= limit {
        push_apc(out, verb, params, body);
        return;
    }
    let pieces: Vec<&[u8]> = chunks(body, limit).collect();
    let last = pieces.len() - 1;
    for (i, piece) in pieces.into_iter().enumerate() {
        let mut ps: Vec<(&str, &str)> = if i == 0 { params.to_vec() } else { Vec::new() };
        ps.push(("c", chunk));
        if i < last {
            ps.push(("m", "1"));
        }
        push_apc(out, verb, &ps, piece);
    }
}

/// `n` in lowercase base 36, as chunk ids are written.
pub fn base36(mut n: u64) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    loop {
        out.push(DIGITS[(n % 36) as usize]);
        n /= 36;
        if n == 0 {
            break;
        }
    }
    out.reverse();
    out.into_iter().map(char::from).collect()
}

/// A chunk sequence being joined.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sequence {
    pub verb: String,
    /// The `c` id.
    pub id: String,
    /// The first chunk's parameters other than `c` and `m`.
    pub params: Vec<(String, String)>,
    /// The bytes joined so far.
    pub body: Vec<u8>,
}

/// What the joiner made of a message, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Joined {
    /// A complete message to handle.
    Message(Message),
    /// A sequence was dropped; the error message to report.
    Dropped(String),
}

/// Joins chunk sequences (§7.1), one at a time.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Joiner {
    active: Option<Sequence>,
    /// A sequence dropped for its size, whose remaining chunks are swallowed.
    swallowing: Option<(String, String)>,
}

impl Joiner {
    pub fn new() -> Self {
        Self::default()
    }

    /// The sequence being joined, if any.
    pub fn pending(&self) -> Option<&Sequence> {
        self.active.as_ref()
    }

    /// The sequence whose remaining chunks are being swallowed after an over-size drop.
    pub fn swallowing(&self) -> Option<(&str, &str)> {
        self.swallowing
            .as_ref()
            .map(|(v, c)| (v.as_str(), c.as_str()))
    }

    /// Feeds one parsed message and returns what it completes, in order.
    pub fn push(&mut self, msg: Message) -> Vec<Joined> {
        let mut out = Vec::new();
        let chunk = msg.param("c").map(str::to_owned);
        let more = msg.param("m") == Some("1");

        if let Some(active) = &mut self.active {
            if chunk.as_deref() == Some(active.id.as_str()) && msg.verb == active.verb {
                if active.body.len() + msg.body.len() > MAX_JOINED {
                    out.push(Joined::Dropped(dropped(
                        &active.verb,
                        &active.id,
                        "over 25165824 bytes",
                    )));
                    if more {
                        self.swallowing = Some((active.verb.clone(), active.id.clone()));
                    }
                    self.active = None;
                    return out;
                }
                active.body.extend_from_slice(&msg.body);
                if !more {
                    let seq = self.active.take().expect("active sequence");
                    out.push(Joined::Message(Message {
                        verb: seq.verb,
                        params: seq.params,
                        body: seq.body,
                    }));
                }
                return out;
            }
            let seq = self.active.take().expect("active sequence");
            out.push(Joined::Dropped(dropped(&seq.verb, &seq.id, "interrupted")));
        }

        if let Some((verb, id)) = &self.swallowing {
            if chunk.as_deref() == Some(id.as_str()) && &msg.verb == verb {
                if !more {
                    self.swallowing = None;
                }
                return out;
            }
            self.swallowing = None;
        }

        match chunk {
            Some(id) => {
                let params = msg
                    .params
                    .into_iter()
                    .filter(|(k, _)| k != "c" && k != "m")
                    .collect();
                if more {
                    self.active = Some(Sequence {
                        verb: msg.verb,
                        id,
                        params,
                        body: msg.body,
                    });
                } else {
                    out.push(Joined::Message(Message {
                        verb: msg.verb,
                        params,
                        body: msg.body,
                    }));
                }
            }
            None => out.push(Joined::Message(msg)),
        }
        out
    }

    /// The chunk timeout passed: drops the active sequence, returning the error to report.
    pub fn expire(&mut self) -> Option<String> {
        self.active
            .take()
            .map(|seq| dropped(&seq.verb, &seq.id, "timed out"))
    }

    /// Resumes swallowing the rest of an over-size sequence (snapshot replay).
    pub fn set_swallowing(&mut self, (verb, id): (String, String)) {
        self.swallowing = Some((verb, id));
    }

    /// RIS: drops everything silently.
    pub fn reset(&mut self) {
        self.active = None;
        self.swallowing = None;
    }

    /// Whether no sequence is being joined or swallowed.
    pub fn is_idle(&self) -> bool {
        self.active.is_none() && self.swallowing.is_none()
    }
}

fn dropped(verb: &str, id: &str, why: &str) -> String {
    format!("chunked {verb} message c={id} dropped: {why}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_verb_params_and_body() {
        let m = parse(b"tsp;b;id=abc;mime=image/png;aGVsbG8=").unwrap();
        assert_eq!(m.verb, "b");
        assert_eq!(m.param("id"), Some("abc"));
        assert_eq!(m.param("mime"), Some("image/png"));
        assert_eq!(m.body, b"aGVsbG8=");
    }

    #[test]
    fn a_trailing_param_shaped_segment_is_body() {
        // `x=1` has no `;` after it, so it is the body.
        let m = parse(b"tsp;f;x=1").unwrap();
        assert!(m.params.is_empty());
        assert_eq!(m.body, b"x=1");
    }

    #[test]
    fn segments_with_spaces_or_bad_keys_start_the_body() {
        let m = parse(b"tsp;f;a b=1;{}").unwrap();
        assert!(m.params.is_empty());
        assert_eq!(m.body, b"a b=1;{}");
        let m = parse(b"tsp;f;k.x=1;{}").unwrap();
        assert!(m.params.is_empty());
        let m = parse(b"tsp;f;k=;{}").unwrap();
        assert_eq!(m.params, vec![("k".into(), String::new())]);
        assert_eq!(m.body, b"{}");
    }

    #[test]
    fn non_tsp_payloads_are_not_parsed() {
        assert_eq!(parse(b"Gf=100;AAAA"), None);
        assert_eq!(parse(b"tsp"), None);
        let m = parse(b"tsp;q").unwrap();
        assert_eq!((m.verb.as_str(), m.body.len()), ("q", 0));
    }

    #[test]
    fn base36_counts_like_the_sdk() {
        let ids: Vec<String> = [1, 9, 10, 35, 36, 37].into_iter().map(base36).collect();
        assert_eq!(ids, ["1", "9", "a", "z", "10", "11"]);
    }

    #[test]
    fn chunker_cuts_only_at_safe_points() {
        let body = "é;k=v;".repeat(50);
        for limit in 1..20 {
            let pieces: Vec<&[u8]> = chunks(body.as_bytes(), limit).collect();
            assert_eq!(pieces.concat(), body.as_bytes());
            for piece in &pieces[1..] {
                assert_ne!(piece[0] & 0xc0, 0x80, "split inside UTF-8");
                assert!(!leads_with_parameter(piece), "chunk leads with a parameter");
            }
        }
        assert_eq!(chunks(b"", 4).count(), 0);
    }

    #[test]
    fn joiner_drops_on_size() {
        let mut j = Joiner::new();
        let big = vec![b'a'; MAX_APC];
        let chunk = |more: bool| Message {
            verb: "f".into(),
            params: if more {
                vec![("c".into(), "1".into()), ("m".into(), "1".into())]
            } else {
                vec![("c".into(), "1".into())]
            },
            body: big.clone(),
        };
        let mut errors = Vec::new();
        for _ in 0..MAX_JOINED / MAX_APC + 4 {
            errors.extend(j.push(chunk(true)));
        }
        assert_eq!(
            errors,
            vec![Joined::Dropped(
                "chunked f message c=1 dropped: over 25165824 bytes".into()
            )]
        );
        // The rest of that sequence is swallowed, its last chunk included.
        assert!(j.push(chunk(false)).is_empty());
        assert!(j.is_idle());
    }
}
