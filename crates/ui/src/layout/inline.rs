//! Inline formatting contexts: Parley breaks lines and places glyphs horizontally; we build the
//! text with CSS white-space processing, represent atomic inlines and inline box edges
//! (margin + border + padding) as Parley inline boxes, and do the vertical part ourselves (line
//! box heights, half-leading, vertical-align, struts) the way WebKit does.

use crate::document::Document;
use crate::dom::{Inner, NodeId, NodeKind, Rect, flags};
use crate::style::*;
use crate::text::{FontMetrics, TextBrush, text_style};

/// Font size of edge runs: small enough that the no-break space itself adds nothing visible.
const EDGE_FONT_SIZE: f32 = 0.01;

/// The character standing for an inline box edge: U+2011 NON-BREAKING HYPHEN is line-break class
/// GL (no break after it, none before it unless after a space) and, unlike a no-break space, is not
/// whitespace, so it never hangs past the end of a line.
const EDGE_CHAR: &str = "\u{2011}";
use std::sync::Arc;
use taffy::{
    AvailableSpace, CollapsibleMarginSet, LayoutInput, LayoutOutput, LayoutPartialTree, MaybeMath,
    MaybeResolve, ResolveOrZero, RunMode, Size, SizingMode,
};

/// An inline box (the formatting context root is owner 0).
#[derive(Debug, Clone)]
pub(crate) struct Owner {
    pub node: NodeId,
    pub parent: u32,
    pub style: Arc<ComputedStyle>,
    pub metrics: FontMetrics,
    pub margin: [f32; 4],
    pub border: [f32; 4],
    pub padding: [f32; 4],
}

impl Owner {
    fn left_edge(&self) -> f32 {
        self.margin[3] + self.border[3] + self.padding[3]
    }
    fn right_edge(&self) -> f32 {
        self.margin[1] + self.border[1] + self.padding[1]
    }
}

/// The start or end edge (margin + border + padding) of an inline box. Edges are laid out as a
/// non-breaking hyphen whose letter-spacing gives it the edge's width, so that line breaking treats
/// them like CSS does: no break opportunity between an edge and the text it touches.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Edge {
    pub owner: u32,
    pub start: bool,
    pub width: f32,
}

#[derive(Debug, Clone)]
pub(crate) struct IBox {
    /// The atomic inline (inline-block, inline-flex, …).
    pub node: NodeId,
    /// The inline box this one sits in.
    pub owner: u32,
    pub index: usize,
    pub width: f32,
    /// For atoms: margin-box height and baseline offset from the margin-box top.
    pub height: f32,
    pub baseline: f32,
    pub margin_left: f32,
    pub margin_top: f32,
}

/// One laid-out line, relative to the content box.
#[derive(Debug, Clone, Copy)]
pub(crate) struct LineInfo {
    pub top: f32,
    pub height: f32,
    /// Baseline offset from the line top.
    pub baseline: f32,
}

/// The inline content of a formatting context root.
pub(crate) struct InlineLayout {
    pub text: String,
    pub layout: parley::Layout<TextBrush>,
    pub owners: Vec<Owner>,
    pub boxes: Vec<IBox>,
    pub edges: Vec<Edge>,
    pub lines: Vec<LineInfo>,
    /// Vertical shift of each owner's baseline from the line's baseline, for the last layout.
    pub shifts: Vec<Vec<f32>>,
    /// The content box origin within the border box.
    pub content_origin: (f32, f32),
    pub width: f32,
    pub height: f32,
    /// Under `text-overflow: ellipsis`: the font (and size) the root's own style gives "…",
    /// for lines where no run's font has it.
    pub ellipsis_font: Option<(parley::FontData, f32)>,
}

impl std::fmt::Debug for InlineLayout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InlineLayout")
            .field("text", &self.text)
            .field("lines", &self.lines.len())
            .finish()
    }
}

/// A style run: an inline box's own style, or one of the edges.
#[derive(Clone, Copy, PartialEq)]
enum RunStyle {
    Owner(u32),
    Edge(u32),
}

struct Builder {
    text: String,
    spans: Vec<(usize, usize, RunStyle)>,
    owners: Vec<Owner>,
    boxes: Vec<IBox>,
    edges: Vec<Edge>,
    prev_space: bool,
}

impl Builder {
    fn push_run(&mut self, style: RunStyle, s: &str) {
        if s.is_empty() {
            return;
        }
        let start = self.text.len();
        self.text.push_str(s);
        match self.spans.last_mut() {
            Some(last)
                if last.2 == style && last.1 == start && matches!(style, RunStyle::Owner(_)) =>
            {
                last.1 = self.text.len()
            }
            _ => self.spans.push((start, self.text.len(), style)),
        }
    }
    fn push_text(&mut self, owner: u32, s: &str) {
        self.push_run(RunStyle::Owner(owner), s);
    }
    fn push_edge(&mut self, owner: u32, start: bool, width: f32) {
        let k = self.edges.len() as u32;
        self.edges.push(Edge {
            owner,
            start,
            width,
        });
        self.push_run(RunStyle::Edge(k), EDGE_CHAR);
    }
}

fn transform(t: &str, tt: TextTransform) -> String {
    match tt {
        TextTransform::None => t.to_string(),
        TextTransform::Uppercase => t.to_uppercase(),
        TextTransform::Lowercase => t.to_lowercase(),
        TextTransform::Capitalize => {
            let mut out = String::with_capacity(t.len());
            let mut start = true;
            for c in t.chars() {
                if start && c.is_alphabetic() {
                    out.extend(c.to_uppercase());
                    start = false;
                } else {
                    if c.is_whitespace() {
                        start = true;
                    }
                    out.push(c);
                }
            }
            out
        }
    }
}

fn edges(s: &ComputedStyle, basis: f32) -> ([f32; 4], [f32; 4], [f32; 4]) {
    let m = [
        s.margin[0].resolve_or_zero(basis),
        s.margin[1].resolve_or_zero(basis),
        s.margin[2].resolve_or_zero(basis),
        s.margin[3].resolve_or_zero(basis),
    ];
    let p = [
        s.padding[0].resolve(basis),
        s.padding[1].resolve(basis),
        s.padding[2].resolve(basis),
        s.padding[3].resolve(basis),
    ];
    (m, s.used_border(), p)
}

impl Document {
    fn ifc_build(&mut self, root: NodeId, basis: f32) -> InlineLayout {
        let root_style = self
            .node(root)
            .style
            .clone()
            .unwrap_or_else(|| self.initial.clone());
        let metrics = self.fonts.metrics(&root_style);
        let mut b = Builder {
            text: String::new(),
            spans: Vec::new(),
            owners: vec![Owner {
                node: root,
                parent: 0,
                style: root_style.clone(),
                metrics,
                margin: [0.0; 4],
                border: [0.0; 4],
                padding: [0.0; 4],
            }],
            boxes: Vec::new(),
            edges: Vec::new(),
            prev_space: true,
        };
        for n in self.box_children(root) {
            self.ifc_walk(n, 0, basis, &mut b);
        }
        // A collapsible space at the very end is removed.
        if b.text.ends_with(' ')
            && b.owners
                .iter()
                .any(|o| o.style.white_space.collapses_spaces())
        {
            let last_owner = match b.spans.last().map(|s| s.2) {
                Some(RunStyle::Owner(o)) => o,
                _ => u32::MAX,
            };
            if last_owner != u32::MAX
                && b.owners[last_owner as usize]
                    .style
                    .white_space
                    .collapses_spaces()
            {
                b.text.pop();
                if let Some(last) = b.spans.last_mut() {
                    last.1 -= 1;
                    if last.1 == last.0 {
                        b.spans.pop();
                    }
                }
                let len = b.text.len();
                for bx in &mut b.boxes {
                    bx.index = bx.index.min(len);
                }
            }
        }
        // Parley needs contiguous style runs covering the text.
        let mut layout = parley::Layout::new();
        let text = b.text.clone();
        {
            let fonts = &mut self.fonts;
            let mut builder =
                fonts
                    .layout_cx
                    .style_run_builder(&mut fonts.font_cx, &text, 1.0, false);
            for (i, o) in b.owners.iter().enumerate() {
                builder.push_style(text_style(
                    &o.style,
                    TextBrush {
                        owner: i as u32,
                        edge: 0,
                    },
                ));
            }
            for (k, e) in b.edges.iter().enumerate() {
                let mut ts = text_style(
                    &b.owners[e.owner as usize].style,
                    TextBrush {
                        owner: e.owner,
                        edge: k as u32 + 1,
                    },
                );
                ts.font_size = EDGE_FONT_SIZE;
                ts.letter_spacing = e.width;
                builder.push_style(ts);
            }
            let n_owners = b.owners.len();
            let mut cursor = 0;
            for (s, e, st) in &b.spans {
                if *s > cursor {
                    builder.push_style_run(0, cursor..*s);
                }
                let idx = match st {
                    RunStyle::Owner(o) => *o as usize,
                    RunStyle::Edge(k) => n_owners + *k as usize,
                };
                builder.push_style_run(idx as u16, *s..*e);
                cursor = *e;
            }
            if cursor < text.len() {
                builder.push_style_run(0, cursor..text.len());
            }
            for (i, bx) in b.boxes.iter().enumerate() {
                builder.push_inline_box(parley::InlineBox {
                    id: i as u64,
                    kind: parley::InlineBoxKind::InFlow,
                    index: bx.index,
                    width: 0.0,
                    height: 0.0,
                });
            }
            builder.build_into(&mut layout, &text);
        }
        let ellipsis_font = (root_style.text_overflow == crate::style::TextOverflow::Ellipsis)
            .then(|| self.fonts.ellipsis_font(&root_style))
            .flatten();
        InlineLayout {
            text: b.text,
            layout,
            owners: b.owners,
            boxes: b.boxes,
            edges: b.edges,
            lines: Vec::new(),
            shifts: Vec::new(),
            content_origin: (0.0, 0.0),
            width: 0.0,
            height: 0.0,
            ellipsis_font,
        }
    }

    fn ifc_walk(&mut self, n: NodeId, owner: u32, basis: f32, b: &mut Builder) {
        let node = self.node(n);
        match &node.kind {
            NodeKind::Text(t) => {
                let style = b.owners[owner as usize].style.clone();
                let t = transform(t, style.text_transform);
                let ws = style.white_space;
                let mut out = String::with_capacity(t.len());
                for ch in t.chars() {
                    match ch {
                        '\r' => {}
                        '\n' if ws.preserves_newlines() => {
                            if ws.collapses_spaces() && out.ends_with(' ') {
                                out.pop();
                            }
                            out.push('\n');
                            b.prev_space = ws.collapses_spaces();
                        }
                        ' ' | '\t' | '\n' | '\x0c' if ws.collapses_spaces() => {
                            if !b.prev_space && !(out.is_empty() && b.text.ends_with('\n')) {
                                out.push(' ');
                            }
                            b.prev_space = true;
                        }
                        c => {
                            out.push(c);
                            b.prev_space = false;
                        }
                    }
                }
                b.push_text(owner, &out);
            }
            NodeKind::Element(_) | NodeKind::Pseudo(_) => {
                let Some(style) = node.style.clone() else {
                    return;
                };
                if style.display == Display::None || style.is_out_of_flow() {
                    return;
                }
                match node.inner {
                    Inner::InlineBox | Inner::Contents => {
                        let (m, bo, p) = edges(&style, basis);
                        let metrics = self.fonts.metrics(&style);
                        let idx = b.owners.len() as u32;
                        b.owners.push(Owner {
                            node: n,
                            parent: owner,
                            style: style.clone(),
                            metrics,
                            margin: m,
                            border: bo,
                            padding: p,
                        });
                        let left = b.owners[idx as usize].left_edge();
                        let right = b.owners[idx as usize].right_edge();
                        if left != 0.0 {
                            b.push_edge(idx, true, left);
                        }
                        for c in self.box_children(n) {
                            self.ifc_walk(c, idx, basis, b);
                        }
                        if right != 0.0 {
                            b.push_edge(idx, false, right);
                        }
                    }
                    _ => {
                        // Atomic inline.
                        b.boxes.push(IBox {
                            node: n,
                            owner,
                            index: b.text.len(),
                            width: 0.0,
                            height: 0.0,
                            baseline: 0.0,
                            margin_left: 0.0,
                            margin_top: 0.0,
                        });
                        b.prev_space = false;
                    }
                }
            }
            NodeKind::Anonymous => {}
        }
    }

    /// The last baseline of a box, from its border-box top, for inline-block alignment.
    pub(crate) fn last_baseline(&self, id: NodeId) -> Option<f32> {
        let n = self.node(id);
        let s = n.style.as_ref()?;
        if s.is_scroll_container() && s.display == Display::InlineBlock {
            return None;
        }
        match n.inner {
            Inner::Ifc => {
                let il = n.inline.as_ref()?;
                let last = il.lines.iter().rev().find(|l| l.height > 0.0)?;
                Some(il.content_origin.1 + last.top + last.baseline)
            }
            Inner::Flow => {
                for &c in n.layout_children.iter().rev() {
                    let cn = self.node(c);
                    if cn.taffy_style.position == taffy::Position::Absolute || cn.skipped {
                        continue;
                    }
                    if let Some(b) = self.last_baseline(c) {
                        return Some(cn.layout.location.y + b);
                    }
                }
                None
            }
            Inner::Flex | Inner::Grid | Inner::Table => self.first_baseline(id),
            _ => None,
        }
    }

    pub(crate) fn first_baseline(&self, id: NodeId) -> Option<f32> {
        let n = self.node(id);
        match n.inner {
            Inner::Ifc => {
                let il = n.inline.as_ref()?;
                let first = il.lines.iter().find(|l| l.height > 0.0)?;
                Some(il.content_origin.1 + first.top + first.baseline)
            }
            // CSS Flexbox §8.5, as WebKit does it: the baseline of the first item that takes part
            // in baseline alignment, else of the startmost item, synthesized from its border box
            // when it has none.
            Inner::Flex => {
                let items: Vec<NodeId> = n
                    .layout_children
                    .iter()
                    .copied()
                    .filter(|&c| {
                        let cn = self.node(c);
                        cn.taffy_style.position != taffy::Position::Absolute && !cn.skipped
                    })
                    .collect();
                let row = matches!(
                    n.taffy_style.flex_direction,
                    taffy::FlexDirection::Row | taffy::FlexDirection::RowReverse
                );
                let aligns_baseline = |c: NodeId| {
                    let align = self
                        .node(c)
                        .taffy_style
                        .align_self
                        .or(n.taffy_style.align_items);
                    row && align == Some(taffy::AlignItems::BASELINE)
                };
                let item = items
                    .iter()
                    .copied()
                    .find(|&c| aligns_baseline(c))
                    .or_else(|| items.first().copied())?;
                let cn = self.node(item);
                let own = self.first_baseline(item).unwrap_or(cn.layout.size.height);
                Some(cn.layout.location.y + own)
            }
            _ => {
                for &c in &n.layout_children {
                    let cn = self.node(c);
                    if cn.taffy_style.position == taffy::Position::Absolute || cn.skipped {
                        continue;
                    }
                    if let Some(b) = self.first_baseline(c) {
                        return Some(cn.layout.location.y + b);
                    }
                }
                None
            }
        }
    }

    /// Lay out an inline formatting context root.
    pub(crate) fn compute_inline_layout(
        &mut self,
        id: NodeId,
        inputs: LayoutInput,
    ) -> LayoutOutput {
        let LayoutInput {
            known_dimensions,
            parent_size,
            available_space,
            sizing_mode,
            run_mode,
            ..
        } = inputs;
        let style = self
            .node(id)
            .style
            .clone()
            .unwrap_or_else(|| self.initial.clone());
        let ts = self.node(id).taffy_style.clone();
        let calc = |p: *const (), b: f32| crate::layout::resolve_calc(p, b);
        let margin = ts.margin.resolve_or_zero(parent_size.width, calc);
        let padding = ts.padding.resolve_or_zero(parent_size.width, calc);
        let border = ts.border.resolve_or_zero(parent_size.width, calc);
        let pb = padding + border;
        let pb_sum = pb.sum_axes();
        let box_sizing_adj = if style.box_sizing == crate::style::BoxSizing::ContentBox {
            pb_sum
        } else {
            Size::ZERO
        };

        let (node_size, min_size, max_size) = match sizing_mode {
            SizingMode::ContentSize => (known_dimensions, Size::NONE, Size::NONE),
            SizingMode::InherentSize => {
                let size = ts
                    .size
                    .maybe_resolve(parent_size, calc)
                    .maybe_add(box_sizing_adj);
                let min = ts
                    .min_size
                    .maybe_resolve(parent_size, calc)
                    .maybe_add(box_sizing_adj);
                let max = ts
                    .max_size
                    .maybe_resolve(parent_size, calc)
                    .maybe_add(box_sizing_adj);
                (known_dimensions.or(size.maybe_clamp(min, max)), min, max)
            }
        };
        let node_size = node_size.maybe_max(pb_sum.map(Some));

        if run_mode == RunMode::ComputeSize
            && let Size {
                width: Some(w),
                height: Some(h),
            } = node_size
        {
            return LayoutOutput::from_outer_size(Size {
                width: w,
                height: h,
            });
        }

        // (Re)build the inline content if needed.
        let dirty = self.node(id).flags & flags::IFC_DIRTY != 0 || self.node(id).inline.is_none();
        let mut il = if dirty {
            self.stats.inline_rebuilt += 1;
            let il = self.ifc_build(id, parent_size.width.unwrap_or(0.0));
            self.node_mut(id).flags &= !flags::IFC_DIRTY;
            il
        } else {
            *self.node_mut(id).inline.take().expect("inline layout")
        };

        // Measure atomic inlines.
        let inner_avail = |avail: AvailableSpace| -> AvailableSpace {
            match avail {
                AvailableSpace::Definite(a) => AvailableSpace::Definite(
                    (a - margin.horizontal_axis_sum() - pb.horizontal_axis_sum()).max(0.0),
                ),
                other => other,
            }
        };
        let child_avail_w = match node_size.width {
            Some(w) => AvailableSpace::Definite((w - pb.horizontal_axis_sum()).max(0.0)),
            None => inner_avail(available_space.width),
        };
        let child_parent_w = match child_avail_w {
            AvailableSpace::Definite(w) => Some(w),
            _ => None,
        };
        for i in 0..il.boxes.len() {
            {
                let a = il.boxes[i].node;
                let astyle = self
                    .node(a)
                    .style
                    .clone()
                    .unwrap_or_else(|| self.initial.clone());
                let (m, _, _) = edges(&astyle, child_parent_w.unwrap_or(0.0));
                let out = self.compute_child_layout(
                    a.taffy(),
                    LayoutInput {
                        known_dimensions: Size::NONE,
                        known_dimensions_are_definite: Size {
                            width: true,
                            height: true,
                        },
                        parent_size: Size {
                            width: child_parent_w,
                            height: None,
                        },
                        available_space: Size {
                            width: child_avail_w,
                            height: AvailableSpace::MaxContent,
                        },
                        sizing_mode: SizingMode::InherentSize,
                        axis: taffy::RequestedAxis::Both,
                        run_mode: RunMode::PerformLayout,
                        vertical_margins_are_collapsible: taffy::Line::FALSE,
                    },
                );
                self.store_layout(a, 0.0, 0.0, out.size, child_parent_w.unwrap_or(0.0));
                let bx = &mut il.boxes[i];
                bx.width = (out.size.width + m[1] + m[3]).max(0.0);
                bx.height = (out.size.height + m[0] + m[2]).max(0.0);
                bx.margin_left = m[3];
                bx.margin_top = m[0];
                let base = self.last_baseline(a);
                bx.baseline = match base {
                    Some(b) => m[0] + b,
                    None => bx.height,
                };
            }
        }
        for (i, b) in il.layout.inline_boxes_mut().iter_mut().enumerate() {
            b.width = il.boxes[i].width;
            b.height = 0.0;
        }

        // Line breaking width.
        let inner_width = node_size
            .width
            .map(|w| (w - pb.horizontal_axis_sum()).max(0.0));
        let content_width = match inner_width {
            Some(w) => w,
            None => {
                let cw = il.layout.calculate_content_widths();
                let w = match available_space.width {
                    AvailableSpace::MinContent => cw.min,
                    AvailableSpace::MaxContent => cw.max,
                    AvailableSpace::Definite(a) => {
                        let avail =
                            (a - margin.horizontal_axis_sum() - pb.horizontal_axis_sum()).max(0.0);
                        avail.min(cw.max).max(cw.min)
                    }
                };
                let outer =
                    (w + pb.horizontal_axis_sum()).maybe_clamp(min_size.width, max_size.width);
                (outer - pb.horizontal_axis_sum()).max(0.0)
            }
        };
        // A little slack so that content exactly as wide as the line doesn't wrap from rounding.
        il.layout.break_all_lines(Some(content_width + 0.01));
        let alignment = match style.text_align {
            TextAlign::Center => parley::Alignment::Center,
            TextAlign::Right | TextAlign::End => parley::Alignment::Right,
            TextAlign::Justify => parley::Alignment::Justify,
            _ => parley::Alignment::Left,
        };
        il.layout.align(
            alignment,
            parley::AlignmentOptions {
                align_when_overflowing: false,
            },
        );

        self.ifc_vertical(&mut il);
        let mut content_height = il.lines.last().map(|l| l.top + l.height).unwrap_or(0.0);
        if let Some(n) = style.line_clamp {
            let mut count = 0;
            for l in &il.lines {
                if l.height > 0.0 {
                    count += 1;
                    if count == n {
                        content_height = l.top + l.height;
                        break;
                    }
                }
            }
        }
        let outer_w = content_width + pb.horizontal_axis_sum();
        let outer_h = node_size
            .height
            .unwrap_or_else(|| {
                (content_height + pb.vertical_axis_sum())
                    .maybe_clamp(min_size.height, max_size.height)
            })
            .max(pb.vertical_axis_sum());
        let size = Size {
            width: node_size.width.unwrap_or(outer_w),
            height: outer_h,
        };
        il.content_origin = (pb.left, pb.top);
        il.width = content_width;
        il.height = content_height;

        if run_mode == RunMode::PerformLayout {
            self.ifc_place(id, &mut il);
        }
        let first_baseline = il
            .lines
            .iter()
            .find(|l| l.height > 0.0)
            .map(|l| pb.top + l.top + l.baseline);
        let has_lines = il.lines.iter().any(|l| l.height > 0.0);
        self.node_mut(id).inline = Some(Box::new(il));

        if run_mode == RunMode::PerformLayout {
            // Absolutely positioned children whose containing block is this box.
            let kids = self.node(id).layout_children.clone();
            for c in kids {
                if self.node(c).taffy_style.position == taffy::Position::Absolute {
                    crate::layout::layout_abspos(self, id, c, size, border);
                }
            }
        }

        let can_collapse_through =
            !has_lines && pb.vertical_axis_sum() == 0.0 && node_size.height.unwrap_or(0.0) == 0.0;
        LayoutOutput {
            size,
            scrollable_overflow_rect: taffy::Rect {
                left: 0.0,
                top: 0.0,
                right: content_width + pb.left,
                bottom: content_height + pb.top,
            },
            baselines: taffy::Baselines {
                first: first_baseline,
                last: None,
            },
            top_margin: CollapsibleMarginSet::ZERO,
            bottom_margin: CollapsibleMarginSet::ZERO,
            margins_can_collapse_through: can_collapse_through,
        }
    }

    /// Line box heights and baselines (CSS 2.1 §10.8, with WebKit's rounding).
    fn ifc_vertical(&mut self, il: &mut InlineLayout) {
        let owners = &il.owners;
        // Each inline box's layout ascent and descent around its baseline, from its line-height.
        let bounds: Vec<(f32, f32)> = owners
            .iter()
            .map(|o| {
                let m = &o.metrics;
                let lh = match o.style.line_height {
                    LineHeight::Normal => m.line_spacing(),
                    LineHeight::Number(n) => n * o.style.font_size,
                    LineHeight::Px(p) => p,
                };
                // Layout bounds use the rounded metrics.
                let (a, d) = (m.ascent.round(), m.descent.round());
                let half = (lh - (a + d)) / 2.0;
                ((a + half).floor(), lh - (a + half).floor())
            })
            .collect();
        // Baseline shift of each owner relative to its parent (positive is down).
        let own_shift: Vec<f32> = owners
            .iter()
            .enumerate()
            .map(|(i, o)| {
                if i == 0 {
                    return 0.0;
                }
                let parent = &owners[o.parent as usize];
                let pm = &parent.metrics;
                match &o.style.vertical_align {
                    VerticalAlign::Baseline | VerticalAlign::Top | VerticalAlign::Bottom => 0.0,
                    VerticalAlign::Sub => parent.style.font_size / 5.0 + 1.0,
                    VerticalAlign::Super => -(parent.style.font_size / 3.0 + 1.0),
                    VerticalAlign::TextTop => -(pm.ascent - o.metrics.ascent),
                    VerticalAlign::TextBottom => pm.descent - o.metrics.descent,
                    VerticalAlign::Middle => {
                        let mid = (o.metrics.ascent - o.metrics.descent) / 2.0;
                        -(pm.x_height / 2.0) + mid
                    }
                    VerticalAlign::Length(l) => -l.resolve(0.0),
                }
            })
            .collect();
        let mut shift = vec![0.0f32; owners.len()];
        for i in 1..owners.len() {
            shift[i] = shift[owners[i].parent as usize] + own_shift[i];
        }
        let ancestors = |mut o: u32, present: &mut Vec<bool>| {
            while o != 0 && !present[o as usize] {
                present[o as usize] = true;
                o = owners[o as usize].parent;
            }
        };

        let mut lines = Vec::new();
        let mut y = 0.0f32;
        for line in il.layout.lines() {
            let mut present = vec![false; owners.len()];
            let mut has_content = il
                .text
                .get(line.text_range())
                .unwrap_or("")
                .chars()
                .any(|c| !c.is_whitespace());
            let mut atoms: Vec<usize> = Vec::new();
            for item in line.items() {
                match item {
                    parley::PositionedLayoutItem::GlyphRun(g) => {
                        let brush = g.style().brush;
                        ancestors(brush.owner, &mut present);
                        if brush.edge != 0 && il.edges[brush.edge as usize - 1].width > 0.0 {
                            has_content = true;
                        }
                    }
                    parley::PositionedLayoutItem::InlineBox(b) => {
                        let bx = &il.boxes[b.id as usize];
                        ancestors(bx.owner, &mut present);
                        atoms.push(b.id as usize);
                        has_content = true;
                    }
                }
            }
            if !has_content {
                lines.push(LineInfo {
                    top: y,
                    height: 0.0,
                    baseline: 0.0,
                });
                continue;
            }
            // Root strut, then each inline box present, then atoms. (top, bottom) around the baseline.
            let mut top = -bounds[0].0;
            let mut bottom = bounds[0].1;
            let mut deferred: Vec<(f32, bool)> = Vec::new();
            for (i, p) in present.iter().enumerate().skip(1) {
                if !p {
                    continue;
                }
                match owners[i].style.vertical_align {
                    VerticalAlign::Top | VerticalAlign::Bottom => {
                        deferred.push((
                            bounds[i].0 + bounds[i].1,
                            owners[i].style.vertical_align == VerticalAlign::Top,
                        ));
                    }
                    _ => {
                        top = top.min(shift[i] - bounds[i].0);
                        bottom = bottom.max(shift[i] + bounds[i].1);
                    }
                }
            }
            for &a in &atoms {
                let bx = &il.boxes[a];
                let node = bx.node;
                let va = self
                    .node(node)
                    .style
                    .as_ref()
                    .map(|s| s.vertical_align.clone())
                    .unwrap_or(VerticalAlign::Baseline);
                let parent = &owners[bx.owner as usize];
                let base_shift = shift[bx.owner as usize];
                let (asc, desc) = (bx.baseline, bx.height - bx.baseline);
                let s = match va {
                    VerticalAlign::Top | VerticalAlign::Bottom => {
                        deferred.push((bx.height, va == VerticalAlign::Top));
                        continue;
                    }
                    VerticalAlign::Middle => {
                        let mid_from_top = bx.height / 2.0;
                        // Place the box's middle at the parent's baseline minus half its x-height.
                        base_shift - parent.metrics.x_height / 2.0 - mid_from_top + asc
                    }
                    VerticalAlign::Sub => base_shift + (parent.style.font_size / 5.0 + 1.0),
                    VerticalAlign::Super => base_shift - (parent.style.font_size / 3.0 + 1.0),
                    VerticalAlign::TextTop => base_shift - parent.metrics.ascent + asc,
                    VerticalAlign::TextBottom => base_shift + parent.metrics.descent - desc,
                    VerticalAlign::Length(l) => base_shift - l.resolve(0.0),
                    VerticalAlign::Baseline => base_shift,
                };
                top = top.min(s - asc);
                bottom = bottom.max(s + desc);
            }
            for (h, _) in &deferred {
                if bottom - top < *h {
                    bottom = top + h;
                }
            }
            let height = bottom - top;
            lines.push(LineInfo {
                top: y,
                height,
                baseline: -top,
            });
            y += height;
        }
        il.lines = lines;
        il.shifts = vec![shift];
    }

    /// Place atoms and record inline box fragments (perform-layout pass).
    fn ifc_place(&mut self, root: NodeId, il: &mut InlineLayout) {
        let (ox, oy) = il.content_origin;
        let shift = il.shifts.first().cloned().unwrap_or_default();
        let mut frags: Vec<Vec<Rect>> = vec![Vec::new(); il.owners.len()];
        let mut atom_pos: Vec<(NodeId, f32, f32)> = Vec::new();
        for (li, line) in il.layout.lines().enumerate() {
            let info = il.lines[li];
            if info.height == 0.0
                && il
                    .text
                    .get(line.text_range())
                    .unwrap_or("")
                    .trim()
                    .is_empty()
            {
                // Lines without content still give empty inline boxes a position.
            }
            let baseline_y = oy + info.top + info.baseline;
            let mut ext: Vec<Option<(f32, f32)>> = vec![None; il.owners.len()];
            let trailing = line.metrics().trailing_whitespace;
            let items: Vec<_> = line.items().collect();
            let last_run = items
                .iter()
                .rposition(|i| matches!(i, parley::PositionedLayoutItem::GlyphRun(_)));
            for (k, item) in items.iter().enumerate() {
                let (owner, x0, x1, start_of, end_of) = match item {
                    parley::PositionedLayoutItem::GlyphRun(g) => {
                        let brush = g.style().brush;
                        if brush.edge != 0 {
                            let e = il.edges[brush.edge as usize - 1];
                            let ow = &il.owners[e.owner as usize];
                            let (a, b) = (g.offset(), g.offset() + g.advance());
                            if e.start {
                                (ow.parent, a + ow.margin[3], b, Some(e.owner), None)
                            } else {
                                (ow.parent, a, b - ow.margin[1], None, Some(e.owner))
                            }
                        } else {
                            let mut x1 = g.offset() + g.advance();
                            if Some(k) == last_run && k + 1 == items.len() {
                                x1 -= trailing;
                            }
                            (brush.owner, g.offset(), x1, None, None)
                        }
                    }
                    parley::PositionedLayoutItem::InlineBox(b) => {
                        let bx = &il.boxes[b.id as usize];
                        let node = bx.node;
                        let va = self
                            .node(node)
                            .style
                            .as_ref()
                            .map(|s| s.vertical_align.clone())
                            .unwrap_or(VerticalAlign::Baseline);
                        let parent = &il.owners[bx.owner as usize];
                        let base_shift = shift.get(bx.owner as usize).copied().unwrap_or(0.0);
                        let asc = bx.baseline;
                        let desc = bx.height - bx.baseline;
                        let top = match va {
                            VerticalAlign::Top => oy + info.top,
                            VerticalAlign::Bottom => oy + info.top + info.height - bx.height,
                            VerticalAlign::Middle => {
                                baseline_y + base_shift
                                    - parent.metrics.x_height / 2.0
                                    - bx.height / 2.0
                            }
                            VerticalAlign::Sub => {
                                baseline_y + base_shift + (parent.style.font_size / 5.0 + 1.0) - asc
                            }
                            VerticalAlign::Super => {
                                baseline_y + base_shift - (parent.style.font_size / 3.0 + 1.0) - asc
                            }
                            VerticalAlign::TextTop => {
                                baseline_y + base_shift - parent.metrics.ascent
                            }
                            VerticalAlign::TextBottom => {
                                baseline_y + base_shift + parent.metrics.descent - desc - asc
                            }
                            VerticalAlign::Length(l) => {
                                baseline_y + base_shift - l.resolve(0.0) - asc
                            }
                            VerticalAlign::Baseline => baseline_y + base_shift - asc,
                        };
                        atom_pos.push((node, ox + b.x + bx.margin_left, top + bx.margin_top));
                        (bx.owner, b.x, b.x + b.width, None, None)
                    }
                };
                let mut extend = |o: u32, a: f32, b: f32| {
                    let e = &mut ext[o as usize];
                    *e = Some(match *e {
                        Some((p, q)) => (p.min(a), q.max(b)),
                        None => (a, b),
                    });
                };
                if let Some(o) = start_of {
                    extend(o, x0, x1);
                }
                if let Some(o) = end_of {
                    extend(o, x0, x1);
                }
                let mut o = owner;
                while o != 0 {
                    extend(o, x0, x1);
                    o = il.owners[o as usize].parent;
                }
            }
            for (o, e) in ext.iter().enumerate().skip(1) {
                let Some((a, b)) = e else { continue };
                let ow = &il.owners[o];
                let s = shift[o];
                let top = baseline_y + s - ow.metrics.ascent - ow.padding[0] - ow.border[0];
                let h = ow.metrics.ascent
                    + ow.metrics.descent
                    + ow.padding[0]
                    + ow.padding[2]
                    + ow.border[0]
                    + ow.border[2];
                frags[o].push(Rect::new(ox + a, top, (b - a).max(0.0), h));
            }
        }
        for (o, f) in frags.into_iter().enumerate().skip(1) {
            let node = il.owners[o].node;
            let n = self.node_mut(node);
            n.fragments = f;
            n.layout_parent = Some(root);
        }
        for (node, x, y) in atom_pos {
            let mut l = self.node(node).layout;
            l.location = taffy::Point { x, y };
            self.node_mut(node).layout = l;
        }
    }
}
