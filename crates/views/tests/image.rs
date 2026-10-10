//! The `image` kind (§8.5, §12.3, §7.12; book:5925-5969): each raster format and animation,
//! sanitized SVG, the placeholder until a blob arrives and the redraw when it does, the built-in
//! picture, sizing, Reduce Motion and the zoom action, through the TSP engine, the views and the
//! UI engine.

mod common;

use std::io::Cursor;

use ::image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use auk_ui::NodeId;
use auk_views::image::{self as pictures, Loops, MAX_SIDE, sanitize_svg};
use auk_views::{Outcome, PointerEvent, ViewSettings};
use base64::Engine as _;
use common::{Harness, payload};
use serde_json::{Value, json};

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn sha(bytes: &[u8]) -> String {
    auk_tsp::blob::blob_id(bytes)
}

/// A `w`×`h` picture: red on the left half, blue on the right.
fn picture(w: u32, h: u32) -> RgbaImage {
    RgbaImage::from_fn(w, h, |x, _| {
        if x < w / 2 {
            Rgba([255, 0, 0, 255])
        } else {
            Rgba([0, 0, 255, 255])
        }
    })
}

fn encode(img: &RgbaImage, format: ImageFormat) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    let img = if format == ImageFormat::Jpeg {
        DynamicImage::ImageRgb8(DynamicImage::ImageRgba8(img.clone()).to_rgb8())
    } else {
        DynamicImage::ImageRgba8(img.clone())
    };
    img.write_to(&mut out, format).expect("encode");
    out.into_inner()
}

/// A GIF of three frames (50 ms, 5 ms, 200 ms), looping forever.
fn animated_gif() -> Vec<u8> {
    use ::image::codecs::gif::{GifEncoder, Repeat};
    let mut out = Vec::new();
    {
        let mut enc = GifEncoder::new(&mut out);
        enc.set_repeat(Repeat::Infinite).unwrap();
        let frames = [(255u8, 50u32), (128, 5), (0, 200)].map(|(v, ms)| {
            ::image::Frame::from_parts(
                RgbaImage::from_pixel(8, 4, Rgba([v, v, v, 255])),
                0,
                0,
                ::image::Delay::from_numer_denom_ms(ms, 1),
            )
        });
        enc.encode_frames(frames).unwrap();
    }
    out
}

/// An APNG of two frames (100 ms each) that plays once.
fn apng() -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, 6, 6);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_animated(2, 1).unwrap();
        let mut w = enc.write_header().unwrap();
        for v in [10u8, 200] {
            w.set_frame_delay(1, 10).unwrap();
            w.write_image_data(&[v; 6 * 6 * 4]).unwrap();
        }
        w.finish().unwrap();
    }
    out
}

/// An animated WebP of two lossless frames (80 ms each) that loops three times, assembled from
/// still frames: VP8X with the animation flag, ANIM, then an ANMF per frame.
fn animated_webp() -> Vec<u8> {
    fn chunk(id: &[u8; 4], body: &[u8], out: &mut Vec<u8>) {
        out.extend_from_slice(id);
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(body);
        if body.len() % 2 == 1 {
            out.push(0);
        }
    }
    fn u24(v: u32, out: &mut Vec<u8>) {
        out.extend_from_slice(&v.to_le_bytes()[..3]);
    }
    let (w, h) = (4u32, 4u32);
    let mut body = b"WEBP".to_vec();
    let mut vp8x = vec![0x02 | 0x10, 0, 0, 0];
    u24(w - 1, &mut vp8x);
    u24(h - 1, &mut vp8x);
    chunk(b"VP8X", &vp8x, &mut body);
    let mut anim = vec![0, 0, 0, 0];
    anim.extend_from_slice(&3u16.to_le_bytes());
    chunk(b"ANIM", &anim, &mut body);
    for v in [30u8, 220] {
        let still = encode(
            &RgbaImage::from_pixel(w, h, Rgba([v, v, v, 255])),
            ImageFormat::WebP,
        );
        // The still file's VP8L chunk, whole.
        let at = still
            .windows(4)
            .position(|c| c == b"VP8L")
            .expect("a VP8L chunk");
        let len = u32::from_le_bytes(still[at + 4..at + 8].try_into().unwrap()) as usize;
        let mut frame = Vec::new();
        u24(0, &mut frame);
        u24(0, &mut frame);
        u24(w - 1, &mut frame);
        u24(h - 1, &mut frame);
        u24(80, &mut frame);
        frame.push(0b10); // no blending, no disposal
        frame.extend_from_slice(&still[at..at + 8 + len + (len & 1)]);
        chunk(b"ANMF", &frame, &mut body);
    }
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

fn harness_with(settings: ViewSettings) -> Harness {
    let mut h = Harness::with(auk_tsp::Config::default(), settings);
    h.hello(&[]);
    h
}

fn harness() -> Harness {
    harness_with(ViewSettings::default())
}

/// Sends a blob as the program would (`b`, no `id`).
fn send_blob(h: &mut Harness, bytes: &[u8], mime: Option<&str>) -> String {
    let params: Vec<(&str, &str)> = mime.map(|m| ("mime", m)).into_iter().collect();
    let p = payload("b", &params, &b64(bytes));
    h.pane.on_apc(&mut h.grid, &p);
    h.sync();
    sha(bytes)
}

/// A surface `s1` showing `nodes` in `main`.
fn show(h: &mut Harness, nodes: Value) {
    h.surface("s1", "inline", nodes);
    h.frame();
    assert!(h.errors().is_empty(), "{:?}", h.errors());
}

/// The node's picture element (`img.sf-img-frame` or `svg.sf-img-svg`), if drawn.
fn frame_el(h: &Harness, id: &str) -> Option<NodeId> {
    let el = h.el("s1", id);
    h.find_class(el, "sf-img-frame")
        .into_iter()
        .chain(h.find_class(el, "sf-img-svg"))
        .next()
}

fn drawn(h: &Harness, id: &str) -> NodeId {
    let el = h.el("s1", id);
    let class = h.class(el);
    assert!(!class.contains("missing"), "{id} is a placeholder: {class}");
    let f = frame_el(h, id).unwrap_or_else(|| panic!("{id} has no picture element"));
    assert!(
        h.doc.image(f).is_some(),
        "{id}'s picture element has no image"
    );
    f
}

#[test]
fn image_is_t1_and_advertised() {
    assert!(auk_tsp::T1_KINDS.contains(&"image"));
    assert!(auk_views::KINDS.contains(&"image"));
    assert!(
        auk_tsp::Config::default()
            .kinds
            .iter()
            .any(|k| k == "image")
    );
}

#[test]
fn png_jpeg_gif_and_webp_draw_at_their_natural_size() {
    for format in [
        ImageFormat::Png,
        ImageFormat::Jpeg,
        ImageFormat::Gif,
        ImageFormat::WebP,
    ] {
        let mut h = harness();
        let bytes = encode(&picture(40, 20), format);
        let id = send_blob(&mut h, &bytes, None);
        show(
            &mut h,
            json!([{"id": "img", "k": "image", "p": {"blob": id, "alt": "two halves"}}]),
        );
        let f = drawn(&h, "img");
        let image = h.doc.image(f).unwrap();
        assert_eq!(
            (image.data.width, image.data.height),
            (40, 20),
            "{format:?} decodes at its size"
        );
        assert_eq!(h.attr(f, "alt").as_deref(), Some("two halves"));
        assert!(h.class(h.el("s1", "img")).contains("sf-zoom"), "{format:?}");
        // The replaced box: the picture's size (the region zoom is 1 at a 13px font), plus the
        // 1px ring.
        let b = h.doc.border_box(f).unwrap();
        assert!(
            (b.w - 42.0).abs() < 0.01 && (b.h - 22.0).abs() < 0.01,
            "{format:?} box {b:?}"
        );
        // The first pixel is red (the left half), whatever the codec.
        let px = &image.data.data.data()[..4];
        assert!(px[0] > 200 && px[2] < 60, "{format:?} pixel {px:?}");
    }
}

#[test]
fn declared_size_completes_from_the_ratio_and_shrinks_to_the_width() {
    let mut h = harness();
    let id = send_blob(&mut h, &encode(&picture(400, 100), ImageFormat::Png), None);
    show(
        &mut h,
        json!([
            {"id": "w", "k": "image", "p": {"blob": id, "w": 200}},
            {"id": "h", "k": "image", "p": {"blob": id, "h": 50}},
            {"id": "wh", "k": "image", "p": {"blob": id, "w": 100, "h": 100}},
            {"id": "big", "k": "image", "p": {"blob": id, "w": 4000}},
            {"id": "lines", "k": "image", "p": {"blob": id, "max": {"h": "2lines"}}},
        ]),
    );
    let content = |id: &str| {
        let b = h.doc.border_box(drawn(&h, id)).unwrap();
        (b.w - 2.0, b.h - 2.0)
    };
    let close = |a: (f32, f32), b: (f32, f32)| (a.0 - b.0).abs() < 0.5 && (a.1 - b.1).abs() < 0.5;
    assert!(close(content("w"), (200.0, 50.0)), "{:?}", content("w"));
    assert!(close(content("h"), (200.0, 50.0)), "{:?}", content("h"));
    // Both given: that box; `object-fit: contain` places the picture in it.
    assert!(close(content("wh"), (100.0, 100.0)), "{:?}", content("wh"));
    let (fit_x, fit_y, fit_w, fit_h) = auk_ui::image::fit(
        auk_ui::style::ObjectFit::Contain,
        (400.0, 100.0),
        (100.0, 100.0),
    );
    assert_eq!((fit_x, fit_y, fit_w, fit_h), (0.0, 37.5, 100.0, 25.0));
    // Wider than the pane: both axes shrink together to fit its width.
    let pane = h.doc.border_box(h.el("s1", "big")).unwrap().w;
    let big = content("big");
    assert!(
        big.0 <= pane && (big.0 / big.1 - 4.0).abs() < 0.05,
        "{big:?} in {pane}"
    );
    // `max.h` in lines bounds the height, keeping the ratio.
    let lh = ViewSettings::default().cell.1;
    let lines = content("lines");
    assert!(
        lines.1 <= 2.0 * lh + 0.5 && (lines.0 / lines.1 - 4.0).abs() < 0.05,
        "{lines:?}, a line is {lh}"
    );
}

#[test]
fn a_blob_that_arrives_after_the_frame_redraws_the_node() {
    let mut h = harness();
    let bytes = encode(&picture(30, 10), ImageFormat::Png);
    let id = sha(&bytes);
    show(
        &mut h,
        json!([{"id": "late", "k": "image", "p": {"blob": id, "alt": "Late"}}]),
    );
    let el = h.el("s1", "late");
    assert!(h.class(el).contains("missing"), "{}", h.class(el));
    let card = h.find_class(el, "sf-img-card")[0];
    assert_eq!(h.attr(card, "aria-busy").as_deref(), Some("true"));
    assert_eq!(
        h.attr(card, "title").as_deref(),
        Some("The image data has not arrived")
    );
    assert!(h.text(card).ends_with("Late"), "{:?}", h.text(card));
    // The blob arrives with no change to the node: it draws anyway (§7.12).
    send_blob(&mut h, &bytes, Some("image/png"));
    h.frame();
    drawn(&h, "late");
    assert!(h.find_class(h.el("s1", "late"), "sf-img-card").is_empty());
}

#[test]
fn a_blob_delivered_outside_the_stream_redraws_the_node() {
    let mut h = harness();
    let bytes = encode(&picture(12, 12), ImageFormat::Png);
    let id = sha(&bytes);
    show(
        &mut h,
        json!([{"id": "i", "k": "image", "p": {"blob": id}}]),
    );
    assert_eq!(h.pane.missing_blobs(), vec![id.clone()]);
    assert!(!h.pane.deliver_blob(&id, None, b"other bytes".to_vec()));
    assert!(h.pane.deliver_blob(&id.to_uppercase(), None, bytes));
    assert!(h.pane.missing_blobs().is_empty());
    h.sync();
    h.frame();
    drawn(&h, "i");
}

#[test]
fn undecodable_and_oversized_blobs_show_unavailable() {
    let mut h = harness();
    let junk = send_blob(&mut h, b"\x00\x01 not an image at all", None);
    let wide = send_blob(
        &mut h,
        &encode(&picture(MAX_SIDE + 1, 1), ImageFormat::Png),
        None,
    );
    let ok = send_blob(
        &mut h,
        &encode(&picture(MAX_SIDE, 1), ImageFormat::Png),
        None,
    );
    show(
        &mut h,
        json!([
            {"id": "junk", "k": "image", "p": {"blob": junk}},
            {"id": "wide", "k": "image", "p": {"blob": wide, "alt": "Too wide"}},
            {"id": "ok", "k": "image", "p": {"blob": ok}},
        ]),
    );
    for (id, text) in [("junk", "Image unavailable"), ("wide", "Too wide")] {
        let el = h.el("s1", id);
        assert!(h.class(el).contains("missing"), "{id}");
        assert!(!h.class(el).contains("sf-zoom"), "{id} doesn't zoom");
        let card = h.find_class(el, "sf-img-card")[0];
        assert!(h.text(card).ends_with(text), "{id}: {:?}", h.text(card));
        let title = h.attr(card, "title").unwrap();
        assert!(
            title.starts_with("Not an image format Auk can show"),
            "{title}"
        );
        assert_eq!(h.attr(card, "aria-busy"), None);
    }
    assert!(
        h.attr(h.find_class(h.el("s1", "wide"), "sf-img-card")[0], "title")
            .unwrap()
            .contains("4096")
    );
    drawn(&h, "ok");
}

#[test]
fn animated_gif_plays_on_the_view_clock_and_holds_under_reduce_motion() {
    let bytes = animated_gif();
    let pic = pictures::decode(&bytes, None, 1.0, "t").unwrap();
    assert_eq!(pic.loops, Loops::Forever);
    let delays: Vec<f64> = pic.frames.iter().map(|f| f.delay).collect();
    assert_eq!(
        delays,
        [50.0, 100.0, 200.0],
        "10 ms or less plays at 100 ms"
    );
    assert_eq!(pic.frame_at(0.0), (0, true));
    assert_eq!(pic.frame_at(60.0), (1, true));
    assert_eq!(pic.frame_at(200.0), (2, true));
    assert_eq!(pic.frame_at(360.0), (0, true), "it loops");

    let mut h = harness();
    let id = send_blob(&mut h, &bytes, Some("image/gif"));
    show(
        &mut h,
        json!([{"id": "a", "k": "image", "p": {"blob": id}}]),
    );
    let start = h.now;
    let f = drawn(&h, "a");
    let shown = |h: &Harness| h.doc.image(f).unwrap().data.data.data()[0];
    assert_eq!(shown(&h), 255);
    for (at, want) in [(60.0, 128), (200.0, 0), (360.0, 255), (420.0, 128)] {
        h.now = start + at;
        let next = h.view.tick(&mut h.doc, &h.pane, h.now);
        assert!(next.is_some(), "an animation asks for frames");
        assert_eq!(shown(&h), want, "at {at} ms");
    }

    let mut still = harness_with(ViewSettings {
        reduce_motion: true,
        ..ViewSettings::default()
    });
    let id = send_blob(&mut still, &bytes, None);
    show(
        &mut still,
        json!([{"id": "a", "k": "image", "p": {"blob": id}}]),
    );
    let f = drawn(&still, "a");
    still.now += 260.0;
    assert_eq!(
        still.view.tick(&mut still.doc, &still.pane, still.now),
        None,
        "nothing moves under Reduce Motion"
    );
    assert_eq!(still.doc.image(f).unwrap().data.data.data()[0], 255);
}

#[test]
fn apng_plays_once_and_holds_its_last_frame() {
    let pic = pictures::decode(&apng(), Some("image/png"), 1.0, "t").unwrap();
    assert_eq!(pic.frames.len(), 2);
    assert_eq!(pic.loops, Loops::Times(1));
    assert_eq!(pic.frame_at(50.0), (0, true));
    assert_eq!(pic.frame_at(150.0), (1, true));
    assert_eq!(
        pic.frame_at(250.0),
        (1, false),
        "done: the last frame holds"
    );
    assert_eq!(pic.frames[1].content.data.data.data()[0], 200);
}

#[test]
fn animated_webp_keeps_its_frames_and_loop_count() {
    let pic = pictures::decode(&animated_webp(), None, 1.0, "t").unwrap();
    assert_eq!(pic.frames.len(), 2);
    assert_eq!(pic.loops, Loops::Times(3));
    assert_eq!(pic.size, (4.0, 4.0));
    let delays: Vec<f64> = pic.frames.iter().map(|f| f.delay).collect();
    assert_eq!(delays, [80.0, 80.0]);
    assert_eq!(pic.frame_at(3.0 * 160.0), (1, false));
    assert_eq!(pic.frames[0].content.data.data.data()[0], 30);
    assert_eq!(pic.frames[1].content.data.data.data()[0], 220);
}

#[test]
fn svg_is_sanitized_to_the_allowlist() {
    let dirty = r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink"
        viewBox="0 0 20 10" onload="alert(1)" style="fill:red">
      <script>alert(1)</script>
      <title>t</title>
      <style>rect { fill: green }</style>
      <defs><linearGradient id="g"><stop offset="0" stop-color="#f00"/></linearGradient></defs>
      <rect id="r" width="10" height="10" fill="url(#g)" stroke="url(https://evil/x.svg#a)"
        onclick="x()" style="fill:blue" class="bar"/>
      <foreignObject><div xmlns="http://www.w3.org/1999/xhtml">html</div></foreignObject>
      <image href="https://evil/x.png" width="5" height="5"/>
      <use xlink:href="#r"/>
      <a href="https://evil"><rect width="1" height="1"/></a>
      <text x="1" y="9" font-size="4">hi &amp; bye</text>
      <mask id="m"><rect width="5" height="5" fill="#fff"/></mask>
      <g mask="url(#m)" transform="translate(1 1)"><circle cx="5" cy="5" r="2"/></g>
    </svg>"##;
    let clean = sanitize_svg(dirty, "p-").unwrap();
    for gone in [
        "script",
        "alert",
        "title",
        "style",
        "foreignObject",
        "html",
        "image",
        "use",
        "href",
        "onload",
        "onclick",
        "evil",
        "<a",
    ] {
        assert!(!clean.contains(gone), "{gone} survived: {clean}");
    }
    for kept in [
        "viewBox=\"0 0 20 10\"",
        "<linearGradient id=\"p-g\">",
        "fill=\"url(#p-g)\"",
        "id=\"p-r\"",
        "class=\"bar\"",
        "<text x=\"1\" y=\"9\" font-size=\"4\">hi &amp; bye</text>",
        "<mask id=\"p-m\">",
        "mask=\"url(#p-m)\"",
        "transform=\"translate(1 1)\"",
        "<circle cx=\"5\" cy=\"5\" r=\"2\">",
    ] {
        assert!(clean.contains(kept), "{kept} is missing: {clean}");
    }
    // A non-local url() takes its attribute with it.
    assert!(!clean.contains("stroke="), "{clean}");

    assert!(
        sanitize_svg("<svg xmlns=\"http://www.w3.org/2000/svg\"/>", "p").is_err(),
        "no size"
    );
    assert!(sanitize_svg("<svg width=\"4\" height=\"4\"/>", "p").is_ok());
    assert!(sanitize_svg("<html/>", "p").is_err());
    assert!(
        sanitize_svg(
            "<!DOCTYPE svg [<!ENTITY x \"boom\">]><svg width=\"1\" height=\"1\"><text>&x;</text></svg>",
            "p"
        )
        .is_err(),
        "no DTDs"
    );
    let huge = format!(
        "<svg width=\"1\" height=\"1\">{}</svg>",
        "<g/>".repeat(pictures::MAX_SVG / 4)
    );
    assert!(sanitize_svg(&huge, "p").unwrap_err().contains("256 KiB"));
}

#[test]
fn svg_draws_at_the_display_density() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10">
      <rect width="10" height="10" fill="#ff0000"/><rect x="10" width="10" height="10" fill="#0000ff"/>
      <script>document.body.innerHTML = ''</script></svg>"##;
    let mut h = harness();
    let id = send_blob(&mut h, svg.as_bytes(), Some("image/svg+xml"));
    h.doc.set_device_scale(2.0);
    show(
        &mut h,
        json!([{"id": "v", "k": "image", "p": {"blob": id, "alt": "Halves"}}]),
    );
    let f = drawn(&h, "v");
    assert!(h.class(f).contains("sf-img-svg"));
    assert_eq!(h.attr(f, "role").as_deref(), Some("img"));
    assert_eq!(h.attr(f, "aria-label").as_deref(), Some("Halves"));
    let img = h.doc.image(f).unwrap();
    assert_eq!((img.data.width, img.data.height), (40, 20), "drawn at 2×");
    assert_eq!(img.natural, (20.0, 10.0));
    let px = |x: u32, y: u32| {
        let i = ((y * img.data.width + x) * 4) as usize;
        img.data.data.data()[i..i + 4].to_vec()
    };
    assert_eq!(px(2, 2), [255, 0, 0, 255]);
    assert_eq!(px(38, 18), [0, 0, 255, 255]);
    // The box is the SVG's size in CSS px, plus nothing (SVGs have no ring).
    let b = h.doc.border_box(f).unwrap();
    assert!(
        (b.w - 20.0).abs() < 0.01 && (b.h - 10.0).abs() < 0.01,
        "{b:?}"
    );

    // Over 256 KiB, or without a size: unavailable.
    let big = format!(
        "<svg width=\"1\" height=\"1\">{}</svg>",
        " ".repeat(pictures::MAX_SVG)
    );
    let sizeless =
        "<svg xmlns=\"http://www.w3.org/2000/svg\"><rect width=\"1\" height=\"1\"/></svg>";
    let mut h = harness();
    let big = send_blob(&mut h, big.as_bytes(), Some("image/svg+xml"));
    let sizeless = send_blob(&mut h, sizeless.as_bytes(), Some("image/svg+xml"));
    show(
        &mut h,
        json!([
            {"id": "big", "k": "image", "p": {"blob": big}},
            {"id": "sizeless", "k": "image", "p": {"blob": sizeless}},
        ]),
    );
    for id in ["big", "sizeless"] {
        assert!(h.class(h.el("s1", id)).contains("missing"), "{id}");
    }
}

#[test]
fn the_builtin_omp_mark_draws_animated_and_never_zooms() {
    let mut h = harness();
    show(
        &mut h,
        json!([{"id": "logo", "k": "image", "p": {"builtin": "omp", "alt": "omp", "w": 128}}]),
    );
    let f = drawn(&h, "logo");
    let el = h.el("s1", "logo");
    assert!(
        !h.class(el).contains("sf-zoom"),
        "builtin images never zoom"
    );
    let b = h.doc.border_box(f).unwrap();
    assert!(
        (b.w - 128.0).abs() < 0.01 && (b.h - 128.0).abs() < 0.01,
        "{b:?}"
    );
    let first = h.doc.image(f).unwrap().data.clone();
    let start = h.now;
    h.now = start + 500.0;
    assert!(
        h.view.tick(&mut h.doc, &h.pane, h.now).is_some(),
        "the mark moves"
    );
    assert_ne!(h.doc.image(f).unwrap().data, first, "a later frame shows");
    let r = {
        let at = center(&h, f);
        h.view.click(&mut h.doc, &h.pane, at)
    };
    assert!(r.is_empty(), "{r:?}");

    // An unknown builtin is unavailable.
    let mut h = harness();
    show(
        &mut h,
        json!([{"id": "x", "k": "image", "p": {"builtin": "nope"}}]),
    );
    assert!(h.class(h.el("s1", "x")).contains("missing"));
}

fn center(h: &Harness, el: NodeId) -> PointerEvent {
    let b = h.doc.border_box(el).unwrap();
    PointerEvent {
        x: b.x + b.w / 2.0,
        y: b.y + b.h / 2.0,
        clicks: 1,
        ..PointerEvent::default()
    }
}

#[test]
fn a_click_zooms_through_the_parents_blob_images() {
    let mut h = harness();
    let a = send_blob(&mut h, &encode(&picture(20, 20), ImageFormat::Png), None);
    let b = send_blob(&mut h, &encode(&picture(60, 60), ImageFormat::Png), None);
    show(
        &mut h,
        json!([
            {"id": "att", "k": "row", "c": [
                {"id": "a", "k": "image", "p": {"blob": a, "alt": "First", "path": "/tmp/a.png"}},
                {"id": "logo", "k": "image", "p": {"builtin": "omp"}},
                {"id": "b", "k": "image", "p": {"blob": b, "title": "Second"}, "c": [
                    {"id": "badge", "k": "text", "p": {"text": "new"}}
                ]},
            ]},
            {"id": "own", "k": "image", "p": {"blob": a, "actions": {"click": "pick"}}},
        ]),
    );
    let out = {
        // Below the child drawn over it.
        let f = drawn(&h, "b");
        let mut at = center(&h, f);
        at.y = h.doc.border_box(f).unwrap().y + 55.0;
        h.view.click(&mut h.doc, &h.pane, at)
    };
    let [Outcome::Zoom(z)] = out.as_slice() else {
        panic!("{out:?}")
    };
    let ids: Vec<&str> = z.images.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(
        ids,
        ["a", "b"],
        "the parent's blob images, builtin left out"
    );
    assert_eq!(z.index, 1);
    assert_eq!(z.images[0].title.as_deref(), Some("First"));
    assert_eq!(z.images[0].open.as_deref(), Some("/tmp/a.png"));
    assert_eq!(
        z.images[1].title.as_deref(),
        Some("Second"),
        "title when there is no alt"
    );
    assert_eq!(z.images[1].blob, b);

    // A click on a child drawn over the picture doesn't zoom.
    let badge = h.el("s1", "badge");
    let out = {
        let at = center(&h, badge);
        h.view.click(&mut h.doc, &h.pane, at)
    };
    assert!(
        !out.iter().any(|o| matches!(o, Outcome::Zoom(_))),
        "{out:?}"
    );

    // A node with its own click runs that instead.
    assert!(!h.class(h.el("s1", "own")).contains("sf-zoom"));
    let out = {
        let at = center(&h, drawn(&h, "own"));
        h.view.click(&mut h.doc, &h.pane, at)
    };
    match out.as_slice() {
        [Outcome::Event { event, .. }] => assert_eq!(event.body("s1")["act"], "pick"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn plugin_sheet_data_urls_decode_and_style_through_the_built_in_loader() {
    // base64 PNG, percent-encoded SVG, and garbage.
    let png = encode(&picture(8, 4), ImageFormat::Png);
    let png_url = format!("data:image/png;base64,{}", b64(&png));
    let p = pictures::load_data_url(&png_url, 2.0).expect("a PNG data: URL");
    assert_eq!((p.data.width, p.data.height), (8, 4));
    let svg_url = "data:image/svg+xml,%3Csvg%20xmlns='http://www.w3.org/2000/svg'%20width='10'%20height='6'%3E%3Crect%20width='10'%20height='6'%20fill='red'/%3E%3C/svg%3E";
    let s = pictures::load_data_url(svg_url, 2.0).expect("an SVG data: URL");
    assert_eq!(s.picture_size(), (10.0, 6.0));
    assert!(pictures::load_data_url("data:image/png;base64,!!!", 2.0).is_none());
    assert!(pictures::load_data_url("https://example.com/x.png", 2.0).is_none());
    // The built-in sheets install the loader: a plugin sheet's picture loads on restyle.
    let mut h = Harness::new();
    h.hello(&[]);
    h.surface(
        "s1",
        "inline",
        json!([{"id": "t", "k": "text", "p": {"text": "x"}}]),
    );
    let errors = h.doc.set_stylesheet(
        "plugin:local:p:styles",
        auk_ui::SheetKind::Plugin,
        None,
        &format!(".sf-text {{ background-image: url({png_url}) }}"),
    );
    assert!(errors.is_empty(), "{errors:?}");
    h.frame();
    let t = h.el("s1", "t");
    let url = h.doc.computed_style(t).unwrap().background_image.clone();
    assert_eq!(url.as_deref(), Some(png_url.as_str()));
    assert!(h.doc.url_image(&png_url).is_some());
}

/// A GIF of two `w`×`h` frames (100 ms each), looping forever.
fn big_animated_gif(w: u32, h: u32) -> Vec<u8> {
    use ::image::codecs::gif::{GifEncoder, Repeat};
    let mut out = Vec::new();
    {
        let mut enc = GifEncoder::new(&mut out);
        enc.set_repeat(Repeat::Infinite).unwrap();
        let frames = [40u8, 220].map(|v| {
            ::image::Frame::from_parts(
                RgbaImage::from_pixel(w, h, Rgba([v, v, v, 255])),
                0,
                0,
                ::image::Delay::from_numer_denom_ms(100, 1),
            )
        });
        enc.encode_frames(frames).unwrap();
    }
    out
}

/// Amendment A9: a picture over 512×512 pixels decodes off the frame path. The node shows the
/// busy placeholder ("Decoding the image") while it decodes, then draws at its natural size
/// with no further TSP traffic; two nodes naming it share one decode.
#[test]
fn a_big_picture_decodes_in_the_background_and_draws_in() {
    let mut h = harness();
    // 900×600: a few KB encoded, but 540,000 pixels.
    let id = send_blob(&mut h, &encode(&picture(900, 600), ImageFormat::Png), None);
    show(
        &mut h,
        json!([
            {"id": "a", "k": "image", "p": {"blob": id, "alt": "Plot"}},
            {"id": "b", "k": "image", "p": {"blob": id}},
        ]),
    );
    for node in ["a", "b"] {
        let el = h.el("s1", node);
        // The decode may already be done; if not, the node is waiting for it.
        if h.class(el).contains("missing") {
            let card = h.find_class(el, "sf-img-card")[0];
            assert_eq!(h.attr(card, "title").as_deref(), Some("Decoding the image"));
            assert_eq!(h.attr(card, "aria-busy").as_deref(), Some("true"));
            // The clock watches for the picture: the tick asks for another frame, unless the
            // decode finished meanwhile and it drew the picture in.
            let next = h.view.tick(&mut h.doc, &h.pane, h.now);
            assert!(
                next.is_some() || !h.class(el).contains("missing"),
                "{node}: neither waiting nor drawn"
            );
        }
    }
    let sent = h.sent.len();
    pictures::settle();
    assert!(!pictures::decoding());
    h.now += 16.0;
    h.frame();
    for node in ["a", "b"] {
        let f = drawn(&h, node);
        let b = h.doc.border_box(f).unwrap();
        // Its natural size, plus the 1px ring.
        assert_eq!((b.w, b.h), (902.0, 602.0), "{node}");
    }
    assert_eq!(h.sent.len(), sent, "no TSP traffic");
    // Cached: a later node draws at once.
    show(
        &mut h,
        json!([{"id": "c", "k": "image", "p": {"blob": id}}]),
    );
    drawn(&h, "c");
}

/// Small pictures (512×512 pixels or fewer, 256 KiB or less encoded), SVGs and the built-in
/// picture decode at once: they never show the placeholder.
#[test]
fn small_pictures_svgs_and_builtins_never_wait() {
    let mut h = harness();
    let small = send_blob(&mut h, &encode(&picture(512, 512), ImageFormat::Png), None);
    let svg = send_blob(
        &mut h,
        b"<svg xmlns='http://www.w3.org/2000/svg' width='900' height='900'><rect width='900' height='900' fill='red'/></svg>",
        Some("image/svg+xml"),
    );
    show(
        &mut h,
        json!([
            {"id": "s", "k": "image", "p": {"blob": small}},
            {"id": "v", "k": "image", "p": {"blob": svg}},
            {"id": "o", "k": "image", "p": {"builtin": "omp"}},
        ]),
    );
    for node in ["s", "v", "o"] {
        drawn(&h, node);
    }
}

/// An animation over the threshold plays once it's decoded.
#[test]
fn a_big_animation_plays_after_its_background_decode() {
    let mut h = harness();
    let id = send_blob(&mut h, &big_animated_gif(600, 600), Some("image/gif"));
    show(
        &mut h,
        json!([{"id": "g", "k": "image", "p": {"blob": id}}]),
    );
    pictures::settle();
    h.now += 16.0;
    h.frame();
    let f = drawn(&h, "g");
    let first = h.doc.image(f).unwrap().data.data.data().to_vec();
    assert!(
        h.view.tick(&mut h.doc, &h.pane, h.now + 150.0).is_some(),
        "it animates"
    );
    h.view.layout(&mut h.doc);
    let later = h.doc.image(f).unwrap().data.data.data().to_vec();
    assert_ne!(first, later, "the second frame shows");
}
