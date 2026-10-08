//! The `app` suite (GOAL.md): the real macOS app (`auk` with no subcommand) on a test daemon,
//! driven through its automation endpoint. Layout replicated across windows (M4), menus and the
//! clipboard, input through the real `NSEvent` → `keyDown:` → `interpretKeyEvents:` →
//! `NSTextInputClient` path, IME composition and dead keys, the accessibility tree, surfaces
//! composited in the pane and acked after drawing, and the shell outliving the app (AC1). The S1
//! comparison posts real key events to Auk and to Ghostty, which needs the Accessibility
//! permission.
#![cfg(target_os = "macos")]

use std::time::Duration;

use auk_conformance::app::{App, InputSource, US_LAYOUT};
use auk_conformance::harness::{TestEnv, alive, wait_for};
use auk_conformance::probe::{Probe, events, hello, open_with, split_input, tsp_bodies};
use serde_json::{Value, json};

fn env(name: &str) -> TestEnv {
    let env = TestEnv::new(env!("CARGO_BIN_EXE_auk"), name);
    env.start_daemon();
    env
}

/// The ids of a layout's sessions, each with its tabs' pane trees and zoom.
fn shape(layout: &Value) -> Value {
    json!({"sessions":
        layout["sessions"]
            .as_array()
            .map(|sessions| {
                sessions
                    .iter()
                    .map(|s| {
                        json!({
                            "id": s["id"],
                            "tabs": s["tabs"].as_array().map(|tabs| tabs
                                .iter()
                                .map(|t| json!({"id": t["id"], "root": t["root"], "zoom": t["zoom"]}))
                                .collect::<Vec<_>>()),
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    })
}

fn tabs(layout: &Value) -> Vec<&Value> {
    layout["sessions"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|s| s["tabs"].as_array().into_iter().flatten())
        .collect()
}

/// Waits until both apps show the same layout and `done` holds for it; returns it.
fn agreed(a: &App, b: &App, what: &str, done: impl Fn(&Value) -> bool) -> Value {
    wait_for(what, || {
        let la = shape(&a.layout());
        let lb = shape(&b.layout());
        (la == lb && done(&la)).then_some(la)
    })
}

fn menu(app: &App, menu: &str, item: &str) {
    app.request(&format!("menu '{menu}' '{item}'"));
}

/// A probe pane shown and focused in `app`, its window the pane's primary.
fn focused_probe(env: &TestEnv, app: &App) -> Probe {
    let probe = Probe::start(env, &[]);
    wait_for("the app to follow the probe's pane", || {
        (app.send(&format!("focus {}", probe.pane))["ok"] == true).then_some(())
    });
    app.wait_primary(probe.pane);
    probe
}

/// Waits until the app's replica has everything the daemon has for `pane` (modes the program
/// just set included).
fn caught_up(env: &TestEnv, app: &App, pane: u64) {
    let seq = env.daemon(&format!("state-hash {pane}"))["seq"]
        .as_u64()
        .expect("a seq");
    app.caught_up(pane, seq);
}

/// Types each stroke through the window and returns what the program read for it.
fn typed(app: &App, probe: &Probe, strokes: &[&str]) -> Vec<Vec<u8>> {
    strokes
        .iter()
        .map(|stroke| {
            app.request(&format!("type '{stroke}'"));
            // Each stroke is one write; a little time lets a multi-byte one arrive whole.
            let first = probe.read_until(&format!("the bytes for {stroke}"), |b| !b.is_empty());
            let mut bytes = first;
            bytes.extend(probe.read_for(Duration::from_millis(150)));
            bytes
        })
        .collect()
}

#[test]
fn m4_sessions_tabs_splits_and_zoom_replicate_to_every_window() {
    let env = env("m4");
    let a = App::start(&env, "a", &[]);
    let b = App::start(&env, "b", &[]);
    let first = agreed(&a, &b, "both windows to show the first session", |l| {
        tabs(l).len() == 1
    });
    let pane = tabs(&first)[0]["root"]["pane"].as_u64().expect("one pane");

    // Splits.
    menu(&a, "Shell", "Split Right");
    let split = agreed(&a, &b, "the split in both windows", |l| {
        tabs(l)[0]["root"]["node"] == "split"
    });
    let root = &tabs(&split)[0]["root"];
    assert!(root.to_string().contains(&format!("{pane}")), "{root}");
    wait_for("the second window to lay out both panes", || {
        (b.state()["visible"].as_array()?.len() == 2).then_some(())
    });
    // Focus is per window: the new pane is focused in the window that split, not the other.
    let focus_a = a.state()["focused"].as_u64().expect("a focus");
    assert_ne!(focus_a, pane, "the splitting window focuses the new pane");
    assert_eq!(b.state()["focused"].as_u64(), Some(pane));

    // Zoom.
    menu(&a, "View", "Zoom Pane");
    agreed(&a, &b, "the zoom in both windows", |l| {
        tabs(l)[0]["zoom"].as_u64() == Some(focus_a)
    });
    wait_for("the second window to show only the zoomed pane", || {
        let visible = b.state()["visible"].as_array()?.clone();
        (visible.len() == 1 && visible[0]["pane"] == focus_a).then_some(())
    });
    menu(&a, "View", "Zoom Pane");
    agreed(&a, &b, "the unzoom in both windows", |l| {
        tabs(l)[0]["zoom"].is_null()
    });

    // Tabs.
    menu(&b, "Shell", "New Tab");
    agreed(&a, &b, "the new tab in both windows", |l| {
        tabs(l).len() == 2
    });
    // Sessions.
    menu(&a, "Shell", "New Session");
    let two = agreed(&a, &b, "the new session in both windows", |l| {
        l["sessions"].as_array().is_some_and(|s| s.len() == 2)
    });
    // Each window shows the session it asked for; switching is the window's own business.
    let session_a = a.state()["session"].clone();
    assert_eq!(session_a, two["sessions"][1]["id"], "{two}");
    menu(&a, "Window", "Show Next Session");
    wait_for("the window to switch sessions", || {
        (a.state()["session"] == two["sessions"][0]["id"]).then_some(())
    });
    // Closing a pane goes through the daemon too.
    menu(&a, "Window", "Show Next Session");
    wait_for("the window to switch back", || {
        (a.state()["session"] == two["sessions"][1]["id"]).then_some(())
    });
    menu(&a, "Shell", "Close Pane");
    agreed(&a, &b, "the closed session in both windows", |l| {
        l["sessions"].as_array().is_some_and(|s| s.len() == 1)
    });
    for app in [&a, &b] {
        let panes = app.request("panes");
        assert_eq!(panes["errors"], json!([]), "stream errors: {panes}");
    }
}

#[test]
fn menus_come_from_the_command_registry_and_full_screen_toggles() {
    let env = env("menus");
    let app = App::start(&env, "a", &[]);
    let menus = app.request("menus");
    let titles = |menu: &str| -> Vec<String> {
        menus
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["menu"] == menu)
            .unwrap_or_else(|| panic!("no {menu} menu: {menus}"))["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t.as_str().unwrap().to_owned())
            .collect()
    };
    for e in auk_app::commands::REGISTRY {
        assert!(
            titles(e.menu).iter().any(|t| t == e.title),
            "{} > {} is in the registry but not the menu bar: {menus}",
            e.menu,
            e.title
        );
    }
    assert!(titles("Auk").iter().any(|t| t == "About Auk"));
    assert!(titles("Auk").iter().any(|t| t == "Quit Auk"));

    // Full screen, from the menu, works without the app being frontmost.
    menu(&app, "View", "Toggle Full Screen");
    wait_for("the window to go full screen", || {
        (app.request("windows")[0]["full_screen"] == true).then_some(())
    });
    // AppKit ignores a toggle while the window is still animating in, so ask until it's out.
    wait_for("the window to leave full screen", || {
        menu(&app, "View", "Toggle Full Screen");
        std::thread::sleep(Duration::from_millis(1500));
        (app.request("windows")[0]["full_screen"] == false).then_some(())
    });
}

#[test]
fn copy_and_bracketed_paste_go_through_the_clipboard() {
    let env = env("clip");
    let app = App::start(&env, "a", &[]);
    let probe = focused_probe(&env, &app);

    // Paste: bracketed, because the program asked for it.
    probe.write(b"\x1b[?2004h");
    caught_up(&env, &app, probe.pane);
    app.request("clipboard-set 'pasted text'");
    menu(&app, "Edit", "Paste");
    let got = probe.read_until("the paste", |b| b.ends_with(b"\x1b[201~"));
    assert_eq!(got, b"\x1b[200~pasted text\x1b[201~");
    // Without bracketed paste, the text alone.
    probe.write(b"\x1b[?2004l");
    caught_up(&env, &app, probe.pane);
    menu(&app, "Edit", "Paste");
    let got = probe.read_until("the paste", |b| b.ends_with(b"text"));
    assert_eq!(got, b"pasted text");

    // Copy: the selection's text to the clipboard.
    probe.write(b"\x1b[2J\x1b[Hcopy me");
    caught_up(&env, &app, probe.pane);
    menu(&app, "Edit", "Select All");
    menu(&app, "Edit", "Copy");
    let clip = app.request("clipboard");
    // Select All takes the whole buffer; the text is the screen's, blank lines and all.
    assert_eq!(clip.as_str().map(str::trim), Some("copy me"), "{clip}");
}

#[test]
fn keys_modifiers_option_as_alt_and_kitty_flag_1_reach_the_program() {
    let env = env("keys");
    let app = App::start(&env, "a", &[]);
    let probe = focused_probe(&env, &app);
    let _us = InputSource::select(&app, US_LAYOUT);

    let got = typed(
        &app,
        &probe,
        &["a", "shift+a", "ctrl+c", "return", "up", "f5", "opt+f"],
    );
    let want: [&[u8]; 7] = [
        b"a",
        b"A",
        b"\x03",
        b"\r",
        b"\x1b[A",
        b"\x1b[15~",
        "ƒ".as_bytes(),
    ];
    assert_eq!(got, want.map(<[u8]>::to_vec), "{:?}", lossy(&got));

    // Option as Alt: the left Option sends ESC-prefixed keys.
    app.request("option-as-alt left");
    let got = typed(&app, &probe, &["opt+f", "opt+b"]);
    assert_eq!(
        got,
        [b"\x1bf".to_vec(), b"\x1bb".to_vec()],
        "{:?}",
        lossy(&got)
    );
    app.request("option-as-alt none");

    // Kitty flag 1 (disambiguate): Esc and ctrl+c become CSI u.
    probe.write(b"\x1b[>1u");
    caught_up(&env, &app, probe.pane);
    let got = typed(&app, &probe, &["esc", "ctrl+c", "a"]);
    assert_eq!(
        got,
        [b"\x1b[27u".to_vec(), b"\x1b[99;5u".to_vec(), b"a".to_vec()],
        "{:?}",
        lossy(&got)
    );
}

fn lossy(all: &[Vec<u8>]) -> Vec<String> {
    all.iter()
        .map(|b| String::from_utf8_lossy(b).escape_debug().to_string())
        .collect()
}

/// S1 (§23): the same physical keystrokes, posted as `CGEvent`s to Auk and to Ghostty, reach the
/// program as the same bytes. Posting needs the Accessibility permission.
#[test]
fn s1_posted_keys_reach_the_program_as_ghostty_sends_them() {
    use auk_app::s1::{GhosttyOptions, PERMISSION, record_ghostty, strokes};

    let env = env("s1");
    let app = App::start(&env, "a", &["--option-as-alt", "left"]);
    assert_eq!(
        app.request("can-post"),
        true,
        "prerequisite missing: {PERMISSION}"
    );
    let probe = focused_probe(&env, &app);
    let _us = InputSource::select(&app, US_LAYOUT);
    let names = [
        "a",
        "shift+a",
        "1",
        "space",
        "return",
        "tab",
        "shift+tab",
        "backspace",
        "esc",
        "up",
        "ctrl+up",
        "shift+left",
        "home",
        "pageup",
        "f1",
        "f12",
        "ctrl+a",
        "ctrl+c",
        "opt+f",
        "opt+b",
        "ctrl+shift+a",
        "/",
    ];
    let strokes = strokes(&names).expect("strokes");
    for kitty in [false, true] {
        let reference = record_ghostty(
            &strokes,
            &GhosttyOptions {
                kitty,
                option_as_alt: Some("left".into()),
            },
        )
        .unwrap_or_else(|e| panic!("prerequisite missing: {e}"));
        if kitty {
            probe.write(b"\x1b[>1u");
            caught_up(&env, &app, probe.pane);
        }
        app.request("activate");
        app.request("focus-window");
        let mut ours = Vec::new();
        for name in names {
            app.request(&format!("post-key '{name}'"));
            let mut bytes = probe.read_until(&format!("the bytes for {name}"), |b| !b.is_empty());
            bytes.extend(probe.read_for(Duration::from_millis(150)));
            ours.push(bytes);
        }
        for ((name, theirs), ours) in names.iter().zip(&reference).zip(&ours) {
            assert_eq!(
                String::from_utf8_lossy(ours).escape_debug().to_string(),
                String::from_utf8_lossy(theirs).escape_debug().to_string(),
                "{name} (kitty flag 1: {kitty}): Auk vs Ghostty"
            );
        }
    }
}

#[test]
fn ime_composition_goes_through_nstextinputclient() {
    let env = env("ime");
    let app = App::start(&env, "a", &[]);
    let probe = focused_probe(&env, &app);

    // Marked text is drawn at the cursor and never sent.
    let marked = app.request("mark にほ");
    assert_eq!(marked["has_marked_text"], true);
    let pre = app.request("preedit");
    assert_eq!(pre["text"], "にほ", "{pre}");
    app.request("mark にほん 0 3");
    assert_eq!(app.request("preedit")["text"], "にほん");
    assert!(
        probe.read_for(Duration::from_millis(300)).is_empty(),
        "composition reached the program"
    );
    // The commit replaces the composition and is sent as text.
    app.request("text 日本");
    let got = probe.read_until("the committed text", |b| b.len() >= "日本".len());
    assert_eq!(String::from_utf8_lossy(&got), "日本");
    let pre = app.request("preedit");
    assert!(
        pre["text"].is_null() && pre["has_marked_text"] == false,
        "{pre}"
    );
}

#[test]
fn dead_keys_compose_before_reaching_the_program() {
    let env = env("dead");
    let app = App::start(&env, "a", &[]);
    let probe = focused_probe(&env, &app);
    let _us = InputSource::select(&app, US_LAYOUT);
    for (dead, base, want) in [
        ("opt+e", "e", "é"),
        ("opt+u", "o", "ö"),
        ("opt+n", "n", "ñ"),
    ] {
        app.request(&format!("type '{dead}'"));
        assert!(
            probe.read_for(Duration::from_millis(200)).is_empty(),
            "{dead} alone reached the program"
        );
        app.request(&format!("type '{base}'"));
        let got = probe.read_until(&format!("{dead} {base}"), |b| b.len() >= want.len());
        assert_eq!(String::from_utf8_lossy(&got), want, "{dead} {base}");
    }
}

#[test]
fn the_accessibility_tree_has_the_pane_text_the_chrome_and_surface_roles() {
    let env = env("a11y");
    let app = App::start(&env, "a", &[]);
    let probe = focused_probe(&env, &app);
    probe.write(b"marker-a11y-text\r\n");
    probe.write(&hello(&[]));
    probe.write(&open_with(
        "s",
        "inline",
        json!([
            {"id": "p", "k": "progress", "p": {"value": 0.25, "label": "building"}},
            {"id": "t", "k": "text", "p": {"text": "a surface"}},
        ]),
    ));
    let tree = wait_for("the pane's text and its surface in the tree", || {
        let tree = app.request("a11y --depth 10");
        let flat = flatten(&tree);
        let text = flat.iter().any(|n| {
            n["role"] == "AXTextArea"
                && n["value"]
                    .as_str()
                    .is_some_and(|v| v.contains("marker-a11y-text"))
        });
        let progress = flat
            .iter()
            .any(|n| n["role"] == "AXProgressIndicator" && n["title"] == "building");
        (text && progress).then_some(tree)
    });
    let flat = flatten(&tree);
    let progress = flat
        .iter()
        .find(|n| n["role"] == "AXProgressIndicator")
        .unwrap();
    assert_eq!(progress["value"], "25", "{progress}");
    // The chrome: sessions and tabs as tab groups with tab buttons, and their buttons.
    let groups: Vec<&&Value> = flat.iter().filter(|n| n["role"] == "AXTabGroup").collect();
    assert_eq!(groups.len(), 2, "{tree}");
    assert!(flat.iter().any(|n| n["subrole"] == "AXTabButton"));
    assert!(flat.iter().any(|n| n["title"] == "New Tab"));
}

fn flatten(node: &Value) -> Vec<&Value> {
    let mut out = vec![node];
    for c in node["children"].as_array().into_iter().flatten() {
        out.extend(flatten(c));
    }
    out
}

#[test]
fn surfaces_take_their_place_in_the_flow_and_frames_are_acked_after_drawing() {
    let env = env("flow");
    let app = App::start(&env, "a", &[]);
    let probe = focused_probe(&env, &app);
    probe.write(b"above\r\n");
    probe.write(&open_with(
        "s1",
        "inline",
        json!([{"id": "t", "k": "text", "p": {"text": "one\ntwo\nthree"}}]),
    ));
    probe.write(b"\r\nbelow");
    // The window draws the frame, then acks it as the pane's primary.
    let input = probe.read_until("the ack", |b| {
        !events(&tsp_bodies(&split_input(b)), "ack").is_empty()
    });
    let acks: Vec<Value> = events(&tsp_bodies(&split_input(&input)), "ack")
        .into_iter()
        .cloned()
        .collect();
    assert_eq!(acks[0], json!({"ev": "ack", "sf": "s1", "s": 1}));
    // The block sits at its anchor row at its natural height (taller than a row), and the view
    // follows the tail: the cursor's line stays in view.
    let layout = wait_for("the block in the pane's flow", || {
        let l = app.request(&format!("pane-layout {}", probe.pane));
        (l["blocks"].as_array()?.len() == 1).then_some(l)
    });
    let block = &layout["blocks"][0];
    assert_eq!(block["sf"], "s1");
    assert_eq!(block["line"], 1, "{layout}");
    assert!(block["height"].as_f64().unwrap() > 20.0, "{layout}");
    assert_eq!(layout["following"], true);
    assert_eq!(layout["screen"], false);
}

#[test]
fn clicks_on_a_surface_go_to_its_program() {
    let env = env("click");
    let app = App::start(&env, "a", &[]);
    let probe = focused_probe(&env, &app);
    probe.write(&hello(&[]));
    probe.write(&open_with(
        "s1",
        "inline",
        json!([{"id": "go", "k": "text", "p": {"text": "click here", "actions": {"click": "run"}}}]),
    ));
    probe.read_until("the ack", |b| {
        !events(&tsp_bodies(&split_input(b)), "ack").is_empty()
    });
    let layout = wait_for("the block in the pane's flow", || {
        let l = app.request(&format!("pane-layout {}", probe.pane));
        (l["blocks"].as_array()?.len() == 1).then_some(l)
    });
    let (x, y) = (
        layout["origin"][0].as_f64().unwrap() + 10.0,
        layout["origin"][1].as_f64().unwrap() + layout["blocks"][0]["top"].as_f64().unwrap() + 5.0,
    );
    app.request(&format!("mouse down {x} {y}"));
    app.request(&format!("mouse up {x} {y}"));
    let input = probe.read_until("the action", |b| {
        !events(&tsp_bodies(&split_input(b)), "action").is_empty()
    });
    let bodies = tsp_bodies(&split_input(&input));
    let action = events(&bodies, "action")[0];
    assert_eq!(action["id"], "go", "{action}");
    assert_eq!(action["sf"], "s1", "{action}");
    // A click on the grid below goes to the grid (a selection), never to the surface.
    let below = y + layout["blocks"][0]["height"].as_f64().unwrap() + 20.0;
    app.request(&format!("mouse down {x} {below}"));
    app.request(&format!("mouse up {x} {below}"));
    assert!(
        events(
            &tsp_bodies(&split_input(&probe.read_for(Duration::from_millis(300)))),
            "action"
        )
        .is_empty()
    );
}

/// The block's first line, in window points: a point inside the surface at `dy` below its top.
fn block_point(app: &App, pane: u64, dy: f64) -> (f64, f64) {
    let layout = wait_for("the block in the pane's flow", || {
        let l = app.request(&format!("pane-layout {pane}"));
        (l["blocks"].as_array()?.len() == 1).then_some(l)
    });
    (
        layout["origin"][0].as_f64().unwrap() + 10.0,
        layout["origin"][1].as_f64().unwrap() + layout["blocks"][0]["top"].as_f64().unwrap() + dy,
    )
}

/// The program's input over `wait`: TSP event bodies other than acks, and the other bytes.
fn program_input(probe: &Probe, wait: Duration) -> (Vec<Value>, Vec<u8>) {
    let items = split_input(&probe.read_for(wait));
    let bodies = tsp_bodies(&items)
        .into_iter()
        .filter(|b| b["ev"] != "ack" && b["ev"] != "resize")
        .collect();
    let bytes = items
        .iter()
        .filter_map(|i| match i {
            auk_conformance::probe::Input::Other(b) => Some(b.clone()),
            _ => None,
        })
        .flatten()
        .collect();
    (bodies, bytes)
}

/// Types a stroke and waits for the program's first event of kind `ev`.
fn stroke_event(app: &App, probe: &Probe, stroke: &str, ev: &str) -> Value {
    app.request(&format!("type '{stroke}'"));
    let input = probe.read_until(&format!("the {ev} event for {stroke}"), |b| {
        !events(&tsp_bodies(&split_input(b)), ev).is_empty()
    });
    let items = split_input(&input);
    let bodies = tsp_bodies(&items);
    let other: Vec<&auk_conformance::probe::Input> = items
        .iter()
        .filter(|i| matches!(i, auk_conformance::probe::Input::Other(_)))
        .collect();
    assert!(other.is_empty(), "{stroke} also reached the program as bytes: {other:?}");
    events(&bodies, ev)[0].clone()
}

#[test]
fn program_field_editing_keys_go_through_the_view_as_edit_events() {
    let env = env("field-keys");
    let app = App::start(&env, "a", &[]);
    let _us = InputSource::select(&app, US_LAYOUT);
    let probe = focused_probe(&env, &app);
    probe.write(&hello(&["edit", "undo"]));
    probe.write(&open_with(
        "s1",
        "inline",
        json!([{"id": "ed", "k": "editor", "p": {"text": "hello world\nsecond line", "cursor": 5}}]),
    ));
    probe.write(&auk_conformance::probe::frame("s1", 2, json!([["focus", "ed"]])));
    probe.read_until("the focus frame's ack", |b| {
        events(&tsp_bodies(&split_input(b)), "ack")
            .iter()
            .any(|a| a["s"] == 2)
    });
    caught_up(&env, &app, probe.pane);
    let len = 23;

    // Selecting keys keep the terminal's own selection: nothing reaches the program.
    for _ in 0..3 {
        app.request("type shift+right");
    }
    let (bodies, bytes) = program_input(&probe, Duration::from_millis(400));
    assert!(bodies.is_empty() && bytes.is_empty(), "{bodies:?} {bytes:?}");
    // Backspace with a selection deletes it as an edit.
    let edit = stroke_event(&app, &probe, "backspace", "edit");
    assert_eq!(
        edit,
        json!({"ev": "edit", "sf": "s1", "id": "ed", "from": 5, "to": 8, "text": "", "cursor": 5, "len": len})
    );
    // ↓ moves the caret to the same column of the next drawn line; End and Home to the ends of
    // the caret's line (the program kept its cursor at 5).
    let down = stroke_event(&app, &probe, "down", "edit");
    assert_eq!((down["from"].clone(), down["to"].clone(), down["text"].clone()), (json!(17), json!(17), json!("")));
    let end = stroke_event(&app, &probe, "end", "edit");
    assert_eq!(end["cursor"], 11, "{end}");
    let home = stroke_event(&app, &probe, "home", "edit");
    assert_eq!(home["cursor"], 0, "{home}");
    // ⇧↓ then ⌘X: the selection goes to the clipboard and is deleted as an edit.
    app.request("type shift+down");
    let cut = stroke_event(&app, &probe, "cmd+x", "edit");
    assert_eq!((cut["from"].clone(), cut["to"].clone()), (json!(5), json!(17)), "{cut}");
    assert_eq!(app.request("clipboard"), " world\nsecon");
    // ⌘Z is an undo.
    let undo = stroke_event(&app, &probe, "cmd+z", "undo");
    assert_eq!(undo["id"], "ed");

    // With no selection, ← is the program's key; so is ↑ on the first drawn line.
    app.request("type left");
    let (bodies, bytes) = program_input(&probe, Duration::from_millis(400));
    assert!(bodies.is_empty(), "{bodies:?}");
    assert!(bytes == b"\x1b[D" || bytes == b"\x1bOD", "{bytes:?}");
    app.request("type up");
    let (bodies, bytes) = program_input(&probe, Duration::from_millis(400));
    assert!(bodies.is_empty(), "{bodies:?}");
    assert!(bytes == b"\x1b[A" || bytes == b"\x1bOA", "{bytes:?}");

    // IME composition draws at the field's caret, not over the grid, and reaches no one.
    app.request("mark にほん");
    let state = app.state();
    assert!(state["preedit"].is_null(), "the field took the composition: {state}");
    let (bodies, bytes) = program_input(&probe, Duration::from_millis(300));
    assert!(bodies.is_empty() && bytes.is_empty(), "{bodies:?} {bytes:?}");
    app.request("unmark");

    // Without the edit feature the same keys are the program's.
    probe.write(&auk_conformance::probe::frame("s1", 3, json!([["set", "ed", {"readonly": true}]])));
    probe.read_until("the readonly frame's ack", |b| {
        events(&tsp_bodies(&split_input(b)), "ack")
            .iter()
            .any(|a| a["s"] == 3)
    });
    caught_up(&env, &app, probe.pane);
    app.request("type home");
    let (bodies, bytes) = program_input(&probe, Duration::from_millis(400));
    assert!(bodies.is_empty(), "{bodies:?}");
    assert!(!bytes.is_empty(), "Home reached the program as a key");
}

#[test]
fn context_menus_run_a_surface_nodes_menu_actions() {
    let env = env("ctx-menu");
    let app = App::start(&env, "a", &[]);
    let probe = focused_probe(&env, &app);
    probe.write(&hello(&[]));
    probe.write(&open_with(
        "s1",
        "inline",
        json!([{"id": "row", "k": "text", "p": {"text": "row text", "actions": {"menu": ["retry", "open-log=build"]}}}]),
    ));
    probe.read_until("the ack", |b| {
        !events(&tsp_bodies(&split_input(b)), "ack").is_empty()
    });
    let (x, y) = block_point(&app, probe.pane, 5.0);

    // A secondary click opens the node's menu: its actions, then Copy.
    app.request(&format!("mouse down {x} {y} --right"));
    app.request(&format!("mouse up {x} {y} --right"));
    let menu = wait_for("the context menu", || {
        let m = app.request("chrome")["menu"].clone();
        m.is_array().then_some(m)
    });
    let actions: Vec<&str> = menu.as_array().unwrap().iter().map(|e| e["action"].as_str().unwrap()).collect();
    let labels: Vec<&str> = menu.as_array().unwrap().iter().map(|e| e["label"].as_str().unwrap()).collect();
    assert_eq!(actions, ["retry", "open-log=build", "copy"]);
    assert_eq!(labels, ["Retry", "Open log", "Copy"]);
    // The right press never reached the program, and the grid has no selection.
    let (bodies, bytes) = program_input(&probe, Duration::from_millis(200));
    assert!(bodies.is_empty() && bytes.is_empty(), "{bodies:?} {bytes:?}");

    // Choosing an entry with a click sends its action to the program, with its value.
    let pick = |i: usize| {
        let rect = wait_for("the menu entry's box", || {
            app.request("chrome")["actions"]
                .as_array()?
                .iter()
                .find(|a| a["action"] == format!("menu:{i}"))
                .map(|a| a["rect"].clone())
        });
        let (mx, my) = (
            rect[0].as_f64().unwrap() + 8.0,
            rect[1].as_f64().unwrap() + rect[3].as_f64().unwrap() / 2.0,
        );
        app.request(&format!("mouse down {mx} {my}"));
        app.request(&format!("mouse up {mx} {my}"));
    };
    pick(1);
    let input = probe.read_until("the menu's action", |b| {
        !events(&tsp_bodies(&split_input(b)), "action").is_empty()
    });
    let bodies = tsp_bodies(&split_input(&input));
    assert_eq!(
        events(&bodies, "action")[0],
        &json!({"ev": "action", "sf": "s1", "id": "row", "act": "open-log", "value": "build"})
    );
    assert!(app.request("chrome")["menu"].is_null(), "the menu closed");

    // Copy copies the node's text; Escape closes a menu without running anything.
    app.request(&format!("mouse down {x} {y} --right"));
    app.request(&format!("mouse up {x} {y} --right"));
    wait_for("the menu again", || app.request("chrome")["menu"].is_array().then_some(()));
    pick(2);
    wait_for("the copied text", || (app.request("clipboard") == "row text").then_some(()));
    app.request(&format!("mouse down {x} {y} --right"));
    app.request(&format!("mouse up {x} {y} --right"));
    wait_for("the menu again", || app.request("chrome")["menu"].is_array().then_some(()));
    app.request("type esc");
    wait_for("the menu to close", || app.request("chrome")["menu"].is_null().then_some(()));
    let (bodies, _) = program_input(&probe, Duration::from_millis(300));
    assert!(bodies.is_empty(), "{bodies:?}");
}

#[test]
fn the_zoom_action_opens_the_image_viewer_over_the_window() {
    use base64::Engine as _;
    use sha2::Digest as _;
    let env = env("viewer");
    let app = App::start(&env, "a", &[]);
    let probe = focused_probe(&env, &app);
    probe.write(&hello(&[]));
    let svg = |w: u32, h: u32, fill: &str| {
        format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"{h}\">\
             <rect width=\"{w}\" height=\"{h}\" fill=\"{fill}\"/></svg>"
        )
    };
    let (a, b) = (svg(60, 30, "#22aa55"), svg(30, 60, "#aa2255"));
    let id = |s: &str| -> String {
        sha2::Sha256::digest(s.as_bytes()).iter().map(|x| format!("{x:02x}")).collect()
    };
    let b64 = base64::engine::general_purpose::STANDARD;
    probe.write(&auk_conformance::probe::tsp("b", &[], &b64.encode(&a)));
    probe.write(&auk_conformance::probe::tsp("b", &[], &b64.encode(&b)));
    probe.write(&open_with(
        "s1",
        "inline",
        json!([{"id": "pics", "k": "row", "c": [
            {"id": "one", "k": "image", "p": {"blob": id(&a), "alt": "Green", "actions": {"click": "zoom"}}},
            {"id": "two", "k": "image", "p": {"blob": id(&b), "alt": "Red"}}
        ]}]),
    ));
    probe.read_until("the ack", |b| {
        !events(&tsp_bodies(&split_input(b)), "ack").is_empty()
    });
    // The middle of the row: the first picture, whichever way the row aligns it.
    let (x, y) = block_point(&app, probe.pane, 30.0);
    app.request(&format!("mouse down {x} {y}"));
    app.request(&format!("mouse up {x} {y}"));
    let viewer = wait_for("the viewer", || {
        let v = app.request("chrome")["viewer"].clone();
        v.is_object().then_some(v)
    });
    assert_eq!(viewer["index"], 0);
    let titles: Vec<&str> = viewer["images"].as_array().unwrap().iter().map(|i| i["title"].as_str().unwrap()).collect();
    assert_eq!(titles, ["Green", "Red"]);
    assert_eq!(viewer["images"][0]["blob"], id(&a));
    // The picture decoded from the pane's blobs (a failure would toast).
    assert_eq!(app.request("toasts"), json!([]));
    // → steps to the next image; Escape closes. Nothing reached the program.
    app.request("type right");
    wait_for("the next image", || (app.request("chrome")["viewer"]["index"] == 1).then_some(()));
    app.request("type esc");
    wait_for("the viewer to close", || app.request("chrome")["viewer"].is_null().then_some(()));
    let (bodies, bytes) = program_input(&probe, Duration::from_millis(300));
    assert!(bodies.is_empty() && bytes.is_empty(), "{bodies:?} {bytes:?}");
}

#[test]
fn ac01_the_shell_outlives_the_app_and_a_new_window_shows_its_scrollback() {
    let env = env("ac01");
    let app = App::start(&env, "a", &[]);
    let pane = app.state()["focused"].as_u64().expect("a focused pane");
    env.wait(pane, "prompt");
    let shell = env.pane_info(pane)["pid"]
        .as_u64()
        .expect("the shell's pid") as u32;
    app.request("text 'for i in $(seq 1 200); do echo line-$i; done; echo outlived-the-app'");
    app.request("type return");
    env.wait(pane, "prompt");
    app.quit();

    // The shell lives on in the daemon.
    assert!(alive(shell), "the shell died with the app");
    env.run(pane, "echo still-here");
    // A new app window shows the session and its scrollback.
    let again = App::start(&env, "b", &[]);
    assert_eq!(again.state()["focused"].as_u64(), Some(pane));
    let text = wait_for("the scrollback in the new window", || {
        let text = again.request(&format!("capture {pane} --scrollback"))["text"]
            .as_str()?
            .to_owned();
        text.contains("still-here").then_some(text)
    });
    assert!(
        text.contains("line-1\n"),
        "the scrollback's start is missing"
    );
    assert!(text.contains("outlived-the-app"));
    let (hash, seq) = env.daemon_hash(pane);
    again.caught_up(pane, seq);
    let ours = again.request(&format!("state-hash {pane}"));
    assert_eq!(
        ours["hash"],
        hash.as_str(),
        "the replica matches the daemon"
    );
}
