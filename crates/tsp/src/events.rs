//! Terminal-to-program messages (§7.7): the outgoing queue's entries and the view events a client
//! originates.

use serde_json::{Map, Value};

use crate::wire::{ToProgram, encode_to_program};

/// One message for the program: a reply or an event, written to the PTY as one APC string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outgoing {
    pub verb: ToProgram,
    pub body: Value,
}

impl Outgoing {
    /// `ESC _ tsp;<r|e>;<json> ESC \`.
    pub fn encode(&self) -> Vec<u8> {
        encode_to_program(self.verb, &self.body)
    }

    /// The event name (`ev`) or reply name (`r`).
    pub fn name(&self) -> &str {
        let key = match self.verb {
            ToProgram::Reply => "r",
            ToProgram::Event => "ev",
        };
        self.body
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
    }
}

/// Modifiers held during a pointer action.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub meta: bool,
}

impl Mods {
    pub const NONE: Mods = Mods {
        shift: false,
        ctrl: false,
        alt: false,
        meta: false,
    };

    /// The names held, in the order shift, ctrl, alt, meta.
    pub fn names(self) -> Vec<&'static str> {
        [
            (self.shift, "shift"),
            (self.ctrl, "ctrl"),
            (self.alt, "alt"),
            (self.meta, "meta"),
        ]
        .into_iter()
        .filter_map(|(on, name)| on.then_some(name))
        .collect()
    }
}

/// An event a client originates from the view (§7.7, §7.8, §7.9). Keys are never events: typed
/// text reaches the program as PTY bytes; only `Edit`, `Undo` and `Send` stand in for keys, each
/// behind its program feature.
#[derive(Clone, Debug, PartialEq)]
pub enum ViewEvent {
    /// A fold flipped locally. `key` defaults to the node's `key` prop.
    Toggle {
        id: String,
        collapsed: bool,
        key: Option<Value>,
    },
    /// A list item was picked (`id` is the list).
    Select {
        id: String,
        item: String,
        values: Option<Map<String, Value>>,
    },
    /// A list item was opened.
    Activate {
        id: String,
        item: String,
        values: Option<Map<String, Value>>,
    },
    /// A custom pointer action. `action` is the name as the node's `actions` prop gives it
    /// (`act` or `act=value`); a menu pick passes no modifiers.
    Action {
        id: String,
        action: String,
        mods: Mods,
        values: Option<Map<String, Value>>,
    },
    /// An `el` checkbox or radio flipped.
    Change {
        id: String,
        value: Value,
        checked: bool,
        name: Option<String>,
        values: Option<Map<String, Value>>,
    },
    /// A `prefs` row changed.
    PrefsChange {
        id: String,
        item: String,
        value: Value,
    },
    /// A click asked for the keys.
    Focus { id: String },
    /// Native editing replaced UTF-16 `[from, to)` with `text` (feature `edit`).
    Edit {
        id: String,
        from: u64,
        to: u64,
        text: String,
        cursor: u64,
        len: u64,
    },
    /// The undo key in a natively edited field (features `edit` and `undo`).
    Undo { id: String },
    /// Submit `text` into a field reporting `sendable:true` (feature `send`).
    Send { id: String, text: String },
}

impl ViewEvent {
    /// The node the event is about.
    pub fn id(&self) -> &str {
        match self {
            ViewEvent::Toggle { id, .. }
            | ViewEvent::Select { id, .. }
            | ViewEvent::Activate { id, .. }
            | ViewEvent::Action { id, .. }
            | ViewEvent::Change { id, .. }
            | ViewEvent::PrefsChange { id, .. }
            | ViewEvent::Focus { id }
            | ViewEvent::Edit { id, .. }
            | ViewEvent::Undo { id }
            | ViewEvent::Send { id, .. } => id,
        }
    }

    /// The program feature the event needs, if any.
    pub fn feature(&self) -> &'static [&'static str] {
        match self {
            ViewEvent::Edit { .. } => &["edit"],
            ViewEvent::Undo { .. } => &["edit", "undo"],
            ViewEvent::Send { .. } => &["send"],
            _ => &[],
        }
    }

    /// The event body for surface `sf`.
    pub(crate) fn body(&self, sf: &str) -> Value {
        let mut m = Map::new();
        let mut put = |k: &str, v: Value| {
            m.insert(k.to_owned(), v);
        };
        let s = |v: &str| Value::String(v.to_owned());
        let (ev, id) = match self {
            ViewEvent::Toggle { .. } => ("toggle", self.id()),
            ViewEvent::Select { .. } => ("select", self.id()),
            ViewEvent::Activate { .. } => ("activate", self.id()),
            ViewEvent::Action { .. } => ("action", self.id()),
            ViewEvent::Change { .. } | ViewEvent::PrefsChange { .. } => ("change", self.id()),
            ViewEvent::Focus { .. } => ("focus", self.id()),
            ViewEvent::Edit { .. } => ("edit", self.id()),
            ViewEvent::Undo { .. } => ("undo", self.id()),
            ViewEvent::Send { .. } => ("send", self.id()),
        };
        put("ev", s(ev));
        put("sf", s(sf));
        put("id", s(id));
        let values = |put: &mut dyn FnMut(&str, Value), values: &Option<Map<String, Value>>| {
            if let Some(v) = values {
                put("values", Value::Object(v.clone()));
            }
        };
        match self {
            ViewEvent::Toggle { collapsed, key, .. } => {
                put("collapsed", Value::Bool(*collapsed));
                if let Some(key) = key {
                    put("key", key.clone());
                }
            }
            ViewEvent::Select {
                item, values: v, ..
            }
            | ViewEvent::Activate {
                item, values: v, ..
            } => {
                put("item", s(item));
                values(&mut put, v);
            }
            ViewEvent::Action {
                action,
                mods,
                values: v,
                ..
            } => {
                let (act, value) = match action.split_once('=') {
                    Some((act, value)) => (act, Some(value)),
                    None => (action.as_str(), None),
                };
                put("act", s(act));
                if let Some(value) = value {
                    put("value", s(value));
                }
                let names = mods.names();
                if !names.is_empty() {
                    put("mods", Value::Array(names.into_iter().map(s).collect()));
                }
                values(&mut put, v);
            }
            ViewEvent::Change {
                value,
                checked,
                name,
                values: v,
                ..
            } => {
                put("value", value.clone());
                put("checked", Value::Bool(*checked));
                if let Some(name) = name {
                    put("name", s(name));
                }
                values(&mut put, v);
            }
            ViewEvent::PrefsChange { item, value, .. } => {
                put("item", s(item));
                put("value", value.clone());
            }
            ViewEvent::Focus { .. } | ViewEvent::Undo { .. } => {}
            ViewEvent::Edit {
                from,
                to,
                text,
                cursor,
                len,
                ..
            } => {
                put("from", Value::from(*from));
                put("to", Value::from(*to));
                put("text", s(text));
                put("cursor", Value::from(*cursor));
                put("len", Value::from(*len));
            }
            ViewEvent::Send { text, .. } => put("text", s(text)),
        }
        Value::Object(m)
    }
}

/// What became of a view event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    /// Queued for the program.
    Sent,
    /// A `send` waiting for its field to report `sendable:true`.
    Held,
}

/// Why a view event wasn't delivered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// No live surface with that id, or it doesn't listen.
    NotListening,
    /// The node isn't in the surface.
    UnknownNode,
    /// The program's hello didn't list the feature.
    Feature,
    /// The field doesn't take this (wrong kind, `readonly`, a Vim `mode`, not sendable).
    Field,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn action_splits_and_orders_mods() {
        let ev = ViewEvent::Action {
            id: "b".into(),
            action: "open=a=b".into(),
            mods: Mods {
                meta: true,
                shift: true,
                ..Mods::NONE
            },
            values: None,
        };
        assert_eq!(
            ev.body("s1"),
            json!({"ev": "action", "sf": "s1", "id": "b", "act": "open", "value": "a=b", "mods": ["shift", "meta"]})
        );
        let ev = ViewEvent::Action {
            id: "b".into(),
            action: "rerun".into(),
            mods: Mods::NONE,
            values: None,
        };
        assert_eq!(
            ev.body("s1"),
            json!({"ev": "action", "sf": "s1", "id": "b", "act": "rerun"})
        );
    }

    #[test]
    fn prefs_change_carries_sf() {
        let ev = ViewEvent::PrefsChange {
            id: "p".into(),
            item: "theme".into(),
            value: json!("dark"),
        };
        assert_eq!(
            ev.body("s1"),
            json!({"ev": "change", "sf": "s1", "id": "p", "item": "theme", "value": "dark"})
        );
    }
}
