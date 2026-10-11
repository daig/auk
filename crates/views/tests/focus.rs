//! Focus and press states: `:focus`, `:focus-visible` and
//! `:focus-within` follow the program's field focus and Tab, `:active` follows a primary press,
//! clickable nodes are buttons and Tab stops, and keys stay the program's while one runs.

mod common;

use auk_tsp::{Mods, SurfaceKey, ViewEvent};
use auk_ui::{ElementState, NodeId};
use auk_views::{NavKey, Outcome, PointerEvent, Region};
use common::Harness;
use serde_json::json;

fn center(h: &Harness, el: NodeId) -> (f32, f32) {
    let r = h.doc.border_box(el).expect("laid out");
    (r.x + r.w / 2.0, r.y + r.h / 2.0)
}

fn click_el(h: &mut Harness, el: NodeId) -> Vec<Outcome> {
    let (x, y) = center(h, el);
    let out = h.view.click(
        &mut h.doc,
        &h.pane,
        PointerEvent {
            x,
            y,
            clicks: 1,
            mods: Mods::NONE,
            ..Default::default()
        },
    );
    h.view.layout(&mut h.doc);
    out
}

fn events(out: &[Outcome]) -> Vec<ViewEvent> {
    out.iter()
        .filter_map(|o| match o {
            Outcome::Event { event, .. } => Some((**event).clone()),
            _ => None,
        })
        .collect()
}

/// `:focus` and `:focus-visible` together.
const SHOWN: ElementState = ElementState(ElementState::FOCUS.0 | ElementState::FOCUS_VISIBLE.0);

fn state(h: &Harness, el: NodeId) -> ElementState {
    h.doc.node(el).element().expect("element").state
}

fn has(h: &Harness, el: NodeId, s: ElementState) -> bool {
    state(h, el).contains(s)
}

fn key(h: &Harness, sf: &str) -> SurfaceKey {
    h.pane
        .surfaces()
        .iter()
        .rev()
        .find(|s| s.id() == sf)
        .expect("surface")
        .key()
}

/// Every element of the document carrying any focus state.
fn focused_anywhere(h: &Harness) -> Vec<NodeId> {
    let any = ElementState(
        ElementState::FOCUS.0 | ElementState::FOCUS_VISIBLE.0 | ElementState::FOCUS_WITHIN.0,
    );
    let mut out = Vec::new();
    let mut stack = vec![h.doc.root()];
    while let Some(n) = stack.pop() {
        if let Some(e) = h.doc.node(n).element()
            && e.state.0 & any.0 != 0
        {
            out.push(n);
        }
        stack.extend(h.doc.children(n).iter().copied());
    }
    out
}

fn two_fields(h: &mut Harness) {
    h.hello(&["edit"]);
    h.surface(
        "s1",
        "inline",
        json!([
            {"id": "a", "k": "col", "c": [{"id": "ed", "k": "editor", "p": {"text": "one"}}]},
            {"id": "b", "k": "col", "c": [{"id": "ed2", "k": "input", "p": {"text": "two"}}]}
        ]),
    );
    h.frame();
}

#[test]
fn a_focused_field_has_focus_and_focus_visible_and_its_ancestors_focus_within() {
    let mut h = Harness::new();
    two_fields(&mut h);
    assert!(focused_anywhere(&h).is_empty(), "nothing focused yet");
    h.frame_ops("s1", 2, json!([["focus", "ed"]]));
    h.frame();
    let ed = h.el("s1", "ed");
    let col = h.el("s1", "a");
    let region = h.view.region(key(&h, "s1"), Region::Main).unwrap();
    assert!(has(&h, ed, ElementState::FOCUS));
    assert!(
        has(&h, ed, ElementState::FOCUS_VISIBLE),
        "a text field always shows focus"
    );
    assert!(!has(&h, ed, ElementState::FOCUS_WITHIN));
    for anc in [col, region] {
        assert!(has(&h, anc, ElementState::FOCUS_WITHIN));
        assert!(!has(&h, anc, ElementState::FOCUS));
        assert!(!has(&h, anc, ElementState::FOCUS_VISIBLE));
    }
    // The chain ends at the surface's region: the pane container is not within.
    assert_eq!(h.view.focus_chain().last(), Some(&region));
    assert!(!has(&h, h.view.element(), ElementState::FOCUS_WITHIN));
    // A sheet sees it.
    assert_eq!(h.view.focus_chain()[0], ed);
}

#[test]
fn moving_focus_clears_the_old_chain_exactly() {
    let mut h = Harness::new();
    two_fields(&mut h);
    h.frame_ops("s1", 2, json!([["focus", "ed"]]));
    h.frame();
    let (ed, ed2) = (h.el("s1", "ed"), h.el("s1", "ed2"));
    let (a, b) = (h.el("s1", "a"), h.el("s1", "b"));
    let region = h.view.region(key(&h, "s1"), Region::Main).unwrap();

    h.frame_ops("s1", 3, json!([["focus", "ed2"]]));
    h.frame();
    assert!(
        state(&h, ed).0 == 0,
        "the old field has no focus state left"
    );
    assert!(
        !has(&h, a, ElementState::FOCUS_WITHIN),
        "nor its own ancestors"
    );
    assert!(has(&h, ed2, ElementState::FOCUS));
    assert!(has(&h, b, ElementState::FOCUS_WITHIN));
    assert!(
        has(&h, region, ElementState::FOCUS_WITHIN),
        "the shared ancestor keeps it"
    );

    // The window (or pane) loses focus: nothing matches; it comes back with the pane.
    h.view.set_focused(&mut h.doc, &h.pane, false);
    assert!(
        focused_anywhere(&h).is_empty(),
        "an unfocused pane shows no focus"
    );
    h.view.set_focused(&mut h.doc, &h.pane, true);
    assert!(has(&h, ed2, ElementState::FOCUS));

    // Focus goes nowhere.
    h.frame_ops("s1", 4, json!([["focus", null]]));
    h.frame();
    assert!(focused_anywhere(&h).is_empty());

    // The focused node is removed.
    h.frame_ops("s1", 5, json!([["focus", "ed2"]]));
    h.frame();
    assert!(has(&h, ed2, ElementState::FOCUS));
    h.frame_ops("s1", 6, json!([["del", "ed2"]]));
    h.frame();
    assert!(
        focused_anywhere(&h).is_empty(),
        "nothing keeps a removed node's focus"
    );
    assert!(h.view.focus_chain().is_empty());

    // The surface closes (its main stays, its focus goes).
    h.frame_ops("s1", 7, json!([["focus", "ed"]]));
    h.frame();
    assert!(has(&h, ed, ElementState::FOCUS));
    h.apc("x", r#"{"id":"s1"}"#);
    h.frame();
    assert!(
        focused_anywhere(&h).is_empty(),
        "a closed surface has no focus"
    );
}

#[test]
fn a_press_on_a_clickable_is_active_until_release() {
    let mut h = Harness::new();
    h.hello(&[]);
    h.surface(
        "s1",
        "inline",
        json!([
            {"id": "go", "k": "row", "p": {"actions": {"click": "go"}}, "c": [
                {"id": "go.t", "k": "text", "p": {"text": "Go"}}
            ]},
            {"id": "plain", "k": "text", "p": {"text": "plain"}}
        ]),
    );
    h.frame();
    let (go, inner, plain) = (h.el("s1", "go"), h.el("s1", "go.t"), h.el("s1", "plain"));
    let out = click_el(&mut h, inner);
    assert_eq!(events(&out).len(), 1);
    assert!(
        has(&h, go, ElementState::ACTIVE),
        "the pressed .sf-act node"
    );
    assert!(
        !has(&h, inner, ElementState::ACTIVE),
        "not the part under the pointer"
    );
    assert!(h.view.release(&mut h.doc));
    assert!(!has(&h, go, ElementState::ACTIVE));
    assert!(!h.view.release(&mut h.doc), "nothing left to release");

    // A press on what isn't clickable is active nowhere.
    click_el(&mut h, plain);
    assert!(!has(&h, plain, ElementState::ACTIVE));
    assert!(!h.view.release(&mut h.doc));

    // A secondary press isn't active either.
    let (x, y) = center(&h, go);
    h.view.click(
        &mut h.doc,
        &h.pane,
        PointerEvent {
            x,
            y,
            clicks: 1,
            button: auk_views::Button::Secondary,
            ..Default::default()
        },
    );
    assert!(!has(&h, go, ElementState::ACTIVE));

    // Losing focus cancels a press.
    click_el(&mut h, go);
    assert!(has(&h, go, ElementState::ACTIVE));
    h.view.set_focused(&mut h.doc, &h.pane, false);
    assert!(!has(&h, go, ElementState::ACTIVE));
}

#[test]
fn a_clickable_text_is_a_tab_stop_and_a_button() {
    let mut h = Harness::new();
    h.hello(&[]);
    h.surface(
        "s1",
        "inline",
        json!([
            {"id": "copy", "k": "text", "p": {"text": "Copy", "actions": {"click": "copy"}}},
            {"id": "plain", "k": "text", "p": {"text": "plain"}},
            {"id": "ed", "k": "editor", "p": {"text": "x", "actions": {"click": "go"}}},
            {"id": "btn", "k": "el", "p": {"tag": "button", "text": "Native", "actions": {"click": "go"}}}
        ]),
    );
    h.frame();
    let copy = h.el("s1", "copy");
    assert!(h.class(copy).contains("sf-act"));
    assert_eq!(h.attr(copy, "tabindex").as_deref(), Some("0"));
    assert_eq!(h.attr(copy, "role").as_deref(), Some("button"));
    let plain = h.el("s1", "plain");
    assert_eq!(h.attr(plain, "tabindex"), None);
    assert_eq!(h.attr(plain, "role"), None);
    let ed = h.el("s1", "ed");
    assert_eq!(
        h.attr(ed, "role").as_deref(),
        Some("textbox"),
        "a field is not a button"
    );
    let btn = h.el("s1", "btn");
    assert_eq!(h.attr(btn, "tabindex").as_deref(), Some("0"));
    assert_eq!(
        h.attr(btn, "role"),
        None,
        "a button element has its own role"
    );

    let ax = h.view.accessibility(&h.doc);
    let node = ax
        .find(&|n| n.id.as_deref() == Some("copy"))
        .expect("in the tree");
    assert_eq!(node.role, "button");
    assert_eq!(node.name.as_deref(), Some("Copy"));
    let node = ax.find(&|n| n.id.as_deref() == Some("btn")).unwrap();
    assert_eq!(node.role, "button");

    // The click action going away takes both.
    h.frame_ops("s1", 2, json!([["set", "copy", {"actions": null}]]));
    h.frame();
    assert_eq!(h.attr(copy, "tabindex"), None);
    assert_eq!(h.attr(copy, "role"), None);
}

fn clickables(h: &mut Harness) {
    h.hello(&[]);
    h.surface(
        "s1",
        "inline",
        json!([
            {"id": "one", "k": "text", "p": {"text": "One", "actions": {"click": "first"}}},
            {"id": "mid", "k": "text", "p": {"text": "not a stop"}},
            {"id": "two", "k": "row", "p": {"actions": {"click": "second"}}, "c": [
                {"id": "two.t", "k": "text", "p": {"text": "Two"}}
            ]}
        ]),
    );
    h.frame();
}

#[test]
fn a_surface_whose_program_runs_keeps_tab_and_enter() {
    let mut h = Harness::new();
    clickables(&mut h);
    for k in [NavKey::Next, NavKey::Previous, NavKey::Activate] {
        assert_eq!(
            h.view.nav_key(&mut h.doc, &h.pane, k),
            vec![Outcome::Keys],
            "{k:?} goes to the program"
        );
    }
    assert!(focused_anywhere(&h).is_empty(), "and focuses nothing");
}

#[test]
fn tab_reaches_clickables_and_enter_sends_what_a_click_sends() {
    let mut h = Harness::new();
    clickables(&mut h);
    // Once the program is gone, nothing reads the keys: Tab is the view's.
    h.apc("x", r#"{"id":"s1"}"#);
    h.frame();
    let (one, two) = (h.el("s1", "one"), h.el("s1", "two"));
    let region = h.view.region(key(&h, "s1"), Region::Main).unwrap();
    assert_eq!(
        h.view.nav_key(&mut h.doc, &h.pane, NavKey::Activate),
        vec![Outcome::Keys],
        "nothing focused to press"
    );

    assert_eq!(
        h.view.nav_key(&mut h.doc, &h.pane, NavKey::Next),
        vec![Outcome::Redraw]
    );
    assert!(has(&h, one, ElementState::FOCUS));
    assert!(
        has(&h, one, ElementState::FOCUS_VISIBLE),
        "keyboard focus is visible"
    );
    assert!(has(&h, region, ElementState::FOCUS_WITHIN));
    h.view.nav_key(&mut h.doc, &h.pane, NavKey::Next);
    assert!(state(&h, one).0 == 0, "Tab moved on");
    assert!(has(&h, two, SHOWN));
    h.view.nav_key(&mut h.doc, &h.pane, NavKey::Next);
    assert!(has(&h, one, ElementState::FOCUS), "Tab wraps");
    h.view.nav_key(&mut h.doc, &h.pane, NavKey::Previous);
    assert!(has(&h, two, ElementState::FOCUS), "⇧Tab goes back");

    // Enter presses it as a click would.
    let by_key = events(&h.view.nav_key(&mut h.doc, &h.pane, NavKey::Activate));
    assert!(has(&h, two, ElementState::FOCUS), "pressing keeps focus");
    let two_t = h.el("s1", "two.t");
    let by_click = events(&click_el(&mut h, two_t));
    assert_eq!(by_key.len(), 1);
    assert_eq!(by_key, by_click);
    h.view.release(&mut h.doc);
    // The press took focus off it (a press focuses nothing).
    assert!(focused_anywhere(&h).is_empty());

    // Unfocused, the remembered node shows nothing until the pane is focused again.
    h.view.nav_key(&mut h.doc, &h.pane, NavKey::Next);
    assert!(has(&h, one, ElementState::FOCUS));
    h.view.set_focused(&mut h.doc, &h.pane, false);
    assert!(focused_anywhere(&h).is_empty());
    h.view.set_focused(&mut h.doc, &h.pane, true);
    assert!(has(&h, one, SHOWN));

    // The surface goes away: so does its focus.
    h.apc("x", r#"{"id":"s1","keep":false}"#);
    h.frame();
    assert!(focused_anywhere(&h).is_empty());
    assert!(h.view.focus_chain().is_empty());
}
