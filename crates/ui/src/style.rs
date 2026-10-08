//! Computed style: the values layout and paint read.

use crate::css::values::{Color, Easing, LengthPct};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Display {
    None,
    Contents,
    Block,
    Inline,
    InlineBlock,
    FlowRoot,
    ListItem,
    Flex,
    InlineFlex,
    Grid,
    InlineGrid,
    Table,
    InlineTable,
    TableRowGroup,
    TableHeaderGroup,
    TableFooterGroup,
    TableRow,
    TableCell,
    TableColumn,
    TableColumnGroup,
}

impl Display {
    /// Inline-level in its parent's formatting context.
    pub fn is_inline_level(self) -> bool {
        matches!(
            self,
            Display::Inline
                | Display::InlineBlock
                | Display::InlineFlex
                | Display::InlineGrid
                | Display::InlineTable
        )
    }
    pub fn is_table_part(self) -> bool {
        matches!(
            self,
            Display::TableRowGroup
                | Display::TableHeaderGroup
                | Display::TableFooterGroup
                | Display::TableRow
                | Display::TableCell
                | Display::TableColumn
                | Display::TableColumnGroup
        )
    }
    pub fn is_row_group(self) -> bool {
        matches!(
            self,
            Display::TableRowGroup | Display::TableHeaderGroup | Display::TableFooterGroup
        )
    }
    /// The display after blockification (flex and grid items, absolutely positioned boxes, the root).
    pub fn blockified(self) -> Display {
        match self {
            Display::Inline | Display::InlineBlock => Display::Block,
            Display::InlineFlex => Display::Flex,
            Display::InlineGrid => Display::Grid,
            Display::InlineTable => Display::Table,
            d if d.is_table_part() => Display::Block,
            d => d,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Position {
    Static,
    Relative,
    Absolute,
    Fixed,
    /// Laid out as relative; sticky insets apply at paint time.
    Sticky,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoxSizing {
    ContentBox,
    BorderBox,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Overflow {
    Visible,
    Hidden,
    Clip,
    Scroll,
    Auto,
}

impl Overflow {
    pub fn is_scroll_container(self) -> bool {
        matches!(self, Overflow::Hidden | Overflow::Scroll | Overflow::Auto)
    }
    pub fn clips(self) -> bool {
        !matches!(self, Overflow::Visible)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BorderStyle {
    None,
    Hidden,
    Solid,
    Dashed,
    Dotted,
    Double,
    Groove,
    Ridge,
    Inset,
    Outset,
}

/// A computed size: width, height, min-*, max-*, flex-basis.
#[derive(Clone, Debug, PartialEq)]
pub enum Size {
    Auto,
    /// `none` for max-*.
    None,
    MinContent,
    MaxContent,
    FitContent,
    Length(LengthPct),
}

/// A computed `margin` or inset value.
#[derive(Clone, Debug, PartialEq)]
pub enum LengthPctAuto {
    Auto,
    Length(LengthPct),
}

impl LengthPctAuto {
    pub const ZERO: LengthPctAuto = LengthPctAuto::Length(LengthPct::Px(0.0));
    pub fn is_auto(&self) -> bool {
        matches!(self, LengthPctAuto::Auto)
    }
    pub fn resolve_or_zero(&self, basis: f32) -> f32 {
        match self {
            LengthPctAuto::Auto => 0.0,
            LengthPctAuto::Length(l) => l.resolve(basis),
        }
    }
    pub fn maybe_resolve(&self, basis: Option<f32>) -> Option<f32> {
        match self {
            LengthPctAuto::Auto => None,
            LengthPctAuto::Length(l) => l.maybe_resolve(basis),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlexDirection {
    Row,
    RowReverse,
    Column,
    ColumnReverse,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlexWrap {
    NoWrap,
    Wrap,
    WrapReverse,
}

/// align-items, align-self, justify-items, justify-self, align-content, justify-content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Normal,
    Auto,
    Start,
    End,
    FlexStart,
    FlexEnd,
    Center,
    Baseline,
    LastBaseline,
    Stretch,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
    Left,
    Right,
    SelfStart,
    SelfEnd,
}

/// One grid track sizing function.
#[derive(Clone, Debug, PartialEq)]
pub enum TrackBreadth {
    Length(LengthPct),
    Fr(f32),
    Auto,
    MinContent,
    MaxContent,
    FitContent(LengthPct),
}

#[derive(Clone, Debug, PartialEq)]
pub struct TrackSize {
    pub min: TrackBreadth,
    pub max: TrackBreadth,
}

impl TrackSize {
    pub fn single(b: TrackBreadth) -> TrackSize {
        match b {
            TrackBreadth::Fr(_) => TrackSize {
                min: TrackBreadth::Auto,
                max: b,
            },
            TrackBreadth::FitContent(_) => TrackSize {
                min: TrackBreadth::Auto,
                max: b,
            },
            b => TrackSize {
                min: b.clone(),
                max: b,
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum RepeatCount {
    Count(u16),
    AutoFill,
    AutoFit,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TrackItem {
    /// A track, with the line names before it.
    Track(TrackSize, Vec<Arc<str>>),
    Repeat(RepeatCount, Vec<TrackSize>, Vec<Vec<Arc<str>>>),
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct TrackList {
    pub items: Vec<TrackItem>,
    /// Names after the last track.
    pub trailing_names: Vec<Arc<str>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GridArea {
    pub name: Arc<str>,
    pub row_start: u16,
    pub row_end: u16,
    pub column_start: u16,
    pub column_end: u16,
}

#[derive(Clone, Debug, PartialEq)]
pub enum GridLine {
    Auto,
    Line(i16, Option<Arc<str>>),
    Span(u16, Option<Arc<str>>),
    Named(Arc<str>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridAutoFlow {
    pub column: bool,
    pub dense: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontStyle {
    Normal,
    Italic,
    Oblique,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LineHeight {
    Normal,
    Number(f32),
    Px(f32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextAlign {
    Start,
    End,
    Left,
    Right,
    Center,
    Justify,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WhiteSpace {
    Normal,
    Pre,
    NoWrap,
    PreWrap,
    PreLine,
    BreakSpaces,
}

impl WhiteSpace {
    pub fn collapses_spaces(self) -> bool {
        matches!(
            self,
            WhiteSpace::Normal | WhiteSpace::NoWrap | WhiteSpace::PreLine
        )
    }
    pub fn preserves_newlines(self) -> bool {
        !matches!(self, WhiteSpace::Normal | WhiteSpace::NoWrap)
    }
    pub fn wraps(self) -> bool {
        !matches!(self, WhiteSpace::Pre | WhiteSpace::NoWrap)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WordBreak {
    Normal,
    BreakAll,
    KeepAll,
    BreakWord,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverflowWrap {
    Normal,
    Anywhere,
    BreakWord,
}

/// How replaced content (an image) fills its content box.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectFit {
    Fill,
    Contain,
    Cover,
    None,
    ScaleDown,
}

/// How an image is scaled: smoothly, or by the nearest pixel (`pixelated`, `crisp-edges`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageRendering {
    Smooth,
    Pixelated,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextOverflow {
    Clip,
    Ellipsis,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextTransform {
    None,
    Uppercase,
    Lowercase,
    Capitalize,
}

#[derive(Clone, Debug, PartialEq)]
pub enum VerticalAlign {
    Baseline,
    Sub,
    Super,
    TextTop,
    TextBottom,
    Middle,
    Top,
    Bottom,
    Length(LengthPct),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Visibility {
    Visible,
    Hidden,
    Collapse,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListStyleType {
    None,
    Disc,
    Circle,
    Square,
    Decimal,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextDecoration {
    pub underline: bool,
    pub overline: bool,
    pub line_through: bool,
}

/// One 2D transform function, with lengths computed.
#[derive(Clone, Debug, PartialEq)]
pub enum TransformFn {
    Translate(LengthPct, LengthPct),
    Scale(f32, f32),
    Rotate(f32),
    Skew(f32, f32),
    Matrix([f32; 6]),
}

/// A computed `box-shadow`: offsets, blur radius and spread in px, and its color.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shadow {
    pub inset: bool,
    pub x: f32,
    pub y: f32,
    pub blur: f32,
    pub spread: f32,
    pub color: Color,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Transition {
    pub property: Arc<str>,
    pub duration: f32,
    pub delay: f32,
    pub easing: Easing,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Animation {
    pub name: Arc<str>,
    pub duration: f32,
    pub delay: f32,
    pub easing: Easing,
    /// `f32::INFINITY` for `infinite`.
    pub iterations: f32,
    pub direction: Arc<str>,
    pub fill_mode: Arc<str>,
    pub paused: bool,
}

/// A piece of generated `content`.
#[derive(Clone, Debug, PartialEq)]
pub enum ContentItem {
    Text(Arc<str>),
    Attr(Arc<str>),
}

/// Custom properties, already substituted.
pub type CustomProps = HashMap<Arc<str>, Arc<str>>;

/// The computed style of one element or pseudo-element.
#[derive(Clone, Debug, PartialEq)]
pub struct ComputedStyle {
    // Inherited.
    pub color: Color,
    pub font_family: Arc<str>,
    pub font_size: f32,
    pub font_weight: f32,
    pub font_style: FontStyle,
    pub line_height: LineHeight,
    pub letter_spacing: f32,
    pub text_align: TextAlign,
    pub white_space: WhiteSpace,
    pub word_break: WordBreak,
    pub overflow_wrap: OverflowWrap,
    pub text_transform: TextTransform,
    pub visibility: Visibility,
    pub cursor: Arc<str>,
    pub pointer_events: bool,
    pub list_style_type: ListStyleType,
    pub list_style_inside: bool,
    pub border_collapse: bool,
    pub border_spacing: (f32, f32),
    pub color_scheme_dark: Option<bool>,
    /// The effective zoom (the product of `zoom` on this element and its ancestors). Lengths in
    /// px are multiplied by it at computed-value time.
    pub zoom: f32,
    pub custom: Arc<CustomProps>,

    // Not inherited.
    pub display: Display,
    pub position: Position,
    pub box_sizing: BoxSizing,
    pub inset: [LengthPctAuto; 4],
    pub margin: [LengthPctAuto; 4],
    pub padding: [LengthPct; 4],
    pub border_width: [f32; 4],
    pub border_style: [BorderStyle; 4],
    pub border_color: [Color; 4],
    /// top-left, top-right, bottom-right, bottom-left; horizontal and vertical radii.
    pub border_radius: [(LengthPct, LengthPct); 4],
    pub width: Size,
    pub height: Size,
    pub min_width: Size,
    pub min_height: Size,
    pub max_width: Size,
    pub max_height: Size,
    pub aspect_ratio: Option<f32>,
    pub overflow_x: Overflow,
    pub overflow_y: Overflow,
    pub flex_direction: FlexDirection,
    pub flex_wrap: FlexWrap,
    pub flex_grow: f32,
    pub flex_shrink: f32,
    pub flex_basis: Size,
    pub order: i32,
    pub align_items: Align,
    pub align_self: Align,
    pub align_content: Align,
    pub justify_content: Align,
    pub justify_items: Align,
    pub justify_self: Align,
    pub row_gap: Option<LengthPct>,
    pub column_gap: Option<LengthPct>,
    pub grid_template_columns: Arc<TrackList>,
    pub grid_template_rows: Arc<TrackList>,
    pub grid_template_areas: Option<Arc<(Vec<GridArea>, u16, u16)>>,
    pub grid_auto_flow: GridAutoFlow,
    pub grid_auto_columns: Arc<Vec<TrackSize>>,
    pub grid_auto_rows: Arc<Vec<TrackSize>>,
    pub grid_row_start: GridLine,
    pub grid_row_end: GridLine,
    pub grid_column_start: GridLine,
    pub grid_column_end: GridLine,
    pub background_color: Color,
    /// A `background-image: url(data:…)` picture: its URL, loaded through the document's URL
    /// loader ([`crate::Document::set_url_loader`]).
    pub background_image: Option<Arc<str>>,
    pub opacity: f32,
    pub z_index: Option<i32>,
    pub vertical_align: VerticalAlign,
    pub text_overflow: TextOverflow,
    pub text_decoration: TextDecoration,
    pub text_decoration_color: Option<Color>,
    pub transform: Option<Arc<Vec<TransformFn>>>,
    pub transform_origin: (LengthPct, LengthPct),
    pub outline_width: f32,
    pub outline_style: BorderStyle,
    pub outline_color: Color,
    pub outline_offset: f32,
    /// `box-shadow`, in paint order from the top (`None` is `none`).
    pub box_shadow: Option<Arc<Vec<Shadow>>>,
    pub object_fit: ObjectFit,
    pub image_rendering: ImageRendering,
    pub content: Option<Arc<Vec<ContentItem>>>,
    pub transitions: Option<Arc<Vec<Transition>>>,
    pub animations: Option<Arc<Vec<Animation>>>,
    pub line_clamp: Option<u32>,
    pub user_select: bool,
}

impl Default for ComputedStyle {
    fn default() -> Self {
        ComputedStyle {
            color: Color::BLACK,
            font_family: Arc::from("system-ui"),
            font_size: 16.0,
            font_weight: 400.0,
            font_style: FontStyle::Normal,
            line_height: LineHeight::Normal,
            letter_spacing: 0.0,
            text_align: TextAlign::Start,
            white_space: WhiteSpace::Normal,
            word_break: WordBreak::Normal,
            overflow_wrap: OverflowWrap::Normal,
            text_transform: TextTransform::None,
            visibility: Visibility::Visible,
            cursor: Arc::from("auto"),
            pointer_events: true,
            list_style_type: ListStyleType::Disc,
            list_style_inside: false,
            border_collapse: false,
            border_spacing: (0.0, 0.0),
            color_scheme_dark: None,
            zoom: 1.0,
            custom: Arc::new(HashMap::new()),
            display: Display::Inline,
            position: Position::Static,
            box_sizing: BoxSizing::ContentBox,
            inset: [
                LengthPctAuto::Auto,
                LengthPctAuto::Auto,
                LengthPctAuto::Auto,
                LengthPctAuto::Auto,
            ],
            margin: [
                LengthPctAuto::ZERO,
                LengthPctAuto::ZERO,
                LengthPctAuto::ZERO,
                LengthPctAuto::ZERO,
            ],
            padding: [
                LengthPct::ZERO,
                LengthPct::ZERO,
                LengthPct::ZERO,
                LengthPct::ZERO,
            ],
            border_width: [3.0; 4],
            border_style: [BorderStyle::None; 4],
            border_color: [Color::BLACK; 4],
            border_radius: [
                (LengthPct::ZERO, LengthPct::ZERO),
                (LengthPct::ZERO, LengthPct::ZERO),
                (LengthPct::ZERO, LengthPct::ZERO),
                (LengthPct::ZERO, LengthPct::ZERO),
            ],
            width: Size::Auto,
            height: Size::Auto,
            min_width: Size::Auto,
            min_height: Size::Auto,
            max_width: Size::None,
            max_height: Size::None,
            aspect_ratio: None,
            overflow_x: Overflow::Visible,
            overflow_y: Overflow::Visible,
            flex_direction: FlexDirection::Row,
            flex_wrap: FlexWrap::NoWrap,
            flex_grow: 0.0,
            flex_shrink: 1.0,
            flex_basis: Size::Auto,
            order: 0,
            align_items: Align::Normal,
            align_self: Align::Auto,
            align_content: Align::Normal,
            justify_content: Align::Normal,
            justify_items: Align::Normal,
            justify_self: Align::Auto,
            row_gap: None,
            column_gap: None,
            grid_template_columns: Arc::new(TrackList::default()),
            grid_template_rows: Arc::new(TrackList::default()),
            grid_template_areas: None,
            grid_auto_flow: GridAutoFlow {
                column: false,
                dense: false,
            },
            grid_auto_columns: Arc::new(Vec::new()),
            grid_auto_rows: Arc::new(Vec::new()),
            grid_row_start: GridLine::Auto,
            grid_row_end: GridLine::Auto,
            grid_column_start: GridLine::Auto,
            grid_column_end: GridLine::Auto,
            background_color: Color::TRANSPARENT,
            background_image: None,
            opacity: 1.0,
            z_index: None,
            vertical_align: VerticalAlign::Baseline,
            text_overflow: TextOverflow::Clip,
            text_decoration: TextDecoration {
                underline: false,
                overline: false,
                line_through: false,
            },
            text_decoration_color: None,
            transform: None,
            transform_origin: (LengthPct::Pct(50.0), LengthPct::Pct(50.0)),
            outline_width: 3.0,
            outline_style: BorderStyle::None,
            outline_color: Color::BLACK,
            outline_offset: 0.0,
            box_shadow: None,
            object_fit: ObjectFit::Fill,
            image_rendering: ImageRendering::Smooth,
            content: None,
            transitions: None,
            animations: None,
            line_clamp: None,
            user_select: true,
        }
    }
}

impl ComputedStyle {
    /// A fresh style for a child of `parent`: inherited properties copied, the rest initial.
    pub fn inherit_from(parent: &ComputedStyle, initial: &ComputedStyle) -> ComputedStyle {
        ComputedStyle {
            color: parent.color,
            font_family: parent.font_family.clone(),
            font_size: parent.font_size,
            font_weight: parent.font_weight,
            font_style: parent.font_style,
            line_height: parent.line_height,
            letter_spacing: parent.letter_spacing,
            text_align: parent.text_align,
            white_space: parent.white_space,
            word_break: parent.word_break,
            overflow_wrap: parent.overflow_wrap,
            text_transform: parent.text_transform,
            visibility: parent.visibility,
            cursor: parent.cursor.clone(),
            pointer_events: parent.pointer_events,
            list_style_type: parent.list_style_type,
            list_style_inside: parent.list_style_inside,
            border_collapse: parent.border_collapse,
            border_spacing: parent.border_spacing,
            color_scheme_dark: parent.color_scheme_dark,
            zoom: parent.zoom,
            custom: parent.custom.clone(),
            user_select: parent.user_select,
            ..initial.clone()
        }
    }

    /// Border widths as used: zero where the style is none or hidden.
    pub fn used_border(&self) -> [f32; 4] {
        let mut b = self.border_width;
        for (i, s) in self.border_style.iter().enumerate() {
            if matches!(s, BorderStyle::None | BorderStyle::Hidden) {
                b[i] = 0.0;
            }
        }
        b
    }

    pub fn is_scroll_container(&self) -> bool {
        self.overflow_x.is_scroll_container() || self.overflow_y.is_scroll_container()
    }

    pub fn is_positioned(&self) -> bool {
        !matches!(self.position, Position::Static)
    }

    pub fn is_out_of_flow(&self) -> bool {
        matches!(self.position, Position::Absolute | Position::Fixed)
    }

    /// The used line height in px for a font with the given ascent and descent.
    pub fn line_height_px(&self, ascent: f32, descent: f32, line_gap: f32) -> f32 {
        match self.line_height {
            LineHeight::Normal => ascent + descent + line_gap,
            LineHeight::Number(n) => n * self.font_size,
            LineHeight::Px(px) => px,
        }
    }
}
