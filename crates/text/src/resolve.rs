//! Font selection: the grid's family in four styles, and per-cluster fallback through fontique.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use fontique::{
    Attributes, Blob, Collection, CollectionOptions, FallbackKey, FamilyId, FontStyle, FontWeight,
    FontWidth, GenericFamily, QueryFamily, QueryFont, QueryStatus, Script, SourceCache, SourceKind,
};
use swash::text::Codepoint as _;

use crate::{CellMetrics, DEFAULT_FAMILIES, FontConfig, FontError, GlyphStyle, is_ignorable};

/// Families tried after script fallback, for symbols in the Common script (arrows, box drawing,
/// math, dingbats) that the grid's family lacks.
const SYMBOL_FALLBACKS: &[&str] = &[
    "Menlo",
    "Apple Symbols",
    "STIX Two Math",
    "Arial Unicode MS",
    "Last Resort",
];

#[cfg(target_os = "macos")]
const SF_MONO_DIR: &str = "/System/Applications/Utilities/Terminal.app/Contents/Resources/Fonts";

/// An index into the resolver's face table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct FaceId(pub u32);

/// A concrete face: one font in one file, with the synthesis it needs for the style it was
/// chosen for.
#[derive(Debug)]
pub(crate) struct Face {
    /// Unique per resolver: backends cache per-face state under it.
    #[cfg_attr(not(target_os = "macos"), expect(dead_code))]
    pub id: FaceId,
    pub blob: Blob<u8>,
    /// Index within a collection file.
    pub index: u32,
    /// The font file, when it came from disk (CoreText opens it by URL rather than copying it).
    #[cfg_attr(not(target_os = "macos"), expect(dead_code))]
    pub path: Option<PathBuf>,
    pub family: String,
    /// Has color glyph tables (sbix, CBDT or COLR): rasterized as RGBA.
    pub color: bool,
    /// Variation axis settings, as (tag, value).
    pub variations: Vec<(u32, f32)>,
    /// Faux bold.
    pub embolden: bool,
    /// Faux italic skew, in degrees.
    pub skew: Option<f32>,
}

impl Face {
    pub fn font_ref(&self) -> Option<swash::FontRef<'_>> {
        swash::FontRef::from_index(self.blob.data(), self.index as usize)
    }

    fn covers(&self, text: &str) -> bool {
        let Some(font) = self.font_ref() else {
            return false;
        };
        let charmap = font.charmap();
        text.chars()
            .filter(|&c| !is_ignorable(c))
            .all(|c| charmap.map(c) != 0)
    }
}

pub(crate) struct Resolver {
    collection: Collection,
    source_cache: SourceCache,
    /// The grid's family, then the configured alternates that exist.
    families: Vec<FamilyId>,
    primary_name: String,
    faces: Vec<Face>,
    /// (blob id, index, style) → face.
    face_ids: HashMap<(u64, u32, GlyphStyle), FaceId>,
    /// The grid's own face per style (`GlyphStyle::index`).
    primary: [Option<FaceId>; 4],
    /// Resolved clusters.
    clusters: HashMap<(Box<str>, GlyphStyle), FaceId>,
}

impl Resolver {
    pub fn new(config: &FontConfig) -> Result<Self, FontError> {
        let mut collection = Collection::new(CollectionOptions {
            shared: false,
            system_fonts: config.system_fonts,
        });
        // SF Mono, the system's monospace face, ships inside Terminal.app rather than in a font
        // directory CoreText lists.
        #[cfg(target_os = "macos")]
        if config.system_fonts {
            let sf_mono = std::path::Path::new(SF_MONO_DIR);
            if sf_mono.is_dir() {
                collection.load_fonts_from_paths([sf_mono]);
            }
        }
        for data in &config.extra_fonts {
            let blob = Blob::new(Arc::clone(data) as Arc<dyn AsRef<[u8]> + Send + Sync>);
            collection.register_fonts(blob, None);
        }
        let names: Vec<String> = if config.families.is_empty() {
            DEFAULT_FAMILIES.iter().map(|s| s.to_string()).collect()
        } else {
            config.families.clone()
        };
        let mut families = Vec::new();
        let mut primary_name = None;
        for name in &names {
            if let Some(id) = collection.family_id(name) {
                if primary_name.is_none() {
                    primary_name = collection.family_name(id).map(str::to_owned);
                }
                families.push(id);
            }
        }
        let Some(primary_name) = primary_name else {
            return Err(FontError::NoFamily(names));
        };
        let mut resolver = Resolver {
            collection,
            source_cache: SourceCache::default(),
            families,
            primary_name,
            faces: Vec::new(),
            face_ids: HashMap::new(),
            primary: [None; 4],
            clusters: HashMap::new(),
        };
        for style in [
            GlyphStyle::REGULAR,
            GlyphStyle {
                bold: true,
                italic: false,
            },
            GlyphStyle {
                bold: false,
                italic: true,
            },
            GlyphStyle {
                bold: true,
                italic: true,
            },
        ] {
            let families: Vec<QueryFamily<'static>> = resolver
                .families
                .iter()
                .map(|&id| QueryFamily::Id(id))
                .collect();
            let face = resolver.first_match(&families, None, style, |_| true);
            resolver.primary[style.index()] = face;
        }
        if resolver.primary[0].is_none() {
            return Err(FontError::BadFont(resolver.primary_name.clone()));
        }
        Ok(resolver)
    }

    pub fn primary_family_name(&self) -> &str {
        &self.primary_name
    }

    pub fn face(&self, id: FaceId) -> &Face {
        &self.faces[id.0 as usize]
    }

    fn primary_face(&self, style: GlyphStyle) -> FaceId {
        self.primary[style.index()]
            .or(self.primary[0])
            .expect("checked in new")
    }

    pub fn is_primary(&self, id: FaceId, style: GlyphStyle) -> bool {
        self.primary_face(style) == id
    }

    /// Cell metrics from the regular face at `size_px`.
    pub fn cell_metrics(&mut self, size_px: f32) -> Result<CellMetrics, FontError> {
        let face = self.face(self.primary_face(GlyphStyle::REGULAR));
        let font = face
            .font_ref()
            .ok_or_else(|| FontError::BadFont(face.family.clone()))?;
        let coords: Vec<i16> = normalized_coords(&font, &face.variations);
        let m = font.metrics(&coords).scale(size_px);
        let advance = {
            let glyph = font.charmap().map('0');
            let glyph = if glyph == 0 {
                font.charmap().map('M')
            } else {
                glyph
            };
            font.glyph_metrics(&coords)
                .scale(size_px)
                .advance_width(glyph)
        };
        let cell_width = advance.round().max(1.0) as u32;
        let leading = m.leading.max(0.0).round();
        let cell_height = ((m.ascent + m.descent).ceil() + leading).max(1.0) as u32;
        let baseline = ((leading / 2.0).floor() + m.ascent.round()) as u32;
        let underline_thickness = m.stroke_size.round().max(1.0) as u32;
        // `underline_offset` is the distance of the underline's center above the baseline
        // (negative: below).
        let underline_center = baseline as f32 - m.underline_offset;
        let underline_position = (underline_center - underline_thickness as f32 / 2.0)
            .round()
            .clamp(0.0, (cell_height - underline_thickness) as f32)
            as u32;
        let strike_center = if m.strikeout_offset > 0.0 {
            baseline as f32 - m.strikeout_offset
        } else {
            baseline as f32 - m.x_height / 2.0
        };
        let strikeout_position = (strike_center - underline_thickness as f32 / 2.0)
            .round()
            .clamp(0.0, (cell_height - underline_thickness) as f32)
            as u32;
        Ok(CellMetrics {
            cell_width,
            cell_height,
            baseline,
            underline_position,
            underline_thickness,
            strikeout_position,
            strikeout_thickness: underline_thickness,
        })
    }

    /// The face a grid cluster draws in.
    pub fn resolve(&mut self, text: &str, style: GlyphStyle) -> FaceId {
        if let Some(&id) = self.clusters.get(&(Box::<str>::from(text), style)) {
            return id;
        }
        let id = self.resolve_uncached(text, style);
        self.clusters.insert((text.into(), style), id);
        id
    }

    fn resolve_uncached(&mut self, text: &str, style: GlyphStyle) -> FaceId {
        let primary = self.primary_face(style);
        let emoji = wants_emoji(text);
        if !emoji && self.face(primary).covers(text) {
            return primary;
        }
        let covers = |face: &Face| face.covers(text);
        if emoji {
            let families = [QueryFamily::Generic(GenericFamily::Emoji)];
            if let Some(face) = self.first_match(&families, None, style, covers) {
                return face;
            }
            if self.face(primary).covers(text) {
                return primary;
            }
        }
        let script = cluster_script(text);
        let families: Vec<QueryFamily<'static>> = self
            .families
            .iter()
            .map(|&id| QueryFamily::Id(id))
            .collect();
        if let Some(face) = self.first_match(&families, Some(script), style, covers) {
            return face;
        }
        let symbols: Vec<FamilyId> = SYMBOL_FALLBACKS
            .iter()
            .filter_map(|name| self.collection.family_id(name))
            .collect();
        let symbols: Vec<QueryFamily<'static>> = symbols
            .into_iter()
            .map(QueryFamily::Id)
            .chain([
                QueryFamily::Generic(GenericFamily::SystemUi),
                QueryFamily::Generic(GenericFamily::Emoji),
            ])
            .collect();
        if let Some(face) = self.first_match(&symbols, None, style, covers) {
            return face;
        }
        // Nothing covers it. The grid's face draws it; CoreText falls back on its own there,
        // swash draws the missing glyph.
        primary
    }

    /// The first font among `families` (then the fallbacks for `script`) that passes `accept`.
    fn first_match(
        &mut self,
        families: &[QueryFamily<'static>],
        script: Option<Script>,
        style: GlyphStyle,
        accept: impl Fn(&Face) -> bool,
    ) -> Option<FaceId> {
        let attributes = Attributes::new(
            FontWidth::NORMAL,
            if style.italic {
                FontStyle::Italic
            } else {
                FontStyle::Normal
            },
            if style.bold {
                FontWeight::BOLD
            } else {
                FontWeight::NORMAL
            },
        );
        let mut found: Vec<QueryFont> = Vec::new();
        {
            let mut query = self.collection.query(&mut self.source_cache);
            query.set_families(families.iter().copied());
            query.set_attributes(attributes);
            if let Some(script) = script {
                query.set_fallbacks(FallbackKey::new(script, None));
            }
            query.matches_with(|font| {
                found.push(font.clone());
                QueryStatus::Continue
            });
        }
        for font in found {
            let id = self.intern(&font, style);
            if accept(self.face(id)) {
                return Some(id);
            }
        }
        None
    }

    fn intern(&mut self, font: &QueryFont, style: GlyphStyle) -> FaceId {
        let key = (font.blob.id(), font.index, style);
        if let Some(&id) = self.face_ids.get(&key) {
            return id;
        }
        let (family_id, font_index) = font.family;
        let info = self.collection.family(family_id);
        let family = info
            .as_ref()
            .map(|f| f.name().to_owned())
            .unwrap_or_default();
        let path = info
            .as_ref()
            .and_then(|f| f.fonts().get(font_index))
            .and_then(|f| match f.source().kind() {
                SourceKind::Path(path) => Some(path.to_path_buf()),
                SourceKind::Memory(_) => None,
            });
        let id = FaceId(self.faces.len() as u32);
        let mut face = Face {
            id,
            blob: font.blob.clone(),
            index: font.index,
            path,
            family,
            color: false,
            variations: font
                .synthesis
                .variation_settings()
                .iter()
                .map(|(tag, value)| (u32::from_be_bytes(tag.to_be_bytes()), *value))
                .collect(),
            embolden: font.synthesis.embolden(),
            skew: font.synthesis.skew(),
        };
        face.color = face.font_ref().is_some_and(|f| {
            [*b"sbix", *b"CBDT", *b"COLR"]
                .iter()
                .any(|tag| f.table(u32::from_be_bytes(*tag)).is_some())
        });
        self.faces.push(face);
        self.face_ids.insert(key, id);
        id
    }
}

/// swash's normalized variation coordinates for `settings`.
pub(crate) fn normalized_coords(font: &swash::FontRef<'_>, settings: &[(u32, f32)]) -> Vec<i16> {
    if settings.is_empty() {
        return Vec::new();
    }
    font.variations()
        .normalized_coords(settings.iter().map(|&(tag, value)| (tag, value)))
        .collect()
}

/// Whether a cluster asks for emoji presentation: an explicit VS16, a flag, a keycap, a ZWJ
/// sequence, or a pictograph from the emoji planes (whose default presentation is emoji).
fn wants_emoji(text: &str) -> bool {
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if text.contains('\u{FE0E}') {
        return false;
    }
    text.contains('\u{FE0F}')
        || text.contains('\u{20E3}')
        || ('\u{1F1E6}'..='\u{1F1FF}').contains(&first)
        || (text.contains('\u{200D}') && first.is_extended_pictographic())
        || (first >= '\u{1F000}' && first.is_extended_pictographic())
}

/// The ISO 15924 script of a cluster's first character, for fallback.
fn cluster_script(text: &str) -> Script {
    let common = Script::from_bytes(*b"Zyyy");
    let Some(c) = text.chars().next() else {
        return common;
    };
    let tag = c.script().to_opentype().to_be_bytes();
    let name: String = tag
        .iter()
        .map(|&b| b as char)
        .filter(|c| c.is_ascii_alphabetic())
        .collect();
    if name.len() != 4 {
        return common;
    }
    // OpenType's tags are ISO 15924 codes in lower case for the scripts that matter here, with
    // a few legacy exceptions.
    let name = match name.as_str() {
        "kana" if ('\u{3040}'..='\u{309F}').contains(&c) => "Hira".to_owned(),
        "DFLT" | "zyyy" | "zinh" => return common,
        _ => name,
    };
    Script::parse(&name).unwrap_or(common)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emoji_presentation() {
        assert!(wants_emoji("😀"));
        assert!(wants_emoji("❤\u{FE0F}"));
        assert!(wants_emoji("🇯🇵"));
        assert!(wants_emoji("1\u{FE0F}\u{20E3}"));
        assert!(wants_emoji("👩\u{200D}💻"));
        assert!(!wants_emoji("a"));
        assert!(!wants_emoji("❤"));
        assert!(!wants_emoji("😀\u{FE0E}"));
        assert!(!wants_emoji("©"));
    }

    #[test]
    fn scripts() {
        assert_eq!(cluster_script("a").as_str(), "Latn");
        assert_eq!(cluster_script("あ").as_str(), "Hira");
        assert_eq!(cluster_script("漢").as_str(), "Hani");
        assert_eq!(cluster_script("ж").as_str(), "Cyrl");
        assert_eq!(cluster_script("─").as_str(), "Zyyy");
    }
}
