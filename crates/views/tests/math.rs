//! TeX math: the engine on common TeX, its
//! errors, determinism and cost; the `math` kind and md math through the TSP engine, the views and
//! the UI engine; and the formulas painted offscreen in `currentColor` in both themes.

mod common;

use auk_ui::{Document, NodeId};
use auk_views::ViewSettings;
use auk_views::math::{self, Poll};
use common::Harness;
use serde_json::json;

/// TeX a chat answer uses: fractions, roots, scripts, Greek, operators, matrices, aligned
/// environments and other `\begin{env}` blocks.
const COMMON: &[&str] = &[
    r"\frac{a}{b}",
    r"\dfrac{n(n+1)}{2}",
    r"\sqrt{x^2 + y^2}",
    r"\sqrt[3]{8}",
    r"x_i^2",
    r"e^{i\pi} + 1 = 0",
    r"\alpha + \beta = \Gamma(\delta) \cdot \Omega",
    r"\sum_{k=1}^{n} k = \frac{n(n+1)}{2}",
    r"\int_0^\infty e^{-x^2}\,dx = \frac{\sqrt{\pi}}{2}",
    r"\prod_{p} \frac{1}{1-p^{-s}}",
    r"\lim_{x \to 0} \frac{\sin x}{x} = 1",
    r"a \le b \ne c \approx d \in \mathbb{R} \subseteq \mathcal{A}",
    r"\left( \frac{a}{b} \right)^2",
    r"\begin{pmatrix} 1 & 0 \\ 0 & 1 \end{pmatrix}",
    r"\begin{bmatrix} a & b \\ c & d \end{bmatrix}",
    r"\begin{vmatrix} a & b \\ c & d \end{vmatrix}",
    r"\begin{aligned} f(x) &= (x+1)^2 \\ &= x^2 + 2x + 1 \end{aligned}",
    r"\begin{cases} 1 & x > 0 \\ 0 & \text{otherwise} \end{cases}",
    r"\begin{array}{cc} a & b \\ c & d \end{array}",
    r"\hat{x} \vec{v} \bar{y} \tilde{z} \dot{a}",
    r"\binom{n}{k} = \frac{n!}{k!(n-k)!}",
    r"\mathbf{F} = m\mathbf{a}",
    r"\nabla \cdot \mathbf{E} = \frac{\rho}{\varepsilon_0}",
    r"\overbrace{1+2+\cdots+n}^{n}",
];

/// The share of a formula's pixels with ink.
fn ink(f: &math::Formula) -> f64 {
    let px = f.content.data.data.data();
    let n = px.len() / 4;
    let inked = px.chunks(4).filter(|p| p[3] > 64).count();
    inked as f64 / n as f64
}

#[test]
fn common_tex_typesets_with_ink_in_its_box() {
    for tex in COMMON {
        for display in [false, true] {
            let f =
                math::typeset_uncached(tex, display, 20).unwrap_or_else(|e| panic!("{tex}: {e}"));
            assert!(f.width > 0.3, "{tex}: width {}", f.width);
            assert!(f.height + f.depth > 0.4, "{tex}: {f:?}");
            let share = ink(&f);
            assert!(share > 0.02 && share < 0.8, "{tex}: ink {share}");
            // The picture is the box plus the pad, at 20px per em.
            let w = ((f.width + 2.0 * math::PAD) * 20.0).ceil() as u32;
            assert_eq!(f.content.data.width, w, "{tex}");
            // White coverage: the paint supplies the color.
            let px = f.content.data.data.data();
            assert!(
                px.chunks(4)
                    .all(|p| p[0] == p[3] && p[1] == p[3] && p[2] == p[3])
            );
        }
    }
}

#[test]
fn layout_follows_tex_structure() {
    let t = |tex: &str, display: bool| math::typeset_uncached(tex, display, 20).unwrap();
    let x = t("x", false);
    let frac = t(r"\frac{a}{b}", false);
    let dfrac = t(r"\frac{a}{b}", true);
    // A fraction stands above and below the baseline; display style is bigger than text style.
    assert!(
        frac.height > x.height && frac.depth > x.depth,
        "{frac:?} vs {x:?}"
    );
    assert!(dfrac.height + dfrac.depth > frac.height + frac.depth);
    // Limits go beside a text-style sum and above and below a display one.
    let sum_t = t(r"\sum_{k=1}^{n} k", false);
    let sum_d = t(r"\sum_{k=1}^{n} k", true);
    assert!(sum_d.height > sum_t.height && sum_d.width < sum_t.width + 0.5);
    // Matrices grow with their rows.
    let m2 = t(r"\begin{pmatrix} 1 \\ 2 \end{pmatrix}", true);
    let m4 = t(r"\begin{pmatrix} 1 \\ 2 \\ 3 \\ 4 \end{pmatrix}", true);
    assert!(m4.height + m4.depth > (m2.height + m2.depth) * 1.5);
    // The CSS sets the box in em and puts the baseline on the text's.
    let css = frac.css();
    assert!(
        css.contains("width: ") && css.contains("em; height: "),
        "{css}"
    );
    assert!(
        css.contains(&format!("vertical-align: -{:.2}", frac.depth + math::PAD)),
        "{css}"
    );
}

#[test]
fn bad_tex_is_an_error_naming_the_problem() {
    for (tex, says) in [
        (r"\frac{1}", "Unexpected end of input"),
        (r"x^", "Expected group after '^'"),
        (r"\begin{pmatrix} 1 & 2", "Expected & or"),
        (r"\left( x", "Expected '\\right'"),
        (r"\notacommand", "Undefined control sequence: \\notacommand"),
        ("{", "Expected '}'"),
    ] {
        let e = math::typeset_uncached(tex, false, 20).expect_err(tex);
        assert!(e.contains(says), "{tex}: {e}");
    }
    let long = "x+".repeat(math::MAX_TEX);
    assert!(
        math::typeset_uncached(&long, false, 20)
            .unwrap_err()
            .contains("too long")
    );
}

#[test]
fn typesetting_is_deterministic() {
    for tex in COMMON {
        let a = math::typeset_uncached(tex, true, 26).unwrap();
        let b = math::typeset_uncached(tex, true, 26).unwrap();
        assert_eq!(
            a.content.data.data.data(),
            b.content.data.data.data(),
            "{tex}"
        );
        assert_eq!((a.width, a.height, a.depth), (b.width, b.height, b.depth));
    }
}

#[test]
fn typesetting_is_cheap_and_cached() {
    // Warm the fonts.
    let _ = math::typeset_uncached("x", false, 28);
    let start = std::time::Instant::now();
    for tex in COMMON {
        math::typeset_uncached(tex, false, 28).unwrap();
    }
    let per = start.elapsed().as_secs_f64() * 1000.0 / COMMON.len() as f64;
    eprintln!("typeset: {per:.3} ms per formula");
    // Generous for debug builds; release is well under a millisecond.
    assert!(per < 25.0, "{per} ms per formula");
    // The cache hands back the same picture.
    let a = math::typeset(r"\frac{p}{q}", false, 28).unwrap();
    let b = math::typeset(r"\frac{p}{q}", false, 28).unwrap();
    assert!(std::sync::Arc::ptr_eq(&a, &b));
    // Long TeX typesets in the background.
    let long = format!("{}1", r"\frac{1}{2} + ".repeat(60));
    assert!(long.len() > math::INLINE_TEX);
    assert!(matches!(math::request(&long, true, 28), Poll::Pending));
    math::settle();
    assert!(matches!(math::request(&long, true, 28), Poll::Ready(Ok(_))));
}

// ---------------------------------------------------------------------------------------------
// The kind and md math in the views

/// A harness whose engine accepts `math` (T2).
fn harness(dark: bool) -> Harness {
    let mut config = auk_tsp::Config::default();
    config.kinds.push("math".into());
    let mut h = Harness::with(
        config,
        ViewSettings {
            dark,
            ..ViewSettings::default()
        },
    );
    h.hello(&[]);
    h
}

fn show(h: &mut Harness, nodes: serde_json::Value) {
    h.surface("s1", "inline", nodes);
    h.frame();
    assert!(h.errors().is_empty(), "{:?}", h.errors());
}

/// The single element child of `el`, which has tag `tag`.
fn only(h: &Harness, el: NodeId, tag: &str) -> NodeId {
    let kids: Vec<NodeId> = h
        .doc
        .children(el)
        .iter()
        .copied()
        .filter(|&c| h.doc.node(c).is_element())
        .collect();
    assert_eq!(kids.len(), 1, "one child of {}", h.class(el));
    let tagged = h
        .doc
        .node(kids[0])
        .element()
        .unwrap()
        .tag
        .as_str()
        .to_owned();
    assert_eq!(tagged, tag, "{}", h.class(kids[0]));
    kids[0]
}

/// The formula's `svg` (drawn: a picture painted in `currentColor`).
fn svg_of(h: &Harness, wrapper: NodeId) -> NodeId {
    let svg = only(h, wrapper, "svg");
    assert!(h.doc.image(svg).is_some(), "the svg has its picture");
    assert!(h.doc.image_tint(svg), "the picture paints in currentColor");
    svg
}

#[test]
fn the_math_kind_draws_inline_and_display_formulas_as_the_book_shows() {
    let mut h = harness(true);
    show(
        &mut h,
        json!([
            {"id": "d", "k": "math", "p": {"text": "e^{i\\pi} + 1 = 0", "display": true}},
            {"id": "i", "k": "math", "p": {"text": "\\sum_{k=1}^{n} k = \\frac{n(n+1)}{2}"}},
        ]),
    );
    // Display: .sf-math.display > .md > div.math-display[data-tex] > svg.
    let d = h.el("s1", "d");
    assert!(h.class(d).starts_with("sf sf-math") && h.class(d).contains("display"));
    let md = only(&h, d, "div");
    assert_eq!(h.class(md), "md");
    let block = only(&h, md, "div");
    assert_eq!(h.class(block), "math-display");
    assert_eq!(
        h.attr(block, "data-tex").as_deref(),
        Some("e^{i\\pi} + 1 = 0")
    );
    let dsvg = svg_of(&h, block);
    // Inline: .sf-math > .md > p > span.math[data-tex] > svg.
    let i = h.el("s1", "i");
    assert!(!h.class(i).contains("display"));
    let p = only(&h, only(&h, i, "div"), "p");
    let span = only(&h, p, "span");
    assert_eq!(h.class(span), "math");
    assert_eq!(
        h.attr(span, "data-tex").as_deref(),
        Some("\\sum_{k=1}^{n} k = \\frac{n(n+1)}{2}")
    );
    let isvg = svg_of(&h, span);

    // Display is centered in the node's width; inline sits at the node's start.
    let node = h.doc.border_box(d).unwrap();
    let b = h.doc.border_box(dsvg).unwrap();
    let mid = b.x + b.w / 2.0;
    assert!(
        (mid - (node.x + node.w / 2.0)).abs() < 2.0,
        "{b:?} in {node:?}"
    );
    let ib = h.doc.border_box(isvg).unwrap();
    let inode = h.doc.border_box(i).unwrap();
    assert!(
        (ib.x - inode.x).abs() < 1.0,
        "{ib:?} at the start of {inode:?}"
    );
    // Sized in em of 1.1 × 13px: the inline sum (text style: limits beside, a small fraction)
    // is wider than tall, and both fit in their nodes.
    assert!(ib.w > ib.h && ib.h > 13.0 && ib.h < 45.0, "{ib:?}");
    assert!(b.h > 13.0 && b.h <= node.h + 0.5, "{b:?} in {node:?}");
    // The box is the formula's in em, as typeset.
    let f = math::typeset("e^{i\\pi} + 1 = 0", true, math::em_px(13.0, 1.0, 1.0)).unwrap();
    let em = 13.0 * f64::from(math::SCALE);
    assert!(
        ((f.width + 2.0 * math::PAD) * em - f64::from(b.w)).abs() < 0.6,
        "{f:?} {b:?}"
    );
}

#[test]
fn display_math_drops_blank_lines_and_inline_math_reads_newlines_as_spaces() {
    assert_eq!(math::normalize("a\n\n  \nb", true), "a\nb");
    assert_eq!(math::normalize("a\nb\r\nc", false), "a b c");
    let mut h = harness(true);
    show(
        &mut h,
        json!([
            {"id": "d", "k": "math", "p": {"text": "\\begin{aligned} a &= b \\\\\n\n c &= d \\end{aligned}", "display": true}},
            {"id": "i", "k": "math", "p": {"text": "a +\nb"}},
        ]),
    );
    let d = h.el("s1", "d");
    let block = h.find_class(d, "math-display")[0];
    assert_eq!(
        h.attr(block, "data-tex").as_deref(),
        Some("\\begin{aligned} a &= b \\\\\n c &= d \\end{aligned}")
    );
    svg_of(&h, block);
    let span = h.find_class(h.el("s1", "i"), "math")[0];
    assert_eq!(h.attr(span, "data-tex").as_deref(), Some("a + b"));
}

#[test]
fn a_formula_that_fails_shows_its_source_as_code_titled_with_the_error() {
    let mut h = harness(true);
    show(
        &mut h,
        json!([
            {"id": "i", "k": "math", "p": {"text": "\\frac{1}"}},
            {"id": "d", "k": "math", "p": {"text": "\\begin{pmatrix} 1", "display": true}},
        ]),
    );
    let i = h.el("s1", "i");
    let err = h.find_class(i, "math-err");
    assert_eq!(err.len(), 1);
    assert_eq!(h.doc.node(err[0]).element().unwrap().tag.as_str(), "code");
    assert_eq!(h.text(err[0]), "\\frac{1}");
    assert!(
        h.attr(err[0], "title")
            .unwrap()
            .contains("Unexpected end of input")
    );
    assert!(h.find_class(i, "math").is_empty(), "no formula wrapper");
    let d = h.el("s1", "d");
    let err = h.find_class(d, "math-err")[0];
    assert_eq!(h.text(err), "\\begin{pmatrix} 1");
    assert!(h.attr(err, "title").unwrap().contains("Expected"));
    // Errors paint in --bad, not the formula's color.
    let bad = h.doc.computed_style(err).unwrap().color;
    let t1 = h
        .doc
        .computed_style(h.find_class(d, "md")[0])
        .unwrap()
        .color;
    assert_ne!(bad, t1);
}

#[test]
fn md_math_takes_every_delimiter_of_omps_dialect() {
    let mut h = harness(true);
    let text = "Inline $x^2$ and \\(\\alpha_1\\) here, `\\(code\\)` and \\\\(not\\\\) stay.\n\n\
                $$\\frac{a}{b}$$\n\n\
                \\[\n\\sqrt{2}\n\\]\n\n\
                \\begin{aligned}\nf(x) &= x^2 \\\\\n&= x \\cdot x\n\\end{aligned}\n\n\
                ```\n\\[ not math \\]\n```\n\n\
                Bad $\\frac{1}$ one.";
    show(&mut h, json!([{"id": "m", "k": "md", "p": {"text": text}}]));
    let m = h.el("s1", "m");
    let inline: Vec<String> = h
        .find_class(m, "math")
        .into_iter()
        .map(|e| {
            svg_of(&h, e);
            h.attr(e, "data-tex").unwrap()
        })
        .collect();
    assert_eq!(inline, ["x^2", "\\alpha_1"]);
    let display: Vec<String> = h
        .find_class(m, "math-display")
        .into_iter()
        .map(|e| {
            svg_of(&h, e);
            h.attr(e, "data-tex").unwrap()
        })
        .collect();
    assert_eq!(
        display,
        [
            "\\frac{a}{b}",
            "\\sqrt{2}",
            "\\begin{aligned}\nf(x) &= x^2 \\\\\n&= x \\cdot x\n\\end{aligned}"
        ]
    );
    let all = h.text(m);
    assert!(
        all.contains("\\(code\\)"),
        "a code span stays literal: {all}"
    );
    assert!(
        all.contains("\\(not\\)"),
        "escaped delimiters stay text: {all}"
    );
    assert!(
        all.contains("\\[ not math \\]"),
        "a fence stays literal: {all}"
    );
    let err = h.find_class(m, "math-err");
    assert_eq!(err.len(), 1);
    assert_eq!(h.text(err[0]), "\\frac{1}");
    // Inline math sits in its line: the line grows no more than the formula needs.
    let x2 = h.find_class(m, "math")[0];
    let svg = only(&h, x2, "svg");
    let sb = h.doc.border_box(svg).unwrap();
    let p = h.doc.border_box(h.doc.parent(x2).unwrap()).unwrap();
    assert!(
        sb.y >= p.y - 0.5 && sb.y + sb.h <= p.y + p.h + 0.5,
        "{sb:?} in {p:?}"
    );
    assert!(p.h < 30.0, "one line: {p:?}");
}

/// Formulas of different depths in one line share the text's baseline.
#[test]
fn inline_formulas_sit_on_the_text_baseline() {
    let mut h = harness(true);
    show(
        &mut h,
        json!([{"id": "m", "k": "md", "p": {"text": "a $x$ b $y_j$ c $\\frac{1}{2}$ d"}}]),
    );
    let m = h.el("s1", "m");
    let em = 13.0 * f64::from(math::SCALE);
    let baselines: Vec<f64> = h
        .find_class(m, "math")
        .into_iter()
        .map(|e| {
            let tex = h.attr(e, "data-tex").unwrap();
            let f = math::typeset(&tex, false, math::em_px(13.0, 1.0, 1.0)).unwrap();
            let b = h.doc.border_box(svg_of(&h, e)).unwrap();
            f64::from(b.y + b.h) - (f.depth + math::PAD) * em
        })
        .collect();
    assert_eq!(baselines.len(), 3);
    for b in &baselines {
        assert!((b - baselines[0]).abs() < 0.75, "{baselines:?}");
    }
}

#[test]
fn a_long_formula_typesets_in_the_background_then_fills_in() {
    let mut h = harness(true);
    let tex = format!("{}1", "\\frac{x}{y} + ".repeat(45));
    assert!(tex.len() > math::INLINE_TEX);
    show(
        &mut h,
        json!([
            {"id": "k", "k": "math", "p": {"text": tex, "display": true}},
            {"id": "m", "k": "md", "p": {"text": format!("$${tex}$$")}},
        ]),
    );
    // Until then each shows its TeX.
    for id in ["k", "m"] {
        let el = h.el("s1", id);
        let pending = h.find_class(el, "pending");
        assert_eq!(pending.len(), 1, "{id} waits for the background");
        assert!(h.text(pending[0]).contains("\\frac{x}{y}"));
    }
    math::settle();
    h.now += 16.0;
    h.frame();
    for id in ["k", "m"] {
        let el = h.el("s1", id);
        assert!(h.find_class(el, "pending").is_empty(), "{id} filled in");
        let block = h.find_class(el, "math-display")[0];
        svg_of(&h, block);
    }
}

// ---------------------------------------------------------------------------------------------
// Painted

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

/// Inside `el`'s box: the page's color (the most common pixel), the pixel furthest from it, and
/// how many pixels differ from it.
fn ink_in(doc: &Document, px: &[u8], w: u32, el: NodeId) -> ([u8; 3], [u8; 3], usize) {
    let b = doc.border_box(el).unwrap();
    let mut counts: std::collections::HashMap<[u8; 3], usize> = Default::default();
    let mut pixels = Vec::new();
    for y in b.y.ceil() as u32..(b.y + b.h).floor() as u32 {
        for x in b.x.ceil() as u32..(b.x + b.w).floor() as u32 {
            let i = ((y * w + x) * 4) as usize;
            let p = [px[i], px[i + 1], px[i + 2]];
            *counts.entry(p).or_default() += 1;
            pixels.push(p);
        }
    }
    let page = *counts.iter().max_by_key(|(_, n)| **n).unwrap().0;
    let dist = |p: &[u8; 3]| {
        p.iter()
            .zip(page)
            .map(|(a, b)| u32::from(a.abs_diff(b)))
            .sum::<u32>()
    };
    let far = *pixels.iter().max_by_key(|p| dist(p)).unwrap();
    let inked = pixels.iter().filter(|p| dist(p) > 24).count();
    (page, far, inked)
}

fn rgb(c: auk_ui::css::values::Color) -> [u8; 3] {
    let [r, g, b, _] = c.to_rgba8();
    [r, g, b]
}

fn near(a: [u8; 3], b: [u8; 3], tol: u8) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= tol)
}

#[test]
fn formulas_paint_in_current_color_in_both_themes_and_follow_a_sheet() {
    for dark in [true, false] {
        let mut h = harness(dark);
        h.doc.set_viewport(400.0, 200.0);
        show(
            &mut h,
            json!([
                {"id": "d", "k": "math", "p": {"text": "\\frac{a+b}{\\sqrt{c}} \\cdot \\begin{pmatrix} 1 & 0 \\\\ 0 & 1 \\end{pmatrix}", "display": true}},
                {"id": "m", "k": "md", "p": {"text": "so $\\alpha^2$ is"}},
            ]),
        );
        let block = h.find_class(h.el("s1", "d"), "math-display")[0];
        let svg = svg_of(&h, block);
        let span = h.find_class(h.el("s1", "m"), "math")[0];
        let isvg = svg_of(&h, span);
        let px = render(&h.doc, 400, 200);
        for el in [svg, isvg] {
            let color = rgb(h.doc.computed_style(el).unwrap().color);
            let (page, far, inked) = ink_in(&h.doc, &px, 400, el);
            assert!(inked > 20, "dark {dark}: the formula has ink ({inked})");
            // The ink is the text color (--t1) at full coverage: light on dark, dark on light.
            assert!(
                near(far, color, 8),
                "dark {dark}: ink {far:?}, color {color:?} on {page:?}"
            );
            let lum = |c: [u8; 3]| c.iter().map(|&v| u32::from(v)).sum::<u32>();
            assert_eq!(
                lum(far) > lum(page),
                dark,
                "dark {dark}: {far:?} on {page:?}"
            );
        }
        // A color on .sf-math doesn't reach the formula; one on the formula does.
        h.doc.set_stylesheet(
            "test",
            auk_ui::SheetKind::BuiltIn,
            None,
            ".sf-math { color: rgb(0, 200, 0); } \
             .sf-math :is(.math, .math-display) { color: rgb(255, 0, 0); }",
        );
        h.frame();
        let px = render(&h.doc, 400, 200);
        let (_, far, _) = ink_in(&h.doc, &px, 400, svg);
        assert!(
            near(far, [255, 0, 0], 8),
            "dark {dark}: recolored ink {far:?}"
        );
    }
}

/// `$$…$$` amid a paragraph's text is `span.math.display` (inline-level in the `p`, drawn as a
/// centered line); alone in its paragraph it is `div.math-display`.
#[test]
fn display_math_amid_text_is_a_span_and_alone_a_div() {
    let mut h = harness(true);
    let text = "So $$a+b$$ holds.\n\n$$c+d$$\n";
    show(&mut h, json!([{"id": "m", "k": "md", "p": {"text": text}}]));
    let m = h.el("s1", "m");
    let amid: Vec<NodeId> = h
        .find_class(m, "display")
        .into_iter()
        .filter(|&e| h.class(e).split(' ').any(|c| c == "math"))
        .collect();
    assert_eq!(amid.len(), 1);
    let tag = |e: NodeId| h.doc.node(e).element().unwrap().tag.as_str().to_owned();
    assert_eq!(tag(amid[0]), "span");
    assert_eq!(h.attr(amid[0], "data-tex").as_deref(), Some("a+b"));
    assert_eq!(tag(h.doc.parent(amid[0]).unwrap()), "p");
    let block = h.find_class(m, "math-display");
    assert_eq!(block.len(), 1);
    assert_eq!(tag(block[0]), "div");
    assert_eq!(h.attr(block[0], "data-tex").as_deref(), Some("c+d"));
    // Drawn as a line of its own, centered like the block.
    let st = h.doc.computed_style(amid[0]).unwrap();
    assert_eq!(
        format!("{:?}", st.display).to_lowercase().contains("block"),
        true,
        "{:?}",
        st.display
    );
}

/// A lone `$` (a price) doesn't swallow a `\(…\)` after it: only `$…$` as pulldown-cmark reads
/// it pairs up (the closing `$` not followed by a digit).
#[test]
fn a_lone_dollar_leaves_later_delimiters_alone() {
    let mut h = harness(true);
    let text = "It costs $5, then \\(x^2\\) and $10 more.";
    show(&mut h, json!([{"id": "m", "k": "md", "p": {"text": text}}]));
    let m = h.el("s1", "m");
    let tex: Vec<String> = h
        .find_class(m, "math")
        .into_iter()
        .filter_map(|e| h.attr(e, "data-tex"))
        .collect();
    assert_eq!(tex, ["x^2"]);
    let all = h.text(m);
    assert!(all.contains("$5") && all.contains("$10"), "{all}");
}

/// A formula wider than [`math::MAX_SIDE`] device px at its size draws at a smaller one that
/// fits, keeping its size in em, instead of failing.
#[test]
fn a_formula_too_wide_to_draw_draws_coarser() {
    let wide = "x".repeat(600);
    let f = math::typeset_uncached(&wide, false, 40).expect("drawn, not refused");
    assert!(f.content.data.width <= math::MAX_SIDE && f.content.data.height <= math::MAX_SIDE);
    let narrow = math::typeset_uncached("xx", false, 40).unwrap();
    assert!(
        f.width > 200.0 * narrow.width / 2.0 * 0.9,
        "its size in em is kept: {}",
        f.width
    );
}
