//! The element tree: an arena of nodes with cheap incremental mutation and dirty tracking.

use crate::css::sheet::DeclBlock;
use crate::layout::inline::InlineLayout;
use crate::style::ComputedStyle;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// A node in a [`crate::Document`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub(crate) u32);

impl NodeId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
    pub(crate) fn taffy(self) -> taffy::NodeId {
        taffy::NodeId::from(self.0 as u64)
    }
    pub(crate) fn from_taffy(id: taffy::NodeId) -> NodeId {
        NodeId(u64::from(id) as u32)
    }
}

impl fmt::Debug for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// An interned-style string with a precomputed hash, used for tags, classes, ids and attribute
/// names.
#[derive(Clone, Eq)]
pub struct Atom(pub(crate) Arc<str>, pub(crate) u32);

impl Atom {
    pub fn new(s: &str) -> Atom {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        s.hash(&mut h);
        Atom(Arc::from(s), h.finish() as u32)
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl PartialEq for Atom {
    fn eq(&self, other: &Atom) -> bool {
        self.1 == other.1 && self.0 == other.0
    }
}
impl Hash for Atom {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u32(self.1)
    }
}
impl fmt::Debug for Atom {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", &*self.0)
    }
}
impl std::borrow::Borrow<str> for Atom {
    fn borrow(&self) -> &str {
        &self.0
    }
}
impl<'a> From<&'a str> for Atom {
    fn from(s: &'a str) -> Atom {
        Atom::new(s)
    }
}
impl AsRef<str> for Atom {
    fn as_ref(&self) -> &str {
        &self.0
    }
}
impl precomputed_hash::PrecomputedHash for Atom {
    fn precomputed_hash(&self) -> u32 {
        self.1
    }
}
impl cssparser::ToCss for Atom {
    fn to_css<W: fmt::Write>(&self, dest: &mut W) -> fmt::Result {
        cssparser::serialize_identifier(&self.0, dest)
    }
}
impl Default for Atom {
    fn default() -> Atom {
        Atom::new("")
    }
}

/// Interaction state that drives `:hover`, `:active`, `:focus`, `:checked` and `:disabled`.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct ElementState(pub u16);

impl ElementState {
    pub const HOVER: ElementState = ElementState(1);
    pub const ACTIVE: ElementState = ElementState(2);
    pub const FOCUS: ElementState = ElementState(4);
    pub const FOCUS_VISIBLE: ElementState = ElementState(8);
    pub const FOCUS_WITHIN: ElementState = ElementState(16);
    pub const CHECKED: ElementState = ElementState(32);
    pub const DISABLED: ElementState = ElementState(64);
    pub fn contains(self, other: ElementState) -> bool {
        self.0 & other.0 == other.0
    }
}

/// `::before` or `::after`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum PseudoKind {
    Before,
    After,
}

#[derive(Debug)]
pub struct ElementData {
    pub tag: Atom,
    pub id: Option<Atom>,
    pub classes: Vec<Atom>,
    pub attrs: Vec<(Atom, String)>,
    pub(crate) inline_style: Option<Arc<DeclBlock>>,
    pub state: ElementState,
}

impl ElementData {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(n, _)| n.as_str() == name)
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Debug)]
pub enum NodeKind {
    Element(ElementData),
    Text(String),
    /// An anonymous block box wrapping a run of inline content; its `children` are the DOM nodes
    /// of the run, which keep their real DOM parent.
    Anonymous,
    /// A `::before` or `::after` box; its child is a generated text node.
    Pseudo(PseudoKind),
}

/// How a box lays out its children.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Inner {
    /// Not rendered (display none, or not yet built).
    None,
    /// Block container holding block-level children (Taffy block layout).
    Flow,
    /// Block container holding inline content (our inline layout).
    Ifc,
    Flex,
    Grid,
    Table,
    /// An inline box inside an inline formatting context.
    InlineBox,
    /// Laid out by the enclosing table.
    TablePart,
    /// Text, laid out by its inline formatting context.
    Text,
    /// `display: contents`.
    Contents,
}

pub(crate) mod flags {
    pub const STYLE_DIRTY: u16 = 1;
    pub const STYLE_DESC: u16 = 2;
    pub const BOX_DIRTY: u16 = 4;
    pub const BOX_DESC: u16 = 8;
    pub const IFC_DIRTY: u16 = 16;
}

/// A rectangle in CSS px.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
    pub fn union(&self, o: &Rect) -> Rect {
        let x0 = self.x.min(o.x);
        let y0 = self.y.min(o.y);
        let x1 = (self.x + self.w).max(o.x + o.w);
        let y1 = (self.y + self.h).max(o.y + o.h);
        Rect {
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
        }
    }
    pub fn intersects(&self, o: &Rect) -> bool {
        self.x < o.x + o.w && o.x < self.x + self.w && self.y < o.y + o.h && o.y < self.y + self.h
    }
    pub fn translate(&self, dx: f32, dy: f32) -> Rect {
        Rect {
            x: self.x + dx,
            y: self.y + dy,
            ..*self
        }
    }
}

#[derive(Debug)]
pub struct Node {
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    pub kind: NodeKind,
    pub(crate) index_in_parent: u32,
    pub(crate) style: Option<Arc<ComputedStyle>>,
    pub(crate) before: Option<NodeId>,
    pub(crate) after: Option<NodeId>,
    pub(crate) flags: u16,
    // Box tree.
    pub(crate) inner: Inner,
    pub(crate) layout_children: Vec<NodeId>,
    pub(crate) layout_parent: Option<NodeId>,
    pub(crate) anon_children: Vec<NodeId>,
    pub(crate) taffy_style: taffy::Style,
    pub(crate) calc_keep: crate::layout::boxes::CalcKeep,
    /// What `taffy_style` was built from: the style's address, inner display, item, abspos.
    pub(crate) taffy_sig: Option<(usize, Inner, bool, bool)>,
    pub(crate) cache: taffy::Cache,
    /// Unrounded layout relative to the layout parent's border box.
    pub(crate) layout: taffy::Layout,
    pub(crate) inline: Option<Box<InlineLayout>>,
    /// Border-box fragments of an inline box, relative to its formatting context root.
    pub(crate) fragments: Vec<Rect>,
    pub(crate) scroll: (f32, f32),
    /// Virtualization: an estimated height for children not laid out (see [`crate::Document::set_virtual`]).
    pub(crate) virtual_estimate: Option<f32>,
    /// Measured border-box heights of virtualized children, by child index.
    pub(crate) measured: Vec<Option<f32>>,
    /// Whether this child was laid out in the last pass (virtualized lists skip children).
    pub(crate) skipped: bool,
}

impl Node {
    pub(crate) fn new(kind: NodeKind) -> Node {
        Node {
            parent: None,
            children: Vec::new(),
            kind,
            index_in_parent: 0,
            style: None,
            before: None,
            after: None,
            flags: flags::STYLE_DIRTY | flags::BOX_DIRTY | flags::IFC_DIRTY,
            inner: Inner::None,
            layout_children: Vec::new(),
            layout_parent: None,
            anon_children: Vec::new(),
            taffy_style: taffy::Style::DEFAULT,
            calc_keep: Vec::new(),
            taffy_sig: None,
            cache: taffy::Cache::new(),
            layout: taffy::Layout::new(),
            inline: None,
            fragments: Vec::new(),
            scroll: (0.0, 0.0),
            virtual_estimate: None,
            measured: Vec::new(),
            skipped: false,
        }
    }

    pub fn element(&self) -> Option<&ElementData> {
        match &self.kind {
            NodeKind::Element(e) => Some(e),
            _ => None,
        }
    }
    pub(crate) fn element_mut(&mut self) -> Option<&mut ElementData> {
        match &mut self.kind {
            NodeKind::Element(e) => Some(e),
            _ => None,
        }
    }
    pub fn is_element(&self) -> bool {
        matches!(self.kind, NodeKind::Element(_))
    }
    pub fn text(&self) -> Option<&str> {
        match &self.kind {
            NodeKind::Text(t) => Some(t),
            _ => None,
        }
    }
    pub fn style(&self) -> Option<&ComputedStyle> {
        self.style.as_deref()
    }
}
