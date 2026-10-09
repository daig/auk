//! T2 `effort` and a program's `block` (amendment A23, book:7074-7146, 8265-8346): props, ARIA,
//! children, pointer behavior, layout, an offscreen pixel check of the ring, and omp's own
//! effort chip from its recorded session.

mod common;

use auk_tsp::{Config, ViewEvent};
use auk_ui::css::values::Color;
use auk_ui::{Document, NodeId};
use auk_views::{Outcome, PointerEvent, ViewSettings};
use common::{Harness, replay};
use serde_json::{Value, json};

/// A harness whose hello advertises T0+T1 plus `effort` and `block`, as the integrated v1 will.
fn harness() -> Harness {
    let mut config = Config::default();
    for k in ["effort", "block"] {
        config.kinds.push(k.to_owned());
    }
    let mut h = Harness::with(config, ViewSettings::default());
    h.hello(&[]);
    h
}

fn settle(h: &mut Harness) {
    // Past every first-show sweep and fade: one frame starts them, the next is past them.
    h.frame();
    h.now += 2000.0;
    h.frame();
}

fn click(h: &mut Harness, el: NodeId) -> Vec<Outcome> {
    let r = h.doc.border_box(el).expect("laid out");
    let out = h.view.click(
        &mut h.doc,
        &h.pane,
        PointerEvent {
            x: r.x + r.w / 2.0,
            y: r.y + r.h / 2.0,
            clicks: 1,
            ..Default::default()
        },
    );
    h.frame();
    out
}

/// The `.rot` rotors of an effort element (right, left).
fn rotors(h: &Harness, el: NodeId) -> (NodeId, NodeId) {
    let r = h.find_class(el, "rot");
    assert_eq!(r.len(), 2, "two rotors");
    (r[0], r[1])
}

fn style(h: &Harness, el: NodeId) -> String {
    h.view.written().style(el).unwrap_or("").to_owned()
}

const LEVELS: [&str; 8] = [
    "off", "minimal", "low", "medium", "high", "xhigh", "max", "auto",
];

fn effort_surface(h: &mut Harness) {
    let nodes: Vec<Value> = LEVELS
        .iter()
        .map(|l| json!({"id": format!("e-{l}"), "k": "effort", "p": {"level": l}}))
        .collect();
    h.surface(
        "s1",
        "inline",
        json!([{"id": "row", "k": "row", "p": {"gap": "lg"}, "c": nodes}]),
    );
    settle(h);
}

#[test]
fn effort_is_an_inline_meter_with_the_books_aria() {
    let mut h = harness();
    effort_surface(&mut h);
    assert!(h.errors().is_empty(), "{:?}", h.errors());
    for (i, l) in LEVELS.iter().enumerate() {
        let el = h.el("s1", &format!("e-{l}"));
        assert_eq!(h.doc.node(el).element().unwrap().tag.as_str(), "span");
        let class = h.class(el);
        assert!(class.starts_with("sf sf-effort"), "{class}");
        assert_eq!(class.contains("unknown"), *l == "auto", "{l}: {class}");
        assert_eq!(h.attr(el, "data-level").as_deref(), Some(*l));
        assert_eq!(h.attr(el, "role").as_deref(), Some("meter"));
        assert_eq!(h.attr(el, "aria-valuemin").as_deref(), Some("0"));
        assert_eq!(h.attr(el, "aria-valuemax").as_deref(), Some("6"));
        let now = if *l == "auto" { 0 } else { i };
        assert_eq!(h.attr(el, "aria-valuenow"), Some(now.to_string()));
        assert_eq!(h.attr(el, "aria-valuetext").as_deref(), Some(*l));
        assert_eq!(h.attr(el, "aria-label").as_deref(), Some("Thinking effort"));
        // The glyph and its parts (book: track, arc, blaze, crown, ember).
        let eff = h.find_class(el, "sf-eff");
        assert_eq!(eff.len(), 1);
        assert_eq!(h.attr(eff[0], "aria-hidden").as_deref(), Some("true"));
        for part in ["track", "arc", "blaze", "crown", "ember"] {
            assert_eq!(h.find_class(eff[0], part).len(), 1, "{l} has .{part}");
        }
        // 14px ring.
        let r = h.doc.border_box(eff[0]).unwrap();
        assert!(
            (r.w - 14.0).abs() < 0.01 && (r.h - 14.0).abs() < 0.01,
            "{r:?}"
        );
    }
    // The arc: one sixth per rung, the right half first.
    let rot = |h: &Harness, l: &str| {
        let (r, lft) = rotors(h, h.el("s1", &format!("e-{l}")));
        (style(h, r), style(h, lft))
    };
    assert_eq!(
        rot(&h, "minimal"),
        (
            "transform: rotate(60deg)".into(),
            "transform: rotate(0deg)".into()
        )
    );
    assert_eq!(
        rot(&h, "high"),
        (
            "transform: rotate(180deg)".into(),
            "transform: rotate(60deg)".into()
        )
    );
    assert_eq!(
        rot(&h, "auto"),
        (
            "transform: rotate(0deg)".into(),
            "transform: rotate(0deg)".into()
        )
    );
    // `aria` names it instead.
    h.frame_ops("s1", 2, json!([["set", "e-low", {"aria": "Reasoning"}]]));
    let el = h.el("s1", "e-low");
    assert_eq!(h.attr(el, "aria-label").as_deref(), Some("Reasoning"));
}

#[test]
fn effort_level_changes_move_the_same_parts_and_ignore_children() {
    let mut h = harness();
    h.surface(
        "s1",
        "inline",
        json!([{"id": "e", "k": "effort", "p": {"level": "low"},
                "c": [{"id": "kid", "k": "text", "p": {"text": "ignored"}}]}]),
    );
    settle(&mut h);
    let el = h.el("s1", "e");
    assert!(!h.text(el).contains("ignored"), "children are ignored");
    let (r0, l0) = rotors(&h, el);
    h.frame_ops("s1", 2, json!([["set", "e", {"level": "xhigh"}]]));
    h.frame();
    let (r1, l1) = rotors(&h, el);
    // The same elements, so the rotors transition (the arc sweeps to the new fill).
    assert_eq!((r0, l0), (r1, l1));
    assert_eq!(style(&h, r1), "transform: rotate(180deg)");
    assert_eq!(style(&h, l1), "transform: rotate(120deg)");
    let st = h.doc.computed_style(r1).unwrap();
    assert!(
        st.transitions
            .as_ref()
            .is_some_and(|t| t.iter().any(|t| &*t.property == "transform")),
        "the rotor transitions its transform"
    );
    // At max the fire shows and the arc fades; unknown levels dash the track.
    h.frame_ops("s1", 3, json!([["set", "e", {"level": "max"}]]));
    settle(&mut h);
    let op = |h: &Harness, c: &str| {
        h.doc
            .computed_style(h.find_class(el, c)[0])
            .unwrap()
            .opacity
    };
    assert_eq!(op(&h, "blaze"), 1.0);
    assert!(op(&h, "ember") > 0.4, "the ember shows (and flickers)");
    assert_eq!(op(&h, "arc"), 0.0);
    let crown = h.doc.computed_style(h.find_class(el, "crown")[0]).unwrap();
    assert!(
        crown
            .animations
            .as_ref()
            .is_some_and(|a| a.iter().any(|a| &*a.name == "sf-flame")),
        "the crown flickers"
    );
    h.frame_ops("s1", 4, json!([["set", "e", {"level": "auto"}]]));
    settle(&mut h);
    let track = h.doc.computed_style(h.find_class(el, "track")[0]).unwrap();
    assert_eq!(
        format!("{:?}", track.border_style[0]),
        "Dashed",
        "unresolved: a dashed empty ring"
    );
    assert_eq!(op(&h, "blaze"), 0.0);
}

#[test]
fn effort_actions_work_as_on_any_node() {
    let mut h = harness();
    h.surface(
        "s1",
        "inline",
        json!([{"id": "e", "k": "effort", "p": {
            "level": "high", "title": "Thinking effort: high", "actions": {"click": "thinking.cycle"}}}]),
    );
    settle(&mut h);
    let el = h.el("s1", "e");
    assert!(h.class(el).contains("sf-act"));
    assert_eq!(
        h.attr(el, "title").as_deref(),
        Some("Thinking effort: high")
    );
    let out = click(&mut h, el);
    let evs: Vec<_> = out
        .iter()
        .filter_map(|o| match o {
            Outcome::Event { event, .. } => Some((**event).clone()),
            _ => None,
        })
        .collect();
    assert!(
        matches!(&evs[..], [ViewEvent::Action { id, action, .. }] if id == "e" && action == "thinking.cycle"),
        "{evs:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// Pixels

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

fn rgb(c: Color) -> [f32; 3] {
    let [r, g, b, _] = c.to_rgba8();
    [r as f32, g as f32, b as f32]
}

fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - y).powi(2))
        .sum::<f32>()
        .sqrt()
}

/// The pixel on the ring's stroke at `deg` clockwise from 12 o'clock.
fn ring_px(px: &[u8], w: u32, eff: auk_ui::Rect, deg: f32) -> [f32; 3] {
    let (cx, cy) = (eff.x + 7.0, eff.y + 7.0);
    let a = deg.to_radians();
    let (x, y) = (cx + 6.0 * a.sin(), cy - 6.0 * a.cos());
    let i = ((y.floor() as u32 * w + x.floor() as u32) * 4) as usize;
    [px[i] as f32, px[i + 1] as f32, px[i + 2] as f32]
}

/// The ring fills clockwise one sixth per rung in the level's color; the rest is the track.
#[test]
fn effort_ring_paints_its_rung() {
    let mut h = harness();
    effort_surface(&mut h);
    let (w, ht) = (960, 120);
    let px = render(&h.doc, w, ht);
    for (rung, l) in LEVELS.iter().enumerate().take(6) {
        let el = h.el("s1", &format!("e-{l}"));
        let eff = h.doc.border_box(h.find_class(el, "sf-eff")[0]).unwrap();
        assert!(eff.y + 14.0 < ht as f32);
        let arc = h.find_class(el, "ring")[0];
        let arc_c = h.doc.computed_style(arc).unwrap().border_color[0];
        // The track over the page (white under the region's background).
        let track = h.find_class(el, "track")[0];
        let tc = h.doc.computed_style(track).unwrap().border_color[0];
        let page = px_at(&px, w, (w - 2) as f32, (ht - 2) as f32);
        let track_rgb = over(tc, page);
        let arc_rgb = over(arc_c, page);
        if rung > 0 {
            assert!(dist(arc_rgb, track_rgb) > 40.0, "{l}: arc and track differ");
        }
        for k in 0..12 {
            let deg = 15.0 + k as f32 * 30.0;
            let got = ring_px(&px, w, eff, deg);
            let filled = deg < rung as f32 * 60.0;
            let want = if filled { arc_rgb } else { track_rgb };
            let other = if filled { track_rgb } else { arc_rgb };
            assert!(
                rung == 0 || dist(got, want) < dist(got, other),
                "{l} at {deg}°: {got:?} should be {} ({want:?}, not {other:?})",
                if filled { "arc" } else { "track" }
            );
        }
        // The middle stays empty.
        let mid = px_at(&px, w, eff.x + 7.0, eff.y + 7.0);
        assert!(
            dist(mid, page) < 8.0,
            "{l}: hollow ring {mid:?} {page:?} {eff:?}"
        );
    }
    // max: a closed ring in the fire color, with an ember in the middle.
    let el = h.el("s1", "e-max");
    let eff = h.doc.border_box(h.find_class(el, "sf-eff")[0]).unwrap();
    let blaze = h
        .doc
        .computed_style(h.find_class(el, "blaze")[0])
        .unwrap()
        .border_color[0];
    let page = px_at(&px, w, (w - 2) as f32, (ht - 2) as f32);
    for k in 0..12 {
        let got = ring_px(&px, w, eff, 15.0 + k as f32 * 30.0);
        assert!(dist(got, over(blaze, page)) < 40.0, "max ring {k}: {got:?}");
    }
    let mid = px_at(&px, w, eff.x + 7.0, eff.y + 7.0);
    assert!(dist(mid, page) > 40.0, "max: the ember");
}

fn px_at(px: &[u8], w: u32, x: f32, y: f32) -> [f32; 3] {
    let i = ((y.floor() as u32 * w + x.floor() as u32) * 4) as usize;
    [px[i] as f32, px[i + 1] as f32, px[i + 2] as f32]
}

fn over(top: Color, under: [f32; 3]) -> [f32; 3] {
    let c = rgb(top);
    let a = top.a;
    [
        c[0] * a + under[0] * (1.0 - a),
        c[1] * a + under[1] * (1.0 - a),
        c[2] * a + under[2] * (1.0 - a),
    ]
}

// ---------------------------------------------------------------------------------------------
// omp

/// omp 18.8.5, under a hello advertising `effort`, puts the effort chip in its composer bar's
/// model segment: `level`, a tooltip and a click action that cycles the level (recorded in
/// `fixtures/tsp/omp-session.jsonl`).
#[test]
fn omp_effort_chip_draws_from_its_recorded_session() {
    let mut checked = false;
    let h = replay("omp-session.jsonl", |h, rec| {
        // Once omp's composer has settled (its dock goes when the session surface closes).
        if checked || rec.verb != "f" || rec.body["s"].as_u64().is_none_or(|s| s < 10) {
            return;
        }
        let Some(sf) = h
            .pane
            .surfaces()
            .iter()
            .rev()
            .find(|s| s.document().get("3.bar/model/effort").is_some())
            .map(|s| s.id().to_owned())
        else {
            return;
        };
        checked = true;
        check_omp_chip(h, &sf);
    });
    assert!(checked, "omp sent its effort chip");
    let errors: Vec<_> = h
        .sent
        .iter()
        .filter(|e| e["ev"] == "error" || e["ev"] == "gone")
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
}

fn check_omp_chip(h: &mut Harness, sf: &str) {
    let now = h.now;
    settle(h);
    let el = h.el(sf, "3.bar/model/effort");
    let class = h.class(el);
    assert!(
        class.starts_with("sf sf-effort") && class.contains("sf-act"),
        "{class}"
    );
    assert_eq!(h.attr(el, "data-level").as_deref(), Some("high"));
    assert_eq!(
        h.attr(el, "data-role").as_deref(),
        Some("omp.composer.model.effort")
    );
    assert!(
        h.attr(el, "title")
            .is_some_and(|t| t.starts_with("Thinking effort: high"))
    );
    assert_eq!(h.attr(el, "aria-valuenow").as_deref(), Some("4"));
    // It sits in the model segment, before the model's name, on the same line.
    let eff = h.doc.border_box(h.find_class(el, "sf-eff")[0]).unwrap();
    let name = h.doc.border_box(h.el(sf, "3.bar/model/name")).unwrap();
    assert!(
        eff.w == 14.0 && eff.x + eff.w <= name.x + 0.5,
        "{eff:?} {name:?}"
    );
    assert!(
        eff.y >= name.y - 4.0 && eff.y + eff.h <= name.y + name.h + 4.0,
        "{eff:?} {name:?}"
    );
    let (r, l) = rotors(h, el);
    assert_eq!(style(h, r), "transform: rotate(180deg)");
    assert_eq!(style(h, l), "transform: rotate(60deg)");
    // A click cycles the level: omp hears its action.
    let out = click(h, el);
    assert!(
        out.iter().any(|o| matches!(o, Outcome::Event { event, .. }
            if matches!(&**event, ViewEvent::Action { action, .. } if action == "thinking.cycle"))),
        "{out:?}"
    );
    h.now = now;
}

// ---------------------------------------------------------------------------------------------
// block

fn block_surface(h: &mut Harness) {
    h.surface(
        "s1",
        "inline",
        json!([{"id": "b", "k": "block", "p": {
        "cmd": "sudo kubectl get pods -A", "lens": "plugin.k8s.get", "state": "done",
        "exit": 1, "took": 2400, "key": "k1"},
        "c": [
            {"id": "v", "k": "col", "c": [{"id": "vt", "k": "text", "p": {"text": "native view"}}]},
            {"id": "raw", "k": "ansi", "p": {"text": "NAME READY\r\npod-a 1/1\r\n"}}
        ]}]),
    );
    settle(h);
}

/// A program's `block` draws as Tern's (book:8265-8346): the toolbar with the lens label, the
/// Native/Raw control and Copy, the foot with the failing exit and the duration.
#[test]
fn a_programs_block_draws_like_a_command_block() {
    let mut h = harness();
    block_surface(&mut h);
    assert!(h.errors().is_empty(), "{:?}", h.errors());
    let el = h.el("s1", "b");
    let class = h.class(el);
    assert!(
        class.starts_with("sf sf-block") && class.contains("failed"),
        "{class}"
    );
    assert_eq!(
        h.attr(el, "data-role").as_deref(),
        Some("lens.plugin.k8s.get")
    );
    assert_eq!(h.attr(el, "data-state").as_deref(), Some("done"));
    assert_eq!(h.attr(el, "data-view").as_deref(), Some("native"));
    let lens = h.find_class(el, "sf-block-lens")[0];
    assert_eq!(
        h.attr(lens, "title").as_deref(),
        Some("sudo kubectl get pods -A")
    );
    assert_eq!(h.text(h.find_class(lens, "lbl")[0]), "kubectl get");
    let opts = h.find_class(el, "sf-block-opt");
    assert_eq!(opts.len(), 2);
    assert_eq!(h.find_class(el, "sf-block-copy").len(), 1);
    let foot = h.find_class(el, "sf-block-foot")[0];
    assert_eq!(h.text(foot), "exit 12.4s");
    // Only the native view is mounted in the body.
    let body = h.find_class(el, "sf-block-body")[0];
    assert!(h.text(body).contains("native view"));
    assert!(!h.text(body).contains("pod-a"));
    let r = h.doc.border_box(el).unwrap();
    assert!(r.h > 20.0 && r.w > 100.0, "{r:?}");
}

/// Native/Raw is local (no event reaches the program) and kept by `key`; Copy and the `copy`
/// action copy an empty string, since a program's block isn't a terminal's command block.
#[test]
fn a_programs_block_toggles_locally_and_copies_nothing() {
    let mut h = harness();
    block_surface(&mut h);
    let el = h.el("s1", "b");
    let raw_opt = h.find_class(el, "sf-block-opt")[1];
    let before = h.sent.len();
    let out = click(&mut h, raw_opt);
    assert!(
        out.iter().all(|o| !matches!(o, Outcome::Event { .. })),
        "{out:?}"
    );
    assert_eq!(h.sent.len(), before);
    assert_eq!(h.attr(el, "data-view").as_deref(), Some("raw"));
    let body = h.find_class(el, "sf-block-body")[0];
    assert!(h.text(body).contains("pod-a"), "{}", h.text(body));
    // The same key under the same parent keeps the choice across a new id.
    h.frame_ops(
        "s1",
        2,
        json!([["del", "b"], ["add", "b2", "main", null, {"id": "b2", "k": "block",
            "p": {"cmd": "kubectl get pods", "key": "k1"},
            "c": [{"id": "v2", "k": "col", "c": []}, {"id": "raw2", "k": "ansi", "p": {"text": "x\r\n"}}]}]]),
    );
    settle(&mut h);
    let el2 = h.el("s1", "b2");
    assert_eq!(h.attr(el2, "data-view").as_deref(), Some("raw"));
    // Copy copies an empty string.
    let copy = h.find_class(el2, "sf-block-copy")[0];
    let out = click(&mut h, copy);
    assert_eq!(out, vec![Outcome::Copy(String::new())]);
    h.frame_ops(
        "s1",
        3,
        json!([["set", "b2", {"actions": {"click": "copy"}}]]),
    );
    settle(&mut h);
    let foot_or_body = h.find_class(el2, "sf-block-body")[0];
    let out = click(&mut h, foot_or_body);
    assert!(
        out.iter()
            .all(|o| !matches!(o, Outcome::Copy(t) if !t.is_empty())),
        "{out:?}"
    );
}

/// A command block (the lens layer's own view) still copies its raw output as plain text.
#[test]
fn a_command_blocks_copy_still_copies_its_raw_output() {
    let mut layer = auk_views::LensLayer::new(ViewSettings::default(), 1.0);
    let block = auk_views::LensInput {
        id: 0,
        anchor: auk_tsp::AnchorKey(1 << 48),
        node: json!({"id": "lens0", "k": "block", "p": {"cmd": "make", "lens": "plugin.t.make"},
            "c": [{"id": "lens0.raw", "k": "ansi", "p": {"text": "\u{1b}[31mone\u{1b}[0m\r\ntwo\r\n"}}]}),
        cols: 40,
    };
    layer.sync(&[block], 0.0);
    layer.layout(40, 400.0, 0.0);
    let (_, _, _, [x, y, w, hh]) = layer
        .parts(0)
        .into_iter()
        .find(|(c, ..)| c.contains("sf-block-copy"))
        .expect("Copy");
    let top = layer
        .doc()
        .border_box(layer.element("lens0").unwrap())
        .unwrap()
        .y;
    let out = layer.click(PointerEvent {
        x: x + w / 2.0,
        y: top + y + hh / 2.0,
        clicks: 1,
        ..Default::default()
    });
    match out.as_slice() {
        [Outcome::Copy(t)] => assert_eq!(t.trim_end(), "one\ntwo"),
        other => panic!("{other:?}"),
    }
}
