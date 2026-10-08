//! Layout: Taffy's tree traits over the document, dispatch to Taffy (block, flex, grid) or our
//! own algorithms (inline, table, absolute boxes in our contexts, virtualized lists), scroll
//! anchoring and box geometry queries.

pub(crate) mod boxes;
pub(crate) mod inline;
pub(crate) mod table;

use crate::css::values::CalcNode;
use crate::document::Document;
use crate::dom::{Inner, NodeId, NodeKind, Rect, VirtRow};
use taffy::prelude::*;
use taffy::{
    BlockContext, CacheTree, LayoutBlockContainer, LayoutFlexboxContainer, LayoutGridContainer,
    LayoutInput, LayoutOutput, LayoutPartialTree, MaybeMath, MaybeResolve, ResolveOrZero, RunMode,
    SizingMode, TraversePartialTree, compute_block_layout, compute_cached_layout,
    compute_flexbox_layout, compute_grid_layout,
};

/// Resolve a calc() pointer stored in a Taffy style.
pub(crate) fn resolve_calc(ptr: *const (), basis: f32) -> f32 {
    // SAFETY: every calc pointer in a Taffy style points into an `Arc<CalcNode>` kept alive in the
    // same node's `calc_keep` for as long as that style is stored.
    let node = unsafe { &*(ptr as *const CalcNode) };
    node.resolve(basis)
}

#[derive(Debug)]
pub struct ChildIter<'a>(std::slice::Iter<'a, NodeId>);
impl Iterator for ChildIter<'_> {
    type Item = taffy::NodeId;
    fn next(&mut self) -> Option<taffy::NodeId> {
        self.0.next().map(|n| n.taffy())
    }
}

impl TraversePartialTree for Document {
    type ChildIter<'a> = ChildIter<'a>;
    fn child_ids(&self, parent: taffy::NodeId) -> ChildIter<'_> {
        ChildIter(self.node(NodeId::from_taffy(parent)).layout_children.iter())
    }
    fn child_count(&self, parent: taffy::NodeId) -> usize {
        self.node(NodeId::from_taffy(parent)).layout_children.len()
    }
    fn get_child_id(&self, parent: taffy::NodeId, index: usize) -> taffy::NodeId {
        self.node(NodeId::from_taffy(parent)).layout_children[index].taffy()
    }
}

impl LayoutPartialTree for Document {
    type CoreContainerStyle<'a> = &'a taffy::Style;
    type CustomIdent = String;

    fn get_core_container_style(&self, node_id: taffy::NodeId) -> &taffy::Style {
        &self.node(NodeId::from_taffy(node_id)).taffy_style
    }
    fn resolve_calc_value(&self, val: *const (), basis: f32) -> f32 {
        resolve_calc(val, basis)
    }
    fn set_unrounded_layout(&mut self, node_id: taffy::NodeId, layout: &Layout) {
        let n = self.node_mut(NodeId::from_taffy(node_id));
        n.layout = *layout;
        n.skipped = false;
    }
    fn compute_child_layout(
        &mut self,
        node_id: taffy::NodeId,
        inputs: LayoutInput,
    ) -> LayoutOutput {
        compute_cached_layout(self, node_id, inputs, |tree, id, inputs| {
            tree.compute_uncached(id, inputs, None)
        })
    }
}

impl CacheTree for Document {
    fn cache_get(&mut self, node_id: taffy::NodeId, input: &LayoutInput) -> Option<LayoutOutput> {
        self.node_mut(NodeId::from_taffy(node_id)).cache.get(input)
    }
    fn cache_store(&mut self, node_id: taffy::NodeId, input: &LayoutInput, out: LayoutOutput) {
        self.node_mut(NodeId::from_taffy(node_id))
            .cache
            .store(input, out)
    }
    fn cache_clear(&mut self, node_id: taffy::NodeId) {
        self.node_mut(NodeId::from_taffy(node_id)).cache.clear();
    }
}

impl LayoutBlockContainer for Document {
    type BlockContainerStyle<'a> = &'a taffy::Style;
    type BlockItemStyle<'a> = &'a taffy::Style;
    fn get_block_container_style(&self, node_id: taffy::NodeId) -> &taffy::Style {
        self.get_core_container_style(node_id)
    }
    fn get_block_child_style(&self, child: taffy::NodeId) -> &taffy::Style {
        self.get_core_container_style(child)
    }
    fn compute_block_child_layout(
        &mut self,
        node_id: taffy::NodeId,
        inputs: LayoutInput,
        block_ctx: Option<&mut BlockContext<'_>>,
    ) -> LayoutOutput {
        compute_cached_layout(self, node_id, inputs, |tree, id, inputs| {
            tree.compute_uncached(id, inputs, block_ctx)
        })
    }
}

impl LayoutFlexboxContainer for Document {
    type FlexboxContainerStyle<'a> = &'a taffy::Style;
    type FlexboxItemStyle<'a> = &'a taffy::Style;
    fn get_flexbox_container_style(&self, node_id: taffy::NodeId) -> &taffy::Style {
        self.get_core_container_style(node_id)
    }
    fn get_flexbox_child_style(&self, child: taffy::NodeId) -> &taffy::Style {
        self.get_core_container_style(child)
    }
}

impl LayoutGridContainer for Document {
    type GridContainerStyle<'a> = &'a taffy::Style;
    type GridItemStyle<'a> = &'a taffy::Style;
    fn get_grid_container_style(&self, node_id: taffy::NodeId) -> &taffy::Style {
        self.get_core_container_style(node_id)
    }
    fn get_grid_child_style(&self, child: taffy::NodeId) -> &taffy::Style {
        self.get_core_container_style(child)
    }
}

impl Document {
    fn compute_uncached(
        &mut self,
        tid: taffy::NodeId,
        inputs: LayoutInput,
        block_ctx: Option<&mut BlockContext<'_>>,
    ) -> LayoutOutput {
        let id = NodeId::from_taffy(tid);
        self.stats.laid_out += 1;
        let run = self.layout_runs;
        self.node_mut(id).laid_out_in = run;
        let n = self.node(id);
        if n.image.is_some()
            && n.layout_children.is_empty()
            && matches!(n.inner, Inner::Flow | Inner::Flex | Inner::Grid)
        {
            return self.compute_replaced_layout(id, inputs);
        }
        match n.inner {
            Inner::Ifc => self.compute_inline_layout(id, inputs),
            // A virtualized list: a block container or a flex column (Tern's `main`).
            Inner::Flow | Inner::Flex if n.virtual_estimate.is_some() => {
                self.compute_virtual_layout(id, inputs)
            }
            Inner::Flow => {
                let out = compute_block_layout(self, tid, inputs, block_ctx);
                if inputs.run_mode == RunMode::PerformLayout {
                    self.after_block_layout(id, &out);
                }
                out
            }
            Inner::Flex => compute_flexbox_layout(self, tid, inputs),
            Inner::Grid => compute_grid_layout(self, tid, inputs),
            Inner::Table => self.compute_table_layout(id, inputs),
            _ => LayoutOutput::HIDDEN,
        }
    }

    /// A replaced box (an element with [`crate::ImageContent`] and no children): its natural size
    /// and ratio stand in for content, under its own size, min and max (CSS 2.1 §10.3.2, §10.4,
    /// §10.6.2). Sizes the parent already decided (`known_dimensions`) are final.
    fn compute_replaced_layout(&mut self, id: NodeId, inputs: LayoutInput) -> LayoutOutput {
        let n = self.node(id);
        // The natural size is in CSS px; the box's lengths are zoomed (§8.3's region zoom).
        let zoom = n.style.as_ref().map_or(1.0, |s| s.zoom);
        let natural = n
            .image
            .as_ref()
            .map_or((0.0, 0.0), |i| (i.natural.0 * zoom, i.natural.1 * zoom));
        let ts = &n.taffy_style;
        let parent = inputs.parent_size;
        let pad = ts.padding.resolve_or_zero(parent.width, resolve_calc);
        let border = ts.border.resolve_or_zero(parent.width, resolve_calc);
        let pb = Size {
            width: pad.left + pad.right + border.left + border.right,
            height: pad.top + pad.bottom + border.top + border.bottom,
        };
        let border_box = ts.box_sizing == taffy::BoxSizing::BorderBox;
        // Specified sizes as content sizes.
        let content =
            |v: Option<f32>, pb: f32| v.map(|v| if border_box { (v - pb).max(0.0) } else { v });
        let styled = inputs.sizing_mode == SizingMode::InherentSize;
        let size = ts.size.maybe_resolve(parent, resolve_calc);
        let min = ts.min_size.maybe_resolve(parent, resolve_calc);
        let max = ts.max_size.maybe_resolve(parent, resolve_calc);
        let known = inputs.known_dimensions;
        let axis =
            |known: Option<f32>, size: Option<f32>, min: Option<f32>, max: Option<f32>, pb: f32| {
                match known {
                    Some(k) => (Some((k - pb).max(0.0)), 0.0, None),
                    None if styled => (
                        content(size, pb),
                        content(min, pb).unwrap_or(0.0),
                        content(max, pb),
                    ),
                    None => (None, 0.0, None),
                }
            };
        let (w, min_w, mut max_w) = axis(known.width, size.width, min.width, max.width, pb.width);
        // Tern's rule for pictures (book:5929): an `auto` width also fits the available width,
        // both axes shrinking together.
        if w.is_none()
            && let AvailableSpace::Definite(avail) = inputs.available_space.width
        {
            let fit = (avail - pb.width).max(0.0);
            max_w = Some(max_w.map_or(fit, |m| m.min(fit)));
        }
        let (h, min_h, max_h) = axis(known.height, size.height, min.height, max.height, pb.height);
        let (cw, ch) = crate::image::replaced_size(w, h, (min_w, min_h), (max_w, max_h), natural);
        LayoutOutput::from_outer_size(Size {
            width: cw + pb.width,
            height: ch + pb.height,
        })
    }

    /// Taffy places absolutely positioned children at their static position relative to the
    /// containing block; nothing else to do for block containers yet.
    fn after_block_layout(&mut self, _id: NodeId, _out: &LayoutOutput) {}

    /// Store a child's layout (location relative to its layout parent's border box), filling in
    /// border, padding and margin from its style.
    pub(crate) fn store_layout(&mut self, id: NodeId, x: f32, y: f32, size: Size<f32>, basis: f32) {
        let ts = &self.node(id).taffy_style;
        let border = ts.border.resolve_or_zero(Some(basis), resolve_calc);
        let padding = ts.padding.resolve_or_zero(Some(basis), resolve_calc);
        let margin = ts.margin.resolve_or_zero(Some(basis), resolve_calc);
        let n = self.node_mut(id);
        n.layout.location = taffy::Point { x, y };
        n.layout.size = size;
        n.layout.border = border;
        n.layout.padding = padding;
        n.layout.margin = margin;
        n.layout.scrollable_overflow_rect = taffy::Rect {
            left: 0.0,
            top: 0.0,
            right: size.width,
            bottom: size.height,
        };
        n.skipped = false;
    }

    /// Lay out the document: rebuild boxes where needed and run layout from the root.
    pub(crate) fn layout(&mut self) {
        if !self.layout_dirty {
            return;
        }
        self.layout_runs += 1;
        self.build_boxes();
        let anchors = self.record_anchors();
        let root = self.root();
        let (w, h) = self.viewport;
        taffy::compute_root_layout(
            self,
            root.taffy(),
            Size {
                width: AvailableSpace::Definite(w),
                height: AvailableSpace::Definite(h),
            },
        );
        self.apply_anchors(anchors);
        self.layout_dirty = std::mem::take(&mut self.needs_relayout);
    }

    /// Scroll anchoring (§3.4): remember, for each scrolled container, the first child under the
    /// top edge and its offset, so content above can change height without moving the view.
    fn record_anchors(&self) -> Vec<(NodeId, NodeId, f32)> {
        let mut out = Vec::new();
        for &id in &self.scrolled {
            let Some(n) = self.try_node(id) else { continue };
            if n.scroll.1 <= 0.0 || n.layout_children.is_empty() {
                continue;
            }
            let sy = n.scroll.1;
            for &c in &n.layout_children {
                let cl = self.node(c).layout;
                if cl.location.y + cl.size.height > sy {
                    out.push((id, c, sy - cl.location.y));
                    break;
                }
            }
        }
        out
    }

    fn apply_anchors(&mut self, anchors: Vec<(NodeId, NodeId, f32)>) {
        for (container, child, offset) in anchors {
            if self.try_node(child).is_none() || self.node(child).layout_parent != Some(container) {
                continue;
            }
            let y = self.node(child).layout.location.y + offset;
            let max = self.max_scroll(container).1;
            self.node_mut(container).scroll.1 = y.clamp(0.0, max.max(0.0));
        }
    }

    /// Mark a block container as a virtualized list: children outside the visible part of the
    /// container (a scroll container) are not laid out; they take their last measured height or
    /// `estimate`. `None` turns virtualization off.
    pub fn set_virtual(&mut self, id: NodeId, estimate: Option<f32>) {
        self.node_mut(id).virtual_estimate = estimate;
        self.invalidate_layout(id);
    }

    /// The visible window of a virtualized container that doesn't scroll itself (an ancestor
    /// does), in its content coordinates (`top`, `bottom`): children outside it, plus an overscan
    /// of half a viewport, are not laid out. `None` uses the container's own scroll and height.
    pub fn set_virtual_window(&mut self, id: NodeId, window: Option<(f32, f32)>) {
        self.set_virtual_window_at(id, window, false);
    }

    /// [`Document::set_virtual_window`], with `from_end`: the window is distances up from the
    /// end of the container's content (`top`, `bottom`; `bottom` may be negative, past the
    /// end), so it stays on the end as the container grows (a view following a transcript's
    /// end lays out the new end, not where the end was).
    pub fn set_virtual_window_at(
        &mut self,
        id: NodeId,
        window: Option<(f32, f32)>,
        from_end: bool,
    ) {
        let from_end = from_end && window.is_some();
        let n = self.node(id);
        if n.virtual_window != window || n.virtual_from_end != from_end {
            let n = self.node_mut(id);
            n.virtual_window = window;
            n.virtual_from_end = from_end;
            if self.node(id).virtual_estimate.is_some() {
                self.invalidate_layout(id);
            }
        }
    }

    /// How far a scroll container can scroll.
    pub fn max_scroll(&self, id: NodeId) -> (f32, f32) {
        let n = self.node(id);
        let l = n.layout;
        let inner_w = l.size.width - l.border.left - l.border.right;
        let inner_h = l.size.height - l.border.top - l.border.bottom;
        let mut cw = l.scrollable_overflow_rect.right;
        let mut ch = l.scrollable_overflow_rect.bottom;
        for &c in &n.layout_children {
            let cl = self.node(c).layout;
            cw = cw.max(
                cl.location.x + cl.size.width + cl.margin.right - l.border.left + l.padding.right,
            );
            ch = ch.max(
                cl.location.y + cl.size.height + cl.margin.bottom - l.border.top + l.padding.bottom,
            );
        }
        ((cw - inner_w).max(0.0), (ch - inner_h).max(0.0))
    }

    pub fn scroll_offset(&self, id: NodeId) -> (f32, f32) {
        self.node(id).scroll
    }

    /// Scroll a scroll container. Virtualized lists lay out again for the new window.
    pub fn set_scroll_offset(&mut self, id: NodeId, x: f32, y: f32) {
        let (mx, my) = self.max_scroll(id);
        let s = (x.clamp(0.0, mx), y.clamp(0.0, my));
        if self.node(id).scroll == s {
            return;
        }
        self.node_mut(id).scroll = s;
        if s == (0.0, 0.0) {
            self.scrolled.remove(&id);
        } else {
            self.scrolled.insert(id);
        }
        if self.node(id).virtual_estimate.is_some() {
            self.invalidate_layout(id);
        }
    }

    /// A virtualized vertical stack: children are stacked with the container's `row-gap` and
    /// their own margins (no collapsing), and only those near the visible window are laid out.
    fn compute_virtual_layout(&mut self, id: NodeId, inputs: LayoutInput) -> LayoutOutput {
        let LayoutInput {
            known_dimensions,
            parent_size,
            available_space,
            run_mode,
            ..
        } = inputs;
        let ts = self.node(id).taffy_style.clone();
        let estimate = self.node(id).virtual_estimate.unwrap_or(40.0);
        let margin = ts.margin.resolve_or_zero(parent_size.width, resolve_calc);
        let padding = ts.padding.resolve_or_zero(parent_size.width, resolve_calc);
        let border = ts.border.resolve_or_zero(parent_size.width, resolve_calc);
        let pb = padding + border;
        let size_style = ts.size.maybe_resolve(parent_size, resolve_calc);
        let border_box = ts.box_sizing == taffy::BoxSizing::BorderBox;
        let adj = |v: Option<f32>, sum: f32| v.map(|v| if border_box { v } else { v + sum });
        let width = known_dimensions
            .width
            .or(adj(size_style.width, pb.horizontal_axis_sum()))
            .or(available_space
                .width
                .into_option()
                .maybe_sub(margin.horizontal_axis_sum()))
            .unwrap_or(0.0);
        let height = known_dimensions
            .height
            .or(adj(size_style.height, pb.vertical_axis_sum()));
        let inner_w = (width - pb.horizontal_axis_sum()).max(0.0);
        let gap = ts.gap.height.resolve_or_zero(Some(inner_w), resolve_calc);
        // A container that scrolls itself shows its own height; one an ancestor scrolls (its
        // window not set yet) shows at most a viewport, even when a flex parent gives it its
        // full content height.
        let scrolls = self
            .node(id)
            .style
            .as_ref()
            .is_some_and(|s| s.is_scroll_container());
        let view_h = height
            .filter(|_| scrolls)
            .map(|h| h - pb.vertical_axis_sum())
            .unwrap_or(self.viewport.1);
        let overscan = view_h * 0.5;
        let scroll_y = self.node(id).scroll.1;
        // The views move a window they set once the view is a tenth of a viewport away from
        // where it was set: that much overscan keeps what's shown laid out. One kept at the end
        // is distances up from the end (widened the same way), placed once the end is known.
        let over = self.viewport.1 * 0.1;
        let from_end = self.node(id).virtual_from_end;
        let window = match self.node(id).virtual_window {
            Some((top, bottom)) if from_end => (top + over, bottom - over),
            Some((top, bottom)) => (top - over, bottom + over),
            None => (scroll_y - overscan, scroll_y + view_h + overscan),
        };

        // Flex and grid parents size a box several times per layout with available heights a
        // list's height doesn't depend on: within one layout run, the same width, height and
        // window give the same size, so only the first walks the children.
        let key = (
            self.layout_runs,
            width.to_bits(),
            height.map(f32::to_bits),
            window.0.to_bits(),
            window.1.to_bits(),
            from_end,
        );
        if run_mode == RunMode::ComputeSize
            && let Some((k, out)) = self.node(id).virt_memo
            && k == key
        {
            return out;
        }
        let kids: Vec<NodeId> = self.node(id).layout_children.clone();
        let n = kids.len();
        let pl = run_mode == RunMode::PerformLayout;
        let mut measured = std::mem::take(&mut self.node_mut(id).measured);
        let mut st = self.node_mut(id).virt.take().unwrap_or_default();
        measured.resize(n, None);
        // The measured heights' sum, count and last (kept as heights are measured).
        if st.mlen != n || n == 0 {
            if st.mlen > n || st.mlen == 0 {
                let (sum, count) = measured
                    .iter()
                    .flatten()
                    .fold((0.0f64, 0usize), |(s, c), h| (s + f64::from(*h), c + 1));
                st.msum = sum;
                st.mcount = count;
                st.mlast = measured.iter().rposition(Option::is_some);
            }
            st.mlen = n;
        }
        // Margins resolve against the content width: another width reads every row again.
        if st.w != inner_w.to_bits() {
            st.rows.clear();
            st.abs.clear();
            st.w = inner_w.to_bits();
        }
        if st.rows.len() > n {
            for r in st.rows.drain(n..) {
                if r.abs {
                    st.abs.retain(|&a| a != r.id);
                }
            }
        }
        let row_of = |doc: &Document, c: NodeId| {
            let ts = &doc.node(c).taffy_style;
            let m = ts.margin.resolve_or_zero(Some(inner_w), resolve_calc);
            VirtRow {
                id: c,
                abs: ts.position == taffy::Position::Absolute,
                margin: [m.top, m.bottom, m.left, m.right],
                stored: None,
                y0: 0.0,
                h: 0.0,
            }
        };
        // Rows that may not be the children at their index any more: read again.
        let old_len = st.rows.len();
        st.pos_valid = st.pos_valid.min(old_len);
        st.store_valid = st.store_valid.min(old_len);
        let from = st.check_from.min(old_len);
        for (i, &c) in kids.iter().enumerate().take(old_len).skip(from) {
            if st.rows[i].id != c {
                let old = st.rows[i];
                if old.abs {
                    st.abs.retain(|&a| a != old.id);
                }
                st.rows[i] = row_of(self, c);
                if st.rows[i].abs {
                    st.abs.push(c);
                }
                st.pos_valid = st.pos_valid.min(i);
                st.store_valid = st.store_valid.min(i);
            }
        }
        for &c in &kids[old_len..] {
            let r = row_of(self, c);
            if r.abs {
                st.abs.push(c);
            }
            st.rows.push(r);
        }
        st.check_from = n;
        // Children past the last one ever laid out (a list that grows at the end: a
        // transcript) are estimated at the mean height of those laid out, so a view following
        // the end finds them about where they land. Earlier ones keep the given estimate: their
        // places don't move as the mean does.
        let tail_estimate = if st.mcount > 0 {
            (st.msum / st.mcount as f64) as f32
        } else {
            estimate
        };
        let last = st.mlast;
        let params = (
            gap.to_bits(),
            estimate.to_bits(),
            tail_estimate.to_bits(),
            last,
        );
        if params != st.params {
            let from = if (params.0, params.1) != (st.params.0, st.params.1) {
                0
            } else {
                match (st.params.3, last) {
                    (Some(a), Some(b)) => a.min(b) + 1,
                    (Some(a), None) | (None, Some(a)) => a + 1,
                    (None, None) => n,
                }
            };
            st.pos_valid = st.pos_valid.min(from);
            st.store_valid = st.store_valid.min(from);
            st.params = params;
        }
        let end_of = |st: &crate::dom::VirtState, i: usize| {
            if i == 0 {
                0.0
            } else {
                st.rows[i - 1].end(i - 1, gap)
            }
        };
        // A window kept at the end (a list the view follows as it grows) goes by where the end
        // is: estimated from what's known, then, if laying out what's there moved it, again.
        let h_of = |measured: &[Option<f32>], i: usize| {
            measured[i].unwrap_or(if last.is_some_and(|l| i > l) {
                tail_estimate
            } else {
                estimate
            })
        };
        let mut end_guess = 0.0f32;
        if from_end {
            let mut y = end_of(&st, st.pos_valid);
            for i in st.pos_valid..n {
                let r = &st.rows[i];
                if r.abs {
                    continue;
                }
                if i > 0 {
                    y += gap;
                }
                y += r.margin[0] + h_of(&measured, i) + r.margin[1];
            }
            end_guess = y;
        }
        let mut content_h;
        let mut attempts = 0;
        loop {
            let window = if from_end {
                (end_guess - window.0, end_guess - window.1)
            } else {
                window
            };
            // Rows before the first that reaches the window's top are out of view: start there, or
            // where places or stored boxes need redoing, or (storing boxes) where what was laid out
            // last time begins.
            let pos_valid = st.pos_valid;
            let first = st.rows[..pos_valid].partition_point(|r| r.end(0, gap) < window.0);
            // (`end` with the gap: rows past the first.)
            let first = st.rows[..pos_valid]
                .iter()
                .enumerate()
                .skip(first.saturating_sub(1))
                .find(|(i, r)| r.end(*i, gap) >= window.0)
                .map_or(pos_valid, |(i, _)| i);
            let mut start = first.min(pos_valid);
            if pl {
                start = start.min(st.store_valid).min(st.vis.0);
            }
            let mut y = end_of(&st, start);
            let mut i = start;
            let mut shown: Option<(usize, usize)> = None;
            let mut changed_from = usize::MAX;
            while i < n {
                // Past the window and what was laid out last time, rows whose places and stored
                // boxes hold are as they were: go on from the first that needs work.
                if y > window.1 && (!pl || i >= st.vis.1) && i < st.pos_valid && st.rows[i].y0 == y
                {
                    let j = if pl {
                        st.pos_valid.min(st.store_valid.max(i))
                    } else {
                        st.pos_valid
                    };
                    if j > i {
                        y = end_of(&st, j);
                        i = j;
                        continue;
                    }
                }
                let c = kids[i];
                let row = st.rows[i];
                if row.abs {
                    st.rows[i].y0 = y;
                    i += 1;
                    continue;
                }
                let y0 = y;
                let mut cm = row.margin;
                if i > 0 {
                    y += gap;
                }
                y += cm[0];
                let h = h_of(&measured, i);
                let visible = y + h >= window.0 && y <= window.1;
                // A child styled only now that it comes into view (restyle leaves the ones out of
                // view to layout).
                if visible && self.style_pending(c) {
                    if !self.style_in_view(id, c) {
                        // It left the flow: the container's boxes are rebuilt next layout.
                        self.needs_relayout = true;
                        y -= cm[0];
                        st.rows[i].id = VirtRow::STALE;
                        st.check_from = st.check_from.min(i);
                        changed_from = changed_from.min(i);
                        i += 1;
                        continue;
                    }
                    let fresh = row_of(self, c);
                    if fresh.abs != row.abs {
                        self.needs_relayout = true;
                    }
                    st.rows[i].margin = fresh.margin;
                    y -= cm[0];
                    cm = fresh.margin;
                    y += cm[0];
                }
                let cw = (inner_w - cm[2] - cm[3]).max(0.0);
                let took;
                if visible {
                    let out = self.compute_child_layout(
                        c.taffy(),
                        LayoutInput {
                            known_dimensions: Size {
                                width: Some(cw),
                                height: None,
                            },
                            known_dimensions_are_definite: Size {
                                width: true,
                                height: true,
                            },
                            parent_size: Size {
                                width: Some(inner_w),
                                height,
                            },
                            available_space: Size {
                                width: AvailableSpace::Definite(cw),
                                height: AvailableSpace::MaxContent,
                            },
                            sizing_mode: SizingMode::InherentSize,
                            axis: taffy::RequestedAxis::Both,
                            run_mode,
                            vertical_margins_are_collapsible: taffy::Line::FALSE,
                        },
                    );
                    let new = out.size.height;
                    match measured[i] {
                        Some(old) => st.msum += f64::from(new) - f64::from(old),
                        None => {
                            st.msum += f64::from(new);
                            st.mcount += 1;
                            st.mlast = st.mlast.max(Some(i));
                        }
                    }
                    measured[i] = Some(new);
                    if pl {
                        self.store_layout(c, pb.left + cm[2], pb.top + y, out.size, inner_w);
                        st.rows[i].stored = None;
                    }
                    shown = Some(shown.map_or((i, i + 1), |(a, _)| (a, i + 1)));
                    took = new;
                } else {
                    // An estimated box, stored unless it already is.
                    let b = [pb.left + cm[2], pb.top + y, cw, h];
                    if pl && st.rows[i].stored != Some(b) {
                        self.store_layout(
                            c,
                            b[0],
                            b[1],
                            Size {
                                width: cw,
                                height: h,
                            },
                            inner_w,
                        );
                        self.node_mut(c).skipped = true;
                        st.rows[i].stored = Some(b);
                    }
                    took = h;
                }
                y += took + cm[1];
                if i >= st.pos_valid || row.y0 != y0 || row.h != took || row.margin != cm {
                    changed_from = changed_from.min(i);
                }
                st.rows[i].y0 = y0;
                st.rows[i].h = took;
                i += 1;
            }
            content_h = y;
            st.pos_valid = n;
            if pl {
                st.store_valid = n;
                st.vis = shown.unwrap_or((n, n));
            } else {
                st.store_valid = st.store_valid.min(changed_from);
            }
            attempts += 1;
            if !from_end || attempts >= 3 || (content_h - end_guess).abs() <= over * 0.5 {
                break;
            }
            end_guess = content_h;
        }
        let abs = st.abs.clone();
        let n = self.node_mut(id);
        n.virt = Some(st);
        n.measured = measured;
        let outer_h = height.unwrap_or(content_h + pb.vertical_axis_sum());
        let size = Size {
            width,
            height: outer_h,
        };
        if pl {
            for c in abs {
                if self.try_node(c).is_some() {
                    layout_abspos(self, id, c, size, border);
                }
            }
            self.node_mut(id).layout.scrollable_overflow_rect = taffy::Rect {
                left: 0.0,
                top: 0.0,
                right: width,
                bottom: content_h + pb.vertical_axis_sum(),
            };
        }
        let out = LayoutOutput {
            size,
            scrollable_overflow_rect: taffy::Rect {
                left: 0.0,
                top: 0.0,
                right: width,
                bottom: content_h + pb.vertical_axis_sum(),
            },
            baselines: taffy::Baselines::NONE,
            top_margin: taffy::CollapsibleMarginSet::ZERO,
            bottom_margin: taffy::CollapsibleMarginSet::ZERO,
            margins_can_collapse_through: false,
        };
        self.node_mut(id).virt_memo = Some((key, out));
        out
    }
}

/// Lay out an absolutely positioned box against its containing block's padding box, for
/// containing blocks we lay out ourselves (inline formatting contexts, tables, virtual lists).
pub(crate) fn layout_abspos(
    doc: &mut Document,
    _cb: NodeId,
    child: NodeId,
    cb_size: Size<f32>,
    cb_border: taffy::Rect<f32>,
) {
    let ts = doc.node(child).taffy_style.clone();
    let pw = cb_size.width - cb_border.left - cb_border.right;
    let ph = cb_size.height - cb_border.top - cb_border.bottom;
    let inset = taffy::Rect {
        left: ts.inset.left.maybe_resolve(Some(pw), resolve_calc),
        right: ts.inset.right.maybe_resolve(Some(pw), resolve_calc),
        top: ts.inset.top.maybe_resolve(Some(ph), resolve_calc),
        bottom: ts.inset.bottom.maybe_resolve(Some(ph), resolve_calc),
    };
    let margin = ts.margin.resolve_or_zero(Some(pw), resolve_calc);
    let pb = ts.padding.resolve_or_zero(Some(pw), resolve_calc)
        + ts.border.resolve_or_zero(Some(pw), resolve_calc);
    let adj = if ts.box_sizing == taffy::BoxSizing::ContentBox {
        pb.sum_axes()
    } else {
        Size::ZERO
    };
    let style_size = ts
        .size
        .maybe_resolve(
            Size {
                width: Some(pw),
                height: Some(ph),
            },
            resolve_calc,
        )
        .maybe_add(adj);
    let known_w = style_size.width.or(match (inset.left, inset.right) {
        (Some(l), Some(r)) => Some((pw - l - r - margin.left - margin.right).max(0.0)),
        _ => None,
    });
    let known_h = style_size.height.or(match (inset.top, inset.bottom) {
        (Some(t), Some(b)) => Some((ph - t - b - margin.top - margin.bottom).max(0.0)),
        _ => None,
    });
    let avail_w =
        (pw - inset.left.unwrap_or(0.0) - inset.right.unwrap_or(0.0) - margin.left - margin.right)
            .max(0.0);
    let out = doc.compute_child_layout(
        child.taffy(),
        LayoutInput {
            known_dimensions: Size {
                width: known_w,
                height: known_h,
            },
            known_dimensions_are_definite: Size {
                width: true,
                height: true,
            },
            parent_size: Size {
                width: Some(pw),
                height: Some(ph),
            },
            available_space: Size {
                width: AvailableSpace::Definite(avail_w),
                height: AvailableSpace::Definite(ph),
            },
            sizing_mode: SizingMode::InherentSize,
            axis: taffy::RequestedAxis::Both,
            run_mode: RunMode::PerformLayout,
            vertical_margins_are_collapsible: taffy::Line::FALSE,
        },
    );
    let x = match (inset.left, inset.right) {
        (Some(l), _) => cb_border.left + l + margin.left,
        (None, Some(r)) => cb_border.left + pw - r - margin.right - out.size.width,
        _ => cb_border.left + margin.left,
    };
    let y = match (inset.top, inset.bottom) {
        (Some(t), _) => cb_border.top + t + margin.top,
        (None, Some(b)) => cb_border.top + ph - b - margin.bottom - out.size.height,
        _ => cb_border.top + margin.top,
    };
    doc.store_layout(child, x, y, out.size, pw);
}

/// The border box of a node in document coordinates.
pub(crate) fn border_box(doc: &Document, id: NodeId) -> Option<Rect> {
    let n = doc.try_node(id)?;
    match n.inner {
        Inner::None | Inner::Contents | Inner::Text => return None,
        Inner::InlineBox => {
            let root = n.layout_parent?;
            let origin = box_origin(doc, root)?;
            let mut it = n.fragments.iter();
            let first = it.next()?;
            let r = it.fold(*first, |acc, f| acc.union(f));
            return Some(r.translate(origin.0, origin.1));
        }
        _ => {}
    }
    if matches!(n.kind, NodeKind::Text(_)) {
        return None;
    }
    let (x, y) = box_origin(doc, id)?;
    Some(Rect::new(x, y, n.layout.size.width, n.layout.size.height))
}

/// The paint-time offset of a `position: sticky` box (§3.6: sticky insets apply at paint time):
/// it stays within its nearest scrolling ancestor's visible area, inside its containing block.
pub(crate) fn sticky_offset(doc: &Document, id: NodeId) -> (f32, f32) {
    let n = doc.node(id);
    let Some(s) = n.style.as_deref() else {
        return (0.0, 0.0);
    };
    if s.position != crate::style::Position::Sticky {
        return (0.0, 0.0);
    }
    let Some(parent) = n.layout_parent else {
        return (0.0, 0.0);
    };
    // Position within the scroller's scrolled content, and the scroller.
    let mut y = n.layout.location.y;
    let mut cur = parent;
    let scroller = loop {
        let cn = doc.node(cur);
        let clips = cn.style.as_ref().is_some_and(|s| s.overflow_y.clips());
        if clips || cn.layout_parent.is_none() {
            break cur;
        }
        y += cn.layout.location.y;
        cur = cn.layout_parent.expect("layout parent");
    };
    let sn = doc.node(scroller);
    let sl = &sn.layout;
    let view_top = sn.scroll.1;
    let view_h = if sn.layout_parent.is_none() {
        doc.viewport().1
    } else {
        sl.size.height - sl.border.top - sl.border.bottom
    };
    let y = y - sl.border.top;
    let h = n.layout.size.height;
    // The containing block (the parent's content box) bounds the offset.
    let pn = doc.node(parent);
    let parent_y = y - n.layout.location.y
        + if parent == scroller {
            sl.border.top
        } else {
            0.0
        };
    let cb_top = parent_y + pn.layout.border.top + pn.layout.padding.top;
    let cb_bottom =
        parent_y + pn.layout.size.height - pn.layout.border.bottom - pn.layout.padding.bottom;
    let basis = view_h;
    let mut dy = 0.0f32;
    if let crate::style::LengthPctAuto::Length(t) = &s.inset[0] {
        let want = view_top + t.resolve(basis) - y;
        if want > 0.0 {
            dy = want.min((cb_bottom - (y + h)).max(0.0));
        }
    } else if let crate::style::LengthPctAuto::Length(b) = &s.inset[2] {
        let want = (view_top + view_h - b.resolve(basis)) - (y + h);
        if want < 0.0 {
            dy = want.max((cb_top - y).min(0.0));
        }
    }
    (0.0, dy)
}

/// The document position of a box's border-box origin.
pub(crate) fn box_origin(doc: &Document, id: NodeId) -> Option<(f32, f32)> {
    let mut x = 0.0;
    let mut y = 0.0;
    let mut cur = id;
    loop {
        let n = doc.try_node(cur)?;
        let (sx, sy) = sticky_offset(doc, cur);
        x += n.layout.location.x + sx;
        y += n.layout.location.y + sy;
        let Some(p) = n.layout_parent else {
            if cur == doc.root() {
                return Some((x, y));
            }
            return None;
        };
        let pn = doc.node(p);
        x -= pn.scroll.0;
        y -= pn.scroll.1;
        cur = p;
    }
}
