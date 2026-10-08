//! Fonts and text metrics: Parley's font and layout contexts, font registration, and primary-font
//! metrics rounded the way WebKit rounds them.

use crate::style::{ComputedStyle, FontStyle, OverflowWrap, WordBreak};
use parley::{FontContext, LayoutContext};
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

/// The brush Parley carries for each run: the index of the inline box that owns the text.
#[derive(Clone, Copy, PartialEq, Default, Debug)]
pub struct TextBrush {
    pub owner: u32,
    /// Nonzero for the no-break spaces that stand for inline box edges (1 + edge index).
    pub edge: u32,
}

/// Primary-font metrics in px, as WebKit uses them for line layout.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FontMetrics {
    pub ascent: f32,
    pub descent: f32,
    pub line_gap: f32,
    pub x_height: f32,
    line_spacing: f32,
}

impl FontMetrics {
    /// The `line-height: normal` height. WebKit computes it from the rounded metrics, before
    /// its per-family adjustments, while the content area uses the unrounded ones.
    pub fn line_spacing(&self) -> f32 {
        self.line_spacing
    }
}

/// Font discovery, layout scratch space and a metrics cache.
pub struct Fonts {
    pub font_cx: FontContext,
    pub layout_cx: LayoutContext<TextBrush>,
    metrics: HashMap<(Arc<str>, u32, u8, u32), FontMetrics>,
}

impl std::fmt::Debug for Fonts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fonts").finish_non_exhaustive()
    }
}

impl Default for Fonts {
    fn default() -> Self {
        Fonts::new()
    }
}

impl Fonts {
    /// The system collection (CoreText on macOS) with fallback.
    pub fn new() -> Fonts {
        Fonts {
            font_cx: FontContext::new(),
            layout_cx: LayoutContext::new(),
            metrics: HashMap::new(),
        }
    }

    /// Register font data (a TTF/OTF/TTC file's bytes). Returns the family names registered.
    pub fn register_font(&mut self, data: Vec<u8>) -> Vec<String> {
        self.register_blob(parley::fontique::Blob::new(Arc::new(data)))
    }

    /// Register font data compiled into the program (a bundled face), without copying it.
    /// Returns the family names registered.
    pub fn register_static_font(&mut self, data: &'static [u8]) -> Vec<String> {
        self.register_blob(parley::fontique::Blob::new(
            Arc::new(data) as Arc<dyn AsRef<[u8]> + Send + Sync>
        ))
    }

    fn register_blob(&mut self, blob: parley::fontique::Blob<u8>) -> Vec<String> {
        let families = self.font_cx.collection.register_fonts(blob, None);
        self.metrics.clear();
        families
            .into_iter()
            .filter_map(|(id, _)| {
                self.font_cx
                    .collection
                    .family_name(id)
                    .map(|s| s.to_string())
            })
            .collect()
    }

    /// Metrics of the primary font for a style.
    pub fn metrics(&mut self, style: &ComputedStyle) -> FontMetrics {
        let key = (
            style.font_family.clone(),
            style.font_weight as u32,
            style.font_style as u8,
            style.font_size.to_bits(),
        );
        if let Some(m) = self.metrics.get(&key) {
            return *m;
        }
        let text = "x";
        let mut b = self
            .layout_cx
            .style_run_builder(&mut self.font_cx, text, 1.0, false);
        let idx = b.push_style(text_style(style, TextBrush::default()));
        b.push_style_run(idx, 0..text.len());
        let mut layout: parley::Layout<TextBrush> = b.build(text);
        layout.break_all_lines(None);
        let mut m = FontMetrics {
            ascent: (style.font_size * 0.8).round(),
            descent: (style.font_size * 0.2).round(),
            line_gap: 0.0,
            x_height: style.font_size * 0.5,
            line_spacing: style.font_size,
        };
        if let Some(line) = layout.lines().next()
            && let Some(run) = line.runs().next()
        {
            let rm = run.metrics();
            m = FontMetrics {
                ascent: rm.ascent,
                descent: rm.descent,
                line_gap: rm.leading,
                x_height: rm.x_height.unwrap_or(rm.ascent * 0.5),
                line_spacing: 0.0,
            };
            // WebKit's adjustment for Times, Helvetica and Courier, to match their Windows
            // counterparts: 15% of the font's height goes on the ascent. It goes by the font
            // used, so a generic family (`monospace` is Courier, `serif` Times) counts as the
            // family it resolves to.
            let mut primary = style
                .font_family
                .split(',')
                .next()
                .unwrap_or("")
                .trim()
                .trim_matches(|c| c == '"' || c == '\'')
                .to_ascii_lowercase();
            if let Some(generic) = parley::GenericFamily::parse(&primary) {
                let collection = &mut self.font_cx.collection;
                let first = collection.generic_families(generic).next();
                if let Some(name) = first.and_then(|id| collection.family_name(id)) {
                    primary = name.to_ascii_lowercase();
                }
            }
            if matches!(primary.as_str(), "times" | "helvetica" | "courier") {
                m.ascent += ((m.ascent + m.descent) * 0.15).round();
            }
            m.line_spacing = m.ascent.round() + m.descent.round() + m.line_gap.round();
        }
        self.metrics.insert(key, m);
        m
    }
}

/// The Parley style for a computed style.
pub fn text_style(
    style: &ComputedStyle,
    brush: TextBrush,
) -> parley::TextStyle<'static, 'static, TextBrush> {
    let nowrap = !style.white_space.wraps();
    parley::TextStyle {
        font_family: parley::FontFamily::Source(Cow::Owned(style.font_family.to_string())),
        font_size: style.font_size,
        font_width: parley::FontWidth::NORMAL,
        font_style: match style.font_style {
            FontStyle::Normal => parley::FontStyle::Normal,
            FontStyle::Italic => parley::FontStyle::Italic,
            FontStyle::Oblique => parley::FontStyle::Oblique(None),
        },
        font_weight: parley::FontWeight::new(style.font_weight),
        // Optical sizing (`font-optical-sizing: auto`), as CoreText applies it.
        font_variations: parley::FontVariations::List(Cow::Owned(vec![
            parley::setting::FontVariation {
                tag: parley::setting::Tag::new(b"opsz"),
                value: style.font_size,
            },
        ])),
        font_features: parley::FontFeatures::empty(),
        locale: None,
        brush,
        has_underline: false,
        underline_offset: None,
        underline_size: None,
        underline_brush: None,
        has_strikethrough: false,
        strikethrough_offset: None,
        strikethrough_size: None,
        strikethrough_brush: None,
        line_height: parley::LineHeight::FontSizeRelative(1.0),
        word_spacing: 0.0,
        letter_spacing: style.letter_spacing,
        word_break: match style.word_break {
            WordBreak::BreakAll => parley::WordBreak::BreakAll,
            WordBreak::KeepAll => parley::WordBreak::KeepAll,
            _ => parley::WordBreak::Normal,
        },
        overflow_wrap: match (style.overflow_wrap, style.word_break) {
            (OverflowWrap::Anywhere, _) => parley::OverflowWrap::Anywhere,
            (OverflowWrap::BreakWord, _) | (_, WordBreak::BreakWord) => {
                parley::OverflowWrap::BreakWord
            }
            _ => parley::OverflowWrap::Normal,
        },
        text_wrap_mode: if nowrap {
            parley::TextWrapMode::NoWrap
        } else {
            parley::TextWrapMode::Wrap
        },
    }
}
