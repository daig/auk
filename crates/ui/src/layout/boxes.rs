//! Box tree construction: which nodes generate boxes, anonymous blocks, inline formatting
//! contexts, hoisting of absolutely positioned boxes to their containing block, and the Taffy
//! style of every box.

use crate::css::values::{CalcNode, LengthPct};
use crate::document::Document;
use crate::dom::{Inner, NodeId, NodeKind, flags};
use crate::style::*;
use std::sync::Arc;
use taffy::prelude::{
    AlignContent, AlignItems, Dimension, GridPlacement, GridTemplateComponent,
    MaxTrackSizingFunction, MinTrackSizingFunction,
};

/// Calc expressions referenced from a Taffy style by pointer; kept alive alongside it.
pub(crate) type CalcKeep = Vec<Arc<CalcNode>>;

#[derive(Clone, Copy, PartialEq)]
enum Class {
    Skip,
    Ws,
    Inline,
    Block,
}

impl Document {
    /// Rebuild the box structure wherever it is marked dirty.
    pub(crate) fn build_boxes(&mut self) {
        let root = self.root();
        if self.node(root).style.is_none() {
            return;
        }
        let style = self.node(root).style.clone().expect("root style");
        let inner = inner_for(style.display.blockified(), false);
        if self.node(root).inner != inner {
            self.node_mut(root).inner = inner;
            self.node_mut(root).flags |= flags::BOX_DIRTY;
        }
        let (ts, keep) = to_taffy(&style, inner, false, false);
        let n = self.node_mut(root);
        n.taffy_style = ts;
        n.calc_keep = keep;
        n.taffy_sig = None;
        self.build_rec(root);
    }

    fn build_rec(&mut self, id: NodeId) {
        let f = self.node(id).flags;
        if f & (flags::BOX_DIRTY | flags::BOX_DESC) == 0 {
            return;
        }
        if f & flags::BOX_DIRTY != 0 {
            if f & flags::BOX_FULL != 0 || !self.extend_virtual_children(id) {
                self.build_children(id);
            }
        }
        self.node_mut(id).flags &= !(flags::BOX_DIRTY | flags::BOX_DESC | flags::BOX_FULL);
        // Only children with box work (a long list has a few).
        let mut i = 0;
        while let Some(&c) = self.node(id).layout_children.get(i) {
            if self.node(c).flags & (flags::BOX_DIRTY | flags::BOX_DESC) != 0 {
                self.build_rec(c);
            }
            i += 1;
        }
        // Inline boxes inside a formatting context have no layout children but may carry flags.
        if self.node(id).inner == Inner::Ifc {
            let mut stack = self.box_children(id);
            while let Some(c) = stack.pop() {
                let n = self.node_mut(c);
                if n.inner == Inner::InlineBox
                    || n.inner == Inner::Text
                    || n.inner == Inner::Contents
                {
                    n.flags &= !(flags::BOX_DIRTY | flags::BOX_DESC);
                    if n.inner != Inner::Text {
                        stack.extend(self.box_children(c));
                    }
                }
            }
        }
    }

    /// The nodes a box's children come from: `::before`, DOM children (through
    /// `display: contents`), `::after`. Anonymous blocks list their run directly.
    pub(crate) fn box_children(&self, id: NodeId) -> Vec<NodeId> {
        let n = self.node(id);
        if matches!(n.kind, NodeKind::Anonymous) {
            return n.children.clone();
        }
        let mut out = Vec::with_capacity(n.children.len() + 2);
        self.push_box_children(id, &mut out);
        out
    }

    fn push_box_children(&self, id: NodeId, out: &mut Vec<NodeId>) {
        let n = self.node(id);
        if let Some(b) = n.before {
            out.push(b);
        }
        for &c in &n.children {
            let cn = self.node(c);
            if cn.is_element()
                && cn
                    .style
                    .as_ref()
                    .is_some_and(|s| s.display == Display::Contents)
            {
                self.push_box_children(c, out);
            } else {
                out.push(c);
            }
        }
        if let Some(a) = n.after {
            out.push(a);
        }
    }

    fn classify(&self, c: NodeId, parent_ws: WhiteSpace) -> Class {
        let n = self.node(c);
        match &n.kind {
            NodeKind::Text(t) => {
                if t.is_empty() {
                    Class::Skip
                } else if parent_ws.collapses_spaces()
                    && t.chars()
                        .all(|ch| matches!(ch, ' ' | '\t' | '\n' | '\r' | '\x0c'))
                {
                    Class::Ws
                } else {
                    Class::Inline
                }
            }
            NodeKind::Anonymous => Class::Block,
            NodeKind::Element(_) | NodeKind::Pseudo(_) => {
                let Some(s) = n.style.as_ref() else {
                    return Class::Skip;
                };
                if s.display == Display::None || s.is_out_of_flow() {
                    Class::Skip
                } else if s.display.is_inline_level() {
                    Class::Inline
                } else {
                    Class::Block
                }
            }
        }
    }

    fn style_of(&self, id: NodeId) -> Arc<ComputedStyle> {
        self.node(id)
            .style
            .clone()
            .unwrap_or_else(|| self.initial.clone())
    }

    fn set_child_box(
        &mut self,
        parent: NodeId,
        child: NodeId,
        inner: Inner,
        item: bool,
        abspos: bool,
    ) {
        let style = self.style_of(child);
        let sig = (Arc::as_ptr(&style) as usize, inner, item, abspos);
        let n = self.node_mut(child);
        if n.inner != inner {
            n.flags |= flags::BOX_DIRTY;
            n.cache.clear();
        }
        n.layout_parent = Some(parent);
        // Rebuilding a big parent's children shouldn't redo every child's Taffy style.
        if n.taffy_sig == Some(sig) {
            return;
        }
        let (ts, keep) = to_taffy(&style, inner, item, abspos);
        let n = self.node_mut(child);
        n.inner = inner;
        n.taffy_style = ts;
        n.calc_keep = keep;
        n.taffy_sig = Some(sig);
        n.cache.clear();
    }

    fn free_anons(&mut self, id: NodeId) {
        let anons = std::mem::take(&mut self.node_mut(id).anon_children);
        for a in anons {
            if self.try_node(a).is_some() {
                // The run's nodes belong to the DOM; only the anonymous box goes.
                self.node_mut(a).children.clear();
                self.free_node(a);
            }
        }
    }

    fn new_anon(&mut self, parent: NodeId, run: Vec<NodeId>) -> NodeId {
        let pstyle = self.style_of(parent);
        let mut s = ComputedStyle::inherit_from(&pstyle, &self.initial);
        s.display = Display::Block;
        let a = self.create_anonymous();
        let an = self.node_mut(a);
        an.children = run;
        an.style = Some(Arc::new(s));
        an.flags |= flags::BOX_DIRTY;
        self.node_mut(parent).anon_children.push(a);
        a
    }

    /// A virtualized container's child styled as layout brings it into view: its box as the
    /// container's build gives a styled child.
    pub(crate) fn set_virtual_child_box(&mut self, container: NodeId, c: NodeId) {
        let item = matches!(self.node(container).inner, Inner::Flex | Inner::Grid);
        let d = self.style_of(c).display;
        let inner = if item {
            inner_for(d.blockified(), true)
        } else {
            inner_for(d, false)
        };
        self.set_child_box(container, c, inner, item, false);
    }

    /// Builds the boxes under a virtualized container's child styled as it came into view.
    pub(crate) fn build_virtual_child(&mut self, c: NodeId) {
        self.build_rec(c);
    }

    /// How box building treats a child: a virtualized container's element children not styled
    /// yet (waiting out of view, `Document::style_waits`) stand in as blocks.
    fn classify_in(&self, parent: NodeId, c: NodeId, parent_ws: WhiteSpace) -> Class {
        let n = self.node(c);
        if n.style.is_none() && n.is_element() && self.node(parent).virtual_estimate.is_some() {
            return Class::Block;
        }
        self.classify(c, parent_ws)
    }

    /// Children added at the end of a virtualized container whose layout children are exactly
    /// its box children, all blocks (`Node::virt_kids`): their boxes join without rebuilding
    /// the others, so a transcript growing by a few entries costs those entries. Returns false
    /// when anything else changed (the full rebuild then runs).
    fn extend_virtual_children(&mut self, id: NodeId) -> bool {
        if self.node(id).virtual_estimate.is_none() {
            return false;
        }
        let n = self.node(id);
        let Some(old) = n.virt_kids.as_ref() else {
            return false;
        };
        // Its box children are its DOM children (no `::before`/`::after`; `display: contents`
        // children are checked below), the old ones first.
        if n.before.is_some()
            || n.after.is_some()
            || n.children.len() < old.len()
            || n.children[..old.len()] != old[..]
        {
            return false;
        }
        let kids = n.children.clone();
        let tail = kids[old.len()..].to_vec();
        let inner = self.node(id).inner;
        let item = matches!(inner, Inner::Flex | Inner::Grid);
        let positioned = self.node(id).style.as_ref().is_some_and(|s| s.is_positioned());
        let ws = self.style_of(id).white_space;
        for &c in &tail {
            if self.classify_in(id, c, ws) != Class::Block
                || matches!(self.node(c).kind, NodeKind::Anonymous)
                || self
                    .node(c)
                    .style
                    .as_ref()
                    .is_some_and(|s| s.display == Display::Contents)
                || self.node(c).style.as_ref().is_some_and(|s| s.order != 0)
            {
                return false;
            }
            if positioned {
                let mut found = Vec::new();
                self.collect_abspos_of(c, &mut found);
                if !found.is_empty() {
                    return false;
                }
            }
        }
        for &c in &tail {
            if item {
                self.set_virtual_child_box(id, c);
            } else if self.node(c).style.is_none() {
                self.set_child_box(id, c, Inner::Flow, false, false);
            } else {
                self.set_virtual_child_box(id, c);
            }
        }
        let n = self.node_mut(id);
        n.layout_children.extend_from_slice(&tail);
        n.virt_kids = Some(kids);
        n.cache.clear();
        true
    }

    /// `collect_abspos` for one child's subtree (the child itself included).
    fn collect_abspos_of(&mut self, c: NodeId, out: &mut Vec<NodeId>) {
        let n = self.node(c);
        let Some(s) = n.style.as_ref() else { return };
        if s.display == Display::None {
            return;
        }
        if s.is_out_of_flow() {
            out.push(c);
        } else if !s.is_positioned() {
            self.collect_abspos(c, out);
        }
    }

    fn build_children(&mut self, id: NodeId) {
        self.free_anons(id);
        let style = self.style_of(id);
        let inner = self.node(id).inner;
        let mut layout_children: Vec<NodeId> = Vec::new();
        let kids = self.box_children(id);
        match inner {
            Inner::Flow | Inner::Ifc => {
                let classes: Vec<Class> = kids
                    .iter()
                    .map(|c| self.classify_in(id, *c, style.white_space))
                    .collect();
                let any_block = classes.contains(&Class::Block);
                let any_inline = classes.contains(&Class::Inline);
                if any_block {
                    self.node_mut(id).inner = Inner::Flow;
                    let mut run: Vec<NodeId> = Vec::new();
                    let mut run_has_content = false;
                    let flush = |doc: &mut Document,
                                 run: &mut Vec<NodeId>,
                                 has: &mut bool,
                                 out: &mut Vec<NodeId>| {
                        if *has {
                            let a = doc.new_anon(id, std::mem::take(run));
                            out.push(a);
                        }
                        run.clear();
                        *has = false;
                    };
                    for (c, cl) in kids.iter().zip(classes.iter()) {
                        match cl {
                            Class::Skip => {}
                            Class::Ws => run.push(*c),
                            Class::Inline => {
                                run.push(*c);
                                run_has_content = true;
                            }
                            Class::Block => {
                                flush(self, &mut run, &mut run_has_content, &mut layout_children);
                                layout_children.push(*c);
                            }
                        }
                    }
                    flush(self, &mut run, &mut run_has_content, &mut layout_children);
                    for c in layout_children.clone() {
                        if matches!(self.node(c).kind, NodeKind::Anonymous) {
                            self.set_child_box(id, c, Inner::Flow, false, false);
                        } else if self.node(c).style.is_none() {
                            // A virtualized container's child waiting to be styled.
                            self.set_child_box(id, c, Inner::Flow, false, false);
                        } else {
                            let d = self.style_of(c).display;
                            self.set_child_box(id, c, inner_for(d, false), false, false);
                        }
                    }
                } else if any_inline {
                    self.node_mut(id).inner = Inner::Ifc;
                    self.node_mut(id).flags |= flags::IFC_DIRTY;
                    self.collect_inline(id, id, &kids, style.white_space, &mut layout_children);
                } else {
                    self.node_mut(id).inner = Inner::Flow;
                }
            }
            Inner::Flex | Inner::Grid => {
                let mut run: Vec<NodeId> = Vec::new();
                let mut has = false;
                let mut items: Vec<(NodeId, bool)> = Vec::new();
                for c in &kids {
                    match &self.node(*c).kind {
                        NodeKind::Text(_) => {
                            run.push(*c);
                            if self.classify(*c, style.white_space) == Class::Inline {
                                has = true;
                            }
                        }
                        _ => {
                            if has {
                                let a = self.new_anon(id, std::mem::take(&mut run));
                                items.push((a, true));
                            }
                            run.clear();
                            has = false;
                            if self.classify_in(id, *c, style.white_space) != Class::Skip {
                                items.push((*c, false));
                            }
                        }
                    }
                }
                if has {
                    let a = self.new_anon(id, run);
                    items.push((a, true));
                }
                // `order` (Taffy lays items out in child order).
                items.sort_by_key(|(c, _)| {
                    self.node(*c).style.as_ref().map(|s| s.order).unwrap_or(0)
                });
                for (c, anon) in items {
                    let inner_c = if anon {
                        Inner::Flow
                    } else {
                        inner_for(self.style_of(c).display.blockified(), true)
                    };
                    self.set_child_box(id, c, inner_c, true, false);
                    layout_children.push(c);
                }
            }
            Inner::Table | Inner::TablePart => {
                let parent_display = style.display;
                for c in &kids {
                    let Some(cs) = self.node(*c).style.clone() else {
                        continue;
                    };
                    if !self.node(*c).is_element()
                        || cs.display == Display::None
                        || cs.is_out_of_flow()
                    {
                        continue;
                    }
                    let d = cs.display;
                    let ci = match (parent_display, d) {
                        (Display::Table | Display::InlineTable, d)
                            if d.is_row_group() || d == Display::TableRow =>
                        {
                            Inner::TablePart
                        }
                        (pd, Display::TableRow) if pd.is_row_group() => Inner::TablePart,
                        (Display::TableRow, _) => inner_for(d.blockified(), true),
                        (
                            Display::Table | Display::InlineTable,
                            Display::TableColumn | Display::TableColumnGroup,
                        ) => continue,
                        (_, d) => inner_for(d.blockified(), true),
                    };
                    self.set_child_box(id, *c, ci, false, false);
                    layout_children.push(*c);
                }
            }
            _ => {}
        }
        // Absolutely positioned descendants whose containing block is this box.
        if (style.is_positioned()
            || self.node(id).parent.is_none() && !matches!(self.node(id).kind, NodeKind::Anonymous))
            && !matches!(self.node(id).kind, NodeKind::Anonymous)
        {
            let mut found = Vec::new();
            self.collect_abspos(id, &mut found);
            for c in found {
                let d = self.style_of(c).display.blockified();
                self.set_child_box(id, c, inner_for(d, true), false, true);
                layout_children.push(c);
            }
        }
        // A virtualized container whose boxes are exactly its children, in order, can be
        // extended later (`extend_virtual_children`).
        let simple = self.node(id).virtual_estimate.is_some()
            && self.node(id).anon_children.is_empty()
            && layout_children == kids;
        let n = self.node_mut(id);
        n.layout_children = layout_children;
        n.virt_kids = simple.then_some(kids);
        n.cache.clear();
    }

    fn collect_abspos(&mut self, id: NodeId, out: &mut Vec<NodeId>) {
        for c in self.box_children(id) {
            let n = self.node(c);
            let Some(s) = n.style.as_ref() else { continue };
            if !(n.is_element() || matches!(n.kind, NodeKind::Pseudo(_)))
                || s.display == Display::None
            {
                continue;
            }
            if s.is_out_of_flow() {
                out.push(c);
            } else if !s.is_positioned() {
                // What a subtree passes up stays the same until a box under it changes.
                if let Some(found) = &n.abspos {
                    out.extend_from_slice(found);
                } else {
                    let mut found = Vec::new();
                    self.collect_abspos(c, &mut found);
                    out.extend_from_slice(&found);
                    self.node_mut(c).abspos = Some(found.into_boxed_slice());
                }
            }
        }
    }

    /// Walk inline content: inline boxes and text join the formatting context of `root`; atomic
    /// inlines become its layout children.
    fn collect_inline(
        &mut self,
        root: NodeId,
        _parent: NodeId,
        kids: &[NodeId],
        ws: WhiteSpace,
        atoms: &mut Vec<NodeId>,
    ) {
        for &c in kids {
            match self.classify(c, ws) {
                Class::Skip => {
                    let n = self.node_mut(c);
                    if !n.is_element() {
                        n.layout_parent = Some(root);
                    }
                }
                Class::Ws => {
                    let n = self.node_mut(c);
                    n.inner = Inner::Text;
                    n.layout_parent = Some(root);
                }
                Class::Inline | Class::Block => {
                    if self.node(c).text().is_some() {
                        let n = self.node_mut(c);
                        n.inner = Inner::Text;
                        n.layout_parent = Some(root);
                        continue;
                    }
                    let s = self.style_of(c);
                    if s.display == Display::Inline {
                        let n = self.node_mut(c);
                        n.inner = Inner::InlineBox;
                        n.layout_parent = Some(root);
                        n.cache.clear();
                        let sub = self.box_children(c);
                        self.collect_inline(root, c, &sub, s.white_space, atoms);
                    } else {
                        // Atomic inline (inline-block, inline-flex, …); a block inside an inline
                        // is treated as one too.
                        let inner = inner_for(s.display.blockified(), true);
                        self.set_child_box(root, c, inner, false, false);
                        atoms.push(c);
                    }
                }
            }
        }
    }
}

/// How a box with this display lays out its children.
pub(crate) fn inner_for(d: Display, blockified: bool) -> Inner {
    match d {
        Display::None => Inner::None,
        Display::Contents => Inner::Contents,
        Display::Inline if !blockified => Inner::InlineBox,
        Display::Flex | Display::InlineFlex => Inner::Flex,
        Display::Grid | Display::InlineGrid => Inner::Grid,
        Display::Table | Display::InlineTable => Inner::Table,
        _ => Inner::Flow,
    }
}

fn lp(v: &LengthPct, keep: &mut CalcKeep) -> taffy::LengthPercentage {
    match v {
        LengthPct::Px(p) => taffy::LengthPercentage::length(*p),
        LengthPct::Pct(p) => taffy::LengthPercentage::percent(p / 100.0),
        LengthPct::Calc(c) => {
            keep.push(c.clone());
            taffy::LengthPercentage::calc(Arc::as_ptr(c) as *const ())
        }
    }
}

fn lpa(v: &LengthPctAuto, keep: &mut CalcKeep) -> taffy::LengthPercentageAuto {
    match v {
        LengthPctAuto::Auto => taffy::LengthPercentageAuto::auto(),
        LengthPctAuto::Length(LengthPct::Px(p)) => taffy::LengthPercentageAuto::length(*p),
        LengthPctAuto::Length(LengthPct::Pct(p)) => taffy::LengthPercentageAuto::percent(p / 100.0),
        LengthPctAuto::Length(LengthPct::Calc(c)) => {
            keep.push(c.clone());
            taffy::LengthPercentageAuto::calc(Arc::as_ptr(c) as *const ())
        }
    }
}

fn dim(v: &crate::style::Size, keep: &mut CalcKeep) -> Dimension {
    match v {
        crate::style::Size::Auto | crate::style::Size::None => Dimension::auto(),
        crate::style::Size::MinContent => Dimension::min_content(),
        crate::style::Size::MaxContent => Dimension::max_content(),
        crate::style::Size::FitContent => Dimension::fit_content(),
        crate::style::Size::Length(LengthPct::Px(p)) => Dimension::length(*p),
        crate::style::Size::Length(LengthPct::Pct(p)) => Dimension::percent(p / 100.0),
        crate::style::Size::Length(LengthPct::Calc(c)) => {
            keep.push(c.clone());
            Dimension::calc(Arc::as_ptr(c) as *const ())
        }
    }
}

fn min_max(v: &crate::style::Size, keep: &mut CalcKeep) -> taffy::LengthPercentageAuto {
    match v {
        crate::style::Size::Length(l) => lpa(&LengthPctAuto::Length(l.clone()), keep),
        _ => taffy::LengthPercentageAuto::auto(),
    }
}

fn align_items(a: Align) -> Option<AlignItems> {
    Some(match a {
        Align::Start | Align::Left => AlignItems::START,
        Align::End | Align::Right => AlignItems::END,
        Align::FlexStart => AlignItems::FLEX_START,
        Align::FlexEnd => AlignItems::FLEX_END,
        Align::SelfStart => AlignItems::SELF_START,
        Align::SelfEnd => AlignItems::SELF_END,
        Align::Center => AlignItems::CENTER,
        Align::Baseline | Align::LastBaseline => AlignItems::BASELINE,
        Align::Stretch => AlignItems::STRETCH,
        _ => return None,
    })
}

fn align_content(a: Align) -> Option<AlignContent> {
    Some(match a {
        Align::Start | Align::Left => AlignContent::START,
        Align::End | Align::Right => AlignContent::END,
        Align::FlexStart => AlignContent::FLEX_START,
        Align::FlexEnd => AlignContent::FLEX_END,
        Align::Center => AlignContent::CENTER,
        Align::Stretch => AlignContent::STRETCH,
        Align::SpaceBetween => AlignContent::SPACE_BETWEEN,
        Align::SpaceAround => AlignContent::SPACE_AROUND,
        Align::SpaceEvenly => AlignContent::SPACE_EVENLY,
        _ => return None,
    })
}

fn overflow(o: crate::style::Overflow) -> taffy::Overflow {
    match o {
        crate::style::Overflow::Visible => taffy::Overflow::Visible,
        crate::style::Overflow::Clip => taffy::Overflow::Clip,
        crate::style::Overflow::Hidden | crate::style::Overflow::Auto => taffy::Overflow::Hidden,
        crate::style::Overflow::Scroll => taffy::Overflow::Scroll,
    }
}

fn track_fn(t: &TrackSize, keep: &mut CalcKeep) -> taffy::TrackSizingFunction {
    let min = match &t.min {
        TrackBreadth::Length(l) => match l {
            LengthPct::Px(p) => MinTrackSizingFunction::length(*p),
            LengthPct::Pct(p) => MinTrackSizingFunction::percent(p / 100.0),
            LengthPct::Calc(c) => {
                keep.push(c.clone());
                MinTrackSizingFunction::calc(Arc::as_ptr(c) as *const ())
            }
        },
        TrackBreadth::MinContent => MinTrackSizingFunction::min_content(),
        TrackBreadth::MaxContent => MinTrackSizingFunction::max_content(),
        _ => MinTrackSizingFunction::auto(),
    };
    let max = match &t.max {
        TrackBreadth::Length(l) => match l {
            LengthPct::Px(p) => MaxTrackSizingFunction::length(*p),
            LengthPct::Pct(p) => MaxTrackSizingFunction::percent(p / 100.0),
            LengthPct::Calc(c) => {
                keep.push(c.clone());
                MaxTrackSizingFunction::calc(Arc::as_ptr(c) as *const ())
            }
        },
        TrackBreadth::Fr(f) => MaxTrackSizingFunction::fr(*f),
        TrackBreadth::MinContent => MaxTrackSizingFunction::min_content(),
        TrackBreadth::MaxContent => MaxTrackSizingFunction::max_content(),
        TrackBreadth::FitContent(LengthPct::Px(p)) => MaxTrackSizingFunction::fit_content_px(*p),
        TrackBreadth::FitContent(LengthPct::Pct(p)) => {
            MaxTrackSizingFunction::fit_content_percent(p / 100.0)
        }
        TrackBreadth::FitContent(_) => MaxTrackSizingFunction::max_content(),
        TrackBreadth::Auto => MaxTrackSizingFunction::auto(),
    };
    taffy::MinMax { min, max }
}

fn template(
    list: &TrackList,
    keep: &mut CalcKeep,
) -> (Vec<GridTemplateComponent<String>>, Vec<Vec<String>>) {
    let mut comps = Vec::new();
    let mut names: Vec<Vec<String>> = Vec::new();
    for item in &list.items {
        match item {
            TrackItem::Track(t, n) => {
                names.push(n.iter().map(|s| s.to_string()).collect());
                comps.push(GridTemplateComponent::Single(track_fn(t, keep)));
            }
            TrackItem::Repeat(count, tracks, n) => {
                names.push(Vec::new());
                comps.push(GridTemplateComponent::Repeat(
                    taffy::GridTemplateRepetition {
                        count: match count {
                            RepeatCount::Count(c) => taffy::RepetitionCount::Count(*c),
                            RepeatCount::AutoFill => taffy::RepetitionCount::AutoFill,
                            RepeatCount::AutoFit => taffy::RepetitionCount::AutoFit,
                        },
                        tracks: tracks.iter().map(|t| track_fn(t, keep)).collect(),
                        line_names: n
                            .iter()
                            .map(|v| v.iter().map(|s| s.to_string()).collect())
                            .collect(),
                    },
                ));
            }
        }
    }
    names.push(list.trailing_names.iter().map(|s| s.to_string()).collect());
    if names.iter().all(|n| n.is_empty()) {
        names.clear();
    }
    (comps, names)
}

fn placement(l: &GridLine) -> GridPlacement<String> {
    match l {
        GridLine::Auto => GridPlacement::Auto,
        GridLine::Line(n, None) => GridPlacement::Line((*n).into()),
        GridLine::Line(n, Some(name)) => GridPlacement::NamedLine(name.to_string(), *n),
        GridLine::Span(n, _) => GridPlacement::Span(*n),
        GridLine::Named(name) => GridPlacement::NamedLine(name.to_string(), 1),
    }
}

/// The Taffy style for a box.
pub(crate) fn to_taffy(
    s: &ComputedStyle,
    inner: Inner,
    _item: bool,
    abspos: bool,
) -> (taffy::Style, CalcKeep) {
    let mut keep = CalcKeep::new();
    let mut t = taffy::Style::DEFAULT;
    t.display = match inner {
        Inner::None | Inner::Contents | Inner::InlineBox | Inner::Text => taffy::Display::None,
        Inner::Flex => taffy::Display::Flex,
        Inner::Grid => taffy::Display::Grid,
        Inner::Flow if s.display == Display::FlowRoot => taffy::Display::FlowRoot,
        _ => taffy::Display::Block,
    };
    t.item_is_table = inner == Inner::Table;
    t.box_sizing = match s.box_sizing {
        crate::style::BoxSizing::BorderBox => taffy::BoxSizing::BorderBox,
        crate::style::BoxSizing::ContentBox => taffy::BoxSizing::ContentBox,
    };
    t.overflow = taffy::Point {
        x: overflow(s.overflow_x),
        y: overflow(s.overflow_y),
    };
    t.scrollbar_width = 0.0;
    t.position = if abspos {
        taffy::Position::Absolute
    } else {
        taffy::Position::Relative
    };
    let positioned = abspos
        || matches!(
            s.position,
            crate::style::Position::Relative | crate::style::Position::Sticky
        );
    t.inset = if positioned && s.position != crate::style::Position::Sticky {
        taffy::Rect {
            top: lpa(&s.inset[0], &mut keep),
            right: lpa(&s.inset[1], &mut keep),
            bottom: lpa(&s.inset[2], &mut keep),
            left: lpa(&s.inset[3], &mut keep),
        }
    } else {
        taffy::Rect::auto()
    };
    t.size = taffy::Size {
        width: dim(&s.width, &mut keep),
        height: dim(&s.height, &mut keep),
    };
    t.min_size = taffy::Size {
        width: min_max(&s.min_width, &mut keep),
        height: min_max(&s.min_height, &mut keep),
    };
    t.max_size = taffy::Size {
        width: min_max(&s.max_width, &mut keep),
        height: min_max(&s.max_height, &mut keep),
    };
    t.aspect_ratio = s.aspect_ratio;
    t.margin = taffy::Rect {
        top: lpa(&s.margin[0], &mut keep),
        right: lpa(&s.margin[1], &mut keep),
        bottom: lpa(&s.margin[2], &mut keep),
        left: lpa(&s.margin[3], &mut keep),
    };
    t.padding = taffy::Rect {
        top: lp(&s.padding[0], &mut keep),
        right: lp(&s.padding[1], &mut keep),
        bottom: lp(&s.padding[2], &mut keep),
        left: lp(&s.padding[3], &mut keep),
    };
    let b = s.used_border();
    t.border = taffy::Rect {
        top: taffy::LengthPercentage::length(b[0]),
        right: taffy::LengthPercentage::length(b[1]),
        bottom: taffy::LengthPercentage::length(b[2]),
        left: taffy::LengthPercentage::length(b[3]),
    };
    t.align_items = align_items(s.align_items);
    t.align_self = align_items(s.align_self);
    t.justify_items = align_items(s.justify_items);
    t.justify_self = align_items(s.justify_self);
    t.align_content = align_content(s.align_content);
    t.justify_content = align_content(s.justify_content);
    if inner == Inner::Flow || inner == Inner::Ifc || inner == Inner::Table {
        // `align-content` on a block container would make Taffy start a new formatting context.
        t.align_content = None;
    }
    t.gap = taffy::Size {
        width: s
            .column_gap
            .as_ref()
            .map(|g| lp(g, &mut keep))
            .unwrap_or(taffy::LengthPercentage::length(0.0)),
        height: s
            .row_gap
            .as_ref()
            .map(|g| lp(g, &mut keep))
            .unwrap_or(taffy::LengthPercentage::length(0.0)),
    };
    t.text_align = match s.text_align {
        TextAlign::Center => taffy::TextAlign::Auto,
        _ => taffy::TextAlign::Auto,
    };
    t.flex_direction = match s.flex_direction {
        crate::style::FlexDirection::Row => taffy::FlexDirection::Row,
        crate::style::FlexDirection::RowReverse => taffy::FlexDirection::RowReverse,
        crate::style::FlexDirection::Column => taffy::FlexDirection::Column,
        crate::style::FlexDirection::ColumnReverse => taffy::FlexDirection::ColumnReverse,
    };
    t.flex_wrap = match s.flex_wrap {
        crate::style::FlexWrap::NoWrap => taffy::FlexWrap::NoWrap,
        crate::style::FlexWrap::Wrap => taffy::FlexWrap::Wrap,
        crate::style::FlexWrap::WrapReverse => taffy::FlexWrap::WrapReverse,
    };
    t.flex_grow = s.flex_grow;
    t.flex_shrink = s.flex_shrink;
    t.flex_basis = dim(&s.flex_basis, &mut keep);
    if inner == Inner::Grid {
        let (cols, col_names) = template(&s.grid_template_columns, &mut keep);
        let (rows, row_names) = template(&s.grid_template_rows, &mut keep);
        t.grid_template_columns = cols;
        t.grid_template_rows = rows;
        t.grid_template_column_names = col_names;
        t.grid_template_row_names = row_names;
        t.grid_auto_columns = s
            .grid_auto_columns
            .iter()
            .map(|x| track_fn(x, &mut keep))
            .collect();
        t.grid_auto_rows = s
            .grid_auto_rows
            .iter()
            .map(|x| track_fn(x, &mut keep))
            .collect();
        t.grid_auto_flow = match (s.grid_auto_flow.column, s.grid_auto_flow.dense) {
            (false, false) => taffy::GridAutoFlow::Row,
            (true, false) => taffy::GridAutoFlow::Column,
            (false, true) => taffy::GridAutoFlow::RowDense,
            (true, true) => taffy::GridAutoFlow::ColumnDense,
        };
        if let Some(a) = &s.grid_template_areas {
            t.grid_template_areas = Some(taffy::GridTemplateAreas {
                areas: a
                    .0
                    .iter()
                    .map(|g| taffy::GridTemplateArea {
                        name: g.name.to_string(),
                        row_start: g.row_start,
                        row_end: g.row_end,
                        column_start: g.column_start,
                        column_end: g.column_end,
                    })
                    .collect(),
                row_count: a.1,
                column_count: a.2,
            });
        }
    }
    t.grid_row = taffy::Line {
        start: placement(&s.grid_row_start),
        end: placement(&s.grid_row_end),
    };
    t.grid_column = taffy::Line {
        start: placement(&s.grid_column_start),
        end: placement(&s.grid_column_end),
    };
    (t, keep)
}
