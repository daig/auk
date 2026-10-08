//! Fonts and glyph rasterization for the cell grid (spec §18.3).
//!
//! [`GridFonts`] resolves a monospace family and its fallbacks through fontique (CoreText's font
//! list on macOS), derives the [`CellMetrics`] of the grid from the primary face, and rasterizes
//! one grid cluster at a time (a cell's text: a base character plus combining marks, or an emoji
//! sequence) into a [`GlyphImage`] positioned relative to the cell's top-left corner.
//!
//! Rasterization goes through CoreText in the app, for the native look, and through swash in the
//! browser client ([`Backend`]). Both backends share font selection and cell metrics, so a grid
//! has the same geometry whichever backend draws it. Text metrics are view-only (§18.3): replicas
//! never depend on them, and grid character widths come from the terminal core.

mod canvas;
#[cfg(target_os = "macos")]
mod coretext;
mod resolve;
mod sprite;
mod swash_backend;

use std::sync::Arc;

use canvas::Canvas;
pub use canvas::{GlyphFormat, GlyphImage};
use resolve::{FaceId, Resolver};

/// Which rasterizer draws glyphs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Backend {
    /// CoreText: the app's native look. macOS only.
    #[cfg(target_os = "macos")]
    #[default]
    CoreText,
    /// swash: pure Rust. The browser client's rasterizer; also available natively.
    #[cfg_attr(not(target_os = "macos"), default)]
    Swash,
}

/// What font the grid uses.
#[derive(Clone, Debug, PartialEq)]
pub struct FontConfig {
    /// Family names to try in order. Empty means the default monospace families
    /// ([`DEFAULT_FAMILIES`]).
    pub families: Vec<String>,
    /// Font size in points.
    pub size_pt: f32,
    /// Display scale: physical pixels per point (2.0 on Retina).
    pub scale: f32,
    /// Load the system's fonts (macOS, Linux). The browser client has none and registers
    /// `extra_fonts` instead.
    pub system_fonts: bool,
    /// Font files (TTF, OTF, TTC) to register before resolving families.
    pub extra_fonts: Vec<Arc<Vec<u8>>>,
    /// Rasterizer.
    pub backend: Backend,
}

/// The bundled Nerd Fonts symbols face (Nerd Fonts v3.4.0, SIL OFL 1.1; see `assets/README.md`).
/// It draws Nerd Font icons, which live in the Private Use Area: surfaces' icon spans (§7.12)
/// and the grid's fallback for PUA codepoints the grid's family lacks (§18.3).
pub static NERD_SYMBOLS: &[u8] = include_bytes!("../assets/SymbolsNerdFontMono-Regular.ttf");

/// The family name of [`NERD_SYMBOLS`].
pub const NERD_SYMBOLS_FAMILY: &str = "Symbols Nerd Font Mono";

/// Whether `c` is a Private Use Area codepoint (where Nerd Font icons live): U+E000–U+F8FF and
/// planes 15–16.
pub fn is_private_use(c: char) -> bool {
    matches!(c as u32, 0xE000..=0xF8FF | 0xF0000..=0x10FFFF)
}

/// The monospace families tried when [`FontConfig::families`] is empty.
pub const DEFAULT_FAMILIES: &[&str] = &["SF Mono", "Menlo", "Monaco", "Courier New", "Courier"];

impl Default for FontConfig {
    fn default() -> Self {
        FontConfig {
            families: Vec::new(),
            size_pt: 13.0,
            scale: 1.0,
            system_fonts: true,
            extra_fonts: Vec::new(),
            backend: Backend::default(),
        }
    }
}

/// Bold and italic, the two SGR attributes that pick a face.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct GlyphStyle {
    pub bold: bool,
    pub italic: bool,
}

impl GlyphStyle {
    pub const REGULAR: GlyphStyle = GlyphStyle {
        bold: false,
        italic: false,
    };

    fn index(self) -> usize {
        usize::from(self.bold) | (usize::from(self.italic) << 1)
    }
}

/// The grid's geometry in physical pixels, derived from the primary face.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellMetrics {
    pub cell_width: u32,
    pub cell_height: u32,
    /// Baseline, from the top of the cell.
    pub baseline: u32,
    /// Top of the underline, from the top of the cell.
    pub underline_position: u32,
    pub underline_thickness: u32,
    /// Top of the strikethrough line, from the top of the cell.
    pub strikeout_position: u32,
    pub strikeout_thickness: u32,
}

#[derive(Debug)]
pub enum FontError {
    /// None of the requested families (or the defaults) exist.
    NoFamily(Vec<String>),
    /// The primary face's tables couldn't be read.
    BadFont(String),
}

impl std::fmt::Display for FontError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FontError::NoFamily(names) => write!(f, "no font family found among {names:?}"),
            FontError::BadFont(name) => write!(f, "font {name} could not be read"),
        }
    }
}

impl std::error::Error for FontError {}

/// The grid's fonts: selection, fallback, metrics and rasterization.
pub struct GridFonts {
    resolver: Resolver,
    metrics: CellMetrics,
    size_px: f32,
    rasterizer: Box<dyn Rasterizer>,
    backend: Backend,
}

impl std::fmt::Debug for GridFonts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GridFonts")
            .field("family", &self.resolver.primary_family_name())
            .field("size_px", &self.size_px)
            .field("metrics", &self.metrics)
            .field("backend", &self.backend)
            .finish()
    }
}

/// A rasterizer draws a cluster of text in one face into a canvas. Font choice and metrics are
/// shared; only drawing differs between backends.
pub(crate) trait Rasterizer {
    /// The advance width of `text` set in `face` at `size_px`.
    fn advance(&mut self, face: &resolve::Face, size_px: f32, text: &str) -> f32;

    /// Draws `text` with its pen at `pen_x` and its baseline at `baseline_y` (canvas pixels,
    /// y down).
    fn draw(
        &mut self,
        face: &resolve::Face,
        size_px: f32,
        text: &str,
        pen_x: f32,
        baseline_y: f32,
        canvas: &mut Canvas,
    );

    /// Drops cached per-size state (after a size or scale change).
    fn clear(&mut self);
}

impl GridFonts {
    pub fn new(config: &FontConfig) -> Result<Self, FontError> {
        let mut resolver = Resolver::new(config)?;
        let size_px = config.size_pt * config.scale;
        let metrics = resolver.cell_metrics(size_px)?;
        let rasterizer: Box<dyn Rasterizer> = match config.backend {
            #[cfg(target_os = "macos")]
            Backend::CoreText => Box::new(coretext::CoreTextRasterizer::default()),
            Backend::Swash => Box::new(swash_backend::SwashRasterizer::default()),
        };
        Ok(GridFonts {
            resolver,
            metrics,
            size_px,
            rasterizer,
            backend: config.backend,
        })
    }

    pub fn metrics(&self) -> CellMetrics {
        self.metrics
    }

    pub fn backend(&self) -> Backend {
        self.backend
    }

    /// The font size in physical pixels.
    pub fn size_px(&self) -> f32 {
        self.size_px
    }

    /// The family the grid is set in.
    pub fn family_name(&self) -> &str {
        self.resolver.primary_family_name()
    }

    /// Changes the size or display scale. Returns the new metrics; glyphs rasterized before
    /// are stale.
    pub fn set_size(&mut self, size_pt: f32, scale: f32) -> Result<CellMetrics, FontError> {
        let size_px = size_pt * scale;
        if size_px != self.size_px {
            self.metrics = self.resolver.cell_metrics(size_px)?;
            self.size_px = size_px;
            self.rasterizer.clear();
        }
        Ok(self.metrics)
    }

    /// The family name of the face `text` resolves to in `style` (after fallback).
    pub fn face_family(&mut self, text: &str, style: GlyphStyle) -> String {
        let face = self.resolver.resolve(text, style);
        self.resolver.face(face).family.clone()
    }

    /// Rasterizes one grid cluster spanning `span` cells (2 for wide characters). Returns `None`
    /// when nothing would be drawn (blank text, whitespace, or a glyph with no ink).
    ///
    /// Box drawing and block elements are drawn from the cell geometry, not a font, so they
    /// join across cells.
    ///
    /// The image is positioned relative to the top-left corner of the cluster's first cell and
    /// may extend past the cells (italic overhang, tall marks).
    pub fn rasterize(&mut self, text: &str, style: GlyphStyle, span: u8) -> Option<GlyphImage> {
        if text.chars().all(|c| c.is_whitespace() || c.is_control()) {
            return None;
        }
        let mut chars = text.chars();
        if let (Some(c), None) = (chars.next(), chars.next())
            && let Some(image) = sprite::draw(c, &self.metrics, span.max(1))
        {
            return Some(image);
        }
        let face_id = self.resolver.resolve(text, style);
        self.rasterize_in(face_id, text, style, span.max(1))
    }

    fn rasterize_in(
        &mut self,
        face_id: FaceId,
        text: &str,
        style: GlyphStyle,
        span: u8,
    ) -> Option<GlyphImage> {
        let m = self.metrics;
        let face = self.resolver.face(face_id);
        let target_w = f32::from(span) * m.cell_width as f32;

        // Faces other than the grid's own may be wider than the cells they occupy (emoji, CJK
        // from a proportional face, symbols). Shrink those to fit; the grid's faces are drawn
        // as they are, so italic and bold overhang stay natural.
        let mut size = self.size_px;
        let mut pen_x = 0.0;
        if !self.resolver.is_primary(face_id, style) {
            let advance = self.rasterizer.advance(face, size, text);
            if advance > target_w && advance > 0.0 {
                size *= target_w / advance;
            }
            if face.color {
                let advance = self.rasterizer.advance(face, size, text);
                pen_x = ((target_w - advance) / 2.0).max(0.0);
            }
        }

        // Pad generously so overhanging ink isn't clipped, then trim to the ink.
        let pad = m.cell_height as usize;
        let width = target_w as usize + 2 * pad;
        let height = m.cell_height as usize + 2 * pad;
        let format = if face.color {
            GlyphFormat::Color
        } else {
            GlyphFormat::Mask
        };
        let mut canvas = Canvas::new(width, height, format);
        let baseline = (pad as u32 + m.baseline) as f32;
        self.rasterizer
            .draw(face, size, text, pad as f32 + pen_x, baseline, &mut canvas);
        canvas.into_glyph(-(pad as i32), -(pad as i32))
    }
}

/// Characters that never decide a cluster's font: joiners, variation selectors, and the like.
pub(crate) fn is_ignorable(c: char) -> bool {
    matches!(c, '\u{200C}' | '\u{200D}' | '\u{FE00}'..='\u{FE0F}' | '\u{E0020}'..='\u{E007F}')
}

#[cfg(test)]
mod tests;
