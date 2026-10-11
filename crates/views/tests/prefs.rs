//! `prefs` (T2): the settings page and sheet drawn
//! from the program's data, the pointer events it sends, the program-driven focus ring, menus and
//! text drafts, the aside docking, and omp's own `/settings` (`fixtures/tsp/omp-settings.jsonl`).

mod common;

use auk_tsp::{Config, Mods, ViewEvent};
use auk_ui::{Document, NodeId};
use auk_views::{Outcome, PointerEvent, ViewSettings};
use common::{Harness, replay};
use serde_json::{Value, json};

/// The engine advertising `prefs` and `aside`, as Tern does.
fn harness() -> Harness {
    let mut config = Config::default();
    config.kinds.push("prefs".into());
    config.kinds.push("picker".into());
    if !config.features.iter().any(|f| f == "aside") {
        config.features.push("aside".into());
    }
    let mut h = Harness::with(config, ViewSettings::default());
    // As the hosts do (`surfaces::HOST_CSS`): the pane fills the window, so a screen surface does.
    h.doc.set_stylesheet(
        "host-pane",
        auk_ui::SheetKind::BuiltIn,
        None,
        ".auk-pane { min-height: 100vh; }",
    );
    h.view.set_cols(&mut h.doc, &h.pane, 120);
    h.hello(&["edit"]);
    h
}

/// Tern's docs' example page (the Lua `settings(state)`), plus the rows it doesn't
/// show: keys, a menu choice, a warning, a disabled row and an ordered multi.
fn page(extra: Value) -> Value {
    let mut p = json!({
        "title": "Deploy settings",
        "lead": "How and where your pushes deploy.",
        "pages": [
            {"id": "general", "label": "General", "icon": "sliders"},
            {"id": "targets", "label": "Targets", "icon": "server", "changed": 2},
            {"id": "keys", "label": "Shortcuts", "icon": "keyboard", "group": "More"},
            {"id": "lab", "label": "Lab", "group": "More", "disabled": "Not in this build"}
        ],
        "page": "general",
        "sections": [
            {"id": "run", "title": "Runs", "rows": [
                {"id": "auto", "label": "Deploy on push", "hint": "Every push to main starts a deploy.",
                 "control": {"k": "switch", "on": true}},
                {"id": "env", "label": "Environment", "changed": true, "defaultLabel": "Dev",
                 "control": {"k": "choice", "value": "staging", "options": [
                    {"value": "dev", "label": "Dev"}, {"value": "staging", "label": "Staging"},
                    {"value": "prod", "label": "Prod"}]}},
                {"id": "region", "label": "Region", "control": {"k": "choice", "value": "eu", "style": "menu",
                 "options": [{"value": "us", "label": "US East", "detail": "Virginia"},
                             {"value": "eu", "label": "EU West", "detail": "Dublin"},
                             {"value": "ap", "label": "Asia Pacific"}]}},
                {"id": "jobs", "label": "Parallel jobs", "control": {"k": "number", "value": 8, "min": 1, "max": 8}},
                {"id": "level", "label": "Level", "control": {"k": "number", "value": 1, "labels": {"0": "Off", "1": "Low", "3": "High"}}},
                {"id": "token", "label": "API token", "control": {"k": "text", "value": "abc", "secret": true}},
                {"id": "host", "label": "Host", "control": {"k": "text", "value": ""}},
                {"id": "notify", "label": "Notify", "control": {"k": "multi", "values": ["email"], "options": [
                    {"value": "slack", "label": "Slack"}, {"value": "email", "label": "Email"},
                    {"value": "pager", "label": "Pager"}]}},
                {"id": "order", "label": "Order", "control": {"k": "multi", "ordered": true, "values": ["b", "a"], "options": [
                    {"value": "a", "label": "Alpha"}, {"value": "b", "label": "Beta"}, {"value": "c", "label": "Gamma"}]}},
                {"id": "logs", "label": "Clear logs", "hint": "Deletes the logs of finished runs.",
                 "control": {"k": "action", "act": "clear-logs", "label": "Clear"}},
                {"id": "send", "label": "Send", "control": {"k": "keys", "keys": [["ctrl", "enter"], ["cmd", "s"]]}},
                {"id": "old", "label": "Legacy mode", "warning": "Going away", "disabled": "Managed by policy",
                 "control": {"k": "switch", "on": false}}
            ]},
            {"id": "empty", "title": "Nothing here", "rows": []}
        ]
    });
    if let (Some(o), Some(e)) = (p.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            o.insert(k.clone(), v.clone());
        }
    }
    p
}

/// A screen surface whose `main` is the page.
fn open_page(h: &mut Harness, extra: Value) {
    h.apc("o", r#"{"id":"s1","mode":"screen"}"#);
    h.frame_ops(
        "s1",
        1,
        json!([["add", "main", "s1", null, {"id": "main", "k": "col", "c": [{"id": "pf", "k": "prefs", "p": page(extra)}]}]]),
    );
    h.frame();
}

/// Lays out, then lets the sheet's slide-in finish (it moves the sheet, so clicks and pixels
/// wait for it).
fn settle(h: &mut Harness) {
    h.frame();
    h.now += 1_000.0;
    h.sync();
    h.frame();
}

fn set(h: &mut Harness, s: u64, props: Value) {
    h.frame_ops("s1", s, json!([["set", "pf", props]]));
    h.frame();
}

fn first(h: &Harness, under: NodeId, class: &str) -> NodeId {
    *h.find_class(under, class)
        .first()
        .unwrap_or_else(|| panic!("a .{class}"))
}

/// The `.pf-row` of a row id.
fn row(h: &Harness, id: &str) -> NodeId {
    let pf = h.el("s1", "pf");
    h.find_class(pf, "pf-row")
        .into_iter()
        .find(|&r| h.attr(r, "data-row").as_deref() == Some(id))
        .unwrap_or_else(|| panic!("row {id}"))
}

fn center(h: &Harness, el: NodeId) -> (f32, f32) {
    let r = h.doc.border_box(el).expect("laid out");
    (r.x + r.w / 2.0, r.y + r.h / 2.0)
}

fn click_at(h: &mut Harness, x: f32, y: f32) -> Vec<ViewEvent> {
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
    out.into_iter()
        .filter_map(|o| match o {
            Outcome::Event { event, .. } => Some(*event),
            _ => None,
        })
        .collect()
}

fn click(h: &mut Harness, el: NodeId) -> Vec<ViewEvent> {
    let (x, y) = center(h, el);
    click_at(h, x, y)
}

/// The wire bodies of view events.
fn bodies(evs: &[ViewEvent]) -> Vec<Value> {
    evs.iter().map(|e| e.body("s1")).collect()
}

#[test]
fn prefs_draws_the_books_page() {
    let mut h = harness();
    open_page(&mut h, json!({"focus": "jobs"}));
    assert!(h.errors().is_empty(), "{:?}", h.errors());
    let pf = h.el("s1", "pf");
    assert!(h.class(pf).contains("sf-prefs"));
    let root = first(&h, pf, "pf");
    assert_eq!(
        h.class(root),
        "pf on",
        "a page, wide enough for the nav column"
    );
    assert_eq!(
        h.attr(root, "aria-label").as_deref(),
        Some("Deploy settings")
    );
    // Nav: brand, search placeholder, groups, the current page, a changed count, a disabled page.
    assert_eq!(h.text(first(&h, root, "pf-brand-t")), "Deploy settings");
    let ph = first(&h, first(&h, root, "pf-search"), "ph");
    assert_eq!(h.text(ph), "Search settings");
    let rows = h.find_class(root, "nav-row");
    assert_eq!(rows.len(), 4);
    assert!(h.class(rows[0]).contains("on"));
    assert_eq!(h.text(first(&h, rows[1], "pf-changed")), "2");
    assert!(h.class(rows[3]).contains("off"));
    assert_eq!(
        h.attr(rows[3], "title").as_deref(),
        Some("Not in this build")
    );
    let heads: Vec<String> = h
        .find_class(root, "nav-head")
        .into_iter()
        .map(|e| h.text(e))
        .collect();
    assert_eq!(
        heads,
        ["More"],
        "consecutive pages with a group share a heading"
    );
    // Header: NN · Label, the title, the lead.
    assert_eq!(h.text(first(&h, root, "pf-eyebrow")), "01 \u{b7} General");
    assert_eq!(h.text(first(&h, root, "pf-title")), "General");
    assert_eq!(
        h.text(first(&h, root, "pf-lead")),
        "How and where your pushes deploy."
    );
    // A section without rows is skipped.
    let secs = h.find_class(root, "pf-sec");
    assert_eq!(secs.len(), 1);
    assert_eq!(h.attr(secs[0], "data-sec").as_deref(), Some("run"));
    // The seven controls.
    let auto = row(&h, "auto");
    assert!(h.class(first(&h, auto, "pf-sw")).contains("on"));
    let env = row(&h, "env");
    assert!(h.class(env).contains("changed"));
    assert_eq!(
        h.attr(first(&h, env, "pf-dot"), "title").as_deref(),
        Some("Changed from default: Dev")
    );
    assert_eq!(h.text(first(&h, env, "pf-reset")), "Reset");
    let seg = first(&h, env, "pf-seg");
    let on: Vec<String> = h
        .doc
        .children(seg)
        .iter()
        .filter(|&&b| h.class(b) == "on")
        .map(|&b| h.text(b))
        .collect();
    assert_eq!(on, ["Staging"], "auto: 3 short options draw as segments");
    let region = row(&h, "region");
    assert!(h.find_class(region, "pf-seg").is_empty());
    let pick = first(&h, region, "pf-pick");
    assert!(
        h.text(pick).starts_with("EU West"),
        "the popup shows the current label"
    );
    assert!(
        h.find_class(region, "pf-menu").is_empty(),
        "closed until editing"
    );
    let jobs = row(&h, "jobs");
    assert!(h.class(jobs).contains("focus"), "the focus ring");
    assert_eq!(h.text(first(&h, jobs, "v")), "8");
    let steps = h.find_class(jobs, "pf-stepb");
    assert!(!h.class(steps[0]).contains("dis"));
    assert!(
        h.class(steps[1]).contains("dis"),
        "+ has nowhere to go past max"
    );
    assert_eq!(h.text(first(&h, row(&h, "level"), "v")), "Low");
    let token = first(&h, row(&h, "token"), "pf-in");
    assert!(h.class(token).contains("secret"));
    assert_eq!(h.text(token), "\u{2022}\u{2022}\u{2022}");
    assert_eq!(h.text(first(&h, row(&h, "host"), "ph")), "Not set");
    let notify = row(&h, "notify");
    assert!(h.class(notify).contains("stack"));
    let chips: Vec<(String, bool)> = h
        .find_class(notify, "pf-chip")
        .into_iter()
        .map(|c| (h.text(c), h.class(c).contains("on")))
        .collect();
    assert_eq!(
        chips,
        [
            ("Slack".into(), false),
            ("Email".into(), true),
            ("Pager".into(), false)
        ]
    );
    let order: Vec<(String, String)> = h
        .find_class(row(&h, "order"), "pf-oi")
        .into_iter()
        .map(|i| (h.text(first(&h, i, "n")), h.text(first(&h, i, "t"))))
        .collect();
    assert_eq!(
        order,
        [
            ("1".into(), "Beta".into()),
            ("2".into(), "Alpha".into()),
            ("".into(), "Gamma".into())
        ],
        "enabled ones first, in values order"
    );
    let logs = row(&h, "logs");
    assert_eq!(h.text(first(&h, logs, "pf-act")), "Clear");
    let send = row(&h, "send");
    assert!(h.class(send).contains("pf-key"));
    let chords: Vec<String> = h
        .find_class(send, "pf-chord")
        .into_iter()
        .map(|c| h.text(c))
        .collect();
    assert_eq!(chords, ["\u{2303}\u{23ce}", "\u{2318}S"]);
    let old = row(&h, "old");
    assert!(h.class(old).contains("off") && h.class(old).contains("warn"));
    assert_eq!(h.text(first(&h, old, "pf-why")), "Managed by policy");
    assert_eq!(h.text(first(&h, old, "pf-warn")), "Going away");
    // Close at the top right of a page.
    assert!(h.text(first(&h, root, "pf-top")).contains("Close"));
    // The page fills the screen surface: the nav column beside a scrolling document.
    let nav = first(&h, root, "pf-nav");
    let scroll = first(&h, root, "pf-scroll");
    let (n, s) = (
        h.doc.border_box(nav).unwrap(),
        h.doc.border_box(scroll).unwrap(),
    );
    assert!((n.w - 220.0).abs() < 1.0, "{n:?}");
    assert!(
        (s.x - (n.x + n.w)).abs() < 1.0 && s.h > 300.0,
        "{n:?} {s:?}"
    );
}

#[test]
fn prefs_pointer_actions_send_the_books_events() {
    let mut h = harness();
    open_page(&mut h, json!({}));
    let pf = h.el("s1", "pf");
    let rows = h.find_class(pf, "nav-row");
    assert_eq!(
        bodies(&click(&mut h, rows[1])),
        [json!({"ev": "action", "sf": "s1", "id": "pf", "act": "page", "value": "targets"})]
    );
    let off = click(&mut h, rows[3]);
    assert!(off.is_empty(), "a disabled page sends nothing: {off:?}");
    let sw = first(&h, row(&h, "auto"), "pf-sw");
    assert_eq!(
        bodies(&click(&mut h, sw)),
        [json!({"ev": "change", "sf": "s1", "id": "pf", "item": "auto", "value": false})]
    );
    let seg = first(&h, row(&h, "env"), "pf-seg");
    let prod = h.doc.children(seg)[2];
    assert_eq!(
        bodies(&click(&mut h, prod)),
        [json!({"ev": "change", "sf": "s1", "id": "pf", "item": "env", "value": "prod"})]
    );
    let reset = first(&h, row(&h, "env"), "pf-reset");
    assert_eq!(
        bodies(&click(&mut h, reset)),
        [json!({"ev": "change", "sf": "s1", "id": "pf", "item": "env", "value": null})]
    );
    let pick = first(&h, row(&h, "region"), "pf-pick");
    assert_eq!(
        bodies(&click(&mut h, pick)),
        [json!({"ev": "activate", "sf": "s1", "id": "pf", "item": "region"})]
    );
    let steps = h.find_class(row(&h, "jobs"), "pf-stepb");
    assert_eq!(
        bodies(&click(&mut h, steps[0])),
        [json!({"ev": "change", "sf": "s1", "id": "pf", "item": "jobs", "value": 7})]
    );
    assert!(
        click(&mut h, steps[1]).is_empty(),
        "a disabled stepper sends nothing"
    );
    let level = h.find_class(row(&h, "level"), "pf-stepb");
    assert_eq!(
        bodies(&click(&mut h, level[1])),
        [json!({"ev": "change", "sf": "s1", "id": "pf", "item": "level", "value": 3})],
        "the next key of labels"
    );
    let field = first(&h, row(&h, "token"), "pf-in");
    assert_eq!(
        bodies(&click(&mut h, field)),
        [json!({"ev": "activate", "sf": "s1", "id": "pf", "item": "token"})]
    );
    let chips = h.find_class(row(&h, "notify"), "pf-chip");
    assert_eq!(
        bodies(&click(&mut h, chips[0])),
        [
            json!({"ev": "change", "sf": "s1", "id": "pf", "item": "notify", "value": ["slack", "email"]})
        ]
    );
    let gamma = h.find_class(row(&h, "order"), "pf-sw")[2];
    assert_eq!(
        bodies(&click(&mut h, gamma)),
        [
            json!({"ev": "change", "sf": "s1", "id": "pf", "item": "order", "value": ["b", "a", "c"]})
        ]
    );
    let act = first(&h, row(&h, "logs"), "pf-act");
    assert_eq!(
        bodies(&click(&mut h, act)),
        [json!({"ev": "action", "sf": "s1", "id": "pf", "act": "clear-logs", "value": "logs"})]
    );
    // A row off its control selects it.
    let label = first(&h, row(&h, "host"), "pf-lbl");
    assert_eq!(
        bodies(&click(&mut h, label)),
        [json!({"ev": "select", "sf": "s1", "id": "pf", "item": "host"})]
    );
    // A disabled row's controls do nothing.
    let old = first(&h, row(&h, "old"), "pf-lbl");
    assert!(click(&mut h, old).is_empty());
    let close = first(&h, pf, "pf-top");
    let close = h.doc.children(close)[0];
    assert_eq!(
        bodies(&click(&mut h, close)),
        [json!({"ev": "action", "sf": "s1", "id": "pf", "act": "close"})]
    );
    // Delivered through the engine, the bodies reach the program unchanged.
    let before = h.sent.len();
    h.send(
        "s1",
        ViewEvent::PrefsChange {
            id: "pf".into(),
            item: "auto".into(),
            value: json!(true),
        },
    );
    assert_eq!(
        h.sent[before..],
        [json!({"ev": "change", "sf": "s1", "id": "pf", "item": "auto", "value": true})]
    );
}

#[test]
fn prefs_click_asks_for_the_keys_first_when_a_field_outside_has_them() {
    let mut h = harness();
    h.apc("o", r#"{"id":"s1","mode":"inline"}"#);
    h.frame_ops(
        "s1",
        1,
        json!([
            ["add", "main", "s1", null, {"id": "main", "k": "col", "c": [{"id": "in", "k": "input", "p": {"text": ""}}]}],
            ["add", "layer", "s1", null, {"id": "layer", "k": "col", "c": [{"id": "pf", "k": "prefs", "p": page(json!({}))}]}],
            ["focus", "in"]
        ]),
    );
    settle(&mut h);
    let sw = first(&h, row(&h, "auto"), "pf-sw");
    assert_eq!(
        bodies(&click(&mut h, sw)),
        [
            json!({"ev": "focus", "sf": "s1", "id": "pf"}),
            json!({"ev": "change", "sf": "s1", "id": "pf", "item": "auto", "value": false})
        ]
    );
}

#[test]
fn prefs_editing_opens_menus_and_draws_drafts() {
    let mut h = harness();
    open_page(
        &mut h,
        json!({"editing": {"row": "region", "option": "us"}}),
    );
    let region = row(&h, "region");
    assert!(h.class(region).contains("editing"));
    let menu = first(&h, region, "pf-menu");
    assert!(
        !h.class(menu).contains("still"),
        "a menu that opens animates in"
    );
    let items = h.find_class(menu, "mi");
    let marks: Vec<String> = items.iter().map(|&i| h.class(i)).collect();
    assert_eq!(marks, ["mi sel two", "mi cur two", "mi"]);
    assert_eq!(h.text(first(&h, items[0], "d")), "Virginia");
    assert!(
        h.attr(menu, "style").is_none(),
        "styles are written, not attributes"
    );
    assert!(h.view.written().style(menu).unwrap().contains("--pf-room"));
    // Pointing at options previews each once.
    let (x, y) = center(&h, items[2]);
    h.view.hover(&mut h.doc, x, y);
    let hovered = h.view.hover_events(&h.doc);
    let ev: Vec<Value> = hovered
        .iter()
        .filter_map(|o| match o {
            Outcome::Event { event, .. } => Some(event.body("s1")),
            _ => None,
        })
        .collect();
    assert_eq!(
        ev,
        [json!({"ev": "select", "sf": "s1", "id": "pf", "item": "region=ap"})]
    );
    h.view.hover(&mut h.doc, x, y + 1.0);
    assert!(h.view.hover_events(&h.doc).is_empty(), "once per option");
    // Picking an option.
    assert_eq!(
        bodies(&click(&mut h, items[0])),
        [json!({"ev": "change", "sf": "s1", "id": "pf", "item": "region", "value": "us"})]
    );
    // The open popup's button again, or anywhere outside the menu, closes it.
    let pick = first(&h, region, "pf-pick");
    let close = json!({"ev": "action", "sf": "s1", "id": "pf", "act": "close", "value": "region"});
    assert_eq!(bodies(&click(&mut h, pick)), std::slice::from_ref(&close));
    let sw = first(&h, row(&h, "auto"), "pf-sw");
    assert_eq!(bodies(&click(&mut h, sw)), [close]);
    // The program moves the highlight: the menu stays put (no re-entry motion).
    set(
        &mut h,
        2,
        json!({"editing": {"row": "region", "option": "ap"}}),
    );
    let menu = first(&h, row(&h, "region"), "pf-menu");
    assert!(h.class(menu).contains("still"));
    let sel: Vec<String> = h
        .find_class(menu, "sel")
        .into_iter()
        .map(|i| h.attr(i, "data-v").unwrap())
        .collect();
    assert_eq!(sel, ["ap"]);
    // A text row's draft with a caret at the UTF-16 cursor.
    set(
        &mut h,
        3,
        json!({"editing": {"row": "host", "draft": "h\u{1f600}st", "cursor": 3}}),
    );
    assert!(
        h.find_class(h.el("s1", "pf"), "pf-menu").is_empty(),
        "the menu closed"
    );
    let field = first(&h, row(&h, "host"), "pf-in");
    assert!(h.class(field).contains("editing"));
    assert_eq!(h.text(first(&h, field, "pre")), "h\u{1f600}");
    assert_eq!(h.text(first(&h, field, "post")), "st");
    assert_eq!(h.find_class(field, "pf-caret").len(), 1);
    // A multi row's highlight on `option`.
    set(
        &mut h,
        4,
        json!({"editing": {"row": "notify", "option": "pager"}}),
    );
    let sel: Vec<String> = h
        .find_class(row(&h, "notify"), "sel")
        .into_iter()
        .map(|c| h.text(c))
        .collect();
    assert_eq!(sel, ["Pager"]);
    set(&mut h, 5, json!({"editing": null}));
    assert!(h.find_class(h.el("s1", "pf"), "editing").is_empty());
}

#[test]
fn prefs_keeps_its_scroll_slides_switches_and_follows_the_focus() {
    let mut h = harness();
    open_page(&mut h, json!({}));
    let pf = h.el("s1", "pf");
    let scroll = first(&h, pf, "pf-scroll");
    let sw = first(&h, row(&h, "auto"), "pf-sw");
    // Marks to tell kept elements from new ones (node ids are reused).
    h.doc.set_attribute(sw, "data-mark", "kept");
    h.doc.set_attribute(scroll, "data-mark", "kept");
    // The program flips the switch: the same element changes class, so the knob slides.
    let mut sections = page(json!({}))["sections"].clone();
    sections[0]["rows"][0]["control"]["on"] = json!(false);
    set(&mut h, 2, json!({"sections": sections}));
    let sw2 = first(&h, row(&h, "auto"), "pf-sw");
    assert_eq!(
        h.attr(sw2, "data-mark").as_deref(),
        Some("kept"),
        "the switch element is kept"
    );
    assert!(!h.class(sw2).split(' ').any(|c| c == "on"));
    // Focusing the last row scrolls it into view (nearest edge); the scroll survives updates.
    let (_, max) = h.doc.max_scroll(scroll);
    assert!(max > 0.0, "the document scrolls in an 800px viewport");
    set(&mut h, 3, json!({"focus": "old"}));
    let (_, y) = h.doc.scroll_offset(scroll);
    let sr = h.doc.border_box(scroll).unwrap();
    let rr = h.doc.border_box(row(&h, "old")).unwrap();
    assert!(
        y > 0.0 && rr.y + rr.h <= sr.y + sr.h + 0.5 && rr.y >= sr.y,
        "{y} {rr:?} {sr:?}"
    );
    set(&mut h, 4, json!({"lead": "Another lead."}));
    let scroll2 = first(&h, pf, "pf-scroll");
    assert_eq!(
        h.attr(scroll2, "data-mark").as_deref(),
        Some("kept"),
        "the scroller is kept"
    );
    assert_eq!(h.doc.scroll_offset(scroll).1, y, "and so is its offset");
    // A page change scrolls to the top and replays the arrive motion (a new .pf-doc).
    set(&mut h, 5, json!({"page": "targets"}));
    let doc2 = first(&h, pf, "pf-doc");
    assert!(h.class(doc2).contains("arrive"));
    assert_eq!(h.doc.scroll_offset(scroll).1, 0.0);
    assert_eq!(h.text(first(&h, pf, "pf-eyebrow")), "02 \u{b7} Targets");
}

#[test]
fn prefs_search_results_group_by_page_and_mark_matches() {
    let mut h = harness();
    let sections = json!([
        {"id": "general/run", "title": "Runs", "page": "general", "rows": [
            {"id": "auto", "label": "Deploy on push", "hint": "Push to deploy.", "control": {"k": "switch", "on": true}}]},
        {"id": "lab/x", "title": "Lab", "page": "lab", "rows": [
            {"id": "x", "label": "Push harder", "control": {"k": "switch", "on": false}}]}
    ]);
    open_page(
        &mut h,
        json!({"query": "push ", "cursor": 4, "sections": sections}),
    );
    let pf = h.el("s1", "pf");
    let q = first(&h, first(&h, pf, "pf-search"), "q");
    assert_eq!(h.text(first(&h, q, "pre")), "push");
    assert_eq!(h.text(first(&h, q, "post")), " ");
    assert!(h.class(first(&h, pf, "pf-head")).contains("pf-found"));
    let parts = h.find_class(pf, "pf-part-h");
    assert_eq!(parts.len(), 2);
    assert!(
        h.class(parts[1]).contains("off"),
        "a disabled page's heading"
    );
    assert!(click(&mut h, parts[1]).is_empty());
    assert_eq!(
        bodies(&click(&mut h, parts[0])),
        [json!({"ev": "action", "sf": "s1", "id": "pf", "act": "page", "value": "general"})]
    );
    let marks: Vec<String> = descendants(&h.doc, pf)
        .into_iter()
        .filter(|&e| {
            h.doc
                .node(e)
                .element()
                .is_some_and(|el| el.tag.as_str() == "mark")
        })
        .map(|e| h.text(e))
        .collect();
    assert_eq!(marks, ["push", "Push", "Push"]);
    set(&mut h, 2, json!({"query": "zzz", "sections": []}));
    assert_eq!(
        h.text(first(&h, pf, "pf-none")),
        "No settings match \u{201c}zzz\u{201d}"
    );
}

fn descendants(doc: &Document, el: NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    let mut stack = vec![el];
    while let Some(n) = stack.pop() {
        out.push(n);
        for &c in doc.children(n).iter().rev() {
            stack.push(c);
        }
    }
    out
}

#[test]
fn prefs_collapses_its_nav_to_a_rail_below_760px() {
    let mut h = harness();
    open_page(&mut h, json!({}));
    h.view.set_cols(&mut h.doc, &h.pane, 90);
    h.frame();
    let pf = h.el("s1", "pf");
    let root = first(&h, pf, "pf");
    assert_eq!(h.class(root), "pf on rail");
    let nav = first(&h, root, "pf-nav");
    assert!(h.doc.border_box(nav).unwrap().w < 60.0);
}

/// Inline surface with `main` text and `layer` holding a prefs sheet.
fn open_sheet(h: &mut Harness) {
    h.apc("o", r#"{"id":"s1","mode":"inline"}"#);
    h.frame_ops(
        "s1",
        1,
        json!([
            ["add", "main", "s1", null, {"id": "main", "k": "col", "c": [{"id": "t", "k": "text", "p": {"text": "transcript"}}]}],
            ["add", "layer", "s1", null, {"id": "layer", "k": "col"}]
        ]),
    );
    h.frame();
    let grid: &dyn auk_tsp::Grid = &h.grid;
    h.pane.ack(grid, "s1", 1);
    let out = h.pane.take_output();
    h.sent.extend(common::decode(&out));
}

#[test]
fn prefs_sheet_docks_beside_an_inline_surface_and_narrows_the_program() {
    let mut h = harness();
    open_sheet(&mut h);
    let resizes = |h: &Harness| -> Vec<u64> {
        h.sent
            .iter()
            .filter(|e| e["ev"] == "resize")
            .map(|e| e["cols"].as_u64().unwrap())
            .collect()
    };
    assert_eq!(resizes(&h), [120]);
    h.frame_ops(
        "s1",
        2,
        json!([["add", "pf", "layer", null, {"id": "pf", "k": "prefs", "p": page(json!({}))}]]),
    );
    settle(&mut h);
    // The program lays out beside the sheet: 120 - 56 columns.
    assert_eq!(resizes(&h), [120, 64]);
    let pane = h.view.element();
    assert!(h.class(pane).contains("auk-aside"));
    let pf = h.el("s1", "pf");
    let root = first(&h, pf, "pf");
    assert_eq!(h.class(root), "pf on sheet");
    let sheet = h.doc.border_box(pf).unwrap();
    assert!((sheet.w - 448.0).abs() < 0.5, "56 cells of 8pt: {sheet:?}");
    assert!(
        (sheet.x + sheet.w - 960.0).abs() < 0.5,
        "docked at the right edge: {sheet:?}"
    );
    assert!(sheet.h >= 799.0, "the pane's height: {sheet:?}");
    // Every row fits the sheet: its label keeps room and its control ends inside the row.
    for r in h.find_class(root, "pf-row") {
        let rb = h.doc.border_box(r).unwrap();
        let c = h.doc.border_box(first(&h, r, "pf-ctl")).unwrap();
        let l = h.doc.border_box(first(&h, r, "pf-lbl")).unwrap();
        let id = h.attr(r, "data-row").unwrap();
        assert!(
            rb.x >= sheet.x && rb.x + rb.w <= sheet.x + sheet.w + 0.5,
            "{id} {rb:?}"
        );
        assert!(
            c.x + c.w <= rb.x + rb.w + 0.5,
            "{id}: control {c:?} in row {rb:?}"
        );
        assert!(l.w >= 60.0, "{id}: label {l:?}");
    }
    // main ends beside it.
    let main = h
        .view
        .region(h.pane.surfaces()[0].key(), auk_views::Region::Main)
        .unwrap();
    let m = h.doc.border_box(main).unwrap();
    assert!(m.x + m.w <= sheet.x + 0.5, "{m:?} {sheet:?}");
    // The sheet's brand row has the close button; the pages are an icon strip.
    let close = first(&h, root, "pf-close");
    assert_eq!(
        bodies(&click(&mut h, close)),
        [json!({"ev": "action", "sf": "s1", "id": "pf", "act": "close"})]
    );
    let rows = h.find_class(root, "nav-row");
    let (a, b) = (
        h.doc.border_box(rows[0]).unwrap(),
        h.doc.border_box(rows[1]).unwrap(),
    );
    assert!((a.y - b.y).abs() < 0.5 && b.x > a.x, "a strip: {a:?} {b:?}");
    // The sheet goes: the width comes back.
    h.frame_ops("s1", 3, json!([["del", "pf"]]));
    h.frame();
    assert_eq!(resizes(&h), [120, 64, 120]);
    assert!(!h.class(h.view.element()).contains("auk-aside"));
}

#[test]
fn prefs_sheet_paints_its_look() {
    let mut h = harness();
    open_sheet(&mut h);
    h.frame_ops(
        "s1",
        2,
        json!([["add", "pf", "layer", null, {"id": "pf", "k": "prefs", "p": page(json!({"focus": "host"}))}]]),
    );
    settle(&mut h);
    let pf = h.el("s1", "pf");
    let root = first(&h, pf, "pf");
    let (w, ht) = (960u32, 800u32);
    let px = render(&h.doc, w, ht);
    let panel = theme_rgb(&h.doc, root, "--panel");
    let accent_fill = theme_rgb(&h.doc, root, "--accent-fill");
    let accent = theme_rgb(&h.doc, root, "--accent");
    // The sheet's background: --panel, from the left edge of the sheet to the right.
    let s = h.doc.border_box(root).unwrap();
    let nav = h.doc.border_box(first(&h, root, "pf-nav")).unwrap();
    let p = pixel(&px, w, s.x + s.w - 6.0, nav.y + nav.h - 3.0);
    assert!(near(p, panel), "sheet {p:?} vs --panel {panel:?}");
    // A switch that is on: --accent-fill left of its knob.
    let sw = h
        .doc
        .border_box(first(&h, row(&h, "auto"), "pf-sw"))
        .unwrap();
    let p = pixel(&px, w, sw.x + 6.0, sw.y + sw.h / 2.0);
    assert!(
        near(p, accent_fill),
        "switch {p:?} vs --accent-fill {accent_fill:?}"
    );
    // The focus ring: --accent on the focused row's left edge.
    let r = h.doc.border_box(row(&h, "host")).unwrap();
    let p = pixel(&px, w, r.x + 0.5, r.y + r.h / 2.0);
    assert!(near(p, accent), "ring {p:?} vs --accent {accent:?}");
    // Left of the sheet the pane shows through (the page's background, not the sheet's).
    let page_bg = theme_rgb(&h.doc, root, "--page");
    let p = pixel(&px, w, s.x - 20.0, 700.0);
    assert!(
        near(p, page_bg),
        "beside the sheet {p:?} vs --page {page_bg:?}"
    );
}

#[test]
fn omp_settings_fixture_draws_and_round_trips() {
    // omp 18.8.5's /settings: focus moves, a choice menu opens, moves and closes, a page switch,
    // a text row's edit, then Esc twice. Checked after the frames that matter.
    let mut seen = Vec::new();
    let h = replay("omp-settings.jsonl", |h, rec| {
        if rec.verb != "f" || rec.body["sf"] != "s:1" {
            return;
        }
        let s = rec.body["s"].as_u64().unwrap();
        h.view.set_cols(&mut h.doc, &h.pane, 120);
        h.frame();
        let Some(key) = h
            .pane
            .surfaces()
            .iter()
            .find(|x| x.id() == "s:1")
            .map(|x| x.key())
        else {
            return;
        };
        let Some(pf) = h.view.node_element(key, "v") else {
            seen.push((s, None));
            return;
        };
        let root = first(h, pf, "pf");
        let focus: Vec<String> = h
            .find_class(pf, "focus")
            .into_iter()
            .filter_map(|e| h.attr(e, "data-row"))
            .collect();
        let menu: Vec<String> = h
            .find_class(pf, "mi")
            .into_iter()
            .filter(|&e| h.class(e).contains("sel"))
            .filter_map(|e| h.attr(e, "data-v"))
            .collect();
        let draft = h
            .find_class(pf, "pf-in")
            .into_iter()
            .any(|e| h.class(e).contains("editing"));
        seen.push((
            s,
            Some((
                h.class(root),
                h.text(first(h, root, "pf-title")),
                focus,
                menu,
                draft,
            )),
        ));
    });
    assert!(h.errors().is_empty(), "{:?}", h.errors());
    let at = |s: u64| {
        seen.iter()
            .find(|(x, _)| *x == s)
            .and_then(|(_, v)| v.clone())
            .unwrap_or_else(|| panic!("frame {s}: {seen:?}"))
    };
    let (class, title, focus, menu, _) = at(6);
    assert_eq!(class, "pf on sheet");
    assert_eq!(title, "Appearance");
    assert_eq!(focus, ["theme.dark"]);
    assert!(menu.is_empty());
    assert_eq!(at(8).2, ["symbolPreset"]);
    assert_eq!(at(9).3, ["unicode"], "the menu opens on the current value");
    assert_eq!(at(10).3, ["nerd"], "the highlight follows the program");
    assert!(at(11).3.is_empty(), "Esc closed it");
    assert_eq!(at(12).1, "Shell");
    assert!(at(13).4, "the text row shows its draft");
    assert!(!at(14).4);
    assert!(
        seen.iter().any(|(s, v)| *s == 15 && v.is_none()),
        "Esc closed the sheet"
    );
    // The aside: omp heard the narrower width while the sheet was up, then the full width.
    let cols: Vec<u64> = h
        .sent
        .iter()
        .filter(|e| e["ev"] == "resize")
        .map(|e| e["cols"].as_u64().unwrap())
        .collect();
    assert_eq!(cols, [120, 64, 120]);
    assert!(!h.class(h.view.element()).contains("auk-aside"));
}

#[test]
fn omp_settings_status_preview_and_page_click() {
    // Stop at the first settings frame: the status-line preview child sits after its section,
    // and clicking a nav row sends what the recording's own event sent.
    let (path, records) = common::fixture("omp-settings.jsonl");
    let mut h = Harness::with(common::config_for(&path, &records), ViewSettings::default());
    h.view.set_cols(&mut h.doc, &h.pane, 120);
    let mut recorded_click = None;
    for rec in &records {
        if rec.dir == auk_conformance::Dir::In && rec.verb == "e" && rec.body["ev"] == "action" {
            recorded_click = Some(rec.body.clone());
            break;
        }
        if rec.dir == auk_conformance::Dir::Out && rec.verb != "osc" {
            let body = rec.body.to_string();
            h.pane
                .on_apc(&mut h.grid, &common::payload(&rec.verb, &[], &body));
            h.sync();
            h.frame();
        }
    }
    let key = h
        .pane
        .surfaces()
        .iter()
        .find(|x| x.id() == "s:1")
        .unwrap()
        .key();
    let pf = h.view.node_element(key, "v").expect("the settings sheet");
    let sec = h
        .find_class(pf, "pf-sec")
        .into_iter()
        .find(|&e| h.attr(e, "data-sec").as_deref() == Some("status-line"))
        .expect("the status-line section");
    let preview = first(&h, sec, "pf-preview");
    let status = h.view.node_element(key, "v.status-preview").unwrap();
    assert_eq!(
        h.doc.parent(status),
        Some(preview),
        "the preview goes after its section"
    );
    let shell = h
        .find_class(pf, "nav-row")
        .into_iter()
        .find(|&e| h.attr(e, "data-v").as_deref() == Some("shell"))
        .unwrap();
    settle(&mut h);
    let (x, y) = center(&h, shell);
    let out = h.view.click(
        &mut h.doc,
        &h.pane,
        PointerEvent {
            x,
            y,
            clicks: 1,
            ..Default::default()
        },
    );
    let evs: Vec<Value> = out
        .into_iter()
        .filter_map(|o| match o {
            Outcome::Event { event, .. } => Some(event.body("s:1")),
            _ => None,
        })
        .collect();
    // omp moves its focus off the composer while the sheet is up, so no `focus` comes first.
    let surface = h.pane.surfaces().iter().find(|x| x.id() == "s:1").unwrap();
    assert_eq!(surface.document().focus(), None);
    assert_eq!(evs, [recorded_click.expect("the recording clicked a page")]);
}

// ---------------------------------------------------------------------------------------------
// Offscreen rendering (as in tests/paint.rs)

fn render(doc: &Document, w: u32, h: u32) -> Vec<u8> {
    let mut scene = vello::Scene::new();
    auk_ui::paint(doc, &mut scene);
    let instance =
        vello::wgpu::Instance::new(vello::wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(
        instance.request_adapter(&vello::wgpu::RequestAdapterOptions::default()),
    )
    .expect("a wgpu adapter for offscreen rendering");
    let (device, queue) =
        pollster::block_on(adapter.request_device(&vello::wgpu::DeviceDescriptor::default()))
            .expect("a wgpu device");
    let mut renderer =
        vello::Renderer::new(&device, vello::RendererOptions::default()).expect("a renderer");
    let texture = device.create_texture(&vello::wgpu::TextureDescriptor {
        label: Some("target"),
        size: vello::wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: vello::wgpu::TextureDimension::D2,
        format: vello::wgpu::TextureFormat::Rgba8Unorm,
        usage: vello::wgpu::TextureUsages::STORAGE_BINDING | vello::wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&Default::default());
    renderer
        .render_to_texture(
            &device,
            &queue,
            &scene,
            &view,
            &vello::RenderParams {
                base_color: vello::peniko::Color::new([1.0, 1.0, 1.0, 1.0]),
                width: w,
                height: h,
                antialiasing_method: vello::AaConfig::Area,
            },
        )
        .expect("render");
    let padded = (w * 4).div_ceil(256) * 256;
    let buffer = device.create_buffer(&vello::wgpu::BufferDescriptor {
        label: None,
        size: (padded * h) as u64,
        usage: vello::wgpu::BufferUsages::MAP_READ | vello::wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&Default::default());
    enc.copy_texture_to_buffer(
        texture.as_image_copy(),
        vello::wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: vello::wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: None,
            },
        },
        vello::wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([enc.finish()]);
    let slice = buffer.slice(..);
    slice.map_async(vello::wgpu::MapMode::Read, |_| {});
    device
        .poll(vello::wgpu::PollType::wait_indefinitely())
        .expect("poll");
    let data = slice.get_mapped_range().expect("mapped");
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for row in 0..h {
        let start = (row * padded) as usize;
        out.extend_from_slice(&data[start..start + (w * 4) as usize]);
    }
    out
}

fn pixel(px: &[u8], w: u32, x: f32, y: f32) -> [u8; 3] {
    let i = ((y as u32 * w + x as u32) * 4) as usize;
    [px[i], px[i + 1], px[i + 2]]
}

fn near(a: [u8; 3], b: [u8; 3]) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 4)
}

/// The rgb of a `#rrggbb` theme variable as the element sees it.
fn theme_rgb(doc: &Document, el: NodeId, var: &str) -> [u8; 3] {
    let style = doc.computed_style(el).unwrap();
    let v = style
        .custom
        .get(var)
        .unwrap_or_else(|| panic!("{var} is set"))
        .trim()
        .to_owned();
    let hex = v
        .strip_prefix('#')
        .unwrap_or_else(|| panic!("{var} is a hex color: {v}"));
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).unwrap();
    [byte(0), byte(2), byte(4)]
}

/// The editor card's facts once laid out: its class, its box, the picker sheet's box and the
/// boxes of the rows drawn in it.
fn editor_card(h: &Harness, pf: NodeId) -> (String, auk_ui::Rect, auk_ui::Rect, Vec<auk_ui::Rect>) {
    let root = first(h, pf, "pf");
    let card = first(h, root, "pf-editor");
    let sheet = first(h, card, "pk-sheet");
    let rows = h
        .find_class(sheet, "pk-row")
        .into_iter()
        .filter(|&r| !h.class(r).contains("grp"))
        .filter_map(|r| h.doc.border_box(r))
        .collect();
    (
        h.class(card),
        h.doc
            .border_box(card)
            .unwrap_or_else(|| panic!("the card is laid out: {}", h.class(card))),
        h.doc.border_box(sheet).expect("the sheet is laid out"),
        rows,
    )
}

/// A picker in the card fills it: the card is md-sheet tall (or the page's room), the sheet
/// covers it edge to edge, and rows show inside it.
fn assert_picker_fills_the_card(
    page: auk_ui::Rect,
    card: auk_ui::Rect,
    sheet: auk_ui::Rect,
    rows: &[auk_ui::Rect],
) {
    let want = 540f32.min(page.h - 80.0);
    assert!(
        (card.h - want).abs() < 1.0,
        "card {card:?} in page {page:?}"
    );
    // The card keeps its 16px insets (at most 600px wide).
    assert!(card.w > 300.0 && card.w <= 600.5, "{card:?}");
    assert!(
        card.x >= page.x + 15.0 && card.x + card.w <= page.x + page.w - 15.0,
        "{card:?} {page:?}"
    );
    // The sheet covers the card inside its 1px border.
    for (a, b) in [
        (sheet.x, card.x + 1.0),
        (sheet.y, card.y + 1.0),
        (sheet.w, card.w - 2.0),
        (sheet.h, card.h - 2.0),
    ] {
        assert!((a - b).abs() < 1.0, "sheet {sheet:?} vs card {card:?}");
    }
    assert!(rows.len() >= 8, "{rows:?}");
    for r in rows.iter().filter(|r| r.y + r.h <= card.y + card.h) {
        assert!(
            r.x >= card.x - 0.5 && r.x + r.w <= card.x + card.w + 0.5,
            "row {r:?} in {card:?}"
        );
    }
}

#[test]
fn omp_theme_submenu_picker_fills_the_editor_card() {
    // omp 18.8.5's /settings with Enter on Dark Theme (102 themes, over omp's 12): the submenu is
    // an md picker child of the prefs node, drawn in the editor card over the docked sheet.
    let mut seen = None;
    let h = replay("omp-settings-theme.jsonl", |h, rec| {
        if rec.verb != "f" || rec.body["sf"] != "s:1" || rec.body["s"] != 7 {
            return;
        }
        h.view.set_cols(&mut h.doc, &h.pane, 120);
        settle(h);
        let key = h
            .pane
            .surfaces()
            .iter()
            .find(|x| x.id() == "s:1")
            .unwrap()
            .key();
        let pf = h.view.node_element(key, "v").expect("the settings sheet");
        let page = h.doc.border_box(first(h, pf, "pf")).unwrap();
        let picker = h.view.node_element(key, "w").expect("the theme picker");
        let card = first(h, first(h, pf, "pf"), "pf-editor");
        assert!(
            std::iter::successors(Some(picker), |&e| h.doc.parent(e)).any(|e| e == card),
            "the picker is in the card"
        );
        seen = Some((page, editor_card(h, pf)));
    });
    assert!(h.errors().is_empty(), "{:?}", h.errors());
    let (page, (class, card, sheet, rows)) = seen.expect("frame 7 drew the submenu");
    assert_eq!(class, "pf-editor on pk");
    assert_picker_fills_the_card(page, card, sheet, &rows);
}

#[test]
fn prefs_editor_column_holding_a_picker_fills_the_card() {
    let mut h = harness();
    let items: Vec<Value> = (0..30)
        .map(|i| json!({"id": format!("t{i}"), "label": format!("theme {i}")}))
        .collect();
    open_page(&mut h, json!({}));
    h.frame_ops(
        "s1",
        2,
        json!([["add", "ed", "pf", null, {"id": "ed", "k": "col", "p": {"role": "omp.prefs.editor"}, "c": [
            {"id": "pk", "k": "picker", "p": {"size": "md", "title": "Theme", "query": "", "items": items, "selected": "t3"}}
        ]}]]),
    );
    settle(&mut h);
    assert!(h.errors().is_empty(), "{:?}", h.errors());
    let pf = h.el("s1", "pf");
    let page = h.doc.border_box(first(&h, pf, "pf")).unwrap();
    let (class, card, sheet, rows) = editor_card(&h, pf);
    assert_eq!(class, "pf-editor on pk");
    assert_picker_fills_the_card(page, card, sheet, &rows);
    assert!(
        (card.w - 600.0).abs() < 1.0,
        "a page's card is 600 wide: {card:?}"
    );
    // A plain editor child keeps the card sized by its content.
    h.frame_ops(
        "s1",
        3,
        json!([["del", "ed"], ["add", "ed2", "pf", null, {"id": "ed2", "k": "col", "p": {"role": "omp.prefs.editor"}, "c": [
            {"id": "x", "k": "text", "p": {"text": "an editor"}}
        ]}]]),
    );
    settle(&mut h);
    let root = first(&h, pf, "pf");
    let card = first(&h, root, "pf-editor");
    assert_eq!(h.class(card), "pf-editor on");
    assert!(h.doc.border_box(card).unwrap().h < 80.0);
}

/// Under "Use program colors" (.sf-pal) the page's kit accents follow the program's accent:
/// `--accent` is it, `--accent-ink` reads on the page, `--accent-fill`
/// carries white; an on switch paints the program's fill. Without the setting, the theme's.
#[test]
fn prefs_accents_follow_the_program_palette() {
    let mut h = harness();
    open_page(&mut h, json!({}));
    h.apc(
        "t",
        r##"{"dark":{"accent":"#e0a000"},"light":{"accent":"#e0a000"}}"##,
    );
    settle(&mut h);
    let pf = h.el("s1", "pf");
    let root = first(&h, pf, "pf");
    let var = |h: &Harness, el: NodeId, v: &str| -> String {
        h.doc
            .computed_style(el)
            .unwrap()
            .custom
            .get(v)
            .map(|s| s.trim().to_owned())
            .unwrap_or_default()
    };
    assert_eq!(var(&h, root, "--accent"), "#e0a000");
    let ink = var(&h, root, "--accent-ink");
    let fill = var(&h, root, "--accent-fill");
    // On the dark page the amber already reads as text; as a fill it darkens for white.
    assert_eq!(ink, "#e0a000");
    assert!(
        fill.starts_with('#') && fill != "#e0a000",
        "darkened for white: {fill}"
    );
    let sw = first(&h, row(&h, "auto"), "pf-sw");
    let bg = h
        .doc
        .computed_style(sw)
        .unwrap()
        .background_color
        .to_rgba8();
    assert_eq!(format!("#{:02x}{:02x}{:02x}", bg[0], bg[1], bg[2]), fill);
    // Outside the page the theme's accent stays.
    let main = h.doc.parent(pf).unwrap();
    assert_ne!(var(&h, main, "--accent"), "#e0a000");
    // Without "Use program colors": the theme's accents.
    let s = ViewSettings {
        program_colors: false,
        ..h.view.settings().clone()
    };
    h.view.set_settings(&mut h.doc, &h.pane, s);
    settle(&mut h);
    let root = first(&h, h.el("s1", "pf"), "pf");
    assert_ne!(var(&h, root, "--accent"), "#e0a000");
    assert!(var(&h, root, "--accent-fill").starts_with('#'));
}

/// An ordered multi list reorders by dragging a grip onto another item:
/// before it, or after it on its lower half; the dragged item is enabled, and the new order of
/// enabled values goes out as `change`. The dragged item shows `.drag` and follows the pointer.
#[test]
fn prefs_ordered_list_reorders_by_dragging_a_grip() {
    let mut h = harness();
    open_page(&mut h, json!({}));
    settle(&mut h);
    let order_row = row(&h, "order");
    let item = |h: &Harness, v: &str| {
        h.find_class(order_row, "pf-oi")
            .into_iter()
            .find(|&e| h.attr(e, "data-v").as_deref() == Some(v))
            .unwrap()
    };
    let drawn: Vec<String> = h
        .find_class(order_row, "pf-oi")
        .into_iter()
        .filter_map(|e| h.attr(e, "data-v"))
        .collect();
    assert_eq!(
        drawn,
        ["b", "a", "c"],
        "enabled ones first, in values order"
    );
    let drag = |h: &mut Harness, from: &str, onto: &str, lower: bool| -> Vec<Value> {
        let g = first(h, item(h, from), "pf-grip");
        let (gx, gy) = center(h, g);
        let t = h.doc.border_box(item(h, onto)).unwrap();
        let ty = if lower {
            t.y + t.h * 0.75
        } else {
            t.y + t.h * 0.25
        };
        assert!(
            h.view.reorder_press(&mut h.doc, &h.pane, gx, gy),
            "the grip starts a drag"
        );
        assert!(h.class(item(h, from)).contains("drag"));
        assert!(h.view.reorder_drag(&mut h.doc, gx, ty));
        let moved = h
            .view
            .written()
            .style(item(h, from))
            .unwrap_or("")
            .to_owned();
        assert!(moved.contains("translateY"), "{moved}");
        let out = h
            .view
            .reorder_release(&mut h.doc, gx, ty)
            .expect("a drag was under way");
        assert!(!h.class(item(h, from)).contains("drag"));
        out.into_iter()
            .filter_map(|o| match o {
                Outcome::Event { event, .. } => Some(event.body("s1")),
                _ => None,
            })
            .collect()
    };
    let change =
        |v: Value| json!({"ev": "change", "sf": "s1", "id": "pf", "item": "order", "value": v});
    // The disabled Gamma dropped on Beta's upper half: first, and enabled.
    assert_eq!(
        drag(&mut h, "c", "b", false),
        [change(json!(["c", "b", "a"]))]
    );
    // Beta dropped on Alpha's lower half: after it.
    assert_eq!(drag(&mut h, "b", "a", true), [change(json!(["a", "b"]))]);
    // Onto itself, or where it already is: nothing.
    assert!(drag(&mut h, "a", "a", true).is_empty());
    assert!(drag(&mut h, "b", "a", false).is_empty());
    // A click on a grip sends nothing (the press is a drag's).
    let g = first(&h, item(&h, "a"), "pf-grip");
    assert!(click(&mut h, g).is_empty());
    // Off a grip, no drag starts.
    let (x, y) = center(&h, first(&h, item(&h, "a"), "t"));
    assert!(!h.view.reorder_press(&mut h.doc, &h.pane, x, y));
    assert!(h.view.reorder_release(&mut h.doc, x, y).is_none());
}

/// While the structure holds (page, query, sections, row ids, control kinds and
/// options), rows patch in place: a flip redraws only its row, the others keep their elements;
/// a new option set (structure) rebuilds the document.
#[test]
fn prefs_rows_patch_in_place_while_the_structure_holds() {
    let mut h = harness();
    open_page(&mut h, json!({}));
    let mark = |h: &mut Harness, ids: &[&str]| {
        for id in ids {
            let r = row(h, id);
            h.doc.set_attribute(r, "data-mark", "kept");
        }
    };
    let kept = |h: &Harness, id: &str| h.attr(row(h, id), "data-mark").as_deref() == Some("kept");
    mark(&mut h, &["auto", "env", "jobs", "host"]);
    let pf = h.el("s1", "pf");
    let head = first(&h, pf, "pf-head");
    h.doc.set_attribute(head, "data-mark", "kept");
    // A flip and a value change: those rows redraw, the rest stay.
    let mut sections = page(json!({}))["sections"].clone();
    sections[0]["rows"][0]["control"]["on"] = json!(false);
    sections[0]["rows"][3]["control"]["value"] = json!(4);
    set(&mut h, 2, json!({"sections": sections.clone()}));
    assert!(!kept(&h, "auto") && !kept(&h, "jobs"));
    assert!(
        kept(&h, "env") && kept(&h, "host"),
        "unchanged rows keep their elements"
    );
    assert_eq!(
        h.attr(first(&h, pf, "pf-head"), "data-mark").as_deref(),
        Some("kept")
    );
    assert_eq!(h.text(first(&h, row(&h, "jobs"), "v")), "4");
    // The focus ring moves: only the two rows involved redraw.
    mark(&mut h, &["auto", "jobs"]);
    set(&mut h, 3, json!({"focus": "host"}));
    assert!(kept(&h, "auto") && kept(&h, "jobs") && kept(&h, "env"));
    assert!(h.class(row(&h, "host")).contains("focus"));
    // New options are a new structure: the document rebuilds.
    sections[0]["rows"][1]["control"]["options"] = json!([{"value": "dev"}, {"value": "qa"}]);
    set(&mut h, 4, json!({"sections": sections}));
    assert!(!kept(&h, "env") && !kept(&h, "auto"));
    assert!(h.attr(first(&h, pf, "pf-head"), "data-mark").is_none());
}

/// A row's `act` holding `=` reaches the program whole, with the row as its value.
#[test]
fn prefs_action_names_holding_an_equals_sign_stay_whole() {
    let mut h = harness();
    let mut sections = page(json!({}))["sections"].clone();
    sections[0]["rows"][9]["control"]["act"] = json!("open=log");
    open_page(&mut h, json!({"sections": sections}));
    settle(&mut h);
    let b = first(&h, row(&h, "logs"), "pf-act");
    assert_eq!(
        bodies(&click(&mut h, b)),
        [json!({"ev": "action", "sf": "s1", "id": "pf", "act": "open=log", "value": "logs"})]
    );
}

/// Search marks hold when a character's lowercase has another length (`İ` lowercases to two
/// characters): the match maps back onto the label's own characters.
#[test]
fn prefs_search_marks_survive_case_changing_lengths() {
    let mut h = harness();
    open_page(
        &mut h,
        json!({"query": "stanbul", "sections": [{"id": "s", "page": "general", "rows": [
            {"id": "city", "label": "İstanbul office", "control": {"k": "switch", "on": true}}
        ]}]}),
    );
    let marks: Vec<String> = h
        .find_class(row(&h, "city"), "t")
        .into_iter()
        .flat_map(|t| h.doc.element_children(t))
        .filter(|&e| {
            h.doc
                .node(e)
                .element()
                .is_some_and(|el| el.tag.as_str() == "mark")
        })
        .map(|e| h.text(e))
        .collect();
    assert_eq!(marks, ["stanbul"]);
    assert_eq!(h.text(first(&h, row(&h, "city"), "t")), "İstanbul office");
}

/// The rail threshold is 760 CSS px of the region, its zoom (font/13) taken out: at a 26px font
/// 60 cells of 16px are 960 device px but 480 CSS px, a rail. An open menu's `--pf-room` is
/// the room its row's control has, not a fixed 360px.
#[test]
fn prefs_rail_threshold_and_menu_room_follow_the_zoom_and_width() {
    let mut h = harness();
    let settings = ViewSettings {
        font_px: 26.0,
        cell: (16.0, 34.0),
        ..h.view.settings().clone()
    };
    h.view.set_settings(&mut h.doc, &h.pane, settings);
    h.view.set_cols(&mut h.doc, &h.pane, 60);
    open_page(&mut h, json!({}));
    let pf = h.el("s1", "pf");
    assert_eq!(h.class(first(&h, pf, "pf")), "pf on rail");
    // A wide page at the default font: the menu's room is the row's control share.
    let mut h = harness();
    h.view.set_cols(&mut h.doc, &h.pane, 160);
    open_page(&mut h, json!({"editing": {"row": "region"}}));
    let menu = first(&h, row(&h, "region"), "pf-menu");
    let style = h.view.written().style(menu).unwrap_or("").to_owned();
    let room: f32 = style
        .trim_start_matches("--pf-room: ")
        .trim_end_matches("px")
        .parse()
        .unwrap_or_else(|_| panic!("{style}"));
    // 1280 px: nav 220, document 760 less its 56px of padding, the row's 32px and the label's.
    assert!(
        (room - (760.0 - 56.0 - 32.0 - 12.0 - 140.0)).abs() < 0.5,
        "{room}"
    );
    let mut h = harness();
    h.view.set_cols(&mut h.doc, &h.pane, 100);
    open_page(&mut h, json!({"editing": {"row": "region"}}));
    let menu = first(&h, row(&h, "region"), "pf-menu");
    let narrow = h.view.written().style(menu).unwrap_or("").to_owned();
    assert_ne!(narrow, style, "a narrower page leaves the menu less room");
}
