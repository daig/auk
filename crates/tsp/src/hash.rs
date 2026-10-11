//! The replicated-state hash: BLAKE3 over a canonical serialization of everything every
//! replica of a pane must agree on.
//!
//! Covered: the replicated config, the answer gate, the program hello, the title state, the next
//! anchor key, the chunk joiner, and every surface in pane order (which decides adoption and
//! retention) with its mode, buffer, state, listen, title, role, anchor, suspension, last `s`,
//! palette, sheets, focus and document (ids, kinds, settle hints, props with object keys sorted).
//!
//! Not covered: client facts, acks and `resize` bookkeeping, the output queue, held `send`s,
//! pending `reveal`/`scroll` requests (view commands), blobs (content-addressed storage outside
//! the documents), replica-local surface keys, and the values of age props, which a snapshot
//! rewrites to snapshot time and which replicas may disagree on.

use serde_json::Value;

use crate::doc::NodeRef;
use crate::pane::{TitleSource, TspPane};
use crate::surface::{State, Surface};

/// Props that carry ages, by kind: `(kind, prop)` at the top level, and `agent`'s nested ones.
pub const AGE_PROPS: [(&str, &str); 3] = [("elapsed", "age"), ("tool", "age"), ("block", "age")];
/// `agent` props whose `age` field is an age.
pub const AGENT_AGE_OBJECTS: [&str; 3] = ["stats", "tool", "retry"];

struct Canon(blake3::Hasher);

impl Canon {
    fn tag(&mut self, t: u8) {
        self.0.update(&[t]);
    }

    fn u64(&mut self, v: u64) {
        self.0.update(&v.to_le_bytes());
    }

    fn bool(&mut self, v: bool) {
        self.0.update(&[u8::from(v)]);
    }

    fn str(&mut self, s: &str) {
        self.u64(s.len() as u64);
        self.0.update(s.as_bytes());
    }

    fn opt_str(&mut self, s: Option<&str>) {
        self.bool(s.is_some());
        if let Some(s) = s {
            self.str(s);
        }
    }

    fn opt_u64(&mut self, v: Option<u64>) {
        self.bool(v.is_some());
        if let Some(v) = v {
            self.u64(v);
        }
    }

    fn strs<'a>(&mut self, items: impl ExactSizeIterator<Item = &'a str>) {
        self.u64(items.len() as u64);
        for s in items {
            self.str(s);
        }
    }

    /// A JSON value with object keys sorted. `age` masks the value of an age field.
    fn value(&mut self, v: &Value, age: &dyn Fn(&str) -> bool) {
        match v {
            Value::Null => self.tag(0),
            Value::Bool(b) => {
                self.tag(1);
                self.bool(*b);
            }
            Value::Number(n) => {
                self.tag(2);
                self.str(&n.to_string());
            }
            Value::String(s) => {
                self.tag(3);
                self.str(s);
            }
            Value::Array(a) => {
                self.tag(4);
                self.u64(a.len() as u64);
                for item in a {
                    self.value(item, &|_| false);
                }
            }
            Value::Object(o) => {
                self.tag(5);
                self.u64(o.len() as u64);
                let mut keys: Vec<&String> = o.keys().collect();
                keys.sort();
                for k in keys {
                    self.str(k);
                    if age(k) {
                        self.tag(6);
                    } else {
                        self.value(&o[k.as_str()], &|_| false);
                    }
                }
            }
        }
    }

    fn node(&mut self, n: NodeRef<'_>) {
        self.str(n.id());
        self.str(n.kind());
        self.bool(n.settled());
        let kind = n.kind();
        self.tag(5);
        let props = n.props();
        self.u64(props.len() as u64);
        let mut keys: Vec<&String> = props.keys().collect();
        keys.sort();
        for k in keys {
            self.str(k);
            let v = &props[k.as_str()];
            if AGE_PROPS.contains(&(kind, k.as_str())) {
                self.tag(6);
            } else if kind == "agent" && AGENT_AGE_OBJECTS.contains(&k.as_str()) {
                self.value(v, &|f| f == "age");
            } else {
                self.value(v, &|_| false);
            }
        }
        self.u64(n.child_count() as u64);
        for c in n.children() {
            self.node(c);
        }
    }

    fn surface(&mut self, s: &Surface) {
        self.str(&s.id);
        self.str(s.mode.as_str());
        self.str(s.buffer.as_str());
        self.bool(s.state == State::Live);
        self.bool(s.listen);
        self.opt_str(s.title.as_deref());
        self.opt_str(s.role.as_deref());
        self.opt_u64(s.anchor.map(|a| a.0));
        self.bool(s.suspended);
        self.bool(s.restored);
        self.opt_u64(s.last_s);
        match &s.palette {
            None => self.bool(false),
            Some(p) => {
                self.bool(true);
                for variant in [&p.dark, &p.light] {
                    self.u64(variant.len() as u64);
                    for (k, v) in variant {
                        self.str(k);
                        self.str(v);
                    }
                }
                match &p.name {
                    None => self.bool(false),
                    Some(n) => {
                        self.bool(true);
                        self.value(n, &|_| false);
                    }
                }
            }
        }
        let sheets: Vec<(&str, &str)> = s.sheets.iter().collect();
        self.u64(sheets.len() as u64);
        for (name, css) in sheets {
            self.str(name);
            self.str(css);
        }
        self.opt_str(s.doc.focus());
        self.node(s.doc.root());
    }
}

impl TspPane {
    /// A hash of all replicated TSP state; equal on every replica that saw the same stream, and
    /// after replaying a snapshot into a fresh engine with the same config. Platform-independent.
    pub fn state_hash(&self) -> [u8; 32] {
        let mut h = Canon(blake3::Hasher::new());
        h.0.update(b"auk-tsp state v1");
        let c = &self.config;
        h.bool(c.enabled);
        h.strs(c.kinds.iter().map(String::as_str));
        h.strs(c.features.iter().map(String::as_str));
        h.u64(u64::from(c.apc));
        h.u64(u64::from(c.credits));
        h.u64(c.retention);
        h.str(&c.term);
        h.str(&c.version);

        h.str(self.gate.as_str());
        match &self.hello {
            None => h.bool(false),
            Some(hello) => {
                h.bool(true);
                h.opt_str(hello.app.as_deref());
                h.strs(hello.features.iter().map(String::as_str));
            }
        }
        match &self.title {
            TitleSource::Grid => h.tag(0),
            TitleSource::Surface(t) => {
                h.tag(1);
                h.opt_str(t.as_deref());
            }
        }
        h.bool(self.command_seen);
        h.u64(self.next_anchor);

        match self.joiner.pending() {
            None => h.bool(false),
            Some(seq) => {
                h.bool(true);
                h.str(&seq.verb);
                h.str(&seq.id);
                h.u64(seq.params.len() as u64);
                for (k, v) in &seq.params {
                    h.str(k);
                    h.str(v);
                }
                h.u64(seq.body.len() as u64);
                h.0.update(&seq.body);
            }
        }
        match self.joiner.swallowing() {
            None => h.bool(false),
            Some((verb, id)) => {
                h.bool(true);
                h.str(verb);
                h.str(id);
            }
        }

        h.u64(self.surfaces.len() as u64);
        for s in &self.surfaces {
            h.surface(s);
        }
        *h.0.finalize().as_bytes()
    }
}
