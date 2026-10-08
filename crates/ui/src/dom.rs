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
    /// The box's children are rebuilt from scratch, not only extended (`BOX_DIRTY` from a
    /// change other than children added or removed).
    pub const BOX_FULL: u16 = 32;
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
    /// The visible part of a virtualized container in its content coordinates, when an ancestor
    /// scrolls it (see [`crate::Document::set_virtual_window`]).
    pub(crate) virtual_window: Option<(f32, f32)>,
    /// `virtual_window` is distances up from the container's end (it follows the end).
    pub(crate) virtual_from_end: bool,
    /// Measured border-box heights of virtualized children, by child index.
    pub(crate) measured: Vec<Option<f32>>,
    /// Whether this child was laid out in the last pass (virtualized lists skip children).
    pub(crate) skipped: bool,
    /// Replaced content: a picture painted into the content box ([`crate::Document::set_image`]).
    pub(crate) image: Option<Box<crate::image::ImageContent>>,
    /// The absolutely positioned boxes this in-flow, non-positioned box passes up to its
    /// containing block (box building's `collect_abspos`), kept until something under it changes
    /// its boxes ([`crate::Document::mark_box_dirty`] clears it on the way up), so rebuilding a
    /// big containing block doesn't walk every subtree under it.
    pub(crate) abspos: Option<Box<[NodeId]>>,
    /// The layout run ([`crate::Document::layout_count`]) that last computed this box (a cache
    /// miss); 0 if none has.
    pub(crate) laid_out_in: u64,
    /// A virtualized container whose layout children are exactly its box children, all blocks
    /// (the common case: a transcript's entries): those children, so children added at the end
    /// extend its boxes without rebuilding them all.
    pub(crate) virt_kids: Option<Vec<NodeId>>,
    /// A virtualized container's last size within a layout run, by what it depends on (layout
    /// run, width, height, window).
    pub(crate) virt_memo: Option<(VirtKey, taffy::LayoutOutput)>,
    /// A virtualized container's layout state across passes ([`VirtState`]).
    pub(crate) virt: Option<Box<VirtState>>,
    /// A virtualized container's children marked for restyle since its last restyle
    /// (`Document::mark_style_dirty`), so restyle visits those, not every child; `None`:
    /// not tracked (visit every child).
    pub(crate) style_kids: Option<Vec<NodeId>>,
}

/// What a virtualized container's size depends on within one layout run.
pub(crate) type VirtKey = (u64, u32, Option<u32>, u32, u32, bool);

/// A virtualized container's layout state across passes: a row per layout child with what
/// its layout reads of the child and where the child went, so a pass reads one small array
/// instead of every child's node, and starts where something changed or what's in view begins
/// rather than at the first child (§7.10 cost: in proportion to what changed and what's shown).
#[derive(Debug, Default)]
pub(crate) struct VirtState {
    pub rows: Vec<VirtRow>,
    /// The content width the rows' margins were resolved at.
    pub w: u32,
    /// Rows from this index on may not be the layout children at their index (the children
    /// were rebuilt or extended, or a child's box style changed: [`VirtRow::STALE`]).
    pub check_from: usize,
    /// Rows before this index have their places (`y0`, `h`) as the inputs give them now.
    pub pos_valid: usize,
    /// Rows before this index, outside `vis`, have the estimated box at their place stored.
    pub store_valid: usize,
    /// The rows laid out (in view) by the last layout that stored boxes.
    pub vis: (usize, usize),
    /// What the places were computed with: gap, estimate, tail estimate, last measured row.
    pub params: (u32, u32, u32, Option<usize>),
    /// The absolutely positioned layout children (not in the stack).
    pub abs: Vec<NodeId>,
    /// Over the container's measured heights: their sum, count and last index, and how many
    /// heights that covers.
    pub msum: f64,
    pub mcount: usize,
    pub mlast: Option<usize>,
    pub mlen: usize,
}

/// What a virtualized container's layout reads of one layout child, and where it went.
#[derive(Clone, Copy, Debug)]
pub(crate) struct VirtRow {
    pub id: NodeId,
    /// Absolutely positioned: not in the stack.
    pub abs: bool,
    /// Its margins (top, bottom, left, right) at the container's content width.
    pub margin: [f32; 4],
    /// The estimated box last stored for it while out of view (x, y, width, height); `None`
    /// once laid out.
    pub stored: Option<[f32; 4]>,
    /// Where it starts in the stack (the end of the one before it), and the height it took.
    pub y0: f32,
    pub h: f32,
}

impl VirtRow {
    pub const STALE: NodeId = NodeId(u32::MAX);

    /// Where the stack goes on after row `i`.
    pub fn end(&self, i: usize, gap: f32) -> f32 {
        if self.abs {
            return self.y0;
        }
        let gap = if i > 0 { gap } else { 0.0 };
        self.y0 + gap + self.margin[0] + self.h + self.margin[1]
    }
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
            flags: flags::STYLE_DIRTY | flags::BOX_DIRTY | flags::BOX_FULL | flags::IFC_DIRTY,
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
            virtual_window: None,
            virtual_from_end: false,
            measured: Vec::new(),
            skipped: false,
            image: None,
            abspos: None,
            laid_out_in: 0,
            virt_kids: None,
            virt_memo: None,
            virt: None,
            style_kids: None,
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
