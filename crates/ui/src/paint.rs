//! Painting into a Vello scene (backgrounds, borders with radii, text from Parley glyph runs,
//! replaced content, overflow clips, opacity layers, 2D transforms) and hit testing in the same
//! order.

use crate::css::values::Color;
use crate::document::Document;
use crate::dom::{Inner, NodeId, NodeKind, Rect};
use crate::image::{ImageContent, fit};
use crate::layout::inline::InlineLayout;
use crate::style::{BorderStyle, ComputedStyle, Position, TransformFn, Visibility};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, RoundedRect, RoundedRectRadii, Stroke};
use vello::peniko::{Fill, Mix};

fn brush(c: Color) -> vello::peniko::Color {
    vello::peniko::Color::new([c.r, c.g, c.b, c.a])
}

fn kr(r: Rect) -> vello::kurbo::Rect {
    vello::kurbo::Rect::new(
        r.x as f64,
        r.y as f64,
        (r.x + r.w) as f64,
        (r.y + r.h) as f64,
    )
}

/// The 2D transform a `transform` property describes, around its origin, for a box at `r`.
pub(crate) fn box_transform(s: &ComputedStyle, r: Rect) -> Option<Affine> {
    let fns = s.transform.as_ref()?;
    let ox = r.x as f64 + s.transform_origin.0.resolve(r.w) as f64;
    let oy = r.y as f64 + s.transform_origin.1.resolve(r.h) as f64;
    let mut m = Affine::IDENTITY;
    for f in fns.iter() {
        let t = match f {
            TransformFn::Translate(x, y) => {
                Affine::translate((x.resolve(r.w) as f64, y.resolve(r.h) as f64))
            }
            TransformFn::Scale(x, y) => Affine::scale_non_uniform(*x as f64, *y as f64),
            TransformFn::Rotate(a) => Affine::rotate((*a as f64).to_radians()),
            TransformFn::Skew(a, b) => Affine::skew(
                (*a as f64).to_radians().tan(),
                (*b as f64).to_radians().tan(),
            ),
            TransformFn::Matrix(v) => Affine::new([
                v[0] as f64,
                v[1] as f64,
                v[2] as f64,
                v[3] as f64,
                v[4] as f64,
                v[5] as f64,
            ]),
        };
        m *= t;
    }
    Some(Affine::translate((ox, oy)) * m * Affine::translate((-ox, -oy)))
}

fn radii(s: &ComputedStyle, r: Rect) -> RoundedRectRadii {
    let max = (r.w.min(r.h) / 2.0).max(0.0);
    let f = |i: usize| s.border_radius[i].0.resolve(r.w).min(max).max(0.0) as f64;
    RoundedRectRadii::new(f(0), f(1), f(2), f(3))
}

fn shrink_radii(rr: RoundedRectRadii, by: f64) -> RoundedRectRadii {
    RoundedRectRadii::new(
        (rr.top_left - by).max(0.0),
        (rr.top_right - by).max(0.0),
        (rr.bottom_right - by).max(0.0),
        (rr.bottom_left - by).max(0.0),
    )
}

/// Draw a box's background and borders.
fn paint_decorations(scene: &mut Scene, xf: Affine, s: &ComputedStyle, r: Rect, border: [f32; 4]) {
    if s.visibility != Visibility::Visible {
        return;
    }
    let rr = radii(s, r);
    if !s.background_color.is_transparent() {
        scene.fill(
            Fill::NonZero,
            xf,
            brush(s.background_color),
            None,
            &RoundedRect::from_rect(kr(r), rr),
        );
    }
    let [bt, br, bb, bl] = border;
    if bt + br + bb + bl <= 0.0 {
        return;
    }
    let uniform =
        bt == br && br == bb && bb == bl && s.border_color.iter().all(|c| *c == s.border_color[0]);
    if uniform && s.border_style.iter().all(|st| *st == s.border_style[0]) {
        if s.border_color[0].is_transparent() {
            return;
        }
        let w = bt as f64;
        let inset = kr(r).inset(-w / 2.0);
        let mut stroke = Stroke::new(w);
        if matches!(s.border_style[0], BorderStyle::Dashed) {
            stroke = stroke.with_dashes(0.0, [w * 3.0, w * 3.0]);
        } else if matches!(s.border_style[0], BorderStyle::Dotted) {
            stroke = stroke.with_dashes(0.0, [w, w]);
        }
        scene.stroke(
            &stroke,
            xf,
            brush(s.border_color[0]),
            None,
            &RoundedRect::from_rect(inset, shrink_radii(rr, w / 2.0)),
        );
        return;
    }
    // Per side: trapezoids between the outer and inner edges.
    let o = kr(r);
    let i = vello::kurbo::Rect::new(
        o.x0 + bl as f64,
        o.y0 + bt as f64,
        o.x1 - br as f64,
        o.y1 - bb as f64,
    );
    let sides = [
        (bt, [(o.x0, o.y0), (o.x1, o.y0), (i.x1, i.y0), (i.x0, i.y0)]),
        (br, [(o.x1, o.y0), (o.x1, o.y1), (i.x1, i.y1), (i.x1, i.y0)]),
        (bb, [(o.x1, o.y1), (o.x0, o.y1), (i.x0, i.y1), (i.x1, i.y1)]),
        (bl, [(o.x0, o.y1), (o.x0, o.y0), (i.x0, i.y0), (i.x0, i.y1)]),
    ];
    for (k, (w, pts)) in sides.iter().enumerate() {
        if *w <= 0.0 || s.border_color[k].is_transparent() {
            continue;
        }
        let mut p = BezPath::new();
        p.move_to(pts[0]);
        for q in &pts[1..] {
            p.line_to(*q);
        }
        p.close_path();
        scene.fill(Fill::NonZero, xf, brush(s.border_color[k]), None, &p);
    }
}

struct Painter<'a> {
    doc: &'a Document,
    scene: &'a mut Scene,
    /// Subtrees left out of this paint.
    skip: &'a [NodeId],
    /// The part of the document that can show, in document coordinates: boxes whose border box
    /// and content overflow both miss it are left out. `None` below a transformed box.
    cull: Option<Rect>,
}

/// Boxes with at least this many children find the ones a cull rectangle reaches by binary
/// search ([`ChildExtents`]).
const INDEXED_CHILDREN: usize = 64;

/// Where a box's children reach on the block axis, relative to where the box places them: for
/// each index, the furthest end of the children up to it and the earliest start of the children
/// from it (both non-decreasing). Children that can move or paint anywhere (sticky, transformed)
/// reach everything.
#[derive(Debug, Default)]
pub(crate) struct ChildExtents {
    generation: u64,
    ends: Vec<f32>,
    starts: Vec<f32>,
}

impl ChildExtents {
    fn build(doc: &Document, kids: &[NodeId], generation: u64) -> ChildExtents {
        let reach: Vec<(f32, f32)> = kids
            .iter()
            .map(|&k| {
                let n = doc.node(k);
                let l = &n.layout;
                let moves = n.style.as_deref().is_some_and(|s| {
                    s.position == Position::Sticky
                        || box_transform(s, Rect::new(0.0, 0.0, l.size.width, l.size.height))
                            .is_some()
                });
                if moves {
                    return (f32::NEG_INFINITY, f32::INFINITY);
                }
                let o = &l.scrollable_overflow_rect;
                let y = l.location.y;
                let py = y + l.border.top;
                (y.min(py + o.top), (y + l.size.height).max(py + o.bottom))
            })
            .collect();
        let mut ends = Vec::with_capacity(reach.len());
        let mut furthest = f32::NEG_INFINITY;
        for &(_, end) in &reach {
            furthest = furthest.max(end);
            ends.push(furthest);
        }
        let mut starts = vec![0.0; reach.len()];
        let mut earliest = f32::INFINITY;
        for (i, &(start, _)) in reach.iter().enumerate().rev() {
            earliest = earliest.min(start);
            starts[i] = earliest;
        }
        ChildExtents {
            generation,
            ends,
            starts,
        }
    }
}

/// How far past the cull rectangle a box may reach and still be painted: room for ink that
/// layout's overflow doesn't count (outlines, borders drawn outside).
const CULL_MARGIN: f32 = 64.0;

impl Painter<'_> {
    /// Paint a box whose layout parent's border box is at `origin`.
    fn paint_box(&mut self, id: NodeId, origin: (f32, f32), xf: Affine) {
        let doc = self.doc;
        let n = doc.node(id);
        if n.skipped
            || self.skip.contains(&id)
            || matches!(
                n.inner,
                Inner::None | Inner::Contents | Inner::Text | Inner::InlineBox
            )
        {
            return;
        }
        let Some(s) = n.style.as_deref() else { return };
        let l = &n.layout;
        let (sx, sy) = crate::layout::sticky_offset(doc, id);
        let r = Rect::new(
            origin.0 + l.location.x + sx,
            origin.1 + l.location.y + sy,
            l.size.width,
            l.size.height,
        );
        if let Some(c) = self.cull {
            // The box with its scrollable overflow (its descendants' border boxes and their
            // unclipped overflow, measured from the padding box's corner).
            let o = &l.scrollable_overflow_rect;
            let (px, py) = (r.x + l.border.left, r.y + l.border.top);
            let x0 = r.x.min(px + o.left);
            let y0 = r.y.min(py + o.top);
            let x1 = (r.x + r.w).max(px + o.right);
            let y1 = (r.y + r.h).max(py + o.bottom);
            if x1 < c.x - CULL_MARGIN
                || y1 < c.y - CULL_MARGIN
                || x0 > c.x + c.w + CULL_MARGIN
                || y0 > c.y + c.h + CULL_MARGIN
            {
                return;
            }
        }
        let transform = box_transform(s, r);
        let cull = self.cull;
        if transform.is_some() {
            self.cull = None;
        }
        self.paint_box_at(id, r, xf, transform);
        self.cull = cull;
    }

    /// The range of `id`'s layout children that can reach the cull rectangle `c`, its children
    /// placed from `origin_y`. Boxes with few children are walked whole; for the others the
    /// document keeps where each child and its overflow reach on the block axis (rebuilt after
    /// each layout), and two binary searches find the range: past its end every child starts
    /// below `c`, before its start every child ends above it.
    fn reaching(&self, id: NodeId, origin_y: f32, c: Rect) -> std::ops::Range<usize> {
        let doc = self.doc;
        let kids = &doc.node(id).layout_children;
        if kids.len() < INDEXED_CHILDREN {
            return 0..kids.len();
        }
        let mut index = doc.child_extents.lock().unwrap_or_else(|e| e.into_inner());
        let generation = doc.generation();
        let ext = index.entry(id).or_default();
        if ext.generation != generation || ext.ends.len() != kids.len() {
            *ext = ChildExtents::build(doc, kids, generation);
        }
        let top = c.y - CULL_MARGIN - origin_y;
        let bottom = c.y + c.h + CULL_MARGIN - origin_y;
        let start = ext.ends.partition_point(|&e| e < top);
        let end = ext.starts.partition_point(|&s| s <= bottom).max(start);
        start..end
    }

    fn paint_box_at(&mut self, id: NodeId, r: Rect, xf: Affine, transform: Option<Affine>) {
        let doc = self.doc;
        let n = doc.node(id);
        let Some(s) = n.style.as_deref() else { return };
        let l = &n.layout;
        let xf = match transform {
            Some(t) => xf * t,
            None => xf,
        };
        let layered = s.opacity < 1.0;
        if layered {
            self.scene
                .push_layer(Fill::NonZero, Mix::Normal, s.opacity, xf, &kr(r));
        }
        if !matches!(n.kind, NodeKind::Anonymous) {
            paint_decorations(
                self.scene,
                xf,
                s,
                r,
                [l.border.top, l.border.right, l.border.bottom, l.border.left],
            );
        }
        if let Some(url) = &s.background_image
            && let Some(image) = doc.url_image(url)
            && s.visibility == Visibility::Visible
        {
            self.paint_background_image(image, s, r, l, xf);
        }
        if let Some(image) = n.image.as_deref()
            && s.visibility == Visibility::Visible
        {
            self.paint_image(image, s, r, l, xf);
        }
        let clip = s.overflow_x.clips() || s.overflow_y.clips();
        if clip {
            let pad = Rect::new(
                r.x + l.border.left,
                r.y + l.border.top,
                (r.w - l.border.left - l.border.right).max(0.0),
                (r.h - l.border.top - l.border.bottom).max(0.0),
            );
            let rr = shrink_radii(radii(s, r), l.border.top as f64);
            self.scene
                .push_clip_layer(Fill::NonZero, xf, &RoundedRect::from_rect(kr(pad), rr));
        }
        let child_origin = (r.x - n.scroll.0, r.y - n.scroll.1);
        // Only the children that can reach the cull rectangle (all of them inline).
        let range = match self.cull {
            Some(c) if n.inner != Inner::Ifc => self.reaching(id, child_origin.1, c),
            _ => 0..n.layout_children.len(),
        };
        let kids = &n.layout_children[range.clone()];
        // Negative z-index, in-flow content, then positioned boxes by z-index.
        let mut positioned: Vec<(i32, usize, NodeId)> = Vec::new();
        for (k, &c) in kids.iter().enumerate() {
            let cs = doc.node(c).style.as_deref();
            if let Some(cs) = cs
                && (cs.position != Position::Static || cs.z_index.is_some())
            {
                positioned.push((cs.z_index.unwrap_or(0), k, c));
            }
        }
        positioned.sort_by_key(|(z, k, _)| (*z, *k));
        for &(z, _, c) in &positioned {
            if z < 0 {
                self.paint_box(c, child_origin, xf);
            }
        }
        if n.inner == Inner::Ifc {
            if let Some(il) = n.inline.as_deref() {
                self.paint_inline(il, child_origin, xf, &positioned);
            }
        } else {
            for &c in kids {
                if positioned.iter().any(|p| p.2 == c) {
                    continue;
                }
                self.paint_box(c, child_origin, xf);
            }
        }
        for &(z, _, c) in &positioned {
            if z >= 0 {
                self.paint_box(c, child_origin, xf);
            }
        }
        if clip {
            self.scene.pop_layer();
        }
        if layered {
            self.scene.pop_layer();
        }
    }

    /// A `background-image` picture: at its own size (times the zoom), from the padding box's
    /// top-left corner, repeated over the border box and clipped to it (CSS initial
    /// `background-size`, `-position`, `-repeat` and `-clip`).
    fn paint_background_image(
        &mut self,
        image: &ImageContent,
        s: &ComputedStyle,
        r: Rect,
        l: &taffy::Layout,
        xf: Affine,
    ) {
        let (iw, ih) = (image.data.width, image.data.height);
        let (pw, ph) = image.picture_size();
        let (tw, th) = (pw * s.zoom, ph * s.zoom);
        if r.w <= 0.0 || r.h <= 0.0 || iw == 0 || ih == 0 || tw <= 0.0 || th <= 0.0 {
            return;
        }
        let origin = (r.x + l.border.left, r.y + l.border.top);
        let brush = image
            .brush(s.image_rendering)
            .with_extend(vello::peniko::Extend::Repeat);
        let place = Affine::translate((origin.0 as f64, origin.1 as f64))
            * Affine::scale_non_uniform(tw as f64 / iw as f64, th as f64 / ih as f64);
        self.scene.fill(
            Fill::NonZero,
            xf,
            &brush,
            Some(place),
            &RoundedRect::from_rect(kr(r), radii(s, r)),
        );
    }

    /// Replaced content: the picture placed in the content box by `object-fit`, clipped to the
    /// content box (with the box's radii, inset by its border).
    fn paint_image(
        &mut self,
        image: &ImageContent,
        s: &ComputedStyle,
        r: Rect,
        l: &taffy::Layout,
        xf: Affine,
    ) {
        let content = Rect::new(
            r.x + l.border.left + l.padding.left,
            r.y + l.border.top + l.padding.top,
            (r.w - l.border.left - l.border.right - l.padding.left - l.padding.right).max(0.0),
            (r.h - l.border.top - l.border.bottom - l.padding.top - l.padding.bottom).max(0.0),
        );
        let (iw, ih) = (image.data.width, image.data.height);
        if content.w <= 0.0 || content.h <= 0.0 || iw == 0 || ih == 0 {
            return;
        }
        let (pw, ph) = image.picture_size();
        let picture = (pw * s.zoom, ph * s.zoom);
        let (x, y, w, h) = fit(s.object_fit, picture, (content.w, content.h));
        let rr = shrink_radii(radii(s, r), l.border.top as f64 + l.padding.top as f64);
        self.scene
            .push_clip_layer(Fill::NonZero, xf, &RoundedRect::from_rect(kr(content), rr));
        let place = Affine::translate(((content.x + x) as f64, (content.y + y) as f64))
            * Affine::scale_non_uniform(w as f64 / iw as f64, h as f64 / ih as f64);
        self.scene
            .draw_image(&image.brush(s.image_rendering), xf * place);
        self.scene.pop_layer();
    }

    fn paint_inline(
        &mut self,
        il: &InlineLayout,
        origin: (f32, f32),
        xf: Affine,
        positioned: &[(i32, usize, NodeId)],
    ) {
        let doc = self.doc;
        // Inline box backgrounds and borders under the text.
        for o in il.owners.iter().skip(1) {
            let n = doc.node(o.node);
            for f in &n.fragments {
                let r = f.translate(origin.0, origin.1);
                paint_decorations(self.scene, xf, &o.style, r, o.border);
            }
        }
        let shift = il.shifts.first().cloned().unwrap_or_default();
        for (li, line) in il.layout.lines().enumerate() {
            let Some(info) = il.lines.get(li) else {
                continue;
            };
            let baseline = origin.1 + il.content_origin.1 + info.top + info.baseline;
            let left = origin.0 + il.content_origin.0;
            // `text-overflow: ellipsis` on a clipping block: glyphs past the end make way for "…".
            let root_style = &il.owners[0].style;
            let line_right =
                line.metrics().advance - line.metrics().trailing_whitespace + line.metrics().offset;
            let ellipsis = if root_style.text_overflow == crate::style::TextOverflow::Ellipsis
                && root_style.overflow_x.clips()
                && line_right > il.width + 0.5
            {
                line.runs()
                    .next()
                    .and_then(|r| ellipsis_glyph(r.font(), r.font_size()))
                    .map(|(gid, adv)| (gid, adv, left + il.width - adv))
            } else {
                None
            };
            let mut last_drawn: Option<(f32, f32, parley::FontData, f32, Color)> = None;
            for item in line.items() {
                match item {
                    parley::PositionedLayoutItem::GlyphRun(g) => {
                        let b = g.style().brush;
                        if b.edge != 0 {
                            continue;
                        }
                        let o = &il.owners[b.owner as usize];
                        if o.style.visibility != Visibility::Visible {
                            continue;
                        }
                        let y = baseline + shift.get(b.owner as usize).copied().unwrap_or(0.0);
                        let run = g.run();
                        let mut x = left + g.offset();
                        let limit = ellipsis.map(|e| e.2).unwrap_or(f32::INFINITY);
                        let mut drawn_end = x;
                        let glyphs: Vec<vello::Glyph> = g
                            .glyphs()
                            .filter_map(|gl| {
                                let out = vello::Glyph {
                                    id: gl.id,
                                    x: x + gl.x,
                                    y: y + gl.y,
                                };
                                let fits = x + gl.advance <= limit + 0.01;
                                x += gl.advance;
                                if fits {
                                    drawn_end = x;
                                }
                                fits.then_some(out)
                            })
                            .collect();
                        if ellipsis.is_some()
                            && last_drawn.as_ref().is_none_or(|l| drawn_end >= l.0)
                        {
                            last_drawn = Some((
                                drawn_end,
                                y,
                                run.font().clone(),
                                run.font_size(),
                                o.style.color,
                            ));
                        }
                        let synthesis = run.synthesis();
                        let glyph_xf = synthesis
                            .skew()
                            .map(|a| Affine::skew((a as f64).to_radians().tan(), 0.0));
                        self.scene
                            .draw_glyphs(run.font())
                            .font_size(run.font_size())
                            .transform(xf)
                            .glyph_transform(glyph_xf)
                            .normalized_coords(run.normalized_coords())
                            .brush(brush(o.style.color))
                            .draw(Fill::NonZero, glyphs.into_iter());
                        let dec = o.style.text_decoration;
                        if dec.underline || dec.line_through || dec.overline {
                            let m = run.metrics();
                            let color =
                                brush(o.style.text_decoration_color.unwrap_or(o.style.color));
                            let x0 = (origin.0 + il.content_origin.0 + g.offset()) as f64;
                            let x1 = x0 + g.advance() as f64;
                            let mut line_at = |off: f32, size: f32| {
                                let top = (y - off) as f64;
                                let rect = vello::kurbo::Rect::new(
                                    x0,
                                    top,
                                    x1,
                                    top + size.max(1.0) as f64,
                                );
                                self.scene.fill(Fill::NonZero, xf, color, None, &rect);
                            };
                            if dec.underline {
                                line_at(m.underline_offset, m.underline_size);
                            }
                            if dec.line_through {
                                line_at(m.strikethrough_offset, m.strikethrough_size);
                            }
                            if dec.overline {
                                line_at(m.ascent, m.underline_size);
                            }
                        }
                    }
                    parley::PositionedLayoutItem::InlineBox(b) => {
                        let node = il.boxes[b.id as usize].node;
                        if ellipsis.is_some_and(|e| left + b.x + b.width > e.2) {
                            continue;
                        }
                        if !positioned.iter().any(|p| p.2 == node) {
                            self.paint_box(node, origin, xf);
                        }
                    }
                }
            }
            if let (Some((gid, _, _)), Some((x, y, font, size, color))) = (ellipsis, last_drawn) {
                self.scene
                    .draw_glyphs(&font)
                    .font_size(size)
                    .transform(xf)
                    .brush(brush(color))
                    .draw(
                        Fill::NonZero,
                        std::iter::once(vello::Glyph { id: gid, x, y }),
                    );
            }
        }
    }
}

/// The glyph and advance of "…" in a font.
fn ellipsis_glyph(font: &parley::FontData, size: f32) -> Option<(u32, f32)> {
    use skrifa::MetadataProvider;
    let f = skrifa::FontRef::from_index(font.data.as_ref(), font.index).ok()?;
    let gid = f.charmap().map('\u{2026}')?;
    let adv = f
        .glyph_metrics(
            skrifa::instance::Size::new(size),
            skrifa::instance::LocationRef::default(),
        )
        .advance_width(gid)?;
    Some((gid.to_u32(), adv))
}

/// Paint the document into a scene, in CSS px with the viewport's top-left at the origin.
/// Rendering the scene to a texture is the render crate's job.
pub fn paint(doc: &Document, scene: &mut Scene) {
    paint_with_transform(doc, scene, Affine::IDENTITY);
}

/// Paint with a transform (for example the window's scale factor).
pub fn paint_with_transform(doc: &Document, scene: &mut Scene, xf: Affine) {
    paint_except(doc, scene, xf, &[], None);
}

/// [`paint_with_transform`], leaving out the boxes rooted at `skip`, as `visibility: hidden` on
/// them would, but without restyling or laying out the document again; and, given `cull` (the
/// part of the document that can show, in document coordinates), the boxes that can't reach it.
pub fn paint_except(
    doc: &Document,
    scene: &mut Scene,
    xf: Affine,
    skip: &[NodeId],
    cull: Option<Rect>,
) {
    let root = doc.root();
    // The canvas takes the root's background, or the body's if the root has none (CSS 2.1 §14.2).
    let mut bg = doc
        .node(root)
        .style
        .as_ref()
        .map(|s| s.background_color)
        .unwrap_or(Color::TRANSPARENT);
    if bg.is_transparent()
        && let Some(body) = doc.element_children(root).into_iter().find(|c| {
            doc.node(*c)
                .element()
                .is_some_and(|e| e.tag.as_str() == "body")
        })
    {
        bg = doc
            .node(body)
            .style
            .as_ref()
            .map(|s| s.background_color)
            .unwrap_or(Color::TRANSPARENT);
    }
    if !bg.is_transparent() {
        let (w, h) = doc.viewport();
        scene.fill(
            Fill::NonZero,
            xf,
            brush(bg),
            None,
            &vello::kurbo::Rect::new(0.0, 0.0, w as f64, h as f64),
        );
    }
    let mut p = Painter {
        doc,
        scene,
        skip,
        cull,
    };
    p.paint_box(root, (0.0, 0.0), xf);
}

/// The deepest element under a point (document coordinates), honoring overflow clips, paint
/// order and `pointer-events: none`.
pub fn hit_test(doc: &Document, x: f32, y: f32) -> Option<NodeId> {
    let (w, h) = doc.viewport();
    // The root covers the whole viewport for hit testing, as in browsers.
    hit_box(doc, doc.root(), (0.0, 0.0), x, y)
        .or((x >= 0.0 && y >= 0.0 && x < w && y < h).then(|| doc.root()))
}

fn hit_box(doc: &Document, id: NodeId, origin: (f32, f32), x: f32, y: f32) -> Option<NodeId> {
    let n = doc.node(id);
    if n.skipped
        || matches!(
            n.inner,
            Inner::None | Inner::Contents | Inner::Text | Inner::InlineBox
        )
    {
        return None;
    }
    let s = n.style.as_deref()?;
    let l = &n.layout;
    let (sx, sy) = crate::layout::sticky_offset(doc, id);
    let r = Rect::new(
        origin.0 + l.location.x + sx,
        origin.1 + l.location.y + sy,
        l.size.width,
        l.size.height,
    );
    let (x, y) = match box_transform(s, r) {
        Some(t) => {
            let p = t.inverse() * vello::kurbo::Point::new(x as f64, y as f64);
            (p.x as f32, p.y as f32)
        }
        None => (x, y),
    };
    let inside = r.contains(x, y);
    if (s.overflow_x.clips() || s.overflow_y.clips()) && !inside {
        return None;
    }
    let child_origin = (r.x - n.scroll.0, r.y - n.scroll.1);
    let mut positioned: Vec<(i32, usize, NodeId)> = Vec::new();
    for (k, &c) in n.layout_children.iter().enumerate() {
        if let Some(cs) = doc.node(c).style.as_deref()
            && (cs.position != Position::Static || cs.z_index.is_some())
        {
            positioned.push((cs.z_index.unwrap_or(0), k, c));
        }
    }
    positioned.sort_by_key(|(z, k, _)| (*z, *k));
    for &(z, _, c) in positioned.iter().rev() {
        if z >= 0
            && let Some(h) = hit_box(doc, c, child_origin, x, y)
        {
            return Some(h);
        }
    }
    if n.inner == Inner::Ifc {
        if let Some(il) = n.inline.as_deref() {
            for b in il.boxes.iter().rev() {
                if positioned.iter().any(|p| p.2 == b.node) {
                    continue;
                }
                if let Some(h) = hit_box(doc, b.node, child_origin, x, y) {
                    return Some(h);
                }
            }
            // Deepest inline box whose fragment contains the point.
            for o in il.owners.iter().skip(1).rev() {
                let on = doc.node(o.node);
                if !o.style.pointer_events {
                    continue;
                }
                if on
                    .fragments
                    .iter()
                    .any(|f| f.translate(child_origin.0, child_origin.1).contains(x, y))
                {
                    return Some(element_for(doc, o.node));
                }
            }
        }
    } else {
        for &c in n.layout_children.iter().rev() {
            if positioned.iter().any(|p| p.2 == c) {
                continue;
            }
            if let Some(h) = hit_box(doc, c, child_origin, x, y) {
                return Some(h);
            }
        }
    }
    for &(z, _, c) in positioned.iter().rev() {
        if z < 0
            && let Some(h) = hit_box(doc, c, child_origin, x, y)
        {
            return Some(h);
        }
    }
    if inside && s.pointer_events && s.visibility == Visibility::Visible {
        return Some(element_for(doc, id));
    }
    None
}

/// Anonymous and pseudo boxes report the element they belong to.
fn element_for(doc: &Document, mut id: NodeId) -> NodeId {
    loop {
        let n = doc.node(id);
        match n.kind {
            NodeKind::Element(_) => return id,
            NodeKind::Anonymous => match n.layout_parent {
                Some(p) => id = p,
                None => return id,
            },
            _ => match n.parent {
                Some(p) => id = p,
                None => return id,
            },
        }
    }
}
