//! The built-in sheets and real programs' sheets drawn: the views' documents painted with Vello
//! and rendered offscreen with wgpu, then pixels checked (amendment A7, `box-shadow`).

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

/// A7 / §9.6: the lens block's 2px state stripe (`box-shadow: inset 2px 0 0 …` in
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

/// A7: the SDK `ask` example's card (fixtures/tsp/docs-ask.jsonl; the same sheet in all four
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
