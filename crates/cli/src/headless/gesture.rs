//! A headless window's view state for a pane's surfaces: local folds and control flips, pointer
//! gestures resolved into events as a window resolves them, and which frames it
//! has acked.

use std::collections::BTreeMap;

use auk_replica::ViewEvent;
use auk_tsp::{Mods, NodeRef, Surface, SurfaceKey, TspPane};
use serde_json::Value;

/// A pointer gesture on a node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Gesture {
    Click,
    Double,
    /// A pick from the node's `menu` actions.
    Menu(String),
}

/// View state for one pane.
#[derive(Debug, Default)]
pub struct PaneView {
    /// Folds flipped locally: `(surface, node)` → collapsed.
    folds: BTreeMap<(SurfaceKey, String), bool>,
    /// `el` checkboxes and radios flipped locally: `(surface, node)` → checked.
    checked: BTreeMap<(SurfaceKey, String), bool>,
    /// The last frame acked per surface.
    acked: BTreeMap<SurfaceKey, u64>,
}

impl PaneView {
    /// Drops state for surfaces that ended.
    pub fn forget(&mut self, changes: &auk_replica::TspChanges) {
        for key in &changes.removed {
            self.acked.remove(key);
            self.folds.retain(|(k, _), _| k != key);
            self.checked.retain(|(k, _), _| k != key);
        }
    }

    /// The acks to send after drawing: each live, listening surface whose last frame changed (or
    /// all of them, for a new primary).
    pub fn acks(
        &mut self,
        pane: &TspPane,
        changes: &auk_replica::TspChanges,
        all: bool,
    ) -> Vec<(String, u64)> {
        let mut out = Vec::new();
        for s in pane.surfaces() {
            if !s.is_live() || !s.listens() {
                continue;
            }
            let Some(last) = s.last_s() else { continue };
            let changed = changes.surfaces.iter().any(|c| c.key == s.key());
            if all || changed || self.acked.get(&s.key()) != Some(&last) {
                self.acked.insert(s.key(), last);
                out.push((s.id().to_owned(), last));
            }
        }
        out
    }

    /// Resolves a gesture on `node` in surface `sf` into the event the program gets, if any,
    /// flipping local state first.
    pub fn gesture(
        &mut self,
        pane: &TspPane,
        sf: &str,
        node: &str,
        gesture: Gesture,
        mods: Mods,
    ) -> Result<Option<ViewEvent>, String> {
        let surface = pane
            .surfaces()
            .iter()
            .rev()
            .find(|s| s.id() == sf && s.is_live())
            .ok_or_else(|| format!("no live surface {sf}"))?;
        let doc = surface.document();
        let n = doc
            .get(node)
            .ok_or_else(|| format!("no node {node} in {sf}"))?;
        // Closed, suspended or `listen:false` surfaces are inert: nothing flips, nothing is sent.
        if !surface.interactive() {
            return Ok(None);
        }
        let key = surface.key();
        let actions = n.prop("actions");
        let action = match &gesture {
            Gesture::Click => actions.and_then(|a| a.get("click")).and_then(Value::as_str),
            Gesture::Double => actions
                .and_then(|a| a.get("dblclick"))
                .and_then(Value::as_str),
            Gesture::Menu(act) => {
                let listed = actions
                    .and_then(|a| a.get("menu"))
                    .and_then(Value::as_array)
                    .is_some_and(|m| m.iter().any(|x| x.as_str() == Some(act)));
                if !listed && act != "copy" {
                    return Err(format!("{node} has no menu action {act}"));
                }
                Some(act.as_str())
            }
        };
        let mods = if matches!(gesture, Gesture::Menu(_)) {
            Mods::NONE
        } else {
            mods
        };
        let values = |checked: &BTreeMap<(SurfaceKey, String), bool>, id: &str| {
            doc.form_values(id, &|n| checked.get(&(key, n.to_owned())).copied())
        };

        match action {
            Some("toggle") => {
                let was = self
                    .folds
                    .get(&(key, node.to_owned()))
                    .copied()
                    .unwrap_or_else(|| collapsed(n));
                self.folds.insert((key, node.to_owned()), !was);
                return Ok(Some(ViewEvent::Toggle {
                    id: node.to_owned(),
                    collapsed: !was,
                    key: None,
                }));
            }
            Some("copy" | "open" | "zoom") => return Ok(None),
            Some(act @ ("select" | "activate")) => {
                let (id, item) = list_and_item(n);
                let values = values(&self.checked, node);
                return Ok(Some(if act == "select" {
                    ViewEvent::Select { id, item, values }
                } else {
                    ViewEvent::Activate { id, item, values }
                }));
            }
            Some(act) => {
                return Ok(Some(ViewEvent::Action {
                    id: node.to_owned(),
                    action: act.to_owned(),
                    value: auk_tsp::ActValue::Split,
                    mods,
                    values: values(&self.checked, node),
                }));
            }
            None => {}
        }

        // Kinds that send events without an `actions` prop.
        if let Some(control) = el_control(n) {
            if gesture != Gesture::Click || truthy(n.prop("disabled")) {
                return Ok(None);
            }
            let was = self
                .checked
                .get(&(key, node.to_owned()))
                .copied()
                .unwrap_or_else(|| truthy(n.prop("checked")));
            let name = n.prop("name").and_then(Value::as_str).map(str::to_owned);
            let now = match control {
                Control::Checkbox => !was,
                Control::Radio => {
                    if let Some(name) = &name {
                        for other in radios(surface, name) {
                            self.checked.insert((key, other), false);
                        }
                    }
                    true
                }
            };
            self.checked.insert((key, node.to_owned()), now);
            let value = n
                .prop("value")
                .cloned()
                .unwrap_or_else(|| Value::String("on".into()));
            return Ok(Some(ViewEvent::Change {
                id: node.to_owned(),
                value,
                checked: now,
                name,
                values: values(&self.checked, node),
            }));
        }
        Ok(match (n.kind(), &gesture) {
            ("item", Gesture::Click) | ("tabs", Gesture::Click) => {
                let (id, item) = list_and_item(n);
                Some(ViewEvent::Select {
                    id,
                    item,
                    values: None,
                })
            }
            ("item", Gesture::Double) => {
                let (id, item) = list_and_item(n);
                Some(ViewEvent::Activate {
                    id,
                    item,
                    values: None,
                })
            }
            ("editor" | "input", Gesture::Click) => Some(ViewEvent::Focus {
                id: node.to_owned(),
            }),
            _ => None,
        })
    }
}

/// The body a client sends for an event (the program's body, which also carries `sf`).
pub fn event_json(event: &ViewEvent) -> Value {
    let mut body = event.body("");
    if let Some(obj) = body.as_object_mut() {
        obj.remove("sf");
    }
    body
}

enum Control {
    Checkbox,
    Radio,
}

fn el_control(n: NodeRef<'_>) -> Option<Control> {
    if n.kind() != "el" || n.prop("tag").and_then(Value::as_str) != Some("input") {
        return None;
    }
    match n.prop("type").and_then(Value::as_str) {
        Some("checkbox") => Some(Control::Checkbox),
        Some("radio") => Some(Control::Radio),
        _ => None,
    }
}

fn radios(surface: &Surface, name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![surface.document().root()];
    while let Some(n) = stack.pop() {
        if matches!(el_control(n), Some(Control::Radio))
            && n.prop("name").and_then(Value::as_str) == Some(name)
        {
            out.push(n.id().to_owned());
        }
        stack.extend(n.children());
    }
    out
}

/// For a list item, the list and the item; otherwise the node itself as both.
fn list_and_item(n: NodeRef<'_>) -> (String, String) {
    match n.parent() {
        Some(p) if n.kind() == "item" && p.kind() == "list" => {
            (p.id().to_owned(), n.id().to_owned())
        }
        _ => (n.id().to_owned(), n.id().to_owned()),
    }
}

fn collapsed(n: NodeRef<'_>) -> bool {
    truthy(n.prop("collapsed"))
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) | Some(Value::Bool(false)) => false,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use auk_tsp::Config;
    use serde_json::json;

    fn pane_with(nodes: Value) -> TspPane {
        let mut grid = auk_term::Terminal::new(80, 24, auk_term::Config::default());
        let mut pane = TspPane::new(Config::default());
        let msg = |verb: &str, body: Value| format!("tsp;{verb};{body}").into_bytes();
        pane.on_apc(&mut grid, &msg("o", json!({"id": "s1", "mode": "flow"})));
        pane.on_apc(
            &mut grid,
            &msg(
                "f",
                json!({"sf": "s1", "s": 1, "ops": [["add", "main", "s1", null, {"id": "main", "k": "col", "c": nodes}]]}),
            ),
        );
        pane
    }

    #[test]
    fn form_controls_flip_and_carry_values() {
        let form = json!([{"id": "f", "k": "el", "p": {"tag": "form"}, "c": [
            {"id": "a", "k": "el", "p": {"tag": "input", "type": "radio", "name": "size", "value": "s", "checked": true}},
            {"id": "b", "k": "el", "p": {"tag": "input", "type": "radio", "name": "size", "value": "l"}},
            {"id": "c", "k": "el", "p": {"tag": "input", "type": "checkbox", "name": "ok"}},
            {"id": "go", "k": "el", "p": {"tag": "button", "actions": {"click": "submit"}}},
        ]}]);
        let pane = pane_with(form);
        let mut v = PaneView::default();
        let ev = v
            .gesture(&pane, "s1", "b", Gesture::Click, Mods::NONE)
            .unwrap()
            .unwrap();
        assert_eq!(
            event_json(&ev),
            json!({"ev": "change", "id": "b", "value": "l", "checked": true, "name": "size", "values": {"size": "l", "ok": false}})
        );
        let ev = v
            .gesture(
                &pane,
                "s1",
                "go",
                Gesture::Click,
                Mods {
                    shift: true,
                    ..Mods::NONE
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            event_json(&ev),
            json!({"ev": "action", "id": "go", "act": "submit", "mods": ["shift"], "values": {"size": "l", "ok": false}})
        );
    }
}
