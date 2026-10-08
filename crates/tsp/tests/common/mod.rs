//! Shared test harness: a simple grid, APC framing helpers, and decoding of what the engine sends
//! with the Rust tern-sdk's own input parser (the program side's oracle).

#![allow(dead_code)]

use std::collections::BTreeMap;

use auk_tsp::{AnchorError, AnchorKey, Config, Grid, Line, Outgoing, ToProgram, TspPane};
use serde_json::{Map, Value};
use tern_sdk::input::{Item, Parser};
use tern_sdk::wire::{Event, Reply};

/// A grid with just enough behavior for anchors: each anchor takes the next row, and a test
/// decides whether something was written below it.
#[derive(Debug, Default)]
pub struct TestGrid {
    pub anchors: BTreeMap<AnchorKey, Line>,
    pub undone: Vec<AnchorKey>,
    pub dropped: Vec<AnchorKey>,
    pub alt: bool,
    pub cols: u16,
    next_line: Line,
    /// Anchors with output written below them (the prompt rule can't undo those).
    pub written_below: Vec<AnchorKey>,
}

impl TestGrid {
    pub fn new() -> Self {
        TestGrid {
            cols: 80,
            ..Default::default()
        }
    }

    /// Output went to the grid: every anchor now has something below it.
    pub fn write(&mut self) {
        self.next_line += 1;
        self.written_below.extend(self.anchors.keys().copied());
    }
}

impl Grid for TestGrid {
    fn open_anchor(&mut self, key: AnchorKey) -> Result<Line, AnchorError> {
        if self.alt {
            return Err(AnchorError::AltScreen);
        }
        if self.anchors.contains_key(&key) {
            return Err(AnchorError::DuplicateKey);
        }
        let line = self.next_line;
        self.next_line += 1;
        self.anchors.insert(key, line);
        Ok(line)
    }

    fn undo_anchor(&mut self, key: AnchorKey) -> bool {
        if self.written_below.contains(&key) || self.anchors.remove(&key).is_none() {
            return false;
        }
        self.undone.push(key);
        true
    }

    fn drop_anchor(&mut self, key: AnchorKey) {
        if self.anchors.remove(&key).is_some() {
            self.dropped.push(key);
        }
    }

    fn anchor_line(&self, key: AnchorKey) -> Option<Line> {
        self.anchors.get(&key).copied()
    }

    fn alt_screen(&self) -> bool {
        self.alt
    }

    fn cols(&self) -> u16 {
        self.cols
    }
}

/// The payloads (after `ESC _`) of every APC string in `bytes`.
pub fn apc_payloads(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(start) = find(&bytes[at..], b"\x1b_") {
        let body = at + start + 2;
        let end = find(&bytes[body..], b"\x1b\\").expect("APC terminated by ST");
        out.push(bytes[body..body + end].to_vec());
        at = body + end + 2;
    }
    out
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// `tsp;<verb>;<params>;<body>` as a payload.
pub fn payload(verb: &str, params: &[(&str, &str)], body: &str) -> Vec<u8> {
    let mut out = format!("tsp;{verb};").into_bytes();
    for (k, v) in params {
        out.extend_from_slice(format!("{k}={v};").as_bytes());
    }
    out.extend_from_slice(body.as_bytes());
    out
}

/// What the program reads back, decoded by the SDK's parser.
#[derive(Debug)]
pub enum Back {
    Reply(Reply),
    Event(Event),
}

/// Decodes the engine's output with the SDK's input parser, checking each message is one ST-ended
/// APC string that the SDK reads whole.
pub fn decode(out: &[Outgoing]) -> Vec<Back> {
    let mut parser = Parser::new();
    let mut back = Vec::new();
    for msg in out {
        let bytes = msg.encode();
        assert!(
            bytes.starts_with(b"\x1b_tsp;") && bytes.ends_with(b"\x1b\\"),
            "one APC string"
        );
        assert!(!bytes[2..bytes.len() - 2].contains(&0x1b), "no ESC inside");
        assert!(!bytes.contains(&0x07), "no BEL");
        let prefix = match msg.verb {
            ToProgram::Reply => "\x1b_tsp;r;{",
            ToProgram::Event => "\x1b_tsp;e;{",
        };
        assert!(bytes.starts_with(prefix.as_bytes()), "no parameters");
        let items = parser.feed(&bytes);
        assert_eq!(items.len(), 1, "one item per message");
        back.push(match items.into_iter().next().unwrap() {
            Item::Reply(r) => Back::Reply(r),
            Item::Event(e) => Back::Event(e),
            other => panic!("not TSP: {other:?}"),
        });
    }
    back
}

/// A pane and its grid.
#[derive(Debug)]
pub struct Harness {
    pub pane: TspPane,
    pub grid: TestGrid,
}

impl Harness {
    pub fn new() -> Self {
        Self::with(Config::default())
    }

    pub fn with(config: Config) -> Self {
        Harness {
            pane: TspPane::new(config),
            grid: TestGrid::new(),
        }
    }

    /// Feeds one message.
    pub fn send(&mut self, verb: &str, body: &str) {
        self.pane.on_apc(&mut self.grid, &payload(verb, &[], body));
    }

    pub fn send_json(&mut self, verb: &str, body: Value) {
        self.send(verb, &body.to_string());
    }

    pub fn send_params(&mut self, verb: &str, params: &[(&str, &str)], body: &str) {
        self.pane
            .on_apc(&mut self.grid, &payload(verb, params, body));
    }

    /// Feeds program output bytes (APC strings only).
    pub fn write(&mut self, bytes: &[u8]) {
        for p in apc_payloads(bytes) {
            self.pane.on_apc(&mut self.grid, &p);
        }
    }

    /// The JSON bodies sent since the last call (decoded by the SDK as a check).
    pub fn out(&mut self) -> Vec<Value> {
        let out = self.pane.take_output();
        decode(&out);
        out.into_iter().map(|o| o.body).collect()
    }

    /// The hello query and its reply.
    pub fn hello(&mut self, features: &[&str]) -> Value {
        self.send_json(
            "q",
            serde_json::json!({"q": "hello", "v": [1], "app": "test", "features": features}),
        );
        let mut out = self.out();
        assert_eq!(out.len(), 1, "one reply");
        out.remove(0)
    }

    /// Opens a surface with a `main` region holding `children`.
    pub fn open(&mut self, id: &str, mode: &str, children: Value) {
        self.send_json("o", serde_json::json!({"id": id, "mode": mode}));
        self.frame(id, 1, serde_json::json!([["add", "main", id, null, {"id": "main", "k": "col", "c": children}]]));
    }

    pub fn frame(&mut self, sf: &str, s: u64, ops: Value) {
        self.send_json("f", serde_json::json!({"sf": sf, "s": s, "ops": ops}));
    }

    /// The live surface's document node as JSON.
    pub fn node(&self, sf: &str, id: &str) -> Option<Value> {
        let s = self.pane.surfaces().iter().rev().find(|s| s.id() == sf)?;
        s.document().get(id).map(|n| n.to_json())
    }

    /// Replays this pane's snapshot into a fresh engine with the same config and a grid holding
    /// the same anchors and screen, returning it.
    pub fn replay(&self) -> Harness {
        let mut fresh = Harness::with(self.pane.config().clone());
        fresh.grid.alt = self.grid.alt;
        fresh.grid.cols = self.grid.cols;
        fresh.grid.anchors = self.grid.anchors.clone();
        fresh.pane.begin_replay();
        fresh.write(&self.pane.snapshot());
        fresh.pane.end_replay();
        fresh
    }
}

/// Events named `ev` among `out`.
pub fn events<'a>(out: &'a [Value], ev: &str) -> Vec<&'a Value> {
    out.iter()
        .filter(|v| v.get("ev").and_then(Value::as_str) == Some(ev))
        .collect()
}

/// The object `v` as a map.
pub fn obj(v: &Value) -> &Map<String, Value> {
    v.as_object().expect("object")
}
