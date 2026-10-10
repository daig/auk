//! The property table: names, specified-value grammars, shorthand expansion and computation.

use super::values::*;
use crate::style::*;
use cssparser::{ParseError, Parser, Token};
use std::sync::Arc;

macro_rules! props {
    ($($v:ident = $name:literal $(, $inh:ident)?;)*) => {
        /// A longhand property.
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
        pub enum Prop { $($v,)* }
        impl Prop {
            /// Every longhand, in computation order.
            pub const ALL: &'static [Prop] = &[$(Prop::$v,)*];
            pub const COUNT: usize = Prop::ALL.len();
            pub fn name(self) -> &'static str {
                match self { $(Prop::$v => $name,)* }
            }
            pub fn from_name(name: &str) -> Option<Prop> {
                match name { $($name => Some(Prop::$v),)* _ => None }
            }
            pub fn inherited(self) -> bool {
                match self { $(Prop::$v => props!(@inh $($inh)?),)* }
            }
        }
    };
    (@inh inherited) => { true };
    (@inh) => { false };
}

// Order matters: zoom, then font-size (em units), then color (currentColor), then everything else.
props! {
    Zoom = "zoom";
    FontSize = "font-size", inherited;
    FontFamily = "font-family", inherited;
    FontWeight = "font-weight", inherited;
    FontStyle = "font-style", inherited;
    LineHeight = "line-height", inherited;
    ColorScheme = "color-scheme", inherited;
    Color = "color", inherited;
    LetterSpacing = "letter-spacing", inherited;
    TextAlign = "text-align", inherited;
    WhiteSpace = "white-space", inherited;
    WordBreak = "word-break", inherited;
    OverflowWrap = "overflow-wrap", inherited;
    TextTransform = "text-transform", inherited;
    Visibility = "visibility", inherited;
    Cursor = "cursor", inherited;
    PointerEvents = "pointer-events", inherited;
    UserSelect = "user-select", inherited;
    ListStyleType = "list-style-type", inherited;
    ListStylePosition = "list-style-position", inherited;
    BorderCollapse = "border-collapse", inherited;
    BorderSpacing = "border-spacing", inherited;
    Display = "display";
    Position = "position";
    BoxSizing = "box-sizing";
    Top = "top";
    Right = "right";
    Bottom = "bottom";
    Left = "left";
    MarginTop = "margin-top";
    MarginRight = "margin-right";
    MarginBottom = "margin-bottom";
    MarginLeft = "margin-left";
    PaddingTop = "padding-top";
    PaddingRight = "padding-right";
    PaddingBottom = "padding-bottom";
    PaddingLeft = "padding-left";
    BorderTopStyle = "border-top-style";
    BorderRightStyle = "border-right-style";
    BorderBottomStyle = "border-bottom-style";
    BorderLeftStyle = "border-left-style";
    BorderTopWidth = "border-top-width";
    BorderRightWidth = "border-right-width";
    BorderBottomWidth = "border-bottom-width";
    BorderLeftWidth = "border-left-width";
    BorderTopColor = "border-top-color";
    BorderRightColor = "border-right-color";
    BorderBottomColor = "border-bottom-color";
    BorderLeftColor = "border-left-color";
    BorderTopLeftRadius = "border-top-left-radius";
    BorderTopRightRadius = "border-top-right-radius";
    BorderBottomRightRadius = "border-bottom-right-radius";
    BorderBottomLeftRadius = "border-bottom-left-radius";
    Width = "width";
    Height = "height";
    MinWidth = "min-width";
    MinHeight = "min-height";
    MaxWidth = "max-width";
    MaxHeight = "max-height";
    AspectRatio = "aspect-ratio";
    OverflowX = "overflow-x";
    OverflowY = "overflow-y";
    FlexDirection = "flex-direction";
    FlexWrap = "flex-wrap";
    FlexGrow = "flex-grow";
    FlexShrink = "flex-shrink";
    FlexBasis = "flex-basis";
    Order = "order";
    AlignItems = "align-items";
    AlignSelf = "align-self";
    AlignContent = "align-content";
    JustifyContent = "justify-content";
    JustifyItems = "justify-items";
    JustifySelf = "justify-self";
    RowGap = "row-gap";
    ColumnGap = "column-gap";
    GridTemplateColumns = "grid-template-columns";
    GridTemplateRows = "grid-template-rows";
    GridTemplateAreas = "grid-template-areas";
    GridAutoFlow = "grid-auto-flow";
    GridAutoColumns = "grid-auto-columns";
    GridAutoRows = "grid-auto-rows";
    GridRowStart = "grid-row-start";
    GridRowEnd = "grid-row-end";
    GridColumnStart = "grid-column-start";
    GridColumnEnd = "grid-column-end";
    BackgroundColor = "background-color";
    BackgroundImage = "background-image";
    Opacity = "opacity";
    ZIndex = "z-index";
    VerticalAlign = "vertical-align";
    TextOverflow = "text-overflow";
    TextDecorationLine = "text-decoration-line";
    TextDecorationStyle = "text-decoration-style";
    TextDecorationColor = "text-decoration-color";
    Transform = "transform";
    TransformOrigin = "transform-origin";
    OutlineWidth = "outline-width";
    OutlineStyle = "outline-style";
    OutlineColor = "outline-color";
    OutlineOffset = "outline-offset";
    BoxShadow = "box-shadow";
    ObjectFit = "object-fit";
    ImageRendering = "image-rendering", inherited;
    Content = "content";
    TransitionProperty = "transition-property";
    TransitionDuration = "transition-duration";
    TransitionTimingFunction = "transition-timing-function";
    TransitionDelay = "transition-delay";
    AnimationName = "animation-name";
    AnimationDuration = "animation-duration";
    AnimationTimingFunction = "animation-timing-function";
    AnimationDelay = "animation-delay";
    AnimationIterationCount = "animation-iteration-count";
    AnimationDirection = "animation-direction";
    AnimationFillMode = "animation-fill-mode";
    AnimationPlayState = "animation-play-state";
    LineClamp = "line-clamp";
}

/// Properties Tern accepts that we parse but don't draw yet (effects, SVG paint, scroll
/// behavior). They are validated only for `url()`.
const ACCEPTED_UNUSED: &[&str] = &[
    "background-position",
    "background-size",
    "background-repeat",
    "background-origin",
    "background-clip",
    "translate",
    "rotate",
    "scale",
    "perspective",
    "perspective-origin",
    "filter",
    "backdrop-filter",
    "mix-blend-mode",
    "isolation",
    "clip-path",
    "mask",
    "mask-image",
    "mask-position",
    "mask-size",
    "mask-repeat",
    "mask-origin",
    "mask-clip",
    "mask-composite",
    "mask-mode",
    "will-change",
    "text-wrap",
    "font-variant-numeric",
    "-webkit-font-smoothing",
    "tab-size",
    "list-style-image",
    "caret-color",
    "touch-action",
    "scrollbar-width",
    "scrollbar-color",
    "overscroll-behavior",
    "scroll-behavior",
    "overflow-anchor",
    "scroll-padding",
    "scroll-padding-top",
    "scroll-padding-right",
    "scroll-padding-bottom",
    "scroll-padding-left",
    "scroll-snap-type",
    "scroll-snap-align",
    "scroll-snap-stop",
    "app-region",
    "fill",
    "fill-opacity",
    "stroke",
    "stroke-width",
    "stroke-linecap",
    "stroke-linejoin",
    "stroke-miterlimit",
    "stroke-dasharray",
    "stroke-dashoffset",
    "stroke-opacity",
    "stop-color",
    "stop-opacity",
    "rx",
    "ry",
    "-webkit-backdrop-filter",
    "-webkit-mask",
    "-webkit-mask-image",
    "-webkit-mask-size",
    "-webkit-mask-position",
    "-webkit-mask-repeat",
    "-webkit-background-clip",
    "-webkit-filter",
    "-webkit-app-region",
    "-webkit-box-orient",
    "text-decoration-thickness",
    "-stencil-caret-animation",
    "-stencil-shape",
    "-stencil-progress",
    "-stencil-time",
    "-stencil-text-effect",
    "-stencil-text-stagger",
    "-stencil-text-offset",
    "-stencil-text-scale",
    "-stencil-text-blur",
    "-stencil-text-duration",
    "-stencil-snapshot-transition",
    "-stencil-snapshot-transition-duration",
    "-stencil-snapshot-transition-target",
    "-stencil-background-extension",
    "-stencil-particles",
    "-stencil-follow",
    "-stencil-follow-target",
    "-stencil-squash",
    "-stencil-text-snap",
    "font-feature-settings",
    "text-rendering",
    "-moz-osx-font-smoothing",
    "-webkit-tap-highlight-color",
    "contain",
    "content-visibility",
];

/// Properties dropped without an error (book:10112-10120).
const SILENTLY_DROPPED: &[&str] = &[
    "-moz-osx-font-smoothing",
    "text-rendering",
    "-webkit-tap-highlight-color",
    "-webkit-box-orient",
];

/// A specified value.
#[derive(Clone, Debug, PartialEq)]
pub enum SV {
    Inherit,
    Initial,
    Unset,
    Kw(&'static str),
    Len(Expr),
    Num(f32),
    Int(i32),
    Color(SColor),
    Str(Arc<str>),
    Tracks(Arc<TrackListSpec>),
    TrackSizes(Arc<Vec<TrackSizeSpec>>),
    Areas(Arc<(Vec<GridArea>, u16, u16)>),
    Line(GridLine),
    AutoFlow(GridAutoFlow),
    Transform(Arc<Vec<TransformSpec>>),
    Origin(Expr, Expr),
    Spacing(Expr, Expr),
    Radius(Expr, Expr),
    Times(Arc<Vec<f32>>),
    Easings(Arc<Vec<Easing>>),
    Idents(Arc<Vec<Arc<str>>>),
    Content(Arc<Vec<ContentItem>>),
    Decoration(TextDecoration),
    /// `box-shadow`: an empty list is `none`.
    Shadows(Arc<Vec<ShadowSpec>>),
    /// A value containing `var()`, parsed after substitution. `shorthand` names the shorthand it
    /// came from, if any.
    Unparsed {
        css: Arc<str>,
        shorthand: Option<&'static str>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum TrackBreadthSpec {
    Length(Expr),
    Fr(f32),
    Auto,
    MinContent,
    MaxContent,
    FitContent(Expr),
}

#[derive(Clone, Debug, PartialEq)]
pub struct TrackSizeSpec {
    pub min: TrackBreadthSpec,
    pub max: TrackBreadthSpec,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TrackItemSpec {
    Track(TrackSizeSpec, Vec<Arc<str>>),
    Repeat(RepeatCount, Vec<TrackSizeSpec>, Vec<Vec<Arc<str>>>),
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct TrackListSpec {
    pub items: Vec<TrackItemSpec>,
    pub trailing_names: Vec<Arc<str>>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TransformSpec {
    Translate(Expr, Expr),
    Scale(f32, f32),
    Rotate(f32),
    Skew(f32, f32),
    Matrix([f32; 6]),
}

/// One specified `box-shadow`: offsets, blur and spread lengths, and its color (`None` is
/// `currentColor`).
#[derive(Clone, Debug, PartialEq)]
pub struct ShadowSpec {
    pub inset: bool,
    pub x: Expr,
    pub y: Expr,
    pub blur: Expr,
    pub spread: Expr,
    pub color: Option<SColor>,
}

/// Why a declaration was dropped.
#[derive(Clone, Debug, PartialEq)]
pub enum DeclError {
    UnknownProperty,
    InvalidValue,
    UnsupportedUnit(String),
    Url,
    PositionFixed,
    PositionVar,
    /// Dropped without a report.
    Silent,
}

impl DeclError {
    pub fn message(&self, name: &str) -> String {
        match self {
            DeclError::UnknownProperty => format!("unknown property `{name}`"),
            DeclError::InvalidValue => format!("invalid value for `{name}`"),
            DeclError::UnsupportedUnit(u) if u.is_empty() => "unsupported length unit".to_string(),
            DeclError::UnsupportedUnit(u) => format!("unsupported length unit `{u}`"),
            DeclError::Url => "url() is not allowed".to_string(),
            DeclError::PositionFixed => "position: fixed is not allowed".to_string(),
            DeclError::PositionVar => "position: var() is not allowed".to_string(),
            DeclError::Silent => String::new(),
        }
    }
}

/// Rules that only apply to program sheets (§7.12).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Restrictions {
    pub no_url: bool,
    pub no_fixed: bool,
    pub tern_units: bool,
}

impl Restrictions {
    pub const NONE: Restrictions = Restrictions {
        no_url: false,
        no_fixed: false,
        tern_units: false,
    };
    pub const PROGRAM: Restrictions = Restrictions {
        no_url: true,
        no_fixed: true,
        tern_units: true,
    };
    pub const PLUGIN: Restrictions = Restrictions {
        no_url: false,
        no_fixed: false,
        tern_units: true,
    };
}

/// Grid properties Tern's engine lacks (program and plugin sheets get "unknown property").
const TERN_MISSING_GRID: [&str; 9] = [
    "grid-template-areas",
    "grid-area",
    "grid-auto-rows",
    "grid-auto-columns",
    "grid-auto-flow",
    "grid-row-start",
    "grid-row-end",
    "grid-column-start",
    "grid-column-end",
];

fn contains_ci(hay: &str, needle: &str) -> bool {
    hay.to_ascii_lowercase().contains(needle)
}

/// Parse one declaration `name: value` into longhands.
pub fn parse_declaration(
    name: &str,
    value: &str,
    r: Restrictions,
) -> Result<Vec<(Prop, SV)>, DeclError> {
    let lname = name.to_ascii_lowercase();
    let name = lname.as_str();
    if r.no_url && contains_ci(value, "url(") {
        return Err(DeclError::Url);
    }
    if name.starts_with("--") {
        return Err(DeclError::UnknownProperty);
    }
    // Vendor aliases.
    let name = match name {
        "word-wrap" => "overflow-wrap",
        "grid-gap" => "gap",
        "grid-row-gap" => "row-gap",
        "grid-column-gap" => "column-gap",
        "-webkit-transform" => "transform",
        "-webkit-user-select" => "user-select",
        "-webkit-line-clamp" => "line-clamp",
        n => n,
    };
    if name == "position" && r.no_fixed {
        let v = value.trim().to_ascii_lowercase();
        if v == "fixed" {
            return Err(DeclError::PositionFixed);
        }
        if v.contains("var(") {
            return Err(DeclError::PositionVar);
        }
    }
    if SILENTLY_DROPPED.contains(&name) {
        return Err(DeclError::Silent);
    }
    // Grid areas, auto tracks and the line longhands are missing from Tern's engine, and using
    // them logs "unknown property" (book:10367, 10796). Our own sheets keep them.
    if r.tern_units && TERN_MISSING_GRID.contains(&name) {
        return Err(DeclError::UnknownProperty);
    }
    if ACCEPTED_UNUSED.contains(&name) {
        return Ok(Vec::new());
    }
    let longhands = shorthand_longhands(name);
    let single = Prop::from_name(name);
    if longhands.is_none() && single.is_none() {
        return Err(DeclError::UnknownProperty);
    }
    let trimmed = value.trim();
    // CSS-wide keywords.
    let wide = match trimmed.to_ascii_lowercase().as_str() {
        "inherit" => Some(SV::Inherit),
        "initial" => Some(SV::Initial),
        "unset" => Some(SV::Unset),
        "revert" | "revert-layer" => return Err(DeclError::InvalidValue),
        _ => None,
    };
    if let Some(w) = wide {
        return Ok(match (single, longhands) {
            (Some(p), _) => vec![(p, w)],
            (None, Some(ls)) => ls.iter().map(|p| (*p, w.clone())).collect(),
            _ => unreachable!(),
        });
    }
    if contains_ci(trimmed, "var(") {
        let css: Arc<str> = Arc::from(trimmed);
        return Ok(match (single, longhands) {
            (Some(p), _) => vec![(
                p,
                SV::Unparsed {
                    css,
                    shorthand: None,
                },
            )],
            (None, Some(ls)) => {
                let sh = static_name(name);
                ls.iter()
                    .map(|p| {
                        (
                            *p,
                            SV::Unparsed {
                                css: css.clone(),
                                shorthand: sh,
                            },
                        )
                    })
                    .collect()
            }
            _ => unreachable!(),
        });
    }
    let mode = if r.tern_units {
        UnitMode::Tern
    } else {
        UnitMode::Full
    };
    with_unit_mode(mode, || parse_value_str(name, single, trimmed))
}

fn static_name(name: &str) -> Option<&'static str> {
    SHORTHANDS.iter().find(|(n, _)| *n == name).map(|(n, _)| *n)
}

fn parse_value_str(
    name: &str,
    single: Option<Prop>,
    value: &str,
) -> Result<Vec<(Prop, SV)>, DeclError> {
    let mut p = Parser::new(value);
    let result = p.parse_entirely(|p| -> PResult<Vec<(Prop, SV)>> {
        match single {
            Some(prop) => Ok(vec![(prop, parse_longhand(prop, p)?)]),
            None => parse_shorthand(name, p),
        }
    });
    result.map_err(|e| match e.kind {
        cssparser::ParseErrorKind::Custom(ValueError::UnsupportedUnit(u)) if !u.is_empty() => {
            DeclError::UnsupportedUnit(u)
        }
        cssparser::ParseErrorKind::Custom(ValueError::Url) => DeclError::Url,
        _ => DeclError::InvalidValue,
    })
}

/// Parse a longhand after var() substitution.
pub fn parse_substituted(prop: Prop, shorthand: Option<&str>, css: &str) -> Option<SV> {
    let list = parse_value_str(
        shorthand.unwrap_or(prop.name()),
        if shorthand.is_some() {
            None
        } else {
            Some(prop)
        },
        css,
    )
    .ok()?;
    list.into_iter().find(|(p, _)| *p == prop).map(|(_, v)| v)
}

const SHORTHANDS: &[(&str, &[Prop])] = &[
    (
        "margin",
        &[
            Prop::MarginTop,
            Prop::MarginRight,
            Prop::MarginBottom,
            Prop::MarginLeft,
        ],
    ),
    (
        "padding",
        &[
            Prop::PaddingTop,
            Prop::PaddingRight,
            Prop::PaddingBottom,
            Prop::PaddingLeft,
        ],
    ),
    ("inset", &[Prop::Top, Prop::Right, Prop::Bottom, Prop::Left]),
    (
        "border-width",
        &[
            Prop::BorderTopWidth,
            Prop::BorderRightWidth,
            Prop::BorderBottomWidth,
            Prop::BorderLeftWidth,
        ],
    ),
    (
        "border-style",
        &[
            Prop::BorderTopStyle,
            Prop::BorderRightStyle,
            Prop::BorderBottomStyle,
            Prop::BorderLeftStyle,
        ],
    ),
    (
        "border-color",
        &[
            Prop::BorderTopColor,
            Prop::BorderRightColor,
            Prop::BorderBottomColor,
            Prop::BorderLeftColor,
        ],
    ),
    (
        "border-top",
        &[
            Prop::BorderTopWidth,
            Prop::BorderTopStyle,
            Prop::BorderTopColor,
        ],
    ),
    (
        "border-right",
        &[
            Prop::BorderRightWidth,
            Prop::BorderRightStyle,
            Prop::BorderRightColor,
        ],
    ),
    (
        "border-bottom",
        &[
            Prop::BorderBottomWidth,
            Prop::BorderBottomStyle,
            Prop::BorderBottomColor,
        ],
    ),
    (
        "border-left",
        &[
            Prop::BorderLeftWidth,
            Prop::BorderLeftStyle,
            Prop::BorderLeftColor,
        ],
    ),
    (
        "border",
        &[
            Prop::BorderTopWidth,
            Prop::BorderRightWidth,
            Prop::BorderBottomWidth,
            Prop::BorderLeftWidth,
            Prop::BorderTopStyle,
            Prop::BorderRightStyle,
            Prop::BorderBottomStyle,
            Prop::BorderLeftStyle,
            Prop::BorderTopColor,
            Prop::BorderRightColor,
            Prop::BorderBottomColor,
            Prop::BorderLeftColor,
        ],
    ),
    (
        "border-radius",
        &[
            Prop::BorderTopLeftRadius,
            Prop::BorderTopRightRadius,
            Prop::BorderBottomRightRadius,
            Prop::BorderBottomLeftRadius,
        ],
    ),
    ("flex", &[Prop::FlexGrow, Prop::FlexShrink, Prop::FlexBasis]),
    ("flex-flow", &[Prop::FlexDirection, Prop::FlexWrap]),
    ("gap", &[Prop::RowGap, Prop::ColumnGap]),
    ("place-items", &[Prop::AlignItems, Prop::JustifyItems]),
    ("place-content", &[Prop::AlignContent, Prop::JustifyContent]),
    ("place-self", &[Prop::AlignSelf, Prop::JustifySelf]),
    ("overflow", &[Prop::OverflowX, Prop::OverflowY]),
    (
        "grid-template",
        &[
            Prop::GridTemplateRows,
            Prop::GridTemplateColumns,
            Prop::GridTemplateAreas,
        ],
    ),
    ("grid-row", &[Prop::GridRowStart, Prop::GridRowEnd]),
    ("grid-column", &[Prop::GridColumnStart, Prop::GridColumnEnd]),
    (
        "grid-area",
        &[
            Prop::GridRowStart,
            Prop::GridColumnStart,
            Prop::GridRowEnd,
            Prop::GridColumnEnd,
        ],
    ),
    (
        "font",
        &[
            Prop::FontStyle,
            Prop::FontWeight,
            Prop::FontSize,
            Prop::LineHeight,
            Prop::FontFamily,
        ],
    ),
    (
        "background",
        &[Prop::BackgroundColor, Prop::BackgroundImage],
    ),
    (
        "text-decoration",
        &[
            Prop::TextDecorationLine,
            Prop::TextDecorationStyle,
            Prop::TextDecorationColor,
        ],
    ),
    (
        "list-style",
        &[Prop::ListStyleType, Prop::ListStylePosition],
    ),
    (
        "outline",
        &[Prop::OutlineWidth, Prop::OutlineStyle, Prop::OutlineColor],
    ),
    (
        "transition",
        &[
            Prop::TransitionProperty,
            Prop::TransitionDuration,
            Prop::TransitionTimingFunction,
            Prop::TransitionDelay,
        ],
    ),
    (
        "animation",
        &[
            Prop::AnimationName,
            Prop::AnimationDuration,
            Prop::AnimationTimingFunction,
            Prop::AnimationDelay,
            Prop::AnimationIterationCount,
            Prop::AnimationDirection,
            Prop::AnimationFillMode,
            Prop::AnimationPlayState,
        ],
    ),
];

pub fn shorthand_longhands(name: &str) -> Option<&'static [Prop]> {
    SHORTHANDS.iter().find(|(n, _)| *n == name).map(|(_, l)| *l)
}

fn kw<'i>(p: &mut Parser<'i>, list: &[&'static str]) -> PResult<&'static str> {
    let ident = p.expect_ident()?.clone();
    for k in list {
        if ident.eq_ignore_ascii_case(k) {
            return Ok(k);
        }
    }
    Err(ParseError::unexpected_token())
}

fn sv_kw<'i>(p: &mut Parser<'i>, list: &[&'static str]) -> PResult<SV> {
    kw(p, list).map(SV::Kw)
}

fn len_or_kw<'i>(p: &mut Parser<'i>, list: &[&'static str], nonneg: bool) -> PResult<SV> {
    if let Ok(k) = p.try_parse(|p| kw(p, list)) {
        return Ok(SV::Kw(k));
    }
    if nonneg {
        parse_length_pct_nonneg(p).map(SV::Len)
    } else {
        parse_length_pct(p).map(SV::Len)
    }
}

fn number<'i>(p: &mut Parser<'i>) -> PResult<f32> {
    p.expect_number().map_err(Into::into)
}

const ALIGN_KWS: &[&str] = &[
    "normal",
    "auto",
    "start",
    "end",
    "flex-start",
    "flex-end",
    "center",
    "baseline",
    "stretch",
    "space-between",
    "space-around",
    "space-evenly",
    "left",
    "right",
    "self-start",
    "self-end",
];

fn parse_align<'i>(p: &mut Parser<'i>) -> PResult<SV> {
    // `safe`/`unsafe` and `first`/`last` prefixes.
    let _ = p.try_parse(|p| kw(p, &["safe", "unsafe"]));
    if p.try_parse(|p| kw(p, &["last"])).is_ok() {
        p.expect_ident_matching("baseline")?;
        return Ok(SV::Kw("last-baseline"));
    }
    let _ = p.try_parse(|p| kw(p, &["first"]));
    if let Ok(k) = p.try_parse(|p| kw(p, &["legacy"])) {
        let _ = p.try_parse(|p| kw(p, &["left", "right", "center"]));
        return Ok(SV::Kw(k));
    }
    sv_kw(p, ALIGN_KWS)
}

fn parse_border_style<'i>(p: &mut Parser<'i>) -> PResult<SV> {
    sv_kw(
        p,
        &[
            "none", "hidden", "solid", "dashed", "dotted", "double", "groove", "ridge", "inset",
            "outset", "auto",
        ],
    )
}

fn parse_border_width<'i>(p: &mut Parser<'i>) -> PResult<SV> {
    if let Ok(k) = p.try_parse(|p| kw(p, &["thin", "medium", "thick"])) {
        return Ok(SV::Len(Expr::Px(match k {
            "thin" => 1.0,
            "medium" => 3.0,
            _ => 5.0,
        })));
    }
    let e = parse_length_pct_nonneg(p)?;
    if e.has_pct() {
        return Err(ParseError::custom(ValueError::UnsupportedUnit(
            String::new(),
        )));
    }
    Ok(SV::Len(e))
}

fn parse_size<'i>(p: &mut Parser<'i>, max: bool) -> PResult<SV> {
    let list: &[&'static str] = if max {
        &[
            "none",
            "min-content",
            "max-content",
            "fit-content",
            "-webkit-fill-available",
            "stretch",
        ]
    } else {
        &[
            "auto",
            "min-content",
            "max-content",
            "fit-content",
            "content",
            "-webkit-fill-available",
            "stretch",
        ]
    };
    len_or_kw(p, list, true)
}

fn parse_track_breadth<'i>(p: &mut Parser<'i>) -> PResult<TrackBreadthSpec> {
    if let Ok(k) = p.try_parse(|p| kw(p, &["auto", "min-content", "max-content"])) {
        return Ok(match k {
            "auto" => TrackBreadthSpec::Auto,
            "min-content" => TrackBreadthSpec::MinContent,
            _ => TrackBreadthSpec::MaxContent,
        });
    }
    if let Ok(fr) = p.try_parse(|p| -> PResult<f32> {
        match p.next()?.clone() {
            Token::Dimension {
                value, ref unit, ..
            } if unit.eq_ignore_ascii_case("fr") && value >= 0.0 => Ok(value),
            _ => Err(ParseError::unexpected_token()),
        }
    }) {
        return Ok(TrackBreadthSpec::Fr(fr));
    }
    parse_length_pct_nonneg(p).map(TrackBreadthSpec::Length)
}

fn parse_track_size<'i>(p: &mut Parser<'i>) -> PResult<TrackSizeSpec> {
    if p.try_parse(|p| p.expect_function_matching("minmax"))
        .is_ok()
    {
        return p.parse_nested_block(|p| {
            let min = parse_track_breadth(p)?;
            p.expect_comma()?;
            let max = parse_track_breadth(p)?;
            Ok(TrackSizeSpec { min, max })
        });
    }
    if p.try_parse(|p| p.expect_function_matching("fit-content"))
        .is_ok()
    {
        let l = p.parse_nested_block(parse_length_pct_nonneg)?;
        return Ok(TrackSizeSpec {
            min: TrackBreadthSpec::Auto,
            max: TrackBreadthSpec::FitContent(l),
        });
    }
    let b = parse_track_breadth(p)?;
    Ok(match b {
        TrackBreadthSpec::Fr(_) => TrackSizeSpec {
            min: TrackBreadthSpec::Auto,
            max: b,
        },
        b => TrackSizeSpec {
            min: b.clone(),
            max: b,
        },
    })
}

fn parse_line_names<'i>(p: &mut Parser<'i>) -> PResult<Vec<Arc<str>>> {
    p.expect_square_bracket_block()?;
    p.parse_nested_block(|p| {
        let mut names = Vec::new();
        while let Ok(id) = p.try_parse(|p| p.expect_ident_cloned()) {
            names.push(Arc::from(id.as_ref()));
        }
        Ok(names)
    })
}

fn parse_track_list<'i>(p: &mut Parser<'i>) -> PResult<TrackListSpec> {
    let mut list = TrackListSpec::default();
    let mut pending = Vec::new();
    loop {
        if let Ok(names) = p.try_parse(parse_line_names) {
            pending.extend(names);
            continue;
        }
        if p.try_parse(|p| p.expect_function_matching("repeat"))
            .is_ok()
        {
            let mut item = p.parse_nested_block(|p| {
                let count = match p.next()?.clone() {
                    Token::Number {
                        int_value: Some(n), ..
                    } if n > 0 => RepeatCount::Count(n.min(10000) as u16),
                    // Tern repeats a whole number of times only: no `repeat(auto-fill | auto-fit,
                    // …)` in program and plugin sheets (book:10367, 10797).
                    Token::Ident(ref s) if s.eq_ignore_ascii_case("auto-fill") && !tern_rules() => {
                        RepeatCount::AutoFill
                    }
                    Token::Ident(ref s) if s.eq_ignore_ascii_case("auto-fit") && !tern_rules() => {
                        RepeatCount::AutoFit
                    }
                    _ => return Err(ParseError::unexpected_token()),
                };
                p.expect_comma()?;
                let mut tracks = Vec::new();
                let mut names = Vec::new();
                let mut cur = Vec::new();
                loop {
                    if let Ok(n) = p.try_parse(parse_line_names) {
                        cur.extend(n);
                        continue;
                    }
                    match p.try_parse(parse_track_size) {
                        Ok(t) => {
                            tracks.push(t);
                            names.push(std::mem::take(&mut cur));
                        }
                        Err(_) => break,
                    }
                }
                names.push(cur);
                if tracks.is_empty() {
                    return Err(ParseError::custom(ValueError::UnsupportedUnit(
                        String::new(),
                    )));
                }
                Ok(TrackItemSpec::Repeat(count, tracks, names))
            })?;
            if !pending.is_empty()
                && let TrackItemSpec::Repeat(_, _, ref mut n) = item
            {
                let mut v = std::mem::take(&mut pending);
                v.append(&mut n[0]);
                n[0] = v;
            }
            list.items.push(item);
            continue;
        }
        match p.try_parse(parse_track_size) {
            Ok(t) => list
                .items
                .push(TrackItemSpec::Track(t, std::mem::take(&mut pending))),
            Err(_) => break,
        }
    }
    list.trailing_names = pending;
    if list.items.is_empty() {
        return Err(ParseError::custom(ValueError::UnsupportedUnit(
            String::new(),
        )));
    }
    Ok(list)
}

fn parse_areas<'i>(p: &mut Parser<'i>) -> PResult<SV> {
    if p.try_parse(|p| p.expect_ident_matching("none")).is_ok() {
        return Ok(SV::Kw("none"));
    }
    let mut rows: Vec<Vec<String>> = Vec::new();
    while let Ok(s) = p.try_parse(|p| p.expect_string_cloned()) {
        let cells: Vec<String> = s
            .split_whitespace()
            .flat_map(|tok| {
                // A run of dots is one null cell token.
                if tok.chars().all(|c| c == '.') {
                    vec![".".to_string()]
                } else {
                    vec![tok.to_string()]
                }
            })
            .collect();
        rows.push(cells);
    }
    if rows.is_empty() {
        return Err(ParseError::custom(ValueError::UnsupportedUnit(
            String::new(),
        )));
    }
    let cols = rows[0].len();
    if cols == 0 || rows.iter().any(|r| r.len() != cols) {
        return Err(ParseError::custom(ValueError::UnsupportedUnit(
            String::new(),
        )));
    }
    let mut areas: Vec<GridArea> = Vec::new();
    for (ri, row) in rows.iter().enumerate() {
        for (ci, name) in row.iter().enumerate() {
            if name == "." {
                continue;
            }
            match areas.iter_mut().find(|a| &*a.name == name.as_str()) {
                Some(a) => {
                    a.row_start = a.row_start.min(ri as u16 + 1);
                    a.row_end = a.row_end.max(ri as u16 + 2);
                    a.column_start = a.column_start.min(ci as u16 + 1);
                    a.column_end = a.column_end.max(ci as u16 + 2);
                }
                None => areas.push(GridArea {
                    name: Arc::from(name.as_str()),
                    row_start: ri as u16 + 1,
                    row_end: ri as u16 + 2,
                    column_start: ci as u16 + 1,
                    column_end: ci as u16 + 2,
                }),
            }
        }
    }
    Ok(SV::Areas(Arc::new((areas, rows.len() as u16, cols as u16))))
}

fn parse_grid_line<'i>(p: &mut Parser<'i>) -> PResult<GridLine> {
    if p.try_parse(|p| p.expect_ident_matching("auto")).is_ok() {
        return Ok(GridLine::Auto);
    }
    let mut span = false;
    let mut num: Option<i32> = None;
    let mut name: Option<Arc<str>> = None;
    for _ in 0..3 {
        if !span && p.try_parse(|p| p.expect_ident_matching("span")).is_ok() {
            span = true;
            continue;
        }
        if num.is_none()
            && let Ok(n) = p.try_parse(|p| p.expect_integer())
        {
            num = Some(n);
            continue;
        }
        if name.is_none()
            && let Ok(id) = p.try_parse(|p| p.expect_ident_cloned())
        {
            name = Some(Arc::from(id.as_ref()));
            continue;
        }
        break;
    }
    match (span, num, name) {
        (true, n, name) => {
            let n = n.unwrap_or(1);
            if n <= 0 {
                return Err(ParseError::custom(ValueError::UnsupportedUnit(
                    String::new(),
                )));
            }
            Ok(GridLine::Span(n as u16, name))
        }
        (false, Some(n), name) if n != 0 => Ok(GridLine::Line(n as i16, name)),
        (false, None, Some(name)) => Ok(GridLine::Named(name)),
        _ => Err(ParseError::custom(ValueError::UnsupportedUnit(
            String::new(),
        ))),
    }
}

/// `box-shadow`: `none`, or a comma list of `[inset]? <x> <y> [<blur> [<spread>]]? [<color>]?` in
/// any order (CSS Backgrounds 3 §7.1). Lengths only; a negative blur is invalid.
fn parse_box_shadow<'i>(p: &mut Parser<'i>) -> PResult<SV> {
    if p.try_parse(|p| p.expect_ident_matching("none")).is_ok() {
        return Ok(SV::Shadows(Arc::new(Vec::new())));
    }
    let length = |p: &mut Parser<'i>| -> PResult<Expr> {
        let e = parse_length_pct(p)?;
        if e.has_pct() {
            return Err(ParseError::custom(ValueError::UnsupportedUnit(
                String::new(),
            )));
        }
        Ok(e)
    };
    let shadow = |p: &mut Parser<'i>| -> PResult<ShadowSpec> {
        let mut inset = false;
        let mut color = None;
        let mut lengths: Option<[Expr; 4]> = None;
        loop {
            if !inset && p.try_parse(|p| p.expect_ident_matching("inset")).is_ok() {
                inset = true;
                continue;
            }
            if lengths.is_none()
                && let Ok(x) = p.try_parse(length)
            {
                let y = length(p)?;
                let blur = match p.try_parse(length) {
                    Ok(b) => {
                        let negative = matches!(
                            b,
                            Expr::Px(v) | Expr::Em(v) | Expr::Rem(v) | Expr::Vw(v) | Expr::Vh(v)
                                if v < 0.0
                        );
                        if negative {
                            return Err(ParseError::custom(ValueError::UnsupportedUnit(
                                String::new(),
                            )));
                        }
                        b
                    }
                    Err(_) => Expr::Px(0.0),
                };
                let spread = p.try_parse(length).unwrap_or(Expr::Px(0.0));
                lengths = Some([x, y, blur, spread]);
                continue;
            }
            if color.is_none()
                && let Ok(c) = p.try_parse(parse_color)
            {
                color = Some(c);
                continue;
            }
            break;
        }
        let Some([x, y, blur, spread]) = lengths else {
            return Err(ParseError::custom(ValueError::UnsupportedUnit(
                String::new(),
            )));
        };
        Ok(ShadowSpec {
            inset,
            x,
            y,
            blur,
            spread,
            color,
        })
    };
    let list = p.parse_comma_separated(shadow)?;
    Ok(SV::Shadows(Arc::new(list)))
}

fn parse_transform<'i>(p: &mut Parser<'i>) -> PResult<SV> {
    if p.try_parse(|p| p.expect_ident_matching("none")).is_ok() {
        return Ok(SV::Kw("none"));
    }
    let mut fns = Vec::new();
    loop {
        let name = match p.try_parse(|p| p.expect_function().cloned()) {
            Ok(n) => n.to_ascii_lowercase(),
            Err(_) => break,
        };
        let f = p.parse_nested_block(|p| -> PResult<TransformSpec> {
            let lens = |p: &mut Parser<'i>| p.parse_comma_separated(parse_length_pct);
            let nums = |p: &mut Parser<'i>| p.parse_comma_separated(|p| number(p));
            Ok(match name.as_str() {
                "translate" | "translate3d" => {
                    let v = lens(p)?;
                    TransformSpec::Translate(
                        v[0].clone(),
                        v.get(1).cloned().unwrap_or(Expr::Px(0.0)),
                    )
                }
                "translatex" => TransformSpec::Translate(parse_length_pct(p)?, Expr::Px(0.0)),
                "translatey" => TransformSpec::Translate(Expr::Px(0.0), parse_length_pct(p)?),
                "translatez" => {
                    parse_length_pct(p)?;
                    TransformSpec::Translate(Expr::Px(0.0), Expr::Px(0.0))
                }
                "scale" | "scale3d" => {
                    let v = nums(p)?;
                    TransformSpec::Scale(v[0], v.get(1).copied().unwrap_or(v[0]))
                }
                "scalex" => TransformSpec::Scale(number(p)?, 1.0),
                "scaley" => TransformSpec::Scale(1.0, number(p)?),
                "scalez" => {
                    number(p)?;
                    TransformSpec::Scale(1.0, 1.0)
                }
                "rotate" | "rotatez" => TransformSpec::Rotate(parse_angle(p)?),
                "rotatex" | "rotatey" => {
                    parse_angle(p)?;
                    TransformSpec::Rotate(0.0)
                }
                "rotate3d" => {
                    let _ = number(p)?;
                    p.expect_comma()?;
                    let _ = number(p)?;
                    p.expect_comma()?;
                    let z = number(p)?;
                    p.expect_comma()?;
                    let a = parse_angle(p)?;
                    TransformSpec::Rotate(if z != 0.0 { a } else { 0.0 })
                }
                "skew" => {
                    let a = parse_angle(p)?;
                    let b = if p.try_parse(|p| p.expect_comma()).is_ok() {
                        parse_angle(p)?
                    } else {
                        0.0
                    };
                    TransformSpec::Skew(a, b)
                }
                "skewx" => TransformSpec::Skew(parse_angle(p)?, 0.0),
                "skewy" => TransformSpec::Skew(0.0, parse_angle(p)?),
                "matrix" => {
                    let v = nums(p)?;
                    if v.len() != 6 {
                        return Err(ParseError::custom(ValueError::UnsupportedUnit(
                            String::new(),
                        )));
                    }
                    TransformSpec::Matrix([v[0], v[1], v[2], v[3], v[4], v[5]])
                }
                "perspective" => {
                    let _ = parse_length_pct(p)?;
                    TransformSpec::Scale(1.0, 1.0)
                }
                "matrix3d" => {
                    let v = nums(p)?;
                    if v.len() != 16 {
                        return Err(ParseError::custom(ValueError::UnsupportedUnit(
                            String::new(),
                        )));
                    }
                    TransformSpec::Matrix([v[0], v[1], v[4], v[5], v[12], v[13]])
                }
                _ => {
                    return Err(ParseError::custom(ValueError::UnsupportedUnit(
                        String::new(),
                    )));
                }
            })
        })?;
        fns.push(f);
    }
    if fns.is_empty() {
        return Err(ParseError::custom(ValueError::UnsupportedUnit(
            String::new(),
        )));
    }
    Ok(SV::Transform(Arc::new(fns)))
}

fn parse_position_component<'i>(p: &mut Parser<'i>) -> PResult<Expr> {
    if let Ok(k) = p.try_parse(|p| kw(p, &["left", "center", "right", "top", "bottom"])) {
        return Ok(Expr::Pct(match k {
            "left" | "top" => 0.0,
            "center" => 50.0,
            _ => 100.0,
        }));
    }
    parse_length_pct(p)
}

fn parse_content<'i>(p: &mut Parser<'i>) -> PResult<SV> {
    if let Ok(k) = p.try_parse(|p| kw(p, &["none", "normal"])) {
        return Ok(SV::Kw(k));
    }
    let mut items = Vec::new();
    loop {
        if let Ok(s) = p.try_parse(|p| p.expect_string_cloned()) {
            items.push(ContentItem::Text(Arc::from(s.as_ref())));
            continue;
        }
        if p.try_parse(|p| p.expect_function_matching("attr")).is_ok() {
            let name = p.parse_nested_block(|p| Ok(p.expect_ident_cloned()?))?;
            items.push(ContentItem::Attr(Arc::from(
                name.to_ascii_lowercase().as_str(),
            )));
            continue;
        }
        break;
    }
    if items.is_empty() {
        return Err(ParseError::custom(ValueError::UnsupportedUnit(
            String::new(),
        )));
    }
    Ok(SV::Content(Arc::new(items)))
}

fn parse_font_family<'i>(p: &mut Parser<'i>) -> PResult<SV> {
    let families = p.parse_comma_separated(|p| -> PResult<String> {
        if let Ok(s) = p.try_parse(|p| p.expect_string_cloned()) {
            return Ok(format!("\"{}\"", s.replace('"', "")));
        }
        let mut words = vec![p.expect_ident_cloned()?.to_string()];
        while let Ok(w) = p.try_parse(|p| p.expect_ident_cloned()) {
            words.push(w.to_string());
        }
        let joined = words.join(" ");
        Ok(if words.len() > 1 {
            format!("\"{joined}\"")
        } else {
            joined
        })
    })?;
    Ok(SV::Str(Arc::from(families.join(", ").as_str())))
}

fn parse_font_weight<'i>(p: &mut Parser<'i>) -> PResult<SV> {
    if let Ok(k) = p.try_parse(|p| kw(p, &["normal", "bold", "bolder", "lighter"])) {
        return Ok(SV::Kw(k));
    }
    let n = number(p)?;
    if !(1.0..=1000.0).contains(&n) {
        return Err(ParseError::custom(ValueError::UnsupportedUnit(
            String::new(),
        )));
    }
    Ok(SV::Num(n))
}

fn parse_font_size<'i>(p: &mut Parser<'i>) -> PResult<SV> {
    len_or_kw(
        p,
        &[
            "xx-small",
            "x-small",
            "small",
            "medium",
            "large",
            "x-large",
            "xx-large",
            "xxx-large",
            "smaller",
            "larger",
        ],
        true,
    )
}

fn parse_line_height<'i>(p: &mut Parser<'i>) -> PResult<SV> {
    if let Ok(k) = p.try_parse(|p| kw(p, &["normal"])) {
        return Ok(SV::Kw(k));
    }
    if let Ok(n) = p.try_parse(|p| p.expect_number()) {
        return Ok(SV::Num(n));
    }
    parse_length_pct_nonneg(p).map(SV::Len)
}

fn parse_times<'i>(p: &mut Parser<'i>) -> PResult<SV> {
    Ok(SV::Times(Arc::new(p.parse_comma_separated(parse_time)?)))
}

fn parse_easings<'i>(p: &mut Parser<'i>) -> PResult<SV> {
    Ok(SV::Easings(Arc::new(
        p.parse_comma_separated(parse_easing)?,
    )))
}

fn parse_idents<'i>(p: &mut Parser<'i>) -> PResult<SV> {
    let v = p.parse_comma_separated(|p| -> PResult<Arc<str>> {
        if let Ok(s) = p.try_parse(|p| p.expect_string_cloned()) {
            return Ok(Arc::from(s.as_ref()));
        }
        Ok(Arc::from(p.expect_ident_cloned()?.as_ref()))
    })?;
    Ok(SV::Idents(Arc::new(v)))
}

fn parse_iterations<'i>(p: &mut Parser<'i>) -> PResult<SV> {
    let v = p.parse_comma_separated(|p| -> PResult<f32> {
        if p.try_parse(|p| p.expect_ident_matching("infinite")).is_ok() {
            return Ok(f32::INFINITY);
        }
        number(p)
    })?;
    Ok(SV::Times(Arc::new(v)))
}

fn parse_kw_list<'i>(p: &mut Parser<'i>, list: &[&'static str]) -> PResult<SV> {
    let v = p.parse_comma_separated(|p| kw(p, list).map(Arc::<str>::from))?;
    Ok(SV::Idents(Arc::new(v)))
}

fn parse_decoration_line<'i>(p: &mut Parser<'i>) -> PResult<SV> {
    if p.try_parse(|p| p.expect_ident_matching("none")).is_ok() {
        return Ok(SV::Decoration(TextDecoration {
            underline: false,
            overline: false,
            line_through: false,
        }));
    }
    let mut d = TextDecoration {
        underline: false,
        overline: false,
        line_through: false,
    };
    let mut any = false;
    while let Ok(k) = p.try_parse(|p| kw(p, &["underline", "overline", "line-through", "blink"])) {
        any = true;
        match k {
            "underline" => d.underline = true,
            "overline" => d.overline = true,
            "line-through" => d.line_through = true,
            _ => {}
        }
    }
    if !any {
        return Err(ParseError::custom(ValueError::UnsupportedUnit(
            String::new(),
        )));
    }
    Ok(SV::Decoration(d))
}

const DISPLAY_KWS: &[&str] = &[
    "none",
    "contents",
    "block",
    "inline",
    "inline-block",
    "flow-root",
    "list-item",
    "flex",
    "inline-flex",
    "grid",
    "inline-grid",
    "table",
    "inline-table",
    "table-row-group",
    "table-header-group",
    "table-footer-group",
    "table-row",
    "table-cell",
    "table-column",
    "table-column-group",
    "-webkit-box",
    "-webkit-inline-box",
];

const CURSORS: &[&str] = &[
    "auto",
    "default",
    "none",
    "context-menu",
    "help",
    "pointer",
    "progress",
    "wait",
    "cell",
    "crosshair",
    "text",
    "vertical-text",
    "alias",
    "copy",
    "move",
    "no-drop",
    "not-allowed",
    "grab",
    "grabbing",
    "all-scroll",
    "col-resize",
    "row-resize",
    "n-resize",
    "e-resize",
    "s-resize",
    "w-resize",
    "ne-resize",
    "nw-resize",
    "se-resize",
    "sw-resize",
    "ew-resize",
    "ns-resize",
    "nesw-resize",
    "nwse-resize",
    "zoom-in",
    "zoom-out",
];

/// Parse one longhand's value.
pub fn parse_longhand<'i>(prop: Prop, p: &mut Parser<'i>) -> PResult<SV> {
    use Prop::*;
    match prop {
        Zoom => {
            if let Ok(k) = p.try_parse(|p| kw(p, &["normal", "reset"])) {
                return Ok(SV::Kw(k));
            }
            if let Ok(pct) = p.try_parse(|p| p.expect_percentage()) {
                return Ok(SV::Num(pct));
            }
            let n = number(p)?;
            if n < 0.0 {
                return Err(ParseError::custom(ValueError::UnsupportedUnit(
                    String::new(),
                )));
            }
            Ok(SV::Num(n))
        }
        FontSize => parse_font_size(p),
        FontFamily => parse_font_family(p),
        FontWeight => parse_font_weight(p),
        FontStyle => {
            let k = kw(p, &["normal", "italic", "oblique"])?;
            if k == "oblique" {
                let _ = p.try_parse(parse_angle);
            }
            Ok(SV::Kw(k))
        }
        LineHeight => parse_line_height(p),
        ColorScheme => {
            let mut last = "normal";
            while let Ok(k) = p.try_parse(|p| kw(p, &["normal", "light", "dark", "only"])) {
                if k != "only" {
                    last = k;
                }
            }
            Ok(SV::Kw(last))
        }
        Color | BackgroundColor | BorderTopColor | BorderRightColor | BorderBottomColor
        | BorderLeftColor | OutlineColor | TextDecorationColor => parse_color(p).map(SV::Color),
        LetterSpacing => len_or_kw(p, &["normal"], false),
        TextAlign => sv_kw(
            p,
            &[
                "start",
                "end",
                "left",
                "right",
                "center",
                "justify",
                "-webkit-center",
                "match-parent",
            ],
        ),
        WhiteSpace => {
            let k = kw(
                p,
                &[
                    "normal",
                    "pre",
                    "nowrap",
                    "pre-wrap",
                    "pre-line",
                    "break-spaces",
                    "collapse",
                    "preserve",
                ],
            )?;
            // `white-space: collapse nowrap` style pairs.
            let second = p.try_parse(|p| kw(p, &["wrap", "nowrap"])).ok();
            Ok(SV::Kw(match (k, second) {
                ("collapse", Some("nowrap")) => "nowrap",
                ("collapse", _) => "normal",
                ("preserve", Some("nowrap")) => "pre",
                ("preserve", _) => "pre-wrap",
                (k, _) => k,
            }))
        }
        WordBreak => sv_kw(p, &["normal", "break-all", "keep-all", "break-word"]),
        OverflowWrap => sv_kw(p, &["normal", "anywhere", "break-word"]),
        TextTransform => sv_kw(p, &["none", "uppercase", "lowercase", "capitalize"]),
        Visibility => sv_kw(p, &["visible", "hidden", "collapse"]),
        Cursor => sv_kw(p, CURSORS),
        PointerEvents => sv_kw(
            p,
            &[
                "auto",
                "none",
                "all",
                "visiblepainted",
                "fill",
                "stroke",
                "painted",
                "visible",
            ],
        ),
        UserSelect => sv_kw(p, &["auto", "none", "text", "all", "contain"]),
        ListStyleType => {
            if let Ok(s) = p.try_parse(|p| p.expect_string_cloned()) {
                return Ok(SV::Str(Arc::from(s.as_ref())));
            }
            sv_kw(p, &["none", "disc", "circle", "square", "decimal"])
        }
        ListStylePosition => sv_kw(p, &["inside", "outside"]),
        BorderCollapse => sv_kw(p, &["collapse", "separate"]),
        BorderSpacing => {
            let a = parse_length_pct_nonneg(p)?;
            let b = p
                .try_parse(parse_length_pct_nonneg)
                .unwrap_or_else(|_| a.clone());
            Ok(SV::Spacing(a, b))
        }
        Display => {
            let k = kw(p, DISPLAY_KWS)?;
            // Two-keyword forms: `block flow`, `inline flex`, ….
            if let Ok(inner) =
                p.try_parse(|p| kw(p, &["flow", "flow-root", "flex", "grid", "table"]))
            {
                return Ok(SV::Kw(match (k, inner) {
                    ("inline", "flow") => "inline",
                    ("inline", "flow-root") => "inline-block",
                    ("inline", "flex") => "inline-flex",
                    ("inline", "grid") => "inline-grid",
                    ("inline", "table") => "inline-table",
                    ("block", "flow-root") => "flow-root",
                    ("block", "flex") => "flex",
                    ("block", "grid") => "grid",
                    ("block", "table") => "table",
                    _ => "block",
                }));
            }
            Ok(SV::Kw(match k {
                "-webkit-box" => "flex",
                "-webkit-inline-box" => "inline-flex",
                k => k,
            }))
        }
        Position => sv_kw(p, &["static", "relative", "absolute", "fixed", "sticky"]),
        BoxSizing => sv_kw(p, &["content-box", "border-box"]),
        Top | Right | Bottom | Left | MarginTop | MarginRight | MarginBottom | MarginLeft => {
            len_or_kw(p, &["auto"], false)
        }
        PaddingTop | PaddingRight | PaddingBottom | PaddingLeft => {
            parse_length_pct_nonneg(p).map(SV::Len)
        }
        BorderTopStyle | BorderRightStyle | BorderBottomStyle | BorderLeftStyle | OutlineStyle => {
            parse_border_style(p)
        }
        BorderTopWidth | BorderRightWidth | BorderBottomWidth | BorderLeftWidth | OutlineWidth => {
            parse_border_width(p)
        }
        BorderTopLeftRadius
        | BorderTopRightRadius
        | BorderBottomRightRadius
        | BorderBottomLeftRadius => {
            let a = parse_length_pct_nonneg(p)?;
            let b = p
                .try_parse(parse_length_pct_nonneg)
                .unwrap_or_else(|_| a.clone());
            Ok(SV::Radius(a, b))
        }
        Width | Height | MinWidth | MinHeight => parse_size(p, false),
        MaxWidth | MaxHeight => parse_size(p, true),
        AspectRatio => {
            if p.try_parse(|p| p.expect_ident_matching("auto")).is_ok() {
                return Ok(SV::Kw("auto"));
            }
            let a = number(p)?;
            let b = if p.try_parse(|p| p.expect_delim('/')).is_ok() {
                number(p)?
            } else {
                1.0
            };
            Ok(SV::Num(if b > 0.0 { a / b } else { 0.0 }))
        }
        OverflowX | OverflowY => sv_kw(
            p,
            &["visible", "hidden", "clip", "scroll", "auto", "overlay"],
        ),
        FlexDirection => sv_kw(p, &["row", "row-reverse", "column", "column-reverse"]),
        FlexWrap => sv_kw(p, &["nowrap", "wrap", "wrap-reverse"]),
        FlexGrow | FlexShrink => {
            let n = number(p)?;
            if n < 0.0 {
                return Err(ParseError::custom(ValueError::UnsupportedUnit(
                    String::new(),
                )));
            }
            Ok(SV::Num(n))
        }
        FlexBasis => len_or_kw(
            p,
            &[
                "auto",
                "content",
                "min-content",
                "max-content",
                "fit-content",
            ],
            true,
        ),
        Order => Ok(SV::Int(p.expect_integer()?)),
        AlignItems | AlignSelf | AlignContent | JustifyContent | JustifyItems | JustifySelf => {
            parse_align(p)
        }
        RowGap | ColumnGap => len_or_kw(p, &["normal"], true),
        GridTemplateColumns | GridTemplateRows => {
            if p.try_parse(|p| p.expect_ident_matching("none")).is_ok() {
                return Ok(SV::Kw("none"));
            }
            Ok(SV::Tracks(Arc::new(parse_track_list(p)?)))
        }
        GridTemplateAreas => parse_areas(p),
        GridAutoFlow => {
            let mut flow = crate::style::GridAutoFlow {
                column: false,
                dense: false,
            };
            let mut any = false;
            while let Ok(k) = p.try_parse(|p| kw(p, &["row", "column", "dense"])) {
                any = true;
                match k {
                    "column" => flow.column = true,
                    "dense" => flow.dense = true,
                    _ => {}
                }
            }
            if !any {
                return Err(ParseError::custom(ValueError::UnsupportedUnit(
                    String::new(),
                )));
            }
            Ok(SV::AutoFlow(flow))
        }
        GridAutoColumns | GridAutoRows => {
            let mut v = vec![parse_track_size(p)?];
            while let Ok(t) = p.try_parse(parse_track_size) {
                v.push(t);
            }
            Ok(SV::TrackSizes(Arc::new(v)))
        }
        GridRowStart | GridRowEnd | GridColumnStart | GridColumnEnd => {
            parse_grid_line(p).map(SV::Line)
        }
        BackgroundImage => {
            if p.try_parse(|p| p.expect_ident_matching("none")).is_ok() {
                return Ok(SV::Kw("none"));
            }
            // `url()` reaches here only from sheets that may load (plugin and built-in sheets;
            // program sheets drop it earlier), and only `data:` URLs: plugin files are inlined
            // as `data:` when the sheet is installed (§9.8 CSS).
            if let Ok(url) = p.try_parse(|p| p.expect_url().map(|u| u.to_string())) {
                return if url
                    .get(..5)
                    .is_some_and(|s| s.eq_ignore_ascii_case("data:"))
                {
                    Ok(SV::Str(Arc::from(url.as_str())))
                } else {
                    Err(ParseError::custom(ValueError::Url))
                };
            }
            // Gradients are accepted; their arguments aren't drawn yet.
            let name = p.expect_function()?.to_ascii_lowercase();
            if !matches!(
                name.as_str(),
                "linear-gradient"
                    | "repeating-linear-gradient"
                    | "radial-gradient"
                    | "repeating-radial-gradient"
            ) {
                return Err(ParseError::custom(if name == "url" {
                    ValueError::Url
                } else {
                    ValueError::UnsupportedUnit(String::new())
                }));
            }
            p.parse_nested_block(|p| {
                while p.next().is_ok() {}
                Ok(())
            })?;
            while p.try_parse(|p| p.expect_comma()).is_ok() {
                p.expect_function()?;
                p.parse_nested_block(|p| {
                    while p.next().is_ok() {}
                    Ok(())
                })?;
            }
            Ok(SV::Kw("none"))
        }
        Opacity => {
            if let Ok(pct) = p.try_parse(|p| p.expect_percentage()) {
                return Ok(SV::Num(pct.clamp(0.0, 1.0)));
            }
            Ok(SV::Num(number(p)?.clamp(0.0, 1.0)))
        }
        ZIndex => {
            if p.try_parse(|p| p.expect_ident_matching("auto")).is_ok() {
                return Ok(SV::Kw("auto"));
            }
            Ok(SV::Int(p.expect_integer()?))
        }
        VerticalAlign => len_or_kw(
            p,
            &[
                "baseline",
                "sub",
                "super",
                "text-top",
                "text-bottom",
                "middle",
                "top",
                "bottom",
            ],
            false,
        ),
        TextOverflow => sv_kw(p, &["clip", "ellipsis"]),
        TextDecorationLine => parse_decoration_line(p),
        TextDecorationStyle => sv_kw(p, &["solid", "double", "dotted", "dashed", "wavy"]),
        Transform => parse_transform(p),
        TransformOrigin => {
            let a = parse_position_component(p)?;
            let b = p
                .try_parse(parse_position_component)
                .unwrap_or(Expr::Pct(50.0));
            let _ = p.try_parse(parse_length_pct);
            Ok(SV::Origin(a, b))
        }
        OutlineOffset => parse_length_pct(p).map(SV::Len),
        BoxShadow => parse_box_shadow(p),
        ObjectFit => sv_kw(p, &["fill", "contain", "cover", "none", "scale-down"]),
        ImageRendering => sv_kw(
            p,
            &["auto", "smooth", "high-quality", "pixelated", "crisp-edges"],
        ),
        Content => parse_content(p),
        TransitionProperty => {
            if p.try_parse(|p| p.expect_ident_matching("none")).is_ok() {
                return Ok(SV::Idents(Arc::new(Vec::new())));
            }
            parse_idents(p)
        }
        TransitionDuration | TransitionDelay | AnimationDuration | AnimationDelay => parse_times(p),
        TransitionTimingFunction | AnimationTimingFunction => parse_easings(p),
        AnimationName => parse_idents(p),
        AnimationIterationCount => parse_iterations(p),
        AnimationDirection => {
            parse_kw_list(p, &["normal", "reverse", "alternate", "alternate-reverse"])
        }
        AnimationFillMode => parse_kw_list(p, &["none", "forwards", "backwards", "both"]),
        AnimationPlayState => parse_kw_list(p, &["running", "paused"]),
        LineClamp => {
            if p.try_parse(|p| p.expect_ident_matching("none")).is_ok() {
                return Ok(SV::Kw("none"));
            }
            Ok(SV::Int(p.expect_integer()?.max(1)))
        }
    }
}

fn four<'i, F>(p: &mut Parser<'i>, props: &[Prop], mut f: F) -> PResult<Vec<(Prop, SV)>>
where
    F: FnMut(&mut Parser<'i>) -> PResult<SV>,
{
    let mut vals = vec![f(p)?];
    while vals.len() < 4 {
        match p.try_parse(&mut f) {
            Ok(v) => vals.push(v),
            Err(_) => break,
        }
    }
    let (t, r, b, l) = match vals.len() {
        1 => (0, 0, 0, 0),
        2 => (0, 1, 0, 1),
        3 => (0, 1, 2, 1),
        _ => (0, 1, 2, 3),
    };
    Ok(vec![
        (props[0], vals[t].clone()),
        (props[1], vals[r].clone()),
        (props[2], vals[b].clone()),
        (props[3], vals[l].clone()),
    ])
}

fn parse_border_side<'i>(p: &mut Parser<'i>) -> PResult<(SV, SV, SV)> {
    let mut width = None;
    let mut style = None;
    let mut color = None;
    for _ in 0..3 {
        if width.is_none()
            && let Ok(w) = p.try_parse(parse_border_width)
        {
            width = Some(w);
            continue;
        }
        if style.is_none()
            && let Ok(s) = p.try_parse(parse_border_style)
        {
            style = Some(s);
            continue;
        }
        if color.is_none()
            && let Ok(c) = p.try_parse(parse_color)
        {
            color = Some(SV::Color(c));
            continue;
        }
        break;
    }
    if width.is_none() && style.is_none() && color.is_none() {
        return Err(ParseError::custom(ValueError::UnsupportedUnit(
            String::new(),
        )));
    }
    Ok((
        width.unwrap_or(SV::Len(Expr::Px(3.0))),
        style.unwrap_or(SV::Kw("none")),
        color.unwrap_or(SV::Color(SColor::CurrentColor)),
    ))
}

fn parse_shorthand<'i>(name: &str, p: &mut Parser<'i>) -> PResult<Vec<(Prop, SV)>> {
    use Prop::*;
    let props = shorthand_longhands(name).unwrap_or(&[]);
    match name {
        "margin" | "inset" => four(p, props, |p| len_or_kw(p, &["auto"], false)),
        "padding" => four(p, props, |p| parse_length_pct_nonneg(p).map(SV::Len)),
        "border-width" => four(p, props, parse_border_width),
        "border-style" => four(p, props, parse_border_style),
        "border-color" => four(p, props, |p| parse_color(p).map(SV::Color)),
        "border-top" | "border-right" | "border-bottom" | "border-left" | "outline" => {
            let (w, s, c) = parse_border_side(p)?;
            Ok(vec![(props[0], w), (props[1], s), (props[2], c)])
        }
        "border" => {
            let (w, s, c) = parse_border_side(p)?;
            let mut v = Vec::new();
            for i in 0..4 {
                v.push((props[i], w.clone()));
                v.push((props[4 + i], s.clone()));
                v.push((props[8 + i], c.clone()));
            }
            Ok(v)
        }
        "border-radius" => {
            let mut h = vec![parse_length_pct_nonneg(p)?];
            while h.len() < 4 {
                match p.try_parse(parse_length_pct_nonneg) {
                    Ok(v) => h.push(v),
                    Err(_) => break,
                }
            }
            let mut v = h.clone();
            if p.try_parse(|p| p.expect_delim('/')).is_ok() {
                v = vec![parse_length_pct_nonneg(p)?];
                while v.len() < 4 {
                    match p.try_parse(parse_length_pct_nonneg) {
                        Ok(x) => v.push(x),
                        Err(_) => break,
                    }
                }
            }
            let expand = |l: &Vec<Expr>| -> [Expr; 4] {
                match l.len() {
                    1 => [l[0].clone(), l[0].clone(), l[0].clone(), l[0].clone()],
                    2 => [l[0].clone(), l[1].clone(), l[0].clone(), l[1].clone()],
                    3 => [l[0].clone(), l[1].clone(), l[2].clone(), l[1].clone()],
                    _ => [l[0].clone(), l[1].clone(), l[2].clone(), l[3].clone()],
                }
            };
            let (h, v) = (expand(&h), expand(&v));
            Ok((0..4)
                .map(|i| (props[i], SV::Radius(h[i].clone(), v[i].clone())))
                .collect())
        }
        "flex" => {
            if p.try_parse(|p| p.expect_ident_matching("none")).is_ok() {
                return Ok(vec![
                    (FlexGrow, SV::Num(0.0)),
                    (FlexShrink, SV::Num(0.0)),
                    (FlexBasis, SV::Kw("auto")),
                ]);
            }
            if p.try_parse(|p| p.expect_ident_matching("auto")).is_ok() {
                return Ok(vec![
                    (FlexGrow, SV::Num(1.0)),
                    (FlexShrink, SV::Num(1.0)),
                    (FlexBasis, SV::Kw("auto")),
                ]);
            }
            let mut grow = None;
            let mut shrink = None;
            let mut basis = None;
            for _ in 0..3 {
                if grow.is_none()
                    && let Ok(n) = p.try_parse(|p| p.expect_number())
                {
                    grow = Some(n);
                    if let Ok(s) = p.try_parse(|p| p.expect_number()) {
                        shrink = Some(s);
                    }
                    continue;
                }
                if basis.is_none()
                    && let Ok(b) = p.try_parse(|p| {
                        len_or_kw(p, &["auto", "content", "min-content", "max-content"], true)
                    })
                {
                    basis = Some(b);
                    continue;
                }
                break;
            }
            if grow.is_none() && basis.is_none() {
                return Err(ParseError::custom(ValueError::UnsupportedUnit(
                    String::new(),
                )));
            }
            Ok(vec![
                (FlexGrow, SV::Num(grow.unwrap_or(1.0))),
                (FlexShrink, SV::Num(shrink.unwrap_or(1.0))),
                (FlexBasis, basis.unwrap_or(SV::Len(Expr::Pct(0.0)))),
            ])
        }
        "flex-flow" => {
            let mut dir = None;
            let mut wrap = None;
            for _ in 0..2 {
                if dir.is_none()
                    && let Ok(d) = p.try_parse(|p| parse_longhand(FlexDirection, p))
                {
                    dir = Some(d);
                    continue;
                }
                if wrap.is_none()
                    && let Ok(w) = p.try_parse(|p| parse_longhand(FlexWrap, p))
                {
                    wrap = Some(w);
                    continue;
                }
                break;
            }
            if dir.is_none() && wrap.is_none() {
                return Err(ParseError::custom(ValueError::UnsupportedUnit(
                    String::new(),
                )));
            }
            Ok(vec![
                (FlexDirection, dir.unwrap_or(SV::Kw("row"))),
                (FlexWrap, wrap.unwrap_or(SV::Kw("nowrap"))),
            ])
        }
        "gap" => {
            let r = len_or_kw(p, &["normal"], true)?;
            let c = p
                .try_parse(|p| len_or_kw(p, &["normal"], true))
                .unwrap_or_else(|_| r.clone());
            Ok(vec![(RowGap, r), (ColumnGap, c)])
        }
        "place-items" | "place-content" | "place-self" => {
            let a = parse_align(p)?;
            let b = p.try_parse(parse_align).unwrap_or_else(|_| a.clone());
            Ok(vec![(props[0], a), (props[1], b)])
        }
        "overflow" => {
            let x = parse_longhand(OverflowX, p)?;
            let y = p
                .try_parse(|p| parse_longhand(OverflowY, p))
                .unwrap_or_else(|_| x.clone());
            Ok(vec![(OverflowX, x), (OverflowY, y)])
        }
        "grid-template" => {
            if p.try_parse(|p| p.expect_ident_matching("none")).is_ok() {
                return Ok(vec![
                    (GridTemplateRows, SV::Kw("none")),
                    (GridTemplateColumns, SV::Kw("none")),
                    (GridTemplateAreas, SV::Kw("none")),
                ]);
            }
            if let Ok(rows) = p.try_parse(|p| -> PResult<SV> {
                let r = parse_track_list(p)?;
                p.expect_delim('/')?;
                Ok(SV::Tracks(Arc::new(r)))
            }) {
                let cols = SV::Tracks(Arc::new(parse_track_list(p)?));
                return Ok(vec![
                    (GridTemplateRows, rows),
                    (GridTemplateColumns, cols),
                    (GridTemplateAreas, SV::Kw("none")),
                ]);
            }
            // Areas form: "a a" 40px "b b" 1fr / cols. Tern has no areas.
            if tern_rules() {
                return Err(ParseError::unexpected_token());
            }
            let mut row_tracks = Vec::new();
            let mut strings = Vec::new();
            while let Ok(s) = p.try_parse(|p| p.expect_string_cloned()) {
                strings.push(s.to_string());
                let t = p.try_parse(parse_track_size).unwrap_or(TrackSizeSpec {
                    min: TrackBreadthSpec::Auto,
                    max: TrackBreadthSpec::Auto,
                });
                row_tracks.push(TrackItemSpec::Track(t, Vec::new()));
            }
            if strings.is_empty() {
                return Err(ParseError::custom(ValueError::UnsupportedUnit(
                    String::new(),
                )));
            }
            let cols = if p.try_parse(|p| p.expect_delim('/')).is_ok() {
                SV::Tracks(Arc::new(parse_track_list(p)?))
            } else {
                SV::Kw("none")
            };
            let quoted: Vec<String> = strings.iter().map(|s| format!("\"{s}\"")).collect();
            let areas_css = quoted.join(" ");
            let mut ap = Parser::new(&areas_css);
            let areas = parse_areas(&mut ap)
                .map_err(|_| ParseError::custom(ValueError::UnsupportedUnit(String::new())))?;
            Ok(vec![
                (
                    GridTemplateRows,
                    SV::Tracks(Arc::new(TrackListSpec {
                        items: row_tracks,
                        trailing_names: Vec::new(),
                    })),
                ),
                (GridTemplateColumns, cols),
                (GridTemplateAreas, areas),
            ])
        }
        "grid-row" | "grid-column" => {
            let a = parse_grid_line(p)?;
            let b = if p.try_parse(|p| p.expect_delim('/')).is_ok() {
                parse_grid_line(p)?
            } else if let GridLine::Named(ref n) = a {
                GridLine::Named(n.clone())
            } else {
                GridLine::Auto
            };
            Ok(vec![(props[0], SV::Line(a)), (props[1], SV::Line(b))])
        }
        "grid-area" => {
            let mut lines = vec![parse_grid_line(p)?];
            while lines.len() < 4 && p.try_parse(|p| p.expect_delim('/')).is_ok() {
                lines.push(parse_grid_line(p)?);
            }
            // Omitted lines copy a named line (the start of the same axis, or the row start).
            let ident = |l: &GridLine| match l {
                GridLine::Named(n) => GridLine::Named(n.clone()),
                _ => GridLine::Auto,
            };
            let row_start = lines[0].clone();
            let col_start = lines.get(1).cloned().unwrap_or_else(|| ident(&row_start));
            let row_end = lines.get(2).cloned().unwrap_or_else(|| ident(&row_start));
            let col_end = lines.get(3).cloned().unwrap_or_else(|| ident(&col_start));
            let get = |i: usize| [&row_start, &col_start, &row_end, &col_end][i].clone();
            Ok(vec![
                (GridRowStart, SV::Line(get(0))),
                (GridColumnStart, SV::Line(get(1))),
                (GridRowEnd, SV::Line(get(2))),
                (GridColumnEnd, SV::Line(get(3))),
            ])
        }
        "font" => {
            let mut style = SV::Kw("normal");
            let mut weight = SV::Kw("normal");
            for _ in 0..4 {
                if let Ok(k) = p.try_parse(|p| kw(p, &["italic", "oblique"])) {
                    style = SV::Kw(k);
                    continue;
                }
                if let Ok(w) = p.try_parse(parse_font_weight) {
                    weight = w;
                    continue;
                }
                if p.try_parse(|p| kw(p, &["small-caps", "normal"])).is_ok() {
                    continue;
                }
                break;
            }
            let size = parse_font_size(p)?;
            let lh = if p.try_parse(|p| p.expect_delim('/')).is_ok() {
                parse_line_height(p)?
            } else {
                SV::Kw("normal")
            };
            let family = parse_font_family(p)?;
            Ok(vec![
                (FontStyle, style),
                (FontWeight, weight),
                (FontSize, size),
                (LineHeight, lh),
                (FontFamily, family),
            ])
        }
        "background" => {
            let mut color = SV::Color(SColor::Rgba(crate::css::values::Color::TRANSPARENT));
            let mut image = SV::Kw("none");
            loop {
                if p.is_exhausted() {
                    break;
                }
                if let Ok(c) = p.try_parse(parse_color) {
                    color = SV::Color(c);
                    continue;
                }
                if let Ok(i) = p.try_parse(|p| parse_longhand(BackgroundImage, p)) {
                    image = i;
                    continue;
                }
                match p.next()?.clone() {
                    Token::Ident(_)
                    | Token::Dimension { .. }
                    | Token::Percentage { .. }
                    | Token::Number { .. } => {}
                    Token::Delim('/') | Token::Comma => {}
                    Token::UnquotedUrl(_) => return Err(ParseError::custom(ValueError::Url)),
                    Token::Function(f) if f.eq_ignore_ascii_case("url") => {
                        return Err(ParseError::custom(ValueError::Url));
                    }
                    _ => return Err(ParseError::unexpected_token()),
                }
            }
            Ok(vec![(BackgroundColor, color), (BackgroundImage, image)])
        }
        "text-decoration" => {
            let mut line = None;
            let mut style = None;
            let mut color = None;
            for _ in 0..3 {
                if line.is_none()
                    && let Ok(l) = p.try_parse(parse_decoration_line)
                {
                    line = Some(l);
                    continue;
                }
                if style.is_none()
                    && let Ok(s) = p.try_parse(|p| parse_longhand(TextDecorationStyle, p))
                {
                    style = Some(s);
                    continue;
                }
                if color.is_none()
                    && let Ok(c) = p.try_parse(parse_color)
                {
                    color = Some(SV::Color(c));
                    continue;
                }
                break;
            }
            Ok(vec![
                (
                    TextDecorationLine,
                    line.unwrap_or(SV::Decoration(TextDecoration {
                        underline: false,
                        overline: false,
                        line_through: false,
                    })),
                ),
                (TextDecorationStyle, style.unwrap_or(SV::Kw("solid"))),
                (
                    TextDecorationColor,
                    color.unwrap_or(SV::Color(SColor::CurrentColor)),
                ),
            ])
        }
        "list-style" => {
            let mut ty = None;
            let mut pos = None;
            for _ in 0..3 {
                if pos.is_none()
                    && let Ok(v) = p.try_parse(|p| parse_longhand(ListStylePosition, p))
                {
                    pos = Some(v);
                    continue;
                }
                if ty.is_none()
                    && let Ok(v) = p.try_parse(|p| parse_longhand(ListStyleType, p))
                {
                    ty = Some(v);
                    continue;
                }
                break;
            }
            Ok(vec![
                (ListStyleType, ty.unwrap_or(SV::Kw("disc"))),
                (ListStylePosition, pos.unwrap_or(SV::Kw("outside"))),
            ])
        }
        "transition" => {
            let items = p.parse_comma_separated(|p| -> PResult<(Arc<str>, f32, Easing, f32)> {
                let mut prop: Option<Arc<str>> = None;
                let mut times = Vec::new();
                let mut easing = None;
                for _ in 0..4 {
                    if let Ok(t) = p.try_parse(parse_time) {
                        times.push(t);
                        continue;
                    }
                    if easing.is_none()
                        && let Ok(e) = p.try_parse(parse_easing)
                    {
                        easing = Some(e);
                        continue;
                    }
                    if prop.is_none()
                        && let Ok(id) = p.try_parse(|p| p.expect_ident_cloned())
                    {
                        prop = Some(Arc::from(id.to_ascii_lowercase().as_str()));
                        continue;
                    }
                    break;
                }
                Ok((
                    prop.unwrap_or_else(|| Arc::from("all")),
                    times.first().copied().unwrap_or(0.0),
                    easing.unwrap_or_default(),
                    times.get(1).copied().unwrap_or(0.0),
                ))
            })?;
            Ok(vec![
                (
                    TransitionProperty,
                    SV::Idents(Arc::new(items.iter().map(|i| i.0.clone()).collect())),
                ),
                (
                    TransitionDuration,
                    SV::Times(Arc::new(items.iter().map(|i| i.1).collect())),
                ),
                (
                    TransitionTimingFunction,
                    SV::Easings(Arc::new(items.iter().map(|i| i.2).collect())),
                ),
                (
                    TransitionDelay,
                    SV::Times(Arc::new(items.iter().map(|i| i.3).collect())),
                ),
            ])
        }
        "animation" => {
            type Item = (
                Arc<str>,
                f32,
                Easing,
                f32,
                f32,
                Arc<str>,
                Arc<str>,
                Arc<str>,
            );
            let items = p.parse_comma_separated(|p| -> PResult<Item> {
                let mut name: Option<Arc<str>> = None;
                let mut times = Vec::new();
                let mut easing = None;
                let mut iters = None;
                let mut dir = None;
                let mut fill = None;
                let mut play = None;
                for _ in 0..8 {
                    if let Ok(t) = p.try_parse(parse_time) {
                        times.push(t);
                        continue;
                    }
                    if easing.is_none()
                        && let Ok(e) = p.try_parse(parse_easing)
                    {
                        easing = Some(e);
                        continue;
                    }
                    if iters.is_none() {
                        if let Ok(n) = p.try_parse(|p| p.expect_number()) {
                            iters = Some(n);
                            continue;
                        }
                        if p.try_parse(|p| p.expect_ident_matching("infinite")).is_ok() {
                            iters = Some(f32::INFINITY);
                            continue;
                        }
                    }
                    if dir.is_none()
                        && let Ok(k) = p.try_parse(|p| {
                            kw(p, &["normal", "reverse", "alternate", "alternate-reverse"])
                        })
                    {
                        dir = Some(k);
                        continue;
                    }
                    if fill.is_none()
                        && let Ok(k) = p.try_parse(|p| kw(p, &["forwards", "backwards", "both"]))
                    {
                        fill = Some(k);
                        continue;
                    }
                    if play.is_none()
                        && let Ok(k) = p.try_parse(|p| kw(p, &["running", "paused"]))
                    {
                        play = Some(k);
                        continue;
                    }
                    if name.is_none() {
                        if let Ok(id) = p.try_parse(|p| p.expect_ident_cloned()) {
                            name = Some(Arc::from(id.as_ref()));
                            continue;
                        }
                        if let Ok(s) = p.try_parse(|p| p.expect_string_cloned()) {
                            name = Some(Arc::from(s.as_ref()));
                            continue;
                        }
                    }
                    break;
                }
                Ok((
                    name.unwrap_or_else(|| Arc::from("none")),
                    times.first().copied().unwrap_or(0.0),
                    easing.unwrap_or_default(),
                    times.get(1).copied().unwrap_or(0.0),
                    iters.unwrap_or(1.0),
                    Arc::from(dir.unwrap_or("normal")),
                    Arc::from(fill.unwrap_or("none")),
                    Arc::from(play.unwrap_or("running")),
                ))
            })?;
            Ok(vec![
                (
                    AnimationName,
                    SV::Idents(Arc::new(items.iter().map(|i| i.0.clone()).collect())),
                ),
                (
                    AnimationDuration,
                    SV::Times(Arc::new(items.iter().map(|i| i.1).collect())),
                ),
                (
                    AnimationTimingFunction,
                    SV::Easings(Arc::new(items.iter().map(|i| i.2).collect())),
                ),
                (
                    AnimationDelay,
                    SV::Times(Arc::new(items.iter().map(|i| i.3).collect())),
                ),
                (
                    AnimationIterationCount,
                    SV::Times(Arc::new(items.iter().map(|i| i.4).collect())),
                ),
                (
                    AnimationDirection,
                    SV::Idents(Arc::new(items.iter().map(|i| i.5.clone()).collect())),
                ),
                (
                    AnimationFillMode,
                    SV::Idents(Arc::new(items.iter().map(|i| i.6.clone()).collect())),
                ),
                (
                    AnimationPlayState,
                    SV::Idents(Arc::new(items.iter().map(|i| i.7.clone()).collect())),
                ),
            ])
        }
        _ => Err(ParseError::custom(ValueError::UnsupportedUnit(
            String::new(),
        ))),
    }
}
