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

/// A `#rrggbb` variable's value in an inline style.
fn var_color(style: &str, var: &str) -> [u8; 3] {
    let v = style
        .split(&format!("{var}: #"))
        .nth(1)
        .unwrap_or_else(|| panic!("no {var} in {style}"));
    let n = u32::from_str_radix(&v[..6], 16).unwrap();
    [(n >> 16) as u8, (n >> 8) as u8, n as u8]
}

#[test]
fn palette_quiet_tokens_get_3_to_1_and_the_light_variant_follows_the_appearance() {
    use auk_views::palette::{PAGE_DARK, PAGE_LIGHT, contrast};
    let mut h = Harness::new();
    h.hello(&[]);
    h.apc("o", r#"{"id":"s1"}"#);
    // Dim text tokens on both variants, and a fill whose tint the text must also clear.
    h.apc(
        "t",
        r##"{"dark":{"muted":"#3a3a3a","text":"#3a3a3a","cardBg":"#202020"},"light":{"muted":"#e0e0e0","text":"#e0e0e0","accent":"#ffeeaa"}}"##,
    );
    h.frame_ops(
        "s1",
        1,
        json!([["add", "main", "s1", null, {"id": "main", "k": "col", "c": [{"id": "t", "k": "text", "p": {"text": "x"}}]}]]),
    );
    h.frame();
    let main = h.view.region(key(&h, "s1"), Region::Main).unwrap();
    let style = h.view.written().style(main).unwrap().to_owned();
    let tint = {
        // cardBg at 60% over the dark page.
        let m = |a: u8, b: u8| (f64::from(a) + (f64::from(b) - f64::from(a)) * 0.6).round() as u8;
        [
            m(PAGE_DARK[0], 0x20),
            m(PAGE_DARK[1], 0x20),
            m(PAGE_DARK[2], 0x20),
        ]
    };
    let muted = var_color(&style, "--sf-p-muted");
    let text = var_color(&style, "--sf-p-text");
    for bg in [PAGE_DARK, tint] {
        let quiet = contrast(muted, bg);
        assert!(quiet >= 3.0, "quiet token at {quiet}:1");
        assert!(
            contrast(text, bg) >= 4.5,
            "body text at {}:1",
            contrast(text, bg)
        );
    }
    // Quiet tokens are corrected only as far as 3:1, so they stay quieter than body text.
    assert!(contrast(muted, PAGE_DARK) < 4.5, "{muted:?}");
    assert!(contrast(muted, PAGE_DARK) < contrast(text, PAGE_DARK));
    assert!(
        !style.contains("--sf-p-accent"),
        "dark has no accent: {style}"
    );

    // Light appearance: the light variant, corrected against the light page.
    let light = ViewSettings {
        dark: false,
        ..Default::default()
    };
    h.view.set_settings(&mut h.doc, &h.pane, light);
    h.frame();
    let style = h.view.written().style(main).unwrap().to_owned();
    assert!(style.contains("--sf-p-accent"), "{style}");
    assert!(
        !style.contains("--sf-tint-neutral"),
        "dark's fill is gone: {style}"
    );
    let muted = var_color(&style, "--sf-p-muted");
    let text = var_color(&style, "--sf-p-text");
    let accent = var_color(&style, "--sf-p-accent");
    assert!(
        (3.0..4.5).contains(&contrast(muted, PAGE_LIGHT)),
        "{muted:?}"
    );
    assert!(contrast(text, PAGE_LIGHT) >= 4.5);
    assert!(contrast(accent, PAGE_LIGHT) >= 4.5);
    // Corrected toward black on a light page.
    assert!(muted.iter().all(|&c| c < 0xe0), "{muted:?}");
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
fn md_streaming_caret_and_tool_and_agent_timers_run_on_the_view_clock() {
    let mut h = Harness::new();
    h.hello(&[]);
    h.surface(
        "s1",
        "inline",
        json!([
            {"id": "md", "k": "md", "p": {"text": "Thinking about it", "stream": true}},
            {"id": "tl", "k": "tool", "p": {"name": "bash", "title": "Run", "status": "running", "age": 2500}},
            {"id": "ag", "k": "agent", "p": {"name": "worker", "status": "running",
                "stats": {"age": 61000},
                "tool": {"name": "read", "intent": "Reading", "age": 300},
                "retry": {"attempt": 2, "max": 5, "delay": 4000, "age": 1000}}}
        ]),
    );
    h.frame();
    let t0 = h.now;
    // The md caret sits at the end of the streaming block and pulses on the view's clock.
    let md = h.el("s1", "md");
    let caret = h.find_class(md, "md-caret");
    assert_eq!(caret.len(), 1, "one caret while streaming");
    let caret = caret[0];
    let opacity = |h: &Harness| h.view.written().style(caret).map(str::to_owned);
    assert_eq!(opacity(&h).as_deref(), Some("opacity: 1"));
    let next = h.view.tick(&mut h.doc, &h.pane, t0 + 600.0);
    assert!(next.is_some(), "the caret asks for frames");
    assert_eq!(opacity(&h).as_deref(), Some("opacity: 0.25"));
    h.view.tick(&mut h.doc, &h.pane, t0 + 1100.0);
    assert_eq!(opacity(&h).as_deref(), Some("opacity: 1"));
    // Timers: start = receive − age, ticking with the clock; the retry counts down to zero.
    let tool_time = h.find_class(h.el("s1", "tl"), "sf-tool-time")[0];
    assert_eq!(h.attr(tool_time, "role").as_deref(), Some("timer"));
    assert_eq!(h.text(tool_time), "3.6s");
    let ag = h.el("s1", "ag");
    let times = h.find_class(ag, "time");
    let texts = |h: &Harness| times.iter().map(|&t| h.text(t)).collect::<Vec<_>>();
    assert_eq!(texts(&h), ["1m 02s", "1.4s", "in 1.9s"]);
    h.view.tick(&mut h.doc, &h.pane, t0 + 5000.0);
    assert_eq!(h.text(tool_time), "7.5s");
    assert_eq!(texts(&h), ["1m 06s", "5.3s", "in 0.0s"]);
    // Finished: `took` freezes the tool's time, and the md caret goes when the stream ends.
    h.now = t0 + 5000.0;
    h.frame_ops(
        "s1",
        2,
        json!([
            ["set", "tl", {"status": "done", "took": 1200}],
            ["set", "md", {"stream": false}]
        ]),
    );
    h.frame();
    let tool_time = h.find_class(h.el("s1", "tl"), "sf-tool-time")[0];
    assert_eq!(h.text(tool_time), "1.2s");
    h.view.tick(&mut h.doc, &h.pane, t0 + 9000.0);
    assert_eq!(h.text(tool_time), "1.2s", "frozen");
    assert!(h.find_class(h.el("s1", "md"), "md-caret").is_empty());
    // A hidden pane pauses all of it.
    h.view.set_visible(&mut h.doc, &h.pane, false);
    assert!(h.view.tick(&mut h.doc, &h.pane, t0 + 9100.0).is_none());
}

#[test]
fn an_arriving_age_restarts_top_level_timers_and_nested_agent_ages_only_on_a_new_value() {
    // §7.13 (book:6731, 7864, 8048): a top-level `age` (elapsed, tool) restarts whenever it
    // arrives, unchanged or a countdown too; an agent's nested ages restart only on a new value.
    let mut h = Harness::new();
    h.hello(&[]);
    h.surface(
        "s1",
        "inline",
        json!([
            {"id": "el", "k": "elapsed", "p": {"age": 1000}},
            {"id": "cd", "k": "elapsed", "p": {"age": -3000, "format": "clock"}},
            {"id": "tl", "k": "tool", "p": {"name": "bash", "title": "Run", "status": "running", "age": 1000}},
            {"id": "ag", "k": "agent", "p": {"name": "worker", "status": "running",
                "stats": {"age": 1000}, "tool": {"name": "read", "age": 1000}}}
        ]),
    );
    h.frame();
    let t0 = h.now;
    let tool_time = |h: &Harness| h.text(h.find_class(h.el("s1", "tl"), "sf-tool-time")[0]);
    let agent_times =
        |h: &Harness| h.find_class(h.el("s1", "ag"), "time").iter().map(|&t| h.text(t)).collect::<Vec<_>>();
    assert_eq!(h.text(h.el("s1", "el")), "1.0s");
    assert_eq!(tool_time(&h), "1.0s");
    assert_eq!(agent_times(&h), ["1.0s", "1.0s"]);
    h.view.tick(&mut h.doc, &h.pane, t0 + 2000.0);
    assert_eq!(h.text(h.el("s1", "el")), "3.0s");
    assert_eq!(h.text(h.el("s1", "cd")), "0:01");
    // The same values arrive again, 2 s later.
    h.now = t0 + 2000.0;
    h.frame_ops(
        "s1",
        2,
        json!([
            ["set", "el", {"age": 1000}],
            ["set", "cd", {"age": -3000}],
            ["set", "tl", {"age": 1000}],
            ["set", "ag", {"stats": {"age": 1000, "tools": 2}, "tool": {"name": "read", "age": 1000}}]
        ]),
    );
    h.frame();
    assert_eq!(h.text(h.el("s1", "el")), "1.0s", "elapsed restarts");
    assert_eq!(h.text(h.el("s1", "cd")), "0:03", "the countdown starts over");
    assert_eq!(tool_time(&h), "1.0s", "tool restarts");
    assert_eq!(agent_times(&h), ["3.0s", "3.0s"], "nested ages keep running");
    // A set of other props leaves the restarted count running.
    h.now = t0 + 2500.0;
    h.frame_ops("s1", 3, json!([["set", "el", {"format": "short"}]]));
    h.frame();
    assert_eq!(h.text(h.el("s1", "el")), "1.5s");
    // A node deleted and re-added under the same id, with the same age, anchors fresh.
    h.now = t0 + 3000.0;
    h.frame_ops("s1", 4, json!([["del", "el"]]));
    h.frame();
    h.now = t0 + 6000.0;
    h.frame_ops(
        "s1",
        5,
        json!([["add", "el", "main", null, {"id": "el", "k": "elapsed", "p": {"age": 1000}}]]),
    );
    h.frame();
    assert_eq!(h.text(h.el("s1", "el")), "1.0s");
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

/// §7.13 Reduce Motion: no shimmer (the `shimmer` kind and span `fx` hold still, in the text's
/// own color), no pulse, and a `meter` shows its value at once.
#[test]
fn reduce_motion_means_no_shimmer_no_pulse_and_meters_at_their_value() {
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
            {"id": "sh", "k": "shimmer", "p": {"text": "Working"}},
            {"id": "tx", "k": "text", "p": {"spans": [
                {"t": "Thinking", "fx": "shimmer"},
                {"t": "live", "fx": "pulse"}
            ]}},
            {"id": "mt", "k": "meter", "p": {"value": 0.4}}
        ]),
    );
    h.frame();
    let t0 = h.now;
    let kind_chars = h.find_class(h.el("s1", "sh"), "sf-sh");
    let fx_chars = h.find_class(h.el("s1", "tx"), "sf-sh");
    let pulse = h.find_class(h.el("s1", "tx"), "sf-fx-pulse")[0];
    for step in 0..8 {
        let next = h.view.tick(&mut h.doc, &h.pane, t0 + 300.0 * f64::from(step));
        assert!(next.is_none(), "nothing asks for frames");
        assert!(levels(&h, &kind_chars).iter().all(|&l| l == 0));
        assert!(levels(&h, &fx_chars).iter().all(|&l| l == 0));
        assert_eq!(h.view.written().style(pulse), None, "no pulse");
    }
    // Still characters draw in the text's color, not the shimmer's low tone.
    h.frame();
    let color = |el| h.doc.computed_style(el).unwrap().color;
    let sh = h.el("s1", "sh");
    assert!(kind_chars.iter().all(|&c| color(c) == color(sh)));
    let fill = h.find_class(h.el("s1", "mt"), "fill");
    let fill = fill.first().copied().unwrap_or_else(|| h.find_class(h.el("s1", "mt"), "sf-mt-fill")[0]);
    assert_eq!(h.view.written().style(fill), Some("width: 40%"));
}

/// §8.4: the system's Reduce Motion, Reduce Transparency and Increase Contrast reach program
/// sheets as `prefers-reduced-motion`, `prefers-reduced-transparency` and `prefers-contrast`, and
/// a settings change re-matches them.
#[test]
fn the_media_preferences_reach_program_sheets() {
    let mut h = Harness::new();
    h.hello(&[]);
    h.surface(
        "s1",
        "inline",
        json!([
            {"id": "m", "k": "el", "p": {"tag": "div", "class": "m"}},
            {"id": "t", "k": "el", "p": {"tag": "div", "class": "t"}},
            {"id": "c", "k": "el", "p": {"tag": "div", "class": "c"}}
        ]),
    );
    h.apc(
        "s",
        r#"{"sf":"s1","name":"main","css":".m, .t, .c { width: 10px } @media (prefers-reduced-motion: reduce) { .m { width: 20px } } @media (prefers-reduced-transparency: reduce) { .t { width: 30px } } @media (prefers-contrast: more) { .c { width: 40px } }"}"#,
    );
    h.frame();
    assert!(h.errors().is_empty(), "{:?}", h.errors());
    let widths = |h: &Harness| ["m", "t", "c"].map(|id| h.doc.border_box(h.el("s1", id)).unwrap().w);
    assert_eq!(widths(&h), [10.0, 10.0, 10.0]);
    for (s, want) in [
        ((true, false, false), [20.0, 10.0, 10.0]),
        ((false, true, false), [10.0, 30.0, 10.0]),
        ((false, false, true), [10.0, 10.0, 40.0]),
        ((true, true, true), [20.0, 30.0, 40.0]),
        ((false, false, false), [10.0, 10.0, 10.0]),
    ] {
        let settings = ViewSettings {
            reduce_motion: s.0,
            reduce_transparency: s.1,
            more_contrast: s.2,
            ..h.view.settings().clone()
        };
        h.view.set_settings(&mut h.doc, &h.pane, settings);
        h.frame();
        assert_eq!(widths(&h), want, "{s:?}");
    }
}

/// The shimmer levels of `chars` (`sf-sh`, `sf-sh mid`, `sf-sh hi`) as 0, 1, 2.
fn levels(h: &Harness, chars: &[auk_ui::NodeId]) -> Vec<u8> {
    chars
        .iter()
        .map(|&c| {
            let class = h.class(c);
            let has = |x: &str| class.split(' ').any(|t| t == x);
            if has("hi") {
                2
            } else if has("mid") {
                1
            } else {
                0
            }
        })
        .collect()
}

/// §7.13: the `shimmer` kind sweeps on the view's clock (2.25 s, staggered per character), and
/// span `fx` (shimmer and pulse) inside any spans move with it.
#[test]
fn shimmer_and_span_fx_move_on_the_view_clock() {
    let mut h = Harness::new();
    h.hello(&[]);
    h.surface(
        "s1",
        "inline",
        json!([
            {"id": "sh", "k": "shimmer", "p": {"text": "Working hard"}},
            {"id": "tx", "k": "text", "p": {"spans": [
                {"t": "Thinking", "fx": "shimmer"},
                {"t": " · "},
                {"t": "live", "fx": "pulse"}
            ]}}
        ]),
    );
    h.frame();
    let t0 = h.now;
    let kind_chars = h.find_class(h.el("s1", "sh"), "sf-sh");
    assert_eq!(kind_chars.len(), "Working hard".chars().count());
    let fx = h.find_class(h.el("s1", "tx"), "sf-fx-shimmer");
    assert_eq!(fx.len(), 1);
    let fx_chars = h.find_class(fx[0], "sf-sh");
    assert_eq!(fx_chars.len(), "Thinking".chars().count());
    let pulse = h.find_class(h.el("s1", "tx"), "sf-fx-pulse")[0];
    let opacity = |h: &Harness| h.view.written().style(pulse).map(str::to_owned);

    let mut kind_seen = Vec::new();
    let mut fx_seen = Vec::new();
    let mut pulse_seen = Vec::new();
    for step in 0..12 {
        let next = h.view.tick(&mut h.doc, &h.pane, t0 + 200.0 * f64::from(step));
        assert!(next.is_some(), "shimmer and fx keep asking for frames");
        kind_seen.push(levels(&h, &kind_chars));
        fx_seen.push(levels(&h, &fx_chars));
        pulse_seen.push(opacity(&h));
    }
    // The sweep moves: the levels change over time, and the highlight reaches every level.
    for seen in [&kind_seen, &fx_seen] {
        assert!(seen.windows(2).any(|w| w[0] != w[1]), "{seen:?}");
        assert!(seen.iter().flatten().any(|&l| l == 2), "{seen:?}");
        assert!(seen.iter().flatten().any(|&l| l == 0), "{seen:?}");
    }
    // The staggered characters aren't all at one level at once.
    assert!(kind_seen.iter().any(|l| l.iter().any(|&x| x != l[0])), "{kind_seen:?}");
    // The pulse's opacity runs 1 → .45 → 1 over 1.6 s.
    let ops: Vec<f64> = pulse_seen
        .iter()
        .map(|o| {
            o.as_deref()
                .and_then(|s| s.strip_prefix("opacity: "))
                .unwrap()
                .parse()
                .unwrap()
        })
        .collect();
    assert!(ops.iter().any(|&o| o < 0.6) && ops.iter().any(|&o| o > 0.9), "{ops:?}");
    // The clock is the view's: the same time gives the same levels.
    h.view.tick(&mut h.doc, &h.pane, t0 + 400.0);
    assert_eq!(levels(&h, &kind_chars), kind_seen[2]);
}

/// §7.13: a `meter` grows from 0 on first show (0.6 s) and later values transition (0.4 s), on
/// the view's clock.
#[test]
fn meter_values_grow_and_transition_on_the_view_clock() {
    let mut h = Harness::new();
    h.hello(&[]);
    h.surface(
        "s1",
        "inline",
        json!([{"id": "mt", "k": "meter", "p": {"value": 0.5}}]),
    );
    h.frame();
    let t0 = h.now;
    let mt = h.el("s1", "mt");
    let fill = |h: &Harness| {
        let fills: Vec<_> = h
            .find_class(mt, "fill")
            .into_iter()
            .chain(h.find_class(mt, "sf-mt-fill"))
            .collect();
        let style = h.view.written().style(fills[0]).unwrap_or("").to_owned();
        style
            .split(';')
            .find_map(|d| d.trim().strip_prefix("width: "))
            .and_then(|w| w.trim_end_matches('%').parse::<f64>().ok())
            .unwrap_or_else(|| panic!("a width in {style:?}"))
    };
    assert_eq!(fill(&h), 0.0, "the first show starts at 0");
    assert!(h.view.tick(&mut h.doc, &h.pane, t0 + 200.0).is_some());
    let mid = fill(&h);
    assert!(mid > 0.0 && mid < 50.0, "{mid}");
    h.view.tick(&mut h.doc, &h.pane, t0 + 700.0);
    assert_eq!(fill(&h), 50.0);
    // Settled: no more frames.
    assert!(h.view.tick(&mut h.doc, &h.pane, t0 + 720.0).is_none());
    // A new value transitions from where it was.
    h.now = t0 + 1000.0;
    h.frame_ops("s1", 2, json!([["set", "mt", {"value": 0.9}]]));
    h.view.tick(&mut h.doc, &h.pane, h.now + 150.0);
    let mid = fill(&h);
    assert!(mid > 50.0 && mid < 90.0, "{mid}");
    h.view.tick(&mut h.doc, &h.pane, h.now + 450.0);
    assert_eq!(fill(&h), 90.0);
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

/// Whether anything under `el` has had its text laid out.
fn text_laid_out(h: &Harness, el: auk_ui::NodeId) -> bool {
    let mut stack = vec![el];
    while let Some(n) = stack.pop() {
        if h.doc.node(n).is_element() && h.doc.line_count(n) > 0 {
            return true;
        }
        stack.extend(h.doc.element_children(n));
    }
    false
}

#[test]
fn long_lists_code_ansi_md_kv_tables_and_trees_virtualize() {
    let n = 600;
    let lines: Vec<String> = (0..n).map(|i| format!("line {i}")).collect();
    let cases: Vec<(&str, serde_json::Value, &str)> = vec![
        (
            "list",
            json!({"id": "x", "k": "list", "c": (0..n).map(|i| json!({"id": format!("i{i}"), "k": "item", "p": {"label": format!("item {i}")}})).collect::<Vec<_>>()}),
            "sf-item",
        ),
        (
            "code",
            json!({"id": "x", "k": "code", "p": {"text": lines.join("\n"), "lang": "txt"}}),
            "sf-ln",
        ),
        (
            "ansi",
            json!({"id": "x", "k": "ansi", "p": {"text": lines.join("\r\n")}}),
            "ln",
        ),
        (
            "md",
            json!({"id": "x", "k": "md", "p": {"text": lines.join("\n\n")}}),
            "md",
        ),
        (
            "kv",
            json!({"id": "x", "k": "kv", "p": {"items": (0..n).map(|i| json!({"k": format!("key{i}"), "v": format!("value {i}")})).collect::<Vec<_>>()}}),
            "sf-kv-item",
        ),
        (
            "table",
            json!({"id": "x", "k": "table", "p": {"cols": [{"id": "a", "head": "A"}, {"id": "b", "head": "B"}], "rows": (0..n).map(|i| json!({"id": i, "cells": {"a": format!("a{i}"), "b": format!("b{i}")}})).collect::<Vec<_>>()}}),
            "sf-tbl-row",
        ),
        (
            "tree",
            json!({"id": "x", "k": "tree", "p": {"nodes": (0..n).map(|i| json!({"id": format!("n{i}"), "label": format!("node {i}")})).collect::<Vec<_>>()}}),
            "sf-tree-node",
        ),
        (
            "tree group",
            json!({"id": "x", "k": "tree", "p": {"nodes": [{"id": "root", "label": "root", "open": true, "children": (0..n).map(|i| json!({"id": format!("n{i}"), "label": format!("node {i}")})).collect::<Vec<_>>()}]}}),
            "sf-tree-node",
        ),
    ];
    for (name, node, row_class) in cases {
        let mut h = Harness::new();
        h.hello(&[]);
        h.surface("s1", "inline", json!([node]));
        h.frame();
        let x = h.el("s1", "x");
        let rows = h.find_class(x, row_class);
        let rows: Vec<_> = if name == "tree group" {
            rows[1..].to_vec()
        } else {
            rows
        };
        assert!(rows.len() >= n, "{name}: {} rows", rows.len());
        let (first, last) = (rows[0], *rows.last().unwrap());
        // Near the view: laid out. Far below it: an estimate, not laid out.
        assert!(
            text_laid_out(&h, first),
            "{name}: the first row is laid out"
        );
        assert!(
            !text_laid_out(&h, last),
            "{name}: the last row is only estimated"
        );
        let laid = h.doc.last_update_stats().laid_out;
        assert!(laid < n, "{name}: only the window lays out ({laid} boxes)");
        // Ops on off-screen rows still apply: they lay out when they come into view.
        let pane = h.doc.border_box(h.view.element()).unwrap();
        h.view
            .set_view_rect(Some(auk_ui::Rect::new(0.0, pane.h - 800.0, 960.0, 800.0)));
        h.view.layout(&mut h.doc);
        let rows = h.find_class(x, row_class);
        let last = *rows.last().unwrap();
        assert!(
            text_laid_out(&h, last),
            "{name}: the last row lays out in view"
        );
        // The copy text still has every row (§8.3: Copy sees everything).
        assert!(h.text(x).contains(&format!("{}", n - 1)), "{name}");
    }
}

/// §7.10 Cost: "Ops on off-screen nodes apply immediately; their layout waits until they come
/// into view." A `set` and a `text` op on rows far below the view of a long, virtualized `col`
/// land in the document, the DOM and the col's Copy at once, while those rows stay unlaid; when
/// the view scrolls to them they lay out with the new text.
#[test]
fn ops_on_off_screen_nodes_apply_at_once_and_lay_out_in_view() {
    let n = 400;
    let mut h = Harness::new();
    h.hello(&[]);
    let rows: Vec<_> = (0..n)
        .map(|i| json!({"id": format!("t{i}"), "k": "text", "p": {"text": format!("entry {i}")}}))
        .collect();
    h.surface(
        "s1",
        "inline",
        json!([{"id": "log", "k": "col", "p": {"actions": {"menu": ["copy"]}}, "c": rows}]),
    );
    h.frame();
    let (last, prev) = (h.el("s1", "t399"), h.el("s1", "t398"));
    assert!(text_laid_out(&h, h.el("s1", "t0")), "the top is laid out");
    assert!(
        !text_laid_out(&h, last) && !text_laid_out(&h, prev),
        "the bottom is estimated"
    );

    h.frame_ops(
        "s1",
        2,
        json!([
            ["set", "t399", {"text": "rewritten far below"}],
            ["text", "t398", "append", " and appended"],
        ]),
    );
    h.frame();
    assert!(h.errors().is_empty(), "{:?}", h.errors());
    // Applied: in the document, the DOM and the Copy of the col (run from its visible top).
    let surface = h
        .pane
        .surfaces()
        .iter()
        .rev()
        .find(|s| s.id() == "s1")
        .unwrap();
    let doc = surface.document();
    let prop = |id: &str| {
        doc.get(id)
            .unwrap()
            .prop("text")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
    };
    assert_eq!(prop("t399").as_deref(), Some("rewritten far below"));
    assert_eq!(prop("t398").as_deref(), Some("entry 398 and appended"));
    assert_eq!(h.text(last), "rewritten far below");
    assert_eq!(h.text(prev), "entry 398 and appended");
    let top = h.doc.border_box(h.el("s1", "t0")).unwrap();
    let out = h.view.menu_pick(
        &mut h.doc,
        &h.pane,
        top.x + 4.0,
        top.y + top.h / 2.0,
        "copy",
    );
    let [auk_views::Outcome::Copy(copied)] = out.as_slice() else {
        panic!("one copy: {out:?}");
    };
    assert!(
        copied.contains("entry 398 and appended") && copied.ends_with("rewritten far below"),
        "{copied}"
    );
    // Not laid out: the op's layout waits, and the frame laid out only a few boxes.
    assert!(
        !text_laid_out(&h, last) && !text_laid_out(&h, prev),
        "still estimated"
    );
    let laid = h.doc.last_update_stats().laid_out;
    assert!(laid < 50, "the off-screen ops laid out {laid} boxes");

    // Scrolled into view: both lay out, with the new text.
    let pane = h.doc.border_box(h.view.element()).unwrap();
    h.view
        .set_view_rect(Some(auk_ui::Rect::new(0.0, pane.h - 800.0, 960.0, 800.0)));
    h.view.layout(&mut h.doc);
    assert!(
        text_laid_out(&h, last) && text_laid_out(&h, prev),
        "laid out in view"
    );
    for el in [last, prev] {
        let r = h.doc.border_box(el).unwrap();
        assert!((r.h - 17.0).abs() < 0.5, "one laid-out line: {r:?}");
    }
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

#[test]
fn code_highlights_a_screenful_on_render_and_the_rest_in_the_background() {
    // Under one APC string (262 144 bytes).
    let n = 12_000;
    let text: Vec<String> = (0..n).map(|i| format!("let x{i}=1;//c")).collect();
    let mut h = Harness::new();
    h.hello(&[]);
    h.surface(
        "s1",
        "inline",
        json!([{"id": "c", "k": "code", "p": {"text": text.join("\n"), "lang": "rust"}}]),
    );
    let lines = |h: &Harness| h.find_class(h.el("s1", "c"), "sf-ln");
    let keyword = |h: &Harness, ln: auk_ui::NodeId| !h.find_class(ln, "tk-keyword").is_empty();
    // Right after the render: every line is drawn, the first screenful highlighted, the tail
    // still plain.
    let ls = lines(&h);
    assert_eq!(ls.len(), n);
    assert!(keyword(&h, ls[0]) && keyword(&h, ls[100]));
    assert!(!keyword(&h, ls[n - 1]), "the tail waits for the background");
    assert_eq!(h.text(ls[n - 1]), format!("let x{}=1;//c", n - 1));
    // The view's clock draws the rest in as the worker finishes it.
    let started = std::time::Instant::now();
    loop {
        h.now += 16.0;
        let more = h.view.tick(&mut h.doc, &h.pane, h.now);
        if more.is_none() {
            break;
        }
        assert!(
            started.elapsed().as_secs() < 60,
            "highlighting never finished"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let ls = lines(&h);
    assert!(keyword(&h, ls[n - 1]), "the tail highlighted");
    assert!(!h.find_class(ls[n / 2], "tk-comment").is_empty());
    // Incremental: an append keeps the drawn lines and highlights the new one at once.
    let kept = ls[n - 2];
    h.frame_ops("s1", 2, json!([["text", "c", "append", "\nlet tail = 1;"]]));
    let ls = lines(&h);
    assert_eq!(ls.len(), n + 1);
    assert_eq!(ls[n - 2], kept, "unchanged lines stay");
    assert!(keyword(&h, ls[n]));
    assert!(
        h.view.tick(&mut h.doc, &h.pane, h.now + 16.0).is_none(),
        "nothing left to do"
    );
}

#[test]
fn fold_and_overlay_transitions_run_on_the_view_clock() {
    let mut h = Harness::new();
    h.hello(&[]);
    h.surface(
        "s1",
        "inline",
        json!([
            {"id": "c", "k": "card", "p": {"head": "Folded", "collapsible": true, "collapsed": true},
             "c": [{"id": "ct", "k": "text", "p": {"text": "inside"}}]},
            {"id": "o", "k": "card", "p": {"head": "Open", "collapsible": true},
             "c": [{"id": "ot", "k": "text", "p": {"text": "inside"}}]}
        ]),
    );
    h.frame();
    let t0 = h.now;
    let body = |h: &Harness, id: &str| h.find_class(h.el("s1", id), "sf-card-body")[0];
    let opacity = |h: &Harness, el| h.doc.computed_style(el).unwrap().opacity;
    // What the first draw shows doesn't animate.
    assert!(!h.class(h.el("s1", "o")).contains("sf-unfold"));
    assert_eq!(opacity(&h, body(&h, "o")), 1.0);
    // The program unfolds a card: its body fades in on the view's clock.
    h.frame_ops("s1", 2, json!([["set", "c", {"collapsed": false}]]));
    h.frame();
    assert!(h.class(h.el("s1", "c")).contains("sf-unfold"));
    let b = body(&h, "c");
    assert!(opacity(&h, b) < 0.2, "{}", opacity(&h, b));
    assert!(
        h.view.tick(&mut h.doc, &h.pane, t0 + 90.0).is_some(),
        "it asks for frames"
    );
    h.view.layout(&mut h.doc);
    let mid = opacity(&h, b);
    assert!(mid > 0.2 && mid < 1.0, "{mid}");
    // A hidden pane pauses it without resetting.
    h.view.set_visible(&mut h.doc, &h.pane, false);
    assert!(h.view.tick(&mut h.doc, &h.pane, t0 + 5000.0).is_none());
    h.view.set_visible(&mut h.doc, &h.pane, true);
    h.view.tick(&mut h.doc, &h.pane, t0 + 5000.0);
    h.view.layout(&mut h.doc);
    assert!((opacity(&h, b) - mid).abs() < 0.05, "resumed where it was");
    h.view.tick(&mut h.doc, &h.pane, t0 + 5300.0);
    h.view.layout(&mut h.doc);
    assert_eq!(opacity(&h, b), 1.0);

    // An overlay that opens later pops in; the card under it scales from 0.96.
    h.now = t0 + 6000.0;
    h.frame_ops(
        "s1",
        3,
        json!([["add", "layer", "s1", null, {"id": "layer", "k": "col", "c": [
            {"id": "ov", "k": "overlay", "p": {"head": "Ask"}, "c": [{"id": "q", "k": "text", "p": {"text": "?"}}]}
        ]}]]),
    );
    h.frame();
    let ov = h.el("s1", "ov");
    assert!(h.class(ov).contains("sf-pop"));
    let card = h.find_class(ov, "sf-ov-card")[0];
    assert!(opacity(&h, card) < 0.2);
    assert!(h.doc.computed_style(card).unwrap().transform.is_some());
    h.view.tick(&mut h.doc, &h.pane, h.now + 300.0);
    h.view.layout(&mut h.doc);
    assert_eq!(opacity(&h, card), 1.0);
    assert!(
        h.view.tick(&mut h.doc, &h.pane, h.now + 316.0).is_none(),
        "settled"
    );
    // New props build its card again, after the pop: it doesn't pop again (A17).
    h.now += 1000.0;
    h.frame_ops("s1", 4, json!([["set", "ov", {"head": "Ask again"}]]));
    h.frame();
    let card = h.find_class(ov, "sf-ov-card")[0];
    assert_eq!(opacity(&h, card), 1.0, "no replay");
    assert!(h.view.tick(&mut h.doc, &h.pane, h.now + 16.0).is_none());

    // Reduce Motion: still equivalents.
    let mut h = Harness::with(
        auk_tsp::Config::default(),
        ViewSettings {
            reduce_motion: true,
            ..Default::default()
        },
    );
    h.hello(&[]);
    h.surface(
        "s1",
        "inline",
        json!([{"id": "c", "k": "card", "p": {"head": "F", "collapsible": true, "collapsed": true},
                "c": [{"id": "ct", "k": "text", "p": {"text": "x"}}]}]),
    );
    h.frame();
    h.frame_ops("s1", 2, json!([["set", "c", {"collapsed": false}]]));
    h.frame();
    assert_eq!(opacity(&h, body(&h, "c")), 1.0);
    assert!(h.view.tick(&mut h.doc, &h.pane, h.now + 16.0).is_none());
}

/// Every scroll container under `el` (itself included) and its offset.
fn scroll_offsets(h: &Harness, el: auk_ui::NodeId) -> Vec<(auk_ui::NodeId, f32)> {
    let mut out = Vec::new();
    let mut stack = vec![el];
    while let Some(n) = stack.pop() {
        if h.doc.node(n).is_element()
            && h.doc
                .computed_style(n)
                .is_some_and(|s| s.is_scroll_container())
        {
            out.push((n, h.doc.scroll_offset(n).1));
        }
        stack.extend(h.doc.children(n).iter().copied());
    }
    out
}

/// §7.6 `scroll`: "An inline surface's `main` ignores it." Scrolling `main` leaves every scroll
/// offset in the surface as it was (a scrollable list inside it included), while the same op on
/// the list itself, and on a screen surface's `main` (which scrolls itself, §7.5), moves them.
#[test]
fn scroll_on_an_inline_surfaces_main_leaves_every_scroll_offset_unchanged() {
    let mut h = Harness::new();
    h.hello(&["scroll"]);
    let items: Vec<_> = (0..40)
        .map(|i| json!({"id": format!("i{i}"), "k": "item", "p": {"label": format!("item {i}")}}))
        .collect();
    h.surface(
        "s1",
        "inline",
        json!([{"id": "ls", "k": "list", "p": {"max": {"lines": 5}}, "c": items}]),
    );
    h.frame();
    let main = h.view.region(key(&h, "s1"), Region::Main).unwrap();
    let sc = h.find_class(h.el("s1", "ls"), "sf-list-scroll")[0];
    // Start part way down so neither direction is clamped away.
    h.frame_ops("s1", 2, json!([["scroll", "ls", "page-down"]]));
    h.frame();
    let before = scroll_offsets(&h, main);
    assert!(before.iter().any(|&(n, y)| n == sc && y > 0.0), "{before:?}");
    let doc_before = h.doc.scroll_offset(h.view.element());
    for (s, by) in [(3, "line-down"), (4, "page-down"), (5, "page-up"), (6, "end"), (7, "start")] {
        h.frame_ops("s1", s, json!([["scroll", "main", by]]));
        h.frame();
        assert_eq!(scroll_offsets(&h, main), before, "main ignores {by}");
        assert_eq!(h.doc.scroll_offset(h.view.element()), doc_before);
    }
    assert!(h.errors().is_empty(), "{:?}", h.errors());
    // The list itself still scrolls.
    h.frame_ops("s1", 8, json!([["scroll", "ls", "start"]]));
    h.frame();
    assert_eq!(h.doc.scroll_offset(sc).1, 0.0);

    // A screen surface's `main` is its own scroll container, and `scroll` steps it.
    let lines: Vec<_> = (0..200)
        .map(|i| json!({"id": format!("l{i}"), "k": "text", "p": {"text": format!("line {i}")}}))
        .collect();
    h.surface("s2", "screen", serde_json::Value::Array(lines));
    h.frame();
    let smain = h.view.region(key(&h, "s2"), Region::Main).unwrap();
    assert!(h.doc.computed_style(smain).unwrap().is_scroll_container());
    let (_, max) = h.doc.max_scroll(smain);
    assert!(max > 0.0, "the screen surface's main overflows and scrolls");
    h.frame_ops("s2", 2, json!([["scroll", "main", "end"]]));
    h.frame();
    assert!((h.doc.scroll_offset(smain).1 - max).abs() < 1.0);
    h.frame_ops("s2", 3, json!([["scroll", "main", "start"]]));
    h.frame();
    assert_eq!(h.doc.scroll_offset(smain).1, 0.0);
}

/// §8.3 virtualization thresholds, at their limits: a `col` virtualizes over 64 children, `code`
/// over 400 lines and a `list` over 200 items. At the limit everything lays out (the last row,
/// far below the view, included); one more and the rows far below the view are only estimated.
#[test]
fn virtualization_starts_just_past_64_col_children_400_code_lines_and_200_list_items() {
    type Make = fn(usize) -> serde_json::Value;
    let col: Make = |n| {
        json!({"id": "x", "k": "col", "c": (0..n).map(|i| json!({"id": format!("c{i}"), "k": "text", "p": {"text": format!("child {i}\nsecond line\nthird line")}})).collect::<Vec<_>>()})
    };
    let code: Make = |n| {
        let lines: Vec<String> = (0..n).map(|i| format!("line {i}")).collect();
        json!({"id": "x", "k": "code", "p": {"text": lines.join("\n"), "lang": "txt"}})
    };
    let list: Make = |n| {
        json!({"id": "x", "k": "list", "c": (0..n).map(|i| json!({"id": format!("i{i}"), "k": "item", "p": {"label": format!("item {i}")}})).collect::<Vec<_>>()})
    };
    let cases: [(&str, usize, Make, &str); 3] = [
        ("col", 64, col, "sf-text"),
        ("code", 400, code, "sf-ln"),
        ("list", 200, list, "sf-item"),
    ];
    for (name, limit, make, row_class) in cases {
        for n in [limit, limit + 1] {
            let mut h = Harness::new();
            h.hello(&[]);
            h.surface("s1", "inline", json!([make(n)]));
            h.frame();
            let x = h.el("s1", "x");
            let rows = h.find_class(x, row_class);
            assert_eq!(rows.len(), n, "{name} {n}: every row is there");
            let last = *rows.last().unwrap();
            let pane = h.doc.border_box(h.view.element()).unwrap();
            // Taller than the view and its overscan (half a view below it).
            assert!(pane.h > 1600.0, "{name} {n}: {} tall", pane.h);
            assert!(text_laid_out(&h, rows[0]), "{name} {n}: the first row lays out");
            if n == limit {
                assert!(
                    text_laid_out(&h, last),
                    "{name} at {n}: not virtualized, the last row lays out"
                );
            } else {
                assert!(
                    !text_laid_out(&h, last),
                    "{name} at {n}: virtualized, the last row is only estimated"
                );
            }
            // Copy and Find still see everything.
            assert!(h.text(x).contains(&format!("{}", n - 1)), "{name} {n}");
        }
    }
}

/// §8.3 Spacing: blocks in `main` are 16px apart (`--sf-block-gap`), and the parts inside a card
/// 8px apart (`--sf-inner-gap`), in the terminal's design units (the region's zoom scales them
/// with the font). Programs never send spacing.
#[test]
fn blocks_sit_16px_apart_and_a_cards_parts_8px_at_13px_and_scale_with_the_font() {
    for (font_px, cell) in [(13.0, (8.0, 17.0)), (26.0, (16.0, 34.0))] {
        let settings = ViewSettings {
            font_px,
            cell,
            ..Default::default()
        };
        let mut h = Harness::with(auk_tsp::Config::default(), settings);
        h.hello(&[]);
        h.surface(
            "s1",
            "inline",
            json!([
                {"id": "a", "k": "text", "p": {"text": "first block"}},
                {"id": "b", "k": "text", "p": {"text": "second block"}},
                {"id": "card", "k": "card", "p": {"head": "Card"}, "c": [
                    {"id": "p1", "k": "text", "p": {"text": "part one"}},
                    {"id": "p2", "k": "text", "p": {"text": "part two"}}
                ]}
            ]),
        );
        h.frame();
        let zoom = font_px / 13.0;
        let bx = |id: &str| h.doc.border_box(h.el("s1", id)).unwrap();
        let (a, b, card) = (bx("a"), bx("b"), bx("card"));
        let gap = |top: auk_ui::Rect, below: auk_ui::Rect| (below.y - (top.y + top.h)) / zoom;
        assert!((gap(a, b) - 16.0).abs() < 0.5, "block gap at {font_px}px: {}", gap(a, b));
        assert!((gap(b, card) - 16.0).abs() < 0.5, "{}", gap(b, card));
        let (p1, p2) = (bx("p1"), bx("p2"));
        assert!((gap(p1, p2) - 8.0).abs() < 0.5, "inner gap at {font_px}px: {}", gap(p1, p2));
        // The values come from the theme's variables.
        let main = h.view.region(key(&h, "s1"), Region::Main).unwrap();
        let style = h.doc.computed_style(main).unwrap();
        let var = |name: &str| style.custom.get(name).map(|v| v.trim().to_owned());
        assert_eq!(var("--sf-block-gap").as_deref(), Some("16px"));
        assert_eq!(var("--sf-inner-gap").as_deref(), Some("8px"));
    }
}

/// The toast the pane's toast layer shows, as text without its icon.
fn shown_toast(h: &Harness) -> Option<String> {
    let layer = h.find_class(h.view.element(), "toast");
    let t = *layer.first()?;
    let parts = h.doc.element_children(t);
    Some(parts[1..].iter().map(|&p| h.text(p)).collect())
}

#[test]
fn a_toast_shows_once_for_its_duration_even_after_its_surface_closes() {
    // omp 18.8.4's session close, as its own TSP record has it: the status toast is added to
    // `main` (with `ttl`), the frame settles it, and the surface closes with `keep` in the same
    // burst. Then omp starts again in the pane and keeps sending frames.
    let mut h = Harness::new();
    h.hello(&["edit", "undo", "send"]);
    h.surface(
        "s:1",
        "inline",
        json!([
            {"id": "g", "k": "col"},
            {"id": "p", "k": "text", "p": {"text": "Working"}}
        ]),
    );
    h.frame();
    let start = h.now;
    h.frame_ops(
        "s:1",
        2,
        json!([
            ["add", "q", "main", null, {"id": "q", "k": "toast",
                "p": {"text": "Closing session…", "ttl": 2400, "role": "omp.toast.status"}}],
            ["add", "r", "g", null, {"id": "r", "k": "col"}],
            ["del", "p"],
            ["settle", "q"]
        ]),
    );
    h.apc("x", &json!({"id": "s:1", "keep": true}).to_string());
    h.frame();
    assert_eq!(shown_toast(&h).as_deref(), Some("Closing session…"));
    // Its icon draws in the bundled Nerd Fonts face.
    let layer = h.find_class(h.view.element(), "toast");
    let icon = h.find_class(layer[0], "ico");
    assert_eq!(
        h.doc.text_font_families(icon[0]),
        [auk_text::NERD_SYMBOLS_FAMILY]
    );

    // omp again, sending frames every 100 ms: the old toast neither stays nor comes back.
    h.surface(
        "s:1",
        "inline",
        json!([{"id": "w", "k": "text", "p": {"text": "tick 0"}}]),
    );
    let mut seen_until = start;
    for i in 1..60u64 {
        h.now = start + 100.0 * i as f64;
        h.frame_ops(
            "s:1",
            1 + i,
            json!([["set", "w", {"text": format!("tick {i}")}]]),
        );
        h.frame();
        if shown_toast(&h).is_some() {
            seen_until = h.now;
        }
    }
    assert!(
        (start + 2300.0..start + 2400.0).contains(&seen_until),
        "the toast shows for its ttl (2.4 s), then goes: last seen {} ms in",
        seen_until - start
    );
}

#[test]
fn a_toast_resets_only_when_it_changes_and_one_asked_for_unseen_doesnt_show_late() {
    let mut h = Harness::new();
    h.hello(&[]);
    let start = h.now;
    h.surface(
        "s1",
        "inline",
        json!([{"id": "t", "k": "toast", "p": {"text": "Saved"}}]),
    );
    h.frame();
    assert_eq!(shown_toast(&h).as_deref(), Some("Saved"));
    // The same props again (a program re-sending its tree) doesn't restart the 2.2 s.
    h.now = start + 1500.0;
    h.frame_ops("s1", 2, json!([["set", "t", {"text": "Saved"}]]));
    h.frame();
    assert_eq!(shown_toast(&h).as_deref(), Some("Saved"));
    h.now = start + 2200.0;
    h.frame();
    assert_eq!(shown_toast(&h), None, "2.2 s from when it was asked for");
    // A change shows it again, for the error duration (5.2 s).
    h.frame_ops(
        "s1",
        3,
        json!([["set", "t", {"text": "Failed", "tone": "error"}]]),
    );
    h.frame();
    assert_eq!(shown_toast(&h).as_deref(), Some("Failed"));
    h.now += 5100.0;
    h.frame();
    assert_eq!(shown_toast(&h).as_deref(), Some("Failed"));
    h.now += 100.0;
    h.frame();
    assert_eq!(shown_toast(&h), None);
    // Asked for while the view's clock didn't run (a pane not drawn), then drawn 6 s later:
    // its time (5.2 s: `set` merges, so the tone is still `error`) is over; it never shows.
    let asked = h.now;
    h.frame_ops("s1", 4, json!([["set", "t", {"text": "Copied"}]]));
    h.now = asked + 6000.0;
    h.frame();
    assert_eq!(shown_toast(&h), None);
}

#[test]
fn a_sheet_in_the_layer_covers_main_and_dock_and_holds_their_motion() {
    let mut h = Harness::new();
    h.hello(&[]);
    h.surface(
        "s1",
        "inline",
        json!([{"id": "sp", "k": "spinner", "p": {"label": "Working"}}]),
    );
    h.frame_ops(
        "s1",
        2,
        json!([["add", "layer", "s1", null, {"id": "layer", "k": "col"}]]),
    );
    h.frame();
    let k = key(&h, "s1");
    let main = h.view.region(k, Region::Main).unwrap();
    let layer = h.view.region(k, Region::Layer).unwrap();
    let covered = |h: &Harness, el| h.class(el).split(' ').any(|c| c == "sf-covered");
    assert!(!covered(&h, main));
    // A modal overlay added into the layer's column covers main (not the layer).
    h.frame_ops(
        "s1",
        3,
        json!([["add", "ov", "layer", null, {"id": "ov", "k": "overlay", "p": {"modal": true}}]]),
    );
    h.frame();
    assert!(covered(&h, main), "{}", h.class(main));
    assert!(!covered(&h, layer));
    // The spinner holds its frame while covered.
    let sp = h.el("s1", "sp");
    let shown = |h: &Harness| h.text(sp) + &h.attr(sp, "style").unwrap_or_default();
    let before = shown(&h);
    for _ in 0..10 {
        h.now += 97.0;
        h.frame();
    }
    assert_eq!(shown(&h), before, "held under the sheet");
    // It goes with the sheet, and the spinner moves again.
    h.frame_ops("s1", 4, json!([["del", "ov"]]));
    h.frame();
    assert!(!covered(&h, main));
    let mut moved = false;
    for _ in 0..10 {
        h.now += 97.0;
        h.frame();
        moved |= shown(&h) != before;
    }
    assert!(moved, "moves once uncovered");
}
