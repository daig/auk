//! The kit fixes the omp sheets depend on (research/omp-stylesheets.md §2.5, K1-K9): computed
//! styles in light and dark. The pixel checks are in paint.rs.

mod common;

use auk_ui::css::values::Color;
use auk_ui::{ElementState, NodeId};
use auk_views::ViewSettings;
use common::Harness;
use serde_json::{Value, json};

fn harness(dark: bool) -> Harness {
    let mut h = Harness::with(
        Default::default(),
        ViewSettings {
            dark,
            ..Default::default()
        },
    );
    h.hello(&[]);
    h
}

/// One inline surface `s1` with `main`, laid out.
fn surface(dark: bool, main: Value) -> Harness {
    let mut h = harness(dark);
    h.surface("s1", "inline", main);
    h.frame();
    h
}

/// A theme token's colour as an element under the body resolves it.
fn token(h: &mut Harness, var: &str) -> Color {
    let probe = h.doc.create_element("span");
    h.doc.append_child(h.body, probe);
    h.doc.set_inline_style(probe, &format!("color: var({var})"));
    h.doc.update();
    let c = h.doc.computed_style(probe).unwrap().color;
    h.doc.remove(probe);
    h.doc.update();
    c
}

/// `color` mixed with transparent at `pct`% (`color-mix(in srgb, color pct%, transparent)`).
fn mixed(c: Color, pct: f32) -> Color {
    Color {
        a: c.a * pct / 100.0,
        ..c
    }
}

fn same(a: Color, b: Color) -> bool {
    let (x, y) = (a.to_rgba8(), b.to_rgba8());
    x.iter().zip(y).all(|(p, q)| p.abs_diff(q) <= 1)
}

fn color(h: &Harness, el: NodeId) -> Color {
    h.doc.computed_style(el).unwrap().color
}

fn background(h: &Harness, el: NodeId) -> Color {
    h.doc.computed_style(el).unwrap().background_color
}

fn shadows(h: &Harness, el: NodeId) -> Vec<auk_ui::style::Shadow> {
    h.doc
        .computed_style(el)
        .unwrap()
        .box_shadow
        .as_deref()
        .cloned()
        .unwrap_or_default()
}

/// K1: a tone colours a text (`--tc`); a neutral one reads as no tone and keeps its parent's
/// colour, and a span's own colour still wins inside a toned text.
#[test]
fn a_toned_text_takes_its_tone_colour() {
    for dark in [false, true] {
        let mut h = surface(
            dark,
            json!([
                {"id": "plain", "k": "text", "p": {"text": "plain"}},
                {"id": "err", "k": "text", "p": {"text": "stop", "tone": "error"}},
                {"id": "pend", "k": "text", "p": {"text": "reading", "tone": "pending"}},
                {"id": "neu", "k": "text", "p": {"text": "neutral", "tone": "neutral"}},
                {"id": "sp", "k": "text", "p": {"tone": "error", "spans": [{"t": "muted", "s": "muted"}, " rest"]}}
            ]),
        );
        let (plain, err, pend, neu) = (
            h.el("s1", "plain"),
            h.el("s1", "err"),
            h.el("s1", "pend"),
            h.el("s1", "neu"),
        );
        let (bad, live) = (token(&mut h, "--bad"), token(&mut h, "--live"));
        assert!(
            same(color(&h, err), bad),
            "dark {dark}: error text is --bad"
        );
        assert!(
            same(color(&h, pend), live),
            "dark {dark}: pending text is --live"
        );
        assert!(
            same(color(&h, neu), color(&h, plain)),
            "dark {dark}: neutral text keeps the body colour"
        );
        let muted = h.find_class(h.el("s1", "sp"), "sf-t-muted")[0];
        let want = token(&mut h, "--sf-c-muted");
        assert!(
            same(color(&h, muted), want),
            "dark {dark}: the span keeps its colour"
        );
    }
}

/// K2: an untoned (or neutral) meter fills with the accent, a level wins over that, and any
/// other tone wins over the level (book:8649, 8723).
#[test]
fn an_untoned_meter_fills_with_the_accent() {
    for dark in [false, true] {
        let mut h = surface(
            dark,
            json!([
                {"id": "plain", "k": "meter", "p": {"value": 0.4}},
                {"id": "neu", "k": "meter", "p": {"value": 0.4, "tone": "neutral"}},
                {"id": "warn", "k": "meter", "p": {"value": 0.8, "thresholds": {"warn": 0.7, "bad": 0.9}}},
                {"id": "bad", "k": "meter", "p": {"value": 0.95, "thresholds": {"warn": 0.7, "bad": 0.9}}},
                {"id": "ok", "k": "meter", "p": {"value": 0.95, "tone": "success", "thresholds": {"warn": 0.7, "bad": 0.9}}},
                {"id": "tbl", "k": "table", "p": {"cols": [{"id": "m", "head": "Use"}],
                 "rows": [{"id": "r0", "cells": {"m": {"meter": {"value": 0.5}}}}]}}
            ]),
        );
        let fill =
            |h: &Harness, id: &str| background(h, h.find_class(h.el("s1", id), "sf-mt-fill")[0]);
        let accent = token(&mut h, "--accent");
        let (warn, bad, ok) = (
            token(&mut h, "--warn"),
            token(&mut h, "--bad"),
            token(&mut h, "--ok"),
        );
        assert!(
            same(fill(&h, "plain"), accent),
            "dark {dark}: untoned is --accent"
        );
        assert!(
            same(fill(&h, "neu"), accent),
            "dark {dark}: neutral is --accent"
        );
        assert!(
            same(fill(&h, "warn"), warn),
            "dark {dark}: the warn level wins"
        );
        assert!(
            same(fill(&h, "bad"), bad),
            "dark {dark}: the bad level wins"
        );
        assert!(
            same(fill(&h, "ok"), ok),
            "dark {dark}: a tone wins over the level"
        );
        let cell = h.find_class(h.el("s1", "tbl"), "sf-mt-fill");
        assert!(!cell.is_empty(), "the table draws its meter cell");
        assert!(
            same(background(&h, cell[0]), accent),
            "dark {dark}: a table meter is --accent"
        );
    }
}

/// K4: a toned badge is a 10% tone fill with a 22% inset tone ring; untoned or neutral, a gray
/// chip (--t2 on --chip-bg with an --l2 ring) (book:7230-7263, 12306-12307).
#[test]
fn a_badge_has_a_tone_fill_and_ring() {
    for dark in [false, true] {
        let mut h = surface(
            dark,
            json!([
                {"id": "ok", "k": "badge", "p": {"text": "passed", "tone": "success"}},
                {"id": "plain", "k": "badge", "p": {"text": "untoned"}},
                {"id": "neu", "k": "badge", "p": {"text": "neutral", "tone": "neutral"}}
            ]),
        );
        let okc = token(&mut h, "--ok");
        let ok = h.el("s1", "ok");
        assert!(same(color(&h, ok), okc));
        assert!(
            same(background(&h, ok), mixed(okc, 10.0)),
            "dark {dark}: 10% fill"
        );
        let ring = shadows(&h, ok);
        assert_eq!(ring.len(), 1, "one ring");
        let r = &ring[0];
        assert!(
            r.inset && r.x == 0.0 && r.y == 0.0 && r.blur == 0.0 && r.spread == 1.0,
            "{r:?}"
        );
        assert!(same(r.color, mixed(okc, 22.0)), "dark {dark}: 22% ring");
        let (t2, chip, l2) = (
            token(&mut h, "--t2"),
            token(&mut h, "--chip-bg"),
            token(&mut h, "--l2"),
        );
        for id in ["plain", "neu"] {
            let el = h.el("s1", id);
            assert!(same(color(&h, el), t2), "dark {dark}: {id} text --t2");
            assert!(
                same(background(&h, el), chip),
                "dark {dark}: {id} on --chip-bg"
            );
            let ring = shadows(&h, el);
            assert!(
                ring.len() == 1 && ring[0].inset && same(ring[0].color, l2),
                "dark {dark}: {id} --l2 ring"
            );
        }
    }
}

/// K5: an inline tool's body is indented behind a hairline under the head's icon (book:9853);
/// a card tool's body has none. The body text stays where it was (34px in).
#[test]
fn an_inline_tool_body_sits_behind_a_hairline() {
    let mut h = surface(
        false,
        json!([
            {"id": "in", "k": "tool", "p": {"name": "read", "title": "Read", "status": "done", "frame": "inline"},
             "c": [{"id": "in.out", "k": "text", "p": {"text": "file contents"}}]},
            {"id": "card", "k": "tool", "p": {"name": "bash", "title": "Run", "status": "done"},
             "c": [{"id": "card.out", "k": "text", "p": {"text": "output"}}]}
        ]),
    );
    let l2 = token(&mut h, "--l2");
    let tool = h.el("s1", "in");
    assert!(h.class(tool).contains("inline"));
    let body = h.find_class(tool, "sf-tool-body")[0];
    let st = h.doc.computed_style(body).unwrap();
    assert_eq!(st.border_width[3], 1.0, "a 1px hairline on the left");
    assert!(same(st.border_color[3], l2), "in --l2");
    let tb = h.doc.border_box(tool).unwrap();
    let bb = h.doc.border_box(body).unwrap();
    let ic = h
        .doc
        .border_box(h.find_class(tool, "sf-tool-ic")[0])
        .unwrap();
    assert!(
        bb.x >= ic.x && bb.x < ic.x + ic.w,
        "the hairline at {} is under the icon {ic:?}",
        bb.x
    );
    // The body text stays where a card tool's is: 34px inside the (transparent) 1px border.
    let text = h.doc.border_box(h.el("s1", "in.out")).unwrap();
    let card_text = h.doc.border_box(h.el("s1", "card.out")).unwrap();
    assert!(
        (text.x - tb.x - 35.0).abs() < 0.5,
        "body text 34px in: {}",
        text.x - tb.x
    );
    assert!(
        (text.x - card_text.x).abs() < 0.5,
        "as in a card: {text:?} {card_text:?}"
    );
    let card_body = h.find_class(h.el("s1", "card"), "sf-tool-body")[0];
    let st = h.doc.computed_style(card_body).unwrap();
    assert_eq!(
        format!("{:?}", st.border_style[3]),
        "None",
        "no hairline on a card"
    );
}

/// K6: a running tool's glyph is a ring turning (an infinite animation), held under Reduce Motion
/// and while the pane is hidden; a cancelled tool shows a slashed circle (book:9856-9858).
#[test]
fn a_running_tool_spins_a_ring_and_a_cancelled_one_shows_a_slashed_circle() {
    let tools = json!([
        {"id": "run", "k": "tool", "p": {"name": "bash", "title": "Run", "status": "running"}},
        {"id": "stop", "k": "tool", "p": {"name": "bash", "title": "Run", "status": "cancelled"}},
        {"id": "done", "k": "tool", "p": {"name": "bash", "title": "Run", "status": "done"}}
    ]);
    let mut h = surface(false, tools.clone());
    let live = token(&mut h, "--live");
    let spin = h.find_class(h.el("s1", "run"), "sf-tool-spin");
    assert_eq!(spin.len(), 1);
    let st = h.doc.computed_style(spin[0]).unwrap();
    let r = h.doc.border_box(spin[0]).unwrap();
    assert!(
        (r.w - 12.0).abs() < 0.5 && (r.h - 12.0).abs() < 0.5,
        "a 12px ring: {r:?}"
    );
    assert_eq!(st.border_width, [2.0; 4]);
    assert!(
        same(st.border_color[1], live) && st.border_color[0].a == 0.0,
        "an open --live ring"
    );
    assert!(
        same(st.background_color, Color::TRANSPARENT),
        "a ring, not a dot"
    );
    let anim = st.animations.as_deref().expect("it turns");
    assert!(anim[0].name.as_ref() == "sf-tool-spin" && anim[0].iterations.is_infinite());
    let moving = |h: &Harness| {
        h.doc
            .computed_style(spin[0])
            .unwrap()
            .animations
            .as_deref()
            .is_some_and(|a| a.iter().any(|a| a.duration > 0.0))
    };
    h.view.set_visible(&mut h.doc, &h.pane, false);
    h.frame();
    assert!(!moving(&h), "held while the pane is hidden");
    h.view.set_visible(&mut h.doc, &h.pane, true);
    h.frame();
    assert!(moving(&h), "turning again once shown");
    let stop = h.find_class(h.el("s1", "stop"), "sf-tool-st")[0];
    let glyph = auk_views::icons::glyph("slash-circle").unwrap().to_string();
    assert_eq!(h.text(stop), glyph, "a slashed circle");
    assert!(h.find_class(h.el("s1", "done"), "sf-tool-spin").is_empty());
    // Reduce Motion: the ring holds still.
    let mut h = Harness::with(
        Default::default(),
        ViewSettings {
            reduce_motion: true,
            ..Default::default()
        },
    );
    h.hello(&[]);
    h.surface("s1", "inline", tools);
    h.frame();
    let spin = h.find_class(h.el("s1", "run"), "sf-tool-spin")[0];
    let anim = h.doc.computed_style(spin).unwrap().animations.clone();
    assert!(
        anim.as_deref()
            .is_none_or(|a| a.iter().all(|a| a.duration == 0.0)),
        "still under Reduce Motion: {anim:?}"
    );
}

/// K7: the overlay card is glass without blur: 12px corners on --pop-bg, an --l2 ring, a
/// --glass-hi top highlight and a drop shadow, deeper in dark.
#[test]
fn an_overlay_card_is_glass() {
    for dark in [false, true] {
        let mut h = harness(dark);
        h.surface(
            "s1",
            "inline",
            json!([{"id": "t", "k": "text", "p": {"text": "under"}}]),
        );
        h.frame_ops(
            "s1",
            2,
            json!([["add", "layer", "s1", null, {"id": "layer", "k": "col", "c": [
                {"id": "ov", "k": "overlay", "p": {"head": "Usage"}, "c": [
                    {"id": "b", "k": "text", "p": {"text": "body"}}
                ]}
            ]}]]),
        );
        h.frame();
        let card = h.find_class(h.el("s1", "ov"), "sf-ov-card")[0];
        let (pop, l2, hi) = (
            token(&mut h, "--pop-bg"),
            token(&mut h, "--l2"),
            token(&mut h, "--glass-hi"),
        );
        let st = h.doc.computed_style(card).unwrap();
        assert!(same(st.background_color, pop), "dark {dark}: --pop-bg");
        assert!(
            st.border_width == [1.0; 4] && same(st.border_color[0], l2),
            "dark {dark}: --l2 ring"
        );
        let r = &st.border_radius[0].0;
        assert_eq!(
            format!("{r:?}"),
            format!("{:?}", auk_ui::css::values::LengthPct::Px(12.0)),
            "12px corners"
        );
        let sh = shadows(&h, card);
        assert_eq!(sh.len(), 2, "dark {dark}: highlight and shadow: {sh:?}");
        assert!(
            sh[0].inset && sh[0].y == 1.0 && sh[0].blur == 0.0 && same(sh[0].color, hi),
            "dark {dark}: {:?}",
            sh[0]
        );
        let (y, blur, a) = if dark {
            (20.0, 60.0, 0.5)
        } else {
            (16.0, 48.0, 0.16)
        };
        assert!(
            !sh[1].inset
                && sh[1].y == y
                && sh[1].blur == blur
                && same(
                    sh[1].color,
                    Color {
                        r: 0.0,
                        g: 0.0,
                        b: 0.0,
                        a
                    }
                ),
            "dark {dark}: {:?}",
            sh[1]
        );
    }
}

fn toast_layer(h: &Harness) -> NodeId {
    h.find_class(h.view.element(), "auk-toasts")[0]
}

/// K8: the toast has 12px corners and wraps past 560px (or the pane less 32px); it sits 28px
/// from the bottom, and 12px above the dock while the dock holds a composer.
#[test]
fn a_toast_wraps_and_rises_above_the_dock() {
    let mut h = harness(false);
    let long = "A notice long enough to wrap: ".repeat(6);
    h.surface(
        "s1",
        "inline",
        json!([{"id": "t", "k": "toast", "p": {"text": long}}]),
    );
    h.frame();
    let toast = h.find_class(toast_layer(&h), "toast")[0];
    let st = h.doc.computed_style(toast).unwrap();
    assert_eq!(
        format!("{:?}", st.border_radius[0].0),
        format!("{:?}", auk_ui::css::values::LengthPct::Px(12.0))
    );
    let r = h.doc.border_box(toast).unwrap();
    assert!((r.w - 560.0).abs() < 0.5, "capped at 560px: {r:?}");
    assert!(r.h > 30.0, "it wraps: {r:?}");
    let pane = h.doc.border_box(h.view.element()).unwrap();
    let layer = h.doc.border_box(toast_layer(&h)).unwrap();
    assert!(
        (pane.y + pane.h - (layer.y + layer.h) - 28.0).abs() < 0.5,
        "28px from the bottom"
    );
    // A narrow pane: the pane's width less 32px.
    h.doc.set_viewport(400.0, 800.0);
    h.frame();
    let r = h.doc.border_box(toast).unwrap();
    assert!((r.w - 368.0).abs() < 0.5, "400 - 32: {r:?}");
    // A composer in the dock: the toast rises to 12px above it.
    h.frame_ops(
        "s1",
        2,
        json!([["add", "dock", "s1", null, {"id": "dock", "k": "col", "c": [{"id": "ed", "k": "editor"}]}]]),
    );
    h.frame();
    let dock = h
        .doc
        .border_box(h.view.region(key(&h), auk_views::Region::Dock).unwrap())
        .unwrap();
    assert!(dock.h > 10.0, "the dock shows: {dock:?}");
    let layer = h.doc.border_box(toast_layer(&h)).unwrap();
    assert!(
        (dock.y - (layer.y + layer.h) - 12.0).abs() < 0.5,
        "12px above the dock: {layer:?} {dock:?}"
    );
    // The dock emptied: back to 28px from the bottom.
    h.frame_ops("s1", 3, json!([["del", "dock"]]));
    h.frame();
    let pane = h.doc.border_box(h.view.element()).unwrap();
    let layer = h.doc.border_box(toast_layer(&h)).unwrap();
    assert!(
        (pane.y + pane.h - (layer.y + layer.h) - 28.0).abs() < 0.5,
        "28px again: {layer:?} {pane:?}"
    );
}

fn key(h: &Harness) -> auk_tsp::SurfaceKey {
    h.pane
        .surfaces()
        .iter()
        .rev()
        .find(|s| s.id() == "s1")
        .unwrap()
        .key()
}

/// K9: a clickable status segment gets an --l1 fill (and --t1 text) on hover (book:9460-9462).
#[test]
fn a_clickable_seg_fills_on_hover() {
    for dark in [false, true] {
        let mut h = surface(
            dark,
            json!([{"id": "bar", "k": "status", "c": [
                {"id": "model", "k": "seg", "p": {"text": "opus", "actions": {"click": "model"}}},
                {"id": "info", "k": "seg", "p": {"text": "info"}}
            ]}]),
        );
        let (l1, t1) = (token(&mut h, "--l1"), token(&mut h, "--t1"));
        let model = h.el("s1", "model");
        assert!(h.class(model).contains("sf-act"));
        assert!(
            same(background(&h, model), Color::TRANSPARENT),
            "no fill at rest"
        );
        h.doc.set_state(model, ElementState::HOVER, true);
        h.doc.update();
        assert!(
            same(background(&h, model), l1),
            "dark {dark}: --l1 on hover"
        );
        assert!(same(color(&h, model), t1), "dark {dark}: --t1 on hover");
        let info = h.el("s1", "info");
        h.doc.set_state(info, ElementState::HOVER, true);
        h.doc.update();
        assert!(
            same(background(&h, info), Color::TRANSPARENT),
            "a plain seg has no hover fill"
        );
    }
}

/// An item's label keeps its width when the detail runs long: the detail (and only it) ends in
/// an ellipsis, and a short detail still sits at the row's end. A label too long for the row is
/// capped so its detail stays visible.
#[test]
fn a_long_item_detail_truncates_before_the_label() {
    let long = "Plan, run, inspect, import, and compare things ".repeat(6);
    let mut h = surface(
        true,
        json!([{"id": "l", "k": "list", "c": [
            {"id": "short", "k": "item", "p": {"label": "security", "detail": "x"}},
            {"id": "long", "k": "item", "p": {"label": "security", "detail": long}},
            {"id": "valued", "k": "item", "p": {"label": "security", "detail": long, "value": "off"}},
            {"id": "huge", "k": "item", "p": {"label": long, "detail": "detail"}}
        ]}]),
    );
    let part = |h: &Harness, id: &str, class: &str| {
        let el = h.el("s1", id);
        h.doc.border_box(h.find_class(el, class)[0]).expect("laid out")
    };
    let natural = part(&h, "short", "sf-item-label").w;
    for id in ["long", "valued"] {
        let label = part(&h, id, "sf-item-label");
        assert!(
            (label.w - natural).abs() < 0.5,
            "{id}: the label keeps its width ({} vs {natural})",
            label.w
        );
        let row = h.doc.border_box(h.el("s1", id)).unwrap();
        let detail = part(&h, id, "sf-item-detail");
        assert!(detail.x + detail.w <= row.x + row.w, "{id}: the detail shrinks to fit");
    }
    let row = h.doc.border_box(h.el("s1", "short")).unwrap();
    let detail = part(&h, "short", "sf-item-detail");
    assert!(
        row.x + row.w - (detail.x + detail.w) < 12.0,
        "a short detail sits at the row's end"
    );
    let row = h.doc.border_box(h.el("s1", "huge")).unwrap();
    let label = part(&h, "huge", "sf-item-label");
    assert!(label.w <= row.w * 0.61, "a huge label is capped ({} of {})", label.w, row.w);
    assert!(part(&h, "huge", "sf-item-detail").w > 20.0, "and its detail stays visible");
}
