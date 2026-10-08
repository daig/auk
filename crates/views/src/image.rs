//! Images (§8.5, §12.3; research/elements.md §2 and §7.3 `image`): blobs decoded into pictures for
//! the UI engine's replaced boxes, the sanitized SVG path, the frame clock of animations, and the
//! images the terminal ships (`builtin`).
//!
//! - **Raster:** PNG (and APNG), JPEG, GIF and WebP, at most [`MAX_SIDE`] px a side. Animated
//!   GIF, APNG and WebP keep every composited frame with its delay (10 ms or less plays at
//!   100 ms) and the file's loop count; past [`MAX_FRAMES`] frames or [`MAX_ANIMATION_BYTES`] of
//!   decoded pixels the first frame shows, still.
//! - **SVG:** at most [`MAX_SVG`] bytes, rebuilt from an element and attribute allowlist
//!   ([`sanitize_svg`]) and drawn by resvg at the display's density.
//! - Decoded pictures are shared by every view in the process, keyed by blob id (and density for
//!   vector pictures), least recently used first out past 256 MiB.

use std::borrow::Cow;
use std::collections::HashMap;
use std::io::Cursor;
use std::sync::{Arc, Mutex, OnceLock};

use auk_ui::ImageContent;
use image::codecs::gif::GifDecoder;
use image::codecs::png::PngDecoder;
use image::codecs::webp::WebPDecoder;
use image::metadata::LoopCount;
use image::{AnimationDecoder, DynamicImage, ImageFormat, ImageReader, RgbaImage};
use resvg::usvg::{self, roxmltree};
use vello::peniko::{Blob, ImageAlphaType, ImageData, ImageFormat as PixelFormat};

/// The largest raster image, per side (§8.5, §12.3).
pub const MAX_SIDE: u32 = 4096;
/// The largest SVG blob (§8.5).
pub const MAX_SVG: usize = 256 << 10;
/// Animations with more frames show their first frame, still.
pub const MAX_FRAMES: usize = 1024;
/// Animations with more decoded pixel bytes show their first frame, still.
pub const MAX_ANIMATION_BYTES: usize = 64 << 20;
/// Decoded pictures kept for reuse.
const CACHE_BYTES: usize = 256 << 20;
/// Frame delays at or under this play at [`SHORT_DELAY_AS`] (as browsers do).
const SHORT_DELAY: f64 = 10.0;
const SHORT_DELAY_AS: f64 = 100.0;

/// How many times an animation plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Loops {
    Forever,
    Times(u32),
}

/// One frame of a picture.
#[derive(Debug)]
pub struct Frame {
    pub content: ImageContent,
    /// How long it shows, in ms.
    pub delay: f64,
}

/// A decoded picture: one frame, or an animation.
#[derive(Debug)]
pub struct Picture {
    pub frames: Vec<Frame>,
    /// The picture's size in CSS px.
    pub size: (f32, f32),
    pub loops: Loops,
    /// Drawn from SVG (mounted as `.sf-img-svg`).
    pub svg: bool,
}

impl Picture {
    pub fn animated(&self) -> bool {
        self.frames.len() > 1
    }

    /// The frame showing `elapsed` ms after the picture first drew, and whether the animation is
    /// still running (it holds its last frame once its loops are done).
    pub fn frame_at(&self, elapsed: f64) -> (usize, bool) {
        let total: f64 = self.frames.iter().map(|f| f.delay).sum();
        if self.frames.len() < 2 || total <= 0.0 {
            return (0, false);
        }
        let elapsed = elapsed.max(0.0);
        if let Loops::Times(n) = self.loops
            && elapsed >= total * f64::from(n.max(1))
        {
            return (self.frames.len() - 1, false);
        }
        let mut t = elapsed % total;
        for (i, f) in self.frames.iter().enumerate() {
            if t < f.delay {
                return (i, true);
            }
            t -= f.delay;
        }
        (self.frames.len() - 1, true)
    }

    fn bytes(&self) -> usize {
        self.frames
            .iter()
            .map(|f| f.content.data.data.data().len())
            .sum()
    }
}

/// Decodes a blob's bytes. `density` is image pixels per CSS px for vector pictures (the region
/// zoom times the display scale); rasters keep their pixels. `prefix` names the picture's SVG ids.
/// The error says why the image can't show.
pub fn decode(
    bytes: &[u8],
    mime: Option<&str>,
    density: f32,
    prefix: &str,
) -> Result<Picture, String> {
    match sniff(bytes, mime) {
        Some(Kind::Raster(format)) => decode_raster(bytes, format),
        Some(Kind::Svg) => decode_svg(bytes, density, prefix),
        None => Err("not an image format Auk can show".into()),
    }
}

enum Kind {
    Raster(ImageFormat),
    Svg,
}

/// The format, from the bytes (the `mime` a program gives is a hint for SVG only).
fn sniff(bytes: &[u8], mime: Option<&str>) -> Option<Kind> {
    if let Ok(f) = image::guess_format(bytes) {
        return matches!(
            f,
            ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Gif | ImageFormat::WebP
        )
        .then_some(Kind::Raster(f));
    }
    let text = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let xml = text
        .iter()
        .find(|b| !b.is_ascii_whitespace())
        .is_some_and(|b| *b == b'<');
    (xml || mime.is_some_and(|m| m.eq_ignore_ascii_case("image/svg+xml"))).then_some(Kind::Svg)
}

fn decode_raster(bytes: &[u8], format: ImageFormat) -> Result<Picture, String> {
    let err = |e: image::ImageError| format!("can't decode the image: {e}");
    let (w, h) = ImageReader::with_format(Cursor::new(bytes), format)
        .into_dimensions()
        .map_err(err)?;
    if w == 0 || h == 0 {
        return Err("an empty image".into());
    }
    if w > MAX_SIDE || h > MAX_SIDE {
        return Err(format!("{w}×{h} px is over {MAX_SIDE} px a side"));
    }
    let (frames, loops) = match format {
        ImageFormat::Gif => {
            let d = GifDecoder::new(Cursor::new(bytes)).map_err(err)?;
            let loops = loop_count(d.loop_count());
            (frames(d.into_frames())?, loops)
        }
        ImageFormat::Png => {
            let d = PngDecoder::new(Cursor::new(bytes)).map_err(err)?;
            if d.is_apng().map_err(err)? {
                let a = d.apng().map_err(err)?;
                let loops = loop_count(a.loop_count());
                (frames(a.into_frames())?, loops)
            } else {
                (
                    still(DynamicImage::from_decoder(d).map_err(err)?),
                    Loops::Times(1),
                )
            }
        }
        ImageFormat::WebP => {
            let d = WebPDecoder::new(Cursor::new(bytes)).map_err(err)?;
            if d.has_animation() {
                (frames(d.into_frames())?, webp_loops(bytes))
            } else {
                (
                    still(DynamicImage::from_decoder(d).map_err(err)?),
                    Loops::Times(1),
                )
            }
        }
        _ => {
            let img = image::load_from_memory_with_format(bytes, format).map_err(err)?;
            (still(img), Loops::Times(1))
        }
    };
    Ok(Picture {
        frames,
        size: (w as f32, h as f32),
        loops,
        svg: false,
    })
}

fn loop_count(c: LoopCount) -> Loops {
    match c {
        LoopCount::Infinite => Loops::Forever,
        LoopCount::Finite(n) => Loops::Times(n.get()),
    }
}

/// An animated WebP's loop count, from its `ANIM` chunk (0 is forever).
fn webp_loops(bytes: &[u8]) -> Loops {
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let len = u32::from_le_bytes([bytes[at + 4], bytes[at + 5], bytes[at + 6], bytes[at + 7]])
            as usize;
        let body = at + 8;
        if id == b"ANIM" && body + 6 <= bytes.len() {
            return match u16::from_le_bytes([bytes[body + 4], bytes[body + 5]]) {
                0 => Loops::Forever,
                n => Loops::Times(u32::from(n)),
            };
        }
        at = body + len + (len & 1);
    }
    Loops::Forever
}

fn still(img: DynamicImage) -> Vec<Frame> {
    vec![Frame {
        content: content(img.into_rgba8(), 1.0, None),
        delay: 0.0,
    }]
}

/// An animation's frames, or its first frame alone past the frame or byte limits.
fn frames(it: image::Frames<'_>) -> Result<Vec<Frame>, String> {
    let mut out = Vec::new();
    let mut bytes = 0usize;
    for f in it {
        let f = f.map_err(|e| format!("can't decode the animation: {e}"))?;
        let (n, d) = f.delay().numer_denom_ms();
        let ms = f64::from(n) / f64::from(d.max(1));
        let delay = if ms <= SHORT_DELAY {
            SHORT_DELAY_AS
        } else {
            ms
        };
        let img = f.into_buffer();
        bytes += img.as_raw().len();
        out.push(Frame {
            content: content(img, 1.0, None),
            delay,
        });
        if out.len() > MAX_FRAMES || bytes > MAX_ANIMATION_BYTES {
            out.truncate(1);
            break;
        }
    }
    if out.is_empty() {
        return Err("an animation without frames".into());
    }
    if out.len() == 1 {
        out[0].delay = 0.0;
    }
    Ok(out)
}

fn content(img: RgbaImage, density: f32, natural: Option<(f32, f32)>) -> ImageContent {
    let (w, h) = img.dimensions();
    let data = ImageData {
        data: Blob::from(img.into_raw()),
        format: PixelFormat::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width: w,
        height: h,
    };
    ImageContent {
        natural: natural.unwrap_or((w as f32 / density, h as f32 / density)),
        data,
        density,
    }
}

// ---------------------------------------------------------------------------------------------
// SVG

/// Elements an SVG keeps (book:5951).
const SVG_ELEMENTS: [&str; 11] = [
    "svg",
    "g",
    "defs",
    "linearGradient",
    "stop",
    "path",
    "circle",
    "line",
    "rect",
    "mask",
    "text",
];

/// Geometry and presentation attributes an SVG keeps, gradient and text ones included. `style`,
/// event handlers and `href` are not among them.
const SVG_ATTRIBUTES: &[&str] = &[
    // Identity and geometry.
    "id",
    "class",
    "viewBox",
    "preserveAspectRatio",
    "width",
    "height",
    "x",
    "y",
    "x1",
    "y1",
    "x2",
    "y2",
    "cx",
    "cy",
    "r",
    "rx",
    "ry",
    "d",
    "pathLength",
    "transform",
    // Presentation.
    "fill",
    "fill-opacity",
    "fill-rule",
    "stroke",
    "stroke-width",
    "stroke-opacity",
    "stroke-linecap",
    "stroke-linejoin",
    "stroke-miterlimit",
    "stroke-dasharray",
    "stroke-dashoffset",
    "opacity",
    "color",
    "mask",
    "visibility",
    "display",
    "clip-rule",
    "shape-rendering",
    "vector-effect",
    "paint-order",
    "mix-blend-mode",
    "isolation",
    // Gradients and masks.
    "gradientUnits",
    "gradientTransform",
    "spreadMethod",
    "offset",
    "stop-color",
    "stop-opacity",
    "maskUnits",
    "maskContentUnits",
    // Text.
    "font-family",
    "font-size",
    "font-weight",
    "font-style",
    "font-variant",
    "font-stretch",
    "text-anchor",
    "dominant-baseline",
    "alignment-baseline",
    "baseline-shift",
    "letter-spacing",
    "word-spacing",
    "text-decoration",
    "text-rendering",
    "writing-mode",
    "dx",
    "dy",
    "rotate",
    "textLength",
    "lengthAdjust",
];

const SVG_NS: &str = "http://www.w3.org/2000/svg";

/// Rebuilds an SVG from the allowlist (§12.3; book:5951): only [`SVG_ELEMENTS`] (anything else is
/// dropped with its subtree: `script`, `foreignObject`, `image`, `use`, `title`, `style`…), only
/// [`SVG_ATTRIBUTES`] (no `style`, handlers, `href` or namespaced attributes), `url()` only as a
/// local `url(#id)`, and every `id` (and each reference to one) prefixed with `prefix` so copies
/// never clash. The root must be `svg` with a `viewBox`, or a `width` and a `height`.
pub fn sanitize_svg(text: &str, prefix: &str) -> Result<String, String> {
    if text.len() > MAX_SVG {
        return Err(format!("an SVG over {} KiB", MAX_SVG >> 10));
    }
    let opt = roxmltree::ParsingOptions {
        allow_dtd: false,
        ..roxmltree::ParsingOptions::default()
    };
    let doc = roxmltree::Document::parse_with_options(text, opt)
        .map_err(|e| format!("not an SVG: {e}"))?;
    let root = doc.root_element();
    if !svg_element(root) || root.tag_name().name() != "svg" {
        return Err("not an SVG".into());
    }
    let sized = root.attribute("viewBox").is_some()
        || (root.attribute("width").is_some() && root.attribute("height").is_some());
    if !sized {
        return Err("an SVG without a size (a viewBox, or a width and a height)".into());
    }
    let mut out = String::with_capacity(text.len());
    write_svg(root, prefix, true, &mut out);
    Ok(out)
}

fn svg_element(n: roxmltree::Node<'_, '_>) -> bool {
    n.is_element()
        && n.tag_name().namespace().is_none_or(|ns| ns == SVG_NS)
        && SVG_ELEMENTS.contains(&n.tag_name().name())
}

fn write_svg(n: roxmltree::Node<'_, '_>, prefix: &str, root: bool, out: &mut String) {
    let name = n.tag_name().name();
    out.push('<');
    out.push_str(name);
    if root {
        out.push_str(" xmlns=\"");
        out.push_str(SVG_NS);
        out.push('"');
    }
    for a in n.attributes() {
        if a.namespace().is_some() || !SVG_ATTRIBUTES.contains(&a.name()) {
            continue;
        }
        let value: Cow<'_, str> = if a.name() == "id" {
            if !local_id(a.value()) {
                continue;
            }
            Cow::Owned(format!("{prefix}{}", a.value()))
        } else if a.value().contains("url(") {
            match local_url(a.value()) {
                Some(id) => Cow::Owned(format!("url(#{prefix}{id})")),
                None => continue,
            }
        } else {
            Cow::Borrowed(a.value())
        };
        out.push(' ');
        out.push_str(a.name());
        out.push_str("=\"");
        escape(&value, out);
        out.push('"');
    }
    out.push('>');
    for c in n.children() {
        if svg_element(c) {
            write_svg(c, prefix, false, out);
        } else if c.is_text() && name == "text" {
            escape(c.text().unwrap_or(""), out);
        }
    }
    out.push_str("</");
    out.push_str(name);
    out.push('>');
}

fn local_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
}

/// The id of a value that is exactly a local `url(#id)` (optionally quoted), else `None`.
fn local_url(v: &str) -> Option<&str> {
    let inner = v.trim().strip_prefix("url(")?.strip_suffix(')')?.trim();
    let inner = inner
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| inner.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(inner);
    let id = inner.strip_prefix('#')?;
    local_id(id).then_some(id)
}

fn escape(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
}

/// The fonts SVG text draws with: the system's, loaded once.
fn svg_fonts() -> Arc<usvg::fontdb::Database> {
    static FONTS: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    FONTS
        .get_or_init(|| {
            let mut db = usvg::fontdb::Database::new();
            db.load_system_fonts();
            if cfg!(target_os = "macos") {
                db.set_sans_serif_family("Helvetica");
                db.set_monospace_family("Menlo");
            }
            Arc::new(db)
        })
        .clone()
}

fn decode_svg(bytes: &[u8], density: f32, prefix: &str) -> Result<Picture, String> {
    if bytes.len() > MAX_SVG {
        return Err(format!("an SVG over {} KiB", MAX_SVG >> 10));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "an SVG that isn't UTF-8".to_owned())?;
    let clean = sanitize_svg(text, prefix)?;
    let (content, size) = render_svg(&clean, density)?;
    Ok(Picture {
        frames: vec![Frame {
            content,
            delay: 0.0,
        }],
        size,
        loops: Loops::Times(1),
        svg: true,
    })
}

/// Draws an SVG (already sanitized, or one the terminal ships) at `density`, capped at
/// [`MAX_SIDE`] px a side. Returns the content and the SVG's size in CSS px.
fn render_svg(svg: &str, density: f32) -> Result<(ImageContent, (f32, f32)), String> {
    let opt = usvg::Options {
        fontdb: svg_fonts(),
        font_family: "Helvetica".into(),
        ..usvg::Options::default()
    };
    let tree = usvg::Tree::from_str(svg, &opt).map_err(|e| format!("can't draw the SVG: {e}"))?;
    let size = tree.size();
    let (w, h) = (size.width(), size.height());
    let max = MAX_SIDE as f32;
    let scale = density.max(0.25).min(max / w).min(max / h);
    let (pw, ph) = (
        ((w * scale).ceil() as u32).clamp(1, MAX_SIDE),
        ((h * scale).ceil() as u32).clamp(1, MAX_SIDE),
    );
    let mut pixmap =
        resvg::tiny_skia::Pixmap::new(pw, ph).ok_or("an SVG too large to draw".to_owned())?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    let data = ImageData {
        data: Blob::from(pixmap.take()),
        format: PixelFormat::Rgba8,
        alpha_type: ImageAlphaType::AlphaPremultiplied,
        width: pw,
        height: ph,
    };
    Ok((
        ImageContent {
            data,
            natural: (w, h),
            density: scale,
        },
        (w, h),
    ))
}

// ---------------------------------------------------------------------------------------------
// Images the terminal ships

/// The images `builtin` names (book:5961): only `"omp"`, omp's animated gradient mark. Ours is
/// drawn here: a rounded square whose gradient turns, with a ring and a dot.
pub const BUILTINS: [&str; 1] = ["omp"];

const OMP_FRAMES: usize = 48;
const OMP_DELAY: f64 = 60.0;

fn omp_frame(t: f32) -> String {
    let a = t * std::f32::consts::TAU;
    let (c, s) = (a.cos() * 64.0, a.sin() * 64.0);
    format!(
        "<svg xmlns=\"{SVG_NS}\" width=\"128\" height=\"128\" viewBox=\"0 0 128 128\">\
         <defs><linearGradient id=\"g\" gradientUnits=\"userSpaceOnUse\" \
         x1=\"{x1}\" y1=\"{y1}\" x2=\"{x2}\" y2=\"{y2}\">\
         <stop offset=\"0\" stop-color=\"#7c5cff\"/>\
         <stop offset=\"0.5\" stop-color=\"#3d8bff\"/>\
         <stop offset=\"1\" stop-color=\"#22d3c5\"/></linearGradient></defs>\
         <rect x=\"6\" y=\"6\" width=\"116\" height=\"116\" rx=\"30\" fill=\"url(#g)\"/>\
         <circle cx=\"58\" cy=\"62\" r=\"26\" fill=\"none\" stroke=\"#ffffff\" stroke-width=\"11\" \
         stroke-opacity=\"0.92\"/>\
         <circle cx=\"92\" cy=\"94\" r=\"9\" fill=\"#ffffff\" fill-opacity=\"0.92\"/></svg>",
        x1 = 64.0 - c,
        y1 = 64.0 - s,
        x2 = 64.0 + c,
        y2 = 64.0 + s,
    )
}

fn draw_builtin(name: &str, density: f32) -> Result<Picture, String> {
    if name != "omp" {
        return Err(format!("no built-in image `{name}`"));
    }
    let mut frames = Vec::with_capacity(OMP_FRAMES);
    let mut size = (0.0, 0.0);
    for i in 0..OMP_FRAMES {
        let (content, s) = render_svg(&omp_frame(i as f32 / OMP_FRAMES as f32), density)?;
        size = s;
        frames.push(Frame {
            content,
            delay: OMP_DELAY,
        });
    }
    Ok(Picture {
        frames,
        size,
        loops: Loops::Forever,
        svg: true,
    })
}

// ---------------------------------------------------------------------------------------------
// The shared cache

type Loaded = Result<Arc<Picture>, String>;

#[derive(Default)]
struct Cache {
    clock: u64,
    bytes: usize,
    entries: HashMap<String, (u64, Loaded)>,
}

fn cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(Mutex::default)
}

/// A picture from the cache, or decoded (outside the lock) and cached.
fn cached(key: String, decode: impl FnOnce() -> Result<Picture, String>) -> Loaded {
    {
        let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
        c.clock += 1;
        let now = c.clock;
        if let Some((used, loaded)) = c.entries.get_mut(&key) {
            *used = now;
            return loaded.clone();
        }
    }
    let loaded = decode().map(Arc::new);
    let size = loaded.as_ref().map_or(0, |p| p.bytes());
    let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
    c.clock += 1;
    let now = c.clock;
    if c.entries
        .insert(key.clone(), (now, loaded.clone()))
        .is_none()
    {
        c.bytes += size;
    }
    while c.bytes > CACHE_BYTES {
        let victim = c
            .entries
            .iter()
            .filter(|(k, _)| **k != key)
            .min_by_key(|(_, (used, _))| *used)
            .map(|(k, _)| k.clone());
        let Some(victim) = victim else { break };
        if let Some((_, Ok(p))) = c.entries.remove(&victim) {
            c.bytes -= p.bytes();
        }
    }
    loaded
}

/// Density buckets of a quarter, so small changes reuse a vector picture.
fn bucket(density: f32) -> f32 {
    ((density.clamp(0.25, 8.0)) * 4.0).round() / 4.0
}

/// A blob's picture. Rasters are cached by id; vector pictures by id and density.
pub fn load_blob(blob: &auk_tsp::Blob, density: f32) -> Loaded {
    let prefix = format!("i{}-", &blob.id[..blob.id.len().min(8)]);
    match sniff(&blob.bytes, blob.mime.as_deref()) {
        Some(Kind::Svg) => {
            let d = bucket(density);
            cached(format!("{}@{d}", blob.id), || {
                decode_svg(&blob.bytes, d, &prefix)
            })
        }
        Some(Kind::Raster(f)) => cached(blob.id.clone(), || decode_raster(&blob.bytes, f)),
        None => Err("not an image format Auk can show".into()),
    }
}

/// The bytes and media type of a `data:` URL (`data:[<type>][;base64],<data>`, RFC 2397): base64
/// or percent-encoded.
pub fn data_url(url: &str) -> Option<(Vec<u8>, Option<String>)> {
    use base64::Engine as _;
    let rest = url.get(..5).filter(|s| s.eq_ignore_ascii_case("data:"))?;
    let rest = &url[rest.len()..];
    let (meta, data) = rest.split_once(',')?;
    let mut params = meta.split(';');
    let mime = params
        .next()
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(str::to_ascii_lowercase);
    let base64 = params.any(|p| p.trim().eq_ignore_ascii_case("base64"));
    let bytes = if base64 {
        use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
        let engine = GeneralPurpose::new(
            &base64::alphabet::STANDARD,
            GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
        );
        let clean: String = data.chars().filter(|c| !c.is_ascii_whitespace()).collect();
        engine.decode(clean).ok()?
    } else {
        let mut out = Vec::with_capacity(data.len());
        let b = data.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'%'
                && let Some(v) = data
                    .get(i + 1..i + 3)
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
            {
                out.push(v);
                i += 3;
            } else {
                out.push(b[i]);
                i += 1;
            }
        }
        out
    };
    Some((bytes, mime))
}

/// The picture of a `data:` URL, for a plugin sheet's `background-image` (§9.8 CSS): PNG, JPEG,
/// GIF, WebP (their first frame) or sanitized SVG drawn at `density`, at most 4096 px a side.
pub fn load_data_url(url: &str, density: f32) -> Option<ImageContent> {
    let (bytes, mime) = data_url(url)?;
    if bytes.len() > auk_tsp::blob::MAX_BLOB {
        return None;
    }
    let id = auk_tsp::blob::blob_id(&bytes);
    let blob = auk_tsp::Blob { id, mime, bytes };
    let picture = load_blob(&blob, density).ok()?;
    picture.frames.first().map(|f| f.content.clone())
}

/// A built-in picture at `density`.
pub fn load_builtin(name: &str, density: f32) -> Loaded {
    let d = bucket(density);
    cached(format!("builtin:{name}@{d}"), || draw_builtin(name, d))
}
