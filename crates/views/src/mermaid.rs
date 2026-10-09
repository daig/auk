//! Mermaid diagrams in `md` fences (§8.5, §18.4, amendment A23; book:5018, 5026, 5065).
//!
//! The engine is [merman](https://crates.io/crates/merman), a headless Rust implementation of
//! Mermaid (parse, layout, SVG) that needs no JavaScript runtime or browser, so the same code
//! draws natively and in the browser client (wasm32). Its resvg-safe SVG is drawn by resvg into a
//! picture like an SVG `image`, and the picture mounts as a replaced box at its natural size.
//!
//! - **Kinds:** flowchart and graph (every direction, node shapes, edge labels, subgraphs),
//!   sequence, class, state, ER, gantt and pie ([`KINDS`]). Any other kind, a source that doesn't
//!   parse, or one over [`MAX_SOURCE`] bytes stays a code block with a quiet note saying why.
//! - **Off the frame path:** a diagram is laid out and drawn in the background (natively on the
//!   image thread, in the browser one job per clock tick), cached by source, theme, fonts and
//!   density, so a re-render or another view of the same diagram is immediate.
//! - **Deterministic:** merman's deterministic environment (no clock, randomness or time zone)
//!   and text measured from the font resvg draws with.
//! - **Sandboxed:** no script engine; the SVG reaches nothing outside itself (no files, URLs or
//!   fonts but the terminal's); `click` callbacks and links in the source do nothing.
//! - **Themed:** the colors are the terminal theme's (theme.css, dark or light): text `--t1`,
//!   nodes an accent tint with an `--accent` border, edges `--t2`, notes a `--warn` tint, the
//!   figure on `--code-bg`; the text is the terminal's monospace face at 13px.

use crate::image::{self, Frame, Loaded, Loops, Picture, Poll};
use merman::svg::{
    DeterministicTextMeasurer, HostTheme, HostThemeAppearance, MeasurementProfileId, Presentation,
    SvgOutputPolicy, SvgPipelinePreset, TextMeasurementPolicy, TextMeasurementProfile,
    TextMeasurementProfileIdentity, TextStyle, ThemeRole,
};
use merman::{OperationControl, RenderOutput, RenderRequest, Renderer, SvgEnvironment, SvgRequest};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, OnceLock};

/// The diagram kinds drawn, by their head label (`mermaid · <kind>`).
pub const KINDS: [&str; 7] = [
    "flowchart",
    "sequence",
    "class",
    "state",
    "er",
    "gantt",
    "pie",
];

/// The largest source drawn as a diagram, in bytes.
pub const MAX_SOURCE: usize = 32 << 10;

/// The diagram text size in px at the 13px design size (regions zoom it, §8.3).
const FONT_PX: f32 = 13.0;

/// What a fence's first statement says it is: `Ok(kind)` for a kind drawn ([`KINDS`]), or
/// `Err(keyword)` for one that isn't (empty when there is no statement).
pub fn kind(src: &str) -> Result<&'static str, String> {
    let mut lines = src.lines().map(str::trim);
    let mut in_front = false;
    // Inside a directive (`%%{ … }%%`) that runs over several lines.
    let mut in_directive = false;
    let mut first = true;
    for line in lines.by_ref() {
        // YAML front matter.
        if first && line == "---" {
            in_front = true;
            first = false;
            continue;
        }
        first = false;
        if in_front {
            if line == "---" {
                in_front = false;
            }
            continue;
        }
        if in_directive {
            in_directive = !line.contains("}%%");
            continue;
        }
        if line.starts_with("%%{") {
            in_directive = !line.contains("}%%");
            continue;
        }
        if line.is_empty() || line.starts_with("%%") {
            continue;
        }
        let word = line
            .split(|c: char| c.is_whitespace() || c == ';')
            .next()
            .unwrap_or("");
        return match word {
            "flowchart" | "graph" | "flowchart-elk" => Ok("flowchart"),
            "sequenceDiagram" => Ok("sequence"),
            "classDiagram" | "classDiagram-v2" => Ok("class"),
            "stateDiagram" | "stateDiagram-v2" => Ok("state"),
            "erDiagram" => Ok("er"),
            "gantt" => Ok("gantt"),
            "pie" => Ok("pie"),
            w => Err(w.to_owned()),
        };
    }
    Err(String::new())
}

// ---------------------------------------------------------------------------------------------
// Theme

/// The theme's variables (theme.css): light at `:root`, dark over it under `data-theme`.
fn theme_vars(dark: bool) -> HashMap<String, String> {
    const CSS: &str = include_str!("css/theme.css");
    fn block(css: &str, head: &str, out: &mut HashMap<String, String>) {
        let Some(at) = css.find(head) else { return };
        let body = &css[at + head.len()..];
        let body = &body[..body.find('}').unwrap_or(body.len())];
        for decl in body.split(';') {
            if let Some((k, v)) = decl.split_once(':') {
                let k = k.trim();
                if let Some(name) = k.strip_prefix("--") {
                    out.insert(name.to_owned(), v.trim().to_owned());
                }
            }
        }
    }
    let mut vars = HashMap::new();
    block(CSS, ":root {", &mut vars);
    if dark {
        block(CSS, ":root[data-theme='dark'] {", &mut vars);
    }
    vars
}

type Rgba = [f32; 4];

fn parse_color(v: &str) -> Option<Rgba> {
    let v = v.trim();
    if let Some(h) = v.strip_prefix('#') {
        let n = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok();
        return match h.len() {
            6 => Some([f32::from(n(0)?), f32::from(n(2)?), f32::from(n(4)?), 1.0]),
            3 => {
                let d = |i: usize| {
                    u8::from_str_radix(h.get(i..i + 1)?, 16)
                        .ok()
                        .map(|x| x * 17)
                };
                Some([f32::from(d(0)?), f32::from(d(1)?), f32::from(d(2)?), 1.0])
            }
            _ => None,
        };
    }
    let inner = v
        .strip_prefix("rgba(")
        .or_else(|| v.strip_prefix("rgb("))?
        .strip_suffix(')')?;
    let parts: Vec<f32> = inner
        .split(',')
        .map(|p| p.trim().parse::<f32>().ok())
        .collect::<Option<_>>()?;
    match parts.as_slice() {
        [r, g, b] => Some([*r, *g, *b, 1.0]),
        [r, g, b, a] => Some([*r, *g, *b, *a]),
        _ => None,
    }
}

/// `a` over `b` at `t` (and `a`'s own alpha).
fn mix(a: Rgba, b: Rgba, t: f32) -> Rgba {
    let t = t * a[3];
    [
        a[0] * t + b[0] * (1.0 - t),
        a[1] * t + b[1] * (1.0 - t),
        a[2] * t + b[2] * (1.0 - t),
        1.0,
    ]
}

fn hex(c: Rgba) -> String {
    format!(
        "#{:02x}{:02x}{:02x}",
        c[0].round().clamp(0.0, 255.0) as u8,
        c[1].round().clamp(0.0, 255.0) as u8,
        c[2].round().clamp(0.0, 255.0) as u8
    )
}

/// A diagram's colors, flattened onto the figure's background.
#[derive(Clone, Debug, PartialEq)]
pub struct Colors {
    pub canvas: String,
    pub node: String,
    pub node_alt: String,
    pub muted: String,
    pub text: String,
    pub subtle: String,
    pub border: String,
    pub line: String,
    pub cluster: String,
    pub cluster_border: String,
    pub note: String,
    pub note_border: String,
    pub activation: String,
    pub error: String,
    pub warning: String,
    pub success: String,
    pub series: Vec<String>,
}

/// The diagram colors of the terminal theme, dark or light.
pub fn colors(dark: bool) -> Colors {
    let vars = theme_vars(dark);
    let get = |k: &str| {
        vars.get(k)
            .and_then(|v| parse_color(v))
            .unwrap_or([128.0, 128.0, 128.0, 1.0])
    };
    let bg = get("code-bg");
    let flat = |k: &str| hex(mix(get(k), bg, 1.0));
    let tint = |k: &str, t: f32| hex(mix(get(k), bg, t));
    Colors {
        canvas: hex(bg),
        node: tint("accent", 0.16),
        node_alt: tint("a2", 0.16),
        muted: tint("t1", 0.06),
        text: flat("t1"),
        subtle: flat("t3"),
        border: flat("accent"),
        line: flat("t2"),
        cluster: tint("t1", 0.04),
        cluster_border: flat("l4"),
        note: tint("warn", 0.16),
        note_border: flat("warn"),
        activation: tint("t1", 0.12),
        error: flat("bad"),
        warning: flat("warn"),
        success: flat("ok"),
        series: ["accent", "ok", "warn", "info", "live", "bad", "ansi6", "a2"]
            .iter()
            .map(|k| flat(k))
            .collect(),
    }
}

fn host_theme(dark: bool) -> Result<HostTheme, String> {
    let c = colors(dark);
    let e = |e: merman::svg::PresentationError| e.to_string();
    let roles = [
        (ThemeRole::Canvas, &c.canvas),
        (ThemeRole::Surface, &c.node),
        (ThemeRole::SurfaceAlt, &c.node_alt),
        (ThemeRole::SurfaceMuted, &c.muted),
        (ThemeRole::Text, &c.text),
        (ThemeRole::SubtleText, &c.subtle),
        (ThemeRole::Border, &c.border),
        (ThemeRole::Line, &c.line),
        (ThemeRole::EdgeLabelBackground, &c.canvas),
        (ThemeRole::ClusterBackground, &c.cluster),
        (ThemeRole::ClusterBorder, &c.cluster_border),
        (ThemeRole::NoteBackground, &c.note),
        (ThemeRole::NoteBorder, &c.note_border),
        (ThemeRole::NoteText, &c.text),
        (ThemeRole::ActorBackground, &c.node),
        (ThemeRole::ActorBorder, &c.border),
        (ThemeRole::ActorText, &c.text),
        (ThemeRole::ActivationBackground, &c.activation),
        (ThemeRole::ActivationBorder, &c.subtle),
        (ThemeRole::Error, &c.error),
        (ThemeRole::Warning, &c.warning),
        (ThemeRole::Success, &c.success),
    ];
    let mut t = HostTheme::new()
        .with_appearance(if dark {
            HostThemeAppearance::Dark
        } else {
            HostThemeAppearance::Light
        })
        .try_with_font_family("monospace")
        .map_err(e)?
        .try_with_font_size(format!("{FONT_PX}px"))
        .map_err(e)?;
    for (role, v) in roles {
        t = t.try_with_role(role, v.as_str()).map_err(e)?;
    }
    t.try_with_series_palette(c.series.iter().map(String::as_str))
        .map_err(e)
}

// ---------------------------------------------------------------------------------------------
// Text measurement: the face resvg draws `monospace` with

/// Advances in em of the monospace face's characters.
struct Advances {
    map: HashMap<char, f32>,
    /// For characters the face lacks (resvg falls back to another font).
    narrow: f32,
}

fn advances() -> Arc<Advances> {
    static CACHE: Mutex<Option<(u64, Arc<Advances>)>> = Mutex::new(None);
    let generation = image::font_generation();
    let mut slot = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((g, a)) = slot.as_ref()
        && *g == generation
    {
        return a.clone();
    }
    let a = Arc::new(load_advances());
    *slot = Some((generation, a.clone()));
    a
}

fn load_advances() -> Advances {
    use resvg::usvg::fontdb::{Family, Query};
    use skrifa::MetadataProvider;
    let db = image::svg_fonts();
    let id = db.query(&Query {
        families: &[Family::Monospace],
        ..Query::default()
    });
    let mut map = HashMap::new();
    if let Some(id) = id {
        db.with_face_data(id, |data, index| {
            let Ok(font) = skrifa::FontRef::from_index(data, index) else {
                return;
            };
            let metrics = font.glyph_metrics(
                skrifa::instance::Size::new(1.0),
                skrifa::instance::LocationRef::default(),
            );
            for (cp, gid) in font.charmap().mappings() {
                if let (Some(c), Some(w)) = (char::from_u32(cp), metrics.advance_width(gid)) {
                    map.insert(c, w);
                }
            }
        });
    }
    let narrow = map.get(&'0').copied().unwrap_or(0.6);
    Advances { map, narrow }
}

fn measure(adv: &Advances, text: &str, style: &TextStyle) -> f64 {
    let em: f32 = text
        .chars()
        .map(|c| {
            adv.map.get(&c).copied().unwrap_or_else(|| {
                if is_wide(c) {
                    adv.narrow * 2.0
                } else {
                    adv.narrow
                }
            })
        })
        .sum();
    f64::from(em) * style.font_size.max(1.0)
}

/// East Asian wide characters, roughly: CJK, Hangul, kana and fullwidth forms.
fn is_wide(c: char) -> bool {
    matches!(u32::from(c),
        0x1100..=0x115f | 0x2e80..=0xa4cf | 0xac00..=0xd7a3 | 0xf900..=0xfaff
        | 0xfe30..=0xfe4f | 0xff00..=0xff60 | 0xffe0..=0xffe6 | 0x1f300..=0x1faff
        | 0x20000..=0x3fffd)
}

// ---------------------------------------------------------------------------------------------
// Rendering

/// A diagram's SVG (resvg-safe, themed), or why it can't be drawn.
pub fn svg(src: &str, dark: bool) -> Result<String, String> {
    if src.len() > MAX_SOURCE {
        return Err(format!("over {} KiB", MAX_SOURCE >> 10));
    }
    kind(src).map_err(|k| unsupported(&k))?;
    let resolved = Presentation::new().with_theme(host_theme(dark)?).resolve();
    let renderer = Renderer::new().with_engine(resolved.materialize_engine(merman::Engine::new()));
    let adv = advances();
    let measurer =
        DeterministicTextMeasurer::default().with_width_callback(move |t, s| measure(&adv, t, s));
    let identity = TextMeasurementProfileIdentity::new(
        MeasurementProfileId::new("auk.svg-monospace").map_err(|e| e.to_string())?,
        format!("fonts-{}", image::font_generation()),
    )
    .map_err(|e| e.to_string())?;
    let policy = TextMeasurementPolicy::uniform(TextMeasurementProfile::new(identity, measurer));
    let output = SvgOutputPolicy {
        preset: SvgPipelinePreset::ResvgSafe,
        // The figure's own background shows through.
        root_background_color: Some("transparent".to_owned()),
        ..SvgOutputPolicy::default()
    };
    let request = SvgRequest {
        environment: SvgEnvironment::deterministic().with_text_measurement_policy(policy),
        pipeline: Some(output.pipeline()),
        presentation: resolved.render_policy(),
        options: merman::svg::SvgRenderOptions {
            diagram_id: Some("m".to_owned()),
            ..Default::default()
        },
        ..Default::default()
    };
    match renderer.render(RenderRequest::svg(src, OperationControl::new(), request)) {
        Ok(RenderOutput::Svg(Some(svg))) => Ok(svg.svg().to_string()),
        Ok(_) => Err("no diagram in the fence".into()),
        Err(e) => Err(short_error(&e.to_string())),
    }
}

/// The note for a kind that isn't drawn.
pub fn unsupported(keyword: &str) -> String {
    if keyword.is_empty() {
        "no diagram in the fence".into()
    } else {
        format!("`{keyword}` diagrams aren't drawn")
    }
}

/// The first line of an engine error, without its diagnostic dump.
fn short_error(e: &str) -> String {
    let line = e.lines().next().unwrap_or(e).trim();
    let line = line.strip_prefix("parse error: ").unwrap_or(line);
    let mut s: String = line.chars().take(160).collect();
    if s.len() < line.len() {
        s.push('…');
    }
    s
}

/// The SVG drawn into a picture at `density`.
fn picture(svg: &str, density: f32) -> Result<Picture, String> {
    let (content, size) = image::render_svg(svg, density)?;
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

/// A diagram's identity: its source, theme and the fonts it was measured with.
pub fn id(src: &str, dark: bool) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    src.hash(&mut h);
    format!(
        "mermaid:{:016x}{}-f{}",
        h.finish(),
        if dark { "d" } else { "l" },
        image::font_generation()
    )
}

/// Sources by diagram id, for drawing one again at another density (the viewer).
fn sources() -> &'static Mutex<HashMap<String, (String, bool)>> {
    static S: OnceLock<Mutex<HashMap<String, (String, bool)>>> = OnceLock::new();
    S.get_or_init(Mutex::default)
}

fn remember(id: &str, src: &str, dark: bool) {
    let mut s = sources().lock().unwrap_or_else(|e| e.into_inner());
    if s.len() > 512 {
        s.clear();
    }
    s.entry(id.to_owned())
        .or_insert_with(|| (src.to_owned(), dark));
}

/// The density buckets pictures are drawn at (a quarter).
fn bucket(density: f32) -> f32 {
    ((density.clamp(0.25, 8.0)) * 4.0).round() / 4.0
}

/// A diagram's picture at `density`: ready when cached (or failed: the error says why),
/// otherwise laid out and drawn in the background.
pub fn request(src: &str, dark: bool, density: f32) -> (String, Poll) {
    let id = id(src, dark);
    remember(&id, src, dark);
    let d = bucket(density);
    let key = format!("{id}@{d}");
    let owned = src.to_owned();
    let poll = image::request_made(&key, move || picture(&svg(&owned, dark)?, d));
    (id, poll)
}

/// The picture of a diagram already requested, if it's done.
pub fn peek(id: &str, density: f32) -> Option<Loaded> {
    let key = format!("{id}@{}", bucket(density));
    image::peek_blob(&key)
}

/// A diagram drawn now (for the viewer, at its scale; and tests).
pub fn load_for_viewer(id: &str, density: f32) -> Option<Loaded> {
    let (src, dark) = sources()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(id)
        .cloned()?;
    let d = bucket(density);
    Some(image::load_made(&format!("{id}@{d}"), || {
        picture(&svg(&src, dark)?, d)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_come_from_the_first_statement() {
        assert_eq!(kind("graph TD\n a-->b"), Ok("flowchart"));
        assert_eq!(kind("%% c\n\nflowchart LR; a-->b"), Ok("flowchart"));
        assert_eq!(
            kind("---\ntitle: x\n---\nsequenceDiagram\n a->>b: hi"),
            Ok("sequence")
        );
        assert_eq!(kind("classDiagram-v2\n"), Ok("class"));
        // A directive over several lines, and one on a line.
        assert_eq!(
            kind("%%{\n  init: { \"theme\": \"dark\" }\n}%%\nsequenceDiagram\n a->>b: hi"),
            Ok("sequence")
        );
        assert_eq!(kind("%%{init: {}}%%\npie\n \"a\": 1"), Ok("pie"));
        assert_eq!(kind("stateDiagram-v2\n"), Ok("state"));
        assert_eq!(kind("erDiagram\n"), Ok("er"));
        assert_eq!(kind("gantt\n"), Ok("gantt"));
        assert_eq!(kind("pie title x\n"), Ok("pie"));
        assert_eq!(kind("mindmap\n root"), Err("mindmap".into()));
        assert_eq!(kind("\n\n"), Err(String::new()));
    }

    #[test]
    fn theme_colors_follow_dark_and_light() {
        let d = colors(true);
        let l = colors(false);
        assert_eq!(d.canvas, "#1a1b20");
        assert_eq!(l.canvas, "#f4f5f8");
        assert_eq!(d.text, "#ececf1");
        assert_eq!(l.text, "#1b1c20");
        assert_eq!(d.border, "#7c8cff");
        assert_eq!(l.border, "#4f5bd5");
        assert_ne!(d.node, l.node);
        assert_eq!(parse_color("rgba(255, 255, 255, 0.22)").unwrap()[3], 0.22);
    }
}
