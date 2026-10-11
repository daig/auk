//! The TSP part of a pane snapshot: a byte stream replayed through the normal parser
//! that rebuilds the engine's replicated state.
//!
//! Order: the hello; each surface, closed ones first, as a synthetic `o` (with the reserved
//! parameters `key`, `buf`, `seeded` and `at`, its place in pane order, restored at the end of
//! the replay, plus `restored=1` for a surface that came back after a daemon restart);
//! its `t` and `s` sheets; one frame with the last `s` that
//! adds the whole document, then `settle`s, `focus` and `suspend`; and `x keep:true` for closed
//! surfaces. A chunk sequence being joined is replayed last, so the replica keeps joining it.
//!
//! Auk's own replica state rides on the hello: `seeded=1` marks it, and its `auk` field carries
//! the answer gate, the next anchor key, the title state and an over-size sequence being
//! swallowed. Replicas honor `seeded` and the reserved `o` parameters only between
//! [`TspPane::begin_replay`] and [`TspPane::end_replay`].

use serde_json::{Map, Value, json};

use crate::doc::NodeRef;
use crate::pane::{TitleSource, TspPane};
use crate::surface::{Mode, State, Surface};
use crate::wire::{base36, chunks, encode_chunked, push_apc};

/// Rewrites a node's props for the snapshot (the replying replica rewrites ages to snapshot
/// time).
pub type Rewrite<'a> = dyn FnMut(&Surface, NodeRef<'_>, &mut Map<String, Value>) + 'a;

struct Writer {
    out: Vec<u8>,
    limit: usize,
    next_chunk: u64,
}

impl Writer {
    fn message(&mut self, verb: &str, params: &[(&str, &str)], body: &[u8]) {
        let chunk = if body.len() > self.limit {
            self.next_chunk += 1;
            base36(self.next_chunk - 1)
        } else {
            String::new()
        };
        encode_chunked(&mut self.out, verb, params, body, self.limit, &chunk);
    }

    fn json(&mut self, verb: &str, params: &[(&str, &str)], body: &Value) {
        self.message(verb, params, body.to_string().as_bytes());
    }
}

impl TspPane {
    /// The snapshot bytes, props as stored.
    pub fn snapshot(&self) -> Vec<u8> {
        self.snapshot_with(&mut |_, _, _| {})
    }

    /// The snapshot bytes, each node's props passed through `rewrite` first.
    pub fn snapshot_with(&self, rewrite: &mut Rewrite<'_>) -> Vec<u8> {
        let mut w = Writer {
            out: Vec::new(),
            limit: self.config.chunk_size() as usize,
            next_chunk: 1,
        };
        w.json("q", &[("seeded", "1")], &self.seed_hello());

        // Closed surfaces first; `at` restores pane order at the end of the replay.
        let indexed = self.surfaces.iter().enumerate();
        let closed = indexed.clone().filter(|(_, s)| s.state == State::Closed);
        let live = indexed.filter(|(_, s)| s.state == State::Live);
        for (at, s) in closed.chain(live) {
            self.write_surface(&mut w, s, at, rewrite);
        }

        if let Some(seq) = self.joiner.pending() {
            let mut first = true;
            let mut write = |piece: &[u8]| {
                let mut params: Vec<(&str, &str)> = if first {
                    seq.params
                        .iter()
                        .map(|(k, v)| (k.as_str(), v.as_str()))
                        .collect()
                } else {
                    Vec::new()
                };
                params.push(("c", &seq.id));
                params.push(("m", "1"));
                push_apc(&mut w.out, &seq.verb, &params, piece);
                first = false;
            };
            if seq.body.is_empty() {
                write(&[]);
            }
            for piece in chunks(&seq.body, w.limit) {
                write(piece);
            }
        }
        w.out
    }

    fn seed_hello(&self) -> Value {
        let (app, features) = match &self.hello {
            Some(h) => (h.app.clone(), h.features.clone()),
            None => (None, Vec::new()),
        };
        let title = match &self.title {
            TitleSource::Grid => Value::Null,
            TitleSource::Surface(t) => json!({"surface": t}),
        };
        let swallow = self.joiner.swallowing().map(|(v, c)| json!([v, c]));
        let mut body = json!({"q": "hello", "v": [1]});
        if let Some(app) = app {
            body["app"] = app.into();
        }
        body["features"] = features.into();
        body["auk"] = json!({
            "program": self.hello.is_some(),
            "gate": self.gate.as_str(),
            "anchor": self.next_anchor,
            "command": self.command_seen,
            "title": title,
            "swallow": swallow,
        });
        body
    }

    fn write_surface(&self, w: &mut Writer, s: &Surface, at: usize, rewrite: &mut Rewrite<'_>) {
        let key = s.anchor.map(|k| k.0.to_string());
        let at = at.to_string();
        let mut params: Vec<(&str, &str)> = Vec::new();
        if let Some(key) = &key {
            params.push(("key", key));
        }
        params.push(("buf", s.buffer.as_str()));
        params.push(("seeded", "1"));
        params.push(("at", &at));
        if s.restored {
            params.push(("restored", "1"));
        }
        let mut open = json!({"id": s.id, "mode": s.mode.as_str()});
        if let Some(title) = &s.title {
            open["title"] = title.as_str().into();
        }
        if let Some(role) = &s.role {
            open["role"] = role.as_str().into();
        }
        if !s.listen {
            open["listen"] = false.into();
        }
        w.json("o", &params, &open);

        if let Some(palette) = &s.palette {
            w.json("t", &[], &palette.to_json(&s.id));
        }
        for (name, css) in s.sheets.iter() {
            w.json("s", &[], &json!({"sf": s.id, "name": name, "css": css}));
        }

        let doc = &s.doc;
        let mut ops: Vec<Value> = doc
            .root()
            .children()
            .map(|region| {
                let node = region.to_json_with(&mut |n, props| rewrite(s, n, props));
                json!(["add", region.id(), s.id, null, node])
            })
            .collect();
        ops.extend(doc.settled().into_iter().map(|id| json!(["settle", id])));
        if let Some(focus) = doc.focus() {
            ops.push(json!(["focus", focus]));
        }
        if s.suspended {
            ops.push(json!(["suspend"]));
        }
        if !ops.is_empty() || s.last_s.is_some() {
            let mut frame = json!({"sf": s.id});
            if let Some(last) = s.last_s {
                frame["s"] = last.into();
            }
            frame["ops"] = Value::Array(ops);
            w.json("f", &[], &frame);
        }

        if s.state == State::Closed && s.mode != Mode::Screen {
            w.json("x", &[], &json!({"id": s.id, "keep": true}));
        }
    }
}
