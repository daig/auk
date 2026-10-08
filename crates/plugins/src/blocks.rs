//! Running blocks (§9.5): a block is a daemon pane that speaks TSP with no program features. Its
//! worker writes the pane's output bytes (a hello, a screen surface with role
//! `plugin.<plugin>.<block>`, frames, titles) and reads the pane's input (keys, and the
//! terminal's replies and events).
//!
//! Views are reconciled into frame ops exactly as the SDK does it (derived ids, `set`/`text`/
//! `move`/`del`/`add`), with the SDK's own reconciler, which Tern's plugin worker also uses.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use mlua::Value;
use serde_json::{Value as Json, json};
use tern_sdk::input::{Item, Parser};
use tern_sdk::keys::{Decoder, Key};
use tern_sdk::reconcile::Doc;
use tern_sdk::wire::{Event, Reply};

use crate::api::BlockHandlers;
use crate::api::block::Geometry;
use crate::events::PaneId;

/// The default largest body per APC string (the hello reply's `apc` replaces it).
const APC_LIMIT: usize = 65536;

/// What a block pane hands the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockOutput {
    /// Bytes for the pane's output stream, as a program would write them.
    Bytes {
        /// The pane.
        pane: PaneId,
        /// TSP messages and title sequences.
        bytes: Vec<u8>,
    },
    /// `save`'s result changed: the JSON text to keep with the pane (passed back on restore).
    Saved {
        /// The pane.
        pane: PaneId,
        /// JSON text.
        saved: String,
    },
    /// The block ended: `cx:exit(code)`, a failed `init` (status 1, the error in `message` to
    /// print in the pane), or a type that no longer exists after a reload.
    Exited {
        /// The pane.
        pane: PaneId,
        /// Exit status.
        status: i32,
        /// Text to print in the pane.
        message: Option<String>,
    },
}

/// What the host remembers of a running block, so it survives a reload or a restart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockRecord {
    /// `<plugin>.<block>`.
    pub kind: String,
    /// Launch arguments.
    pub args: Vec<String>,
    /// The last stored `save` result (JSON text).
    pub saved: Option<String>,
    /// The pane's directory when the block started.
    pub cwd: Option<String>,
    /// Size in cells.
    pub cols: u16,
    /// Size in cells.
    pub rows: u16,
    /// Which start this is in the pane (each start opens surface `s<generation>`).
    pub generation: u32,
}

/// A request to start a block in a worker.
#[derive(Debug, Clone)]
pub(crate) struct BlockStart {
    pub pane: PaneId,
    pub kind: String,
    pub block: String,
    pub title: String,
    pub plugin: String,
    pub args: Vec<String>,
    pub saved: Option<String>,
    pub cwd: Option<String>,
    pub cols: u16,
    pub rows: u16,
    pub surface: String,
    pub previous_surface: Option<String>,
}

/// Something the pane's input carried for the block.
#[derive(Debug)]
pub(crate) enum BlockInput {
    Key(Key),
    Event(serde_json::Map<String, Json>),
}

/// A running block on its worker.
pub(crate) struct BlockRun {
    pub block: String,
    kind: String,
    title_default: String,
    role: String,
    pub handlers: BlockHandlers,
    pub state: Value,
    pub cx: Value,
    pub geometry: Rc<RefCell<Geometry>>,
    surface: String,
    doc: Doc,
    seq: u64,
    title: Option<String>,
    pub saved: Option<String>,
    limit: usize,
    next_chunk: u64,
    input: Parser,
    keys: Decoder,
    /// When undecided key bytes are released.
    pub flush_at: Option<Instant>,
}

fn base36(mut n: u64) -> String {
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
    String::from_utf8(out).unwrap_or_default()
}

impl BlockRun {
    pub(crate) fn new(
        start: &BlockStart,
        handlers: BlockHandlers,
        state: Value,
        cx: Value,
        geometry: Rc<RefCell<Geometry>>,
    ) -> Self {
        BlockRun {
            block: start.block.clone(),
            kind: start.kind.clone(),
            title_default: start.title.clone(),
            role: format!("plugin.{}.{}", start.plugin, start.block),
            handlers,
            state,
            cx,
            geometry,
            surface: start.surface.clone(),
            doc: Doc::new(),
            seq: 0,
            title: None,
            saved: start.saved.clone(),
            limit: APC_LIMIT,
            next_chunk: 1,
            input: Parser::new(),
            keys: Decoder::new(),
            flush_at: None,
        }
    }

    fn message(&mut self, verb: &str, params: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
        let chunk = if body.len() > self.limit {
            let id = base36(self.next_chunk);
            self.next_chunk += 1;
            id
        } else {
            String::new()
        };
        tern_sdk::wire::encode(verb, params, body, self.limit, &chunk)
    }

    fn json_message(&mut self, verb: &str, body: &Json) -> Vec<u8> {
        let text = body.to_string();
        self.message(verb, &[], text.as_bytes())
    }

    /// The first bytes of a run: the hello (no features), closing the previous run's surface when
    /// the block restarted in this pane, and the screen surface.
    pub(crate) fn opening(&mut self, previous: Option<&str>) -> Vec<u8> {
        let hello = json!({"q": "hello", "v": [1], "app": self.kind});
        let mut out = self.json_message("q", &hello);
        if let Some(prev) = previous {
            out.extend(self.json_message("x", &json!({"id": prev, "keep": false})));
        }
        let open = json!({"id": self.surface, "mode": "screen", "title": self.title_default, "role": self.role});
        out.extend(self.json_message("o", &open));
        out
    }

    /// Closes the surface when the block exits; `main` stays in the pane.
    pub(crate) fn closing(&mut self) -> Vec<u8> {
        let close = json!({"id": self.surface, "keep": true});
        self.json_message("x", &close)
    }

    /// Diffs `next` against the last view sent and writes one frame (nothing when unchanged).
    pub(crate) fn render(&mut self, next: Doc) -> Vec<u8> {
        let ops = self.doc.ops(&next, &self.surface);
        self.doc = next;
        if ops.is_empty() {
            return Vec::new();
        }
        self.seq += 1;
        let ops = serde_json::to_value(&ops).unwrap_or(Json::Array(Vec::new()));
        let body = json!({"sf": self.surface, "s": self.seq, "ops": ops});
        self.json_message("f", &body)
    }

    /// `cx:frame(ops)`: raw ops as one frame, outside the diff.
    pub(crate) fn raw_frame(&mut self, ops: Vec<Json>) -> Vec<u8> {
        self.seq += 1;
        let body = json!({"sf": self.surface, "s": self.seq, "ops": ops});
        self.json_message("f", &body)
    }

    /// `cx:blob(bytes, mime)`. A `mime` that isn't a plain `type/subtype` token is left out
    /// (the terminal sniffs the bytes), so it can't break the APC parameters or end the string.
    pub(crate) fn blob(&mut self, bytes: &[u8], mime: &str) -> Vec<u8> {
        use base64::Engine as _;
        let body = base64::engine::general_purpose::STANDARD.encode(bytes);
        let params: &[(&str, &str)] = if is_mime_token(mime) {
            &[("mime", mime)]
        } else {
            &[]
        };
        self.message("b", params, body.as_bytes())
    }

    /// A new title from `title`: the pane's OSC 2 title, when it changed.
    pub(crate) fn retitle(&mut self, title: &str) -> Option<Vec<u8>> {
        if self.title.as_deref() == Some(title) {
            return None;
        }
        self.title = Some(title.to_owned());
        let clean: String = title.chars().filter(|c| !c.is_control()).collect();
        Some(format!("\x1b]2;{clean}\x1b\\").into_bytes())
    }

    /// Feeds the pane's input. Keys and view events come back; replies set the chunk limit.
    pub(crate) fn feed(&mut self, bytes: &[u8], flush_at: Instant) -> Vec<BlockInput> {
        let items = self.input.feed(bytes);
        let out = self.items(items);
        self.flush_at = (self.input.pending() || self.keys.pending()).then_some(flush_at);
        out
    }

    /// Releases undecided bytes (a lone ESC is Escape).
    pub(crate) fn flush(&mut self) -> Vec<BlockInput> {
        self.flush_at = None;
        let items = self.input.flush();
        let mut out = self.items(items);
        out.extend(self.keys.flush().into_iter().map(BlockInput::Key));
        out
    }

    fn items(&mut self, items: Vec<Item>) -> Vec<BlockInput> {
        let mut out = Vec::new();
        for item in items {
            match item {
                Item::Keys(bytes) => {
                    out.extend(self.keys.feed(&bytes).into_iter().map(BlockInput::Key));
                }
                Item::Reply(Reply::Hello(hello)) => {
                    if hello.apc > 0 {
                        self.limit = hello.apc;
                    }
                }
                Item::Event(event) => {
                    // Acks are dropped: a block writes without waiting for credits (§9.5, D25).
                    if !matches!(event, Event::Ack(_) | Event::Resize(_)) {
                        out.push(BlockInput::Event(event.raw().clone()));
                    }
                }
                Item::Reply(_) | Item::Da1 => {}
            }
        }
        out
    }
}

/// Whether `mime` is a plain `type/subtype` (RFC 6838 restricted names, no parameters).
fn is_mime_token(mime: &str) -> bool {
    let name = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$&^_.+-".contains(&b))
    };
    mime.split_once('/')
        .is_some_and(|(ty, sub)| name(ty) && name(sub))
}
