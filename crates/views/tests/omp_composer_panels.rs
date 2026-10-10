//! omp's composer and panel sheets (amendment A24; research/omp-stylesheets.md §9-10, §16.5-16.6,
//! §16.9): the composer's frame, field, context hairline, bar and fold; the overlay forms, the
//! Usage sheet and the overlay head icons (K10). Recorded frames where omp sent them, constructed
//! ones otherwise.

mod common;

use auk_ui::{Document, ElementState, NodeId};
use auk_views::icons::glyph;
use common::*;
use serde_json::{Value, json};

/// A pane with an omp session surface whose regions hold `main`, `dock` and `layer`.
fn omp_regions(main: Value, dock: Value, layer: Value, dark: bool) -> Harness {
    let mut h = Harness::with(
        Default::default(),
        auk_views::ViewSettings {
            dark,
            ..Default::default()
        },
    );
    h.hello(&[]);
    h.apc("o", r#"{"id":"s1","mode":"flow","role":"omp.session"}"#);
    let frame = json!({"sf": "s1", "s": 1, "ops": [
        ["add", "main", "s1", null, {"id": "main", "k": "col", "c": main}],
        ["add", "dock", "s1", null, {"id": "dock", "k": "col", "c": dock}],
        ["add", "layer", "s1", null, {"id": "layer", "k": "col", "c": layer}]]});
    h.apc("f", &frame.to_string());
    h.frame();
    assert!(h.errors().is_empty(), "{:?}", h.errors());
    h
}

/// What `value` (a token, e.g. `var(--l1)`) resolves to as a background where `el` sits. The
/// probe is an in-flow child of `el` while it lives (in a flex container with a gap, one more
/// gap), so the layout is brought back up to date after it goes.
fn token(h: &mut Harness, el: NodeId, value: &str) -> [u8; 4] {
    let probe = h.doc.create_element("div");
    h.doc.append_child(el, probe);
    h.doc
        .set_attribute(probe, "style", &format!("background-color: {value}"));
    h.doc.update();
    let c = h
        .doc
        .computed_style(probe)
        .unwrap()
        .background_color
        .to_rgba8();
    h.doc.remove(probe);
    h.doc.update();
    c
}

fn bg(h: &Harness, el: NodeId) -> [u8; 4] {
    h.doc
        .computed_style(el)
        .unwrap()
        .background_color
        .to_rgba8()
}

fn fg(h: &Harness, el: NodeId) -> [u8; 4] {
    h.doc.computed_style(el).unwrap().color.to_rgba8()
}

fn var(h: &Harness, el: NodeId, name: &str) -> String {
    let st = h.doc.computed_style(el).unwrap();
    st.custom
        .get(name)
        .map(|v| v.trim().to_owned())
        .unwrap_or_default()
}

/// A px custom property as a number.
fn var_px(h: &Harness, el: NodeId, name: &str) -> f32 {
    let v = var(h, el, name);
    v.trim_end_matches("px")
        .parse()
        .unwrap_or_else(|_| panic!("{name} = {v:?}"))
}

/// The first box-shadow's colour, if any.
fn ring(h: &Harness, el: NodeId) -> Option<[u8; 4]> {
    let st = h.doc.computed_style(el).unwrap();
    st.box_shadow.as_ref().map(|s| s[0].color.to_rgba8())
}

fn display(h: &Harness, el: NodeId) -> String {
    format!("{:?}", h.doc.computed_style(el).unwrap().display)
}

/// The `[top, right, bottom, left]` padding, as written by the engine (`Px(n)`).
fn padding(h: &Harness, el: NodeId) -> Vec<String> {
    h.doc
        .computed_style(el)
        .unwrap()
        .padding
        .iter()
        .map(|p| format!("{p:?}"))
        .collect()
}

fn one(h: &Harness, el: NodeId, class: &str) -> NodeId {
    let found = h.find_class(el, class);
    assert_eq!(found.len(), 1, "one .{class}");
    found[0]
}

/// Hover as the pointer sets it: the element and its ancestors.
fn hover(h: &mut Harness, el: NodeId, on: bool) {
    let mut n = Some(el);
    while let Some(e) = n {
        if h.doc.node(e).is_element() {
            h.doc.set_state(e, ElementState::HOVER, on);
        }
        n = h.doc.parent(e);
    }
    h.doc.update();
    h.view.layout(&mut h.doc);
}

/// Replays a recording, running `at` after each listed line (1-based) is fed.
fn replay_at(name: &str, lines: &[usize], mut at: impl FnMut(&mut Harness, usize)) -> Harness {
    let mut line = 0;
    replay(name, |h, _| {
        line += 1;
        if lines.contains(&line) {
            at(h, line);
        }
    })
}

/// Every probed node: no transition and no animation of the omp sheets' own (T6.2).
fn still(h: &Harness, el: NodeId) {
    let st = h.doc.computed_style(el).unwrap();
    assert!(st.transitions.is_none(), "{:?}", st.transitions);
    assert!(st.animations.is_none(), "{:?}", st.animations);
}

/// T4.1, T4.6 and Q-C1 on omp's recorded session: the composer is a ring with no fill whose
/// field draws no well; the ring takes the accent while the field has focus; it sits in the
/// transcript's column; and when the pane loses focus it folds to one line, then unfolds.
#[test]
fn the_composer_is_a_ring_that_folds_when_the_pane_loses_focus() {
    replay_at("omp-session.jsonl", &[5], |h, _| {
        h.view.layout(&mut h.doc);
        let c = h.el("s:1", "3");
        let st = h.doc.computed_style(c).unwrap();
        assert_eq!(format!("{:?}", st.border_radius[0].0), "Px(12.0)");
        assert_eq!(bg(h, c)[3], 0, "no fill");
        let ring_focused = ring(h, c).expect("a ring");
        let l2 = token(h, c, "var(--l2)");
        assert_ne!(ring_focused, l2, "the focused field tints the ring");
        let field = h.el("s:1", "3.line/input");
        let st = h.doc.computed_style(field).unwrap();
        assert_eq!(st.background_color.to_rgba8()[3], 0);
        assert!(st.border_color.iter().all(|b| b.to_rgba8()[3] == 0));
        still(h, c);
        still(h, field);

        // Q-C1: the dock follows the transcript's column.
        let welcome = h.doc.border_box(h.el("s:1", "1")).unwrap();
        let frame = h.doc.border_box(c).unwrap();
        assert!((welcome.x - frame.x).abs() <= 1.0, "{welcome:?} {frame:?}");
        assert!((welcome.w - frame.w).abs() <= 1.0, "{welcome:?} {frame:?}");

        let lh = var_px(h, c, "--sf-lh");
        let open = frame.h;
        let (bar, ctx) = (h.el("s:1", "3.bar"), h.el("s:1", "3.context"));
        // The frame ends at the bar and its 4px bottom padding: the absolutely placed context
        // hairline adds no gap.
        let bar_box = h.doc.border_box(bar).unwrap();
        assert_eq!(bar_box.y + bar_box.h + 4.0, frame.y + frame.h, "{bar_box:?} {frame:?}");
        h.view.set_focused(&mut h.doc, &h.pane, false);
        h.view.layout(&mut h.doc);
        assert_eq!(display(h, bar), "None");
        assert_eq!(display(h, ctx), "None");
        let folded = h.doc.border_box(c).unwrap().h;
        assert!(folded <= lh + 8.5, "one line: {folded} (lh {lh})");
        let l1 = token(h, c, "var(--l1)");
        assert_eq!(ring(h, c), Some(l1), "a quieter ring");
        h.view.set_focused(&mut h.doc, &h.pane, true);
        h.view.layout(&mut h.doc);
        assert_ne!(display(h, bar), "None");
        let again = h.doc.border_box(c).unwrap().h;
        assert!((again - open).abs() < 0.5, "{again} vs {open}");
        assert_eq!(ring(h, c), Some(ring_focused));
    });
}

/// T4.2, T4.4 and T4.7: while omp runs the ring takes the live colour, Stop is a red quiet
/// control, the working row's separator is faint and the activity line spaced; afterwards the
/// ring returns.
#[test]
fn a_running_turn_tints_the_ring_and_shows_a_red_stop() {
    let mut rest = None;
    replay_at("omp-session.jsonl", &[5, 17, 661], |h, line| {
        let c = h.el("s:1", "3");
        if line == 661 {
            assert_eq!(h.attr(c, "data-tone"), None, "the turn ended");
            assert_eq!(ring(h, c), rest, "the ring returns");
            return;
        }
        if line == 5 {
            rest = ring(h, c);
            for id in ["3.bar/model", "3.bar/usage"] {
                let el = h.el("s:1", id);
                let r = h.doc.border_box(el).unwrap();
                assert!((r.h - 20.0).abs() < 0.5, "{id}: {r:?}");
                let l1 = token(h, el, "var(--l1)");
                hover(h, el, true);
                assert_eq!(bg(h, el), l1, "{id} hovered");
                hover(h, el, false);
            }
            return;
        }
        let pending = ring(h, c).expect("a ring");
        assert_ne!(Some(pending), rest, "the live colour");
        let stop = h.el("s:1", "3.bar/stop");
        // Tone error: the palette's error colour, else --bad (K1).
        let bad = token(h, stop, "var(--sf-bad, var(--bad))");
        assert_eq!(fg(h, stop), bad);
        let rest_fill = bg(h, stop);
        assert!(rest_fill[3] > 0, "a red tint at rest");
        hover(h, stop, true);
        assert!(bg(h, stop)[3] > rest_fill[3], "deeper on hover");
        assert_eq!(fg(h, stop), bad);
        hover(h, stop, false);
        let sep = h.el("s:1", "v.sep");
        let t4 = token(h, sep, "var(--t4)");
        assert_eq!(fg(h, sep), t4);
        let act = h.el("s:1", "e");
        assert_eq!(
            format!("{:?}", h.doc.computed_style(act).unwrap().column_gap),
            "Some(Px(10.0))"
        );
        still(h, stop);
        still(h, act);
    });
}

/// T4.3 and Q-C2: the context meter is a 2px hairline on the frame's top edge, its share a
/// legend cut out of the edge with the page colour; the total hides.
#[test]
fn the_context_share_is_a_hairline_on_the_frames_top_edge_with_a_legend() {
    replay_at("omp-session.jsonl", &[661], |h, _| context_hairline(h));
}

fn context_hairline(h: &mut Harness) {
    let c = h.el("s:1", "3");
    let ctx = h.el("s:1", "3.context");
    assert_eq!(
        format!("{:?}", h.doc.computed_style(ctx).unwrap().position),
        "Absolute"
    );
    let frame = h.doc.border_box(c).unwrap();
    let track = one(h, ctx, "sf-mt-track");
    let t = h.doc.border_box(track).unwrap();
    assert!((t.h - 2.0).abs() < 0.5, "{t:?}");
    assert!(
        (t.y + t.h / 2.0 - frame.y).abs() <= 3.0,
        "on the edge: {t:?} {frame:?}"
    );
    assert_eq!(display(h, one(h, ctx, "sf-mt-total")), "None");
    let lbl = one(h, ctx, "sf-mt-lbl");
    assert!(h.text(lbl).ends_with('%'), "{}", h.text(lbl));
    let l = h.doc.border_box(lbl).unwrap();
    assert!(
        (l.y + l.h / 2.0 - frame.y).abs() <= 3.0,
        "the legend sits on the edge: {l:?} {frame:?}"
    );
    assert!(l.x + l.w <= frame.x + frame.w, "inside the frame's width");
    let page = token(h, c, "var(--tv-bg)");
    assert_eq!(bg(h, lbl), page, "cut out of the ring");
    let fill = one(h, ctx, "sf-mt-fill");
    let accent = token(h, ctx, "var(--accent)");
    assert_eq!(bg(h, fill), accent, "untoned: the accent (K2)");
}

/// T4.1 (the dialog) and T4.5: an omp.editor in the layer draws no frame; in the dock, vim's
/// insert mode shows no chip and normal mode a block caret; a plain editor keeps its chip.
#[test]
fn vim_modes_in_the_composer_and_no_frame_outside_the_dock() {
    let editor = |id: &str, mode: &str| json!({"id": id, "k": "editor", "p": {"text": "hi", "cursor": 1, "mode": mode}});
    let composer = |id: &str, mode: &str| {
        json!({"id": id, "k": "col", "p": {"role": "omp.editor"}, "c": [
            {"id": format!("{id}.line"), "k": "row", "p": {"role": "omp.composer.line"},
             "c": [editor(&format!("{id}.ed"), mode)]}]})
    };
    let mut h = omp_regions(
        json!([editor("plain", "insert")]),
        json!([composer("c", "normal")]),
        json!([{"id": "ask", "k": "overlay", "c": [composer("d", "insert")]}]),
        true,
    );
    h.frame_ops("s1", 2, json!([["focus", "c.ed"]]));
    h.frame();
    let d = h.el("s1", "d");
    assert_eq!(ring(&h, d), None, "no frame in the layer");
    assert!(ring(&h, h.el("s1", "c")).is_some());

    let ed = h.el("s1", "c.ed");
    let caret = h.find_class(ed, "sf-caret");
    assert!(!caret.is_empty(), "a caret");
    let cw = var_px(&h, ed, "--sf-cw");
    let w = h.doc.border_box(caret[0]).unwrap().w;
    assert!((w - cw).abs() < 0.5, "a block caret: {w} vs {cw}");
    assert_ne!(
        display(&h, one(&h, ed, "sf-ed-mode")),
        "None",
        "NORMAL shows"
    );
    let insert = one(&h, h.el("s1", "d.ed"), "sf-ed-mode");
    assert_eq!(display(&h, insert), "None", "INSERT is the resting mode");
    let plain = one(&h, h.el("s1", "plain"), "sf-ed-mode");
    assert_ne!(display(&h, plain), "None", "outside omp the kit chip stays");
}

/// T6.1 for the composer: its ring and the focused tint differ between themes, and are tokens.
#[test]
fn the_composer_ring_follows_the_theme() {
    let mut rings = Vec::new();
    for dark in [false, true] {
        let mut h = omp_regions(
            json!([]),
            json!([{"id": "c", "k": "col", "p": {"role": "omp.editor"}, "c": [
                {"id": "c.line", "k": "row", "p": {"role": "omp.composer.line"},
                 "c": [{"id": "c.ed", "k": "editor", "p": {"text": ""}}]}]}]),
            json!([]),
            dark,
        );
        let c = h.el("s1", "c");
        let l2 = token(&mut h, c, "var(--l2)");
        assert_eq!(ring(&h, c), Some(l2), "dark {dark}: --l2 at rest");
        h.frame_ops("s1", 2, json!([["focus", "c.ed"]]));
        h.frame();
        let focused = ring(&h, c).unwrap();
        assert_ne!(focused, l2, "dark {dark}");
        rings.push((l2, focused));
    }
    assert_ne!(rings[0].0, rings[1].0);
    assert_ne!(rings[0].1, rings[1].1);
}

/// T4.8: the autocomplete is 480px wide (omp's recorded `/model` completion).
#[test]
fn the_autocomplete_is_480px_wide() {
    replay_at("omp-picker.jsonl", &[15], |h, _| {
        let ov = h.el("s:1", "3.^complete");
        let card = one(h, ov, "sf-ov-card");
        let w = h.doc.border_box(card).unwrap().w;
        assert!((w - 480.0).abs() < 0.5, "{w}");
    });
}

/// Paints `doc` (1 CSS px per pixel) over white and reads back its RGBA pixels.
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

/// T5.1 and T6.1 on omp's recorded model picker (form B): the card in the overlay draws no
/// frame of its own, so the sheet shows one ring edge; in both themes.
#[test]
fn an_overlay_card_draws_no_second_frame() {
    for dark in [true, false] {
        replay_at("omp-app.jsonl", &[20], |h, _| {
            auk_views::sheets::set_theme(&mut h.doc, dark);
            // Past the sheet's pop-in.
            h.now += 1000.0;
            h.frame();
            let v = h.el("s:1", "v");
            let st = h.doc.computed_style(v).unwrap();
            assert!(st.border_color.iter().all(|b| b.to_rgba8()[3] == 0));
            assert_eq!(st.background_color.to_rgba8()[3], 0);
            let body = one(h, v, "sf-card-body");
            assert_eq!(padding(h, body)[3], "Px(0.0)");
            still(h, v);

            let ov = h.el("s:1", "layer.ov-v");
            let card = h.doc.border_box(one(h, ov, "sf-ov-card")).unwrap();
            let head = h.doc.border_box(one(h, v, "sf-card-head")).unwrap();
            let inner = h.doc.border_box(v).unwrap();
            assert!(inner.x > card.x + 4.0, "{inner:?} inside {card:?}");
            let (w, hh) = (960u32, 800u32);
            let px = render(&h.doc, w, hh);
            // Along the blank strip under the head, from just inside the sheet's ring to well
            // past where the card's own border was: one colour, the sheet's.
            let y = head.y + head.h - 2.0;
            let base = pixel(&px, w, card.x + 3.0, y);
            for x in (card.x + 3.0) as u32..(inner.x + 24.0) as u32 {
                let p = pixel(&px, w, x as f32, y);
                assert!(
                    p.iter().zip(base).all(|(a, b)| a.abs_diff(b) <= 3),
                    "dark {dark}: an edge at x={x} ({p:?} vs {base:?})"
                );
            }
            let edge = (card.x - 1.0) as u32..=(card.x + 1.0) as u32;
            assert!(
                edge.clone().any(|x| pixel(&px, w, x as f32, y)
                    .iter()
                    .zip(base)
                    .any(|(a, b)| a.abs_diff(b) > 3)),
                "dark {dark}: the sheet's own ring at {edge:?}"
            );
        });
    }
}

/// T5.2, T5.3 and K10 on omp's recorded Usage sheet (form C): roomier sheet padding, a chart
/// head icon, the provider in a well, and the share column 9 cells wide, right-aligned.
#[test]
fn the_usage_sheet_has_room_a_chart_icon_and_aligned_columns() {
    replay_at("omp-app.jsonl", &[16], |h, _| {
        let ov = h.el("s:1", "layer.ov-u");
        let body = one(h, ov, "sf-ov-body");
        assert_eq!(
            padding(h, body),
            ["Px(8.0)", "Px(16.0)", "Px(14.0)", "Px(16.0)"]
        );
        let ic = one(h, ov, "sf-ov-ic");
        assert!(!h.class(ic).contains("sf-hidden"));
        assert_eq!(h.text(ic), glyph("chart").unwrap().to_string());

        let pct = h.el("s:1", "u.overview/providers/xai-oauth/w0/2");
        let cw = var_px(h, pct, "--sf-cw");
        let w = h.doc.border_box(pct).unwrap().w;
        assert!((w - 9.0 * cw).abs() < 0.5, "{w} vs 9 × {cw}");
        assert_eq!(
            format!("{:?}", h.doc.computed_style(pct).unwrap().text_align),
            "Right"
        );
        let reset = h.el("s:1", "u.overview/providers/xai-oauth/w0/3");
        assert_eq!(h.doc.computed_style(reset).unwrap().font_size, 11.0);
        let card = h.el("s:1", "u.overview/providers/xai-oauth");
        let well = token(h, card, "var(--well)");
        assert_eq!(bg(h, card), well);
        still(h, card);
        still(h, pct);
    });
}

/// The Usage sheet's windows stay one line however the pointer moves (A25's jump): every
/// element of the sheet hovered in turn leaves every window row, label, share and reset at one
/// line and where it was.
#[test]
fn hovering_the_usage_sheet_never_wraps_or_moves_a_window() {
    replay_at("omp-app.jsonl", &[16], |h, _| {
        let ov = h.el("s:1", "layer.ov-u");
        let lh = var_px(h, ov, "--sf-lh");
        let windows: Vec<NodeId> = (0..3)
            .map(|i| h.el("s:1", &format!("u.overview/providers/xai-oauth/w{i}")))
            .collect();
        let parts: Vec<NodeId> = (0..3)
            .flat_map(|i| [0, 2, 3].map(|j| format!("u.overview/providers/xai-oauth/w{i}/{j}")))
            .map(|id| h.el("s:1", &id))
            .collect();
        let boxes = |h: &Harness| -> Vec<(f32, f32, f32)> {
            windows
                .iter()
                .chain(&parts)
                .map(|e| {
                    let r = h.doc.border_box(*e).unwrap();
                    (r.x, r.y, r.h)
                })
                .collect()
        };
        let before = boxes(h);
        for (x, y, height) in &before {
            assert!(
                *height <= lh + 4.5,
                "one line: {height} at ({x}, {y}), lh {lh}"
            );
        }
        let mut all = Vec::new();
        let mut stack = vec![one(h, ov, "sf-ov-card")];
        while let Some(n) = stack.pop() {
            if h.doc.node(n).is_element() {
                all.push(n);
            }
            stack.extend(h.doc.children(n).iter().copied());
        }
        assert!(all.len() > 50, "{} elements", all.len());
        for el in all {
            hover(h, el, true);
            let now = boxes(h);
            assert_eq!(now, before, "hovering {el:?} ({})", h.class(el));
            hover(h, el, false);
        }
    });
}

/// K10: an overlay with an omp.overlay.<name> role heads with its sheet's icon (omp's app icon
/// for any other name); a card with such a role, an overlay without a head, and other overlays
/// keep the kit's behaviour.
#[test]
fn omp_overlay_heads_carry_their_sheets_icon() {
    let names = [
        ("usage", "chart"),
        ("jobs", "activity"),
        ("hotkeys", "keyboard"),
        ("sessionInfo", "info"),
        ("model-hub", "model"),
        ("model-picker", "model"),
        ("login", "key"),
        ("planReview", "list-checks"),
        ("console", "terminal"),
        ("btwHistory", "app-omp"),
    ];
    let layer: Vec<Value> = names
        .iter()
        .map(|(n, _)| {
            json!({"id": format!("ov-{n}"), "k": "overlay",
                   "p": {"role": format!("omp.overlay.{n}"), "head": n},
                   "c": [{"id": format!("ov-{n}.t"), "k": "text", "p": {"text": "x"}}]})
        })
        .chain([
            json!({"id": "bare", "k": "overlay", "p": {"role": "omp.overlay.fireworks"},
                   "c": [{"id": "bare.t", "k": "text", "p": {"text": "x"}}]}),
            json!({"id": "plain", "k": "overlay", "p": {"head": "Plain"},
                   "c": [{"id": "card", "k": "card",
                          "p": {"role": "omp.overlay.model-picker", "head": "Switch Model"}}]}),
        ])
        .collect();
    let h = omp_regions(json!([]), json!([]), json!(layer), true);
    for (n, icon) in names {
        let ov = h.el("s1", &format!("ov-{n}"));
        let ic = one(&h, ov, "sf-ov-ic");
        assert!(!h.class(ic).contains("sf-hidden"), "{n}");
        let g = glyph(icon).unwrap_or_else(|| panic!("{icon} is in the icon table"));
        assert_eq!(h.text(ic), g.to_string(), "{n} → {icon}");
    }
    assert!(
        h.find_class(h.el("s1", "bare"), "sf-ov-head").is_empty(),
        "no head without one"
    );
    let plain = h.el("s1", "plain");
    assert!(h.class(one(&h, plain, "sf-ov-ic")).contains("sf-hidden"));
    let card = h.el("s1", "card");
    assert!(
        h.class(one(&h, card, "sf-card-ic")).contains("sf-hidden"),
        "the card keeps the kit's role icons"
    );
}
