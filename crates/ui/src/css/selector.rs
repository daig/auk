//! Selector parsing and matching through the `selectors` crate.

use crate::document::Document;
use crate::dom::{Atom, ElementState, NodeId, NodeKind, PseudoKind};
use cssparser::{CowRcStr, ParseError, Parser as CssParser, ToCss};
use selectors::attr::{AttrSelectorOperation, CaseSensitivity, NamespaceConstraint};
use selectors::bloom::BloomFilter;
use selectors::context::{
    MatchingContext, MatchingForInvalidation, MatchingMode, NeedsSelectorFlags, QuirksMode,
    SelectorCaches,
};
use selectors::matching::ElementSelectorFlags;
use selectors::parser::{ParseRelative, SelectorList, SelectorParseErrorKind};
use selectors::{Element, OpaqueElement};
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Impl;

/// Pseudo-classes other than the tree-structural ones the crate handles itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PseudoClass {
    Hover,
    Active,
    Focus,
    FocusVisible,
    FocusWithin,
    Checked,
    Disabled,
}

impl selectors::parser::NonTSPseudoClass for PseudoClass {
    fn is_active_or_hover(&self) -> bool {
        matches!(self, PseudoClass::Hover | PseudoClass::Active)
    }
    fn is_user_action_state(&self) -> bool {
        matches!(
            self,
            PseudoClass::Hover | PseudoClass::Active | PseudoClass::Focus
        )
    }
}

impl ToCss for PseudoClass {
    fn to_css<W: fmt::Write>(&self, dest: &mut W) -> fmt::Result {
        dest.write_str(match self {
            PseudoClass::Hover => ":hover",
            PseudoClass::Active => ":active",
            PseudoClass::Focus => ":focus",
            PseudoClass::FocusVisible => ":focus-visible",
            PseudoClass::FocusWithin => ":focus-within",
            PseudoClass::Checked => ":checked",
            PseudoClass::Disabled => ":disabled",
        })
    }
}

/// Pseudo-elements we parse. Only `::before` and `::after` generate boxes; the rest are accepted
/// so that rules using them don't fail, and match nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PseudoElement {
    Before,
    After,
    Marker,
    Placeholder,
    Selection,
    Scrollbar,
}

impl selectors::parser::PseudoElement for PseudoElement {
    fn accepts_state_pseudo_classes(&self) -> bool {
        true
    }
}

impl ToCss for PseudoElement {
    fn to_css<W: fmt::Write>(&self, dest: &mut W) -> fmt::Result {
        dest.write_str(match self {
            PseudoElement::Before => "::before",
            PseudoElement::After => "::after",
            PseudoElement::Marker => "::marker",
            PseudoElement::Placeholder => "::placeholder",
            PseudoElement::Selection => "::selection",
            PseudoElement::Scrollbar => "::-webkit-scrollbar",
        })
    }
}

/// An attribute value in a selector.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct AttrValue(pub String);
impl<'a> From<&'a str> for AttrValue {
    fn from(s: &'a str) -> Self {
        AttrValue(s.to_string())
    }
}
impl AsRef<str> for AttrValue {
    fn as_ref(&self) -> &str {
        &self.0
    }
}
impl ToCss for AttrValue {
    fn to_css<W: fmt::Write>(&self, dest: &mut W) -> fmt::Result {
        cssparser::serialize_string(&self.0, dest)
    }
}

impl selectors::SelectorImpl for Impl {
    type ExtraMatchingData<'a> = ();
    type AttrValue = AttrValue;
    type Identifier = Atom;
    type LocalName = Atom;
    type NamespaceUrl = Atom;
    type NamespacePrefix = Atom;
    type BorrowedNamespaceUrl = str;
    type BorrowedLocalName = str;
    type NonTSPseudoClass = PseudoClass;
    type PseudoElement = PseudoElement;
}

pub type Selector = selectors::parser::Selector<Impl>;

/// Parses selectors with Tern's supported set (§4.5): no `:has`, `:nth-child(of S)` supported.
#[derive(Debug)]
pub struct SelectorParser;

impl<'i> selectors::parser::Parser<'i> for SelectorParser {
    type Impl = Impl;
    type Error = SelectorParseErrorKind;

    fn parse_is_and_where(&self) -> bool {
        true
    }
    fn parse_nth_child_of(&self) -> bool {
        true
    }
    fn parse_parent_selector(&self) -> bool {
        true
    }
    fn allow_forgiving_selectors(&self) -> bool {
        false
    }
    fn parse_non_ts_pseudo_class(
        &self,
        name: CowRcStr<'i>,
    ) -> Result<PseudoClass, ParseError<Self::Error>> {
        Ok(match name.to_ascii_lowercase().as_str() {
            "hover" => PseudoClass::Hover,
            "active" => PseudoClass::Active,
            "focus" => PseudoClass::Focus,
            "focus-visible" => PseudoClass::FocusVisible,
            "focus-within" => PseudoClass::FocusWithin,
            "checked" => PseudoClass::Checked,
            "disabled" => PseudoClass::Disabled,
            _ => {
                return Err(ParseError::custom(
                    SelectorParseErrorKind::UnsupportedPseudoClassOrElement,
                ));
            }
        })
    }
    fn parse_pseudo_element(
        &self,
        name: CowRcStr<'i>,
    ) -> Result<PseudoElement, ParseError<Self::Error>> {
        Ok(match name.to_ascii_lowercase().as_str() {
            "before" => PseudoElement::Before,
            "after" => PseudoElement::After,
            "marker" => PseudoElement::Marker,
            "placeholder" => PseudoElement::Placeholder,
            "selection" => PseudoElement::Selection,
            "-webkit-scrollbar" => PseudoElement::Scrollbar,
            _ => {
                return Err(ParseError::custom(
                    SelectorParseErrorKind::UnsupportedPseudoClassOrElement,
                ));
            }
        })
    }
}

/// Parse a selector list. Returns None if any selector in it is unsupported (the whole rule is
/// dropped).
pub fn parse_selector_list(input: &mut CssParser<'_>) -> Option<SelectorList<Impl>> {
    SelectorList::parse(&SelectorParser, input, ParseRelative::No).ok()
}

/// Whether a selector uses a combinator or pseudo-class Tern rejects (`~`, `:has`). Checked for
/// program and plugin sheets; our own sheets may use `~`.
pub fn uses_unsupported(text: &str) -> bool {
    // `~` outside attribute selectors and strings is the subsequent-sibling combinator.
    let mut depth = 0;
    let mut in_str: Option<char> = None;
    let bytes: Vec<char> = text.chars().collect();
    for (i, c) in bytes.iter().enumerate() {
        match (in_str, *c) {
            (Some(q), ch) if ch == q => in_str = None,
            (Some(_), _) => {}
            (None, '"') | (None, '\'') => in_str = Some(*c),
            (None, '[') => depth += 1,
            (None, ']') => depth -= 1,
            (None, '~') if depth == 0 && bytes.get(i + 1) != Some(&'=') => return true,
            _ => {}
        }
    }
    text.to_ascii_lowercase().contains(":has(")
}

/// An element as the selector engine sees it.
#[derive(Clone, Copy)]
pub struct ElementRef<'a> {
    pub doc: &'a Document,
    pub id: NodeId,
    /// For program sheets: matching never walks to or above this ancestor.
    pub scope_root: Option<NodeId>,
}

impl fmt::Debug for ElementRef<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ElementRef({:?})", self.id)
    }
}

impl<'a> ElementRef<'a> {
    fn data(&self) -> &'a crate::dom::ElementData {
        self.doc.node(self.id).element().expect("element")
    }
    fn wrap(&self, id: NodeId) -> ElementRef<'a> {
        ElementRef {
            doc: self.doc,
            id,
            scope_root: self.scope_root,
        }
    }
    fn sibling_element(&self, forward: bool) -> Option<ElementRef<'a>> {
        let node = self.doc.node(self.id);
        let parent = self.doc.node(node.parent?);
        let mut i = node.index_in_parent as isize;
        loop {
            i += if forward { 1 } else { -1 };
            if i < 0 || i as usize >= parent.children.len() {
                return None;
            }
            let c = parent.children[i as usize];
            if self.doc.node(c).is_element() {
                return Some(self.wrap(c));
            }
        }
    }
}

impl<'a> Element for ElementRef<'a> {
    type Impl = Impl;

    fn opaque(&self) -> OpaqueElement {
        OpaqueElement::new(self.doc.node(self.id))
    }
    fn parent_element(&self) -> Option<Self> {
        let p = self.doc.node(self.id).parent?;
        if Some(p) == self.scope_root {
            return None;
        }
        if !self.doc.node(p).is_element() {
            return None;
        }
        Some(self.wrap(p))
    }
    fn parent_node_is_shadow_root(&self) -> bool {
        false
    }
    fn containing_shadow_host(&self) -> Option<Self> {
        None
    }
    fn is_pseudo_element(&self) -> bool {
        false
    }
    fn prev_sibling_element(&self) -> Option<Self> {
        self.sibling_element(false)
    }
    fn next_sibling_element(&self) -> Option<Self> {
        self.sibling_element(true)
    }
    fn first_element_child(&self) -> Option<Self> {
        self.doc
            .node(self.id)
            .children
            .iter()
            .find(|c| self.doc.node(**c).is_element())
            .map(|c| self.wrap(*c))
    }
    fn is_html_element_in_html_document(&self) -> bool {
        true
    }
    fn has_local_name(&self, local_name: &str) -> bool {
        self.data().tag.as_str().eq_ignore_ascii_case(local_name)
    }
    fn has_namespace(&self, _ns: &str) -> bool {
        true
    }
    fn is_same_type(&self, other: &Self) -> bool {
        self.data().tag == other.data().tag
    }
    fn attr_matches(
        &self,
        _ns: &NamespaceConstraint<&Atom>,
        local_name: &Atom,
        operation: &AttrSelectorOperation<&AttrValue>,
    ) -> bool {
        let d = self.data();
        let name = local_name.as_str();
        if name == "class" {
            let joined: String = d
                .classes
                .iter()
                .map(|c| c.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            return !d.classes.is_empty() && operation.eval_str(&joined);
        }
        if name == "id" {
            return d
                .id
                .as_ref()
                .is_some_and(|id| operation.eval_str(id.as_str()));
        }
        match d.attr(name) {
            Some(v) => operation.eval_str(v),
            None => false,
        }
    }
    fn match_non_ts_pseudo_class(
        &self,
        pc: &PseudoClass,
        _context: &mut MatchingContext<Impl>,
    ) -> bool {
        let d = self.data();
        let s = d.state;
        match pc {
            PseudoClass::Hover => s.contains(ElementState::HOVER),
            PseudoClass::Active => s.contains(ElementState::ACTIVE),
            PseudoClass::Focus => s.contains(ElementState::FOCUS),
            PseudoClass::FocusVisible => s.contains(ElementState::FOCUS_VISIBLE),
            PseudoClass::FocusWithin => {
                s.contains(ElementState::FOCUS_WITHIN) || s.contains(ElementState::FOCUS)
            }
            PseudoClass::Checked => {
                s.contains(ElementState::CHECKED) || d.attr("checked").is_some()
            }
            PseudoClass::Disabled => {
                s.contains(ElementState::DISABLED) || d.attr("disabled").is_some()
            }
        }
    }
    fn match_pseudo_element(
        &self,
        _pe: &PseudoElement,
        _context: &mut MatchingContext<Impl>,
    ) -> bool {
        false
    }
    fn apply_selector_flags(&self, _flags: ElementSelectorFlags) {}
    fn is_link(&self) -> bool {
        false
    }
    fn is_html_slot_element(&self) -> bool {
        false
    }
    fn has_id(&self, id: &Atom, case_sensitivity: CaseSensitivity) -> bool {
        self.data()
            .id
            .as_ref()
            .is_some_and(|i| case_sensitivity.eq(i.as_str().as_bytes(), id.as_str().as_bytes()))
    }
    fn has_class(&self, name: &Atom, case_sensitivity: CaseSensitivity) -> bool {
        self.data()
            .classes
            .iter()
            .any(|c| case_sensitivity.eq(c.as_str().as_bytes(), name.as_str().as_bytes()))
    }
    fn has_custom_state(&self, _name: &Atom) -> bool {
        false
    }
    fn imported_part(&self, _name: &Atom) -> Option<Atom> {
        None
    }
    fn is_part(&self, _name: &Atom) -> bool {
        false
    }
    fn is_empty(&self) -> bool {
        self.doc
            .node(self.id)
            .children
            .iter()
            .all(|c| match &self.doc.node(*c).kind {
                NodeKind::Text(t) => t.is_empty(),
                _ => false,
            })
    }
    fn is_root(&self) -> bool {
        self.scope_root.is_none() && self.doc.node(self.id).parent.is_none()
    }
    fn add_element_unique_hashes(&self, _filter: &mut BloomFilter) -> bool {
        false
    }
}

/// Match a selector against an element (or, with `pseudo`, against its pseudo-element).
pub fn matches(
    sel: &Selector,
    el: ElementRef<'_>,
    pseudo: Option<PseudoKind>,
    caches: &mut SelectorCaches,
) -> bool {
    let mode = if pseudo.is_some() {
        MatchingMode::ForStatelessPseudoElement
    } else {
        MatchingMode::Normal
    };
    let mut cx = MatchingContext::new(
        mode,
        None,
        caches,
        QuirksMode::NoQuirks,
        NeedsSelectorFlags::No,
        MatchingForInvalidation::No,
    );
    selectors::matching::matches_selector(sel, 0, None, &el, &mut cx)
}
