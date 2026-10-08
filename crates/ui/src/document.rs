//! The document: DOM mutation, sheets, restyle and the frame update.

use crate::css::cascade::{SheetKind, Stylist, cascade};
use crate::css::compute::ComputeEnv;
use crate::css::properties::Restrictions;
use crate::css::selector::ElementRef;
use crate::css::sheet::parse_inline_style;
use crate::dom::{
    Atom, ElementData, ElementState, Node, NodeId, NodeKind, PseudoKind, Rect, flags,
};
use crate::style::{ComputedStyle, ContentItem, Display};
use crate::text::Fonts;
use selectors::context::SelectorCaches;
use std::sync::Arc;

const STYLE_SUBTREE: u16 = 64;

/// One DOM + CSS + layout pipeline (§18.2).
pub struct Document {
    pub(crate) nodes: Vec<Option<Node>>,
    free: Vec<u32>,
    root: NodeId,
    pub(crate) stylist: Stylist,
    pub(crate) fonts: Fonts,
    pub(crate) viewport: (f32, f32),
    pub(crate) dark: bool,
    pub(crate) device_scale: f32,
    pub(crate) initial: Arc<ComputedStyle>,
    caches: SelectorCaches,
    /// Inline style restrictions (program-built DOMs use Tern's rules).
    pub inline_style_restrictions: Restrictions,
    pub(crate) layout_dirty: bool,
    pub(crate) stats: UpdateStats,
    /// Scroll containers with a nonzero offset (for scroll anchoring).
    pub(crate) scrolled: std::collections::BTreeSet<NodeId>,
}

/// What the last [`Document::update`] did, for checking that work is incremental.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UpdateStats {
    /// Elements whose style was recomputed.
    pub restyled: usize,
    /// Inline formatting contexts whose text was rebuilt and reshaped.
    pub inline_rebuilt: usize,
    /// Boxes laid out (cache misses), counting each measure pass.
    pub laid_out: usize,
}

impl std::fmt::Debug for Document {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Document")
            .field("nodes", &(self.nodes.len() - self.free.len()))
            .finish()
    }
}

impl Default for Document {
    fn default() -> Self {
        Document::new()
    }
}

impl Document {
    /// A document with the system fonts and the user-agent sheet, whose root is an `html` element.
    pub fn new() -> Document {
        Document::with_fonts(Fonts::new())
    }

    pub fn with_fonts(fonts: Fonts) -> Document {
        let mut doc = Document {
            nodes: Vec::new(),
            free: Vec::new(),
            root: NodeId(0),
            stylist: Stylist::default(),
            fonts,
            viewport: (800.0, 600.0),
            dark: false,
            device_scale: 2.0,
            initial: Arc::new(ComputedStyle::default()),
            caches: SelectorCaches::default(),
            inline_style_restrictions: Restrictions::NONE,
            layout_dirty: true,
            stats: UpdateStats::default(),
            scrolled: Default::default(),
        };
        doc.root = doc.create_element("html");
        doc.stylist
            .set_sheet("ua", SheetKind::BuiltIn, None, crate::ua::UA_CSS);
        doc.stylist.media.width = doc.viewport.0;
        doc.stylist.media.height = doc.viewport.1;
        doc
    }

    pub fn root(&self) -> NodeId {
        self.root
    }

    pub fn fonts_mut(&mut self) -> &mut Fonts {
        &mut self.fonts
    }

    pub fn node(&self, id: NodeId) -> &Node {
        self.nodes[id.index()].as_ref().expect("live node")
    }

    pub(crate) fn node_mut(&mut self, id: NodeId) -> &mut Node {
        self.nodes[id.index()].as_mut().expect("live node")
    }

    pub fn try_node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id.index()).and_then(|n| n.as_ref())
    }

    fn alloc(&mut self, node: Node) -> NodeId {
        if let Some(i) = self.free.pop() {
            self.nodes[i as usize] = Some(node);
            NodeId(i)
        } else {
            self.nodes.push(Some(node));
            NodeId(self.nodes.len() as u32 - 1)
        }
    }

    /// Create a detached element.
    pub fn create_element(&mut self, tag: &str) -> NodeId {
        self.alloc(Node::new(NodeKind::Element(ElementData {
            tag: Atom::new(&tag.to_ascii_lowercase()),
            id: None,
            classes: Vec::new(),
            attrs: Vec::new(),
            inline_style: None,
            state: ElementState::default(),
        })))
    }

    /// Create a detached text node.
    pub fn create_text(&mut self, text: &str) -> NodeId {
        self.alloc(Node::new(NodeKind::Text(text.to_string())))
    }

    pub(crate) fn create_anonymous(&mut self) -> NodeId {
        self.alloc(Node::new(NodeKind::Anonymous))
    }

    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.node(id).parent
    }

    pub fn children(&self, id: NodeId) -> &[NodeId] {
        &self.node(id).children
    }

    /// Element children only.
    pub fn element_children(&self, id: NodeId) -> Vec<NodeId> {
        self.node(id)
            .children
            .iter()
            .copied()
            .filter(|c| self.node(*c).is_element())
            .collect()
    }

    pub fn append_child(&mut self, parent: NodeId, child: NodeId) {
        self.insert_before(parent, child, None);
    }

    /// Insert `child` before `before` (or at the end), moving it if it is attached elsewhere.
    pub fn insert_before(&mut self, parent: NodeId, child: NodeId, before: Option<NodeId>) {
        if self.node(child).parent.is_some() {
            self.detach(child);
        }
        let at = match before {
            Some(b) if self.node(b).parent == Some(parent) => self.node(b).index_in_parent as usize,
            _ => self.node(parent).children.len(),
        };
        self.node_mut(parent).children.insert(at, child);
        self.node_mut(child).parent = Some(parent);
        self.reindex(parent, at);
        self.structure_changed(parent, at);
        self.mark_subtree_style(child);
    }

    /// Detach a node (and its subtree) from its parent without freeing it.
    pub fn detach(&mut self, child: NodeId) {
        let Some(parent) = self.node(child).parent else {
            return;
        };
        let at = self.node(child).index_in_parent as usize;
        if self.subtree_has_out_of_flow(child) {
            self.mark_containing_block_dirty(child);
        }
        self.invalidate_layout(child);
        self.node_mut(parent).children.remove(at);
        let n = self.node_mut(child);
        n.parent = None;
        n.layout_parent = None;
        self.reindex(parent, at);
        self.structure_changed(parent, at.saturating_sub(1));
    }

    /// Detach and free a node and its subtree.
    pub fn remove(&mut self, id: NodeId) {
        self.detach(id);
        self.free_subtree(id);
    }

    fn free_subtree(&mut self, id: NodeId) {
        let n = self.nodes[id.index()].take().expect("live node");
        for c in n.children.iter().chain(n.anon_children.iter()) {
            if self
                .try_node(*c)
                .is_some_and(|cn| cn.parent == Some(id) || matches!(cn.kind, NodeKind::Anonymous))
            {
                self.free_subtree(*c);
            }
        }
        for p in [n.before, n.after].into_iter().flatten() {
            if self.try_node(p).is_some() {
                self.free_subtree(p);
            }
        }
        self.free.push(id.0);
    }

    /// Free one node (not its children).
    pub(crate) fn free_node(&mut self, id: NodeId) {
        if self.nodes[id.index()].take().is_some() {
            self.free.push(id.0);
        }
    }

    fn reindex(&mut self, parent: NodeId, from: usize) {
        let children = std::mem::take(&mut self.node_mut(parent).children);
        for (i, c) in children.iter().enumerate().skip(from) {
            self.node_mut(*c).index_in_parent = i as u32;
        }
        self.node_mut(parent).children = children;
    }

    /// After children change: the parent's boxes and its neighbours' styles (sibling selectors).
    fn structure_changed(&mut self, parent: NodeId, at: usize) {
        self.mark_box_dirty(parent);
        self.invalidate_layout(parent);
        let kids = &self.node(parent).children;
        let neighbours: Vec<NodeId> = [at.checked_sub(1), Some(at), Some(at + 1)]
            .into_iter()
            .flatten()
            .filter_map(|i| kids.get(i).copied())
            .collect();
        for n in neighbours {
            self.mark_style_dirty(n, true);
        }
        // `:empty` and the parent's own pseudo-elements.
        self.mark_style_dirty(parent, false);
    }

    /// Replace the text of a text node.
    pub fn set_text(&mut self, id: NodeId, text: &str) {
        if let NodeKind::Text(t) = &mut self.node_mut(id).kind {
            if t == text {
                return;
            }
            *t = text.to_string();
        }
        self.invalidate_layout(id);
    }

    pub fn attribute(&self, id: NodeId, name: &str) -> Option<&str> {
        let e = self.node(id).element()?;
        match name {
            "class" => None,
            "id" => e.id.as_ref().map(|a| a.as_str()),
            _ => e.attr(name),
        }
    }

    /// Set an attribute. `class`, `id` and `style` are interpreted.
    pub fn set_attribute(&mut self, id: NodeId, name: &str, value: &str) {
        let name = name.to_ascii_lowercase();
        let restrictions = self.inline_style_restrictions;
        let Some(e) = self.node_mut(id).element_mut() else {
            return;
        };
        match name.as_str() {
            "class" => e.classes = value.split_ascii_whitespace().map(Atom::new).collect(),
            "id" => e.id = Some(Atom::new(value)),
            "style" => {
                let (block, _errors) = parse_inline_style(value, restrictions);
                e.inline_style = Some(Arc::new(block));
            }
            _ => match e.attrs.iter_mut().find(|(n, _)| n.as_str() == name) {
                Some((_, v)) if v == value => return,
                Some((_, v)) => *v = value.to_string(),
                None => e.attrs.push((Atom::new(&name), value.to_string())),
            },
        }
        self.attribute_changed(id);
    }

    pub fn remove_attribute(&mut self, id: NodeId, name: &str) {
        let Some(e) = self.node_mut(id).element_mut() else {
            return;
        };
        match name {
            "class" => e.classes.clear(),
            "id" => e.id = None,
            "style" => e.inline_style = None,
            _ => e.attrs.retain(|(n, _)| n.as_str() != name),
        }
        self.attribute_changed(id);
    }

    /// Set the inline style of an element, returning parse errors.
    pub fn set_inline_style(&mut self, id: NodeId, css: &str) -> Vec<String> {
        let (block, errors) = parse_inline_style(css, self.inline_style_restrictions);
        if let Some(e) = self.node_mut(id).element_mut() {
            e.inline_style = if block.decls.is_empty() {
                None
            } else {
                Some(Arc::new(block))
            };
        }
        self.mark_style_dirty(id, false);
        errors
    }

    pub fn add_class(&mut self, id: NodeId, class: &str) {
        if let Some(e) = self.node_mut(id).element_mut() {
            if e.classes.iter().any(|c| c.as_str() == class) {
                return;
            }
            e.classes.push(Atom::new(class));
        }
        self.attribute_changed(id);
    }

    pub fn remove_class(&mut self, id: NodeId, class: &str) {
        if let Some(e) = self.node_mut(id).element_mut() {
            e.classes.retain(|c| c.as_str() != class);
        }
        self.attribute_changed(id);
    }

    fn attribute_changed(&mut self, id: NodeId) {
        // Descendant and sibling selectors can depend on it.
        self.mark_style_dirty(id, true);
        if let Some(p) = self.node(id).parent {
            let at = self.node(id).index_in_parent as usize;
            if let Some(next) = self.node(p).children.get(at + 1).copied() {
                self.mark_style_dirty(next, true);
            }
        }
    }

    /// Set or clear interaction state (`:hover`, `:focus`, `:checked`, …).
    pub fn set_state(&mut self, id: NodeId, state: ElementState, on: bool) {
        let Some(e) = self.node_mut(id).element_mut() else {
            return;
        };
        let before = e.state;
        e.state = if on {
            ElementState(e.state.0 | state.0)
        } else {
            ElementState(e.state.0 & !state.0)
        };
        if e.state != before {
            self.attribute_changed(id);
        }
    }

    /// Install or replace a stylesheet. Program sheets take the surface's `data-scope` value.
    /// Returns the errors in Tern's format.
    pub fn set_stylesheet(
        &mut self,
        name: &str,
        kind: SheetKind,
        scope: Option<&str>,
        css: &str,
    ) -> Vec<String> {
        let (_, errors) = self.stylist.set_sheet(name, kind, scope, css);
        self.mark_all_styles_dirty();
        errors
    }

    pub fn remove_stylesheet(&mut self, name: &str, scope: Option<&str>) {
        if self.stylist.remove_sheet(name, scope) {
            self.mark_all_styles_dirty();
        }
    }

    pub fn set_viewport(&mut self, width: f32, height: f32) {
        if self.viewport == (width, height) {
            return;
        }
        self.viewport = (width, height);
        self.stylist.media.width = width;
        self.stylist.media.height = height;
        // vw/vh units and media queries.
        self.mark_all_styles_dirty();
        self.invalidate_layout(self.root);
    }

    pub fn viewport(&self) -> (f32, f32) {
        self.viewport
    }

    /// Device pixels per CSS px.
    pub fn device_scale(&self) -> f32 {
        self.device_scale
    }

    /// Gives an element replaced content: it lays out with the picture's natural size and ratio
    /// when its own size is `auto`, and paints the picture in its content box (`object-fit`,
    /// `image-rendering`). `None` makes it an ordinary element again. A new picture of the same
    /// natural size (an animation's next frame) only repaints.
    pub fn set_image(&mut self, id: NodeId, image: Option<crate::image::ImageContent>) {
        let n = self.node_mut(id);
        let relayout = n.image.as_ref().map(|i| i.natural) != image.as_ref().map(|i| i.natural);
        n.image = image.map(Box::new);
        if relayout {
            self.invalidate_layout(id);
        }
    }

    /// An element's replaced content.
    pub fn image(&self, id: NodeId) -> Option<&crate::image::ImageContent> {
        self.node(id).image.as_deref()
    }

    /// Device pixels per CSS px (2 on Retina displays). Border widths snap to device pixels.
    pub fn set_device_scale(&mut self, scale: f32) {
        if self.device_scale != scale {
            self.device_scale = scale;
            self.mark_all_styles_dirty();
        }
    }

    pub fn set_dark(&mut self, dark: bool) {
        if self.dark != dark {
            self.dark = dark;
            self.mark_all_styles_dirty();
        }
    }

    /// Media features: reduced motion, reduced transparency, more contrast.
    pub fn set_media_preferences(
        &mut self,
        reduced_motion: bool,
        reduced_transparency: bool,
        more_contrast: bool,
    ) {
        self.stylist.media.reduced_motion = reduced_motion;
        self.stylist.media.reduced_transparency = reduced_transparency;
        self.stylist.media.more_contrast = more_contrast;
        self.mark_all_styles_dirty();
    }

    pub fn computed_style(&self, id: NodeId) -> Option<&ComputedStyle> {
        self.node(id).style.as_deref()
    }

    fn mark_all_styles_dirty(&mut self) {
        let root = self.root;
        self.node_mut(root).flags |= flags::STYLE_DIRTY | STYLE_SUBTREE;
    }

    fn mark_subtree_style(&mut self, id: NodeId) {
        self.mark_style_dirty(id, true);
    }

    pub(crate) fn mark_style_dirty(&mut self, id: NodeId, subtree: bool) {
        let n = self.node_mut(id);
        n.flags |= flags::STYLE_DIRTY | if subtree { STYLE_SUBTREE } else { 0 };
        let mut cur = n.parent;
        while let Some(p) = cur {
            let pn = self.node_mut(p);
            if pn.flags & flags::STYLE_DESC != 0 {
                break;
            }
            pn.flags |= flags::STYLE_DESC;
            cur = pn.parent;
        }
    }

    /// Mark a node's box structure (its layout children) for rebuilding, along with the nearest
    /// block container, which holds the boxes of inline content.
    pub(crate) fn mark_box_dirty(&mut self, id: NodeId) {
        self.node_mut(id).flags |= flags::BOX_DIRTY;
        let mut cur = self.node(id).parent;
        let mut need_block = matches!(
            self.node(id).inner,
            crate::dom::Inner::InlineBox | crate::dom::Inner::Contents | crate::dom::Inner::None
        );
        while let Some(p) = cur {
            let pn = self.node_mut(p);
            if need_block {
                pn.flags |= flags::BOX_DIRTY;
            }
            pn.flags |= flags::BOX_DESC;
            if !matches!(
                pn.inner,
                crate::dom::Inner::InlineBox | crate::dom::Inner::Contents
            ) {
                need_block = false;
            }
            cur = pn.parent;
        }
        self.layout_dirty = true;
    }

    /// Mark the containing block of absolutely positioned boxes at or under `id` (the nearest
    /// positioned ancestor, or the root) for rebuilding, since it holds their boxes.
    pub(crate) fn mark_containing_block_dirty(&mut self, id: NodeId) {
        let mut cur = self.node(id).parent;
        while let Some(p) = cur {
            let pn = self.node(p);
            if pn.parent.is_none() || pn.style.as_ref().is_some_and(|s| s.is_positioned()) {
                self.mark_box_dirty(p);
                return;
            }
            cur = pn.parent;
        }
    }

    fn subtree_has_out_of_flow(&self, id: NodeId) -> bool {
        let n = self.node(id);
        n.style.as_ref().is_some_and(|s| s.is_out_of_flow())
            || n.children.iter().any(|c| self.subtree_has_out_of_flow(*c))
    }

    /// Clear cached layout from a node up to the root, and mark its formatting context for a
    /// rebuild of its inline content.
    pub(crate) fn invalidate_layout(&mut self, id: NodeId) {
        self.layout_dirty = true;
        let mut cur = Some(id);
        let mut first = true;
        while let Some(c) = cur {
            let n = self.node_mut(c);
            n.cache.clear();
            if first {
                n.flags |= flags::IFC_DIRTY;
            }
            first = false;
            let lp = n.layout_parent;
            let parent = n.parent;
            if let Some(lp) = lp {
                let ln = self.node_mut(lp);
                ln.cache.clear();
                ln.flags |= flags::IFC_DIRTY;
                if matches!(ln.kind, NodeKind::Anonymous) {
                    // Anonymous blocks aren't DOM ancestors; continue from their parent.
                    cur = ln.layout_parent.or(parent);
                    continue;
                }
            }
            cur = parent.or(lp);
            if cur == Some(c) {
                break;
            }
        }
    }

    // ---- Restyle ----

    pub(crate) fn restyle(&mut self) {
        self.stylist.prepare();
        let root = self.root;
        let rn = self.node(root);
        if rn.flags & (flags::STYLE_DIRTY | flags::STYLE_DESC) == 0 {
            return;
        }
        let initial = self.initial.clone();
        let mut caches = std::mem::take(&mut self.caches);
        self.restyle_node(root, &initial, false, None, &mut caches);
        self.caches = caches;
    }

    fn restyle_node(
        &mut self,
        id: NodeId,
        parent_style: &Arc<ComputedStyle>,
        force: bool,
        scope: Option<(NodeId, Atom)>,
        caches: &mut SelectorCaches,
    ) {
        let f = self.node(id).flags;
        let dirty = force || f & flags::STYLE_DIRTY != 0;
        let mut force_children = force || f & STYLE_SUBTREE != 0;
        let is_element = self.node(id).is_element();
        let mut style = self.node(id).style.clone();
        if dirty {
            if is_element {
                self.stats.restyled += 1;
                let new = Arc::new(self.compute_element_style(
                    id,
                    None,
                    parent_style,
                    scope.as_ref(),
                    caches,
                ));
                let changed = style.as_deref() != Some(&*new);
                if changed {
                    let layout_changed = style.as_deref().is_none_or(|old| !layout_eq(old, &new));
                    self.node_mut(id).style = Some(new.clone());
                    if layout_changed {
                        self.mark_box_dirty(id);
                        if let Some(p) = self.node(id).parent {
                            self.mark_box_dirty(p);
                        }
                        let old_oof = style.as_ref().is_some_and(|s| s.is_out_of_flow());
                        let old_pos = style.as_ref().is_some_and(|s| s.is_positioned());
                        if old_oof || new.is_out_of_flow() || old_pos != new.is_positioned() {
                            self.mark_containing_block_dirty(id);
                            if old_pos != new.is_positioned() {
                                // Its absolutely positioned descendants change containing block.
                                self.mark_box_dirty(id);
                            }
                        }
                        self.invalidate_layout(id);
                    }
                    force_children = true;
                }
                style = Some(new);
                self.update_pseudos(id, scope.as_ref(), caches);
            } else if !style.as_ref().is_some_and(|s| Arc::ptr_eq(s, parent_style)) {
                self.node_mut(id).style = Some(parent_style.clone());
                style = Some(parent_style.clone());
                self.invalidate_layout(id);
            }
        }
        let n = self.node_mut(id);
        n.flags &= !(flags::STYLE_DIRTY | STYLE_SUBTREE);
        let visit_children = force_children || f & flags::STYLE_DESC != 0;
        n.flags &= !flags::STYLE_DESC;
        if !visit_children {
            return;
        }
        let my_style = style.unwrap_or_else(|| parent_style.clone());
        let child_scope = match self.node(id).element().and_then(|e| e.attr("data-scope")) {
            Some(v) => Some((id, Atom::new(v))),
            None => scope,
        };
        let children = self.node(id).children.clone();
        for c in children {
            self.restyle_node(c, &my_style, force_children, child_scope.clone(), caches);
        }
        // Pseudo-element children follow their element.
        for p in [self.node(id).before, self.node(id).after]
            .into_iter()
            .flatten()
        {
            if let Some(ps) = self.node(p).style.clone() {
                for c in self.node(p).children.clone() {
                    if self
                        .node(c)
                        .style
                        .as_ref()
                        .is_none_or(|s| !Arc::ptr_eq(s, &ps))
                    {
                        self.node_mut(c).style = Some(ps.clone());
                        self.invalidate_layout(c);
                    }
                }
            }
        }
    }

    fn compute_element_style(
        &mut self,
        id: NodeId,
        pseudo: Option<PseudoKind>,
        parent_style: &ComputedStyle,
        scope: Option<&(NodeId, Atom)>,
        caches: &mut SelectorCaches,
    ) -> ComputedStyle {
        let mut blocks = Vec::new();
        let el = ElementRef {
            doc: self,
            id,
            scope_root: None,
        };
        self.stylist
            .matched_blocks(el, pseudo, scope.map(|(n, a)| (*n, a)), caches, &mut blocks);
        let inline = if pseudo.is_none() {
            self.node(id).element().and_then(|e| e.inline_style.clone())
        } else {
            None
        };
        let root_fs = if id == self.root && pseudo.is_none() {
            16.0
        } else {
            self.node(self.root)
                .style
                .as_ref()
                .map(|s| s.font_size)
                .unwrap_or(16.0)
        };
        let env = ComputeEnv {
            root_font_size: root_fs,
            vw: self.viewport.0,
            vh: self.viewport.1,
            dark: self.dark,
            dpr: self.device_scale,
        };
        let attr = |name: &str| self.attribute(id, name).map(|s| s.to_string());
        cascade(
            &blocks,
            inline.as_deref(),
            parent_style,
            &self.initial,
            &env,
            &attr,
        )
    }

    fn update_pseudos(
        &mut self,
        id: NodeId,
        scope: Option<&(NodeId, Atom)>,
        caches: &mut SelectorCaches,
    ) {
        let Some(style) = self.node(id).style.clone() else {
            return;
        };
        for kind in [PseudoKind::Before, PseudoKind::After] {
            let ps = self.compute_element_style(id, Some(kind), &style, scope, caches);
            let wanted = ps.content.is_some() && ps.display != Display::None;
            let existing = match kind {
                PseudoKind::Before => self.node(id).before,
                PseudoKind::After => self.node(id).after,
            };
            if !wanted {
                if let Some(p) = existing {
                    self.free_subtree(p);
                    match kind {
                        PseudoKind::Before => self.node_mut(id).before = None,
                        PseudoKind::After => self.node_mut(id).after = None,
                    }
                    self.mark_box_dirty(id);
                    self.invalidate_layout(id);
                }
                continue;
            }
            let text: String = ps
                .content
                .as_ref()
                .map(|items| {
                    items
                        .iter()
                        .map(|i| match i {
                            ContentItem::Text(t) => t.to_string(),
                            ContentItem::Attr(_) => String::new(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            let ps = Arc::new(ps);
            let pid = match existing {
                Some(p) => p,
                None => {
                    let p = self.alloc(Node::new(NodeKind::Pseudo(kind)));
                    let t = self.create_text("");
                    self.node_mut(p).children.push(t);
                    self.node_mut(t).parent = Some(p);
                    self.node_mut(p).parent = Some(id);
                    match kind {
                        PseudoKind::Before => self.node_mut(id).before = Some(p),
                        PseudoKind::After => self.node_mut(id).after = Some(p),
                    }
                    self.mark_box_dirty(id);
                    p
                }
            };
            let old = self.node(pid).style.clone();
            if old.as_deref() != Some(&*ps) {
                self.node_mut(pid).style = Some(ps.clone());
                self.mark_box_dirty(pid);
                self.invalidate_layout(pid);
            }
            let t = self.node(pid).children[0];
            if self.node(t).text() != Some(text.as_str()) {
                self.node_mut(t).kind = NodeKind::Text(text);
                self.invalidate_layout(t);
            }
            self.node_mut(t).style = Some(ps.clone());
            self.node_mut(t).flags &= !flags::STYLE_DIRTY;
            self.node_mut(pid).flags &= !flags::STYLE_DIRTY;
        }
    }

    /// Restyle and lay out whatever changed.
    pub fn update(&mut self) {
        self.stats = UpdateStats::default();
        self.restyle();
        self.layout();
    }

    /// A measure-then-layout loop (§8.3): update, then let `adjust` read the layout and change
    /// the document; repeat while it reports a change, at most `max_passes` times. Returns the
    /// number of layout passes run.
    pub fn update_with(
        &mut self,
        max_passes: usize,
        mut adjust: impl FnMut(&mut Document) -> bool,
    ) -> usize {
        self.update();
        let mut passes = 1;
        while passes < max_passes.max(1) && adjust(self) {
            self.update();
            passes += 1;
        }
        passes
    }

    /// Lines of text laid out in a box's inline formatting context (for clamps such as
    /// "N more lines"); 0 if it has none.
    pub fn line_count(&self, id: NodeId) -> usize {
        let n = self.node(id);
        let ifc = match n.inner {
            crate::dom::Inner::Ifc => Some(id),
            _ => n
                .layout_children
                .iter()
                .copied()
                .find(|c| matches!(self.node(*c).kind, NodeKind::Anonymous)),
        };
        ifc.and_then(|i| self.node(i).inline.as_ref())
            .map(|il| il.lines.iter().filter(|l| l.height > 0.0).count())
            .unwrap_or(0)
    }

    /// The min-content and max-content border-box widths of a laid-out box.
    pub fn intrinsic_widths(&mut self, id: NodeId) -> (f32, f32) {
        use taffy::{
            AvailableSpace, LayoutInput, LayoutPartialTree, RequestedAxis, RunMode, Size,
            SizingMode,
        };
        let mut measure = |avail: AvailableSpace| {
            self.compute_child_layout(
                id.taffy(),
                LayoutInput {
                    known_dimensions: Size::NONE,
                    known_dimensions_are_definite: Size {
                        width: true,
                        height: true,
                    },
                    parent_size: Size::NONE,
                    available_space: Size {
                        width: avail,
                        height: AvailableSpace::MaxContent,
                    },
                    sizing_mode: SizingMode::InherentSize,
                    axis: RequestedAxis::Horizontal,
                    run_mode: RunMode::ComputeSize,
                    vertical_margins_are_collapsible: taffy::Line::FALSE,
                },
            )
            .size
            .width
        };
        (
            measure(AvailableSpace::MinContent),
            measure(AvailableSpace::MaxContent),
        )
    }

    /// The size of a box's scrollable content (at least its padding box).
    pub fn scroll_size(&self, id: NodeId) -> (f32, f32) {
        let l = self.node(id).layout;
        let (mx, my) = self.max_scroll(id);
        (
            l.size.width - l.border.left - l.border.right + mx,
            l.size.height - l.border.top - l.border.bottom + my,
        )
    }

    /// A `@keyframes` rule by name, looking first in the scope's program sheets.
    pub fn keyframes(
        &self,
        name: &str,
        scope: Option<&str>,
    ) -> Option<&crate::css::sheet::Keyframes> {
        self.stylist.keyframes(name, scope)
    }

    /// Counters from the last [`Document::update`].
    pub fn last_update_stats(&self) -> UpdateStats {
        self.stats
    }

    /// The border box of an element in document coordinates (scroll offsets of ancestors applied),
    /// as `getBoundingClientRect()` plus the page scroll would report it. Inline boxes report the
    /// union of their fragments. Elements without boxes report None.
    pub fn border_box(&self, id: NodeId) -> Option<Rect> {
        crate::layout::border_box(self, id)
    }
}

/// Whether two styles lay out the same (they may paint differently).
fn layout_eq(a: &ComputedStyle, b: &ComputedStyle) -> bool {
    let mut b2 = b.clone();
    b2.color = a.color;
    b2.background_color = a.background_color;
    b2.border_color = a.border_color;
    b2.opacity = a.opacity;
    b2.cursor = a.cursor.clone();
    b2.outline_color = a.outline_color;
    b2.outline_style = a.outline_style;
    b2.outline_width = a.outline_width;
    b2.outline_offset = a.outline_offset;
    b2.visibility = a.visibility;
    b2.transitions = a.transitions.clone();
    b2.animations = a.animations.clone();
    b2.text_decoration = a.text_decoration;
    b2.text_decoration_color = a.text_decoration_color;
    b2.transform = a.transform.clone();
    b2.z_index = a.z_index;
    b2.pointer_events = a.pointer_events;
    b2.custom = a.custom.clone();
    *a == b2
}
