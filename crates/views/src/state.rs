//! View state of one surface in one window (§7.8, research/elements.md §1.9): folds, control
//! flips, "N more lines" expansions, tree items, field selections and IME preedit. Never
//! replicated and never in snapshots.

use auk_tsp::{NodeRef, Reveal, Scroll};
use auk_ui::NodeId;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// A local flip of a controlling prop. Resending the same prop value keeps the user's choice; a
/// different value resets it (§7.8).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Flip {
    /// The prop value last seen.
    pub prop: Value,
    /// What the view shows.
    pub local: bool,
}

/// A field's native selection, in UTF-16 offsets (§7.9).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: u64,
    pub cursor: u64,
}

impl Selection {
    pub fn range(&self) -> (u64, u64) {
        (self.anchor.min(self.cursor), self.anchor.max(self.cursor))
    }
    pub fn is_empty(&self) -> bool {
        self.anchor == self.cursor
    }
}

/// IME composition drawn at a field's caret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preedit {
    pub id: String,
    pub text: String,
    /// The caret within the preedit text, in chars.
    pub cursor: usize,
}

#[derive(Debug, Default)]
pub(crate) struct SurfaceState {
    /// Folds of card, section, tool, agent and checklist phases, by state key.
    pub folds: HashMap<String, Flip>,
    /// Folds as last drawn, by state key: whether collapsed, and whether it is unfolding
    /// (opened since, drawn with the fold transition, §7.13).
    pub drawn_folds: HashMap<String, (bool, bool)>,
    /// The surface has been drawn once.
    pub drawn: bool,
    /// Overlays by node id: whether each opened after the first draw (and pops in).
    pub overlays: HashMap<String, bool>,
    /// `el` checkbox and radio flips, by node id.
    pub checks: HashMap<String, Flip>,
    /// Expanded previews ("N more lines"), by state key.
    pub more: HashMap<String, bool>,
    /// Tree items, by `(tree id, item id)`.
    pub tree: HashMap<(String, String), Flip>,
    /// The checklist hud popover, by node id.
    pub hud: HashMap<String, bool>,
    /// Native selections in fields, by node id.
    pub selection: HashMap<String, Selection>,
    pub preedit: Option<Preedit>,
    /// The latest `reveal` not yet applied.
    pub reveal: Option<Reveal>,
    pub scrolls: Vec<Scroll>,
    /// Elapsed/agent timers: the age value last seen and its anchor, by `id/field`.
    pub anchors: HashMap<String, (Value, f64)>,
    /// Status strips to fit after layout.
    pub fit: HashSet<String>,
    /// Overlays anchored to a node or a caret, and what they anchor to.
    pub anchored: HashSet<String>,
    pub anchor_targets: HashMap<String, String>,
    pub anchor_caret: HashSet<String>,
    /// Toasts the last render asked for.
    pub toasts: Vec<crate::kinds::layout::ToastReq>,
    /// Lists with `max`: their scroll element.
    pub scrollers: HashMap<String, NodeId>,
    /// Text cut after layout.
    pub truncs: HashMap<String, crate::kinds::data::Trunc>,
    /// Tables fitted after layout.
    pub tables: HashMap<String, crate::kinds::data::TableInfo>,
    /// `ansi` with `follow` and `max.h`: the clip that follows the tail, and where it was left.
    pub follow: HashMap<String, NodeId>,
    pub follow_at: HashMap<NodeId, f32>,
    /// Fields whose scroll the program owns (`topline`), and fields keeping the caret in view.
    pub topline: HashMap<String, (NodeId, f64)>,
    pub caret_follow: HashMap<String, (NodeId, Option<NodeId>)>,
    /// Virtualized containers: (element, estimated child height), by node id.
    pub virt: HashMap<String, (NodeId, f32)>,
    /// The window last given to each virtualized container.
    pub virt_window: HashMap<NodeId, (f32, f32)>,
}

/// The key local view state is remembered by: the node's `key` under the same parent, or its id
/// (research/elements.md §1.3).
pub(crate) fn state_key(node: NodeRef) -> String {
    let parent = node.parent().map(|p| p.id()).unwrap_or("");
    match node.prop("key") {
        Some(Value::String(k)) => format!("{parent}\u{1f}k{k}"),
        Some(Value::Number(n)) => format!("{parent}\u{1f}k{n}"),
        _ => format!("{parent}\u{1f}#{}", node.id()),
    }
}

impl SurfaceState {
    /// The shown state of a flip whose controlling prop is `prop` (`default` when absent).
    pub fn flip(
        map: &mut HashMap<String, Flip>,
        key: &str,
        prop: Option<&Value>,
        default: bool,
    ) -> bool {
        let pv = prop.cloned().unwrap_or(Value::Null);
        let from_prop = prop.and_then(Value::as_bool).unwrap_or(default);
        match map.get_mut(key) {
            Some(f) if f.prop == pv => f.local,
            Some(f) => {
                f.prop = pv;
                f.local = from_prop;
                from_prop
            }
            None => {
                map.insert(
                    key.to_owned(),
                    Flip {
                        prop: pv,
                        local: from_prop,
                    },
                );
                from_prop
            }
        }
    }

    /// Flip locally; returns the new state.
    pub fn toggle(map: &mut HashMap<String, Flip>, key: &str, current: bool) -> bool {
        let f = map.entry(key.to_owned()).or_insert(Flip {
            prop: Value::Null,
            local: current,
        });
        f.local = !current;
        f.local
    }

    pub fn fold(&mut self, node: NodeRef, default: bool) -> bool {
        let key = state_key(node);
        Self::flip(&mut self.folds, &key, node.prop("collapsed"), default)
    }

    /// Whether a fold drawn now as `collapsed` opened after it was drawn collapsed: it takes the
    /// fold transition (`.sf-unfold`) until it collapses again. A fold first drawn open doesn't.
    pub fn unfolding(&mut self, node: NodeRef, collapsed: bool) -> bool {
        self.unfolding_key(state_key(node), collapsed)
    }

    /// [`SurfaceState::unfolding`] for a fold known by `key` (a tree item).
    pub fn unfolding_key(&mut self, key: String, collapsed: bool) -> bool {
        let unfolding = match self.drawn_folds.get(&key) {
            Some(&(was, unf)) => !collapsed && (was || unf),
            None => false,
        };
        self.drawn_folds.insert(key, (collapsed, unfolding));
        unfolding
    }

    /// A node's state went away with it. Folds are kept by key so a re-added node keeps them.
    pub fn forget(&mut self, id: &str) {
        self.checks.remove(id);
        self.selection.remove(id);
        self.hud.remove(id);
        self.fit.remove(id);
        self.anchored.remove(id);
        self.anchor_targets.remove(id);
        self.anchor_caret.remove(id);
        self.scrollers.remove(id);
        self.truncs.remove(id);
        self.tables.remove(id);
        if let Some(c) = self.follow.remove(id) {
            self.follow_at.remove(&c);
        }
        self.topline.remove(id);
        self.caret_follow.remove(id);
        if let Some((el, _)) = self.virt.remove(id) {
            self.virt_window.remove(&el);
        }
        self.forget_virtual_under(id);
        self.overlays.remove(id);
        if self.preedit.as_ref().is_some_and(|p| p.id == id) {
            self.preedit = None;
        }
    }

    /// Forgets the virtualized containers a node registered under `<id>/…` (a tree's groups).
    pub fn forget_virtual_under(&mut self, id: &str) {
        let prefix = format!("{id}/");
        let gone: Vec<String> = self
            .virt
            .keys()
            .filter(|k| k.starts_with(&prefix))
            .cloned()
            .collect();
        for k in gone {
            if let Some((el, _)) = self.virt.remove(&k) {
                self.virt_window.remove(&el);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn same_value_keeps_the_users_choice() {
        let mut m = HashMap::new();
        assert!(SurfaceState::flip(&mut m, "a", Some(&json!(true)), false));
        assert!(!SurfaceState::toggle(&mut m, "a", true));
        // The program resends the same value: the user's choice stays.
        assert!(!SurfaceState::flip(&mut m, "a", Some(&json!(true)), false));
        // A different value resets it.
        assert!(!SurfaceState::flip(&mut m, "a", Some(&json!(false)), false));
        assert!(SurfaceState::flip(&mut m, "a", Some(&json!(true)), false));
    }
}
