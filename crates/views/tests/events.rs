//! View state and input (§7.8, §7.9): folds and control flips with the "same value keeps the
//! user's choice" rule, form values, pointer actions mapped to events, native editing and IME.

mod common;

use auk_tsp::{Mods, ViewEvent};
use auk_ui::NodeId;
use auk_views::{Outcome, PointerEvent};
use common::Harness;
use serde_json::{Value, json};

fn center(h: &Harness, el: NodeId) -> (f32, f32) {
    let r = h.doc.border_box(el).expect("laid out");
    (r.x + r.w / 2.0, r.y + r.h / 2.0)
}

fn click_el(h: &mut Harness, el: NodeId, clicks: u8, mods: Mods) -> Vec<Outcome> {
    let (x, y) = center(h, el);
    let out = h.view.click(
        &mut h.doc,
        &h.pane,
        PointerEvent {
            x,
            y,
            clicks,
            mods,
            ..Default::default()
        },
    );
    h.view.layout(&mut h.doc);
    out
}

/// Deliver outcomes' events through the engine; returns what the program heard.
fn deliver(h: &mut Harness, out: Vec<Outcome>) -> Vec<Value> {
    let before = h.sent.len();
    for o in out {
        if let Outcome::Event { sf, event, .. } = o {
            h.send(&sf, *event);
        }
    }
    h.sent[before..].to_vec()
}

fn events(out: &[Outcome]) -> Vec<ViewEvent> {
    out.iter()
        .filter_map(|o| match o {
            Outcome::Event { event, .. } => Some((**event).clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn folds_flip_locally_and_keep_the_users_choice() {
    let mut h = Harness::new();
    h.hello(&[]);
    h.surface(
        "s1",
        "inline",
        json!([{"id": "c", "k": "card", "p": {"head": "Build", "collapsible": true, "collapsed": true, "key": "k1"}, "c": [
            {"id": "c.t", "k": "text", "p": {"text": "body"}}
        ]}]),
    );
    h.frame();
    let card = h.el("s1", "c");
    assert!(h.class(card).contains("collapsed"));
    let head = h.find_class(card, "sf-card-head")[0];
    let out = click_el(&mut h, head, 1, Mods::NONE);
    assert!(
        !h.class(card).contains("collapsed"),
        "flipped locally at once"
    );
    let heard = deliver(&mut h, out);
    assert_eq!(
        heard,
        vec![json!({"ev": "toggle", "sf": "s1", "id": "c", "collapsed": false, "key": "k1"})]
    );
    // The program resends the same value: the user's choice stays.
    h.frame_ops(
        "s1",
        2,
        json!([["set", "c", {"collapsed": true, "head": "Build 2"}]]),
    );
    h.frame();
    assert!(!h.class(card).contains("collapsed"));
    // A different value resets it.
    h.frame_ops("s1", 3, json!([["set", "c", {"collapsed": false}]]));
    h.frame_ops("s1", 4, json!([["set", "c", {"collapsed": true}]]));
    h.frame();
    assert!(h.class(card).contains("collapsed"));
    // Deleting and adding with a new id and the same key keeps the fold.
    let head = h.find_class(card, "sf-card-head")[0];
    click_el(&mut h, head, 1, Mods::NONE);
    h.frame_ops(
        "s1",
        5,
        json!([["del", "c"], ["add", "c2", "main", null, {"id": "c2", "k": "card", "p": {"head": "Build", "collapsible": true, "collapsed": true, "key": "k1"}}]]),
    );
    h.frame();
    assert!(!h.class(h.el("s1", "c2")).contains("collapsed"));
}

#[test]
fn form_controls_flip_and_carry_values() {
    let mut h = Harness::new();
    h.hello(&[]);
    h.surface(
        "s1",
        "inline",
        json!([{"id": "f", "k": "el", "p": {"tag": "form"}, "c": [
            {"id": "l1", "k": "el", "p": {"tag": "label"}, "c": [
                {"id": "r1", "k": "el", "p": {"tag": "input", "type": "radio", "name": "size", "value": "s"}},
                {"id": "t1", "k": "el", "p": {"tag": "span", "text": "Small"}}
            ]},
            {"id": "l2", "k": "el", "p": {"tag": "label"}, "c": [
                {"id": "r2", "k": "el", "p": {"tag": "input", "type": "radio", "name": "size", "value": "l", "checked": true}},
                {"id": "t2", "k": "el", "p": {"tag": "span", "text": "Large"}}
            ]},
            {"id": "cb", "k": "el", "p": {"tag": "input", "type": "checkbox", "name": "fast"}},
            {"id": "x1", "k": "el", "p": {"tag": "input", "type": "checkbox", "name": "tags", "value": "a", "checked": true}},
            {"id": "x2", "k": "el", "p": {"tag": "input", "type": "checkbox", "name": "tags", "value": "b"}},
            {"id": "off", "k": "el", "p": {"tag": "input", "type": "checkbox", "name": "never", "disabled": true}},
            {"id": "go", "k": "el", "p": {"tag": "button", "text": "Create", "actions": {"click": "submit"}}}
        ]}]),
    );
    h.frame();
    // A label click flips its first control.
    let t1 = h.el("s1", "t1");
    let out = click_el(&mut h, t1, 1, Mods::NONE);
    let r1 = h.el("s1", "r1");
    let r2 = h.el("s1", "r2");
    assert!(h.class(r1).contains("on"));
    assert!(
        !h.class(r2).contains("on"),
        "radios group by name in the form"
    );
    let heard = deliver(&mut h, out);
    assert_eq!(
        heard,
        vec![
            json!({"ev": "change", "sf": "s1", "id": "r1", "value": "s", "checked": true, "name": "size",
            "values": {"size": "s", "fast": false, "tags": ["a"]}})
        ]
    );
    // A disabled control doesn't flip.
    let off = h.el("s1", "off");
    assert!(click_el(&mut h, off, 1, Mods::NONE).is_empty());
    // Submit: an action with the form values and modifiers in order.
    let cb = h.el("s1", "cb");
    let out = click_el(&mut h, cb, 1, Mods::NONE);
    deliver(&mut h, out);
    let go = h.el("s1", "go");
    let out = click_el(
        &mut h,
        go,
        1,
        Mods {
            shift: true,
            meta: true,
            ..Mods::NONE
        },
    );
    let heard = deliver(&mut h, out);
    assert_eq!(
        heard,
        vec![
            json!({"ev": "action", "sf": "s1", "id": "go", "act": "submit", "mods": ["shift", "meta"],
            "values": {"size": "s", "fast": true, "tags": ["a"]}})
        ]
    );
    // The program resends `checked` unchanged: the user's choice stays.
    h.frame_ops(
        "s1",
        2,
        json!([["set", "r2", {"checked": true, "value": "l"}]]),
    );
    h.frame();
    assert!(!h.class(r2).contains("on"));
}

#[test]
fn actions_lists_tabs_trees_links_and_menus() {
    let mut h = Harness::new();
    h.hello(&[]);
    h.surface(
        "s1",
        "inline",
        json!([
            {"id": "a", "k": "text", "p": {"text": "sort", "actions": {"click": "sort=name", "dblclick": "open-it", "menu": ["open-file", "pin"]}}},
            {"id": "ls", "k": "list", "c": [
                {"id": "i1", "k": "item", "p": {"label": "one"}},
                {"id": "i2", "k": "item", "p": {"label": "two", "actions": {"click": "custom"}}}
            ]},
            {"id": "tb", "k": "tabs", "p": {"items": [{"id": "x", "label": "X"}, {"id": "y", "label": "Y"}], "active": "x"}},
            {"id": "tr", "k": "tree", "p": {"nodes": [{"id": "src", "label": "src", "children": [{"id": "a.rs", "label": "a.rs"}]}]}},
            {"id": "ln", "k": "text", "p": {"spans": [{"t": "docs", "href": "https://example.com"}]}},
            {"id": "cp", "k": "code", "p": {"text": "let x = 1;", "actions": {"click": "copy"}}}
        ]),
    );
    h.frame();
    let a = h.el("s1", "a");
    let out = click_el(
        &mut h,
        a,
        1,
        Mods {
            ctrl: true,
            ..Mods::NONE
        },
    );
    assert_eq!(
        deliver(&mut h, out),
        vec![
            json!({"ev": "action", "sf": "s1", "id": "a", "act": "sort", "value": "name", "mods": ["ctrl"]})
        ]
    );
    let out = click_el(&mut h, a, 2, Mods::NONE);
    assert_eq!(deliver(&mut h, out)[0]["act"], "open-it");
    let (x, y) = center(&h, a);
    let menu = h.view.menu(&h.doc, &h.pane, x, y);
    let labels: Vec<&str> = menu.iter().map(|m| m.label.as_str()).collect();
    assert_eq!(labels, vec!["Open file", "Pin", "Copy"]);
    let out = h.view.menu_pick(&mut h.doc, &h.pane, x, y, "pin");
    assert_eq!(
        deliver(&mut h, out),
        vec![json!({"ev": "action", "sf": "s1", "id": "a", "act": "pin"})]
    );
    // List items: select on click, activate on double click; an item's own action wins.
    let i1 = h.el("s1", "i1");
    let out = click_el(&mut h, i1, 1, Mods::NONE);
    assert_eq!(
        deliver(&mut h, out),
        vec![json!({"ev": "select", "sf": "s1", "id": "ls", "item": "i1"})]
    );
    let out = click_el(&mut h, i1, 2, Mods::NONE);
    assert_eq!(
        deliver(&mut h, out),
        vec![json!({"ev": "activate", "sf": "s1", "id": "ls", "item": "i1"})]
    );
    let i2 = h.el("s1", "i2");
    let out = click_el(&mut h, i2, 1, Mods::NONE);
    assert_eq!(deliver(&mut h, out)[0]["act"], "custom");
    // The program moves the selection: the items show it.
    h.frame_ops("s1", 2, json!([["set", "ls", {"selected": "i2"}]]));
    h.frame();
    assert!(h.class(h.el("s1", "i2")).contains("sel"));
    assert!(!h.class(h.el("s1", "i1")).contains("sel"));
    // Tabs select; tree twisties toggle locally with the item as key.
    let tb = h.el("s1", "tb");
    let y_tab = h.find_class(tb, "sf-tab")[1];
    let out = click_el(&mut h, y_tab, 1, Mods::NONE);
    assert_eq!(
        deliver(&mut h, out),
        vec![json!({"ev": "select", "sf": "s1", "id": "tb", "item": "y"})]
    );
    let tr = h.el("s1", "tr");
    let tw = h.find_class(tr, "sf-tree-tw")[0];
    let out = click_el(&mut h, tw, 1, Mods::NONE);
    assert_eq!(
        deliver(&mut h, out),
        vec![json!({"ev": "toggle", "sf": "s1", "id": "tr", "collapsed": false, "key": "src"})]
    );
    assert_eq!(
        h.find_class(h.el("s1", "tr"), "sf-tree-node").len(),
        2,
        "opened locally"
    );
    // Links open on ⌘-click only (by default); plain clicks don't.
    let link = h.find_class(h.el("s1", "ln"), "sf-link")[0];
    assert!(click_el(&mut h, link, 1, Mods::NONE).is_empty());
    assert_eq!(
        click_el(
            &mut h,
            link,
            1,
            Mods {
                meta: true,
                ..Mods::NONE
            }
        ),
        vec![Outcome::Open("https://example.com".into())]
    );
    // Terminal-run actions send no event.
    let cp = h.el("s1", "cp");
    assert_eq!(
        click_el(&mut h, cp, 1, Mods::NONE),
        vec![Outcome::Copy("let x = 1;".into())]
    );
}

#[test]
fn native_editing_behind_features() {
    let mut h = Harness::new();
    h.hello(&["edit", "undo", "send"]);
    h.surface(
        "s1",
        "inline",
        json!([
            {"id": "ed", "k": "editor", "p": {"text": "hello world", "cursor": 11, "sendable": true}},
            {"id": "ro", "k": "editor", "p": {"text": "read only", "readonly": true}},
            {"id": "vim", "k": "editor", "p": {"text": "x", "mode": "normal"}}
        ]),
    );
    h.frame();
    // A click in an unfocused field asks for the keys.
    let ed = h.el("s1", "ed");
    let out = click_el(&mut h, ed, 1, Mods::NONE);
    assert_eq!(
        deliver(&mut h, out),
        vec![json!({"ev": "focus", "sf": "s1", "id": "ed"})]
    );
    let ro = h.el("s1", "ro");
    assert!(
        click_el(&mut h, ro, 1, Mods::NONE).is_empty(),
        "readonly: no focus"
    );
    h.frame_ops("s1", 2, json!([["focus", "ed"]]));
    h.frame();
    // Typing without a selection is keys.
    assert_eq!(h.view.type_text(&mut h.doc, &h.pane, "!"), Outcome::Keys);
    // ⌘A then typing replaces the selection as an edit.
    assert_eq!(h.view.select_all(&mut h.doc, &h.pane), Outcome::Redraw);
    h.frame();
    assert!(h.class(ed).contains("selecting"));
    assert_eq!(h.text(h.find_class(ed, "sf-ed-sel")[0]), "hello world");
    let out = h.view.type_text(&mut h.doc, &h.pane, "hi");
    assert_eq!(
        deliver(&mut h, vec![out]),
        vec![
            json!({"ev": "edit", "sf": "s1", "id": "ed", "from": 0, "to": 11, "text": "hi", "cursor": 2, "len": 11})
        ]
    );
    assert!(
        !h.class(ed).contains("selecting"),
        "the selection went with the edit"
    );
    // A selection, then delete.
    h.view.select(&mut h.doc, &h.pane, 6, 11);
    let out = h.view.delete_selection(&mut h.doc, &h.pane);
    assert_eq!(deliver(&mut h, vec![out])[0]["to"], 11);
    // Home/End and ↑/↓ become caret edits.
    let out = h.view.move_caret(&mut h.doc, &h.pane, 0);
    assert_eq!(
        deliver(&mut h, vec![out])[0],
        json!({"ev": "edit", "sf": "s1", "id": "ed", "from": 0, "to": 0, "text": "", "cursor": 0, "len": 11})
    );
    // Undo, send.
    let out = h.view.undo(&h.pane);
    assert_eq!(
        deliver(&mut h, vec![out]),
        vec![json!({"ev": "undo", "sf": "s1", "id": "ed"})]
    );
    let out = h.view.send(&h.pane, "ed", "a prompt").unwrap();
    assert_eq!(
        deliver(&mut h, vec![out]),
        vec![json!({"ev": "send", "sf": "s1", "id": "ed", "text": "a prompt"})]
    );
    // IME preedit draws at the caret; a commit is typed text.
    h.view.ime_preedit(&mut h.doc, &h.pane, "にほ", 1);
    h.frame();
    let pre = h.find_class(ed, "sf-ed-preedit");
    assert_eq!(pre.len(), 1);
    assert_eq!(h.text(pre[0]), "にほ");
    assert_eq!(h.find_class(pre[0], "sf-caret").len(), 1);
    assert!(h.view.caret_rect(&h.doc, &h.pane).is_some());
    assert_eq!(
        h.view.ime_commit(&mut h.doc, &h.pane, "日本"),
        Outcome::Keys
    );
    h.frame();
    assert!(h.find_class(ed, "sf-ed-preedit").is_empty());
    // A field with a Vim mode never edits natively.
    h.frame_ops("s1", 3, json!([["focus", "vim"]]));
    h.frame();
    assert_eq!(h.view.select_all(&mut h.doc, &h.pane), Outcome::Keys);
    assert_eq!(h.view.undo(&h.pane), Outcome::Keys);
}

#[test]
fn no_edit_feature_means_keys() {
    let mut h = Harness::new();
    h.hello(&[]);
    h.surface(
        "s1",
        "inline",
        json!([{"id": "ed", "k": "editor", "p": {"text": "abc"}}]),
    );
    h.frame_ops("s1", 2, json!([["focus", "ed"]]));
    h.frame();
    assert_eq!(h.view.select_all(&mut h.doc, &h.pane), Outcome::Keys);
    assert_eq!(h.view.undo(&h.pane), Outcome::Keys);
    assert_eq!(h.view.send(&h.pane, "ed", "x"), None);
    // A click on the focused field doesn't place a caret natively.
    let ed = h.el("s1", "ed");
    assert!(events(&click_el(&mut h, ed, 1, Mods::NONE)).is_empty());
}

#[test]
fn controls_are_inert_in_listen_false_surfaces() {
    let mut h = Harness::new();
    h.apc("o", r#"{"id":"s1","mode":"flow","listen":false}"#);
    h.frame_ops(
        "s1",
        1,
        json!([["add", "main", "s1", null, {"id": "main", "k": "col", "c": [
            {"id": "cb", "k": "el", "p": {"tag": "input", "type": "checkbox", "name": "x"}}
        ]}]]),
    );
    h.frame();
    let cb = h.el("s1", "cb");
    assert!(click_el(&mut h, cb, 1, Mods::NONE).is_empty());
    assert!(!h.class(cb).contains("on"));
}

#[test]
fn hover_sets_state_on_the_chain() {
    let mut h = Harness::new();
    h.hello(&[]);
    h.surface(
        "s1",
        "inline",
        json!([{"id": "ls", "k": "list", "c": [{"id": "i1", "k": "item", "p": {"label": "one"}}]}]),
    );
    h.frame();
    let i1 = h.el("s1", "i1");
    let (x, y) = center(&h, i1);
    h.view.hover(&mut h.doc, x, y);
    h.frame();
    let st = h.doc.node(i1).element().unwrap().state;
    assert!(st.contains(auk_ui::ElementState::HOVER));
    h.view.hover(&mut h.doc, -10.0, -10.0);
    let st = h.doc.node(i1).element().unwrap().state;
    assert!(!st.contains(auk_ui::ElementState::HOVER));
}

/// §7.9: "The terminal draws the text, caret, selection, decorations and IME preedit." A field's
/// program `decor` ranges (UTF-16 offsets) are drawn as styled runs, and a new `decor` redraws.
#[test]
fn a_fields_program_decor_decorations_are_drawn() {
    let mut h = Harness::new();
    h.hello(&["edit"]);
    h.surface(
        "s1",
        "inline",
        json!([{"id": "ed", "k": "editor", "p": {"text": "é let x = 1", "cursor": 0, "decor": [
            {"from": 2, "to": 5, "s": "strong"},
            {"from": 6, "to": 7, "s": "error"},
            {"from": 4, "to": 4, "s": "strong"}
        ]}}]),
    );
    h.frame();
    let ed = h.el("s1", "ed");
    let style = |h: &Harness, el| h.doc.computed_style(el).unwrap().clone();
    let strong = h.find_class(ed, "sf-t-strong");
    assert_eq!(strong.len(), 1, "an empty range draws nothing");
    assert_eq!(h.text(strong[0]), "let", "offsets count UTF-16 units");
    let error = h.find_class(ed, "sf-t-error");
    assert_eq!(error.len(), 1);
    assert_eq!(h.text(error[0]), "x");
    let plain = style(&h, ed);
    assert!(style(&h, strong[0]).font_weight > plain.font_weight, "strong is bolder");
    assert_ne!(style(&h, error[0]).color, plain.color, "error has its own color");
    // A program token (`syntaxString`) draws in its color; a new `decor` replaces the old.
    h.frame_ops(
        "s1",
        2,
        json!([["set", "ed", {"decor": [{"from": 8, "to": 9, "s": "syntaxString"}]}]]),
    );
    h.frame();
    assert!(h.find_class(ed, "sf-t-strong").is_empty() && h.find_class(ed, "sf-t-error").is_empty());
    let text_el = h.find_class(ed, "sf-ed-text");
    let spans: Vec<NodeId> = h
        .doc
        .children(text_el[0])
        .iter()
        .copied()
        .filter(|&c| h.doc.node(c).element().is_some() && h.text(c) == "=")
        .collect();
    assert_eq!(spans.len(), 1, "the decorated run is its own span");
    assert_ne!(style(&h, spans[0]).color, plain.color, "drawn in the token's color");
}
