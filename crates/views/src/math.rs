//! TeX math (§8.5, §18.4; research/elements.md §7.1, §7.3; amendment A23): the `math` kind and
//! md math share this engine.
//!
//! - **Engine:** RaTeX, a pure-Rust port of KaTeX's parser and layout (`ratex-parser`,
//!   `ratex-layout`), with the KaTeX fonts compiled in (`ratex-katex-fonts`). It runs the same in
//!   the app and in the browser (wasm32), reads no files, fonts or environment, and starts no
//!   JS engine: input is a string, output a display list of glyphs, rules and paths.
//! - **Drawing:** the display list is filled into a coverage mask (glyph outlines through
//!   skrifa, rules and paths through tiny-skia) at the display's density, and the mask paints in
//!   the element's `color` ([`auk_ui::Document::set_image_tint`]), so a formula is drawn once and
//!   follows the theme and sheets like `currentColor` SVG. `\color` and `\colorbox` paint in
//!   `currentColor` too (a mask has one color).
//! - **Placement:** the formula's box is `width × (height + depth)` em plus [`PAD`] a side; the
//!   element sets that size in `em` and `vertical-align: -(depth + PAD)em`, so the baseline sits
//!   on the text's.
//! - **Cost:** formulas are cached process-wide by TeX, style and density (LRU past
//!   [`CACHE_BYTES`]). Short TeX ([`INLINE_TEX`] bytes or less, typically well under a
//!   millisecond) typesets on the caller's thread; longer TeX typesets in the background, as big
//!   images decode (A9): natively on a worker thread, in the browser one formula per clock tick
//!   ([`pump`]). Until then the element shows the TeX as text (`.math.pending`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use auk_ui::{ImageContent, NodeId};
use ratex_layout::LayoutOptions;
use ratex_types::display_item::{DisplayItem, DisplayList};
use ratex_types::math_style::MathStyle;
use ratex_types::path_command::PathCommand;
use resvg::tiny_skia::{self, FillRule, Paint, PathBuilder, Pixmap, Stroke, Transform};
use skrifa::MetadataProvider;
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use vello::peniko::{Blob, ImageAlphaType, ImageData, ImageFormat as PixelFormat};

use crate::dom::Dom;

/// The longest TeX typeset; longer shows as an error.
pub const MAX_TEX: usize = 16 << 10;
/// TeX at most this long typesets on the caller's thread ([`request`]).
pub const INLINE_TEX: usize = 512;
/// The largest formula picture, per side, in device pixels.
pub const MAX_SIDE: u32 = 4096;
/// Space around the formula's box, in em, so ink past it (italic overhang, accents) shows.
pub const PAD: f64 = 0.08;
/// Math's size relative to the surrounding text (KaTeX draws at 1.21em; terminal text is set
/// tighter, so a little less).
pub const SCALE: f32 = 1.1;
/// Formula pictures kept for reuse.
const CACHE_BYTES: usize = 32 << 20;
/// Unfilled paths' stroke width, in em (KaTeX's SVG export: 1.5 at 40px).
const STROKE_EM: f64 = 0.0375;

/// A typeset formula: a coverage mask and its box.
#[derive(Debug)]
pub struct Formula {
    /// White coverage (premultiplied), painted tinted.
    pub content: ImageContent,
    /// The box in em, [`PAD`] excluded: width, height above the baseline, depth below it.
    pub width: f64,
    pub height: f64,
    pub depth: f64,
}

impl Formula {
    /// The element's CSS: its size in em and the baseline on the text's.
    pub fn css(&self) -> String {
        let f = |v: f64| {
            format!("{:.4}", v)
                .trim_end_matches('0')
                .trim_end_matches('.')
                .to_owned()
        };
        format!(
            "width: {}em; height: {}em; vertical-align: {}em",
            f(self.width + 2.0 * PAD),
            f(self.height + self.depth + 2.0 * PAD),
            f(-(self.depth + PAD)),
        )
    }

    fn bytes(&self) -> usize {
        self.content.data.data.data().len()
    }
}

pub type Typeset = Result<Arc<Formula>, String>;

/// The TeX a `math` node or md delimiter typesets: display math drops blank lines (they would end
/// the block), inline math is one line (newlines read as spaces) (book: `math`).
pub fn normalize(tex: &str, display: bool) -> String {
    if display {
        tex.lines()
            .filter(|l| !l.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        tex.replace("\r\n", " ").replace(['\n', '\r'], " ")
    }
}

/// Device pixels per em for math in text of `font_px` CSS px at region zoom `zoom` on a display
/// of `scale`, in whole pixels (so near densities share a picture).
pub fn em_px(font_px: f32, zoom: f32, scale: f32) -> u32 {
    ((font_px * SCALE * zoom * scale).round() as u32).clamp(4, 512)
}

/// Typesets `tex` (no delimiters) now, uncached: display or text style, `em` device pixels per em.
pub fn typeset_uncached(tex: &str, display: bool, em: u32) -> Result<Formula, String> {
    if tex.len() > MAX_TEX {
        return Err(format!(
            "formula too long ({} bytes, at most {MAX_TEX})",
            tex.len()
        ));
    }
    let list = layout(tex, display)?;
    draw(&list, f64::from(em))
}

/// TeX → display list, guarding against the engine panicking where unwinding works.
fn layout(tex: &str, display: bool) -> Result<DisplayList, String> {
    let run = || {
        let nodes = ratex_parser::parse(tex).map_err(|e| e.message.clone())?;
        let opts = LayoutOptions {
            style: if display {
                MathStyle::Display
            } else {
                MathStyle::Text
            },
            ..LayoutOptions::default()
        };
        let lbox = ratex_layout::layout(&nodes, &opts);
        Ok(ratex_layout::to_display_list(&lbox))
    };
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::panic::catch_unwind(run).unwrap_or_else(|_| Err("the typesetter failed".into()))
    }
    #[cfg(target_arch = "wasm32")]
    {
        run()
    }
}

/// The KaTeX font for a display list's font name, parsed once.
fn font(name: &str) -> Option<skrifa::FontRef<'static>> {
    type Fonts = Mutex<HashMap<String, Option<&'static [u8]>>>;
    static FONTS: OnceLock<Fonts> = OnceLock::new();
    let bytes = {
        let mut fonts = FONTS
            .get_or_init(Mutex::default)
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *fonts.entry(name.to_owned()).or_insert_with(|| {
            let file = format!("KaTeX_{name}.ttf");
            ratex_katex_fonts::ttf_bytes(&file).map(|b| -> &'static [u8] {
                match b {
                    std::borrow::Cow::Borrowed(b) => b,
                    std::borrow::Cow::Owned(v) => Box::leak(v.into_boxed_slice()),
                }
            })
        })
    }?;
    skrifa::FontRef::new(bytes).ok()
}

/// Glyph outlines into a tiny-skia path, at an origin in device px, y down.
struct Pen<'a> {
    b: &'a mut PathBuilder,
    x: f32,
    y: f32,
}

impl OutlinePen for Pen<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.b.move_to(self.x + x, self.y - y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.b.line_to(self.x + x, self.y - y);
    }
    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.b
            .quad_to(self.x + cx0, self.y - cy0, self.x + x, self.y - y);
    }
    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.b.cubic_to(
            self.x + cx0,
            self.y - cy0,
            self.x + cx1,
            self.y - cy1,
            self.x + x,
            self.y - y,
        );
    }
    fn close(&mut self) {
        self.b.close();
    }
}

/// Draws a glyph; false if no KaTeX font has it.
fn glyph(b: &mut PathBuilder, x: f32, y: f32, px: f32, font_name: &str, code: u32) -> bool {
    let id = ratex_font::FontId::parse(font_name).unwrap_or(ratex_font::FontId::MainRegular);
    let ch = ratex_font::katex_ttf_glyph_char(id, code);
    // CJK and emoji have no KaTeX font (and Auk reads no system fonts here): Main, else nothing.
    for name in [id.as_str(), "Main-Regular"] {
        let Some(f) = font(name) else { continue };
        let Some(gid) = f.charmap().map(ch) else {
            continue;
        };
        let Some(g) = f.outline_glyphs().get(gid) else {
            continue;
        };
        let settings = DrawSettings::unhinted(Size::new(px), LocationRef::default());
        return g.draw(settings, &mut Pen { b, x, y }).is_ok();
    }
    false
}

/// Fills a display list's coverage into a picture at `em` device px per em.
fn draw(list: &DisplayList, em: f64) -> Result<Formula, String> {
    let total = list.height + list.depth;
    let (w, h) = (
        ((list.width + 2.0 * PAD) * em).ceil().max(1.0),
        ((total + 2.0 * PAD) * em).ceil().max(1.0),
    );
    if w > f64::from(MAX_SIDE) || h > f64::from(MAX_SIDE) {
        return Err("formula too large to draw".into());
    }
    let mut pixmap = Pixmap::new(w as u32, h as u32).ok_or("formula too large to draw")?;
    let mut paint = Paint::default();
    paint.set_color_rgba8(255, 255, 255, 255);
    paint.anti_alias = true;
    let tx = |x: f64| ((x + PAD) * em) as f32;
    let fill_rect = |pixmap: &mut Pixmap, x: f32, y: f32, w: f32, h: f32| {
        if let Some(r) = tiny_skia::Rect::from_xywh(x, y, w.max(0.0), h.max(0.0)) {
            pixmap.fill_rect(r, &paint, Transform::identity(), None);
        }
    };
    let mut glyphs = PathBuilder::new();
    for item in &list.items {
        match item {
            DisplayItem::GlyphPath {
                x,
                y,
                scale,
                font,
                char_code,
                ..
            } => {
                glyph(
                    &mut glyphs,
                    tx(*x),
                    tx(*y),
                    (scale * em) as f32,
                    font,
                    *char_code,
                );
            }
            DisplayItem::Line {
                x,
                y,
                width,
                thickness,
                dashed,
                ..
            } => {
                let t = (thickness * em).max(1.0) as f32;
                let (x0, yc, len) = (tx(*x), tx(*y), (width * em) as f32);
                if *dashed {
                    let dash = 3.0 * t;
                    let mut at = 0.0;
                    while at < len {
                        fill_rect(&mut pixmap, x0 + at, yc - t / 2.0, dash.min(len - at), t);
                        at += 2.0 * dash;
                    }
                } else {
                    fill_rect(&mut pixmap, x0, yc - t / 2.0, len, t);
                }
            }
            DisplayItem::Rect {
                x,
                y,
                width,
                height,
                ..
            } => fill_rect(
                &mut pixmap,
                tx(*x),
                tx(*y),
                (width * em) as f32,
                (height * em) as f32,
            ),
            DisplayItem::Path {
                x,
                y,
                commands,
                fill,
                ..
            } => {
                let (ox, oy) = (tx(*x), tx(*y));
                let p = |v: f64| (v * em) as f32;
                // Filled paths fill each subpath on its own (as KaTeX's SVG does).
                let mut b = PathBuilder::new();
                let flush = |b: &mut PathBuilder, pixmap: &mut Pixmap| {
                    let done = std::mem::take(b);
                    if let Some(path) = done.finish() {
                        if *fill {
                            pixmap.fill_path(
                                &path,
                                &paint,
                                FillRule::Winding,
                                Transform::identity(),
                                None,
                            );
                        } else {
                            let stroke = Stroke {
                                width: ((STROKE_EM * em) as f32).max(1.0),
                                ..Stroke::default()
                            };
                            pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
                        }
                    }
                };
                for c in commands {
                    match *c {
                        PathCommand::MoveTo { x, y } => {
                            if *fill {
                                flush(&mut b, &mut pixmap);
                            }
                            b.move_to(ox + p(x), oy + p(y));
                        }
                        PathCommand::LineTo { x, y } => b.line_to(ox + p(x), oy + p(y)),
                        PathCommand::QuadTo { x1, y1, x, y } => {
                            b.quad_to(ox + p(x1), oy + p(y1), ox + p(x), oy + p(y));
                        }
                        PathCommand::CubicTo {
                            x1,
                            y1,
                            x2,
                            y2,
                            x,
                            y,
                        } => b.cubic_to(
                            ox + p(x1),
                            oy + p(y1),
                            ox + p(x2),
                            oy + p(y2),
                            ox + p(x),
                            oy + p(y),
                        ),
                        PathCommand::Close => b.close(),
                    }
                }
                flush(&mut b, &mut pixmap);
            }
        }
    }
    if let Some(path) = glyphs.finish() {
        pixmap.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
    let (pw, ph) = (pixmap.width(), pixmap.height());
    let density = em as f32;
    let data = ImageData {
        data: Blob::from(pixmap.take()),
        format: PixelFormat::Rgba8,
        alpha_type: ImageAlphaType::AlphaPremultiplied,
        width: pw,
        height: ph,
    };
    Ok(Formula {
        content: ImageContent {
            // Natural size in em-sized CSS px; the element's em size overrides it.
            natural: (pw as f32 / density, ph as f32 / density),
            data,
            density,
        },
        width: list.width,
        height: list.height,
        depth: list.depth,
    })
}

// ---------------------------------------------------------------------------------------------
// The shared cache

enum Slot {
    Done(Typeset),
    Pending,
}

#[derive(Default)]
struct Cache {
    clock: u64,
    bytes: usize,
    entries: HashMap<String, (u64, Slot)>,
}

fn lock() -> std::sync::MutexGuard<'static, Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

impl Cache {
    fn hit(&mut self, key: &str) -> Option<&Slot> {
        self.clock += 1;
        let now = self.clock;
        let (used, slot) = self.entries.get_mut(key)?;
        *used = now;
        Some(slot)
    }

    fn store(&mut self, key: &str, t: &Typeset) {
        self.clock += 1;
        let now = self.clock;
        if matches!(self.entries.get(key), Some((_, Slot::Done(_)))) {
            return;
        }
        self.bytes += t.as_ref().map_or(0, |f| f.bytes());
        self.entries
            .insert(key.to_owned(), (now, Slot::Done(t.clone())));
        while self.bytes > CACHE_BYTES {
            let victim = self
                .entries
                .iter()
                .filter(|(k, (_, s))| k.as_str() != key && matches!(s, Slot::Done(_)))
                .min_by_key(|(_, (used, _))| *used)
                .map(|(k, _)| k.clone());
            let Some(victim) = victim else { break };
            if let Some((_, Slot::Done(Ok(f)))) = self.entries.remove(&victim) {
                self.bytes -= f.bytes();
            }
        }
    }
}

fn key(tex: &str, display: bool, em: u32) -> String {
    format!("{}{em}:{tex}", if display { 'D' } else { 'T' })
}

/// A formula, typeset now (or from the cache) whatever its length.
pub fn typeset(tex: &str, display: bool, em: u32) -> Typeset {
    let k = key(tex, display, em);
    if let Some(Slot::Done(t)) = lock().hit(&k) {
        return t.clone();
    }
    let t = typeset_uncached(tex, display, em).map(Arc::new);
    lock().store(&k, &t);
    t
}

/// A formula asked for without waiting: typeset, or typesetting in the background.
#[derive(Debug)]
pub enum Poll {
    Ready(Typeset),
    Pending,
}

/// A formula without making the caller wait for a long one: TeX of at most [`INLINE_TEX`] bytes
/// (and TeX past [`MAX_TEX`], which fails at once) typesets now; longer TeX typesets in the
/// background and is [`Poll::Pending`] until cached. Ask again (the view's clock does).
pub fn request(tex: &str, display: bool, em: u32) -> Poll {
    if tex.len() <= INLINE_TEX || tex.len() > MAX_TEX {
        return Poll::Ready(typeset(tex, display, em));
    }
    let k = key(tex, display, em);
    {
        let mut c = lock();
        match c.hit(&k) {
            Some(Slot::Done(t)) => return Poll::Ready(t.clone()),
            Some(Slot::Pending) => return Poll::Pending,
            None => {}
        }
        c.clock += 1;
        let now = c.clock;
        c.entries.insert(k.clone(), (now, Slot::Pending));
    }
    background::submit(Job {
        key: k,
        tex: tex.to_owned(),
        display,
        em,
    });
    Poll::Pending
}

/// Whether formulas are typesetting in the background.
pub fn busy() -> bool {
    lock()
        .entries
        .values()
        .any(|(_, s)| matches!(s, Slot::Pending))
}

/// Waits until nothing typesets in the background (doing the browser's queue on this thread).
pub fn settle() {
    loop {
        pump();
        if !busy() {
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

/// Typesets what the browser queued, one formula per call; natively a worker does it.
pub fn pump() {
    background::pump();
}

struct Job {
    key: String,
    tex: String,
    display: bool,
    em: u32,
}

impl Job {
    fn run(self) {
        let t = typeset_uncached(&self.tex, self.display, self.em).map(Arc::new);
        lock().store(&self.key, &t);
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod background {
    use super::Job;
    use std::sync::OnceLock;
    use std::sync::mpsc::Sender;

    pub(super) fn submit(job: Job) {
        static WORKER: OnceLock<std::sync::Mutex<Sender<Job>>> = OnceLock::new();
        let tx = WORKER.get_or_init(|| {
            let (tx, rx) = std::sync::mpsc::channel::<Job>();
            std::thread::Builder::new()
                .name("auk-math".into())
                .spawn(move || {
                    for job in rx {
                        job.run();
                    }
                })
                .expect("the math thread");
            std::sync::Mutex::new(tx)
        });
        let _ = tx.lock().unwrap_or_else(|e| e.into_inner()).send(job);
    }

    pub(super) fn pump() {}
}

#[cfg(target_arch = "wasm32")]
mod background {
    use super::Job;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    thread_local! {
        static QUEUE: RefCell<VecDeque<Job>> = RefCell::new(VecDeque::new());
    }

    pub(super) fn submit(job: Job) {
        QUEUE.with(|q| q.borrow_mut().push_back(job));
    }

    pub(super) fn pump() {
        if let Some(job) = QUEUE.with(|q| q.borrow_mut().pop_front()) {
            job.run();
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Elements

/// A formula element waiting for the background: its wrapper and what it asked for.
#[derive(Debug, Clone)]
pub(crate) struct Pending {
    pub el: NodeId,
    /// The TeX as written (normalized), and as typeset (trimmed).
    pub source: String,
    pub tex: String,
    pub display: bool,
    pub em: u32,
}

/// Appends a formula to `parent` (book: md Math, `math`): `span.math` (inline) or
/// `div.math-display` (display) holding the TeX in `data-tex` around an `svg` that paints the
/// formula in `currentColor`; a formula that fails is `code.math-err` titled with the error
/// (in a `div.math-display` for display math). Returns the wrapper and, if the formula is
/// typesetting in the background, what to fill in later ([`fill`]).
pub(crate) fn mount(
    dom: &mut Dom,
    parent: NodeId,
    tex: &str,
    display: bool,
    em: u32,
) -> (NodeId, Option<Pending>) {
    let tex = normalize(tex, display);
    let (tag, class) = if display {
        ("div", "math-display")
    } else {
        ("span", "math")
    };
    match request(tex.trim(), display, em) {
        Poll::Ready(Err(e)) if !display => {
            let c = dom.child(parent, "code", "math-err");
            dom.attr(c, "title", &e);
            dom.text(c, &tex);
            (c, None)
        }
        Poll::Ready(t) => {
            let el = dom.child(parent, tag, class);
            dom.attr(el, "data-tex", &tex);
            fill_with(dom, el, &tex, t);
            (el, None)
        }
        Poll::Pending => {
            let el = dom.child(parent, tag, &format!("{class} pending"));
            dom.attr(el, "data-tex", &tex);
            dom.text(el, &tex);
            let p = Pending {
                el,
                source: tex.clone(),
                tex: tex.trim().to_owned(),
                display,
                em,
            };
            (el, Some(p))
        }
    }
}

/// Fills a wrapper's formula (or its error).
fn fill_with(dom: &mut Dom, el: NodeId, tex: &str, t: Typeset) {
    dom.clear(el);
    match t {
        Ok(f) => {
            let svg = dom.child(el, "svg", "");
            dom.attr(svg, "aria-hidden", "true");
            dom.set_style(svg, &f.css());
            dom.doc.set_image(svg, Some(f.content.clone()));
            dom.doc.set_image_tint(svg, true);
            dom.attr(el, "role", "math");
            dom.attr(el, "aria-label", tex);
        }
        Err(e) => {
            // Display math's error sits in its block (inline math's replaces the wrapper, unless
            // it was typeset in the background).
            let c = dom.child(el, "code", "math-err");
            dom.attr(c, "title", &e);
            dom.text(c, tex);
        }
    }
}

/// Fills pending formulas that are ready; returns those still waiting.
pub(crate) fn fill(dom: &mut Dom, pending: Vec<Pending>) -> Vec<Pending> {
    pump();
    let mut left = Vec::new();
    for p in pending {
        if dom.doc.try_node(p.el).is_none() {
            continue;
        }
        match request(&p.tex, p.display, p.em) {
            Poll::Ready(t) => {
                let class = if p.display { "math-display" } else { "math" };
                dom.set_class(p.el, class);
                fill_with(dom, p.el, &p.source, t);
            }
            Poll::Pending => left.push(p),
        }
    }
    left
}
