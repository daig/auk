//! Focus and press states: `:focus`, `:focus-visible` and `:focus-within`
//! follow the pane's keyboard focus, and `:active` the `.sf-act` node a primary press is down on.
//! Like hover they are the window's own: never replicated, never in snapshots or HTML.
//!
//! What has focus, first match wins:
//! - nothing while the pane isn't the key window's focused pane (`.sf-unfocused`): as in a
//!   browser, a window without focus matches no `:focus*`. The node Tab reached is remembered
//!   and focused again when the pane is;
//! - a node Tab reached ([`PaneView::nav_key`]): focus-visible, as keyboard focus always is;
//! - the program's focused `editor` or `input` (`["focus", id]`) in the topmost live surface:
//!   focus-visible unless `readonly` (CSS: a field that takes text always shows its focus).
//!
//! A press never focuses what it presses (Safari's rule for buttons; the program moves field
//! focus itself through `focus` events), but it takes focus off a node Tab reached.
//!
//! Tab, ⇧Tab, Enter and Space reach the view only where no program reads the keys: a live
//! surface's program owns the pane's keyboard (every key, Tab included, goes to it through the
//! PTY), so [`PaneView::nav_key`] answers [`Outcome::Keys`] whenever one is live.

use crate::events::{Outcome, PointerEvent};
use crate::view::PaneView;
use auk_tsp::TspPane;
use auk_ui::{Document, ElementState, NodeId};

/// Keyboard navigation between focusable nodes (`tabindex`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavKey {
    /// Tab: the next focusable node.
    Next,
    /// ⇧Tab: the previous one.
    Previous,
    /// Enter or Space: the focused node's click.
    Activate,
}

/// The view's focus and press states.
#[derive(Debug, Default)]
pub(crate) struct FocusStates {
    /// Elements carrying focus states now: the focused element (`FOCUS`, maybe
    /// `FOCUS_VISIBLE`), then its ancestors up to its region (`FOCUS_WITHIN`).
    chain: Vec<NodeId>,
    /// The focused element shows `:focus-visible`.
    visible: bool,
    /// The element Tab reached, and the node it belongs to (its own or its nearest `data-id`),
    /// so a reused element id isn't taken for it.
    nav: Option<(NodeId, String)>,
    /// The element a primary press is down on (`ACTIVE`).
    active: Option<NodeId>,
}

const FOCUS_ALL: ElementState = ElementState(
    ElementState::FOCUS.0 | ElementState::FOCUS_VISIBLE.0 | ElementState::FOCUS_WITHIN.0,
);

/// The `data-id` of the node an element belongs to.
fn owner(doc: &Document, mut el: NodeId) -> Option<String> {
    loop {
        if let Some(id) = doc.attribute(el, "data-id") {
            return Some(id.to_owned());
        }
        if doc.attribute(el, "data-scope").is_some() {
            return None;
        }
        el = doc.parent(el)?;
    }
}

fn is_element(doc: &Document, el: NodeId) -> bool {
    doc.try_node(el).is_some_and(|n| n.is_element())
}

impl PaneView {
    /// Whether a program reads the pane's keys: any live surface's does.
    fn program_keys(pane: &TspPane) -> bool {
        pane.surfaces().iter().any(|s| s.is_live())
    }

    /// The element Tab reached, while it is still there and focusable.
    fn nav_target(&self, doc: &Document) -> Option<NodeId> {
        let (el, id) = self.focus.nav.as_ref()?;
        (is_element(doc, *el)
            && doc.attribute(*el, "tabindex").is_some()
            && owner(doc, *el).as_deref() == Some(id.as_str())
            && self.in_pane(doc, *el))
        .then_some(*el)
    }

    /// Whether an element is inside the pane container.
    fn in_pane(&self, doc: &Document, mut el: NodeId) -> bool {
        loop {
            if el == self.container {
                return true;
            }
            match doc.parent(el) {
                Some(p) => el = p,
                None => return false,
            }
        }
    }

    /// The focused element and whether it shows `:focus-visible`.
    fn focus_target(&mut self, doc: &Document, pane: &TspPane) -> Option<(NodeId, bool)> {
        let nav = self.nav_target(doc);
        if nav.is_none() {
            self.focus.nav = None;
        }
        if !self.focused {
            return None;
        }
        if let Some(el) = nav {
            return Some((el, true));
        }
        for s in pane.surfaces().iter().rev() {
            if !s.is_live() {
                continue;
            }
            let Some(id) = s.document().focus() else {
                continue;
            };
            let Some(node) = s.document().get(id) else {
                continue;
            };
            if !matches!(node.kind(), "editor" | "input") {
                continue;
            }
            let Some(el) = self.node_element(s.key(), id) else {
                continue;
            };
            if !is_element(doc, el) || !self.in_pane(doc, el) {
                continue;
            }
            return Some((el, !crate::common::bool_prop(node.props(), "readonly")));
        }
        None
    }

    /// Bring `:focus`, `:focus-visible` and `:focus-within` up to date: after a sync (focus
    /// moved, nodes went or were rebuilt), pane focus changes, presses and navigation.
    pub(crate) fn update_focus(&mut self, doc: &mut Document, pane: &TspPane) {
        let target = self.focus_target(doc, pane);
        let mut chain = Vec::new();
        if let Some((el, _)) = target {
            let mut cur = Some(el);
            while let Some(e) = cur {
                if doc.node(e).is_element() {
                    chain.push(e);
                    // The surface's region is the last: focus stays within its surface.
                    if doc.attribute(e, "data-scope").is_some() {
                        break;
                    }
                }
                cur = doc.parent(e);
            }
        }
        let visible = target.is_some_and(|(_, v)| v);
        if chain == self.focus.chain && visible == self.focus.visible {
            return;
        }
        // Old elements first (an id may have been reused by a new element: clearing what it
        // never had is a no-op), then exactly the states each new one should have.
        for &e in &self.focus.chain {
            if !chain.contains(&e) && is_element(doc, e) {
                doc.set_state(e, FOCUS_ALL, false);
            }
        }
        for (i, &e) in chain.iter().enumerate() {
            let first = i == 0;
            doc.set_state(e, ElementState::FOCUS, first);
            doc.set_state(e, ElementState::FOCUS_VISIBLE, first && visible);
            doc.set_state(e, ElementState::FOCUS_WITHIN, !first);
        }
        self.focus.chain = chain;
        self.focus.visible = visible;
    }

    /// A press takes focus off the node Tab reached.
    pub(crate) fn blur_nav(&mut self, doc: &mut Document, pane: &TspPane) {
        if self.focus.nav.take().is_some() {
            self.update_focus(doc, pane);
        }
    }

    /// A primary press down on `el`: `:active` on the `.sf-act` node it is in.
    pub(crate) fn press_state(&mut self, doc: &mut Document, el: NodeId) {
        let mut cur = Some(el);
        let mut act = None;
        while let Some(e) = cur {
            if doc.node(e).is_element() {
                if self.class_has(e, "sf-act") {
                    act = Some(e);
                    break;
                }
                if doc.attribute(e, "data-scope").is_some() {
                    break;
                }
            }
            cur = doc.parent(e);
        }
        if act != self.focus.active {
            self.release(doc);
        }
        if let Some(a) = act {
            doc.set_state(a, ElementState::ACTIVE, true);
            self.focus.active = Some(a);
        }
    }

    /// The press ended (the button came up, or the press was cancelled): `:active` goes.
    /// Returns whether anything changed.
    pub fn release(&mut self, doc: &mut Document) -> bool {
        let Some(a) = self.focus.active.take() else {
            return false;
        };
        let had = is_element(doc, a)
            && doc
                .node(a)
                .element()
                .is_some_and(|e| e.state.contains(ElementState::ACTIVE));
        if had {
            doc.set_state(a, ElementState::ACTIVE, false);
        }
        had
    }

    /// The elements carrying focus states: the focused element first, then its ancestors up to
    /// its surface's region. Empty when nothing has focus.
    pub fn focus_chain(&self) -> &[NodeId] {
        &self.focus.chain
    }

    /// Focusable elements (`tabindex`) of the pane in document order, skipping what isn't drawn
    /// and regions a modal sheet covers.
    fn focusables(&self, doc: &Document) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack = vec![self.container];
        while let Some(e) = stack.pop() {
            if !doc.node(e).is_element() {
                continue;
            }
            let shown = doc
                .computed_style(e)
                .is_some_and(|s| s.display != auk_ui::style::Display::None);
            if !shown || self.class_has(e, "sf-covered") {
                continue;
            }
            if doc
                .attribute(e, "tabindex")
                .and_then(|t| t.trim().parse::<i32>().ok())
                .is_some_and(|t| t >= 0)
            {
                out.push(e);
            }
            stack.extend(doc.children(e).iter().rev().copied());
        }
        out
    }

    /// Tab, ⇧Tab, Enter or Space in the pane. A live surface's program reads every key,
    /// so then (and when there is nothing to move to or press) the key is the program's:
    /// [`Outcome::Keys`]. Otherwise Tab moves focus between focusable nodes (wrapping) and Enter
    /// or Space press the focused node as a click would.
    pub fn nav_key(&mut self, doc: &mut Document, pane: &TspPane, key: NavKey) -> Vec<Outcome> {
        if Self::program_keys(pane) {
            return vec![Outcome::Keys];
        }
        let cur = self.nav_target(doc);
        match key {
            NavKey::Next | NavKey::Previous => {
                let all = self.focusables(doc);
                if all.is_empty() {
                    return vec![Outcome::Keys];
                }
                let at = cur.and_then(|c| all.iter().position(|&e| e == c));
                let n = all.len();
                let next = match (at, key == NavKey::Next) {
                    (Some(i), true) => (i + 1) % n,
                    (Some(i), false) => (i + n - 1) % n,
                    (None, true) => 0,
                    (None, false) => n - 1,
                };
                let el = all[next];
                let Some(id) = owner(doc, el) else {
                    return vec![Outcome::Keys];
                };
                self.focus.nav = Some((el, id));
                self.update_focus(doc, pane);
                vec![Outcome::Redraw]
            }
            NavKey::Activate => {
                let Some(el) = cur else {
                    return vec![Outcome::Keys];
                };
                let (x, y) = doc
                    .border_box(el)
                    .map_or((0.0, 0.0), |r| (r.x + r.w / 2.0, r.y + r.h / 2.0));
                let hits = self.hits_from(doc, el);
                let ev = PointerEvent {
                    x,
                    y,
                    clicks: 1,
                    ..PointerEvent::default()
                };
                let out = self.press_hits(doc, pane, hits, ev);
                self.update_focus(doc, pane);
                out
            }
        }
    }
}
