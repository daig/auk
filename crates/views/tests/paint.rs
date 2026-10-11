//! The built-in sheets and real programs' sheets drawn: the views' documents painted with Vello
//! and rendered offscreen with wgpu, then pixels checked (`box-shadow`).

mod common;

use auk_tsp::AnchorKey;
use auk_ui::css::values::Color;
use auk_ui::{Document, NodeId};
use auk_views::{LensInput, LensLayer, ViewSettings};
use common::replay;
use serde_json::json;

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

/// The pixel at (`x`, `y`) of a `w`-wide render.
fn pixel(px: &[u8], w: u32, x: f32, y: f32) -> [u8; 3] {
    let i = ((y as u32 * w + x as u32) * 4) as usize;
    [px[i], px[i + 1], px[i + 2]]
}

/// `top` composited over opaque `under`, as 8-bit RGB.
fn over(top: Color, under: [u8; 3]) -> [u8; 3] {
    let [r, g, b, _] = top.to_rgba8();
    let a = top.a;
    let mix = |t: u8, u: u8| (t as f32 * a + u as f32 * (1.0 - a)).round() as u8;
    [mix(r, under[0]), mix(g, under[1]), mix(b, under[2])]
}

fn near(a: [u8; 3], b: [u8; 3]) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 3)
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

fn lens_block(state: &str, exit: i64) -> LensInput {
    LensInput {
        id: 0,
        anchor: AnchorKey(1 << 48),
        node: json!({"id": "lens0", "k": "block", "p": {
            "cmd": "make", "line": "make", "lens": "plugin.t.make", "role": "lens.plugin.t.make",
            "state": state, "exit": exit, "took": 2400
        }, "c": [
            {"id": "lens0.raw", "k": "ansi", "p": {"text": "line one\nline two\nline three\n"}}
        ]}),
        cols: 40,
    }
}

/// The lens block's 2px state stripe (`box-shadow: inset 2px 0 0 …` in
/// surface-lens.css) is drawn: `--live` while running, `--sf-bad` (`--bad`) after a failing
/// exit, a faint `--t3` mix when done.
#[test]
fn lens_block_draws_its_state_stripe() {
    const W: u32 = 400;
    let mut layer = LensLayer::new(ViewSettings::default(), 1.0);
    let mut stripe = |state: &str, exit: i64| -> ([u8; 3], [u8; 3], [u8; 3], [u8; 3]) {
        layer.sync(&[lens_block(state, exit)], 0.0);
        layer.layout(40, W as f32, 0.0);
        let doc = layer.doc();
        let el = layer.element("lens0").expect("the block element");
        let r = doc.border_box(el).expect("the block's box");
        assert!(r.h > 20.0, "the block has its output: {r:?}");
        let px = render(doc, W, (r.y + r.h).ceil() as u32 + 4);
        let y = r.y + r.h / 2.0;
        let want = match (state, exit) {
            ("running", _) => theme_rgb(doc, el, "--live"),
            (_, 0) => [0, 0, 0],
            _ => theme_rgb(doc, el, "--bad"),
        };
        (
            pixel(&px, W, r.x + 0.5, y),
            pixel(&px, W, r.x + 1.5, y),
            pixel(&px, W, r.x + 3.5, y),
            want,
        )
    };
    let (a, b, past, live) = stripe("running", 0);
    assert!(
        near(a, live) && near(b, live),
        "running stripe {a:?} {b:?}, --live {live:?}"
    );
    assert!(near(past, [255, 255, 255]), "2px wide: {past:?}");
    let (a, b, past, bad) = stripe("done", 2);
    assert!(
        near(a, bad) && near(b, bad),
        "failed stripe {a:?} {b:?}, --bad {bad:?}"
    );
    assert!(near(past, [255, 255, 255]), "2px wide: {past:?}");
    let (a, _, past, _) = stripe("done", 0);
    assert!(
        a != [255, 255, 255] && a.iter().all(|c| *c > 120),
        "a faint stripe when done: {a:?}"
    );
    assert!(near(past, [255, 255, 255]), "2px wide: {past:?}");
}

/// The SDK `ask` example's card (fixtures/tsp/docs-ask.jsonl; the same sheet in all four
/// SDKs' `ask` examples) draws its 1px ring, `box-shadow: inset 0 0 0 1px var(--l2)`, over
/// `--card`.
#[test]
fn sdk_ask_card_draws_its_ring() {
    let mut h = replay("docs-ask.jsonl", |_, _| {});
    h.frame();
    let ask = h.el("pick", "ask");
    let style = h.doc.computed_style(ask).unwrap();
    let ring = style
        .box_shadow
        .as_deref()
        .expect("the ask card has a box-shadow")[0];
    assert!(ring.inset && ring.spread == 1.0, "{ring:?}");
    let card = style.background_color.to_rgba8();
    let card = [card[0], card[1], card[2]];
    let r = h.doc.border_box(ask).unwrap();
    let (w, hgt) = (960, (r.y + r.h).ceil() as u32 + 4);
    let px = render(&h.doc, w, hgt);
    let want = over(ring.color, card);
    assert!(
        want.iter().zip(card).any(|(a, b)| a.abs_diff(b) >= 8),
        "the ring is visible against the card: {want:?} on {card:?}"
    );
    let mid = r.y + r.h / 2.0;
    let edge = [
        pixel(&px, w, r.x + 0.5, mid),
        pixel(&px, w, r.x + r.w - 0.5, mid),
        pixel(&px, w, r.x + r.w / 2.0, r.y + 0.5),
        pixel(&px, w, r.x + r.w / 2.0, r.y + r.h - 0.5),
    ];
    for (side, p) in ["left", "right", "top", "bottom"].iter().zip(edge) {
        assert!(
            near(p, want),
            "{side} ring {p:?}, want {want:?} (--l2 over --card {card:?})"
        );
    }
    let inside = pixel(&px, w, r.x + 2.5, mid);
    assert!(
        near(inside, card),
        "--card inside the ring: {inside:?} vs {card:?}"
    );
}

/// A split diff drawn: the removed half tinted with `--bad`, the added half with `--ok`,
/// and the blank side of an unpaired line shaded with `--sf-shade` over the diff's `--code-bg`.
#[test]
fn split_diff_draws_its_halves_and_the_blank_side() {
    let mut h = common::Harness::new();
    h.hello(&[]);
    h.surface(
        "s1",
        "inline",
        json!([{"id": "d", "k": "diff", "p": {"mode": "split", "hunks": [
            {"oldStart": 1, "newStart": 1, "lines": ["-old words", "+new words", "+added"]}
        ]}}]),
    );
    h.frame();
    let d = h.el("s1", "d");
    let rows = h.find_class(d, "sf-dr");
    assert_eq!(rows.len(), 2);
    let r = h.doc.border_box(d).unwrap();
    let (w, hgt) = (960, (r.y + r.h).ceil() as u32 + 4);
    let px = render(&h.doc, w, hgt);
    let bg = h.doc.computed_style(d).unwrap().background_color;
    let base = over(bg, [255, 255, 255]);
    // Right of the text, inside each half.
    let at = |el: NodeId| {
        let b = h.doc.border_box(el).unwrap();
        pixel(&px, w, b.x + b.w - 6.0, b.y + b.h / 2.0)
    };
    let first = h.doc.element_children(rows[0]);
    let (del, add) = (at(first[0]), at(first[1]));
    assert!(
        del[0] > del[1] + 8 && add[1] > add[0] + 8,
        "removed reads red {del:?}, added green {add:?}"
    );
    let second = h.doc.element_children(rows[1]);
    assert!(h.class(second[0]).contains("nil"));
    let shade = h.doc.computed_style(second[0]).unwrap().background_color;
    let want = over(shade, base);
    let got = at(second[0]);
    assert!(
        near(got, want),
        "--sf-shade over --code-bg: {got:?}, want {want:?}"
    );
    assert!(
        got.iter().zip(base).any(|(a, b)| a.abs_diff(b) >= 2),
        "the blank side is visible: {got:?} vs {base:?}"
    );
}

fn kit_harness(dark: bool) -> common::Harness {
    let mut h = common::Harness::with(
        Default::default(),
        ViewSettings {
            dark,
            ..Default::default()
        },
    );
    h.hello(&[]);
    h
}

/// A toned badge draws its 10% fill inside a 22% ring,
/// in light and dark.
#[test]
fn a_badge_draws_its_tone_ring() {
    for dark in [false, true] {
        let mut h = kit_harness(dark);
        h.surface(
            "s1",
            "inline",
            json!([{"id": "r", "k": "row", "c": [{"id": "b", "k": "badge", "p": {"text": "failed", "tone": "error"}}]}]),
        );
        h.frame();
        let el = h.el("s1", "b");
        let st = h.doc.computed_style(el).unwrap();
        let (fill, ring) = (
            st.background_color,
            st.box_shadow.as_deref().unwrap()[0].color,
        );
        let r = h.doc.border_box(el).unwrap();
        let (w, hgt) = (960, (r.y + r.h).ceil() as u32 + 4);
        let px = render(&h.doc, w, hgt);
        let under = pixel(&px, w, r.x + r.w + 4.0, r.y + r.h / 2.0);
        let want_fill = over(fill, under);
        let want_ring = over(ring, want_fill);
        let got_fill = pixel(&px, w, r.x + 3.5, r.y + r.h / 2.0);
        let got_ring = pixel(&px, w, r.x + r.w / 2.0, r.y + 0.5);
        assert!(
            near(got_fill, want_fill),
            "dark {dark}: fill {got_fill:?}, want {want_fill:?}"
        );
        assert!(
            near(got_ring, want_ring),
            "dark {dark}: ring {got_ring:?}, want {want_ring:?}"
        );
        assert!(
            got_ring
                .iter()
                .zip(got_fill)
                .any(|(a, b)| a.abs_diff(b) >= 6),
            "dark {dark}: the ring shows against the fill: {got_ring:?} vs {got_fill:?}"
        );
    }
}

/// An inline tool's body draws its --l2 hairline, in light and dark.
#[test]
fn an_inline_tool_draws_its_hairline() {
    for dark in [false, true] {
        let mut h = kit_harness(dark);
        h.surface(
            "s1",
            "inline",
            json!([{"id": "t", "k": "tool", "p": {"name": "read", "title": "Read", "status": "done", "frame": "inline"},
                "c": [{"id": "o", "k": "text", "p": {"text": "one\ntwo\nthree"}}]}]),
        );
        h.frame();
        let body = h.find_class(h.el("s1", "t"), "sf-tool-body")[0];
        let line = h.doc.computed_style(body).unwrap().border_color[3];
        let r = h.doc.border_box(body).unwrap();
        let (w, hgt) = (960, (r.y + r.h).ceil() as u32 + 4);
        let px = render(&h.doc, w, hgt);
        let y = r.y + r.h / 2.0;
        let under = pixel(&px, w, r.x - 3.0, y);
        let got = pixel(&px, w, r.x + 0.5, y);
        assert!(
            near(got, over(line, under)),
            "dark {dark}: hairline {got:?} over {under:?}"
        );
        assert!(got != under, "dark {dark}: visible");
        assert!(
            near(pixel(&px, w, r.x + 3.0, y), under),
            "dark {dark}: 1px wide"
        );
    }
}

/// The overlay card casts its drop shadow below it, and in dark its --glass-hi top
/// highlight lightens its first row.
#[test]
fn an_overlay_card_casts_its_glass_shadow() {
    for dark in [false, true] {
        let mut h = kit_harness(dark);
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
        // Past the card's 220ms pop-in.
        h.now += 300.0;
        h.frame();
        let card = h.find_class(h.el("s1", "ov"), "sf-ov-card")[0];
        let r = h.doc.border_box(card).unwrap();
        let w = 960;
        let px = render(&h.doc, w, 800);
        let y = r.y + r.h + 12.0;
        let below = pixel(&px, w, r.x + r.w / 2.0, y);
        let away = pixel(&px, w, 20.0, y);
        let sum = |p: [u8; 3]| p.iter().map(|&c| c as u32).sum::<u32>();
        assert!(
            sum(below) + 6 < sum(away),
            "dark {dark}: a shadow below {below:?} vs {away:?}"
        );
        if dark {
            let [r8, g8, b8, _] = h
                .doc
                .computed_style(card)
                .unwrap()
                .background_color
                .to_rgba8();
            let top = pixel(&px, w, r.x + 24.0, r.y + 1.5);
            let inside = pixel(&px, w, r.x + 24.0, r.y + 5.0);
            assert!(near(inside, [r8, g8, b8]), "--pop-bg inside: {inside:?}");
            assert!(
                sum(top) > sum(inside) + 3,
                "the top highlight: {top:?} vs {inside:?}"
            );
        }
    }
}

/// omp's user bubble at its first prompt (F:17)
/// paints a fill that shows against the page, in light and dark: with no ring at its edge, and
/// with omp's palette a whisper ring.
#[test]
fn an_omp_user_bubble_paints_a_fill_ringed_only_under_a_palette() {
    for palette in [false, true] {
        for dark in [false, true] {
            let at = format!("dark {dark}, palette {palette}");
            let mut line = 0;
            let mut seen = false;
            replay("omp-session.jsonl", |h, _| {
                line += 1;
                if line == 1 {
                    let s = ViewSettings {
                        dark,
                        program_colors: palette,
                        ..Default::default()
                    };
                    h.view.set_settings(&mut h.doc, &h.pane, s);
                }
                if line != 17 {
                    return;
                }
                seen = true;
                h.frame();
                let r = h.doc.border_box(h.el("s:1", "u")).unwrap();
                let w = 960;
                let px = render(&h.doc, w, (r.y + r.h).ceil() as u32 + 8);
                let y = r.y + r.h / 2.0;
                let page = pixel(&px, w, r.x - 8.0, y);
                let fill = pixel(&px, w, r.x + 4.0, y);
                assert!(
                    page.iter().zip(fill).any(|(a, b)| a.abs_diff(b) >= 3),
                    "{at}: the fill {fill:?} shows on the page {page:?}"
                );
                if palette {
                    // Titanium's fill is about 1.02:1 on the page, so a palette keeps a
                    // whisper ring, which shows against the fill.
                    // Inside the card's transparent 1px border.
                    let edge = pixel(&px, w, r.x + r.w / 2.0, r.y + 1.5);
                    assert!(
                        edge.iter().zip(fill).any(|(a, b)| a.abs_diff(b) >= 3),
                        "{at}: ring {edge:?} on {fill:?}"
                    );
                    return;
                }
                // Edge pixels lie between the page and the fill: no ring beyond either.
                for (x, y) in [
                    (r.x + 0.5, y),
                    (r.x + r.w - 0.5, y),
                    (r.x + r.w / 2.0, r.y + 0.5),
                    (r.x + r.w / 2.0, r.y + r.h - 0.5),
                ] {
                    let edge = pixel(&px, w, x, y);
                    for i in 0..3 {
                        let (lo, hi) = (page[i].min(fill[i]), page[i].max(fill[i]));
                        assert!(
                            edge[i] + 3 >= lo && edge[i] <= hi.saturating_add(3),
                            "{at}: edge {edge:?} at ({x}, {y}) beyond {page:?} and {fill:?}"
                        );
                    }
                }
            });
            assert!(seen, "{at}");
        }
    }
}

/// An omp marker draws a hairline on each side of its label.
#[test]
fn an_omp_marker_draws_hairlines_either_side() {
    for dark in [false, true] {
        let mut h = kit_harness(dark);
        h.apc("o", r#"{"id":"s1","mode":"inline","role":"omp.session"}"#);
        h.frame_ops(
            "s1",
            1,
            json!([["add", "main", "s1", null, {"id": "main", "k": "col", "c": [
                {"id": "mk", "k": "row", "p": {"role": "omp.marker.cache-miss", "gap": "sm"}, "c": [
                    {"id": "mk.t", "k": "text", "p": {"text": "cache miss"}}]}]}]]),
        );
        h.frame();
        let r = h.doc.border_box(h.el("s1", "mk")).unwrap();
        let w = 960;
        let px = render(&h.doc, w, (r.y + r.h).ceil() as u32 + 4);
        let page = pixel(&px, w, r.x + 4.0, r.y + 0.5);
        let line_at = |x: f32| {
            (0..r.h as u32).any(|dy| {
                let p = pixel(&px, w, x, r.y + dy as f32);
                p.iter().zip(page).any(|(a, b)| a.abs_diff(b) >= 6)
            })
        };
        assert!(line_at(r.x + 4.0), "dark {dark}: the left hairline");
        assert!(line_at(r.x + r.w - 4.0), "dark {dark}: the right hairline");
    }
}
