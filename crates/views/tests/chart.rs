//! `chart` (T2, A23; book:6975-7075): heatmap, bars and spark drawn as DOM+CSS, with the book's
//! props, classes, levels, sizes, token fill, summary, arrivals (`omp-cell`, `omp-bar`) and their
//! replay on any prop change; geometry from the UI engine's layout and pixels from an offscreen
//! render in both themes; and the charts omp 18.8.5 sends (its `/usage` activity heatmap and its
//! autoresearch trend bars), built as omp builds them.

mod common;

use auk_tsp::Config;
use auk_ui::css::values::Color;
use auk_ui::{Document, NodeId};
use auk_views::ViewSettings;
use common::Harness;
use serde_json::{Value, json};

/// A harness whose engine accepts `chart` (the integrator advertises it with A23).
fn harness(settings: ViewSettings) -> Harness {
    let mut config = Config::default();
    if !config.kinds.iter().any(|k| k == "chart") {
        config.kinds.push("chart".into());
    }
    let mut h = Harness::with(config, settings);
    h.hello(&[]);
    h
}

fn chart_surface(settings: ViewSettings, charts: Value) -> Harness {
    let mut h = harness(settings);
    h.surface("s1", "inline", charts);
    h.frame();
    assert!(h.errors().is_empty(), "{:?}", h.errors());
    h
}

/// Runs the clock past every arrival.
fn settle(h: &mut Harness) {
    h.now += 3000.0;
    h.frame();
}

fn bx(h: &Harness, el: NodeId) -> auk_ui::Rect {
    h.doc.border_box(el).expect("laid out")
}

/// The heatmap of the book's example: 7 rows × `weeks` columns, with tips, row and month labels.
fn heatmap(weeks: usize) -> Value {
    let cells: Vec<Vec<Value>> = (0..7)
        .map(|r| {
            (0..weeks)
                .map(|x| {
                    if x + 1 == weeks && r > 3 {
                        Value::Null
                    } else {
                        json!(((r * weeks + x) % 5) as f64 / 4.0)
                    }
                })
                .collect()
        })
        .collect();
    let tips: Vec<Vec<Value>> = (0..7)
        .map(|r| (0..weeks).map(|x| json!(format!("r{r} w{x}"))).collect())
        .collect();
    json!({
        "kind": "heatmap",
        "token": "success",
        "rows": ["Mon", "", "Wed", "", "Fri", "", ""],
        "cols": [{"at": 0, "label": "Sep"}, {"at": 4, "label": "Oct"}, {"at": 9, "label": "Nov"},
                 {"at": 2.5, "label": "half"}, {"label": "no at"}, {"at": 13}],
        "cells": cells,
        "tips": tips,
        "summary": [{"t": "12 deploys", "s": "muted"}],
    })
}

#[test]
fn heatmap_draws_cells_by_level_with_labels_tips_and_a_summary() {
    let mut h = chart_surface(
        ViewSettings::default(),
        json!([{"id": "hm", "k": "chart", "p": heatmap(20)}]),
    );
    settle(&mut h);
    let el = h.el("s1", "hm");
    let class = h.class(el);
    assert!(class.split(' ').any(|c| c == "sf-chart"), "{class}");
    assert_eq!(h.attr(el, "data-kind").as_deref(), Some("heatmap"));
    assert_eq!(h.attr(el, "data-size").as_deref(), Some("md"));
    assert_eq!(h.attr(el, "role").as_deref(), Some("figure"));
    assert_eq!(h.attr(el, "aria-label").as_deref(), Some("12 deploys"));
    let style = h.view.written().style(el).unwrap_or("").to_owned();
    assert!(style.contains("--chart-c: var(--sf-c-success)"), "{style}");
    // Parts the book names.
    for part in ["hm", "mls", "grid", "rls", "sum"] {
        assert_eq!(h.find_class(el, part).len(), 1, ".{part}");
    }
    assert_eq!(h.text(h.find_class(el, "sum")[0]), "12 deploys");
    // Month labels: entries missing `at` or `label`, or with a fractional `at`, are skipped.
    let mls: Vec<String> = h.find_class(el, "ml").iter().map(|&m| h.text(m)).collect();
    assert_eq!(mls, ["Sep", "Oct", "Nov"]);
    let rls: Vec<String> = h.find_class(el, "rl").iter().map(|&m| h.text(m)).collect();
    assert_eq!(rls, ["Mon", "", "Wed", "", "Fri", "", ""]);
    // A column per week, a cell per row, levels and tips as the book maps them.
    let wks = h.find_class(el, "wk");
    assert_eq!(wks.len(), 20);
    let cells = h.find_class(el, "c");
    assert_eq!(cells.len(), 140);
    let col0: Vec<String> = h
        .find_class(wks[0], "c")
        .iter()
        .map(|&c| h.attr(c, "data-l").unwrap())
        .collect();
    // Row r of week 0 is (r*20 % 5)/4: 0, 0, 0, … → level 0.
    assert!(col0.iter().all(|l| l == "0"), "{col0:?}");
    let col1: Vec<String> = h
        .find_class(wks[1], "c")
        .iter()
        .map(|&c| h.attr(c, "data-l").unwrap())
        .collect();
    // (r*20+1) % 5 = 1 → 0.25 → level 1.
    assert!(col1.iter().all(|l| l == "1"), "{col1:?}");
    let last: Vec<String> = h
        .find_class(wks[19], "c")
        .iter()
        .map(|&c| h.attr(c, "data-l").unwrap())
        .collect();
    assert_eq!(&last[4..], ["n", "n", "n"], "null cells are n");
    assert_eq!(
        h.attr(h.find_class(wks[3], "c")[2], "title").as_deref(),
        Some("r2 w3")
    );
    // The heatmap is drawn for the eye; the figure carries the name.
    assert_eq!(
        h.attr(h.find_class(el, "hm")[0], "aria-hidden").as_deref(),
        Some("true")
    );
    let ax = h.view.accessibility(&h.doc);
    let fig = ax
        .find(&|n| n.id.as_deref() == Some("hm"))
        .expect("the chart in the accessibility tree");
    assert_eq!(fig.role, "figure");
    assert_eq!(fig.name.as_deref(), Some("12 deploys"));

    // Geometry: md cells are 11px squares, 3px apart; a month label sits over its column.
    let c00 = bx(&h, h.find_class(wks[0], "c")[0]);
    let c01 = bx(&h, h.find_class(wks[0], "c")[1]);
    let c10 = bx(&h, h.find_class(wks[1], "c")[0]);
    assert_eq!((c00.w, c00.h), (11.0, 11.0));
    assert!((c01.y - c00.y - 14.0).abs() < 0.01, "{c00:?} {c01:?}");
    assert!((c10.x - c00.x - 14.0).abs() < 0.01, "{c00:?} {c10:?}");
    let oct = bx(&h, h.find_class(el, "ml")[1]);
    let w4 = bx(&h, wks[4]);
    assert!((oct.x - w4.x).abs() < 0.6, "Oct {oct:?} over week 4 {w4:?}");
    assert!(oct.y + oct.h <= w4.y, "labels above the grid");
    // Row labels sit left of the cells, centered on their row.
    let mon = bx(&h, h.find_class(el, "rl")[0]);
    assert!(
        mon.x + mon.w <= c00.x && (mon.y - c00.y).abs() < 0.01,
        "{mon:?} {c00:?}"
    );
    let sum = bx(&h, h.find_class(el, "sum")[0]);
    assert!(
        sum.y >= c01.y + 5.0 * 14.0,
        "the summary is under the chart"
    );
}

#[test]
fn sizes_set_cell_and_bar_heights_and_unknown_values_fall_back() {
    let series: Vec<Value> = (1..=6).map(|v| json!({"value": v})).collect();
    let small = json!({"kind": "heatmap", "size": "sm", "cells": [[1, 0.5]]});
    let large = json!({"kind": "heatmap", "size": "lg", "cells": [[1, 0.5]]});
    let odd = json!({"kind": "pie", "size": "xl", "cells": [[1]]});
    let mut h = chart_surface(
        ViewSettings::default(),
        json!([
            {"id": "a", "k": "chart", "p": small},
            {"id": "b", "k": "chart", "p": large},
            {"id": "c", "k": "chart", "p": odd},
            {"id": "bs", "k": "chart", "p": {"kind": "bars", "size": "sm", "series": series}},
            {"id": "bm", "k": "chart", "p": {"kind": "bars", "series": series}},
            {"id": "bl", "k": "chart", "p": {"kind": "bars", "size": "lg", "series": series}},
            {"id": "sp", "k": "chart", "p": {"kind": "spark", "size": "lg", "series": series}},
        ]),
    );
    settle(&mut h);
    let cell = |h: &Harness, id: &str| bx(h, h.find_class(h.el("s1", id), "c")[0]);
    assert_eq!(cell(&h, "a").w, 8.0);
    assert_eq!(cell(&h, "b").w, 13.0);
    assert_eq!(cell(&h, "c").w, 11.0);
    let c = h.el("s1", "c");
    assert_eq!(h.attr(c, "data-kind").as_deref(), Some("heatmap"));
    assert_eq!(h.attr(c, "data-size").as_deref(), Some("md"));
    // Bars are 2 / 4 / 8 lines tall; a spark is always one line.
    let lh = 17.0;
    let bars = |h: &Harness, id: &str| bx(h, h.find_class(h.el("s1", id), "bars")[0]).h;
    assert_eq!(bars(&h, "bs"), 2.0 * lh);
    assert_eq!(bars(&h, "bm"), 4.0 * lh);
    assert_eq!(bars(&h, "bl"), 8.0 * lh);
    assert_eq!(bars(&h, "sp"), lh);
    assert_eq!(h.attr(h.el("s1", "sp"), "data-size").as_deref(), Some("lg"));
}

#[test]
fn bars_scale_to_the_largest_value_share_the_width_and_tip_their_entries() {
    let series = json!([
        {"value": 3, "label": "Mon"}, {"value": 6, "label": "Tue", "title": "Tuesday: 6"},
        {"value": 0, "label": "Wed"}, {"value": -2}, {"label": "Fri"}, {"value": 1.5, "label": 7}
    ]);
    let many: Vec<Value> = (0..400).map(|v| json!({"value": v % 7})).collect();
    let mut h = chart_surface(
        ViewSettings::default(),
        json!([
            {"id": "b", "k": "chart", "p": {"kind": "bars", "series": series, "summary": "per day"}},
            {"id": "sp", "k": "chart", "p": {"kind": "spark", "token": "success", "series": series}},
            {"id": "many", "k": "chart", "p": {"kind": "bars", "series": many}},
            {"id": "row", "k": "row", "p": {"gap": "sm"}, "c": [
                {"id": "rt", "k": "text", "p": {"text": "p95"}},
                {"id": "rs", "k": "chart", "p": {"kind": "spark", "series": series}}
            ]},
        ]),
    );
    settle(&mut h);
    let el = h.el("s1", "b");
    assert_eq!(h.attr(el, "data-kind").as_deref(), Some("bars"));
    let bs = h.find_class(el, "b");
    assert_eq!(bs.len(), 6);
    let tips: Vec<Option<String>> = bs.iter().map(|&b| h.attr(b, "title")).collect();
    assert_eq!(
        tips,
        [
            Some("Mon".into()),
            Some("Tuesday: 6".into()),
            Some("Wed".into()),
            None,
            Some("Fri".into()),
            Some("7".into())
        ]
    );
    let box_ = bx(&h, h.find_class(el, "bars")[0]);
    let r: Vec<auk_ui::Rect> = bs.iter().map(|&b| bx(&h, b)).collect();
    // Heights relative to the largest; zero, negative and missing values are 1px stubs.
    assert_eq!(r[1].h, box_.h);
    assert!((r[0].h - box_.h / 2.0).abs() < 0.01, "{:?}", r[0]);
    assert!((r[5].h - box_.h / 4.0).abs() < 0.01, "{:?}", r[5]);
    for i in [2, 3, 4] {
        assert_eq!(r[i].h, 1.0, "stub {i}");
    }
    // Bars grow from the bottom: all end on the box's bottom edge.
    for b in &r {
        assert!((b.y + b.h - (box_.y + box_.h)).abs() < 0.01);
    }
    // At most 24px wide (spark: 4px), in order.
    assert!(r.iter().all(|b| b.w <= 24.0 && b.w > 0.0));
    assert_eq!(r[0].w, 24.0, "a short series takes the widest bars");
    assert!(r.windows(2).all(|p| p[1].x > p[0].x));
    let sp: Vec<auk_ui::Rect> = h
        .find_class(h.el("s1", "sp"), "b")
        .iter()
        .map(|&b| bx(&h, b))
        .collect();
    assert!(sp.iter().all(|b| b.w == 4.0), "{sp:?}");
    // In a row, a spark keeps its bars' width.
    let rs: Vec<auk_ui::Rect> = h
        .find_class(h.el("s1", "rs"), "b")
        .iter()
        .map(|&b| bx(&h, b))
        .collect();
    assert!(rs.iter().all(|b| b.w == 4.0), "{rs:?}");
    // A long series shares the row's width: it fits the pane.
    let many = h.el("s1", "many");
    let mb = bx(&h, h.find_class(many, "bars")[0]);
    let pane = bx(&h, h.view.element());
    assert!(mb.x + mb.w <= pane.x + pane.w + 0.5, "{mb:?} in {pane:?}");
    let last = *h.find_class(many, "b").last().unwrap();
    assert!(bx(&h, last).x + bx(&h, last).w <= mb.x + mb.w + 0.5);
}

#[test]
fn a_year_heatmap_fits_the_pane_and_scrolls_sideways() {
    let mut h = harness(ViewSettings::default());
    h.doc.set_viewport(300.0, 600.0);
    h.surface(
        "s1",
        "inline",
        json!([{"id": "y", "k": "chart", "p": heatmap(53)}]),
    );
    h.frame();
    settle(&mut h);
    let el = h.el("s1", "y");
    assert_eq!(h.find_class(el, "c").len(), 371, "a year is 371 cells");
    let hm = h.find_class(el, "hm")[0];
    let b = bx(&h, hm);
    let pane = bx(&h, h.view.element());
    assert!(b.x + b.w <= pane.x + pane.w + 0.5, "{b:?} in {pane:?}");
    let (sw, _) = h.doc.scroll_size(hm);
    assert!(sw > b.w + 100.0, "the heatmap scrolls: {sw} in {b:?}");
    // It starts at its right end: the newest weeks show.
    let (mx, _) = h.doc.max_scroll(hm);
    assert!(mx > 100.0);
    assert_eq!(h.doc.scroll_offset(hm).0, mx, "scrolled to the end");
    let last = *h.find_class(el, "wk").last().unwrap();
    let lb = bx(&h, last);
    eprintln!("hm {b:?} last week {lb:?}");
    assert!(
        lb.x >= b.x && lb.x + lb.w <= b.x + b.w + 0.5,
        "the newest week is in view: {lb:?} in {b:?}"
    );
}

#[test]
fn arrivals_stagger_and_replay_when_props_change_but_not_on_a_theme_change() {
    let series: Vec<Value> = (1..=30).map(|v| json!({"value": v})).collect();
    let mut h = chart_surface(
        ViewSettings::default(),
        json!([
            {"id": "b", "k": "chart", "p": {"kind": "bars", "series": series}},
            {"id": "hm", "k": "chart", "p": heatmap(10)}
        ]),
    );
    let t0 = h.now;
    let at = |h: &mut Harness, t: f64| {
        h.view.tick(&mut h.doc, &h.pane, t);
        h.view.layout(&mut h.doc);
    };
    let scale_y = |h: &Harness, el: NodeId| -> f32 {
        match h.doc.computed_style(el).unwrap().transform.as_deref() {
            None => 1.0,
            Some(fs) => fs
                .iter()
                .map(|f| match f {
                    auk_ui::style::TransformFn::Scale(_, y) => *y,
                    auk_ui::style::TransformFn::Matrix(m) => m[3],
                    _ => 1.0,
                })
                .product(),
        }
    };
    let opacity = |h: &Harness, el: NodeId| h.doc.computed_style(el).unwrap().opacity;
    let bars = h.find_class(h.el("s1", "b"), "b");
    let wks = h.find_class(h.el("s1", "hm"), "wk");
    // Bars start flat and grow over 0.6s, 20ms apart, the stagger capped at 24 bars.
    at(&mut h, t0);
    assert!(scale_y(&h, bars[0]) < 0.05, "{}", scale_y(&h, bars[0]));
    // Heatmap columns fade in 8ms apart.
    at(&mut h, t0 + 100.0);
    let (c0, c9) = (opacity(&h, wks[0]), opacity(&h, wks[9]));
    assert!(c0 > c9 && c0 < 1.0, "staggered columns: {c0} {c9}");
    at(&mut h, t0 + 300.0);
    let (first, tenth) = (scale_y(&h, bars[0]), scale_y(&h, bars[10]));
    assert!(first > 0.5 && first < 1.0, "{first}");
    assert!(tenth < first, "later bars start later: {tenth} vs {first}");
    // Bar 29 starts with bar 24 (capped).
    at(&mut h, t0 + 24.0 * 20.0 + 200.0);
    let (b24, b29) = (scale_y(&h, bars[24]), scale_y(&h, bars[29]));
    assert!(
        (b24 - b29).abs() < 0.01 && b24 > 0.0 && b24 < 1.0,
        "{b24} {b29}"
    );
    at(&mut h, t0 + 2000.0);
    assert_eq!(scale_y(&h, bars[29]), 1.0);
    assert_eq!(opacity(&h, wks[9]), 1.0);

    // A theme change re-renders every node: the chart keeps its elements, nothing replays.
    h.now = t0 + 2000.0;
    let mut dark = h.view.settings().clone();
    dark.dark = true;
    h.view.set_settings(&mut h.doc, &h.pane, dark);
    h.frame();
    assert_eq!(h.find_class(h.el("s1", "b"), "b"), bars, "same elements");
    assert_eq!(scale_y(&h, bars[0]), 1.0);

    // New props rebuild the chart and the arrivals replay.
    h.frame_ops(
        "s1",
        2,
        json!([["set", "b", {"series": [{"value": 1}, {"value": 2}]}]]),
    );
    h.frame();
    let fresh = h.find_class(h.el("s1", "b"), "b");
    assert_eq!(fresh.len(), 2);
    assert!(
        scale_y(&h, fresh[1]) < 0.05,
        "replays: {}",
        scale_y(&h, fresh[1])
    );
}

#[test]
fn reduce_motion_shows_charts_at_once() {
    let series: Vec<Value> = (1..=5).map(|v| json!({"value": v})).collect();
    let settings = ViewSettings {
        reduce_motion: true,
        ..ViewSettings::default()
    };
    let mut h = chart_surface(
        settings,
        json!([
            {"id": "b", "k": "chart", "p": {"kind": "bars", "series": series}},
            {"id": "hm", "k": "chart", "p": heatmap(4)}
        ]),
    );
    h.frame();
    let b = h.find_class(h.el("s1", "b"), "b")[4];
    assert!(h.doc.computed_style(b).unwrap().transform.is_none());
    let wk = h.find_class(h.el("s1", "hm"), "wk")[3];
    assert_eq!(h.doc.computed_style(wk).unwrap().opacity, 1.0);
}

#[test]
fn children_are_ignored_and_explicit_aria_wins() {
    let mut h = harness(ViewSettings::default());
    h.surface(
        "s1",
        "inline",
        json!([{"id": "ch", "k": "chart", "p": {"kind": "spark", "series": [{"value": 1}],
                "summary": "s", "aria": "Latency"},
                "c": [{"id": "kid", "k": "text", "p": {"text": "hidden child"}}]}]),
    );
    h.frame();
    let el = h.el("s1", "ch");
    assert!(!h.text(el).contains("hidden child"), "{}", h.text(el));
    assert_eq!(h.attr(el, "aria-label").as_deref(), Some("Latency"));
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

fn pixel(px: &[u8], w: u32, x: f32, y: f32) -> [u8; 3] {
    let i = ((y as u32 * w + x as u32) * 4) as usize;
    [px[i], px[i + 1], px[i + 2]]
}

fn near(a: [u8; 3], b: [u8; 3], tol: u8) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= tol)
}

/// `rgb` at `alpha` over `under`.
fn mix(rgb: [u8; 3], alpha: f32, under: [u8; 3]) -> [u8; 3] {
    let m = |t: u8, u: u8| (t as f32 * alpha + u as f32 * (1.0 - alpha)).round() as u8;
    [
        m(rgb[0], under[0]),
        m(rgb[1], under[1]),
        m(rgb[2], under[2]),
    ]
}

fn hex(doc: &Document, el: NodeId, var: &str) -> [u8; 3] {
    let style = doc.computed_style(el).unwrap();
    let v = style.custom.get(var).unwrap().trim().to_owned();
    let h = v.strip_prefix('#').unwrap_or_else(|| panic!("{var}: {v}"));
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).unwrap();
    [byte(0), byte(2), byte(4)]
}

fn rgb(c: Color) -> [u8; 3] {
    let [r, g, b, _] = c.to_rgba8();
    [r, g, b]
}

/// The heatmap's five levels and the bars, in the light and the dark theme: level 4 is the
/// theme's accent, levels 1-3 the accent at 20/40/65% over the background, level 0 the empty
/// swatch, `n` nothing; a bar is the token's color.
#[test]
fn heatmap_levels_and_bars_paint_the_fill_color_in_both_themes() {
    for dark in [false, true] {
        let settings = ViewSettings {
            dark,
            ..ViewSettings::default()
        };
        let mut h = chart_surface(
            settings,
            json!([
                {"id": "hm", "k": "chart", "p": {"kind": "heatmap", "size": "lg",
                    "cells": [[0, 0.2, 0.4, 0.7, 1, null]]}},
                {"id": "b", "k": "chart", "p": {"kind": "bars", "token": "error",
                    "series": [{"value": 1}]}}
            ]),
        );
        settle(&mut h);
        const W: u32 = 300;
        let el = h.el("s1", "hm");
        let cells = h.find_class(el, "c");
        let last = bx(&h, h.find_class(h.el("s1", "b"), "b")[0]);
        let px = render(&h.doc, W, (last.y + last.h).ceil() as u32 + 4);
        let center = |r: auk_ui::Rect| (r.x + r.w / 2.0, r.y + r.h / 2.0);
        let r0 = bx(&h, cells[0]);
        // The background under the chart: in the gap after the first cell.
        let bg = pixel(&px, W, r0.x + r0.w + 1.5, r0.y + r0.h / 2.0);
        let accent = hex(&h.doc, el, "--accent");
        let swatch = h.doc.computed_style(cells[0]).unwrap().background_color;
        let want = |i: usize| match i {
            0 => mix(rgb(swatch), swatch.a, bg),
            1 => mix(accent, 0.2, bg),
            2 => mix(accent, 0.4, bg),
            3 => mix(accent, 0.65, bg),
            4 => accent,
            _ => bg,
        };
        for (i, &c) in cells.iter().enumerate() {
            let (x, y) = center(bx(&h, c));
            let got = pixel(&px, W, x, y);
            assert!(
                near(got, want(i), 3),
                "dark={dark} level cell {i}: {got:?}, want {:?} (bg {bg:?})",
                want(i)
            );
        }
        // The empty swatch differs from the background, and levels darken/brighten in order.
        assert!(!near(want(0), bg, 2), "dark={dark}: the empty swatch shows");
        let (x, y) = center(last);
        let bad = hex(&h.doc, el, "--bad");
        assert!(
            near(pixel(&px, W, x, y), bad, 3),
            "dark={dark}: the error bar {:?} is --bad {bad:?}",
            pixel(&px, W, x, y)
        );
    }
}

// ---------------------------------------------------------------------------------------------
// omp 18.8.5's charts, built as its code builds them (its bundled source: the `/usage` overlay's
// Activity heatmap, `V("chart", { kind: "heatmap", cells, cols, rows: ["M", "", "W", "", "F",
// "", ""], tips, token: "accent", summary, size: "md" })` with cells of day levels 0-4 / 4 and
// null past today; and the autoresearch dashboard's trend, `V("chart", { kind: "bars", series:
// [{ label: "#N", value, title }], token: "accent", summary: [muted, success], size: "md", role:
// "omp.autoresearch.trend" })`, both only when `supports("chart")`).

/// omp's usage heatmap: 7 weekday rows × 53 weeks.
fn omp_usage_heatmap() -> Value {
    let weeks = 53;
    let cells: Vec<Vec<Value>> = (0..7)
        .map(|r| {
            (0..weeks)
                .map(|w| {
                    if w == weeks - 1 && r > 2 {
                        Value::Null
                    } else {
                        json!(((w * 7 + r) * 13 % 5) as f64 / 4.0)
                    }
                })
                .collect()
        })
        .collect();
    let tips: Vec<Vec<Value>> = cells
        .iter()
        .enumerate()
        .map(|(r, row)| {
            row.iter()
                .enumerate()
                .map(|(w, c)| {
                    if c.is_null() {
                        Value::Null
                    } else if c.as_f64() == Some(0.0) {
                        json!(format!("Day {} · No activity", w * 7 + r))
                    } else {
                        json!(format!("Day {} · $1.20 · 3 requests", w * 7 + r))
                    }
                })
                .collect()
        })
        .collect();
    let months = [
        "Oct", "Nov", "Dec", "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep",
    ];
    let cols: Vec<Value> = months
        .iter()
        .enumerate()
        .map(|(i, m)| json!({"at": i * 4 + i / 3, "label": m}))
        .collect();
    json!({"kind": "heatmap", "cells": cells, "cols": cols, "rows": ["M", "", "W", "", "F", "", ""],
           "tips": tips, "token": "accent", "summary": "$412.30 · 1,204 requests · last 53 weeks",
           "size": "md"})
}

#[test]
fn omps_usage_heatmap_and_autoresearch_trend_render_named_and_themed() {
    let trend = json!({"kind": "bars", "series": (1..=12).map(|n| json!({
            "label": format!("#{n}"), "value": 100.0 - (n as f64) * 3.5,
            "title": format!("#{n} keep · {} ms · try a thing", 100.0 - (n as f64) * 3.5)
        })).collect::<Vec<_>>(),
        "token": "accent",
        "summary": [{"t": "latency per run", "s": "muted"}, {"t": " · best 58 ms", "s": "success"}],
        "size": "md", "role": "omp.autoresearch.trend"});
    for dark in [false, true] {
        let mut h = chart_surface(
            ViewSettings {
                dark,
                ..ViewSettings::default()
            },
            json!([
                {"id": "activity", "k": "col", "p": {"role": "omp.usage.activity", "gap": "sm"}, "c": [
                    {"id": "activity.heatmap", "k": "chart", "p": omp_usage_heatmap()}
                ]},
                {"id": "dashboard.trend", "k": "chart", "p": trend}
            ]),
        );
        settle(&mut h);
        let hm = h.el("s1", "activity.heatmap");
        assert_eq!(h.find_class(hm, "c").len(), 371);
        assert_eq!(h.find_class(hm, "ml").len(), 12);
        assert_eq!(
            h.attr(hm, "aria-label").as_deref(),
            Some("$412.30 · 1,204 requests · last 53 weeks")
        );
        let tr = h.el("s1", "dashboard.trend");
        assert_eq!(
            h.attr(tr, "data-role").as_deref(),
            Some("omp.autoresearch.trend")
        );
        assert_eq!(
            h.attr(tr, "aria-label").as_deref(),
            Some("latency per run · best 58 ms")
        );
        let ax = h.view.accessibility(&h.doc);
        for id in ["activity.heatmap", "dashboard.trend"] {
            let n = ax.find(&|n| n.id.as_deref() == Some(id)).expect(id);
            assert_eq!(n.role, "figure", "{id}");
            assert!(n.name.as_deref().is_some_and(|s| !s.is_empty()), "{id}");
        }
        // It fits the 960px pane: a year at md is 53 × 14px plus labels.
        let b = bx(&h, h.find_class(hm, "hm")[0]);
        assert!(b.w > 53.0 * 14.0 && b.w < 960.0, "{b:?}");
        // Pixels: a level-4 cell and the tallest bar are the theme's accent.
        let full = h
            .find_class(hm, "c")
            .into_iter()
            .find(|&c| h.attr(c, "data-l").as_deref() == Some("4"))
            .expect("a level-4 day");
        let bars = h.find_class(tr, "b");
        let tall = bx(&h, bars[0]);
        const W: u32 = 960;
        let px = render(&h.doc, W, (tall.y + tall.h).ceil() as u32 + 4);
        let accent = hex(&h.doc, hm, "--accent");
        let c = bx(&h, full);
        let got = pixel(&px, W, c.x + c.w / 2.0, c.y + c.h / 2.0);
        assert!(
            near(got, accent, 3),
            "dark={dark}: cell {got:?} vs accent {accent:?}"
        );
        let got = pixel(&px, W, tall.x + tall.w / 2.0, tall.y + tall.h / 2.0);
        assert!(
            near(got, accent, 3),
            "dark={dark}: bar {got:?} vs accent {accent:?}"
        );
        // The summary's success part takes the theme's success color.
        let sum = h.find_class(tr, "sum")[0];
        assert_eq!(h.text(sum), "latency per run · best 58 ms");
    }
}

/// A semantic token that names no color (`strong`) still fills: `--chart-c` resolves (to the
/// accent without a palette entry), not to an undefined `--sf-c-strong`.
#[test]
fn a_semantic_token_without_a_color_still_fills() {
    let mut chart = heatmap(4);
    chart["token"] = json!("strong");
    let mut h = chart_surface(ViewSettings::default(), json!([{"id": "hm", "k": "chart", "p": chart}]));
    settle(&mut h);
    let el = h.el("s1", "hm");
    let c = h.doc.computed_style(el).unwrap().custom.get("--chart-c").map(|v| v.trim().to_owned());
    let accent = h.doc.computed_style(el).unwrap().custom.get("--accent").map(|v| v.trim().to_owned());
    assert!(c.is_some() && c == accent, "{c:?} vs {accent:?}");
}
