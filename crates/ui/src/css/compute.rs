//! Turning cascaded specified values into computed values.

use super::properties::*;
use super::values::*;
use crate::style::*;
use std::sync::Arc;

/// Inputs to value computation that don't come from the element's own declarations.
#[derive(Clone, Copy, Debug)]
pub struct ComputeEnv {
    pub root_font_size: f32,
    pub vw: f32,
    pub vh: f32,
    pub dark: bool,
    /// Device pixels per CSS px; border widths snap to device pixels.
    pub dpr: f32,
}

macro_rules! copy_props {
    ($d:ident, $s:ident, $p:ident; $($v:ident => $f:ident $([$i:literal])?;)*) => {
        match $p { $(Prop::$v => { $d.$f$([$i])? = $s.$f$([$i])?.clone(); })* }
    };
}

/// Copy one property's computed value from `src` (for `inherit`, `initial` and `unset`).
pub fn copy_prop(d: &mut ComputedStyle, s: &ComputedStyle, p: Prop) {
    copy_props! { d, s, p;
        Zoom => zoom;
        FontSize => font_size;
        FontFamily => font_family;
        FontWeight => font_weight;
        FontStyle => font_style;
        LineHeight => line_height;
        ColorScheme => color_scheme_dark;
        Color => color;
        LetterSpacing => letter_spacing;
        TextAlign => text_align;
        WhiteSpace => white_space;
        WordBreak => word_break;
        OverflowWrap => overflow_wrap;
        TextTransform => text_transform;
        Visibility => visibility;
        Cursor => cursor;
        PointerEvents => pointer_events;
        UserSelect => user_select;
        ListStyleType => list_style_type;
        ListStylePosition => list_style_inside;
        BorderCollapse => border_collapse;
        BorderSpacing => border_spacing;
        Display => display;
        Position => position;
        BoxSizing => box_sizing;
        Top => inset[0];
        Right => inset[1];
        Bottom => inset[2];
        Left => inset[3];
        MarginTop => margin[0];
        MarginRight => margin[1];
        MarginBottom => margin[2];
        MarginLeft => margin[3];
        PaddingTop => padding[0];
        PaddingRight => padding[1];
        PaddingBottom => padding[2];
        PaddingLeft => padding[3];
        BorderTopStyle => border_style[0];
        BorderRightStyle => border_style[1];
        BorderBottomStyle => border_style[2];
        BorderLeftStyle => border_style[3];
        BorderTopWidth => border_width[0];
        BorderRightWidth => border_width[1];
        BorderBottomWidth => border_width[2];
        BorderLeftWidth => border_width[3];
        BorderTopColor => border_color[0];
        BorderRightColor => border_color[1];
        BorderBottomColor => border_color[2];
        BorderLeftColor => border_color[3];
        BorderTopLeftRadius => border_radius[0];
        BorderTopRightRadius => border_radius[1];
        BorderBottomRightRadius => border_radius[2];
        BorderBottomLeftRadius => border_radius[3];
        Width => width;
        Height => height;
        MinWidth => min_width;
        MinHeight => min_height;
        MaxWidth => max_width;
        MaxHeight => max_height;
        AspectRatio => aspect_ratio;
        OverflowX => overflow_x;
        OverflowY => overflow_y;
        FlexDirection => flex_direction;
        FlexWrap => flex_wrap;
        FlexGrow => flex_grow;
        FlexShrink => flex_shrink;
        FlexBasis => flex_basis;
        Order => order;
        AlignItems => align_items;
        AlignSelf => align_self;
        AlignContent => align_content;
        JustifyContent => justify_content;
        JustifyItems => justify_items;
        JustifySelf => justify_self;
        RowGap => row_gap;
        ColumnGap => column_gap;
        GridTemplateColumns => grid_template_columns;
        GridTemplateRows => grid_template_rows;
        GridTemplateAreas => grid_template_areas;
        GridAutoFlow => grid_auto_flow;
        GridAutoColumns => grid_auto_columns;
        GridAutoRows => grid_auto_rows;
        GridRowStart => grid_row_start;
        GridRowEnd => grid_row_end;
        GridColumnStart => grid_column_start;
        GridColumnEnd => grid_column_end;
        BackgroundColor => background_color;
        BackgroundImage => background_image;
        Opacity => opacity;
        ZIndex => z_index;
        VerticalAlign => vertical_align;
        TextOverflow => text_overflow;
        TextDecorationLine => text_decoration;
        TextDecorationStyle => text_decoration;
        TextDecorationColor => text_decoration_color;
        Transform => transform;
        TransformOrigin => transform_origin;
        OutlineWidth => outline_width;
        OutlineStyle => outline_style;
        OutlineColor => outline_color;
        OutlineOffset => outline_offset;
        TableLayout => table_layout_fixed;
        ObjectFit => object_fit;
        ImageRendering => image_rendering;
        Content => content;
        TransitionProperty => transitions;
        TransitionDuration => transitions;
        TransitionTimingFunction => transitions;
        TransitionDelay => transitions;
        AnimationName => animations;
        AnimationDuration => animations;
        AnimationTimingFunction => animations;
        AnimationDelay => animations;
        AnimationIterationCount => animations;
        AnimationDirection => animations;
        AnimationFillMode => animations;
        AnimationPlayState => animations;
        LineClamp => line_clamp;
    }
}

fn font_size_keyword(k: &str, parent: f32) -> f32 {
    match k {
        "xx-small" => 9.0,
        "x-small" => 10.0,
        "small" => 13.0,
        "medium" => 16.0,
        "large" => 18.0,
        "x-large" => 24.0,
        "xx-large" => 32.0,
        "xxx-large" => 48.0,
        "smaller" => parent / 1.2,
        "larger" => parent * 1.2,
        _ => parent,
    }
}

fn align(k: &str) -> Align {
    match k {
        "auto" => Align::Auto,
        "start" => Align::Start,
        "end" => Align::End,
        "flex-start" => Align::FlexStart,
        "flex-end" => Align::FlexEnd,
        "center" => Align::Center,
        "baseline" => Align::Baseline,
        "last-baseline" => Align::LastBaseline,
        "stretch" => Align::Stretch,
        "space-between" => Align::SpaceBetween,
        "space-around" => Align::SpaceAround,
        "space-evenly" => Align::SpaceEvenly,
        "left" => Align::Left,
        "right" => Align::Right,
        "self-start" => Align::SelfStart,
        "self-end" => Align::SelfEnd,
        _ => Align::Normal,
    }
}

fn border_style(k: &str) -> BorderStyle {
    match k {
        "hidden" => BorderStyle::Hidden,
        "solid" | "auto" => BorderStyle::Solid,
        "dashed" => BorderStyle::Dashed,
        "dotted" => BorderStyle::Dotted,
        "double" => BorderStyle::Double,
        "groove" => BorderStyle::Groove,
        "ridge" => BorderStyle::Ridge,
        "inset" => BorderStyle::Inset,
        "outset" => BorderStyle::Outset,
        _ => BorderStyle::None,
    }
}

fn size(sv: &SV, cx: &LengthContext) -> Option<Size> {
    Some(match sv {
        SV::Kw("auto") => Size::Auto,
        SV::Kw("none") => Size::None,
        SV::Kw("min-content") => Size::MinContent,
        SV::Kw("max-content") | SV::Kw("content") => Size::MaxContent,
        SV::Kw("fit-content") | SV::Kw("-webkit-fill-available") | SV::Kw("stretch") => {
            Size::FitContent
        }
        SV::Len(e) => Size::Length(e.compute(cx)),
        _ => return None,
    })
}

fn lpa(sv: &SV, cx: &LengthContext) -> Option<LengthPctAuto> {
    Some(match sv {
        SV::Kw("auto") => LengthPctAuto::Auto,
        SV::Len(e) => LengthPctAuto::Length(e.compute(cx)),
        _ => return None,
    })
}

fn breadth(b: &TrackBreadthSpec, cx: &LengthContext) -> TrackBreadth {
    match b {
        TrackBreadthSpec::Length(e) => TrackBreadth::Length(e.compute(cx)),
        TrackBreadthSpec::Fr(f) => TrackBreadth::Fr(*f),
        TrackBreadthSpec::Auto => TrackBreadth::Auto,
        TrackBreadthSpec::MinContent => TrackBreadth::MinContent,
        TrackBreadthSpec::MaxContent => TrackBreadth::MaxContent,
        TrackBreadthSpec::FitContent(e) => TrackBreadth::FitContent(e.compute(cx)),
    }
}

fn track_size(t: &TrackSizeSpec, cx: &LengthContext) -> TrackSize {
    TrackSize {
        min: breadth(&t.min, cx),
        max: breadth(&t.max, cx),
    }
}

fn track_list(l: &TrackListSpec, cx: &LengthContext) -> TrackList {
    TrackList {
        items: l
            .items
            .iter()
            .map(|i| match i {
                TrackItemSpec::Track(t, n) => TrackItem::Track(track_size(t, cx), n.clone()),
                TrackItemSpec::Repeat(c, ts, n) => TrackItem::Repeat(
                    c.clone(),
                    ts.iter().map(|t| track_size(t, cx)).collect(),
                    n.clone(),
                ),
            })
            .collect(),
        trailing_names: l.trailing_names.clone(),
    }
}

/// Build or update the transitions list from one of its longhands.
fn set_list<T: Clone>(
    list: &mut Vec<T>,
    len: usize,
    make: impl Fn() -> T,
    set: impl Fn(&mut T, usize),
) {
    if list.len() < len {
        list.resize_with(len, &make);
    }
    for (i, item) in list.iter_mut().enumerate() {
        set(item, i);
    }
}

/// Compute one property from its winning specified value. Returns false if the value doesn't fit
/// (after var() substitution), in which case the caller treats the property as unset.
pub fn compute_prop(
    cs: &mut ComputedStyle,
    parent: &ComputedStyle,
    initial: &ComputedStyle,
    prop: Prop,
    sv: &SV,
    env: &ComputeEnv,
    attr: &dyn Fn(&str) -> Option<String>,
) -> bool {
    match sv {
        SV::Inherit => {
            copy_prop(cs, parent, prop);
            return true;
        }
        SV::Initial => {
            copy_prop(cs, initial, prop);
            return true;
        }
        SV::Unset => {
            copy_prop(cs, if prop.inherited() { parent } else { initial }, prop);
            return true;
        }
        _ => {}
    }
    let cx = LengthContext {
        font_size: cs.font_size,
        root_font_size: env.root_font_size,
        vw: env.vw,
        vh: env.vh,
        zoom: cs.zoom,
    };
    let color = |sv: &SV, current: Color| -> Option<Color> {
        match sv {
            SV::Color(c) => Some(c.compute(current, cs_dark(parent, env))),
            _ => None,
        }
    };
    use Prop as P;
    macro_rules! kw_map {
        ($field:expr, { $($k:literal => $v:expr),* $(,)? }) => {
            match sv { $(SV::Kw($k) => { $field = $v; true })* _ => false }
        };
    }
    match prop {
        P::Zoom => {
            let z = match sv {
                SV::Num(n) if *n > 0.0 => *n,
                SV::Kw("reset") => 1.0 / parent.zoom,
                _ => 1.0,
            };
            cs.zoom = parent.zoom * z;
            // Inherited lengths follow the new zoom.
            cs.font_size *= z;
            cs.letter_spacing *= z;
            cs.border_spacing = (cs.border_spacing.0 * z, cs.border_spacing.1 * z);
            if let LineHeight::Px(p) = cs.line_height {
                cs.line_height = LineHeight::Px(p * z);
            }
            true
        }
        P::FontSize => match sv {
            SV::Kw(k) => {
                cs.font_size = match *k {
                    "smaller" | "larger" => {
                        font_size_keyword(k, parent.font_size) * cs.zoom / parent.zoom
                    }
                    k => font_size_keyword(k, parent.font_size) * cs.zoom,
                };
                true
            }
            SV::Len(e) => {
                let pcx = LengthContext {
                    font_size: parent.font_size,
                    ..cx
                };
                cs.font_size = e.compute_px(&pcx, parent.font_size).max(0.0);
                true
            }
            _ => false,
        },
        P::FontFamily => match sv {
            SV::Str(s) => {
                cs.font_family = s.clone();
                true
            }
            _ => false,
        },
        P::FontWeight => match sv {
            SV::Num(n) => {
                cs.font_weight = *n;
                true
            }
            SV::Kw(k) => {
                let pw = parent.font_weight;
                cs.font_weight = match *k {
                    "bold" => 700.0,
                    "bolder" => {
                        if pw < 350.0 {
                            400.0
                        } else if pw < 550.0 {
                            700.0
                        } else {
                            900.0_f32.max(pw)
                        }
                    }
                    "lighter" => {
                        if pw < 550.0 {
                            100.0
                        } else if pw < 750.0 {
                            400.0
                        } else {
                            700.0
                        }
                    }
                    _ => 400.0,
                };
                true
            }
            _ => false,
        },
        P::FontStyle => kw_map!(cs.font_style, {
            "normal" => FontStyle::Normal, "italic" => FontStyle::Italic, "oblique" => FontStyle::Oblique,
        }),
        P::LineHeight => match sv {
            SV::Kw(_) => {
                cs.line_height = LineHeight::Normal;
                true
            }
            SV::Num(n) => {
                cs.line_height = LineHeight::Number(*n);
                true
            }
            SV::Len(e) => {
                cs.line_height = LineHeight::Px(e.compute_px(&cx, cs.font_size));
                true
            }
            _ => false,
        },
        P::ColorScheme => match sv {
            SV::Kw(k) => {
                cs.color_scheme_dark = match *k {
                    "dark" => Some(true),
                    "light" => Some(false),
                    _ => None,
                };
                true
            }
            _ => false,
        },
        P::Color => match color(sv, parent.color) {
            Some(c) => {
                cs.color = c;
                true
            }
            None => false,
        },
        P::LetterSpacing => match sv {
            SV::Kw(_) => {
                cs.letter_spacing = 0.0;
                true
            }
            SV::Len(e) => {
                cs.letter_spacing = e.compute_px(&cx, 0.0);
                true
            }
            _ => false,
        },
        P::TextAlign => kw_map!(cs.text_align, {
            "start" => TextAlign::Start, "end" => TextAlign::End, "left" => TextAlign::Left,
            "right" => TextAlign::Right, "center" => TextAlign::Center, "-webkit-center" => TextAlign::Center,
            "justify" => TextAlign::Justify, "match-parent" => parent.text_align,
        }),
        P::WhiteSpace => kw_map!(cs.white_space, {
            "normal" => WhiteSpace::Normal, "pre" => WhiteSpace::Pre, "nowrap" => WhiteSpace::NoWrap,
            "pre-wrap" => WhiteSpace::PreWrap, "pre-line" => WhiteSpace::PreLine, "break-spaces" => WhiteSpace::BreakSpaces,
        }),
        P::WordBreak => kw_map!(cs.word_break, {
            "normal" => WordBreak::Normal, "break-all" => WordBreak::BreakAll, "keep-all" => WordBreak::KeepAll,
            "break-word" => WordBreak::BreakWord,
        }),
        P::OverflowWrap => kw_map!(cs.overflow_wrap, {
            "normal" => OverflowWrap::Normal, "anywhere" => OverflowWrap::Anywhere, "break-word" => OverflowWrap::BreakWord,
        }),
        P::TextTransform => kw_map!(cs.text_transform, {
            "none" => TextTransform::None, "uppercase" => TextTransform::Uppercase,
            "lowercase" => TextTransform::Lowercase, "capitalize" => TextTransform::Capitalize,
        }),
        P::Visibility => kw_map!(cs.visibility, {
            "visible" => Visibility::Visible, "hidden" => Visibility::Hidden, "collapse" => Visibility::Collapse,
        }),
        P::Cursor => match sv {
            SV::Kw(k) => {
                cs.cursor = Arc::from(*k);
                true
            }
            _ => false,
        },
        P::PointerEvents => match sv {
            SV::Kw(k) => {
                cs.pointer_events = *k != "none";
                true
            }
            _ => false,
        },
        P::UserSelect => match sv {
            SV::Kw(k) => {
                cs.user_select = *k != "none";
                true
            }
            _ => false,
        },
        P::ListStyleType => match sv {
            SV::Str(_) => {
                cs.list_style_type = ListStyleType::None;
                true
            }
            _ => kw_map!(cs.list_style_type, {
                "none" => ListStyleType::None, "disc" => ListStyleType::Disc, "circle" => ListStyleType::Circle,
                "square" => ListStyleType::Square, "decimal" => ListStyleType::Decimal,
            }),
        },
        P::ListStylePosition => {
            kw_map!(cs.list_style_inside, { "inside" => true, "outside" => false })
        }
        P::BorderCollapse => {
            kw_map!(cs.border_collapse, { "collapse" => true, "separate" => false })
        }
        P::BorderSpacing => match sv {
            SV::Spacing(a, b) => {
                cs.border_spacing = (a.compute_px(&cx, 0.0), b.compute_px(&cx, 0.0));
                true
            }
            _ => false,
        },
        P::Display => kw_map!(cs.display, {
            "none" => Display::None, "contents" => Display::Contents, "block" => Display::Block,
            "inline" => Display::Inline, "inline-block" => Display::InlineBlock, "flow-root" => Display::FlowRoot,
            "list-item" => Display::ListItem, "flex" => Display::Flex, "inline-flex" => Display::InlineFlex,
            "grid" => Display::Grid, "inline-grid" => Display::InlineGrid, "table" => Display::Table,
            "inline-table" => Display::InlineTable, "table-row-group" => Display::TableRowGroup,
            "table-header-group" => Display::TableHeaderGroup, "table-footer-group" => Display::TableFooterGroup,
            "table-row" => Display::TableRow, "table-cell" => Display::TableCell, "table-column" => Display::TableColumn,
            "table-column-group" => Display::TableColumnGroup, "table-caption" => Display::TableCaption,
        }),
        P::Position => kw_map!(cs.position, {
            "static" => Position::Static, "relative" => Position::Relative, "absolute" => Position::Absolute,
            "fixed" => Position::Fixed, "sticky" => Position::Sticky,
        }),
        P::BoxSizing => kw_map!(cs.box_sizing, {
            "content-box" => BoxSizing::ContentBox, "border-box" => BoxSizing::BorderBox,
        }),
        P::Top | P::Right | P::Bottom | P::Left => {
            let i = [P::Top, P::Right, P::Bottom, P::Left]
                .iter()
                .position(|p| *p == prop)
                .unwrap_or(0);
            match lpa(sv, &cx) {
                Some(v) => {
                    cs.inset[i] = v;
                    true
                }
                None => false,
            }
        }
        P::MarginTop | P::MarginRight | P::MarginBottom | P::MarginLeft => {
            let i = [P::MarginTop, P::MarginRight, P::MarginBottom, P::MarginLeft]
                .iter()
                .position(|p| *p == prop)
                .unwrap_or(0);
            match lpa(sv, &cx) {
                Some(v) => {
                    cs.margin[i] = v;
                    true
                }
                None => false,
            }
        }
        P::PaddingTop | P::PaddingRight | P::PaddingBottom | P::PaddingLeft => {
            let i = [
                P::PaddingTop,
                P::PaddingRight,
                P::PaddingBottom,
                P::PaddingLeft,
            ]
            .iter()
            .position(|p| *p == prop)
            .unwrap_or(0);
            match sv {
                SV::Len(e) => {
                    cs.padding[i] = e.compute(&cx);
                    true
                }
                _ => false,
            }
        }
        P::BorderTopStyle | P::BorderRightStyle | P::BorderBottomStyle | P::BorderLeftStyle => {
            let i = [
                P::BorderTopStyle,
                P::BorderRightStyle,
                P::BorderBottomStyle,
                P::BorderLeftStyle,
            ]
            .iter()
            .position(|p| *p == prop)
            .unwrap_or(0);
            match sv {
                SV::Kw(k) => {
                    cs.border_style[i] = border_style(k);
                    true
                }
                _ => false,
            }
        }
        P::BorderTopWidth | P::BorderRightWidth | P::BorderBottomWidth | P::BorderLeftWidth => {
            let i = [
                P::BorderTopWidth,
                P::BorderRightWidth,
                P::BorderBottomWidth,
                P::BorderLeftWidth,
            ]
            .iter()
            .position(|p| *p == prop)
            .unwrap_or(0);
            match sv {
                SV::Len(e) => {
                    let w = e.compute_px(&cx, 0.0).max(0.0);
                    // Browsers floor border widths to device pixels, keeping at least one.
                    let d = env.dpr.max(1.0);
                    cs.border_width[i] = if w > 0.0 && w < 1.0 / d {
                        1.0 / d
                    } else {
                        (w * d).floor() / d
                    };
                    true
                }
                _ => false,
            }
        }
        P::BorderTopColor | P::BorderRightColor | P::BorderBottomColor | P::BorderLeftColor => {
            let i = [
                P::BorderTopColor,
                P::BorderRightColor,
                P::BorderBottomColor,
                P::BorderLeftColor,
            ]
            .iter()
            .position(|p| *p == prop)
            .unwrap_or(0);
            match color(sv, cs.color) {
                Some(c) => {
                    cs.border_color[i] = c;
                    true
                }
                None => false,
            }
        }
        P::BorderTopLeftRadius
        | P::BorderTopRightRadius
        | P::BorderBottomRightRadius
        | P::BorderBottomLeftRadius => {
            let i = [
                P::BorderTopLeftRadius,
                P::BorderTopRightRadius,
                P::BorderBottomRightRadius,
                P::BorderBottomLeftRadius,
            ]
            .iter()
            .position(|p| *p == prop)
            .unwrap_or(0);
            match sv {
                SV::Radius(a, b) => {
                    cs.border_radius[i] = (a.compute(&cx), b.compute(&cx));
                    true
                }
                _ => false,
            }
        }
        P::Width
        | P::Height
        | P::MinWidth
        | P::MinHeight
        | P::MaxWidth
        | P::MaxHeight
        | P::FlexBasis => {
            let Some(v) = size(sv, &cx) else { return false };
            match prop {
                P::Width => cs.width = v,
                P::Height => cs.height = v,
                P::MinWidth => cs.min_width = v,
                P::MinHeight => cs.min_height = v,
                P::MaxWidth => cs.max_width = v,
                P::MaxHeight => cs.max_height = v,
                _ => cs.flex_basis = v,
            }
            true
        }
        P::AspectRatio => match sv {
            SV::Kw(_) => {
                cs.aspect_ratio = None;
                true
            }
            SV::Num(n) => {
                cs.aspect_ratio = if *n > 0.0 { Some(*n) } else { None };
                true
            }
            _ => false,
        },
        P::OverflowX | P::OverflowY => {
            let v = match sv {
                SV::Kw("visible") => Overflow::Visible,
                SV::Kw("hidden") => Overflow::Hidden,
                SV::Kw("clip") => Overflow::Clip,
                SV::Kw("scroll") => Overflow::Scroll,
                SV::Kw("auto") | SV::Kw("overlay") => Overflow::Auto,
                _ => return false,
            };
            if prop == P::OverflowX {
                cs.overflow_x = v
            } else {
                cs.overflow_y = v
            }
            true
        }
        P::FlexDirection => kw_map!(cs.flex_direction, {
            "row" => FlexDirection::Row, "row-reverse" => FlexDirection::RowReverse,
            "column" => FlexDirection::Column, "column-reverse" => FlexDirection::ColumnReverse,
        }),
        P::FlexWrap => kw_map!(cs.flex_wrap, {
            "nowrap" => FlexWrap::NoWrap, "wrap" => FlexWrap::Wrap, "wrap-reverse" => FlexWrap::WrapReverse,
        }),
        P::FlexGrow | P::FlexShrink => match sv {
            SV::Num(n) => {
                if prop == P::FlexGrow {
                    cs.flex_grow = *n
                } else {
                    cs.flex_shrink = *n
                }
                true
            }
            _ => false,
        },
        P::Order => match sv {
            SV::Int(n) => {
                cs.order = *n;
                true
            }
            _ => false,
        },
        P::AlignItems
        | P::AlignSelf
        | P::AlignContent
        | P::JustifyContent
        | P::JustifyItems
        | P::JustifySelf => {
            let SV::Kw(k) = sv else { return false };
            let v = if *k == "legacy" {
                Align::Normal
            } else {
                align(k)
            };
            match prop {
                P::AlignItems => cs.align_items = v,
                P::AlignSelf => cs.align_self = v,
                P::AlignContent => cs.align_content = v,
                P::JustifyContent => cs.justify_content = v,
                P::JustifyItems => cs.justify_items = v,
                _ => cs.justify_self = v,
            }
            true
        }
        P::RowGap | P::ColumnGap => {
            let v = match sv {
                SV::Kw(_) => None,
                SV::Len(e) => Some(e.compute(&cx)),
                _ => return false,
            };
            if prop == P::RowGap {
                cs.row_gap = v
            } else {
                cs.column_gap = v
            }
            true
        }
        P::GridTemplateColumns | P::GridTemplateRows => {
            let v = match sv {
                SV::Kw(_) => TrackList::default(),
                SV::Tracks(t) => track_list(t, &cx),
                _ => return false,
            };
            if prop == P::GridTemplateColumns {
                cs.grid_template_columns = Arc::new(v)
            } else {
                cs.grid_template_rows = Arc::new(v)
            }
            true
        }
        P::GridTemplateAreas => match sv {
            SV::Kw(_) => {
                cs.grid_template_areas = None;
                true
            }
            SV::Areas(a) => {
                cs.grid_template_areas = Some(a.clone());
                true
            }
            _ => false,
        },
        P::GridAutoFlow => match sv {
            SV::AutoFlow(f) => {
                cs.grid_auto_flow = *f;
                true
            }
            _ => false,
        },
        P::GridAutoColumns | P::GridAutoRows => match sv {
            SV::TrackSizes(t) => {
                let v = Arc::new(t.iter().map(|t| track_size(t, &cx)).collect());
                if prop == P::GridAutoColumns {
                    cs.grid_auto_columns = v
                } else {
                    cs.grid_auto_rows = v
                }
                true
            }
            _ => false,
        },
        P::GridRowStart | P::GridRowEnd | P::GridColumnStart | P::GridColumnEnd => match sv {
            SV::Line(l) => {
                match prop {
                    P::GridRowStart => cs.grid_row_start = l.clone(),
                    P::GridRowEnd => cs.grid_row_end = l.clone(),
                    P::GridColumnStart => cs.grid_column_start = l.clone(),
                    _ => cs.grid_column_end = l.clone(),
                }
                true
            }
            _ => false,
        },
        P::BackgroundColor => match color(sv, cs.color) {
            Some(c) => {
                cs.background_color = c;
                true
            }
            None => false,
        },
        P::BackgroundImage => {
            cs.background_image = match sv {
                SV::Str(url) => Some(url.clone()),
                _ => None,
            };
            true
        }
        P::Opacity => match sv {
            SV::Num(n) => {
                cs.opacity = *n;
                true
            }
            _ => false,
        },
        P::ZIndex => match sv {
            SV::Kw(_) => {
                cs.z_index = None;
                true
            }
            SV::Int(n) => {
                cs.z_index = Some(*n);
                true
            }
            _ => false,
        },
        P::VerticalAlign => match sv {
            SV::Len(e) => {
                let lh = match cs.line_height {
                    LineHeight::Normal => cs.font_size * 1.2,
                    LineHeight::Number(n) => n * cs.font_size,
                    LineHeight::Px(p) => p,
                };
                cs.vertical_align = VerticalAlign::Length(LengthPct::Px(e.compute_px(&cx, lh)));
                true
            }
            _ => kw_map!(cs.vertical_align, {
                "baseline" => VerticalAlign::Baseline, "sub" => VerticalAlign::Sub, "super" => VerticalAlign::Super,
                "text-top" => VerticalAlign::TextTop, "text-bottom" => VerticalAlign::TextBottom,
                "middle" => VerticalAlign::Middle, "top" => VerticalAlign::Top, "bottom" => VerticalAlign::Bottom,
            }),
        },
        P::TextOverflow => {
            kw_map!(cs.text_overflow, { "clip" => TextOverflow::Clip, "ellipsis" => TextOverflow::Ellipsis })
        }
        P::TextDecorationLine => match sv {
            SV::Decoration(d) => {
                cs.text_decoration = *d;
                true
            }
            _ => false,
        },
        P::TextDecorationStyle => true,
        P::TextDecorationColor => match sv {
            SV::Color(SColor::CurrentColor) => {
                cs.text_decoration_color = None;
                true
            }
            _ => match color(sv, cs.color) {
                Some(c) => {
                    cs.text_decoration_color = Some(c);
                    true
                }
                None => false,
            },
        },
        P::Transform => match sv {
            SV::Kw(_) => {
                cs.transform = None;
                true
            }
            SV::Transform(fns) => {
                cs.transform = Some(Arc::new(
                    fns.iter()
                        .map(|f| match f {
                            TransformSpec::Translate(x, y) => {
                                TransformFn::Translate(x.compute(&cx), y.compute(&cx))
                            }
                            TransformSpec::Scale(x, y) => TransformFn::Scale(*x, *y),
                            TransformSpec::Rotate(a) => TransformFn::Rotate(*a),
                            TransformSpec::Skew(a, b) => TransformFn::Skew(*a, *b),
                            TransformSpec::Matrix(m) => TransformFn::Matrix(*m),
                        })
                        .collect(),
                ));
                true
            }
            _ => false,
        },
        P::TransformOrigin => match sv {
            SV::Origin(a, b) => {
                cs.transform_origin = (a.compute(&cx), b.compute(&cx));
                true
            }
            _ => false,
        },
        P::OutlineWidth => match sv {
            SV::Len(e) => {
                cs.outline_width = e.compute_px(&cx, 0.0).max(0.0);
                true
            }
            _ => false,
        },
        P::OutlineStyle => match sv {
            SV::Kw(k) => {
                cs.outline_style = border_style(k);
                true
            }
            _ => false,
        },
        P::OutlineColor => match color(sv, cs.color) {
            Some(c) => {
                cs.outline_color = c;
                true
            }
            None => false,
        },
        P::OutlineOffset => match sv {
            SV::Len(e) => {
                cs.outline_offset = e.compute_px(&cx, 0.0);
                true
            }
            _ => false,
        },
        P::TableLayout => kw_map!(cs.table_layout_fixed, { "fixed" => true, "auto" => false }),
        P::ObjectFit => kw_map!(cs.object_fit, {
            "fill" => ObjectFit::Fill,
            "contain" => ObjectFit::Contain,
            "cover" => ObjectFit::Cover,
            "none" => ObjectFit::None,
            "scale-down" => ObjectFit::ScaleDown,
        }),
        P::ImageRendering => kw_map!(cs.image_rendering, {
            "auto" => ImageRendering::Smooth,
            "smooth" => ImageRendering::Smooth,
            "high-quality" => ImageRendering::Smooth,
            "pixelated" => ImageRendering::Pixelated,
            "crisp-edges" => ImageRendering::Pixelated,
        }),
        P::Content => match sv {
            SV::Kw(_) => {
                cs.content = None;
                true
            }
            SV::Content(items) => {
                // attr() is resolved now, against the originating element.
                let resolved: Vec<ContentItem> = items
                    .iter()
                    .map(|i| match i {
                        ContentItem::Attr(name) => {
                            ContentItem::Text(Arc::from(attr(name).unwrap_or_default().as_str()))
                        }
                        t => t.clone(),
                    })
                    .collect();
                cs.content = Some(Arc::new(resolved));
                true
            }
            _ => false,
        },
        P::TransitionProperty
        | P::TransitionDuration
        | P::TransitionTimingFunction
        | P::TransitionDelay => {
            let mut list: Vec<Transition> = cs.transitions.as_deref().cloned().unwrap_or_default();
            let make = || Transition {
                property: Arc::from("all"),
                duration: 0.0,
                delay: 0.0,
                easing: Easing::default(),
            };
            match (prop, sv) {
                (P::TransitionProperty, SV::Idents(v)) => {
                    list.truncate(v.len().max(if v.is_empty() { 0 } else { 1 }));
                    set_list(&mut list, v.len(), make, |t, i| {
                        t.property = v[i % v.len()].clone()
                    });
                }
                (P::TransitionDuration, SV::Times(v)) => {
                    set_list(&mut list, 1, make, |t, i| t.duration = v[i % v.len()])
                }
                (P::TransitionDelay, SV::Times(v)) => {
                    set_list(&mut list, 1, make, |t, i| t.delay = v[i % v.len()])
                }
                (P::TransitionTimingFunction, SV::Easings(v)) => {
                    set_list(&mut list, 1, make, |t, i| t.easing = v[i % v.len()])
                }
                _ => return false,
            }
            cs.transitions = if list.is_empty() {
                None
            } else {
                Some(Arc::new(list))
            };
            true
        }
        P::AnimationName
        | P::AnimationDuration
        | P::AnimationTimingFunction
        | P::AnimationDelay
        | P::AnimationIterationCount
        | P::AnimationDirection
        | P::AnimationFillMode
        | P::AnimationPlayState => {
            let mut list: Vec<Animation> = cs.animations.as_deref().cloned().unwrap_or_default();
            let make = || Animation {
                name: Arc::from("none"),
                duration: 0.0,
                delay: 0.0,
                easing: Easing::default(),
                iterations: 1.0,
                direction: Arc::from("normal"),
                fill_mode: Arc::from("none"),
                paused: false,
            };
            match (prop, sv) {
                (P::AnimationName, SV::Idents(v)) => {
                    list.truncate(v.len());
                    set_list(&mut list, v.len(), make, |a, i| {
                        a.name = v[i % v.len()].clone()
                    });
                }
                (P::AnimationDuration, SV::Times(v)) => {
                    set_list(&mut list, 1, make, |a, i| a.duration = v[i % v.len()])
                }
                (P::AnimationDelay, SV::Times(v)) => {
                    set_list(&mut list, 1, make, |a, i| a.delay = v[i % v.len()])
                }
                (P::AnimationIterationCount, SV::Times(v)) => {
                    set_list(&mut list, 1, make, |a, i| a.iterations = v[i % v.len()])
                }
                (P::AnimationTimingFunction, SV::Easings(v)) => {
                    set_list(&mut list, 1, make, |a, i| a.easing = v[i % v.len()])
                }
                (P::AnimationDirection, SV::Idents(v)) => set_list(&mut list, 1, make, |a, i| {
                    a.direction = v[i % v.len()].clone()
                }),
                (P::AnimationFillMode, SV::Idents(v)) => set_list(&mut list, 1, make, |a, i| {
                    a.fill_mode = v[i % v.len()].clone()
                }),
                (P::AnimationPlayState, SV::Idents(v)) => set_list(&mut list, 1, make, |a, i| {
                    a.paused = &*v[i % v.len()] == "paused"
                }),
                _ => return false,
            }
            list.retain(|a| &*a.name != "none");
            cs.animations = if list.is_empty() {
                None
            } else {
                Some(Arc::new(list))
            };
            true
        }
        P::LineClamp => match sv {
            SV::Kw(_) => {
                cs.line_clamp = None;
                true
            }
            SV::Int(n) => {
                cs.line_clamp = Some(*n as u32);
                true
            }
            _ => false,
        },
    }
}

fn cs_dark(parent: &ComputedStyle, env: &ComputeEnv) -> bool {
    parent.color_scheme_dark.unwrap_or(env.dark)
}
