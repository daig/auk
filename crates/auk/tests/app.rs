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
    // One poster for both terminals: this process posts the keys to Ghostty and to Auk, so the
    // comparison is fair and only this process needs the Accessibility permission.
    assert!(
        auk_platform_macos::synth::can_post_events(),
        "prerequisite missing: {PERMISSION}"
    );
    let app = App::start(&env, "a", &["--option-as-alt", "left"]);
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
            auk_platform_macos::synth::post_to_pid(app.pid as i32, strokes[ours.len()]);
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
    // So is ⌃Z (§7.9), and the program never sees the ^Z byte (`stroke_event` checks).
    let undo = stroke_event(&app, &probe, "ctrl+z", "undo");
    assert_eq!((undo["sf"].clone(), undo["id"].clone()), (json!("s1"), json!("ed")), "{undo}");

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

// -------------------------------------------------------------------------------------------
// Find, prompt navigation, links, the reporting app, native editing, and the mixed layout (§5.3,
// §6.2, §7.2, §7.3, §7.8, §7.9, §7.14, §8.3, §12.2), and view state per window (§4.4, §4.5).

/// The pane's flow as last drawn, once `done` holds for it.
fn pane_layout(app: &App, pane: u64, what: &str, done: impl Fn(&Value) -> bool) -> Value {
    wait_for(what, || {
        let l = app.send(&format!("pane-layout {pane}"));
        (l["ok"] == true && done(&l["value"])).then(|| l["value"].clone())
    })
}

/// A drawn line's top and height, in window points.
fn line_top(layout: &Value, line: u64) -> Option<(f64, f64)> {
    let oy = layout["origin"][1].as_f64()?;
    layout["rows"].as_array()?.iter().find_map(|r| {
        (r[0].as_u64()? == line).then(|| (oy + r[1].as_f64().unwrap(), r[2].as_f64().unwrap()))
    })
}

/// The window box of a drawn surface node, if it is drawn.
fn node_box(app: &App, pane: u64, id: &str) -> Option<[f64; 4]> {
    let r = app.send(&format!("node-box {pane} {id}"));
    let v = r["value"].as_array().filter(|_| r["ok"] == true)?;
    Some([0, 1, 2, 3].map(|i| v[i].as_f64().unwrap()))
}

/// Shows and focuses `pane` in `app`'s window, once the window knows it.
fn show(app: &App, pane: u64) {
    wait_for("the window to show the pane", || {
        (app.send(&format!("focus {pane}"))["ok"] == true).then_some(())
    });
}

fn center(b: [f64; 4]) -> (f64, f64) {
    (b[0] + b[2] / 2.0, b[1] + b[3] / 2.0)
}

fn click(app: &App, (x, y): (f64, f64)) {
    app.request(&format!("mouse down {x} {y}"));
    app.request(&format!("mouse up {x} {y}"));
}

/// Waits for the ack of frame `s` (of any surface).
fn acked(probe: &Probe, s: u64) {
    probe.read_until(&format!("the ack of frame {s}"), |b| {
        events(&tsp_bodies(&split_input(b)), "ack")
            .iter()
            .any(|a| a["s"] == s)
    });
}

/// The first TSP event of kind `ev` the program reads.
fn program_event(probe: &Probe, ev: &str) -> Value {
    let input = probe.read_until(&format!("the {ev} event"), |b| {
        !events(&tsp_bodies(&split_input(b)), ev).is_empty()
    });
    events(&tsp_bodies(&split_input(&input)), ev)[0].clone()
}

#[test]
fn find_sees_the_grid_and_surfaces_virtualized_content_too_and_steps_through_the_flow() {
    let env = env("find");
    let app = App::start(&env, "a", &[]);
    let probe = focused_probe(&env, &app);
    probe.write(&hello(&[]));
    let mut out = String::new();
    for i in 0..80 {
        if i == 10 || i == 70 {
            out.push_str(&format!("row {i} Needle-grid\r\n"));
        } else {
            out.push_str(&format!("row {i}\r\n"));
        }
    }
    probe.write(out.as_bytes());
    // A column of 300 rows virtualizes (§8.3): rows far from the view aren't laid out.
    let rows: Vec<Value> = (0..300)
        .map(|i| {
            let text = if i == 250 { "the needle-item here".to_owned() } else { format!("item {i}") };
            json!({"id": format!("t{i}"), "k": "text", "p": {"text": text}})
        })
        .collect();
    probe.write(&open_with("s1", "inline", json!([{"id": "big", "k": "col", "c": rows}])));
    acked(&probe, 1);
    probe.write(&auk_conformance::probe::tsp_json("x", &json!({"id": "s1"})));
    probe.write(b"tail needle-tail\r\n");
    caught_up(&env, &app, probe.pane);
    pane_layout(&app, probe.pane, "the block", |l| l["blocks"].as_array().is_some_and(|b| b.len() == 1));

    menu(&app, "Edit", "Find…");
    app.request("text needle");
    let find = wait_for("four matches", || {
        let f = app.request("find");
        (f["count"] == 4).then_some(f)
    });
    // In flow order: the two grid rows, the surface row at its anchor, the tail.
    let kinds: Vec<&str> = find["matches"].as_array().unwrap().iter().map(|m| m["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["grid", "grid", "surface", "grid"], "{find}");
    assert_eq!(find["matches"][2]["id"], "t250", "{find}");
    assert_eq!(find["matches"][2]["sf"], "s1");
    assert_eq!(find["current"], 3, "Find starts at the match nearest the tail");
    assert_eq!(app.request("chrome")["find_bar"], true);
    // The current grid match is the selection (smart case: `needle` matched `Needle`).
    assert_eq!(app.request("selection"), "needle");

    // ⇧Return: the surface match, scrolled into view and marked.
    app.request("type shift+return");
    wait_for("the surface match", || (app.request("find")["current"] == 2).then_some(()));
    let pane = probe.pane;
    wait_for("the found row in view", || {
        let l = app.request(&format!("pane-layout {pane}"));
        let top = l["origin"][1].as_f64()?;
        let bottom = top + l["content_height"].as_f64()?;
        let b = node_box(&app, pane, "t250")?;
        (b[1] >= top && b[1] + b[3] <= bottom).then_some(())
    });
    assert!(app.request("find")["marked"].as_u64().unwrap() >= 1);

    // The grid match above it: selected and on screen, through the mixed layout.
    app.request("type shift+return");
    let m = wait_for("the second grid match", || {
        let f = app.request("find");
        (f["current"] == 1).then(|| f["match"].clone())
    });
    let line = m["line"].as_u64().unwrap();
    pane_layout(&app, pane, "the grid match on screen", |l| line_top(l, line).is_some());
    assert_eq!(app.request("selection"), "Needle");
    // Previous again, then past the first match it wraps to the last.
    app.request("type shift+return");
    let first = wait_for("the first match", || {
        let f = app.request("find");
        (f["current"] == 0).then(|| f["match"].clone())
    });
    let line0 = first["line"].as_u64().unwrap();
    pane_layout(&app, pane, "the first match on screen", |l| line_top(l, line0).is_some());
    app.request("type shift+return");
    wait_for("the wrap", || (app.request("find")["current"] == 3).then_some(()));
    // ⌘G steps forward and wraps to the first.
    menu(&app, "Edit", "Find Next");
    wait_for("the next match", || (app.request("find")["current"] == 0).then_some(()));
    // Editing the query searches again.
    app.request("type backspace");
    app.request("text e-item");
    wait_for("one match", || (app.request("find")["count"] == 1).then_some(()));
    // Escape closes the bar; nothing reached the program.
    app.request("type esc");
    wait_for("the bar to close", || (app.request("chrome")["find_bar"] == false).then_some(()));
    let (bodies, bytes) = program_input(&probe, Duration::from_millis(300));
    assert!(bodies.is_empty() && bytes.is_empty(), "{bodies:?} {bytes:?}");
}

#[test]
fn prompt_navigation_jumps_between_command_marks_in_every_window() {
    let env = env("prompts");
    let app = App::start(&env, "a", &[]);
    let probe = focused_probe(&env, &app);
    let mut out = String::new();
    for cmd in 1..=3 {
        out.push_str(&format!("\x1b]133;A\x07$ cmd-{cmd}\r\n"));
        for i in 0..60 {
            out.push_str(&format!("output {cmd}.{i}\r\n"));
        }
    }
    out.push_str("\x1b]133;A\x07$ ");
    probe.write(out.as_bytes());
    caught_up(&env, &app, probe.pane);
    let pane = probe.pane;
    let layout = pane_layout(&app, pane, "the four prompts", |l| {
        l["prompts"].as_array().is_some_and(|p| p.len() == 4)
    });
    let prompts: Vec<u64> = layout["prompts"].as_array().unwrap().iter().map(|p| p.as_u64().unwrap()).collect();
    // Drawn with the last prompt at the tail.
    let layout = pane_layout(&app, pane, "the tail drawn", |l| line_top(l, prompts[3]).is_some());
    assert_eq!(layout["following"], true);

    let jump = |item: &str, line: u64, text: &str| {
        menu(&app, "View", item);
        let l = pane_layout(&app, pane, &format!("{item} to line {line}"), |l| l["first_line"] == line);
        assert_eq!(l["first_text"], text, "{l}");
        assert_eq!(l["following"], false);
    };
    jump("Jump to Previous Prompt", prompts[2], "$ cmd-3");
    jump("Jump to Previous Prompt", prompts[1], "$ cmd-2");
    jump("Jump to Previous Prompt", prompts[0], "$ cmd-1");
    jump("Jump to Next Prompt", prompts[1], "$ cmd-2");
    jump("Jump to Next Prompt", prompts[2], "$ cmd-3");
    // The last prompt is at the tail: the view follows it again.
    menu(&app, "View", "Jump to Next Prompt");
    pane_layout(&app, pane, "the view to follow the tail", |l| l["following"] == true);

    // The marks are pane state: a window that attaches later has them too.
    let late = App::start(&env, "b", &[]);
    show(&late, pane);
    let l = pane_layout(&late, pane, "the late window's prompts", |l| l["prompts"].is_array());
    assert_eq!(l["prompts"], json!(prompts));
}

#[test]
fn hover_shows_a_links_target_on_the_grid_and_on_surfaces_and_a_plain_click_opens_nothing() {
    let env = env("hover");
    let app = App::start(&env, "a", &[]);
    let probe = focused_probe(&env, &app);
    probe.write(&hello(&[]));
    probe.write(b"see \x1b]8;;https://example.com/grid\x1b\\the grid link\x1b]8;;\x1b\\ here\r\n");
    probe.write(&open_with(
        "s1",
        "inline",
        json!([{"id": "lnk", "k": "text", "p": {"text": "the docs", "href": "https://example.com/docs"}}]),
    ));
    acked(&probe, 1);
    caught_up(&env, &app, probe.pane);
    let pane = probe.pane;
    let layout = pane_layout(&app, pane, "the link's row", |l| line_top(l, 0).is_some());
    let cw = layout["cell"][0].as_f64().unwrap();
    let (top, h) = line_top(&layout, 0).unwrap();
    let x = layout["origin"][0].as_f64().unwrap() + cw * 7.5;
    let y = top + h / 2.0;
    app.request(&format!("mouse move {x} {y}"));
    wait_for("the grid link's target", || {
        (app.request("chrome")["hover_link"] == "https://example.com/grid").then_some(())
    });
    // A plain click on it opens nothing: links need ⌘-click (§7.14).
    click(&app, (x, y));
    assert_eq!(app.request("opens")["opened"], json!([]));

    let b = wait_for("the surface link", || node_box(&app, pane, "lnk"));
    let (lx, ly) = center(b);
    app.request(&format!("mouse move {lx} {ly}"));
    wait_for("the surface link's target", || {
        (app.request("chrome")["hover_link"] == "https://example.com/docs").then_some(())
    });
    // Off the links, the readout goes.
    app.request(&format!("mouse move {x} {}", ly + 200.0));
    wait_for("no readout", || app.request("chrome")["hover_link"].is_null().then_some(()));
}

#[test]
fn the_chrome_names_the_reporting_app_only_while_a_listening_surface_is_open() {
    let env = env("app-name");
    let app = App::start(&env, "a", &[]);
    let probe = focused_probe(&env, &app);
    // A hello alone names nothing.
    probe.write(&hello(&[]));
    caught_up(&env, &app, probe.pane);
    let chrome = app.request("chrome");
    assert!(chrome["pane_app"].is_null(), "{chrome}");
    // A listening surface: the pane's tab and the status line name the program.
    probe.write(&open_with("s1", "inline", json!([{"id": "t", "k": "text", "p": {"text": "hi"}}])));
    acked(&probe, 1);
    let chrome = wait_for("the app in the chrome", || {
        let c = app.request("chrome");
        (c["pane_app"] == "probe").then_some(c)
    });
    let tab = app.state()["tab"].as_u64().unwrap().to_string();
    assert_eq!(chrome["tab_apps"][&tab], "probe", "{chrome}");
    // Closed: gone again.
    probe.write(&auk_conformance::probe::tsp_json("x", &json!({"id": "s1"})));
    caught_up(&env, &app, probe.pane);
    wait_for("the app to leave the chrome", || {
        let c = app.request("chrome");
        (c["pane_app"].is_null() && c["tab_apps"] == json!({})).then_some(())
    });
    // A surface that doesn't listen names nothing.
    probe.write(&auk_conformance::probe::tsp_json(
        "o",
        &json!({"id": "s2", "mode": "inline", "listen": false}),
    ));
    caught_up(&env, &app, probe.pane);
    std::thread::sleep(Duration::from_millis(300));
    assert!(app.request("chrome")["pane_app"].is_null());
}

/// §7.2: the hello's app names the pane's program in the command palette too, by the same rule
/// as the tab (§12.2): only while a listening surface is open, and not once the program is gone.
#[test]
fn the_palette_names_the_focused_panes_reporting_app() {
    let env = env("app-palette");
    let app = App::start(&env, "a", &[]);
    let probe = focused_probe(&env, &app);
    probe.write(&hello(&[]));
    probe.write(&open_with(
        "s1",
        "inline",
        json!([{"id": "t", "k": "text", "p": {"text": "hi"}}]),
    ));
    acked(&probe, 1);
    wait_for("the app in the chrome", || {
        (app.request("chrome")["pane_app"] == "probe").then_some(())
    });
    // Opened as a person would, the palette's header names the program.
    app.request("menu View 'Command Palette…'");
    let drawn = wait_for("the palette to name the app", || {
        let p = app.request("palette");
        (p["open"] == true && !p["drawn_app"].is_null()).then(|| p["drawn_app"].clone())
    });
    assert_eq!(drawn["app"], "probe", "{drawn}");
    assert_eq!(drawn["text"], "probe \u{2014} Commands", "{drawn}");
    // The surface closes while the palette is up: the header goes with it.
    probe.write(&auk_conformance::probe::tsp_json("x", &json!({"id": "s1"})));
    caught_up(&env, &app, probe.pane);
    wait_for("the header to leave the palette", || {
        let p = app.request("palette");
        (p["open"] == true && p["drawn_app"].is_null()).then_some(())
    });
    app.request("type escape");
    wait_for("the palette to close", || {
        (app.request("palette")["open"] == false).then_some(())
    });
    // Listening again, then the program exits: the palette names nothing.
    probe.write(&open_with(
        "s2",
        "inline",
        json!([{"id": "t", "k": "text", "p": {"text": "hi"}}]),
    ));
    acked(&probe, 1);
    wait_for("the app back in the chrome", || {
        (app.request("chrome")["pane_app"] == "probe").then_some(())
    });
    probe.exit(0);
    wait_for("the app to leave the chrome", || {
        app.request("chrome")["pane_app"].is_null().then_some(())
    });
    app.request("menu View 'Command Palette…'");
    wait_for("the palette", || {
        (app.request("palette")["open"] == true).then_some(())
    });
    std::thread::sleep(Duration::from_millis(300));
    let p = app.request("palette");
    assert_eq!(p["open"], true, "{p}");
    assert!(p["drawn_app"].is_null(), "{p}");
}

/// The system's 12/24-hour setting, read apart from the app: the hour pattern Foundation gives
/// the `j` skeleton in the current locale (through JavaScript for Automation), 12-hour when it has
/// an `h` or `K` field outside quotes.
fn system_hour12() -> bool {
    let out = std::process::Command::new("/usr/bin/osascript")
        .args([
            "-l",
            "JavaScript",
            "-e",
            "ObjC.import('Foundation'); \
             ObjC.unwrap($.NSDateFormatter.dateFormatFromTemplateOptionsLocale('j', 0, $.NSLocale.currentLocale))",
        ])
        .output()
        .expect("run osascript");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let pattern = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    assert!(!pattern.is_empty(), "no hour pattern");
    let mut quoted = false;
    pattern.chars().any(|c| {
        if c == '\'' {
            quoted = !quoted;
        }
        !quoted && (c == 'h' || c == 'K')
    })
}

/// §4.5, §7.2: the app reports the system's 12/24-hour clock, so the hello reply's `hour12` is
/// the system setting, and the daemon holds it as the pane's fact.
#[test]
fn the_hello_reply_carries_the_systems_12_hour_clock() {
    let env = env("hour12");
    let app = App::start(&env, "a", &[]);
    let probe = focused_probe(&env, &app);
    let want = system_hour12();
    probe.write(&hello(&[]));
    let input = probe.read_until("the hello reply", |b| {
        tsp_bodies(&split_input(b)).iter().any(|v| v.get("hour12").is_some())
    });
    let reply = tsp_bodies(&split_input(&input)).into_iter().find(|v| v.get("hour12").is_some()).unwrap();
    assert_eq!(reply["hour12"], want, "{reply}");
    let tsp = env.daemon(&format!("tsp {}", probe.pane));
    assert_eq!(tsp["facts"]["hour12"], want, "{tsp}");
}

/// §5.1: an agent pane's tab shows the agent icon in the app's tab bar; a shell's tab doesn't.
#[test]
fn an_agent_panes_tab_shows_the_agent_icon() {
    let env = env("agent-icon");
    write_settings(&env, json!({"agent_command": "echo agent-ready; sleep 300"}));
    let app = App::start(&env, "a", &[]);
    let shell_tab = app.state()["tab"].as_u64().expect("a shown tab");
    let out = env.cli(&["--json", "new", "tab", "--agent", "--cwd", &env.home.to_string_lossy()]);
    let made: Value = serde_json::from_slice(&out.stdout).expect("new prints JSON");
    let agent = made["pane"].as_u64().expect("the agent pane");
    let agent_tab = wait_for("the agent pane's tab in the app", || {
        tabs(&app.layout())
            .into_iter()
            .find(|t| t["root"]["pane"] == agent)
            .and_then(|t| t["id"].as_u64())
    });
    let chrome = wait_for("the agent icon in the tab bar", || {
        let c = app.request("chrome");
        c["agent_badges"].as_array().is_some_and(|b| !b.is_empty()).then_some(c)
    });
    let badges = chrome["agent_badges"].as_array().unwrap();
    assert_eq!(badges.len(), 1, "{chrome}");
    assert_eq!(badges[0]["tab"], agent_tab, "{chrome}");
    assert_ne!(agent_tab, shell_tab);
    // Drawn inside the agent tab's button, at a visible size.
    let r = &badges[0]["rect"];
    let tab_rect = chrome["actions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["action"] == format!("tab:{agent_tab}"))
        .map(|a| a["rect"].clone())
        .expect("the agent tab's box");
    let f = |v: &Value, i: usize| v[i].as_f64().unwrap();
    assert!(f(r, 2) > 4.0 && f(r, 3) > 4.0, "{r}");
    assert!(f(r, 0) >= f(&tab_rect, 0) && f(r, 0) + f(r, 2) <= f(&tab_rect, 0) + f(&tab_rect, 2), "{r} in {tab_rect}");
    assert!(f(r, 1) >= 0.0 && f(r, 1) + f(r, 3) <= 30.0, "in the bar: {r}");
}

#[test]
fn native_editing_drag_selects_in_a_field_and_copy_and_paste_go_through_it() {
    let env = env("field-drag");
    let app = App::start(&env, "a", &[]);
    let probe = focused_probe(&env, &app);
    probe.write(&hello(&["edit"]));
    probe.write(&open_with(
        "s1",
        "inline",
        json!([{"id": "ed", "k": "editor", "p": {"text": "hello world", "cursor": 0}}]),
    ));
    probe.write(&auk_conformance::probe::frame("s1", 2, json!([["focus", "ed"]])));
    acked(&probe, 2);
    caught_up(&env, &app, probe.pane);
    let pane = probe.pane;
    let field = wait_for("the field's box", || {
        let r = app.send(&format!("field-box {pane}"));
        (r["ok"] == true).then(|| r["value"].clone())
    });
    let (bx, by) = (field["box"][0].as_f64().unwrap(), field["box"][1].as_f64().unwrap());
    let (cw, ch) = (field["cell"][0].as_f64().unwrap(), field["cell"][1].as_f64().unwrap());
    let y = by + ch / 2.0;
    let (x0, x5) = (bx + 1.0, bx + 5.0 * cw + 1.0);
    // The press places the caret (an edit with no text); the drag selects natively, locally.
    app.request(&format!("mouse down {x0} {y}"));
    let caret = program_event(&probe, "edit");
    assert_eq!((caret["from"].clone(), caret["to"].clone()), (json!(0), json!(0)), "{caret}");
    app.request(&format!("mouse drag {} {y}", (x0 + x5) / 2.0));
    app.request(&format!("mouse drag {x5} {y}"));
    app.request(&format!("mouse up {x5} {y}"));
    let (bodies, bytes) = program_input(&probe, Duration::from_millis(300));
    assert!(bodies.is_empty() && bytes.is_empty(), "the drag stays local: {bodies:?} {bytes:?}");
    // ⌘C copies the field's selection.
    menu(&app, "Edit", "Copy");
    wait_for("the field's selection on the clipboard", || {
        (app.request("clipboard") == "hello").then_some(())
    });
    // A paste with the selection is an edit that replaces it (§7.9), not bracketed keys.
    app.request("clipboard-set XY");
    menu(&app, "Edit", "Paste");
    let edit = program_event(&probe, "edit");
    assert_eq!(
        edit,
        json!({"ev": "edit", "sf": "s1", "id": "ed", "from": 0, "to": 5, "text": "XY", "cursor": 2, "len": 11})
    );
    // With no selection left, a paste is the program's input.
    menu(&app, "Edit", "Paste");
    let got = probe.read_until("the pasted keys", |b| b.ends_with(b"XY"));
    assert!(events(&tsp_bodies(&split_input(&got)), "edit").is_empty());
}

#[test]
fn checklist_toggle_and_copy_of_md_and_ansi_from_the_window() {
    let env = env("actions");
    let app = App::start(&env, "a", &[]);
    let probe = focused_probe(&env, &app);
    probe.write(&hello(&[]));
    probe.write(&open_with(
        "s1",
        "inline",
        json!([
            {"id": "md", "k": "md", "p": {"text": "# Title\n\nsome **bold** text", "actions": {"click": "copy"}}},
            {"id": "an", "k": "ansi", "p": {"text": "\u{1b}[31mred\u{1b}[0m and plain", "actions": {"click": "copy"}}},
            {"id": "ck", "k": "checklist", "p": {"actions": {"click": "toggle"}, "phases": [
                {"id": "p1", "title": "Phase", "items": [
                    {"id": "a", "text": "first", "status": "done"},
                    {"id": "b", "text": "second", "status": "active"},
                    {"id": "c", "text": "third", "status": "pending"}
                ]}
            ]}}
        ]),
    ));
    acked(&probe, 1);
    let pane = probe.pane;
    // Copy: the md source, and the ansi text without its escapes.
    let md = wait_for("the md node", || node_box(&app, pane, "md"));
    click(&app, center(md));
    wait_for("the md source", || {
        (app.request("clipboard") == "# Title\n\nsome **bold** text").then_some(())
    });
    let an = wait_for("the ansi node", || node_box(&app, pane, "an"));
    click(&app, center(an));
    wait_for("the ansi text", || {
        (app.request("clipboard").as_str().map(str::trim_end) == Some("red and plain")).then_some(())
    });
    // Toggle folds the checklist to its phase heads and reports it.
    let ck = wait_for("the checklist", || node_box(&app, pane, "ck"));
    click(&app, (ck[0] + 30.0, ck[1] + ck[3] - 6.0));
    let toggle = program_event(&probe, "toggle");
    assert_eq!(toggle, json!({"ev": "toggle", "sf": "s1", "id": "ck", "collapsed": true}));
    wait_for("the folded checklist", || {
        let b = node_box(&app, pane, "ck")?;
        (b[3] < ck[3] - 10.0).then_some(())
    });
}

/// A stand-in system opener that logs each run's arguments, space-separated on one line, to
/// `opened.log`.
fn logging_opener(env: &TestEnv) -> (std::path::PathBuf, std::path::PathBuf) {
    let log = env.root.join("opened.log");
    let opener = env.root.join("opener.sh");
    std::fs::write(&opener, format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n", log.display()))
        .expect("write the opener");
    std::fs::set_permissions(&opener, std::os::unix::fs::PermissionsExt::from_mode(0o755)).expect("chmod");
    (opener, log)
}

/// §7.8: the `open` action opens a node's `href` or `path` (through the routes, to the system
/// opener), `copy` on a node with an `href` copies the `href`, and a click on the checklist
/// hud's pill opens it and sends `activate`.
#[test]
fn the_open_action_opens_href_or_path_copy_takes_the_href_and_the_checklist_hud_activates() {
    let env = env("open-act");
    let (opener, log) = logging_opener(&env);
    let notes = env.root.join("notes.txt");
    std::fs::write(&notes, "notes").expect("write notes");
    let notes = std::fs::canonicalize(&notes).expect("canonical");
    let app = App::start_with(&env, "a", &[], &[("AUK_OPENER", &opener.to_string_lossy())]);
    let probe = focused_probe(&env, &app);
    probe.write(&hello(&[]));
    probe.write(&open_with(
        "s1",
        "inline",
        json!([
            {"id": "op-href", "k": "text", "p": {"text": "the docs", "href": "https://example.com/open", "actions": {"click": "open"}}},
            {"id": "op-path", "k": "code", "p": {"text": "notes = 1", "path": notes.to_string_lossy(), "actions": {"click": "open"}}},
            {"id": "cp-href", "k": "text", "p": {"text": "copy me", "href": "https://example.com/copied", "actions": {"click": "copy"}}},
            {"id": "ck", "k": "checklist", "p": {"mode": "hud", "phases": [
                {"id": "p1", "title": "Phase", "items": [
                    {"id": "a", "text": "first", "status": "done"},
                    {"id": "b", "text": "second", "status": "active"}
                ]}
            ]}}
        ]),
    ));
    acked(&probe, 1);
    let pane = probe.pane;
    let opened = |n: usize| {
        wait_for(&format!("{n} opens"), || {
            let o = app.request("opens")["opened"].clone();
            (o.as_array()?.len() >= n).then_some(o)
        })
    };
    // open: the href (a plain click, not a link gesture: the action runs).
    click(&app, center(wait_for("the href node", || node_box(&app, pane, "op-href"))));
    let o = opened(1);
    assert_eq!((o[0]["target"].clone(), o[0]["origin"].clone()), (json!("https://example.com/open"), json!("link")), "{o}");
    // open: the path.
    click(&app, center(wait_for("the path node", || node_box(&app, pane, "op-path"))));
    let o = opened(2);
    assert_eq!(o[1]["target"], json!(notes.to_string_lossy()), "{o}");
    let logged = wait_for("the opener's log", || {
        let l = std::fs::read_to_string(&log).ok()?;
        (l.lines().count() >= 2).then_some(l)
    });
    // (The two openers run concurrently, so their lines can land in either order.)
    let mut lines: Vec<&str> = logged.lines().collect();
    lines.sort();
    let mut want = ["https://example.com/open", notes.to_str().unwrap()];
    want.sort();
    assert_eq!(lines, want);
    // copy: the node's href.
    click(&app, center(wait_for("the copy node", || node_box(&app, pane, "cp-href"))));
    wait_for("the href on the clipboard", || {
        (app.request("clipboard") == "https://example.com/copied").then_some(())
    });
    // The hud: a click on its pill opens it and sends activate.
    let hud = wait_for("the checklist hud", || node_box(&app, pane, "ck"));
    click(&app, (hud[0] + 14.0, hud[1] + hud[3] / 2.0));
    let activate = program_event(&probe, "activate");
    assert_eq!(activate, json!({"ev": "activate", "sf": "s1", "id": "ck", "item": "hud"}));
    wait_for("the open hud's list", || {
        let b = node_box(&app, pane, "ck")?;
        (b[3] > hud[3] + 10.0).then_some(())
    });
    // None of that typed anything, and opening ran nothing else.
    assert_eq!(app.request("opens")["opened"].as_array().unwrap().len(), 2);
}

/// §7.8: "A link click opens the link instead of running click." A node with a `click` action
/// holds a link span: the link gesture (⌘-click, or a plain click under `links_plain_click`) on
/// the link opens it and sends the program nothing; a plain click elsewhere, or on the link
/// without the setting, runs the node's click.
#[test]
fn a_link_click_inside_a_clickable_node_opens_the_link_instead_of_running_click() {
    let env = env("link-click");
    let (opener, log) = logging_opener(&env);
    let app = App::start_with(&env, "a", &[], &[("AUK_OPENER", &opener.to_string_lossy())]);
    let probe = focused_probe(&env, &app);
    let pane = probe.pane;
    probe.write(&hello(&[]));
    probe.write(&open_with(
        "s1",
        "inline",
        json!([{"id": "go", "k": "text", "p": {
            "spans": [{"t": "run me "}, {"t": "or read the linked docs", "href": "https://example.com/linked"}],
            "actions": {"click": "run"}
        }}]),
    ));
    acked(&probe, 1);
    caught_up(&env, &app, pane);
    let b = wait_for("the node", || node_box(&app, pane, "go"));
    let y = b[1] + b[3] / 2.0;
    // Where the link is, by the hover readout; and a point on the node off the link.
    let on_link = |x: f64| {
        app.request(&format!("mouse move {x} {y}"));
        std::thread::sleep(Duration::from_millis(60));
        app.request("chrome")["hover_link"] == "https://example.com/linked"
    };
    let link_x = (1..40)
        .map(|i| b[0] + f64::from(i) * 8.0)
        .find(|&x| on_link(x))
        .expect("a point over the link");
    let plain_x = b[0] + 3.0;
    assert!(!on_link(plain_x), "the node's first word isn't the link");
    let opened = |n: usize| {
        wait_for(&format!("{n} opens"), || {
            let o = app.request("opens")["opened"].clone();
            (o.as_array()?.len() >= n).then_some(o)
        })
    };
    let actions = |wait| -> Vec<Value> {
        program_input(&probe, wait).0.into_iter().filter(|b| b["ev"] == "action").collect()
    };

    // ⌘-click on the link: the link opens, and click doesn't run.
    app.request(&format!("mouse down {link_x} {y} --cmd"));
    app.request(&format!("mouse up {link_x} {y} --cmd"));
    let o = opened(1);
    assert_eq!(o[0]["target"], "https://example.com/linked", "{o}");
    assert_eq!(actions(Duration::from_millis(400)), Vec::<Value>::new());
    wait_for("the opener to run", || {
        std::fs::read_to_string(&log).ok().filter(|l| l.contains("https://example.com/linked")).map(|_| ())
    });

    // A plain click: no link gesture, so the node's click runs, on the link or off it.
    for x in [link_x, plain_x] {
        click(&app, (x, y));
        let action = program_event(&probe, "action");
        assert_eq!((&action["sf"], &action["id"]), (&json!("s1"), &json!("go")), "{action}");
    }
    assert_eq!(app.request("opens")["opened"].as_array().unwrap().len(), 1);

    // With plain clicks opening links, a plain click on the link opens it instead of running
    // click; off the link, click still runs.
    write_settings(&env, json!({"links_plain_click": true}));
    menu(&app, "View", "Reload Settings");
    // (The pane's views take the setting on the window's next frame.)
    wait_for("the setting in the pane's views", || {
        (app.request("settings")["view"]["links_plain_click"] == true).then_some(())
    });
    click(&app, (link_x, y));
    let o = opened(2);
    assert_eq!(o[1]["target"], "https://example.com/linked", "{o}");
    assert_eq!(actions(Duration::from_millis(400)), Vec::<Value>::new());
    click(&app, (plain_x, y));
    let action = program_event(&probe, "action");
    assert_eq!(action["id"], "go", "{action}");
    assert_eq!(app.request("opens")["opened"].as_array().unwrap().len(), 2);
}

/// §7.14: "`file://` links are limited to paths, and nothing runs a command." A `file://` link
/// opens its path; a file URL naming another host or carrying a query is refused with a toast;
/// a link to an executable is revealed in the Finder rather than opened; and a link whose scheme
/// the system hands to a terminal (`ssh:`) is refused.
#[test]
fn file_links_open_only_local_paths_and_links_never_run_a_command() {
    let env = env("file-links");
    let (opener, log) = logging_opener(&env);
    let doc = env.root.join("read me.txt");
    std::fs::write(&doc, "hello").expect("write the doc");
    let doc = std::fs::canonicalize(&doc).expect("canonical");
    let script = env.root.join("run.sh");
    std::fs::write(&script, "#!/bin/sh\ntouch ran\n").expect("write the script");
    std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).expect("chmod");
    let script = std::fs::canonicalize(&script).expect("canonical");
    let url = |p: &std::path::Path| format!("file://{}", p.to_string_lossy().replace(' ', "%20"));
    let app = App::start_with(&env, "a", &[], &[("AUK_OPENER", &opener.to_string_lossy())]);
    let probe = focused_probe(&env, &app);
    probe.write(&hello(&[]));
    let links = [
        ("ok-path", url(&doc)),
        ("other-host", format!("file://elsewhere.example{}", doc.display())),
        ("query", format!("{}?x=1", url(&doc))),
        ("exe", url(&script)),
        ("ssh", "ssh://example.com".to_owned()),
    ];
    let nodes: Vec<Value> = links
        .iter()
        .map(|(id, href)| json!({"id": id, "k": "text", "p": {"spans": [{"t": format!("link {id}"), "href": href}]}}))
        .collect();
    probe.write(&open_with("s1", "inline", Value::Array(nodes)));
    acked(&probe, 1);
    let pane = probe.pane;
    // Each one by ⌘-click, the explicit gesture.
    for (id, _) in &links {
        // (On the link's first characters: the node is as wide as the pane, its span isn't.)
        let b = wait_for("the link", || node_box(&app, pane, id));
        let (x, y) = (b[0] + 12.0, b[1] + b[3] / 2.0);
        app.request(&format!("mouse down {x} {y} --cmd"));
        app.request(&format!("mouse up {x} {y} --cmd"));
    }
    let opens = wait_for("two opens and three refusals", || {
        let o = app.request("opens");
        (o["opened"].as_array()?.len() + o["refused"].as_array()?.len() >= 5).then_some(o)
    });
    let opened: Vec<(Value, Value)> = opens["opened"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| (o["target"].clone(), o["how"].clone()))
        .collect();
    assert_eq!(
        opened,
        [
            (json!(doc.to_string_lossy()), json!("default")),
            (json!(script.to_string_lossy()), json!("reveal"))
        ],
        "{opens}"
    );
    let refused: Vec<&Value> = opens["refused"].as_array().unwrap().iter().map(|r| &r["target"]).collect();
    assert_eq!(refused, [&json!(links[1].1), &json!(links[2].1), &json!(links[4].1)], "{opens}");
    let toasts = app.request("toasts");
    let told: Vec<&Value> = toasts
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["level"] == "error")
        .map(|t| &t["sub"])
        .collect();
    assert_eq!(told, refused, "each refusal is a toast: {toasts}");
    // What the system opener got: the document's path, and the script only to reveal.
    let logged = wait_for("the opener's log", || {
        let l = std::fs::read_to_string(&log).ok()?;
        (l.lines().count() >= 2).then_some(l)
    });
    let mut lines: Vec<String> = logged.lines().map(str::to_owned).collect();
    lines.sort();
    let mut want = [doc.display().to_string(), format!("-R {}", script.display())];
    want.sort();
    assert_eq!(lines, want);
    assert!(!env.root.join("ran").exists() && !std::path::Path::new("ran").exists(), "the script never ran");
}

#[test]
fn selection_links_mouse_reports_and_the_cursor_follow_rows_a_surface_block_shifts() {
    let env = env("shift");
    let app = App::start(&env, "a", &[]);
    let probe = focused_probe(&env, &app);
    probe.write(&hello(&[]));
    probe.write(b"above\r\n");
    probe.write(&open_with(
        "s1",
        "inline",
        json!([{"id": "t", "k": "text", "p": {"text": "one\ntwo\nthree\nfour\nfive"}}]),
    ));
    acked(&probe, 1);
    probe.write(&auk_conformance::probe::tsp_json("x", &json!({"id": "s1"})));
    probe.write(b"below \x1b]8;;https://example.com/below\x1b\\link\x1b]8;;\x1b\\\r\nsecond row\r\n> ");
    caught_up(&env, &app, probe.pane);
    let pane = probe.pane;
    // Drawn once the closed surface lets the rows after it show (lines 2 to 4).
    let layout = pane_layout(&app, pane, "the block and the rows below it", |l| {
        l["blocks"].as_array().is_some_and(|b| b.len() == 1)
            && (2..=4).all(|line| line_top(l, line).is_some_and(|(_, h)| h > 0.0))
    });
    let block = &layout["blocks"][0];
    let anchor = block["line"].as_u64().unwrap();
    let cw = layout["cell"][0].as_f64().unwrap();
    let ch = layout["cell"][1].as_f64().unwrap();
    let ox = layout["origin"][0].as_f64().unwrap();
    // The block is taller than its row, so the rows after it sit lower than a plain grid's.
    let (link_top, link_h) = line_top(&layout, anchor + 1).unwrap();
    let (anchor_top, _) = line_top(&layout, anchor).unwrap();
    assert!(link_top - anchor_top > 2.0 * ch, "{layout}");
    let (second_top, _) = line_top(&layout, anchor + 2).unwrap();
    let mid = |top: f64| top + link_h / 2.0;

    // Selection: a drag over "second" on the shifted row.
    app.request(&format!("mouse down {} {}", ox + 0.5 * cw, mid(second_top)));
    app.request(&format!("mouse drag {} {}", ox + 5.5 * cw, mid(second_top)));
    app.request(&format!("mouse up {} {}", ox + 5.5 * cw, mid(second_top)));
    assert_eq!(app.request("selection"), "second");

    // Links: the hover readout names the link on the shifted row.
    app.request(&format!("mouse move {} {}", ox + 8.5 * cw, mid(link_top)));
    wait_for("the shifted link's target", || {
        (app.request("chrome")["hover_link"] == "https://example.com/below").then_some(())
    });

    // The cursor: the input method is told where the cursor's row was drawn.
    let cursor_line = anchor + 3;
    let (cursor_top, _) = line_top(&layout, cursor_line).unwrap();
    let ime = wait_for("the IME area", || {
        let a = app.state()["ime_area"].clone();
        a.is_array().then_some(a)
    });
    assert!((ime[1].as_f64().unwrap() - cursor_top).abs() < 0.5, "{ime} vs {cursor_top}");
    assert!((ime[0].as_f64().unwrap() - (ox + 2.0 * cw)).abs() < 0.5, "{ime}");

    // Mouse reports name the screen cell under the pointer, not a plain grid's row.
    probe.write(b"\x1b[?1000h\x1b[?1006h");
    caught_up(&env, &app, pane);
    let screen = app.request(&format!("capture {pane}"))["text"].as_str().unwrap().to_owned();
    let row = screen.lines().position(|l| l.starts_with("second row")).unwrap() + 1;
    app.request(&format!("mouse down {} {}", ox + 3.5 * cw, mid(second_top)));
    app.request(&format!("mouse up {} {}", ox + 3.5 * cw, mid(second_top)));
    let got = probe.read_until("the press and release", |b| b.ends_with(b"m"));
    assert_eq!(
        String::from_utf8_lossy(&got),
        format!("\x1b[<0;4;{row}M\x1b[<0;4;{row}m")
    );
}

#[test]
fn a_screen_surface_over_an_inline_one_takes_pointer_caret_and_visible() {
    let env = env("screen-over");
    let app = App::start(&env, "a", &[]);
    let probe = focused_probe(&env, &app);
    probe.write(&hello(&["edit"]));
    probe.write(&open_with(
        "s1",
        "inline",
        json!([
            {"id": "in-go", "k": "text", "p": {"text": "inline button", "actions": {"click": "inline-act"}}},
            {"id": "ed1", "k": "editor", "p": {"text": "inline field", "cursor": 0}}
        ]),
    ));
    probe.write(&auk_conformance::probe::frame("s1", 2, json!([["focus", "ed1"]])));
    acked(&probe, 2);
    let pane = probe.pane;
    let inline_go = wait_for("the inline button", || node_box(&app, pane, "in-go"));
    // A screen surface covers the pane and the inline surface.
    probe.write(&open_with(
        "s2",
        "screen",
        json!([
            {"id": "sc-go", "k": "text", "p": {"text": "screen button", "actions": {"click": "screen-act"}}},
            {"id": "ed2", "k": "input", "p": {"text": "screen field", "cursor": 0}}
        ]),
    ));
    probe.write(&auk_conformance::probe::frame("s2", 2, json!([["focus", "ed2"]])));
    acked(&probe, 2);
    pane_layout(&app, pane, "the screen surface", |l| l["screen"] == true);
    let _ = program_input(&probe, Duration::from_millis(200));
    // Pointer: where the inline button was drawn, the screen surface takes the press.
    click(&app, center(inline_go));
    let sc = wait_for("the screen button", || node_box(&app, pane, "sc-go"));
    click(&app, center(sc));
    let (bodies, _) = program_input(&probe, Duration::from_millis(500));
    let actions: Vec<&Value> = events(&bodies, "action");
    assert!(!actions.is_empty(), "{bodies:?}");
    assert!(actions.iter().all(|a| a["sf"] == "s2"), "{bodies:?}");
    // Caret: native editing keys go to the screen surface's field.
    app.request("type shift+right");
    let edit = stroke_event(&app, &probe, "backspace", "edit");
    assert_eq!((edit["sf"].clone(), edit["id"].clone()), (json!("s2"), json!("ed2")), "{edit}");
    // Resize: a narrower window reports the new width to the screen surface, not the inline one.
    let _ = probe.read_for(Duration::from_millis(200));
    app.request("window-size 560 420");
    let mut input = probe.read_until("a resize event", |b| {
        !events(&tsp_bodies(&split_input(b)), "resize").is_empty()
    });
    input.extend(probe.read_for(Duration::from_millis(500)));
    let bodies = tsp_bodies(&split_input(&input));
    let resizes = events(&bodies, "resize");
    assert!(resizes.iter().all(|r| r["sf"] == "s2"), "{resizes:?}");
    // `visible`: when no window shows the pane, only the screen surface hears of it.
    app.quit();
    let input = probe.read_until("visible:false", |b| {
        !events(&tsp_bodies(&split_input(b)), "visible").is_empty()
    });
    std::thread::sleep(Duration::from_millis(200));
    let mut all = input;
    all.extend(probe.read_for(Duration::from_millis(200)));
    let bodies = tsp_bodies(&split_input(&all));
    assert_eq!(
        events(&bodies, "visible"),
        [&json!({"ev": "visible", "sf": "s2", "visible": false})]
    );
}

/// A text node per line, ids `{prefix}{i}`.
fn text_lines(prefix: &str, n: usize) -> Value {
    Value::Array(
        (0..n)
            .map(|i| json!({"id": format!("{prefix}{i}"), "k": "text", "p": {"text": format!("{prefix} line {i}")}}))
            .collect(),
    )
}

fn num(v: &Value) -> f64 {
    v.as_f64().unwrap_or_else(|| panic!("not a number: {v}"))
}

/// §7.5 `dock`: sticky at the pane bottom while live, `main` laid out above it, and the tail kept
/// in view until the user scrolls up; checked by where the window drew them.
#[test]
fn the_dock_sits_at_the_pane_bottom_with_main_above_it_and_the_tail_in_view() {
    let env = env("dock");
    let app = App::start(&env, "a", &[]);
    let probe = focused_probe(&env, &app);
    let pane = probe.pane;
    probe.write(&hello(&["dock"]));
    let mut out = Vec::new();
    for i in 0..80 {
        out.extend(format!("output {i}\r\n").into_bytes());
    }
    probe.write(&out);
    probe.write(&open_with("s1", "inline", text_lines("m", 4)));
    probe.write(&auk_conformance::probe::frame(
        "s1",
        2,
        json!([["add", "dock", "s1", null, {"id": "dock", "k": "col", "c": [
            {"id": "d1", "k": "text", "p": {"text": "docked one"}},
            {"id": "d2", "k": "text", "p": {"text": "docked two"}}
        ]}]]),
    ));
    acked(&probe, 2);
    // Where the dock, main and the flow are, once both are drawn.
    let check = |what: &str, main_lines: usize| -> (Value, [f64; 4]) {
        let l = pane_layout(&app, pane, what, |l| {
            l["dock"].is_object()
                && l["blocks"].as_array().is_some_and(|b| b.len() == 1)
                && node_box(&app, pane, &format!("m{}", main_lines - 1)).is_some()
        });
        let (oy, content) = (num(&l["origin"][1]), num(&l["content_height"]));
        let dock_h = num(&l["dock"]["height"]);
        let dock = node_box(&app, pane, "dock").expect("the dock drawn");
        // Sticky at the pane bottom.
        assert!(
            (dock[1] + dock[3] - (oy + content)).abs() < 1.0,
            "{what}: the dock's bottom {} is the pane's {}",
            dock[1] + dock[3],
            oy + content
        );
        assert!((dock[3] - dock_h).abs() < 1.0, "{what}: {dock:?} vs {dock_h}");
        for id in ["d1", "d2"] {
            let d = node_box(&app, pane, id).expect("docked text drawn");
            assert!(d[1] >= dock[1] - 0.5 && d[1] + d[3] <= dock[1] + dock[3] + 0.5, "{what}: {id}");
        }
        // `main` lays out above it, and the tail (the surface's last line) is in view right
        // above the dock.
        let flow_bottom = oy + content - dock_h;
        let last = node_box(&app, pane, &format!("m{}", main_lines - 1)).unwrap();
        assert!(last[1] + last[3] <= flow_bottom + 0.5, "{what}: main's last line {last:?} is above the dock at {flow_bottom}");
        assert!(last[1] >= oy, "{what}: and in view");
        let block = &l["blocks"][0];
        let block_bottom = oy + num(&block["top"]) + num(&block["height"]);
        assert!(block_bottom <= flow_bottom + 0.5, "{what}: {block_bottom} > {flow_bottom}");
        assert!(
            block_bottom >= flow_bottom - 2.0 * num(&l["cell"][1]),
            "{what}: the tail is kept at the bottom of the flow: {block_bottom} vs {flow_bottom}"
        );
        (l, dock)
    };
    let (l, dock) = check("the dock and main", 4);
    assert_eq!(l["following"], true);
    // main grows: the tail stays in view above the dock.
    let more: Vec<Value> = (4..12)
        .map(|i| json!(["add", format!("m{i}"), "main", null, {"id": format!("m{i}"), "k": "text", "p": {"text": format!("m line {i}")}}]))
        .collect();
    probe.write(&auk_conformance::probe::frame("s1", 3, Value::Array(more)));
    acked(&probe, 3);
    let (l2, dock2) = check("main grown", 12);
    assert_eq!(l2["following"], true);
    assert_eq!(dock2, dock, "the dock didn't move");
    // The user scrolls up: the dock stays at the bottom, and the flow stops following the tail.
    let (ox, oy) = (num(&l2["origin"][0]), num(&l2["origin"][1]));
    let grid_y = oy + num(&l2["blocks"][0]["top"]) / 2.0;
    app.request(&format!("scroll 5 {} {grid_y}", ox + 40.0));
    let up = pane_layout(&app, pane, "the flow scrolled up", |l| l["following"] == false);
    let from_bottom = num(&up["from_bottom"]);
    assert!(from_bottom > 0.0, "{up}");
    assert_eq!(node_box(&app, pane, "dock"), Some(dock), "the dock stays put");
    probe.write(&auk_conformance::probe::frame(
        "s1",
        4,
        json!([["add", "m12", "main", null, {"id": "m12", "k": "text", "p": {"text": "m line 12"}}]]),
    ));
    acked(&probe, 4);
    let after = pane_layout(&app, pane, "the frame drawn while scrolled up", |l| {
        l["blocks"][0]["height"].as_f64() > l2["blocks"][0]["height"].as_f64()
    });
    assert_eq!(after["following"], false, "the tail isn't followed once the user scrolled up");
    assert_eq!(node_box(&app, pane, "dock"), Some(dock));
}

/// §7.5 `main` in a screen surface takes the pane's height and scrolls itself: the wheel over it
/// scrolls its content, not the (hidden) grid.
#[test]
fn a_screen_surfaces_main_takes_the_pane_height_and_scrolls_itself() {
    let env = env("screen-main");
    let app = App::start(&env, "a", &[]);
    let probe = focused_probe(&env, &app);
    let pane = probe.pane;
    probe.write(&hello(&[]));
    let mut out = Vec::new();
    for i in 0..80 {
        out.extend(format!("output {i}\r\n").into_bytes());
    }
    probe.write(&out);
    probe.write(&open_with("s2", "screen", text_lines("r", 150)));
    acked(&probe, 1);
    let l = pane_layout(&app, pane, "the screen surface", |l| l["screen"] == true);
    let (ox, oy, content) = (num(&l["origin"][0]), num(&l["origin"][1]), num(&l["content_height"]));
    let main = wait_for("the screen surface's main", || node_box(&app, pane, "main"));
    assert!((main[1] - oy).abs() < 1.0, "main starts at the pane top: {main:?} vs {oy}");
    assert!((main[3] - content).abs() < 1.0, "main takes the pane's height: {main:?} vs {content}");
    let r0 = node_box(&app, pane, "r0").expect("the first line");
    assert!(r0[1] >= oy && r0[1] < oy + 20.0, "{r0:?}");
    let grid = (l["from_bottom"].clone(), l["first_line"].clone());
    // Wheel down over it: main scrolls its content; the grid under it stays where it was.
    app.request(&format!("scroll -6 {} {}", ox + 40.0, oy + content / 2.0));
    let moved = wait_for("main to scroll", || {
        let r = node_box(&app, pane, "r0")?;
        (r[1] < r0[1] - 20.0).then_some(r)
    });
    let l2 = pane_layout(&app, pane, "the pane after the wheel", |l| l["screen"] == true);
    assert_eq!((l2["from_bottom"].clone(), l2["first_line"].clone()), grid, "the grid didn't scroll");
    assert_eq!(l2["following"], true);
    let main2 = node_box(&app, pane, "main").unwrap();
    assert_eq!(main2, main, "main itself stays put");
    // And back up to its start.
    app.request(&format!("scroll 20 {} {}", ox + 40.0, oy + content / 2.0));
    wait_for("main to scroll back to its start", || {
        let r = node_box(&app, pane, "r0")?;
        ((r[1] - r0[1]).abs() < 0.5).then_some(())
    });
    assert!(moved[1] < r0[1]);
}

/// §7.5 `layer` overlays above everything: placed from the pane's top whatever the flow does,
/// and on top of the surface's `main` where they overlap, so a press there is the layer's.
#[test]
fn a_layer_overlays_the_pane_from_its_top_above_main() {
    let env = env("layer");
    let app = App::start(&env, "a", &[]);
    let probe = focused_probe(&env, &app);
    let pane = probe.pane;
    probe.write(&hello(&[]));
    probe.write(&open_with(
        "s3",
        "inline",
        json!([{"id": "in-go", "k": "text", "p": {"text": "inline button\nsecond line\nthird line", "actions": {"click": "inline-act"}}}]),
    ));
    probe.write(&auk_conformance::probe::frame(
        "s3",
        2,
        json!([["add", "layer", "s3", null, {"id": "layer", "k": "col", "c": [
            {"id": "ly", "k": "text", "p": {"text": "layer", "actions": {"click": "layer-act"}}}
        ]}]]),
    ));
    acked(&probe, 2);
    let l = pane_layout(&app, pane, "the layer up", |l| l["layers"] == true);
    let (ox, oy) = (num(&l["origin"][0]), num(&l["origin"][1]));
    let ly = wait_for("the layer's node", || node_box(&app, pane, "ly"));
    let inline = wait_for("main's node", || node_box(&app, pane, "in-go"));
    assert!((ly[0] - ox).abs() < 1.0 && (ly[1] - oy).abs() < 1.0, "at the pane's top-left: {ly:?} vs ({ox}, {oy})");
    // They overlap: the layer is on top.
    let overlap = (ly[0] + ly[2].min(inline[2]) / 2.0, ly[1].max(inline[1]) + 2.0);
    assert!(overlap.1 < inline[1] + inline[3] && overlap.1 < ly[1] + ly[3], "{ly:?} {inline:?}");
    let _ = program_input(&probe, Duration::from_millis(200));
    click(&app, overlap);
    assert_eq!(program_event(&probe, "action")["id"], "ly");
    // Below the layer's node, the layer lets presses through to main.
    assert!(ly[1] + ly[3] + 4.0 < inline[1] + inline[3], "{ly:?} {inline:?}");
    let _ = program_input(&probe, Duration::from_millis(200));
    click(&app, (inline[0] + 10.0, inline[1] + inline[3] - 4.0));
    assert_eq!(program_event(&probe, "action")["id"], "in-go");
    // With main far down the flow, a layer is still placed from the pane's top.
    probe.write(&auk_conformance::probe::tsp_json("x", &json!({"id": "s3", "keep": false})));
    let mut out = Vec::new();
    for i in 0..60 {
        out.extend(format!("pushed {i}\r\n").into_bytes());
    }
    probe.write(&out);
    probe.write(&open_with(
        "s4",
        "inline",
        json!([{"id": "low", "k": "text", "p": {"text": "main at the bottom"}}]),
    ));
    probe.write(&auk_conformance::probe::frame(
        "s4",
        2,
        json!([["add", "layer", "s4", null, {"id": "layer", "k": "col", "c": [
            {"id": "ly4", "k": "text", "p": {"text": "layer again"}}
        ]}]]),
    ));
    acked(&probe, 2);
    let low = wait_for("main far down the flow", || {
        let b = node_box(&app, pane, "low")?;
        (b[1] > oy + 200.0).then_some(b)
    });
    let ly4 = wait_for("the second layer", || node_box(&app, pane, "ly4"));
    assert!((ly4[1] - oy).abs() < 1.0, "the layer at the pane's top: {ly4:?}, main at {low:?}");
    // Closing the surface removes its layer.
    probe.write(&auk_conformance::probe::tsp_json("x", &json!({"id": "s4"})));
    pane_layout(&app, pane, "the layer gone", |l| l["layers"] == false);
    assert_eq!(node_box(&app, pane, "ly4"), None);
}

/// §8.1 `title` is the node's tooltip: hovering the node (or anything inside it) shows it by the
/// pointer, and it goes when the pointer leaves.
#[test]
fn hovering_a_node_with_a_title_shows_its_tooltip() {
    let env = env("tooltip");
    let app = App::start(&env, "a", &[]);
    let probe = focused_probe(&env, &app);
    let pane = probe.pane;
    probe.write(&hello(&[]));
    probe.write(b"one\r\ntwo\r\nthree\r\nfour\r\n");
    probe.write(&open_with(
        "s1",
        "inline",
        json!([
            {"id": "row", "k": "row", "p": {"title": "Row tip"}, "c": [
                {"id": "inner", "k": "text", "p": {"text": "inside the row"}}
            ]},
            {"id": "tipped", "k": "text", "p": {"text": "has a tooltip", "title": "Saved 3 files"}},
            {"id": "plain", "k": "text", "p": {"text": "no tooltip"}}
        ]),
    ));
    acked(&probe, 1);
    // Each check moves the pointer again (a real pointer passing over the window moves it too).
    let tooltip = |at: (f64, f64)| {
        app.request(&format!("mouse move {} {}", at.0, at.1));
        app.request("chrome")
    };
    let tipped = wait_for("the node drawn", || node_box(&app, pane, "tipped"));
    let at = (tipped[0] + 20.0, tipped[1] + tipped[3] / 2.0);
    let chrome = wait_for("the tooltip drawn", || {
        let c = tooltip(at);
        (c["tooltip_drawn"]["text"] == "Saved 3 files").then_some(c)
    });
    assert_eq!(chrome["tooltip"]["text"], "Saved 3 files", "{chrome}");
    assert_eq!(chrome["tooltip"]["at"], json!([at.0, at.1]));
    // The chrome draws it just below the pointer.
    let drawn = &chrome["tooltip_drawn"];
    let r: Vec<f64> = drawn["rect"].as_array().unwrap().iter().map(num).collect();
    assert!(r[1] > at.1 && r[1] < at.1 + 40.0, "below the pointer: {drawn}");
    assert!(r[0] >= at.0 && r[0] < at.0 + 20.0 && r[2] > 40.0 && r[3] > 8.0, "{drawn}");
    // The nearest ancestor with a title gives it.
    let inner = node_box(&app, pane, "inner").unwrap();
    let at = (inner[0] + 20.0, inner[1] + inner[3] / 2.0);
    wait_for("the row's tooltip", || {
        (tooltip(at)["tooltip_drawn"]["text"] == "Row tip").then_some(())
    });
    // No title, no tooltip; nor over the grid.
    let plain = node_box(&app, pane, "plain").unwrap();
    let at = (plain[0] + 20.0, plain[1] + plain[3] / 2.0);
    wait_for("no tooltip over a node without a title", || {
        let c = tooltip(at);
        (c["tooltip"].is_null() && c["tooltip_drawn"].is_null()).then_some(())
    });
    let l = pane_layout(&app, pane, "the flow", |l| l["rows"].is_array());
    let (_, line_y) = line_top(&l, 1).expect("line 1 drawn");
    let at = (plain[0] + 20.0, line_y + 4.0);
    wait_for("no tooltip over the grid", || {
        let c = tooltip(at);
        (c["tooltip"].is_null() && c["tooltip_drawn"].is_null()).then_some(())
    });
}

/// §5.5: Edit ▸ Clear Scrollback (⌘K) is the in-band pane operation: the daemon records it, so
/// every replica's scrollback clears (the other window's and the daemon's too), the screen stays,
/// and nothing reaches the program.
#[test]
fn clear_scrollback_from_the_window_clears_every_replicas_scrollback() {
    let env = env("clear-sb");
    let a = App::start(&env, "a", &[]);
    let probe = focused_probe(&env, &a);
    let pane = probe.pane;
    let b = App::start(&env, "b", &[]);
    show(&b, pane);
    let mut out = String::new();
    for i in 0..300 {
        out.push_str(&format!("scrolled line {i}\r\n"));
    }
    out.push_str("the last line");
    probe.write(out.as_bytes());
    caught_up(&env, &a, pane);
    caught_up(&env, &b, pane);
    for (who, text) in [
        ("a", a.request(&format!("capture {pane} --scrollback"))["text"].clone()),
        ("b", b.request(&format!("capture {pane} --scrollback"))["text"].clone()),
        ("the daemon", env.daemon(&format!("capture {pane} --scrollback"))["text"].clone()),
    ] {
        assert!(text.as_str().unwrap().contains("scrolled line 0\n"), "{who}: {text}");
    }
    let _ = program_input(&probe, Duration::from_millis(200));

    menu(&a, "Edit", "Clear Scrollback");
    let cleared = |text: &Value| {
        let t = text.as_str().unwrap_or_default();
        !t.contains("scrolled line 0\n") && t.contains("the last line")
    };
    wait_for("the daemon's scrollback to clear", || {
        cleared(&env.daemon(&format!("capture {pane} --scrollback"))["text"]).then_some(())
    });
    caught_up(&env, &a, pane);
    caught_up(&env, &b, pane);
    for app in [&a, &b] {
        let text = app.request(&format!("capture {pane} --scrollback"))["text"].clone();
        assert!(cleared(&text), "{text}");
        let screen = app.request(&format!("capture {pane}"))["text"].clone();
        assert!(screen.as_str().unwrap().contains("the last line"), "the screen stays: {screen}");
    }
    // Every replica agrees with the daemon.
    let (hash, seq) = env.daemon_hash(pane);
    for app in [&a, &b] {
        app.caught_up(pane, seq);
        let h = app.request(&format!("state-hash {pane}"));
        assert_eq!(h["hash"], hash, "{h}");
    }
    // ⌘K is no keystroke: the program read nothing (no ^L).
    let (bodies, bytes) = program_input(&probe, Duration::from_millis(300));
    assert!(bodies.is_empty() && bytes.is_empty(), "{bodies:?} {bytes:?}");
}

#[test]
fn view_state_belongs_to_each_window_and_never_reaches_snapshots() {
    let env = env("view-state");
    let a = App::start(&env, "a", &[]);
    let probe = focused_probe(&env, &a);
    let b = App::start(&env, "b", &[]);
    let pane = probe.pane;
    show(&b, pane);
    probe.write(&hello(&[]));
    let mut out = String::new();
    for i in 0..120 {
        out.push_str(&format!("line {i}\r\n"));
    }
    probe.write(out.as_bytes());
    probe.write(&open_with(
        "s1",
        "flow",
        json!([{"id": "card", "k": "card", "p": {"head": "Details", "collapsible": true}, "c": [
            {"id": "body", "k": "text", "p": {"text": "1\n2\n3\n4\n5\n6"}}
        ]}]),
    ));
    acked(&probe, 1);
    caught_up(&env, &a, pane);
    caught_up(&env, &b, pane);
    let (hash_before, _) = env.daemon_hash(pane);
    let open_a = wait_for("the card in a", || node_box(&a, pane, "card"));
    let open_b = wait_for("the card in b", || node_box(&b, pane, "card"));

    // Fold in a: a's card shrinks, b's doesn't.
    click(&a, (open_a[0] + 40.0, open_a[1] + 8.0));
    let toggle = program_event(&probe, "toggle");
    assert_eq!(toggle["collapsed"], true, "{toggle}");
    wait_for("a's card to fold", || {
        (node_box(&a, pane, "card")?[3] < open_a[3] - 20.0).then_some(())
    });
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(node_box(&b, pane, "card").map(|r| r[3]), Some(open_b[3]));
    // Select and scroll in a: b has no selection and still follows the tail.
    menu(&a, "Edit", "Select All");
    assert!(a.request("selection").is_string());
    let la = pane_layout(&a, pane, "a's flow drawn with its scrollback", |l| {
        l["max_scroll"].as_f64().is_some_and(|m| m > 0.0)
    });
    assert_eq!(la["following"], true);
    menu(&a, "View", "Scroll to Top");
    pane_layout(&a, pane, "a scrolled to the top", |l| l["following"] == false && l["first_line"] == 0);
    assert!(b.request("selection").is_null());
    assert_eq!(pane_layout(&b, pane, "b's flow", |l| l["rows"].is_array())["following"], true);

    // None of it is pane state: the daemon's hash is unchanged, and a window attaching now
    // (from a snapshot) shows the card open, nothing selected, following the tail.
    assert_eq!(env.daemon_hash(pane).0, hash_before);
    let c = App::start(&env, "c", &[]);
    show(&c, pane);
    let open_c = wait_for("the card in c", || node_box(&c, pane, "card"));
    assert_eq!(open_c[3], open_b[3]);
    assert!(c.request("selection").is_null());
    assert_eq!(pane_layout(&c, pane, "c's flow", |l| l["rows"].is_array())["following"], true);
}

/// Writes the settings file the app reads (`<config>/settings.json`; the harness's config dir
/// is the state dir).
fn write_settings(env: &TestEnv, settings: Value) {
    std::fs::write(env.state.join("settings.json"), settings.to_string()).expect("write settings.json");
}

/// §7.8, §7.9, §7.12, §7.13: the settings file turns off program colors and native editing,
/// makes a plain click open links, and reduces motion (Auk's own setting over the system's); the
/// program learns motion from the hello and a `motion` event when "Reload Settings" changes it.
#[test]
fn settings_drive_program_colors_native_editing_plain_click_links_and_reduce_motion() {
    let env = env("settings");
    write_settings(
        &env,
        json!({"program_colors": false, "native_editing": false, "links_plain_click": true, "reduce_motion": true}),
    );
    let opener = env.root.join("opener.sh");
    std::fs::write(&opener, "#!/bin/sh\nexit 0\n").expect("write the opener");
    std::fs::set_permissions(&opener, std::os::unix::fs::PermissionsExt::from_mode(0o755)).expect("chmod");
    let app = App::start_with(&env, "a", &[], &[("AUK_OPENER", &opener.to_string_lossy())]);
    let probe = focused_probe(&env, &app);
    let pane = probe.pane;
    let settings = wait_for("the views' settings", || {
        let s = app.request("settings");
        (!s["view"].is_null()).then_some(s)
    });
    assert_eq!(
        settings["view"],
        json!({"program_colors": false, "native_editing": false, "links_plain_click": true, "reduce_motion": true}),
        "{settings}"
    );

    // The program's hello says motion is reduced.
    probe.write(&hello(&["reduce-motion"]));
    let input = probe.read_until("the hello reply", |b| {
        tsp_bodies(&split_input(b)).iter().any(|v| v.get("reduceMotion").is_some())
    });
    let reply = tsp_bodies(&split_input(&input)).into_iter().find(|v| v.get("reduceMotion").is_some()).unwrap();
    assert_eq!(reply["reduceMotion"], true, "{reply}");

    // A plain click on a link opens it.
    probe.write(b"see \x1b]8;;https://example.com/plain\x1b\\the link\x1b]8;;\x1b\\ here\r\n");
    caught_up(&env, &app, pane);
    let layout = pane_layout(&app, pane, "the link's row", |l| line_top(l, 0).is_some());
    let cw = layout["cell"][0].as_f64().unwrap();
    let (top, h) = line_top(&layout, 0).unwrap();
    let (x, y) = (layout["origin"][0].as_f64().unwrap() + cw * 6.5, top + h / 2.0);
    click(&app, (x, y));
    let opened = wait_for("the plain click's open", || {
        let o = app.request("opens")["opened"].clone();
        (o.as_array()?.len() == 1).then_some(o)
    });
    assert_eq!(opened[0]["target"], "https://example.com/plain", "{opened}");

    // Reload Settings with the defaults back: motion runs again (a `motion` event), and a plain
    // click opens nothing while a ⌘-click does.
    write_settings(&env, json!({"reduce_motion": false}));
    // (Events go to a program with a live listening surface.)
    probe.write(&open_with("s1", "inline", json!([{"id": "t", "k": "text", "p": {"text": "x"}}])));
    acked(&probe, 1);
    menu(&app, "View", "Reload Settings");
    let motion = program_event(&probe, "motion");
    assert_eq!(motion["reduce"], false, "{motion}");
    let settings = app.request("settings");
    assert_eq!(settings["links_plain_click"], false, "{settings}");
    click(&app, (x, y));
    app.request(&format!("mouse down {x} {y} --cmd"));
    app.request(&format!("mouse up {x} {y} --cmd"));
    let opened = wait_for("the ⌘-click's open", || {
        let o = app.request("opens")["opened"].clone();
        (o.as_array()?.len() >= 2).then_some(o)
    });
    assert_eq!(opened.as_array().unwrap().len(), 2, "the plain click opened nothing: {opened}");
    wait_for("the views' settings back to the defaults", || {
        let v = app.request("settings")["view"].clone();
        (v == json!({"program_colors": true, "native_editing": true, "links_plain_click": false, "reduce_motion": false}))
            .then_some(())
    });
}

/// §9.8 keymap layers: the preset (`keymap`, `keymap_prefix`) under the user's `keybinds`, which
/// replace a preset chord, take a default's keys from the menu, and `unbind` one.
#[test]
fn keymap_preset_and_user_keybinds_with_unbind() {
    let env = env("keymap");
    write_settings(
        &env,
        json!({
            "keymap": "tmux",
            "keymap_prefix": "ctrl+a",
            "keybinds": {
                "ctrl+a>c": "split_down",
                "cmd+d": ["plugin.nobody.here", "new_tab"],
                "cmd+shift+d": "unbind",
            },
        }),
    );
    let app = App::start(&env, "a", &[]);
    let _us = InputSource::select(&app, US_LAYOUT);
    let first = wait_for("a focused pane", || app.state()["focused"].as_u64());
    env.wait(first, "prompt");
    fn leaves(n: &Value) -> usize {
        if n["pane"].is_u64() {
            1
        } else {
            leaves(&n["first"]) + leaves(&n["second"])
        }
    }
    let panes = |app: &App| -> usize { tabs(&app.layout()).iter().map(|t| leaves(&t["root"])).sum() };

    // The preset: ctrl+a then % splits right.
    app.request("type ctrl+a shift+5");
    wait_for("the preset's split", || (panes(&app) == 2).then_some(()));
    assert_eq!(tabs(&app.layout()).len(), 1);
    assert_eq!(tabs(&app.layout())[0]["root"]["dir"], "right");

    // The user layer replaces the preset's ctrl+a>c (new tab) with split down.
    app.request("type ctrl+a c");
    wait_for("the user's split", || (panes(&app) == 3).then_some(()));
    assert_eq!(tabs(&app.layout()).len(), 1, "no new tab");

    // A user keybind takes a default's keys (cmd+d) from the menu; its list runs the first
    // action that applies.
    assert_eq!(app.request("menu-key Shell 'Split Right'")["key"], "", "the menu gives cmd+d up");
    app.request("type cmd+d");
    wait_for("the new tab", || (tabs(&app.layout()).len() == 2).then_some(()));
    // `unbind` removes a default without binding it again; the others keep theirs.
    assert_eq!(app.request("menu-key Shell 'Split Down'")["key"], "");
    assert_eq!(app.request("menu-key Shell 'New Tab'")["key"], "t");
    let settings = app.request("settings");
    assert!(settings["user"].as_array().unwrap().len() == 2, "unbind binds nothing: {settings}");
}

#[test]
fn with_native_editing_switched_off_field_keys_and_pastes_are_the_programs() {
    let env = env("native-off");
    write_settings(&env, json!({"native_editing": false}));
    let app = App::start(&env, "a", &[]);
    let _us = InputSource::select(&app, US_LAYOUT);
    let probe = focused_probe(&env, &app);
    probe.write(&hello(&["edit", "undo"]));
    probe.write(&open_with(
        "s1",
        "inline",
        json!([{"id": "ed", "k": "editor", "p": {"text": "hello world", "cursor": 0}}]),
    ));
    probe.write(&auk_conformance::probe::frame("s1", 2, json!([["focus", "ed"]])));
    acked(&probe, 2);
    caught_up(&env, &app, probe.pane);
    // The program granted `edit`, but the user's switch wins (§7.9): selecting keys are keys.
    app.request("type shift+right");
    let (bodies, bytes) = program_input(&probe, Duration::from_millis(400));
    assert!(bodies.is_empty(), "{bodies:?}");
    assert_eq!(bytes, b"\x1b[1;2C");
    // ⌘Z is the program's key too, not an `undo` event.
    app.request("type cmd+z");
    let (bodies, _) = program_input(&probe, Duration::from_millis(400));
    assert!(events(&bodies, "undo").is_empty(), "{bodies:?}");
    // A paste is input, not an edit.
    app.request("clipboard-set XY");
    menu(&app, "Edit", "Paste");
    let got = probe.read_until("the pasted keys", |b| b.ends_with(b"XY"));
    assert!(events(&tsp_bodies(&split_input(&got)), "edit").is_empty());
}

/// §4.3 "can be locked": Shell › Lock Session locks the shown session through the daemon; a
/// close is then refused with a toast in the window, and the same command unlocks it.
#[test]
fn lock_session_from_the_menu_refuses_closes_with_a_toast_and_unlocks() {
    let env = env("lock");
    let app = App::start(&env, "a", &[]);
    let pane = wait_for("a focused pane", || app.state()["focused"].as_u64());
    env.wait(pane, "prompt");
    let locked = |app: &App| app.layout()["sessions"][0]["locked"] == true;
    menu(&app, "Shell", "Lock Session");
    wait_for("the session to lock", || locked(&app).then_some(()));
    let closed = env.daemon(&format!("close {pane}"));
    assert_eq!(closed["closed"], false, "{closed}");
    wait_for("the window's toast", || {
        app.request("toasts")
            .as_array()?
            .iter()
            .any(|t| t["text"].as_str().is_some_and(|s| s.ends_with("is locked")))
            .then_some(())
    });
    assert!(env.pane_info(pane)["exit"].is_null(), "the pane stays");
    menu(&app, "Shell", "Lock Session");
    wait_for("the session to unlock", || (!locked(&app)).then_some(()));
}

/// §10.1 Presence: the window shows the other clients attached to its host, with their
/// identity, and stops when they leave.
#[test]
fn presence_shows_the_other_clients_attached_to_the_host() {
    let env = env("presence");
    let app = App::start(&env, "a", &[]);
    let pane = wait_for("a focused pane", || app.state()["focused"].as_u64());
    env.wait(pane, "prompt");
    let headless = env.start_headless("h", &[]);
    let shown = wait_for("the headless window in the app's presence", || {
        let p = app.request("presence");
        p["shown"].as_str().filter(|s| s.contains("headless")).map(str::to_owned)
    });
    assert!(shown.starts_with("Also here: "), "{shown}");
    let p = app.request("presence");
    let me = p["you"].clone();
    assert!(p["clients"].as_array().unwrap().iter().any(|c| c["id"] == me && c["kind"] == "app"), "{p}");
    std::process::Command::new("/bin/kill").arg(headless.pid.to_string()).status().expect("kill the headless window");
    wait_for("the headless window to leave", || app.request("presence")["shown"].is_null().then_some(()));
}

/// §4.2, §10.1 "Who runs what", §10.4 "Remote window": `auk --host` opens the app's windows on a
/// remote host's daemon (the Lima VM, over SSH). The remote daemon owns the panes; the window
/// halves run here and see them (a status formatter reads the remote pane's directory); plugin
/// CSS is the remote host's catalog styles plus the local manifests.
#[test]
fn remote_window_shows_remote_panes_to_local_window_halves_with_local_and_remote_css() {
    let r = auk_conformance::remote::RemoteEnv::new(env!("CARGO_BIN_EXE_auk"), "app-remote");
    // A plugin with manifest styles on the host, and one with a window half and styles here.
    let remote_plugin = format!("{}/state/plugins/rstyle", r.root);
    r.vm.ssh_ok(&format!(
        "mkdir -p {remote_plugin} && printf '%s\\n' 'schema = 1' 'id = \"rstyle\"' 'name = \"Remote Style\"' \
         'version = \"1\"' 'host = \"host.luau\"' 'styles = [\"r.css\"]' > {remote_plugin}/plugin.toml && \
         echo '-- nothing' > {remote_plugin}/host.luau && echo '.rstyle {{ color: red; }}' > {remote_plugin}/r.css"
    ));
    let local = r.local.state.join("plugins/localwin");
    std::fs::create_dir_all(&local).expect("mkdir");
    std::fs::write(
        local.join("plugin.toml"),
        "schema = 1\nid = \"localwin\"\nname = \"Local Window\"\nversion = \"1\"\nwindow = \"window.luau\"\nstyles = [\"l.css\"]\n",
    )
    .expect("write plugin.toml");
    std::fs::write(local.join("l.css"), ".localwin { color: blue; }").expect("write l.css");
    std::fs::write(
        local.join("window.luau"),
        r#"tern.chrome.status(function(p) return { { text = "at " .. p.cwd } } end)"#,
    )
    .expect("write window.luau");

    let pane = r.new_pane(&[]);
    let config = r.vm.ssh_config.to_string_lossy().into_owned();
    let command = r.remote_command();
    let app = App::start(
        &r.local,
        "app",
        &["--host", &r.vm.host, "--ssh-config", &config, "--remote-command", &command],
    );
    assert_eq!(app.state()["focused"], pane, "the window shows the remote pane");
    assert!(!auk_conformance::harness::ping(&r.local.control_path()), "no local daemon serves it");

    // The local window half sees the remote pane: its directory on the VM.
    let home = format!("{}/home", r.root);
    wait_for("the local formatter's segment for the remote pane", || {
        app.request("chrome")["status"]
            .as_array()?
            .iter()
            .any(|s| s["plugin"] == "localwin" && s["text"] == format!("at {home}"))
            .then_some(())
    });

    // Plugin CSS: the host's catalog styles under its name, and the local manifests.
    let catalog = wait_for("the remote catalog's styles", || {
        let c = app.request("catalog");
        (c["styles"].as_array()?.iter().any(|s| s == "rstyle")).then_some(c)
    });
    assert_eq!(catalog["host"], r.vm.host.as_str());
    let sheets: Vec<&str> = catalog["sheets"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert!(sheets.contains(&format!("plugin:{}:rstyle:styles", r.vm.host).as_str()), "{sheets:?}");
    assert!(sheets.contains(&"plugin:local:localwin:styles"), "{sheets:?}");
    wait_for("the sheets in the remote pane's document", || {
        let s = app.request(&format!("surface-sheets {pane}"));
        s.as_array()?.iter().any(|n| n.as_str().is_some_and(|n| n.ends_with(":rstyle:styles"))).then_some(())
    });

    // Typing in the window reaches the remote shell.
    app.request("text 'echo remote-$((6*7))'");
    app.request("type return");
    wait_for("the remote output in the window", || {
        app.request(&format!("capture {pane}"))["text"].as_str()?.contains("\nremote-42").then_some(())
    });
}

/// Writes `content` to `path` on the VM (making its directory), executable when `exec`.
fn vm_file(r: &auk_conformance::remote::RemoteEnv, path: &str, content: &str, exec: bool) {
    let dir = path.rsplit_once('/').map_or(".", |(d, _)| d);
    let chmod = if exec { format!(" && chmod +x {path}") } else { String::new() };
    let out = r.vm.ssh_with_input(&format!("mkdir -p {dir} && cat > {path}{chmod}"), content.as_bytes());
    assert!(out.status.success(), "write {path} on the VM: {}", String::from_utf8_lossy(&out.stderr));
}

/// A remote window's app, attached to `r`'s host over SSH.
fn remote_app(r: &auk_conformance::remote::RemoteEnv, name: &str) -> App {
    let config = r.vm.ssh_config.to_string_lossy().into_owned();
    let command = r.remote_command();
    App::start(&r.local, name, &["--host", &r.vm.host, "--ssh-config", &config, "--remote-command", &command])
}

/// The host half of `rdemo`, installed on the VM: a `title` hook that toasts, a lens on
/// `rdemo-cmd *` whose view has a clickable node (it copies and toasts), and a block type
/// `panel` whose node bumps a counter and toasts.
const RDEMO_HOST: &str = r#"
tern.on("title", function(ev, cx) cx:toast("info", "remote title " .. ev.title, "rdemo") end)
tern.lens.define("echo", {
    open = function(run) return { lines = 0 } end,
    line = function(s, l) s.lines += 1 end,
    finish = function(s, status) end,
    view = function(s)
        return { k = "col", c = {
            { k = "text", p = { text = "remote-lens " .. s.lines .. " lines" } },
            { id = "go", k = "text", p = { text = "Copy it", actions = { click = "copy-it" } } },
        } }
    end,
    event = function(s, ev, cx)
        if ev.ev == "action" and ev.act == "copy-it" then
            cx:copy("copied-from-the-remote-lens")
            cx:toast("success", "remote lens clicked", "rdemo")
        end
    end,
})
tern.block.define("panel", {
    init = function(cx, args, saved) return { n = 0 } end,
    view = function(s)
        return { main = { k = "col", c = {
            { k = "text", p = { key = "bump", text = "remote-panel " .. s.n, actions = { click = "bump" } } },
        } } }
    end,
    event = function(s, ev, cx)
        if ev.ev == "action" and ev.act == "bump" then
            s.n += 1
            cx:toast("info", "remote bumped " .. s.n)
            cx:render()
        end
    end,
})
"#;

/// A zsh program for the VM: it says hello, opens an inline surface whose node `go` has a click
/// action, reads its input until the action arrives, closes the surface (kept in scrollback) and
/// prints what it read.
const REMOTE_SURFACE_PROGRAM: &str = r#"#!/bin/zsh
stty raw -echo
printf '\033_tsp;q;{"q":"hello","v":[1],"app":"rprobe","ver":"1.0","features":[]}\033\\'
printf '\033_tsp;o;{"id":"r1","mode":"inline"}\033\\'
printf '\033_tsp;f;{"sf":"r1","s":1,"ops":[["add","main","r1",null,{"id":"main","k":"col","c":[{"id":"go","k":"text","p":{"text":"remote surface button","actions":{"click":"hit"}}}]}]]}\033\\'
buf=''
while read -r -u 0 -k 1 c; do
  buf+=$c
  [[ $buf == *'"ev":"action"'*'\'* ]] && break
done
printf '\033_tsp;x;{"id":"r1","keep":true}\033\\'
stty sane
[[ $buf == *'"ev":"ack"'* ]] && print -r -- "surface-acked-$((20+1))"
[[ $buf == *'"act":"hit"'* ]] && print -r -- "surface-clicked-$((40+2))"
"#;

/// §10.1 "Who runs what", §10.4 "Remote window": the remote daemon runs its own plugins' host
/// halves (a `title` hook, a lens and a block, all in a plugin installed only on the VM), and a
/// local remote window draws what they make, with a TSP surface of a remote program: each is
/// drawn, and a click on each goes back over the link to the code on the host. No local daemon
/// runs, so every effect seen here came from the host.
#[test]
fn remote_window_draws_the_remote_hosts_surfaces_blocks_and_lens_views_from_its_host_halves() {
    let r = auk_conformance::remote::RemoteEnv::new(env!("CARGO_BIN_EXE_auk"), "app-remote-draw");
    let plugin = format!("{}/state/plugins/rdemo", r.root);
    vm_file(
        &r,
        &format!("{plugin}/plugin.toml"),
        "schema = 1\nid = \"rdemo\"\nname = \"Remote demo\"\nversion = \"1\"\nhost = \"host.luau\"\n\
         [[lenses]]\nid = \"echo\"\nmatch = [\"rdemo-cmd *\"]\n[[blocks]]\nid = \"panel\"\ntitle = \"Panel\"\n",
        false,
    );
    vm_file(&r, &format!("{plugin}/host.luau"), RDEMO_HOST, false);
    let cmd = format!("{}/bin/rdemo-cmd", r.root);
    vm_file(&r, &cmd, "#!/bin/sh\nprintf 'one\\ntwo\\nthree\\n'\n", true);
    let surface = format!("{}/bin/rsurface", r.root);
    vm_file(&r, &surface, REMOTE_SURFACE_PROGRAM, true);

    let pane = r.new_pane(&[]);
    let app = remote_app(&r, "app");
    assert_eq!(app.state()["focused"], pane, "the window shows the remote pane");
    assert!(!auk_conformance::harness::ping(&r.local.control_path()), "no local daemon serves it");
    wait_for("the remote catalog's block type", || {
        let c = app.request("catalog");
        c["block_types"].as_array()?.iter().any(|b| b["kind"] == "rdemo.panel").then_some(())
    });

    // A host hook: the remote daemon's `title` hook toasts in the local window.
    r.auk_ok(&["run", &pane.to_string(), "printf '\\e]2;%s\\a' hooked-$((40+2))"]);
    wait_for("the remote title hook's toast", || {
        app.request("toasts").as_array()?.iter().any(|t| t["text"] == "remote title hooked-42").then_some(())
    });
    r.wait(pane, "prompt");

    // A TSP surface from a remote program: drawn here (the window acks it), and a click on its
    // node reaches the program on the VM.
    r.auk_ok(&["run", &pane.to_string(), &surface]);
    let go = wait_for("the remote surface's node, drawn", || node_box(&app, pane, "go"));
    click(&app, center(go));
    r.wait(pane, "prompt");
    let text = r.capture(pane);
    assert!(text.contains("\nsurface-acked-21"), "the window's ack didn't reach the program: {text}");
    assert!(text.contains("\nsurface-clicked-42"), "the click didn't reach the program: {text}");

    // A lens: the remote host half claims the command, its view draws here, and a click in the
    // view is a lens event the host answers with effects.
    r.auk_ok(&["run", &pane.to_string(), &format!("{cmd} go")]);
    let block = wait_for("the remote lens view, drawn", || {
        let b = app.request(&format!("lens {pane}"))["blocks"][0].clone();
        (b["plugin"] == "rdemo" && b["shown"] == "native" && b["text"].as_str()?.contains("remote-lens 3 lines"))
            .then_some(b)
    });
    let go = block["parts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["text"].as_str().is_some_and(|t| t.contains("Copy it")))
        .cloned()
        .unwrap_or_else(|| panic!("the view's node isn't drawn: {block}"));
    let rect = &go["rect"];
    let n = |i: usize| rect[i].as_f64().unwrap();
    let at = (n(0) + n(2) / 2.0, n(1) + n(3) / 2.0);
    app.request(&format!("mouse move {} {}", at.0, at.1));
    click(&app, at);
    wait_for("the remote lens's copy", || {
        (app.request("clipboard") == "copied-from-the-remote-lens").then_some(())
    });
    wait_for("the remote lens's toast", || {
        app.request("toasts").as_array()?.iter().any(|t| t["text"] == "remote lens clicked").then_some(())
    });

    // A plugin block the host opens: its view draws here, and a click reaches the block.
    let block: u64 = r.auk_ok(&["block", "rdemo.panel"]).parse().expect("a block pane id");
    show(&app, block);
    // Block views get derived ids: the region, then each node's key (tern-sdk reconcile).
    let bump = wait_for("the remote block's view, drawn", || node_box(&app, block, "main.bump"));
    click(&app, center(bump));
    wait_for("the remote block's toast", || {
        app.request("toasts").as_array()?.iter().any(|t| t["text"] == "remote bumped 1").then_some(())
    });
    wait_for("the block's re-render on the host", || {
        r.auk_ok(&["capture", &block.to_string(), "--surfaces"]).contains("remote-panel 1").then_some(())
    });
}

/// Runs the palette row matching `query`, which must be `action`, as a person would: open it,
/// type, Return.
fn palette_run(app: &App, query: &str, action: &str) {
    app.request("menu View 'Command Palette…'");
    wait_for("the palette", || (app.request("palette")["open"] == true).then_some(()));
    app.request(&format!("text {}", auk_conformance::harness::quote(query)));
    let rows = wait_for("the palette's rows", || {
        let p = app.request("palette");
        (p["query"] == query).then(|| p["rows"].clone())
    });
    assert_eq!(rows[0]["action"], action, "{rows}");
    app.request("type return");
    wait_for("the palette to close", || (app.request("palette")["open"] == false).then_some(()));
}

/// §9.10 "Whose rights": a window half, running here, types into a remote pane with `cx:run`, and
/// the remote shell runs it. §10.4 "Remote window", blocks: the window opens a block type of its
/// host's catalog from the palette; the remote daemon starts it, and the window focuses and draws
/// it.
#[test]
fn remote_window_half_types_into_a_remote_pane_and_opens_the_hosts_block_types() {
    let r = auk_conformance::remote::RemoteEnv::new(env!("CARGO_BIN_EXE_auk"), "app-remote-half");
    let plugin = format!("{}/state/plugins/rpanel", r.root);
    vm_file(
        &r,
        &format!("{plugin}/plugin.toml"),
        "schema = 1\nid = \"rpanel\"\nname = \"Remote panel\"\nversion = \"1\"\nhost = \"host.luau\"\n\
         [[blocks]]\nid = \"panel\"\ntitle = \"Remote Panel\"\n",
        false,
    );
    vm_file(
        &r,
        &format!("{plugin}/host.luau"),
        r#"tern.block.define("panel", {
            init = function(cx, args, saved) return {} end,
            view = function(s) return { main = { k = "col", c = {
                { k = "text", p = { key = "hello", text = "remote panel opened from the window" } },
            } } } end,
        })"#,
        false,
    );
    let local = r.local.state.join("plugins/typer");
    std::fs::create_dir_all(&local).expect("mkdir");
    std::fs::write(
        local.join("plugin.toml"),
        "schema = 1\nid = \"typer\"\nname = \"Typer\"\nversion = \"1\"\nwindow = \"window.luau\"\n",
    )
    .expect("write plugin.toml");
    std::fs::write(
        local.join("window.luau"),
        r#"tern.command({
            id = "type-remote",
            title = "Type into the focused pane",
            run = function(cx)
                cx:run(cx.session:focused(), "echo typed-by-the-window-half-$((6*7))\r")
            end,
        })
        tern.command({
            id = "open-remote",
            title = "Open panes on the host",
            run = function(cx)
                local f = cx.session:focused()
                local b = cx:new_block("rpanel.panel", {"x"}, "below", {focus = false})
                local s = cx.layout:split(f, "up", {command = "cat"}, {focus = false})
                local t, err = cx.layout:tab({
                    split = "left", ratio = 0.4, {}, {launch = {block = "rpanel.panel"}},
                }, {focus = false})
                local tree = cx.session:layout(t).root
                cx:copy(string.format("f=%d block=%d split=%d first=%d second=%d err=%s",
                    f, b, s, tree[2].pane, tree[1].pane, tostring(err)))
            end,
        })"#,
    )
    .expect("write window.luau");

    let pane = r.new_pane(&[]);
    let app = remote_app(&r, "app");
    assert_eq!(app.state()["focused"], pane, "the window shows the remote pane");
    assert!(!auk_conformance::harness::ping(&r.local.control_path()), "no local daemon serves it");

    // The window half's `cx:run` reaches the remote shell, which runs the line on the VM.
    palette_run(&app, "type into the focused", "plugin.typer.type-remote");
    r.wait(pane, "prompt");
    wait_for("the remote shell's output", || {
        r.capture(pane).contains("\ntyped-by-the-window-half-42").then_some(())
    });
    wait_for("the output in the window", || {
        app.request(&format!("capture {pane}"))["text"].as_str()?.contains("\ntyped-by-the-window-half-42").then_some(())
    });

    // The host's block type is in the palette; the remote daemon opens it, and the window
    // focuses and draws it.
    palette_run(&app, "new remote panel", "block:rpanel.panel");
    let block = wait_for("the block pane on the host", || {
        r.ls()["panes"].as_array()?.iter().find_map(|p| {
            (p["kind"]["block"] == "rpanel.panel").then(|| p["id"].as_u64()).flatten()
        })
    });
    wait_for("the window to focus the block", || (app.state()["focused"] == block).then_some(()));
    wait_for("the block's view, drawn", || node_box(&app, block, "main.hello"));

    // §9.8 on a remote window: `cx:new_block`, `layout:split` with a launch and `layout:tab` ask
    // the window's own host over its link and answer with the new panes' ids, placed on the
    // sides and at the ratio asked.
    app.request(&format!("focus {pane}"));
    palette_run(&app, "open panes on the host", "plugin.typer.open-remote");
    let report = wait_for("the open's report", || {
        let c = app.request("clipboard");
        c.as_str()?.starts_with("f=").then(|| c.as_str().unwrap().to_owned())
    });
    assert!(report.ends_with("err=nil"), "{report}");
    let ids: std::collections::BTreeMap<&str, u64> = report
        .split(' ')
        .filter_map(|kv| kv.split_once('='))
        .filter_map(|(k, v)| Some((k, v.parse().ok()?)))
        .collect();
    let (f, opened, split, first, second) = (ids["f"], ids["block"], ids["split"], ids["first"], ids["second"]);
    assert_eq!(f, pane);
    assert_eq!(r.pane_info(opened)["kind"]["block"], "rpanel.panel");
    assert_eq!(r.pane_info(split)["kind"]["line"], "cat");
    assert_eq!(r.pane_info(second)["kind"]["block"], "rpanel.panel");
    assert!(r.pane_info(first)["kind"]["block"].is_null());
    let host = r.ls()["layout"]["main"].clone();
    use auk_conformance::harness::{layout_shape, tab_holding};
    assert_eq!(
        layout_shape(&tab_holding(&host, pane)["root"], true),
        format!("D500(D500({split},{pane}),{opened})"),
        "{host}"
    );
    assert_eq!(layout_shape(&tab_holding(&host, first)["root"], true), format!("R600({second},{first})"));
    assert_eq!(tab_holding(&app.layout(), first), tab_holding(&host, first), "the window shows the host's tab");
}

/// §4.1: the first window starts the daemon when none answers, and the daemon outlives it.
#[test]
fn the_first_window_starts_a_daemon_when_none_answers_and_it_outlives_the_window() {
    let env = TestEnv::new(env!("CARGO_BIN_EXE_auk"), "autostart");
    assert!(!auk_conformance::harness::ping(&env.control_path()), "a daemon already answers");
    let app = App::start(&env, "a", &[]);
    // A daemon answers on the test's socket now: a process of its own, with the window's first
    // pane.
    let state = env.daemon("state");
    let daemon = state["pid"].as_u64().expect("the daemon's pid") as u32;
    assert_ne!(daemon, app.pid, "the daemon runs inside the app");
    assert!(alive(daemon));
    let pane = app.state()["focused"].as_u64().expect("a focused pane");
    let ls = env.daemon("ls");
    assert!(
        ls["panes"].as_array().is_some_and(|p| p.iter().any(|p| p["id"] == pane)),
        "{ls}"
    );
    env.wait(pane, "prompt");
    // It outlives the window, with the pane.
    app.quit();
    assert!(alive(daemon), "the daemon ended with the window");
    assert_eq!(env.daemon("state")["pid"], state["pid"]);
    assert!(env.pane_info(pane)["exit"].is_null());
}

/// The status line's link segments as the window drew them: `(identity, state, state tone)`.
fn link_shown(app: &App) -> (Option<String>, Option<String>, Option<String>) {
    let link = app.request("link");
    let seg = |plugin: &str| {
        link["shown"]
            .as_array()
            .and_then(|s| s.iter().find(|s| s["plugin"] == plugin).cloned())
    };
    let identity = seg("auk.identity").and_then(|s| s["text"].as_str().map(str::to_owned));
    let state = seg("auk.link");
    (
        identity,
        state.as_ref().and_then(|s| s["text"].as_str().map(str::to_owned)),
        state.and_then(|s| s["tone"].as_str().map(str::to_owned)),
    )
}

/// Waits until the status line shows the link in `state`; returns the link's status.
fn wait_link(app: &App, state: &str, tone: &str) -> Value {
    wait_for(&format!("the status line to show the link {state}"), || {
        let (_, shown, t) = link_shown(app);
        (shown.as_deref() == Some(state) && t.as_deref() == Some(tone)).then(|| app.request("link")["status"].clone())
    })
}

/// Any string in an accessibility tree node that contains `needle`.
fn a11y_has(tree: &Value, needle: &str) -> bool {
    flatten(tree).iter().any(|n| {
        n.as_object()
            .is_some_and(|o| o.values().any(|v| v.as_str().is_some_and(|s| s.contains(needle))))
    })
}

/// §10.1 "Identity and keys", "Connection behavior": a local window's status line shows its
/// host's identity chain, the local hop ("dai@mac via local from mac as dai@mac"), and the link's
/// state, connected; both are in the accessibility tree too.
#[test]
fn a_local_windows_status_line_shows_its_local_hop_and_the_link_state() {
    let env = env("link-local");
    let app = App::start(&env, "a", &[]);
    let status = wait_link(&app, "connected", "muted");
    let hop = &status["identity"][0];
    assert_eq!(hop["method"], "local", "{status}");
    assert_eq!(status["identity"].as_array().unwrap().len(), 1, "{status}");
    let user = std::env::var("USER").expect("USER");
    let acting = hop["acting"].as_str().unwrap();
    let origin = hop["origin"].as_str().unwrap();
    assert_eq!(acting, format!("{user}@{origin}"), "{status}");
    let chain = format!("{} via local from {origin} as {acting}", hop["principal"].as_str().unwrap());
    let (identity, ..) = link_shown(&app);
    assert_eq!(identity.as_deref(), Some(chain.as_str()));
    let tree = app.request("a11y --depth 12");
    assert!(a11y_has(&tree, &chain), "the chain isn't in the accessibility tree: {tree}");
    assert!(a11y_has(&tree, "connected"), "{tree}");
}

/// §10.1 "Identity and keys", "Connection behavior" on a remote window (the Lima VM): the
/// status line shows the SSH key hop ("<key> via ssh-key from <this mac> as <user>@<vm>") and
/// the link's state. When the ssh process dies the window shows "reconnecting (attempt 1)" and
/// holds what is typed; once the link resyncs it shows "connected" again and the typing reaches
/// the remote shell once. A link that gives up shows "failed" and why, and the window stays.
#[test]
fn remote_window_shows_its_ssh_hop_and_the_link_reconnecting_then_connected_and_typing_arrives_once() {
    let r = auk_conformance::remote::RemoteEnv::new(env!("CARGO_BIN_EXE_auk"), "app-remote-link");
    let pane = r.new_pane(&[]);
    let app = remote_app(&r, "app");
    assert_eq!(app.state()["focused"], pane, "the window shows the remote pane");
    r.wait(pane, "prompt");

    let before = wait_link(&app, "connected", "muted");
    let hop = &before["identity"][0];
    assert_eq!(hop["method"], "ssh-key", "{before}");
    let vm_user = r.vm.ssh_ok("id -un").trim().to_owned();
    let vm_host = r.vm.ssh_ok("hostname").trim().to_owned();
    assert_eq!(hop["acting"], format!("{vm_user}@{vm_host}"), "{before}");
    let here = std::process::Command::new("/bin/hostname").output().expect("hostname").stdout;
    let here = String::from_utf8_lossy(&here).trim().to_owned();
    let here_short = here.split('.').next().unwrap_or(&here);
    let origin = hop["origin"].as_str().unwrap();
    assert!(origin == here || origin == here_short, "origin {origin} isn't this machine ({here}): {before}");
    let chain = format!(
        "{} via ssh-key from {origin} as {vm_user}@{vm_host}",
        hop["principal"].as_str().unwrap()
    );
    assert_eq!(link_shown(&app).0.as_deref(), Some(chain.as_str()));
    let ssh = before["ssh_pid"].as_u64().expect("the ssh pid");

    // Drop the link, and hold the next connection at the gate so the outage lasts.
    r.vm.ssh_ok(&format!("touch {}", r.gate()));
    let killed = std::process::Command::new("/bin/kill").args(["-KILL", &ssh.to_string()]).status();
    assert!(killed.is_ok_and(|s| s.success()), "kill the ssh process");
    let down = wait_link(&app, "reconnecting (attempt 1)", "warning");
    assert_eq!(down["reconnects"], 0, "{down}");

    // Typing in the window waits for the resync.
    app.request("text 'echo typed-while-down-$((6*7))'");
    app.request("type return");
    let held = wait_for("the typing held", || {
        let st = app.request("link")["status"].clone();
        (st["held"].as_u64()? >= 2).then_some(st)
    });
    assert_eq!(held["label"], "reconnecting(1)", "{held}");
    assert!(!r.capture(pane).contains("typed-while-down"));

    // Open the gate: the link reconnects and resyncs, the status line says connected, and the
    // typing reaches the remote shell, once.
    r.vm.ssh_ok(&format!("rm {}", r.gate()));
    let after = wait_link(&app, "connected", "muted");
    assert_eq!(after["reconnects"], 1, "{after}");
    assert_ne!(after["ssh_pid"].as_u64(), Some(ssh));
    let history: Vec<&str> = after["history"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    let at = history.iter().position(|s| *s == "reconnecting(1)").unwrap_or_else(|| panic!("{history:?}"));
    assert_eq!(history.last(), Some(&"connected"), "{history:?}");
    assert!(at < history.len() - 1, "{history:?}");
    wait_for("the typing in the window", || {
        app.request(&format!("capture {pane}"))["text"].as_str()?.contains("\ntyped-while-down-42").then_some(())
    });
    r.wait(pane, "prompt");
    let remote = r.capture(pane);
    assert_eq!(remote.matches("\ntyped-while-down-42").count(), 1, "{remote}");
    assert_eq!(remote.matches("echo typed-while-down-$((6*7))").count(), 1, "{remote}");
    assert_eq!(app.request("link")["status"]["held"], 0);
    assert_eq!(link_shown(&app).0.as_deref(), Some(chain.as_str()), "the chain after the reconnect");

    // A link that gives up: the host's `auk` is gone when it reconnects. The window stays and
    // says why.
    let ssh = after["ssh_pid"].as_u64().expect("the ssh pid");
    r.vm.ssh_ok(&format!("touch {}", r.gate()));
    let killed = std::process::Command::new("/bin/kill").args(["-KILL", &ssh.to_string()]).status();
    assert!(killed.is_ok_and(|s| s.success()), "kill the ssh process");
    wait_link(&app, "reconnecting (attempt 1)", "warning");
    r.vm.ssh_ok(&format!("mv {0} {0}.gone && rm {1}", r.auk, r.gate()));
    let failed = wait_for("the status line to show the link failed", || {
        let (_, shown, tone) = link_shown(&app);
        (shown.as_deref()?.starts_with("failed: ") && tone.as_deref() == Some("error")).then_some(shown.unwrap())
    });
    assert!(failed.contains("wasn't found"), "{failed}");
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(app.state()["focused"], pane, "the window stays open");
    assert_eq!(link_shown(&app).1.as_deref(), Some(failed.as_str()));
}

/// §9.5 "Keys and events": in a focused block pane, the window keymap takes the keys it binds
/// first: a bound chord runs its action (the palette) and never reaches the block's `key`, while
/// an unbound key does, as `{name, ctrl, …}`.
#[test]
fn the_window_keymap_takes_its_keys_before_a_focused_block() {
    let env = TestEnv::new(env!("CARGO_BIN_EXE_auk"), "block-keys");
    let plugin = env.state.join("plugins/keylog");
    std::fs::create_dir_all(&plugin).expect("mkdir");
    std::fs::write(
        plugin.join("plugin.toml"),
        "schema = 1\nid = \"keylog\"\nname = \"Key log\"\nversion = \"1\"\nhost = \"host.luau\"\n\
         [[blocks]]\nid = \"log\"\ntitle = \"Key log\"\n",
    )
    .expect("write plugin.toml");
    std::fs::write(
        plugin.join("host.luau"),
        r#"tern.block.define("log", {
            init = function(cx, args, saved) return { keys = "" } end,
            view = function(s)
                return { main = { k = "col", c = { { k = "text", p = { key = "log", text = "keys:" .. s.keys } } } } }
            end,
            key = function(s, key)
                s.keys ..= " " .. (key.ctrl and "ctrl+" or "") .. key.name
                return true
            end,
        })"#,
    )
    .expect("write host.luau");
    write_settings(&env, json!({"keybinds": {"ctrl+k": "palette"}}));
    env.start_daemon();
    let app = App::start(&env, "a", &[]);
    let _us = InputSource::select(&app, US_LAYOUT);
    let first = wait_for("a focused pane", || app.state()["focused"].as_u64());
    env.wait(first, "prompt");
    let block = env.daemon("block keylog.log")["pane"].as_u64().expect("a block pane");
    show(&app, block);
    wait_for("the block's view, drawn", || node_box(&app, block, "main.log"));
    let shown = || -> String {
        let out = env.cli(&["capture", &block.to_string(), "--surfaces"]);
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    };

    // An unbound key reaches the block.
    app.request("type x");
    wait_for("the unbound key in the block", || shown().contains("keys: x").then_some(()));

    // The bound chord runs its action and never reaches the block.
    app.request("type ctrl+k");
    wait_for("the palette", || (app.request("palette")["open"] == true).then_some(()));
    app.request("type escape");
    wait_for("the palette to close", || (app.request("palette")["open"] == false).then_some(()));
    assert_eq!(app.state()["focused"], block);

    // A later unbound chord with ctrl still reaches it, after the bound one: nothing came between.
    app.request("type ctrl+e");
    let text = wait_for("the next key in the block", || {
        let t = shown();
        t.contains("ctrl+e").then_some(t)
    });
    assert!(text.contains("keys: x ctrl+e"), "the bound chord reached the block: {text}");
    assert!(!text.contains("ctrl+k") && !text.contains(" k"), "{text}");
}
