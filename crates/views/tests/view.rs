//! The pane view as a whole: regions and units, palettes and program sheets, the motion clock,
//! the measure-then-layout loops, virtualization, and the accessibility tree.

mod common;

use auk_tsp::Mods;
use auk_views::{PointerEvent, Region, ViewSettings};
use common::Harness;
use serde_json::json;

fn key(h: &Harness, sf: &str) -> auk_tsp::SurfaceKey {
    h.pane
        .surfaces()
        .iter()
        .rev()
        .find(|s| s.id() == sf)
        .unwrap()
        .key()
}

#[test]
fn regions_carry_scope_role_app_and_units() {
    let settings = ViewSettings {
        font_px: 26.0,
        cell: (16.0, 34.0),
        ..Default::default()
    };
    let mut h = Harness::with(auk_tsp::Config::default(), settings);
    h.hello(&[]);
    h.apc("o", r#"{"id":"s1","mode":"inline","role":"demo.surface"}"#);
    h.frame_ops(
        "s1",
        1,
        json!([["add", "main", "s1", null, {"id": "main", "k": "col", "p": {"role": "demo.main"}, "c": [
            {"id": "t", "k": "text", "p": {"text": "x", "max": {"w": "40ch"}}}
        ]}]]),
    );
    h.frame();
    let main = h.view.region(key(&h, "s1"), Region::Main).unwrap();
    assert_eq!(
        h.attr(main, "data-surface").as_deref(),
        Some("demo.surface")
    );
    assert_eq!(h.attr(main, "data-role").as_deref(), Some("demo.main"));
    assert_eq!(h.attr(main, "data-app").as_deref(), Some("test"));
    assert_eq!(
        h.view.written().style(main),
        Some("zoom: 2; --sf-zoom: 2; --sf-cw: 8px; --sf-lh: 17px;")
    );
    // Region zoom font/13: a 40ch extent is 40 cells of the real font.
    let t = h.el("s1", "t");
    let w = h.doc.border_box(t).unwrap().w;
    assert!((w - 640.0).abs() < 0.5, "{w}");
}

#[test]
fn palettes_become_variables_with_the_setting() {
    let mut h = Harness::new();
    h.hello(&[]);
    h.apc("o", r#"{"id":"s1"}"#);
    h.apc("t", r##"{"dark":{"accent":"#00b4ff","statusLineModel":"#ff00aa","toolSuccessBg":"#0f2a1c"},"light":{"accent":"#004488"}}"##);
    h.frame_ops(
        "s1",
        1,
        json!([["add", "main", "s1", null, {"id": "main", "k": "col", "c": [
            {"id": "t", "k": "text", "p": {"spans": [{"t": "model", "s": "statusLineModel"}]}}
        ]}]]),
    );
    h.frame();
    let main = h.view.region(key(&h, "s1"), Region::Main).unwrap();
    assert!(h.class(main).contains("sf-pal"));
    let style = h.view.written().style(main).unwrap().to_owned();
    assert!(style.contains("--sf-p-accent: #00b4ff;"), "{style}");
    assert!(style.contains("--sf-accent: #00b4ff;"), "{style}");
    assert!(
        style.contains("--sf-tint-success: rgb(15 42 28 / 60%);"),
        "{style}"
    );
    // The span colors from the palette token.
    let span = h.doc.element_children(h.el("s1", "t"))[0];
    let c = h.doc.computed_style(span).unwrap().color.to_rgba8();
    assert_eq!(c, [0xff, 0x00, 0xaa, 0xff]);
    // "Use program colors" off: the theme's colors, the palette kept.
    let s = ViewSettings {
        program_colors: false,
        ..Default::default()
    };
    h.view.set_settings(&mut h.doc, &h.pane, s);
    h.frame();
    assert!(!h.class(main).contains("sf-pal"));
    let c2 = h.doc.computed_style(span).unwrap().color.to_rgba8();
    assert_ne!(c2, c);
}

#[test]
fn program_sheets_are_scoped_and_report_terns_errors() {
    let mut h = Harness::new();
    h.hello(&[]);
    h.surface(
        "s1",
        "inline",
        json!([{"id": "a", "k": "el", "p": {"tag": "div", "class": "x"}}]),
    );
    h.surface(
        "s2",
        "flow",
        json!([{"id": "b", "k": "el", "p": {"tag": "div", "class": "x"}}]),
    );
    h.apc("s", r#"{"sf":"s1","name":"main","css":".x { width: 120px } .y { background: url(x.png); width: 2rem }"}"#);
    h.frame();
    let errs = h.errors();
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert_eq!(errs[0]["sheet"], "main");
    let msg = errs[0]["msg"].as_str().unwrap();
    assert!(
        msg.split("; ")
            .all(|m| m.split(':').next().unwrap().parse::<u32>().is_ok()),
        "{msg}"
    );
    // Only s1's element takes the rule.
    assert_eq!(h.doc.border_box(h.el("s1", "a")).unwrap().w, 120.0);
    assert_ne!(h.doc.border_box(h.el("s2", "b")).unwrap().w, 120.0);
    // Replacing keeps one sheet; removing drops it.
    h.apc(
        "s",
        r#"{"sf":"s1","name":"main","css":".x { width: 80px }"}"#,
    );
    h.frame();
    assert_eq!(h.doc.border_box(h.el("s1", "a")).unwrap().w, 80.0);
    h.apc("s", r#"{"sf":"s1","name":"main"}"#);
    h.frame();
    assert_ne!(h.doc.border_box(h.el("s1", "a")).unwrap().w, 80.0);
}

#[test]
fn screen_surfaces_cover_and_hide_inline() {
    let mut h = Harness::new();
    h.hello(&[]);
    h.surface(
        "in",
        "inline",
        json!([{"id": "t", "k": "text", "p": {"text": "inline"}}]),
    );
    h.surface(
        "sc",
        "screen",
        json!([{"id": "u", "k": "text", "p": {"text": "screen"}}]),
    );
    h.frame();
    let inline_main = h.view.region(key(&h, "in"), Region::Main).unwrap();
    assert!(h.class(inline_main).contains("sf-covered"));
    let screen_main = h.view.region(key(&h, "sc"), Region::Main).unwrap();
    assert!(h.class(screen_main).contains("sf-screen"));
    let cover = h.doc.parent(screen_main).unwrap();
    assert_eq!(h.class(cover), "auk-screen");
}

#[test]
fn motion_is_clocked_by_the_view() {
    let mut h = Harness::new();
    h.hello(&[]);
    h.surface(
        "s1",
        "inline",
        json!([
            {"id": "sp", "k": "spinner", "p": {"style": "braille"}},
            {"id": "el", "k": "elapsed", "p": {"age": 1500}},
            {"id": "cd", "k": "elapsed", "p": {"age": -3000, "format": "clock"}},
            {"id": "rt", "k": "rate", "p": {"value": 10, "unit": "tok/s"}},
            {"id": "pg", "k": "progress", "p": {"value": 0.2}},
            {"id": "sh", "k": "shimmer", "p": {"text": "Working"}},
            {"id": "md", "k": "md", "p": {"text": "streaming", "stream": true}}
        ]),
    );
    h.frame();
    let t0 = h.now;
    let glyph0 = h.text(h.el("s1", "sp"));
    assert_eq!(h.text(h.el("s1", "el")), "1.5s", "start = receive - age");
    assert_eq!(h.text(h.el("s1", "cd")), "0:03");
    // Time passes: the spinner turns, timers tick, the countdown stops at zero.
    let next = h.view.tick(&mut h.doc, &h.pane, t0 + 350.0);
    assert!(next.is_some(), "something moves: frames requested");
    assert_ne!(h.text(h.el("s1", "sp")), glyph0);
    assert_eq!(h.text(h.el("s1", "el")), "1.8s");
    h.view.tick(&mut h.doc, &h.pane, t0 + 5000.0);
    assert_eq!(h.text(h.el("s1", "cd")), "0:00");
    assert_eq!(h.text(h.el("s1", "el")), "6.5s");
    // A set of other props leaves the timer running; a new age re-anchors.
    h.now = t0 + 5000.0;
    h.frame_ops("s1", 2, json!([["set", "el", {"format": "short"}]]));
    assert_eq!(h.text(h.el("s1", "el")), "6.5s");
    h.frame_ops("s1", 3, json!([["set", "el", {"age": 100}]]));
    assert_eq!(h.text(h.el("s1", "el")), "0.1s");
    // rate eases to a new value over 420 ms.
    h.frame_ops("s1", 4, json!([["set", "rt", {"value": 20}]]));
    h.view.tick(&mut h.doc, &h.pane, h.now + 100.0);
    let mid: f64 = h
        .text(h.el("s1", "rt"))
        .split(' ')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert!(mid > 10.0 && mid < 20.0, "{mid}");
    h.view.tick(&mut h.doc, &h.pane, h.now + 500.0);
    assert_eq!(h.text(h.el("s1", "rt")), "20.0 tok/s");
    // progress transitions to its value.
    h.frame_ops("s1", 5, json!([["set", "pg", {"value": 0.8}]]));
    let fill = h.find_class(h.el("s1", "pg"), "fill")[0];
    h.view.tick(&mut h.doc, &h.pane, h.now + 100.0);
    let mid = h.view.written().style(fill).unwrap().to_owned();
    h.view.tick(&mut h.doc, &h.pane, h.now + 400.0);
    assert_eq!(h.view.written().style(fill), Some("width: 80%"));
    assert_ne!(mid, "width: 80%");
}

#[test]
fn reduce_motion_gives_still_equivalents() {
    let s = ViewSettings {
        reduce_motion: true,
        ..Default::default()
    };
    let mut h = Harness::with(auk_tsp::Config::default(), s);
    h.hello(&[]);
    h.surface(
        "s1",
        "inline",
        json!([
            {"id": "sp", "k": "spinner", "p": {"style": "braille", "label": "Wait"}},
            {"id": "pg", "k": "progress", "p": {}},
            {"id": "el", "k": "elapsed", "p": {"age": 0}}
        ]),
    );
    h.frame();
    let main = h.view.region(key(&h, "s1"), Region::Main).unwrap();
    assert!(h.class(main).contains("sf-still"));
    let g = h.text(h.el("s1", "sp"));
    h.view.tick(&mut h.doc, &h.pane, h.now + 777.0);
    assert_eq!(h.text(h.el("s1", "sp")), g, "a still spinner");
    let fill = h.find_class(h.el("s1", "pg"), "fill")[0];
    assert_eq!(
        h.view.written().style(fill),
        Some("left: 0%; width: 100%; opacity: 0.35")
    );
    // Timers still tick.
    assert_eq!(h.text(h.el("s1", "el")), "0.7s");
}

#[test]
fn hidden_panes_ask_for_no_frames() {
    let mut h = Harness::new();
    h.hello(&[]);
    h.surface(
        "s1",
        "inline",
        json!([{"id": "sp", "k": "spinner", "p": {}}]),
    );
    h.frame();
    assert!(h.view.tick(&mut h.doc, &h.pane, h.now + 16.0).is_some());
    h.view.set_visible(&mut h.doc, &h.pane, false);
    assert!(h.view.tick(&mut h.doc, &h.pane, h.now + 32.0).is_none());
    let main = h.view.region(key(&h, "s1"), Region::Main).unwrap();
    assert!(h.class(main).contains("sf-paused"));
    // Nothing moving: no frames.
    let mut h = Harness::new();
    h.hello(&[]);
    h.surface(
        "s1",
        "inline",
        json!([{"id": "t", "k": "text", "p": {"text": "still"}}]),
    );
    h.frame();
    assert!(h.view.tick(&mut h.doc, &h.pane, h.now + 16.0).is_none());
}

#[test]
fn card_preview_clamps_with_more_lines() {
    let mut h = Harness::new();
    h.hello(&[]);
    let log: String = (1..=8).map(|i| format!("step {i}\n")).collect();
    h.surface(
        "s1",
        "inline",
        json!([{"id": "c", "k": "card", "p": {"head": "Build", "collapsible": true, "collapsed": true, "preview": {"lines": 2}}, "c": [
            {"id": "c.a", "k": "ansi", "p": {"text": log.trim_end()}}
        ]}]),
    );
    h.frame();
    let card = h.el("s1", "c");
    assert!(h.class(card).contains("clamped"), "{}", h.class(card));
    let more = h.find_class(card, "sf-more")[0];
    assert_eq!(h.text(more), "6 more lines");
    let body = h.find_class(card, "sf-card-body")[0];
    assert!((h.doc.border_box(body).unwrap().h - 34.0).abs() < 0.5);
    // "Show all": the same local flip as the head.
    let r = h.doc.border_box(more).unwrap();
    h.view.click(
        &mut h.doc,
        &h.pane,
        PointerEvent {
            x: r.x + 2.0,
            y: r.y + 2.0,
            clicks: 1,
            mods: Mods::NONE,
            ..Default::default()
        },
    );
    h.frame();
    assert!(!h.class(card).contains("collapsed"));
}

#[test]
fn status_drops_low_priority_segments_to_fit() {
    let mut h = Harness::new();
    h.hello(&[]);
    h.surface(
        "s1",
        "inline",
        json!([{"id": "bar", "k": "status", "p": {"max": {"w": "30ch"}}, "c": [
            {"id": "a", "k": "seg", "p": {"text": "aaaaaaaaaa", "priority": 5}},
            {"id": "b", "k": "seg", "p": {"text": "bbbbbbbbbb", "priority": 1}},
            {"id": "c", "k": "seg", "p": {"text": "cccccccccc", "priority": 3, "side": "right"}}
        ]}]),
    );
    h.frame();
    assert!(
        h.class(h.el("s1", "b")).contains("sf-fit-off"),
        "lowest priority drops"
    );
    assert!(!h.class(h.el("s1", "a")).contains("sf-fit-off"));
    assert!(!h.class(h.el("s1", "c")).contains("sf-fit-off"));
}

#[test]
fn tables_fit_columns_by_priority() {
    let mut h = Harness::new();
    h.hello(&[]);
    h.surface(
        "s1",
        "inline",
        json!([{"id": "t", "k": "table", "p": {"max": {"w": "30ch"}, "cols": [
            {"id": "a", "head": "Name", "priority": 3},
            {"id": "b", "head": "Description", "priority": 0},
            {"id": "c", "head": "Size", "priority": 2}
        ], "rows": [{"id": "r", "cells": {"a": "cargo.toml", "b": "the workspace manifest file", "c": "2.1K"}}]}}]),
    );
    h.frame();
    let t = h.el("s1", "t");
    let heads = h.find_class(t, "sf-tbl-h");
    assert!(
        h.class(heads[1]).contains("off"),
        "the lowest priority hides"
    );
    assert!(!h.class(heads[0]).contains("off"));
    let grid = h.find_class(t, "sf-tbl")[0];
    assert!(h.doc.border_box(grid).unwrap().w <= 240.5);
}

#[test]
fn reveal_and_scroll_step_scroll_containers() {
    let mut h = Harness::new();
    h.hello(&[]);
    let items: Vec<_> = (0..40)
        .map(|i| json!({"id": format!("i{i}"), "k": "item", "p": {"label": format!("item {i}")}}))
        .collect();
    h.surface(
        "s1",
        "inline",
        json!([{"id": "ls", "k": "list", "p": {"max": {"lines": 5}}, "c": items}]),
    );
    h.frame();
    let sc = h.find_class(h.el("s1", "ls"), "sf-list-scroll")[0];
    assert_eq!(h.doc.scroll_offset(sc).1, 0.0);
    h.frame_ops("s1", 2, json!([["reveal", "i30", "end"]]));
    h.frame();
    let y = h.doc.scroll_offset(sc).1;
    assert!(y > 0.0);
    let item = h.doc.border_box(h.el("s1", "i30")).unwrap();
    let port = h.doc.border_box(sc).unwrap();
    assert!(
        (item.y + item.h - (port.y + port.h)).abs() < 1.0,
        "revealed at the end"
    );
    h.frame_ops("s1", 3, json!([["scroll", "ls", "start"]]));
    h.frame();
    assert_eq!(h.doc.scroll_offset(sc).1, 0.0);
    h.frame_ops("s1", 4, json!([["scroll", "ls", "line-down"]]));
    h.frame();
    assert!((h.doc.scroll_offset(sc).1 - 17.0).abs() < 0.5);
}

#[test]
fn ansi_follow_tail_in_a_scroller() {
    let mut h = Harness::new();
    h.hello(&[]);
    let text: String = (0..50).map(|i| format!("line {i}\n")).collect();
    h.surface("s1", "inline", json!([{"id": "a", "k": "ansi", "p": {"text": text, "follow": true, "max": {"h": "5lines"}}}]));
    h.frame();
    let clip = h.find_class(h.el("s1", "a"), "sf-term-clip")[0];
    let (_, max) = h.doc.max_scroll(clip);
    assert!(max > 0.0);
    assert_eq!(h.doc.scroll_offset(clip).1, max, "follows the tail");
    h.frame_ops(
        "s1",
        2,
        json!([["text", "a", "append", "more\nand more\n"]]),
    );
    h.frame();
    let (_, max2) = h.doc.max_scroll(clip);
    assert!(max2 > max);
    assert_eq!(h.doc.scroll_offset(clip).1, max2);
}

#[test]
fn long_containers_virtualize_around_the_view() {
    let mut h = Harness::new();
    h.hello(&[]);
    let main: Vec<_> = (0..400)
        .map(|i| json!({"id": format!("t{i}"), "k": "text", "p": {"text": format!("entry {i}")}}))
        .collect();
    h.surface("s1", "inline", serde_json::Value::Array(main));
    h.frame();
    // Near the top: the first entries are laid out with their real height.
    let first = h.doc.border_box(h.el("s1", "t0")).unwrap();
    assert!((first.h - 17.0).abs() < 0.5);
    // Scroll the window to the end: the last entries lay out, the first are estimated.
    let pane = h.doc.border_box(h.view.element()).unwrap();
    h.view
        .set_view_rect(Some(auk_ui::Rect::new(0.0, pane.h - 800.0, 960.0, 800.0)));
    h.view.layout(&mut h.doc);
    let last = h.el("s1", "t399");
    let r = h.doc.border_box(last).unwrap();
    assert!((r.h - 17.0).abs() < 0.5, "laid out: {r:?}");
    let laid = h.doc.last_update_stats().laid_out;
    assert!(laid < 400, "only the window lays out: {laid}");
}

#[test]
fn accessibility_tree_has_roles() {
    let mut h = Harness::new();
    h.hello(&[]);
    h.surface(
        "s1",
        "inline",
        json!([
            {"id": "p", "k": "progress", "p": {"value": 0.25, "label": "Upload"}},
            {"id": "m", "k": "meter", "p": {"value": 0.5}},
            {"id": "e", "k": "elapsed", "p": {"age": 2000}},
            {"id": "ls", "k": "list", "p": {"selected": "a"}, "c": [{"id": "a", "k": "item", "p": {"label": "Alpha"}}]},
            {"id": "tr", "k": "tree", "p": {"nodes": [{"id": "n", "label": "src", "open": true, "children": [{"id": "x", "label": "x"}]}]}},
            {"id": "c", "k": "card", "p": {"head": "Fold", "collapsible": true}}
        ]),
    );
    h.apc("f", &json!({"sf": "s1", "s": 2, "ops": [["add", "layer", "s1", null, {"id": "layer", "k": "col", "c": [
        {"id": "o", "k": "overlay", "p": {"head": "Confirm"}, "c": [{"id": "o.t", "k": "text", "p": {"text": "Sure?"}}]}
    ]}]]}).to_string());
    h.frame();
    let ax = h.view.accessibility(&h.doc);
    let by_id = |id: &str| {
        ax.find(&|n| n.id.as_deref() == Some(id))
            .cloned()
            .unwrap_or_else(|| panic!("{id}"))
    };
    assert_eq!(by_id("p").role, "progressbar");
    assert_eq!(by_id("p").value.as_deref(), Some("25"));
    assert_eq!(by_id("m").role, "meter");
    let timer = by_id("e");
    assert_eq!(timer.role, "timer");
    assert_eq!(timer.value.as_deref(), Some("2.0s"));
    let ls = by_id("ls");
    assert_eq!(ls.role, "listbox");
    let opt = by_id("a");
    assert_eq!(opt.role, "option");
    assert_eq!(opt.selected, Some(true));
    assert_eq!(by_id("tr").role, "tree");
    assert!(
        ax.find(&|n| n.role == "treeitem" && n.expanded == Some(true))
            .is_some()
    );
    assert!(
        ax.find(&|n| n.role == "dialog" && n.name.as_deref() == Some("Confirm"))
            .is_some()
    );
    assert!(
        ax.find(&|n| n.role == "button" && n.expanded == Some(true))
            .is_some(),
        "card fold head"
    );
}

#[test]
fn updates_are_incremental() {
    let mut h = Harness::new();
    h.hello(&[]);
    h.surface(
        "s1",
        "inline",
        json!([
            {"id": "md", "k": "md", "p": {"text": "# One\n\npara two\n\nthree", "stream": true}},
            {"id": "code", "k": "code", "p": {"text": "fn a() {}\nfn b() {}", "lang": "rust"}},
            {"id": "an", "k": "ansi", "p": {"text": "l1\nl2"}},
            {"id": "e", "k": "el", "p": {"tag": "p", "text": "x"}, "c": [{"id": "e.k", "k": "text", "p": {"text": "kid"}}]},
            {"id": "c", "k": "col", "c": [{"id": "c.a", "k": "text", "p": {"text": "a"}}, {"id": "c.b", "k": "text", "p": {"text": "b"}}]}
        ]),
    );
    h.frame();
    let blocks = |h: &Harness| h.doc.element_children(h.el("s1", "md"));
    let before = blocks(&h);
    assert_eq!(before.len(), 3);
    assert!(h.class(before[2]).contains("streaming"));
    // A streaming append re-renders only the last block.
    h.frame_ops("s1", 2, json!([["text", "md", "append", " and more"]]));
    let after = blocks(&h);
    assert_eq!(after[..2], before[..2]);
    assert_ne!(after[2], before[2]);
    assert!(h.text(h.el("s1", "md")).contains("three and more"));
    assert_eq!(h.find_class(h.el("s1", "md"), "md-caret").len(), 1);
    // A new block appends; the earlier ones stay.
    h.frame_ops(
        "s1",
        3,
        json!([["text", "md", "append", "\n\n- four"], ["set", "md", {"stream": false}]]),
    );
    let after2 = blocks(&h);
    assert_eq!(after2.len(), 4);
    assert_eq!(after2[..2], before[..2]);
    assert!(h.find_class(h.el("s1", "md"), "md-caret").is_empty());
    // code: appending keeps the unchanged lines' elements.
    let lines = |h: &Harness| h.find_class(h.el("s1", "code"), "sf-ln");
    let l0 = lines(&h);
    h.frame_ops("s1", 4, json!([["text", "code", "append", "\nfn c() {}"]]));
    let l1 = lines(&h);
    assert_eq!(l1.len(), 3);
    assert_eq!(l1[0], l0[0]);
    // ansi: unchanged rows keep their elements.
    let rows = |h: &Harness| h.find_class(h.el("s1", "an"), "ln");
    let r0 = rows(&h);
    h.frame_ops("s1", 5, json!([["text", "an", "append", "\nl3"]]));
    let r1 = rows(&h);
    assert_eq!(r1.len(), 3);
    assert_eq!(r1[0], r0[0]);
    // An el changing its tag gets a new element; its children move over.
    let kid = h.el("s1", "e.k");
    h.frame_ops("s1", 6, json!([["set", "e", {"tag": "section"}]]));
    let e = h.el("s1", "e");
    assert_eq!(h.doc.node(e).element().unwrap().tag.as_str(), "section");
    assert_eq!(h.doc.parent(kid), Some(e));
    assert_eq!(h.text(e), "xkid");
    // move, then del with re-add of the same id.
    h.frame_ops("s1", 7, json!([["move", "c.b", "c", "c.a"]]));
    let kids: Vec<String> = h
        .doc
        .element_children(h.el("s1", "c"))
        .iter()
        .map(|k| h.attr(*k, "data-id").unwrap())
        .collect();
    assert_eq!(kids, ["c.b", "c.a"]);
    h.frame_ops("s1", 8, json!([["del", "c.a"], ["add", "c.a", "c", "c.b", {"id": "c.a", "k": "badge", "p": {"text": "A"}}]]));
    let kids: Vec<String> = h
        .doc
        .element_children(h.el("s1", "c"))
        .iter()
        .map(|k| h.attr(*k, "data-id").unwrap())
        .collect();
    assert_eq!(kids, ["c.a", "c.b"]);
    assert!(h.class(h.el("s1", "c.a")).starts_with("sf sf-badge"));
    assert!(h.errors().is_empty(), "{:?}", h.errors());
}
