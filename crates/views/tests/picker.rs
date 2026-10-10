//! `picker` (T2, amendment A23; book:6274-6528, research/elements.md §2.4, §6.4): omp's modal
//! sheet drawn from its props: the head with a drawn search line, scopes, tabs, fixed-height rows
//! with facts, the preview, the strip above the action bar and the bar; pointer input as
//! `select`, `activate` and `action` events; keys stay the program's. Checked on synthetic sheets
//! and on omp 18.8.5's own model hub and model dropdown (`fixtures/tsp/omp-picker.jsonl`).

mod common;

use auk_tsp::ViewEvent;
use auk_ui::{Document, NodeId};
use auk_views::{Outcome, PointerEvent};
use common::{Harness, replay};
use serde_json::{Value, json};

// ---------------------------------------------------------------------------------------------
// Helpers

fn harness(cols: u16) -> Harness {
    let mut h = Harness::new();
    h.hello(&[]);
    h.view.set_cols(&mut h.doc, &h.pane, cols);
    h
}

/// Opens an inline surface with a line of text in `main` and `picker` (id `pk`) in the layer.
fn open(h: &mut Harness, props: Value, preview: Value) {
    h.surface(
        "s1",
        "inline",
        json!([{"id": "t", "k": "text", "p": {"text": "hello"}}]),
    );
    h.frame_ops(
        "s1",
        2,
        json!([["add", "layer", "s1", null, {"id": "layer", "k": "col", "c": [
            {"id": "pk", "k": "picker", "p": props, "c": preview}
        ]}]]),
    );
    h.frame();
    assert!(h.errors().is_empty(), "{:?}", h.errors());
    settle(h);
}

/// Lets the sheet-in animation finish (its transform would move hit points off the boxes).
fn settle(h: &mut Harness) {
    h.now += 1000.0;
    h.frame();
    let el = h.el("s1", "pk");
    let sheet = one(h, el, "pk-sheet");
    let t = h.doc.computed_style(sheet).map(|s| s.opacity);
    assert_eq!(t, Some(1.0), "the sheet-in finished");
}

fn set(h: &mut Harness, s: u64, props: Value) {
    h.frame_ops("s1", s, json!([["set", "pk", props]]));
    h.frame();
    assert!(h.errors().is_empty(), "{:?}", h.errors());
}

fn one(h: &Harness, root: NodeId, class: &str) -> NodeId {
    let found = h.find_class(root, class);
    assert_eq!(found.len(), 1, "one .{class}, found {}", found.len());
    found[0]
}

fn first(h: &Harness, root: NodeId, class: &str) -> NodeId {
    *h.find_class(root, class)
        .first()
        .unwrap_or_else(|| panic!("no .{class}"))
}

fn has(h: &Harness, el: NodeId, class: &str) -> bool {
    h.class(el).split(' ').any(|c| c == class)
}

fn bbox(doc: &Document, el: NodeId) -> auk_ui::Rect {
    doc.border_box(el).expect("laid out")
}

/// Clicks the middle of `el` (`clicks` presses) and delivers any event to the engine; returns
/// the outcomes.
fn click(h: &mut Harness, el: NodeId, clicks: u8) -> Vec<Outcome> {
    let r = bbox(&h.doc, el);
    click_at(h, r.x + r.w / 2.0, r.y + r.h / 2.0, clicks)
}

fn click_at(h: &mut Harness, x: f32, y: f32, clicks: u8) -> Vec<Outcome> {
    let out = h.view.click(
        &mut h.doc,
        &h.pane,
        PointerEvent {
            x,
            y,
            clicks,
            ..Default::default()
        },
    );
    for o in &out {
        if let Outcome::Event { sf, event, .. } = o {
            h.send(sf, (**event).clone());
        }
    }
    h.frame();
    out
}

/// The events the program got since `from`, without acks.
fn events(h: &Harness, from: usize) -> Vec<Value> {
    h.sent[from..]
        .iter()
        .filter(|e| e["ev"] != "ack" && e.get("ev").is_some())
        .cloned()
        .collect()
}

fn events_of(out: &[Outcome]) -> Vec<ViewEvent> {
    out.iter()
        .filter_map(|o| match o {
            Outcome::Event { event, .. } => Some((**event).clone()),
            _ => None,
        })
        .collect()
}

/// A model-hub-like sheet: scopes in groups, tabs, columns, rows in groups, a strip.
fn hub_props() -> Value {
    json!({
        "title": "Models",
        "subtitle": "Session-only switch",
        "icon": "cpu",
        "noun": "models",
        "query": "son",
        "cursor": 1,
        "scopes": [
            {"id": "roles", "label": "Roles", "icon": "sparkles", "count": 15},
            {"id": "all", "label": "All models", "icon": "list", "count": 1604},
            {"id": "provider:xai", "label": "xai-oauth", "mark": {"text": "XO", "seed": "xai"}, "group": "Providers", "count": 12, "dot": "success"},
            {"id": "provider:local", "label": "local", "mark": {"text": "Lo"}, "group": "Providers", "count": 13},
            {"id": "provider:anthropic", "label": "anthropic", "mark": {"text": "An"}, "group": "Not signed in", "disabled": "Sign in with /login", "dot": "muted"},
        ],
        "scope": "all",
        "tabs": [{"id": "all", "label": "All", "count": 47}, {"id": "chat", "label": "Chat", "count": 11}],
        "tab": "all",
        "columns": [
            {"id": "int", "head": "Int", "format": "num", "priority": 1},
            {"id": "ctx", "head": "Ctx", "format": "num", "priority": 4},
            {"id": "price", "head": "$/M", "format": "price", "priority": 3},
        ],
        "items": [
            {"id": "xai/grok-4.7", "label": "xai/grok-4.7", "mono": true, "facts": {"int": "46", "ctx": 500000, "price": "$2·6"},
             "chips": [{"text": "default", "on": true, "dot": "thinkingMedium"}, {"text": "slow", "auto": true}], "title": "Intelligence 46"},
            {"id": "xai/grok-son", "label": "xai/grok-son", "mono": true, "facts": {"ctx": 1600000}, "badges": [
                {"text": "free", "tone": "success"}, {"text": "new", "tone": "accent"}, {"text": "beta", "tone": "warning"}, {"text": "x"}, {"text": "y"}]},
            {"id": "local/sonic", "label": "Sonic local", "icon": "cpu", "dot": "warning", "detail": "on device"},
            {"id": "local/off", "label": "Offline", "disabled": "not running"},
        ],
        "order": [
            {"group": "recent", "label": "Recent", "count": 1}, "xai/grok-4.7",
            {"group": "rest", "label": "Matches", "count": 3}, "xai/grok-son", "local/sonic", "local/off", "unknown",
        ],
        "hits": {"xai/grok-son": [[9, 12]], "local/sonic": [[0, 3]]},
        "selected": "xai/grok-son",
        "current": ["xai/grok-4.7"],
        "total": 47,
        "strip": {"label": [{"t": "Assign ", "s": "muted"}, {"t": "grok-son", "s": "mono"}], "items": [
            {"id": "0", "label": "default", "on": true}, {"id": "1", "label": "smol", "dot": "thinkingHigh"}, {"id": "2", "label": "slow"}], "selected": "1"},
        "actions": [
            {"id": "assign", "label": "Assign role", "keys": ["enter"], "primary": true},
            {"id": "refresh", "label": "Refresh", "keys": ["f5"]},
            {"id": "help", "label": "Help", "end": true, "disabled": "No help yet"},
            {"id": "close", "label": "Close", "keys": ["esc"], "end": true},
        ],
    })
}

fn many_items(n: usize) -> (Vec<Value>, Vec<Value>) {
    let items: Vec<Value> = (0..n)
        .map(|i| json!({"id": format!("m{i}"), "label": format!("model {i}"), "facts": {"ctx": i * 1000}}))
        .collect();
    let order = (0..n).map(|i| json!(format!("m{i}"))).collect();
    (items, order)
}

// ---------------------------------------------------------------------------------------------
// Props and parts

#[test]
fn picker_draws_head_scopes_tabs_rows_strip_and_bar_from_its_props() {
    let mut h = harness(150);
    open(
        &mut h,
        hub_props(),
        json!([{"id": "pv", "k": "text", "p": {"text": "Grok son preview"}}]),
    );
    assert!(h.errors().is_empty(), "{:?}", h.errors());
    let el = h.el("s1", "pk");
    let class = h.class(el);
    for c in ["sf", "sf-picker", "sz-lg", "lay-rows", "pv-side", "has-cur"] {
        assert!(has(&h, el, c), "{c} in {class}");
    }
    for c in ["narrow", "no-title", "no-query", "pv-hide", "st-loading"] {
        assert!(!has(&h, el, c), "no {c} in {class}");
    }
    // `title` is the heading, not a tooltip.
    assert_eq!(h.attr(el, "title"), None);
    let sheet = one(&h, el, "pk-sheet");
    assert!(has(&h, sheet, "f-list"));
    assert_eq!(h.attr(sheet, "role").as_deref(), Some("dialog"));
    assert_eq!(h.attr(sheet, "aria-label").as_deref(), Some("Models"));
    // Head: title, subtitle, the drawn query with its caret after `s`, the count.
    assert_eq!(h.text(first(&h, el, "t")), "Models");
    assert_eq!(h.text(one(&h, el, "sub")), "Session-only switch");
    let q = one(&h, el, "q");
    assert_eq!(h.text(q), "son");
    let kids = h.doc.children(q).to_vec();
    assert_eq!(kids.len(), 3, "before, caret, after");
    assert!(has(&h, kids[1], "caret"));
    assert_eq!(h.doc.node(kids[0]).text(), Some("s"));
    assert_eq!(h.text(one(&h, el, "pk-count")), "4 of 47");
    // An action bound to Escape stands in for the head's esc.
    assert!(h.find_class(el, "pk-esc").is_empty());
    // Scopes: group headers, the active one, a disabled one with a lock and its reason.
    let heads: Vec<String> = h
        .find_class(el, "pk-sgh")
        .iter()
        .map(|&e| h.text(e))
        .collect();
    assert_eq!(heads, ["Providers", "Not signed in"]);
    let scopes = h.find_class(el, "pk-scope");
    assert_eq!(scopes.len(), 5);
    assert!(has(&h, scopes[1], "on"));
    assert!(has(&h, scopes[4], "dis"));
    assert_eq!(
        h.attr(scopes[4], "title").as_deref(),
        Some("Sign in with /login")
    );
    assert_eq!(h.find_class(scopes[4], "pk-lock").len(), 1);
    assert_eq!(h.text(first(&h, scopes[1], "n")), "1,604");
    assert_eq!(h.text(first(&h, scopes[2], "pk-mark")), "XO");
    assert!(
        h.view
            .written()
            .style(first(&h, scopes[2], "pk-mark"))
            .unwrap()
            .contains("linear-gradient")
    );
    // Tabs: a segmented control (≤4).
    let tabs = one(&h, el, "pk-tabs");
    assert!(has(&h, tabs, "seg"));
    let tab = h.find_class(el, "pk-tab");
    assert!(has(&h, tab[0], "on") && !has(&h, tab[1], "on"));
    // Rows: two group headers and four items (the unknown id skipped), in order.
    let rows = h.find_class(el, "pk-row");
    let kinds: Vec<String> = rows
        .iter()
        .map(|&r| h.attr(r, "data-v").unwrap_or_else(|| "grp".into()))
        .collect();
    assert_eq!(
        kinds,
        [
            "grp",
            "xai/grok-4.7",
            "grp",
            "xai/grok-son",
            "local/sonic",
            "local/off"
        ]
    );
    // The group headers carry the column heads (every one: whichever sticks shows them). At
    // this width (scopes and a side
    // preview) the label keeps its 260px only without the lowest-priority column (`int`).
    let heads: Vec<String> = h
        .find_class(rows[0], "head")
        .iter()
        .map(|&e| h.text(e))
        .collect();
    assert_eq!(heads, ["Ctx", "$/M"]);
    assert!(has(&h, el, "h0") && !has(&h, el, "h1") && !has(&h, el, "h2"));
    assert_eq!(h.find_class(rows[2], "head").len(), 2);
    assert_eq!(h.text(first(&h, rows[0], "pk-gl")), "Recent");
    // Current, selected, disabled.
    assert!(has(&h, rows[1], "cur"));
    assert_eq!(h.find_class(rows[1], "pk-cur").len(), 1);
    assert!(has(&h, rows[3], "sel"));
    assert_eq!(h.attr(rows[3], "aria-selected").as_deref(), Some("true"));
    assert!(has(&h, rows[5], "off"));
    assert_eq!(h.attr(rows[5], "title").as_deref(), Some("not running"));
    let scroll = one(&h, el, "pk-scroll");
    assert_eq!(
        h.attr(scroll, "aria-activedescendant").as_deref(),
        Some("pk-xai/grok-son")
    );
    // A mono path label: a dim dir and the name; hits bold (UTF-16 offsets into the label).
    let label = first(&h, rows[3], "pk-label");
    assert!(has(&h, label, "path"));
    assert_eq!(h.text(first(&h, label, "dir")), "xai/");
    assert_eq!(h.text(first(&h, label, "name")), "grok-son");
    let marks: Vec<String> = h
        .find_class(rows[3], "sf-mark")
        .iter()
        .map(|&e| h.text(e))
        .collect();
    assert_eq!(marks, ["son"]);
    let marks: Vec<String> = h
        .find_class(rows[4], "sf-mark")
        .iter()
        .map(|&e| h.text(e))
        .collect();
    assert_eq!(marks, ["Son"]);
    // Facts: compact numbers, text as sent with its tooltip.
    let facts: Vec<String> = h
        .find_class(rows[1], "pk-fact")
        .iter()
        .map(|&e| h.text(e))
        .collect();
    assert_eq!(facts, ["500k", "$2·6"]);
    assert_eq!(h.text(first(&h, rows[3], "c1")), "1.6M");
    // Badges: three, then +N with the rest as its tooltip; chips with their dot.
    let badges = h.find_class(rows[3], "pk-badge");
    assert_eq!(badges.len(), 4);
    assert_eq!(h.text(badges[3]), "+2");
    assert_eq!(h.attr(badges[3], "title").as_deref(), Some("x, y"));
    let chips = h.find_class(rows[1], "pk-chip");
    assert!(has(&h, chips[0], "on") && has(&h, chips[1], "auto"));
    assert!(
        h.view
            .written()
            .style(first(&h, chips[0], "pk-dot"))
            .unwrap()
            .contains("--sf-p-thinkingMedium")
    );
    // A lead icon with a corner dot; detail inline.
    assert_eq!(h.find_class(rows[4], "corner").len(), 1);
    assert_eq!(h.text(first(&h, rows[4], "pk-detail")), "on device");
    // The preview holds the children.
    let slot = one(&h, el, "pk-pv-slot");
    let pv = h.el("s1", "pv");
    assert_eq!(h.doc.parent(pv), Some(slot));
    assert!(h.find_class(el, "pk-pv-empty").is_empty());
    // Strip: its label, chips checked (`on`) and outlined (`selected`).
    let strip = one(&h, el, "pk-strip");
    assert_eq!(h.text(one(&h, strip, "pk-strip-l")), "Assign grok-son");
    let sch = h.find_class(strip, "pk-schip");
    assert_eq!(sch.len(), 3);
    assert!(has(&h, sch[0], "on") && !has(&h, sch[0], "sel"));
    assert!(has(&h, sch[1], "sel"));
    // Bar: secondary left, the fill, end ones, the primary last; keycaps; disabled dimmed.
    let bar = one(&h, el, "pk-bar");
    let order: Vec<String> = h
        .doc
        .children(bar)
        .iter()
        .map(|&b| h.attr(b, "data-v").unwrap_or_else(|| "fill".into()))
        .collect();
    assert_eq!(order, ["refresh", "fill", "help", "close", "assign"]);
    let btns = h.find_class(bar, "pk-btn");
    assert!(has(&h, btns[3], "primary"));
    assert!(has(&h, btns[1], "off"));
    assert_eq!(h.attr(btns[1], "title").as_deref(), Some("No help yet"));
    assert!(has(&h, btns[2], "esc"));
    let caps: Vec<String> = h
        .find_class(btns[3], "kbd")
        .iter()
        .map(|&e| h.text(e))
        .collect();
    assert_eq!(caps, ["⏎"]);
    // The sheet covers the surface below it (.sf-covered on main).
    let main = h.doc.parent(h.el("s1", "t")).unwrap();
    assert!(has(&h, main, "sf-covered"));
}

#[test]
fn picker_head_variants_no_title_no_query_placeholder_and_esc() {
    let mut h = harness(150);
    open(
        &mut h,
        json!({"noun": "sessions", "query": "", "items": [{"id": "a", "label": "A"}], "size": "md"}),
        json!([]),
    );
    let el = h.el("s1", "pk");
    assert!(has(&h, el, "no-title") && has(&h, el, "sz-md") && has(&h, el, "pv-none"));
    assert!(h.find_class(el, "pk-title").is_empty());
    assert_eq!(h.text(one(&h, el, "ph")), "Search sessions…");
    assert_eq!(h.find_class(el, "caret").len(), 1);
    let sheet = one(&h, el, "pk-sheet");
    assert_eq!(h.attr(sheet, "aria-label").as_deref(), Some("sessions"));
    // No action bound to Escape: the head's esc sends `close`.
    let esc = one(&h, el, "pk-esc");
    let from = h.sent.len();
    click(&mut h, esc, 1);
    let ev = events(&h, from);
    assert_eq!(
        ev,
        [json!({"ev": "action", "sf": "s1", "id": "pk", "act": "close"})]
    );
    // Unset query hides the search; an md sheet then shows its subtitle under the title.
    set(
        &mut h,
        3,
        json!({"query": null, "title": "Pick", "subtitle": "one of these"}),
    );
    let el = h.el("s1", "pk");
    assert!(has(&h, el, "no-query"));
    assert!(h.find_class(el, "pk-search").is_empty());
    let sub = one(&h, el, "sub");
    let st = h.doc.computed_style(sub).unwrap();
    assert_ne!(st.display, auk_ui::style::Display::None);
    let t = first(&h, el, "t");
    assert!(
        bbox(&h.doc, sub).y > bbox(&h.doc, t).y,
        "the subtitle is on a line under the title"
    );
    // With a query, an md sheet hides it.
    set(&mut h, 4, json!({"query": "x"}));
    let sub = one(&h, h.el("s1", "pk"), "sub");
    assert_eq!(
        h.doc.computed_style(sub).unwrap().display,
        auk_ui::style::Display::None
    );
}

#[test]
fn picker_focus_rings_the_part_the_keys_drive() {
    let mut h = harness(150);
    open(&mut h, hub_props(), json!([]));
    let el = h.el("s1", "pk");
    assert_eq!(
        h.find_class(el, "caret").len(),
        1,
        "the caret shows for `list`"
    );
    set(&mut h, 3, json!({"focus": "strip"}));
    let el = h.el("s1", "pk");
    let sheet = one(&h, el, "pk-sheet");
    assert!(has(&h, sheet, "f-strip"));
    assert!(
        h.find_class(el, "caret").is_empty(),
        "no caret off the list"
    );
    // The selected chip's ring is the accent.
    let chip = h.find_class(el, "pk-schip")[1];
    let st = h.doc.computed_style(chip).unwrap();
    let accent = h
        .doc
        .computed_style(sheet)
        .unwrap()
        .custom
        .get("--accent")
        .cloned()
        .unwrap();
    assert_eq!(accent.trim(), "#7c8cff");
    let [r, g, b, _] = st.border_color[0].to_rgba8();
    assert_eq!((r, g, b), (0x7c, 0x8c, 0xff));
    set(&mut h, 4, json!({"focus": "scopes"}));
    let sheet = one(&h, h.el("s1", "pk"), "pk-sheet");
    assert!(has(&h, sheet, "f-scopes"));
}

#[test]
fn picker_states_loading_error_and_empty() {
    let mut h = harness(150);
    open(
        &mut h,
        json!({"title": "Sessions", "noun": "sessions", "query": "", "state": "loading", "items": [{"id": "a", "label": "A"}],
               "actions": [{"id": "retry", "label": "Retry"}, {"id": "new", "label": "New", "keys": ["tab"]}]}),
        json!([]),
    );
    let el = h.el("s1", "pk");
    assert!(has(&h, el, "st-loading"));
    assert_eq!(h.find_class(el, "pk-sk").len(), 8);
    assert_eq!(h.text(one(&h, el, "pk-count")), "…");
    let list = one(&h, el, "pk-list");
    assert_eq!(
        h.doc.computed_style(list).unwrap().display,
        auk_ui::style::Display::None
    );
    // Error: the message in an alert card, with the retry action.
    set(
        &mut h,
        3,
        json!({"state": "error", "message": "Network down"}),
    );
    let el = h.el("s1", "pk");
    assert!(has(&h, el, "st-error"));
    let err = one(&h, el, "pk-err");
    assert!(h.text(err).contains("Network down"));
    let retry = one(&h, err, "pk-btn");
    let from = h.sent.len();
    click(&mut h, retry, 1);
    assert_eq!(events(&h, from)[0]["act"], "retry");
    // Empty with no query: `empty` (default "No <noun> yet") and the tab action as a button.
    set(&mut h, 4, json!({"state": "ready", "order": []}));
    let el = h.el("s1", "pk");
    let empty = one(&h, el, "pk-empty");
    assert!(
        h.text(empty).starts_with("No sessions yet"),
        "{}",
        h.text(empty)
    );
    let b = one(&h, empty, "pk-btn");
    assert_eq!(h.attr(b, "data-v").as_deref(), Some("new"));
    // Empty with a query: Clear search sends `clear`.
    set(&mut h, 5, json!({"query": "zz"}));
    let el = h.el("s1", "pk");
    let empty = one(&h, el, "pk-empty");
    assert!(h.text(empty).contains("No sessions match “zz”"));
    let from = h.sent.len();
    let b = one(&h, empty, "pk-btn");
    click(&mut h, b, 1);
    assert_eq!(
        events(&h, from),
        [json!({"ev": "action", "sf": "s1", "id": "pk", "act": "clear"})]
    );
    assert_eq!(h.text(one(&h, el, "pk-count")), "0");
}

// ---------------------------------------------------------------------------------------------
// Pointer input

#[test]
fn picker_rows_select_and_activate_as_the_book_says() {
    let mut h = harness(150);
    open(&mut h, hub_props(), json!([]));
    let row = |h: &Harness, id: &str| {
        h.find_class(h.el("s1", "pk"), "pk-row")
            .into_iter()
            .find(|&r| h.attr(r, "data-v").as_deref() == Some(id))
            .unwrap()
    };
    let ev = |out: &[Outcome]| -> Vec<String> {
        events_of(out)
            .iter()
            .map(|e| match e {
                ViewEvent::Select { item, .. } => format!("select {item}"),
                ViewEvent::Activate { item, .. } => format!("activate {item}"),
                other => format!("{other:?}"),
            })
            .collect()
    };
    // A row selects; clicking it again (the row clicked last) activates.
    let r = row(&h, "local/sonic");
    assert_eq!(ev(&click(&mut h, r, 1)), ["select local/sonic"]);
    let r = row(&h, "local/sonic");
    assert_eq!(ev(&click(&mut h, r, 1)), ["activate local/sonic"]);
    // The selected row activates on one click; its double click's second press sends nothing.
    let r = row(&h, "xai/grok-son");
    assert_eq!(ev(&click(&mut h, r, 1)), ["activate xai/grok-son"]);
    assert!(ev(&click(&mut h, r, 2)).is_empty(), "once per double click");
    // A double click on another row: select, then activate once.
    let r = row(&h, "xai/grok-4.7");
    assert_eq!(ev(&click(&mut h, r, 1)), ["select xai/grok-4.7"]);
    assert_eq!(ev(&click(&mut h, r, 2)), ["activate xai/grok-4.7"]);
    // A disabled row selects but never activates.
    let r = row(&h, "local/off");
    assert_eq!(ev(&click(&mut h, r, 1)), ["select local/off"]);
    assert_eq!(ev(&click(&mut h, r, 1)), ["select local/off"]);
    assert!(ev(&click(&mut h, r, 2)).is_empty());
    // Group headers send nothing.
    let g = h.find_class(h.el("s1", "pk"), "grp")[0];
    assert!(events_of(&click(&mut h, g, 1)).is_empty());
    // The wire shape.
    let from = h.sent.len();
    let r = row(&h, "local/sonic");
    click(&mut h, r, 1);
    assert_eq!(
        events(&h, from),
        [json!({"ev": "select", "sf": "s1", "id": "pk", "item": "local/sonic"})]
    );
}

#[test]
fn picker_scopes_tabs_strip_buttons_and_backdrop_send_actions() {
    let mut h = harness(150);
    open(&mut h, hub_props(), json!([]));
    let el = h.el("s1", "pk");
    let scopes = h.find_class(el, "pk-scope");
    let tabs = h.find_class(el, "pk-tab");
    let chips = h.find_class(el, "pk-schip");
    let btns = h.find_class(one(&h, el, "pk-bar"), "pk-btn");
    let from = h.sent.len();
    click(&mut h, scopes[2], 1);
    // A disabled scope still sends.
    click(&mut h, scopes[4], 1);
    click(&mut h, tabs[1], 1);
    click(&mut h, chips[2], 1);
    click(&mut h, btns[0], 1);
    // A disabled button sends nothing.
    click(&mut h, btns[1], 1);
    click(&mut h, btns[3], 1);
    // The backdrop sends the Escape action.
    let sheet = bbox(&h.doc, one(&h, el, "pk-sheet"));
    click_at(&mut h, sheet.x + sheet.w / 2.0, sheet.y + sheet.h + 6.0, 1);
    let got: Vec<Value> = events(&h, from)
        .into_iter()
        .map(|mut e| {
            let o = e.as_object_mut().unwrap();
            o.remove("sf");
            o.remove("id");
            e
        })
        .collect();
    assert_eq!(
        got,
        [
            json!({"ev": "action", "act": "scope", "value": "provider:xai"}),
            json!({"ev": "action", "act": "scope", "value": "provider:anthropic"}),
            json!({"ev": "action", "act": "tab", "value": "chat"}),
            json!({"ev": "action", "act": "strip", "value": "2"}),
            json!({"ev": "action", "act": "refresh"}),
            json!({"ev": "action", "act": "assign"}),
            json!({"ev": "action", "act": "close"}),
        ]
    );
}

#[test]
fn picker_keys_stay_the_programs() {
    let mut h = harness(150);
    open(&mut h, hub_props(), json!([]));
    // No field is focused: a typed key is the program's.
    let out = h.view.type_text(&mut h.doc, &h.pane, "x");
    assert_eq!(out, Outcome::Keys);
}

// ---------------------------------------------------------------------------------------------
// Layout

#[test]
fn picker_sheet_fits_the_layer_and_the_strip_stays_above_the_bar() {
    let mut h = harness(150);
    let (items, order) = many_items(60);
    let mut props = hub_props();
    props["items"] = json!(items);
    props["order"] = json!(order);
    props["selected"] = json!("m55");
    props["hits"] = json!({});
    open(&mut h, props, json!([]));
    let el = h.el("s1", "pk");
    let doc = &h.doc;
    let layer = bbox(doc, el);
    let sheet = bbox(doc, one(&h, el, "pk-sheet"));
    // lg: top-centered, 28px from the top, up to 1120 wide and 760 tall, within the layer.
    assert!(
        (sheet.y - layer.y - 28.0).abs() < 1.0,
        "{sheet:?} in {layer:?}"
    );
    assert!(
        sheet.w <= 1120.0 + 0.5 && sheet.h <= 760.0 + 0.5,
        "{sheet:?}"
    );
    assert!(
        sheet.y + sheet.h <= layer.y + layer.h + 0.5,
        "{sheet:?} in {layer:?}"
    );
    assert!(
        ((sheet.x - layer.x) - (layer.x + layer.w - sheet.x - sheet.w)).abs() < 1.0,
        "centered"
    );
    // The strip sits right above the bar, both inside the sheet's bottom.
    let strip = bbox(doc, one(&h, el, "pk-strip"));
    let bar = bbox(doc, one(&h, el, "pk-bar"));
    assert!((strip.y + strip.h - bar.y).abs() < 1.0, "{strip:?} {bar:?}");
    assert!(
        (bar.y + bar.h - (sheet.y + sheet.h)).abs() < 2.0,
        "{bar:?} {sheet:?}"
    );
    // Rows are 36px tall; the list scrolls and shows the selection.
    let rows = h.find_class(el, "pk-row");
    let r0 = bbox(doc, rows[0]);
    assert!((r0.h - 36.0).abs() < 0.5, "{r0:?}");
    let scroll = one(&h, el, "pk-scroll");
    let sc = bbox(doc, scroll);
    assert!(doc.max_scroll(scroll).1 > 0.0, "60 rows scroll");
    let sel = h.find_class(el, "sel")[0];
    let s = bbox(doc, sel);
    assert!(
        s.y >= sc.y - 0.5 && s.y + s.h <= sc.y + sc.h + 0.5,
        "the selection {s:?} is in view {sc:?}"
    );
    // The selection moving up out of view scrolls back to it.
    set(&mut h, 3, json!({"selected": "m2"}));
    let el = h.el("s1", "pk");
    let scroll = one(&h, el, "pk-scroll");
    let sc = bbox(&h.doc, scroll);
    let s = bbox(&h.doc, h.find_class(el, "sel")[0]);
    assert!(
        s.y >= sc.y - 0.5 && s.y + s.h <= sc.y + sc.h + 0.5,
        "{s:?} in {sc:?}"
    );
    // A list the reader scrolled away from stays put through an update that leaves the
    // selection where it was.
    h.doc.set_scroll_offset(scroll, 0.0, 1200.0);
    h.frame();
    let y = h.doc.scroll_offset(scroll).1;
    set(&mut h, 4, json!({"total": 61}));
    assert_eq!(h.doc.scroll_offset(scroll).1, y);
    // The preview pane sits at the side, the scope column at the left.
    let el = h.el("s1", "pk");
    let pv = bbox(&h.doc, one(&h, el, "pk-pv"));
    let main = bbox(&h.doc, one(&h, el, "pk-main"));
    let scopes = bbox(&h.doc, one(&h, el, "pk-scopes"));
    assert!(pv.x >= main.x + main.w - 0.5 && scopes.x + scopes.w <= main.x + 0.5);
}

#[test]
fn picker_virtualizes_long_lists_and_reuses_rows_while_filtering() {
    let mut h = harness(150);
    let (items, order) = many_items(1600);
    open(
        &mut h,
        json!({"title": "Models", "query": "", "items": items, "order": order, "selected": "m0",
               "columns": [{"id": "ctx", "head": "Ctx", "format": "num"}]}),
        json!([]),
    );
    let el = h.el("s1", "pk");
    let rows = h.find_class(el, "pk-row");
    assert_eq!(rows.len(), 1600);
    let laid = rows.iter().filter(|&&r| h.doc.laid_out(r)).count();
    assert!(laid < 120, "only the window lays out: {laid}");
    assert_eq!(h.text(one(&h, el, "pk-count")), "1,600");
    // A colhead row carries the heads when the list doesn't start with a group.
    assert_eq!(h.text(one(&h, el, "pk-colhead")), "Ctx");
    // Filtering (a new order) moves the rows that stay; only the selection's rows redraw.
    let before: std::collections::HashMap<String, NodeId> = rows
        .iter()
        .map(|&r| (h.attr(r, "data-v").unwrap(), r))
        .collect();
    let keep: Vec<Value> = (0..1600)
        .filter(|i| i % 3 == 0)
        .rev()
        .map(|i| json!(format!("m{i}")))
        .collect();
    set(
        &mut h,
        3,
        json!({"query": "3", "order": keep, "selected": "m3"}),
    );
    let el = h.el("s1", "pk");
    let rows = h.find_class(el, "pk-row");
    assert_eq!(rows.len(), 534);
    assert_eq!(h.attr(rows[0], "data-v").as_deref(), Some("m1599"));
    let reused = rows
        .iter()
        .filter(|&&r| before.get(&h.attr(r, "data-v").unwrap()) == Some(&r))
        .count();
    // (A redrawn row may land on its old element's freed slot, so this is at least.)
    assert!(
        reused >= 532,
        "all but the old and the new selection: {reused}"
    );
    let sel: Vec<String> = h
        .find_class(el, "sel")
        .iter()
        .filter_map(|&r| h.attr(r, "data-v"))
        .collect();
    assert_eq!(sel, ["m3"]);
    // The new selection near the end is scrolled into view.
    let scroll = one(&h, el, "pk-scroll");
    let sc = bbox(&h.doc, scroll);
    let sel = h.find_class(el, "sel")[0];
    assert!(h.doc.laid_out(sel));
    let s = bbox(&h.doc, sel);
    assert!(
        s.y >= sc.y - 0.5 && s.y + s.h <= sc.y + sc.h + 0.5,
        "{s:?} in {sc:?}"
    );
    // The wheel scrolls the list (terminal-owned), and the rows that come into view lay out.
    let y = h.doc.scroll_offset(scroll).1;
    assert!(
        h.view
            .wheel(&mut h.doc, sc.x + sc.w / 2.0, sc.y + sc.h / 2.0, 3000.0)
    );
    h.frame();
    assert_eq!(h.doc.scroll_offset(scroll).1, (y - 3000.0).max(0.0));
    let top = h
        .find_class(el, "pk-row")
        .into_iter()
        .find(|&r| {
            let b = bbox(&h.doc, r);
            b.y >= sc.y - 0.5 && b.y < sc.y + 36.0
        })
        .expect("a row at the top of the view");
    assert!(h.doc.laid_out(top));
}

#[test]
fn picker_narrow_sheets_fold_the_scopes_and_the_preview() {
    // 90 cols × 8px = 720px: under 760 the scopes fold into the head button, under 980 a side
    // preview moves below.
    let mut h = harness(90);
    open(
        &mut h,
        hub_props(),
        json!([{"id": "pv", "k": "text", "p": {"text": "preview"}}]),
    );
    let el = h.el("s1", "pk");
    assert!(has(&h, el, "narrow") && has(&h, el, "pv-below-auto") && !has(&h, el, "pv-hide"));
    assert!(h.find_class(el, "pk-scopes").is_empty());
    let sub = one(&h, el, "sub");
    assert_eq!(
        h.doc.computed_style(sub).unwrap().display,
        auk_ui::style::Display::None
    );
    let btn = one(&h, el, "pk-scopebtn");
    assert_eq!(
        h.text(btn).trim_end_matches(|c: char| !c.is_alphanumeric()),
        "All models"
    );
    let pv = bbox(&h.doc, one(&h, el, "pk-pv"));
    let main = bbox(&h.doc, one(&h, el, "pk-main"));
    assert!(
        pv.y >= main.y + main.h - 0.5,
        "the preview sits below: {pv:?} {main:?}"
    );
    // The head button's menu sends the same scope action and closes.
    let out = click(&mut h, btn, 1);
    assert_eq!(out, [Outcome::Redraw]);
    let el = h.el("s1", "pk");
    let menu = one(&h, el, "pk-menu");
    let item = h.find_class(menu, "pk-scope")[2];
    let from = h.sent.len();
    click(&mut h, item, 1);
    let ev = events(&h, from);
    assert_eq!(
        (ev[0]["act"].as_str(), ev[0]["value"].as_str()),
        (Some("scope"), Some("provider:xai"))
    );
    assert!(h.find_class(h.el("s1", "pk"), "pk-menu").is_empty());
    // Under 640px the preview and the count hide; with the preview gone every fact column fits
    // next to the label's 260px.
    h.view.set_cols(&mut h.doc, &h.pane, 70);
    h.frame();
    let el = h.el("s1", "pk");
    assert!(has(&h, el, "pv-hide"));
    let count = one(&h, el, "pk-count");
    assert_eq!(
        h.doc.computed_style(count).unwrap().display,
        auk_ui::style::Display::None
    );
    assert!(!has(&h, el, "h0"), "every column shows: {}", h.class(el));
    let row = h.find_class(el, "pk-row")[1];
    let label = bbox(&h.doc, first(&h, row, "pk-txt"));
    assert!(label.w >= 200.0, "the label keeps its room: {label:?}");
}

// ---------------------------------------------------------------------------------------------
// Look (offscreen pixels)

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

fn near(a: [u8; 3], b: [u8; 3], tol: u8) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= tol)
}

fn hex(v: &str) -> [u8; 3] {
    let h = v.trim().trim_start_matches('#');
    let b = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).unwrap();
    [b(0), b(2), b(4)]
}

/// The dark sheet over its dim: the sheet is `--pop-bg`, the backdrop darker than the page, the
/// selected row tinted with the accent, the primary button `--accent-fill`, and the focused
/// strip chip ringed with the accent.
#[test]
fn picker_look_sheet_dim_selection_primary_and_strip_ring() {
    let mut h = harness(120);
    let mut props = hub_props();
    props["focus"] = json!("strip");
    open(&mut h, props, json!([]));
    let (w, hh) = (960u32, 800u32);
    let px = render(&h.doc, w, hh);
    let el = h.el("s1", "pk");
    let sheet = one(&h, el, "pk-sheet");
    let vars = h.doc.computed_style(sheet).unwrap().custom.clone();
    let pop = hex(&vars["--pop-bg"]);
    let page = hex(&vars["--page"]);
    let s = bbox(&h.doc, sheet);
    // A point of the head's empty right part... use the bar's fill instead: plain sheet.
    let fill = bbox(&h.doc, one(&h, el, "pk-bar-fill"));
    let p = pixel(&px, w, fill.x + fill.w / 2.0, fill.y + 4.0);
    assert!(near(p, pop, 3), "the sheet is --pop-bg {pop:?}: {p:?}");
    // The backdrop under the sheet's bottom: dimmer than the page.
    let d = pixel(
        &px,
        w,
        s.x + s.w / 2.0,
        (s.y + s.h + 8.0).min(hh as f32 - 2.0),
    );
    assert!(
        d.iter().zip(page).all(|(a, b)| *a <= b),
        "dimmed {d:?} vs page {page:?}"
    );
    assert!(d != page, "the backdrop dims: {d:?}");
    // The selected row: tinted toward the accent (blue over the sheet).
    let sel = bbox(&h.doc, h.find_class(el, "sel")[0]);
    let p = pixel(&px, w, sel.x + sel.w - 40.0, sel.y + 4.0);
    assert!(
        p[2] > pop[2] + 10 && p[2] > p[0],
        "an accent tint: {p:?} over {pop:?}"
    );
    // The primary button.
    let prim = bbox(&h.doc, first(&h, el, "primary"));
    let p = pixel(&px, w, prim.x + 3.0, prim.y + prim.h / 2.0);
    assert!(near(p, hex(&vars["--accent-fill"]), 6), "primary {p:?}");
    // The focused strip chip's ring: accent pixels just outside its border.
    let chip = bbox(&h.doc, h.find_class(el, "pk-schip")[1]);
    let accent = hex(&vars["--accent"]);
    let ps: Vec<[u8; 3]> = (0..4)
        .map(|d| pixel(&px, w, chip.x + chip.w / 2.0, chip.y + d as f32 + 0.5))
        .collect();
    assert!(
        ps.iter().any(|&p| near(p, accent, 30)),
        "the selected chip is ringed with {accent:?}: {ps:?}"
    );
    // An unselected chip's border isn't.
    let other = bbox(&h.doc, h.find_class(el, "pk-schip")[2]);
    let ps: Vec<[u8; 3]> = (0..4)
        .map(|d| pixel(&px, w, other.x + other.w / 2.0, other.y + d as f32 + 0.5))
        .collect();
    assert!(!ps.iter().any(|&p| near(p, accent, 30)), "{ps:?}");
}

// ---------------------------------------------------------------------------------------------
// omp 18.8.5's own sheets

/// The recording (`fixtures/tsp/omp-picker.jsonl`, no prompts): `/model` opens omp's model hub
/// as a `picker` in the session's layer (scopes, tabs, columns, a side preview), typing `son`
/// filters it with hits, Down/Up move the selection, Enter opens the role strip with focus on
/// it, Right/Left move its selected chip, Esc cancels, backspaces clear the query, Esc closes.
/// Then the composer's model control opens the md "Switch model" sheet (preview below), `gp`
/// empties it with Clear search, and Esc twice closes it. Every frame draws without errors.
#[test]
fn omp_model_hub_and_dropdown_replay_as_picker_sheets() {
    let mut seen = Vec::new();
    let h = replay("omp-picker.jsonl", |h, rec| {
        if rec.verb == "q" {
            h.view.set_cols(&mut h.doc, &h.pane, 120);
        }
        if rec.verb != "f" {
            return;
        }
        let s = rec.body["s"].as_u64().unwrap();
        let sf = &h.pane.surfaces()[0];
        let doc = sf.document();
        let pk = ["v", "w"].into_iter().find(|id| doc.get(id).is_some());
        let Some(pk) = pk else {
            seen.push((s, None));
            return;
        };
        let el = h.el("s:1", pk);
        let sheet = one(h, el, "pk-sheet");
        let focus = ["f-list", "f-scopes", "f-strip"]
            .into_iter()
            .find(|f| has(h, sheet, f))
            .unwrap_or("?");
        let mut facts = vec![
            pk.to_owned(),
            focus.to_owned(),
            h.class(el)
                .split(' ')
                .filter(|c| c.starts_with("sz-") || c.starts_with("pv-"))
                .collect::<Vec<_>>()
                .join(","),
        ];
        let q = h
            .find_class(el, "q")
            .first()
            .map(|&q| h.text(q))
            .unwrap_or_default();
        facts.push(format!("q={q}"));
        let sel = h
            .find_class(el, "sel")
            .iter()
            .filter(|&&r| has(h, r, "pk-row"))
            .filter_map(|&r| h.attr(r, "data-v"))
            .collect::<Vec<_>>()
            .join(",");
        facts.push(format!("sel={sel}"));
        if let Some(&strip) = h.find_class(el, "pk-strip").first() {
            let chips = h.find_class(strip, "pk-schip");
            let at = chips.iter().position(|&c| has(h, c, "sel"));
            facts.push(format!("strip {} sel={at:?}", chips.len()));
            // Always visible: inside the sheet, right above the bar, within the layer.
            let st = bbox(&h.doc, strip);
            let bar = bbox(&h.doc, one(h, el, "pk-bar"));
            let sh = bbox(&h.doc, sheet);
            assert!(
                st.h > 0.0 && (st.y + st.h - bar.y).abs() < 1.0,
                "s{s}: {st:?} {bar:?}"
            );
            assert!(bar.y + bar.h <= sh.y + sh.h + 1.0, "s{s}");
            assert!(
                sh.y + sh.h <= h.doc.viewport().1 + 0.5,
                "s{s}: the sheet fits {sh:?}"
            );
        }
        if !h.find_class(el, "pk-empty").is_empty() {
            facts.push("empty".into());
        }
        seen.push((s, Some(facts.join(" "))));
    });
    assert!(h.errors().is_empty(), "{:?}", h.errors());
    for (s, f) in &seen {
        eprintln!("s{s}: {f:?}");
    }
    let at = |s: u64| {
        seen.iter()
            .find(|(n, _)| *n == s)
            .and_then(|(_, f)| f.clone())
            .unwrap_or_default()
    };
    // The hub opens with the scopes focused, the current model selected.
    assert!(
        at(6).starts_with("v f-scopes sz-lg,pv-side,pv-below-auto q= sel=xai-oauth/grok-4.7"),
        "{}",
        at(6)
    );
    // Typing filters; arrows move the selection.
    assert!(at(10).contains("q=son"), "{}", at(10));
    assert!(
        at(11).contains("sel=xai-oauth/grok-4.20-0309-reasoning"),
        "{}",
        at(11)
    );
    assert!(
        at(12).contains("sel=xai-oauth/grok-4.20-0309-non-reasoning"),
        "{}",
        at(12)
    );
    // Enter opens the strip with focus on it; Right, Right, Left move its selected chip.
    assert!(
        at(14).contains("f-strip") && at(14).contains("strip 15 sel=Some(0)"),
        "{}",
        at(14)
    );
    assert!(at(15).contains("strip 15 sel=Some(1)"), "{}", at(15));
    assert!(at(16).contains("strip 15 sel=Some(2)"), "{}", at(16));
    assert!(at(17).contains("strip 15 sel=Some(1)"), "{}", at(17));
    // Esc cancels the strip; the hub closes at s22.
    assert!(!at(18).contains("strip"), "{}", at(18));
    assert_eq!(at(22), "");
    // The model dropdown: an md sheet with its preview below; `gp` matches nothing.
    assert!(at(23).starts_with("w f-list sz-md,pv-below"), "{}", at(23));
    assert!(
        at(25).contains("empty") && at(25).contains("q=gp"),
        "{}",
        at(25)
    );
    assert_eq!(at(28), "");
    // Closed: nothing covers the session.
    let regions = h.find_class(h.view.element(), "sf-covered");
    assert!(
        regions.is_empty(),
        "the session isn't covered after the sheets close"
    );
}

/// `confirm` (book): a strip over the selected row with a trash icon, the text, a danger
/// button (default "Delete", sending `confirm.act` or "confirm") and Keep (`cancel`); the list
/// is `.pk-spacer.confirming` meanwhile. It follows the selection, survives virtualization, and
/// goes with the prop.
#[test]
fn picker_confirm_strip_sits_over_the_selected_row() {
    let mut h = harness(120);
    let (items, order) = many_items(260);
    open(
        &mut h,
        json!({"title": "Sessions", "query": "", "items": items, "order": order, "selected": "m3",
               "confirm": {"text": [{"t": "Delete "}, {"t": "model 3", "s": "strong"}, {"t": "?"}]}}),
        json!([]),
    );
    let pk = h.el("s1", "pk");
    let strip = one(&h, pk, "pk-confirm");
    assert_eq!(h.attr(strip, "role").as_deref(), Some("alertdialog"));
    assert_eq!(h.text(one(&h, strip, "pk-ct")), "Delete model 3?");
    assert!(!h.find_class(strip, "pk-ct-ic").is_empty());
    let spacer = one(&h, pk, "pk-spacer");
    assert!(has(&h, spacer, "confirming"));
    let row = |h: &Harness, id: &str| {
        h.find_class(pk, "pk-row")
            .into_iter()
            .find(|&r| h.attr(r, "data-v").as_deref() == Some(id))
            .unwrap()
    };
    let (s, r) = (bbox(&h.doc, strip), bbox(&h.doc, row(&h, "m3")));
    assert!(
        (s.y - r.y).abs() < 0.5 && (s.h - r.h).abs() < 0.5,
        "strip {s:?} over row {r:?}"
    );
    let buttons = h.find_class(strip, "pk-btn");
    let labels: Vec<String> = buttons.iter().map(|&b| h.text(b)).collect();
    assert_eq!(labels, ["Delete", "Keep"]);
    assert!(has(&h, buttons[0], "danger"));
    let from = h.sent.len();
    click(&mut h, buttons[0], 1);
    let pk = h.el("s1", "pk");
    let strip = one(&h, pk, "pk-confirm");
    let keep = h.find_class(strip, "pk-btn")[1];
    click(&mut h, keep, 1);
    assert_eq!(
        events(&h, from),
        [
            json!({"ev": "action", "sf": "s1", "id": "pk", "act": "confirm"}),
            json!({"ev": "action", "sf": "s1", "id": "pk", "act": "cancel"})
        ]
    );
    // A custom act and label; the strip moves with the selection.
    set(
        &mut h,
        3,
        json!({"selected": "m5", "confirm": {"text": "Forget it?", "act": "forget", "label": "Forget"}}),
    );
    settle(&mut h);
    let pk = h.el("s1", "pk");
    let strip = one(&h, pk, "pk-confirm");
    let b = h.find_class(strip, "pk-btn")[0];
    assert_eq!(h.text(b), "Forget");
    assert_eq!(h.attr(b, "data-v").as_deref(), Some("forget"));
    let (s, r) = (bbox(&h.doc, strip), bbox(&h.doc, row(&h, "m5")));
    assert!((s.y - r.y).abs() < 0.5, "{s:?} {r:?}");
    // Gone with the prop.
    set(&mut h, 4, json!({"confirm": null}));
    let pk = h.el("s1", "pk");
    assert!(h.find_class(pk, "pk-confirm").is_empty());
    assert!(!has(&h, one(&h, pk, "pk-spacer"), "confirming"));
}

/// Column heads ride on every group header, so whichever group sticks at the top of the list
/// shows them (book: Column `head`), not only the first.
#[test]
fn picker_column_heads_ride_on_every_group_header() {
    let mut h = harness(160);
    open(&mut h, hub_props(), json!([]));
    let pk = h.el("s1", "pk");
    let groups: Vec<NodeId> = h
        .find_class(pk, "pk-row")
        .into_iter()
        .filter(|&r| has(&h, r, "grp"))
        .collect();
    assert_eq!(groups.len(), 2);
    let heads_of = |g: NodeId| -> Vec<String> {
        h.find_class(g, "head")
            .into_iter()
            .map(|e| h.text(e))
            .collect()
    };
    let first_heads = heads_of(groups[0]);
    assert!(!first_heads.is_empty());
    for g in groups {
        assert_eq!(heads_of(g), first_heads, "{}", h.text(g));
    }
    assert!(
        h.find_class(pk, "pk-colhead").is_empty(),
        "the list starts with a group"
    );
}

// ---------------------------------------------------------------------------------------------
// omp's other picker layouts (fixtures/tsp/omp-picker-sessions.jsonl, omp-picker-tree-copy.jsonl)

/// The heights of the item rows (not group headers) under `el`, by `data-v`.
fn row_heights(h: &Harness, el: NodeId) -> Vec<(String, f32)> {
    h.find_class(el, "pk-row")
        .into_iter()
        .filter(|&r| !has(h, r, "grp") && h.doc.laid_out(r))
        .filter_map(|r| Some((h.attr(r, "data-v")?, bbox(&h.doc, r).h)))
        .collect()
}

/// omp 18.8.5's session picker (`omp --resume`, no prompt): a `screen` sheet of `cards` with a
/// side preview, a "Today" group, `when`/`size` columns, items added and replaced through
/// `itemsAdd` (a "history" badge that comes and goes), and search hits.
#[test]
fn omp_session_picker_is_a_screen_of_cards() {
    let mut seen: Vec<(u64, Vec<String>)> = Vec::new();
    let h = replay("omp-picker-sessions.jsonl", |h, rec| {
        if rec.verb == "q" {
            h.view.set_cols(&mut h.doc, &h.pane, 120);
            // As the hosts do (`surfaces::HOST_CSS`): the pane fills the window, so a screen
            // surface does.
            h.doc.set_stylesheet(
                "host-pane",
                auk_ui::SheetKind::BuiltIn,
                None,
                ".auk-pane { min-height: 100vh; }",
            );
        }
        if rec.verb != "f" || rec.body["sf"] != "s:2" {
            return;
        }
        let s = rec.body["s"].as_u64().unwrap();
        h.now += 1000.0;
        h.frame();
        let el = h.el("s:2", "0");
        let mut facts = vec![h.class(el)];
        let sheet = one(h, el, "pk-sheet");
        // The screen surface fills the pane, which fills the window.
        let (vw, vh) = h.doc.viewport();
        let screen = auk_ui::Rect::new(0.0, 0.0, vw, vh);
        let sb = bbox(&h.doc, sheet);
        facts.push(format!(
            "fills={}",
            (sb.w - screen.w).abs() < 1.0 && (sb.h - screen.h).abs() < 1.0
        ));
        let dim = one(h, el, "pk-dim");
        facts.push(format!(
            "dim={}",
            h.doc.laid_out(dim) && bbox(&h.doc, dim).w > 0.0
        ));
        for (id, ht) in row_heights(h, el) {
            facts.push(format!("{}:{ht}", id.rsplit('_').next().unwrap_or("")));
        }
        for g in h.find_class(el, "grp") {
            facts.push(format!(
                "grp {} {}",
                h.text(first(h, g, "pk-gl")),
                bbox(&h.doc, g).h
            ));
        }
        let badges: Vec<String> = h
            .find_class(el, "pk-badge")
            .into_iter()
            .map(|b| h.text(b))
            .collect();
        facts.push(format!("badges={badges:?}"));
        facts.push(format!("marks={}", h.find_class(el, "sf-mark").len()));
        let slot = one(h, el, "pk-pv-slot");
        facts.push(format!("preview={}", h.doc.element_children(slot).len()));
        seen.push((s, facts));
    });
    assert!(h.errors().is_empty(), "{:?}", h.errors());
    let at = |s: u64| -> &Vec<String> {
        &seen
            .iter()
            .find(|(x, _)| *x == s)
            .unwrap_or_else(|| panic!("s{s}: {seen:?}"))
            .1
    };
    let first_frame = at(1);
    let class = &first_frame[0];
    for c in ["sz-screen", "lay-cards", "pv-side"] {
        assert!(class.split(' ').any(|x| x == c), "{class}");
    }
    assert!(
        first_frame.contains(&"fills=true".to_owned()),
        "{first_frame:?}"
    );
    assert!(
        first_frame.contains(&"dim=false".to_owned()),
        "no backdrop: {first_frame:?}"
    );
    // Card rows are 54px, group headers 32px.
    let heights: Vec<&String> = first_frame
        .iter()
        .filter(|f| f.ends_with(".jsonl:54"))
        .collect();
    assert_eq!(heights.len(), 2, "{first_frame:?}");
    assert!(
        first_frame.contains(&"grp Today 32".to_owned()),
        "{first_frame:?}"
    );
    assert!(
        first_frame
            .iter()
            .any(|f| f.starts_with("preview=") && f != "preview=0")
    );
    // itemsAdd replaces an item through a render: the badge comes, then goes.
    assert!(
        at(6).contains(&"badges=[\"history\"]".to_owned()),
        "{:?}",
        at(6)
    );
    assert!(at(7).contains(&"badges=[]".to_owned()), "{:?}", at(7));
    assert!(
        at(7)
            .iter()
            .any(|f| f.starts_with("marks=") && f != "marks=0"),
        "{:?}",
        at(7)
    );
}

/// omp 18.8.5's `/tree` and `/copy` on a resumed session (no prompt): the tree is 24px rows
/// with chevrons (leaves here) and five filter tabs drawn as chips (past four); the copy picker
/// is a timeline (user rows 34px, the rest 26px, nodes by kind) whose `focus: "preview"` rings
/// the preview pane.
#[test]
fn omp_tree_and_copy_pickers_draw_their_layouts() {
    let mut tree = None;
    let mut timeline = Vec::new();
    let h = replay("omp-picker-tree-copy.jsonl", |h, rec| {
        if rec.verb == "q" {
            h.view.set_cols(&mut h.doc, &h.pane, 120);
        }
        if rec.verb != "f" {
            return;
        }
        let s = rec.body["s"].as_u64().unwrap();
        h.now += 1000.0;
        h.frame();
        let doc = h.pane.surfaces()[0].document();
        if s == 5 && doc.get("16.^picker").is_some() {
            let el = h.el("s:1", "16.^picker");
            let tabs = one(h, el, "pk-tabs");
            let chev: Vec<String> = h
                .find_class(el, "pk-chev")
                .into_iter()
                .map(|c| h.class(c))
                .collect();
            tree = Some((
                h.class(el),
                h.class(tabs),
                h.find_class(tabs, "pk-tab").len(),
                row_heights(h, el),
                chev,
            ));
        }
        if doc.get("18").is_some() {
            let el = h.el("s:1", "18");
            let sheet = one(h, el, "pk-sheet");
            let nodes: Vec<String> = h
                .find_class(el, "pk-node")
                .into_iter()
                .map(|n| h.class(n))
                .collect();
            let tool_icons = h
                .find_class(el, "n-tool")
                .into_iter()
                .filter(|&n| !h.find_class(n, "ico").is_empty())
                .count();
            timeline.push((
                s,
                h.class(el),
                h.class(sheet),
                row_heights(h, el),
                nodes,
                tool_icons,
            ));
        }
    });
    assert!(h.errors().is_empty(), "{:?}", h.errors());
    let (class, tabs, ntabs, heights, chev) = tree.expect("the tree picker drew");
    assert!(class.split(' ').any(|c| c == "lay-tree"), "{class}");
    assert_eq!(ntabs, 5);
    assert!(
        tabs.split(' ').any(|c| c == "chips"),
        "past four tabs are chips: {tabs}"
    );
    assert!(
        !heights.is_empty() && heights.iter().all(|(_, h)| (*h - 24.0).abs() < 0.5),
        "{heights:?}"
    );
    assert!(chev.iter().all(|c| c == "pk-chev leaf"), "{chev:?}");
    let (s, class, sheet, heights, nodes, tool_icons) = &timeline[0];
    assert_eq!(*s, 10);
    assert!(class.split(' ').any(|c| c == "lay-timeline"), "{class}");
    assert!(sheet.split(' ').any(|c| c == "f-list"), "{sheet}");
    assert_eq!(nodes.len(), 11);
    assert!(
        nodes.contains(&"pk-node n-user".to_owned())
            && nodes.contains(&"pk-node n-assistant".to_owned())
    );
    assert!(*tool_icons > 0, "tool nodes carry their tool's icon");
    for (id, ht) in heights {
        let user = nodes.len() == 11 && ["98cdd7d1"].contains(&id.as_str());
        if user {
            assert!((ht - 34.0).abs() < 0.5, "{id} {ht}");
        } else {
            assert!(
                (ht - 26.0).abs() < 0.5 || (ht - 34.0).abs() < 0.5,
                "{id} {ht}"
            );
        }
    }
    let users = heights
        .iter()
        .filter(|(_, h)| (*h - 34.0).abs() < 0.5)
        .count();
    assert_eq!(users, 3, "the three user turns: {heights:?}");
    let preview = timeline.iter().find(|t| t.0 == 12).expect("frame 12");
    assert!(
        preview.2.split(' ').any(|c| c == "f-preview"),
        "{}",
        preview.2
    );
}

/// Tree rows (book: `depth` draws indent guides, `open` a chevron rotated when true, unset a
/// leaf; `dot: "accent"` lights the guides) and timeline nodes, on a synthetic sheet.
#[test]
fn picker_tree_guides_and_chevrons_follow_depth_and_open() {
    let mut h = harness(140);
    open(
        &mut h,
        json!({"layout": "tree", "query": "", "items": [
            {"id": "a", "label": "root", "open": true},
            {"id": "b", "label": "child", "depth": 1, "open": false},
            {"id": "c", "label": "leaf", "depth": 2, "dot": "accent"},
        ]}),
        json!([]),
    );
    let el = h.el("s1", "pk");
    let row = |h: &Harness, v: &str| {
        h.find_class(el, "pk-row")
            .into_iter()
            .find(|&r| h.attr(r, "data-v").as_deref() == Some(v))
            .unwrap()
    };
    let guides = |h: &Harness, v: &str| -> (usize, String) {
        match h.find_class(row(h, v), "pk-guides").first() {
            Some(&g) => (h.find_class(g, "pk-g").len(), h.class(g)),
            None => (0, String::new()),
        }
    };
    assert_eq!(guides(&h, "a").0, 0);
    assert_eq!(guides(&h, "b"), (1, "pk-guides".to_owned()));
    assert_eq!(guides(&h, "c"), (2, "pk-guides accent".to_owned()));
    let chev = |h: &Harness, v: &str| h.class(one(h, row(h, v), "pk-chev"));
    assert_eq!(chev(&h, "a"), "pk-chev open");
    assert_eq!(chev(&h, "b"), "pk-chev");
    assert_eq!(chev(&h, "c"), "pk-chev leaf");
    assert!(
        h.find_class(one(&h, row(&h, "c"), "pk-chev"), "ico")
            .is_empty(),
        "a leaf draws no chevron"
    );
    // No dot lead in a tree: the accent is the guides'.
    assert!(h.find_class(row(&h, "c"), "pk-dot").is_empty());
    for v in ["a", "b", "c"] {
        assert!((bbox(&h.doc, row(&h, v)).h - 24.0).abs() < 0.5);
    }
}

/// The bar folds as it narrows (book: Action bar folding): the Escape action moves into the
/// head's esc (`.fold-esc`), then secondary buttons drop their keycaps (`.fold-keys`); with no
/// Escape action the keycaps go first. It unfolds when there is room again.
#[test]
fn picker_action_bar_folds_esc_then_keys() {
    let acts = |esc: bool| {
        let mut a = vec![
            json!({"id": "go", "label": "Switch to this one", "keys": ["enter"], "primary": true}),
            json!({"id": "sum", "label": "Summarize & switch", "keys": ["shift", "enter"]}),
            json!({"id": "label", "label": "Label the entry", "keys": ["shift", "l"]}),
            json!({"id": "filter", "label": "Filter entries", "keys": ["ctrl", "o"]}),
        ];
        if esc {
            a.push(
                json!({"id": "close", "label": "Close the sheet", "keys": ["esc"], "end": true}),
            );
        }
        json!({"size": "lg", "query": "", "items": [{"id": "x", "label": "x"}], "actions": a})
    };
    let folds = |cols: u16, esc: bool| {
        let mut h = harness(cols);
        open(&mut h, acts(esc), json!([]));
        let el = h.el("s1", "pk");
        (
            has(&h, el, "fold-esc"),
            has(&h, el, "fold-keys"),
            h.find_class(el, "pk-esc").len(),
        )
    };
    assert_eq!(folds(150, true), (false, false, 0), "room for everything");
    assert_eq!(folds(106, true), (true, false, 1), "esc moves to the head");
    assert_eq!(folds(80, true), (true, true, 1), "then the keycaps go");
    assert_eq!(
        folds(80, false).1,
        true,
        "without an Escape action the keycaps go first"
    );
}

/// `itemsDel` drops items from the catalog through a render, and the rows go with them.
#[test]
fn picker_items_add_and_del_update_the_rows() {
    let mut h = harness(140);
    let (items, _) = many_items(5);
    open(&mut h, json!({"query": "", "items": items}), json!([]));
    let ids = |h: &Harness| -> Vec<String> {
        let el = h.el("s1", "pk");
        h.find_class(el, "pk-row")
            .into_iter()
            .filter_map(|r| h.attr(r, "data-v"))
            .collect()
    };
    assert_eq!(ids(&h), ["m0", "m1", "m2", "m3", "m4"]);
    set(&mut h, 3, json!({"itemsDel": ["m1", "m3"]}));
    assert_eq!(ids(&h), ["m0", "m2", "m4"]);
    set(
        &mut h,
        4,
        json!({"itemsAdd": [{"id": "m9", "label": "late"}, {"id": "m0", "label": "renamed"}]}),
    );
    assert_eq!(ids(&h), ["m0", "m2", "m4", "m9"]);
    let el = h.el("s1", "pk");
    let m0 = h
        .find_class(el, "pk-row")
        .into_iter()
        .find(|&r| h.attr(r, "data-v").as_deref() == Some("m0"))
        .unwrap();
    assert_eq!(h.text(one(&h, m0, "pk-label")), "renamed");
}

/// Small behaviors: hits mark styled (spans) labels too; a text fact's tooltip only when it is
/// cut; a numeric `selected` is the same row as its string id (a click on it activates); the
/// error state's retry, disabled in the bar, is `.off` there too.
#[test]
fn picker_details_hits_on_spans_fact_tips_numeric_selected_and_off_state_buttons() {
    let mut h = harness(140);
    open(
        &mut h,
        json!({"query": "us", "items": [
            {"id": "1", "label": [{"t": "user: ", "s": "accent"}, {"t": "hello"}],
             "facts": {"who": "short", "long": "a value far too long for its column"}},
            {"id": "2", "label": "two"}
        ], "columns": [{"id": "who"}, {"id": "long", "format": "dim"}],
          "hits": {"1": [[0, 2]]}, "selected": 1}),
        json!([]),
    );
    let el = h.el("s1", "pk");
    let row1 = h
        .find_class(el, "pk-row")
        .into_iter()
        .find(|&r| h.attr(r, "data-v").as_deref() == Some("1"))
        .unwrap();
    let marks: Vec<String> = h
        .find_class(row1, "sf-mark")
        .into_iter()
        .map(|m| h.text(m))
        .collect();
    assert_eq!(marks, ["us"]);
    assert_eq!(h.text(one(&h, row1, "pk-label")), "user: hello");
    let facts = h.find_class(row1, "pk-fact");
    assert!(
        h.attr(facts[0], "title").is_none(),
        "a value that fits has no tooltip"
    );
    assert_eq!(
        h.attr(facts[1], "title").as_deref(),
        Some("a value far too long for its column")
    );
    // `selected: 1` selects row "1": a click on it activates.
    assert!(has(&h, row1, "sel"));
    let out = click(&mut h, row1, 1);
    assert!(
        matches!(events_of(&out)[..], [ViewEvent::Activate { .. }]),
        "{out:?}"
    );
    // The error state's retry, disabled.
    set(
        &mut h,
        3,
        json!({"state": "error", "message": "boom", "actions": [
        {"id": "retry", "label": "Retry", "disabled": "Offline"}]}),
    );
    let el = h.el("s1", "pk");
    let err = one(&h, el, "pk-err");
    let b = one(&h, err, "pk-btn");
    assert!(has(&h, b, "off"));
    assert_eq!(h.attr(b, "title").as_deref(), Some("Offline"));
    let from = h.sent.len();
    click(&mut h, b, 1);
    assert!(
        events(&h, from).is_empty(),
        "a disabled retry sends nothing"
    );
}
