//! The cascade: sheet registry, rule index, matching, ordering, var() and computed styles.

use super::compute::{ComputeEnv, compute_prop};
use super::properties::{Prop, Restrictions, SV, parse_substituted};
use super::selector::{ElementRef, PseudoElement, Selector, ancestor_hashes, matches};
use super::sheet::{Decl, DeclBlock, DeclProp, Keyframes, MediaEnv, MediaQuery, Stylesheet};
use crate::dom::{Atom, ElementData, PseudoKind};
use crate::style::{ComputedStyle, CustomProps};
use selectors::bloom::BloomFilter;
use selectors::context::SelectorCaches;
use selectors::parser::{AncestorHashes, Combinator, Component};
use std::collections::HashMap;
use std::sync::Arc;

/// Where a sheet comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Origin {
    /// The user-agent sheet.
    UserAgent,
    /// The terminal's built-in sheets, then plugin and program sheets, in install order.
    Author,
}

/// The kind of author sheet, which decides the accept/reject rules applied to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SheetKind {
    /// The user-agent sheet or one of the terminal's own sheets: no restrictions.
    BuiltIn,
    /// A plugin sheet: global, Tern's unit set.
    Plugin,
    /// A program (TSP) sheet: confined to its surface's regions, no `url()` or `position: fixed`.
    Program,
}

/// A handle to an installed sheet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SheetId(pub u32);

pub(crate) struct SheetEntry {
    pub id: SheetId,
    pub name: String,
    pub origin: Origin,
    pub kind: SheetKind,
    /// For program sheets: the `data-scope` value of the surface's regions.
    pub scope: Option<Atom>,
    pub sheet: Stylesheet,
}

struct IndexedRule {
    sel: Selector,
    /// The hashes of what the selector needs among the element's ancestors.
    hashes: AncestorHashes,
    specificity: u32,
    pseudo: Option<PseudoKind>,
    block: Arc<DeclBlock>,
    origin: Origin,
    order: u32,
    scope: Option<Atom>,
    media: Option<Arc<MediaQuery>>,
}

/// Rules bucketed by the most selective simple selector of the compound the element itself must
/// match (an id, else a class, else a tag); the rest are universal.
#[derive(Default)]
struct Buckets {
    by_id: HashMap<Atom, Vec<u32>>,
    by_class: HashMap<Atom, Vec<u32>>,
    by_tag: HashMap<Atom, Vec<u32>>,
    universal: Vec<u32>,
}

impl Buckets {
    fn add(&mut self, i: u32, id: Option<Atom>, class: Option<Atom>, tag: Option<Atom>) {
        if let Some(a) = id {
            self.by_id.entry(a).or_default().push(i);
        } else if let Some(a) = class {
            self.by_class.entry(a).or_default().push(i);
        } else if let Some(a) = tag {
            self.by_tag.entry(a).or_default().push(i);
        } else {
            self.universal.push(i);
        }
    }

    /// Every rule that may match an element with this id, these classes and this tag.
    fn each(&self, data: &ElementData, mut f: impl FnMut(u32)) {
        if let Some(id) = &data.id
            && let Some(v) = self.by_id.get(id)
        {
            v.iter().for_each(|&i| f(i));
        }
        for c in &data.classes {
            if let Some(v) = self.by_class.get(c) {
                v.iter().for_each(|&i| f(i));
            }
        }
        if let Some(v) = self.by_tag.get(&data.tag) {
            v.iter().for_each(|&i| f(i));
        }
        self.universal.iter().for_each(|&i| f(i));
    }
}

#[derive(Default)]
struct RuleIndex {
    rules: Vec<IndexedRule>,
    /// Element rules.
    elements: Buckets,
    /// `::before` and `::after` rules, bucketed by their originating element's compound.
    pseudos: Buckets,
    /// The pseudo-element rules that declare `content`. Unless one of them matches, an element
    /// has no `::before` or `::after` (`content` isn't inherited, and is `normal` initially), and
    /// the other pseudo-element rules needn't be tried.
    pseudo_content: Buckets,
}

/// The installed sheets and their rule index.
pub struct Stylist {
    pub(crate) sheets: Vec<SheetEntry>,
    next_id: u32,
    index: RuleIndex,
    index_dirty: bool,
    pub media: MediaEnv,
    pub(crate) has_media_rules: bool,
}

impl std::fmt::Debug for Stylist {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Stylist")
            .field("sheets", &self.sheets.len())
            .finish()
    }
}

impl Default for Stylist {
    fn default() -> Self {
        Stylist {
            sheets: Vec::new(),
            next_id: 0,
            index: RuleIndex::default(),
            index_dirty: true,
            media: MediaEnv::default(),
            has_media_rules: false,
        }
    }
}

impl Stylist {
    /// Install or replace a sheet. A replacement keeps its place in the cascade. Returns the
    /// sheet's id and its errors.
    pub fn set_sheet(
        &mut self,
        name: &str,
        kind: SheetKind,
        scope: Option<&str>,
        css: &str,
    ) -> (SheetId, Vec<String>) {
        let r = match kind {
            SheetKind::BuiltIn => Restrictions::NONE,
            SheetKind::Plugin => Restrictions::PLUGIN,
            SheetKind::Program => Restrictions::PROGRAM,
        };
        let sheet = Stylesheet::parse(css, r);
        let errors = sheet.errors.clone();
        let scope = scope.map(Atom::new);
        self.index_dirty = true;
        if let Some(e) = self
            .sheets
            .iter_mut()
            .find(|e| e.name == name && e.scope == scope)
        {
            e.sheet = sheet;
            e.kind = kind;
            return (e.id, errors);
        }
        let id = SheetId(self.next_id);
        self.next_id += 1;
        let origin = if name == "ua" {
            Origin::UserAgent
        } else {
            Origin::Author
        };
        self.sheets.push(SheetEntry {
            id,
            name: name.to_string(),
            origin,
            kind,
            scope,
            sheet,
        });
        (id, errors)
    }

    pub fn remove_sheet(&mut self, name: &str, scope: Option<&str>) -> bool {
        let before = self.sheets.len();
        self.sheets
            .retain(|e| !(e.name == name && e.scope.as_ref().map(|a| a.as_str()) == scope));
        self.index_dirty = true;
        before != self.sheets.len()
    }

    /// Keyframes by name, looking first in the scope's own sheets (private to program sheets).
    pub fn keyframes(&self, name: &str, scope: Option<&str>) -> Option<&Keyframes> {
        if let Some(scope) = scope {
            for e in self.sheets.iter().rev() {
                if e.scope.as_ref().is_some_and(|s| s.as_str() == scope)
                    && let Some(k) = e.sheet.keyframes.get(name)
                {
                    return Some(k);
                }
            }
        }
        self.sheets
            .iter()
            .rev()
            .filter(|e| e.scope.is_none())
            .find_map(|e| e.sheet.keyframes.get(name))
    }

    fn ensure_index(&mut self) {
        if !self.index_dirty {
            return;
        }
        self.index_dirty = false;
        let mut idx = RuleIndex::default();
        let mut order = 0u32;
        self.has_media_rules = false;
        // The UA sheet first, then author sheets in install order.
        let mut sheets: Vec<&SheetEntry> = self.sheets.iter().collect();
        sheets.sort_by_key(|e| e.origin);
        for e in sheets {
            for rule in &e.sheet.rules {
                if rule.media.is_some() {
                    self.has_media_rules = true;
                }
                for sel in rule.selectors.slice() {
                    order += 1;
                    let pseudo = match sel.pseudo_element() {
                        None => None,
                        Some(PseudoElement::Before) => Some(PseudoKind::Before),
                        Some(PseudoElement::After) => Some(PseudoKind::After),
                        Some(_) => continue,
                    };
                    let i = idx.rules.len() as u32;
                    idx.rules.push(IndexedRule {
                        sel: sel.clone(),
                        hashes: ancestor_hashes(sel),
                        specificity: sel.specificity(),
                        pseudo,
                        block: rule.block.clone(),
                        origin: e.origin,
                        order,
                        scope: e.scope.clone(),
                        media: rule.media.clone(),
                    });
                    // Bucket by the most selective simple selector of the compound the element
                    // itself must match: the rightmost, or for a pseudo-element's rule the one
                    // before the pseudo-element.
                    let mut id = None;
                    let mut class = None;
                    let mut tag = None;
                    let mut it = sel.iter();
                    let own = pseudo.is_none() || {
                        for _ in &mut it {}
                        it.next_sequence() == Some(Combinator::PseudoElement)
                    };
                    if own {
                        for c in &mut it {
                            match c {
                                Component::ID(a) => id = Some(a.clone()),
                                Component::Class(a) => class = class.or(Some(a.clone())),
                                Component::LocalName(l) => tag = Some(l.lower_name.clone()),
                                _ => {}
                            }
                        }
                    }
                    if pseudo.is_none() {
                        idx.elements.add(i, id, class, tag);
                    } else {
                        if rule
                            .block
                            .decls
                            .iter()
                            .any(|d| d.prop == DeclProp::Long(Prop::Content))
                        {
                            idx.pseudo_content
                                .add(i, id.clone(), class.clone(), tag.clone());
                        }
                        idx.pseudos.add(i, id, class, tag);
                    }
                }
            }
        }
        self.index = idx;
    }

    pub(crate) fn prepare(&mut self) {
        self.ensure_index();
    }

    /// The declarations that apply to an element (or its pseudo-element), in cascade order:
    /// lowest priority first. `scope` is the element's nearest `data-scope` ancestor and value.
    pub(crate) fn matched_blocks(
        &self,
        el: ElementRef<'_>,
        pseudo: Option<PseudoKind>,
        scope: Option<(crate::dom::NodeId, &Atom)>,
        ancestors: Option<&BloomFilter>,
        caches: &mut SelectorCaches,
        out: &mut Vec<(Origin, u32, u32, Arc<DeclBlock>)>,
    ) {
        out.clear();
        let data = el.doc.node(el.id).element().expect("element");
        let idx = &self.index;
        let mut consider = |i: u32, out: &mut Vec<(Origin, u32, u32, Arc<DeclBlock>)>| {
            let r = &idx.rules[i as usize];
            if r.pseudo != pseudo {
                return;
            }
            if let Some(m) = &r.media
                && !m.matches(&self.media)
            {
                return;
            }
            let mut elr = el;
            if let Some(rs) = &r.scope {
                match scope {
                    Some((root, v)) if v == rs => elr.scope_root = Some(root),
                    _ => return,
                }
            }
            if matches(&r.sel, &r.hashes, elr, pseudo, ancestors, caches) {
                out.push((r.origin, r.specificity, r.order, r.block.clone()));
            }
        };
        let buckets = if pseudo.is_some() {
            &idx.pseudos
        } else {
            &idx.elements
        };
        buckets.each(data, |i| consider(i, out));
        out.sort_by_key(|(o, s, ord, _)| (*o, *s, *ord));
    }

    /// Whether an element may have this pseudo-element: a rule that declares `content` for it
    /// matches. When none does, its `content` computes to `normal` and it has none.
    pub(crate) fn may_have_pseudo(
        &self,
        el: ElementRef<'_>,
        pseudo: PseudoKind,
        scope: Option<(crate::dom::NodeId, &Atom)>,
        ancestors: Option<&BloomFilter>,
        caches: &mut SelectorCaches,
    ) -> bool {
        let data = el.doc.node(el.id).element().expect("element");
        let idx = &self.index;
        let mut found = false;
        idx.pseudo_content.each(data, |i| {
            let r = &idx.rules[i as usize];
            if found || r.pseudo != Some(pseudo) {
                return;
            }
            if let Some(m) = &r.media
                && !m.matches(&self.media)
            {
                return;
            }
            let mut elr = el;
            if let Some(rs) = &r.scope {
                match scope {
                    Some((root, v)) if v == rs => elr.scope_root = Some(root),
                    _ => return,
                }
            }
            found = matches(&r.sel, &r.hashes, elr, Some(pseudo), ancestors, caches);
        });
        found
    }
}

/// Replace `var()` references in `css`. Returns None when a reference can't be resolved (the
/// property is then invalid at computed-value time).
pub fn substitute_vars(css: &str, custom: &CustomProps, depth: u32) -> Option<String> {
    if depth > 32 {
        return None;
    }
    let lower = css.to_ascii_lowercase();
    let mut out = String::with_capacity(css.len());
    let mut i = 0;
    let bytes = css.as_bytes();
    while let Some(off) = lower[i..].find("var(") {
        let start = i + off;
        // Only a function token: preceded by a non-identifier character.
        if start > 0 {
            let prev = bytes[start - 1];
            if prev.is_ascii_alphanumeric() || prev == b'-' || prev == b'_' {
                out.push_str(&css[i..start + 4]);
                i = start + 4;
                continue;
            }
        }
        out.push_str(&css[i..start]);
        // Find the matching close paren.
        let mut depth_p = 0;
        let mut end = None;
        let mut in_str: Option<u8> = None;
        for (j, &b) in bytes.iter().enumerate().skip(start + 3) {
            match (in_str, b) {
                (Some(q), c) if c == q => in_str = None,
                (Some(_), _) => {}
                (None, b'"') | (None, b'\'') => in_str = Some(b),
                (None, b'(') => depth_p += 1,
                (None, b')') => {
                    depth_p -= 1;
                    if depth_p == 0 {
                        end = Some(j);
                        break;
                    }
                }
                _ => {}
            }
        }
        let end = end?;
        let inner = &css[start + 4..end];
        let (name, fallback) = match top_level_comma(inner) {
            Some(c) => (inner[..c].trim(), Some(&inner[c + 1..])),
            None => (inner.trim(), None),
        };
        let value = match custom.get(name) {
            Some(v) if !v.trim().is_empty() || fallback.is_none() => Some(v.to_string()),
            _ => fallback.map(|f| f.trim().to_string()),
        }?;
        let value = if value.to_ascii_lowercase().contains("var(") {
            substitute_vars(&value, custom, depth + 1)?
        } else {
            value
        };
        out.push_str(&value);
        i = end + 1;
    }
    out.push_str(&css[i..]);
    Some(out)
}

fn top_level_comma(s: &str) -> Option<usize> {
    let mut depth = 0;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => return Some(i),
            _ => {}
        }
    }
    None
}

fn apply<'a>(d: &'a Decl, winners: &mut [Option<&'a SV>], customs: &mut Vec<(&'a Atom, &'a SV)>) {
    match &d.prop {
        DeclProp::Long(p) => winners[*p as usize] = Some(&d.value),
        DeclProp::Custom(name) => customs.push((name, &d.value)),
    }
}

/// Compute an element's style from its matched blocks and inline style.
pub(crate) fn cascade(
    blocks: &[(Origin, u32, u32, Arc<DeclBlock>)],
    inline: Option<&DeclBlock>,
    parent: &ComputedStyle,
    initial: &ComputedStyle,
    env: &ComputeEnv,
    attr: &dyn Fn(&str) -> Option<String>,
) -> ComputedStyle {
    let mut winners: Vec<Option<&SV>> = vec![None; Prop::COUNT];
    let mut customs: Vec<(&Atom, &SV)> = Vec::new();
    // Normal declarations: origins in order, then inline style.
    for (_, _, _, b) in blocks {
        for d in b.decls.iter().filter(|d| !d.important) {
            apply(d, &mut winners, &mut customs);
        }
    }
    if let Some(b) = inline {
        for d in b.decls.iter().filter(|d| !d.important) {
            apply(d, &mut winners, &mut customs);
        }
    }
    // Important declarations, in the same order.
    for (_, _, _, b) in blocks {
        for d in b.decls.iter().filter(|d| d.important) {
            apply(d, &mut winners, &mut customs);
        }
    }
    if let Some(b) = inline {
        for d in b.decls.iter().filter(|d| d.important) {
            apply(d, &mut winners, &mut customs);
        }
    }

    let mut cs = ComputedStyle::inherit_from(parent, initial);
    if !customs.is_empty() {
        let mut map: CustomProps = (*parent.custom).clone();
        let mut raw: Vec<(Arc<str>, Option<Arc<str>>)> = Vec::new();
        for (name, v) in &customs {
            let key: Arc<str> = Arc::from(name.as_str());
            let val = match v {
                SV::Str(s) => Some(s.clone()),
                SV::Inherit | SV::Unset => parent.custom.get(&key).cloned(),
                _ => None,
            };
            raw.retain(|(k, _)| *k != key);
            raw.push((key, val));
        }
        for (k, v) in &raw {
            match v {
                Some(v) => {
                    map.insert(k.clone(), v.clone());
                }
                None => {
                    map.remove(k);
                }
            }
        }
        // Resolve references between custom properties. Cycles and chains deeper than 32 drop
        // the property.
        for (k, v) in &raw {
            if let Some(v) = v
                && v.to_ascii_lowercase().contains("var(")
            {
                let mut without = map.clone();
                without.remove(k);
                match substitute_vars(v, &without, 0) {
                    Some(s) => {
                        map.insert(k.clone(), Arc::from(s.as_str()));
                    }
                    None => {
                        map.remove(k);
                    }
                }
            }
        }
        cs.custom = Arc::new(map);
    }

    for &prop in Prop::ALL {
        let Some(sv) = winners[prop as usize] else {
            continue;
        };
        let substituted;
        let sv = match sv {
            SV::Unparsed { css, shorthand } => {
                substituted = substitute_vars(css, &cs.custom, 0)
                    .and_then(|s| parse_substituted(prop, *shorthand, &s));
                match &substituted {
                    Some(v) => v,
                    None => &SV::Unset,
                }
            }
            v => v,
        };
        if !compute_prop(&mut cs, parent, initial, prop, sv, env, attr) {
            compute_prop(&mut cs, parent, initial, prop, &SV::Unset, env, attr);
        }
    }
    // Border colors default to currentColor.
    for (i, p) in [
        Prop::BorderTopColor,
        Prop::BorderRightColor,
        Prop::BorderBottomColor,
        Prop::BorderLeftColor,
    ]
    .into_iter()
    .enumerate()
    {
        if winners[p as usize].is_none() {
            cs.border_color[i] = cs.color;
        }
    }
    if winners[Prop::OutlineColor as usize].is_none() {
        cs.outline_color = cs.color;
    }
    cs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn var_substitution() {
        let mut m = CustomProps::new();
        m.insert(Arc::from("--a"), Arc::from("4px"));
        m.insert(Arc::from("--b"), Arc::from("var(--a) 2px"));
        assert_eq!(
            substitute_vars("calc(var(--a) * 2)", &m, 0).as_deref(),
            Some("calc(4px * 2)")
        );
        assert_eq!(
            substitute_vars("var(--b)", &m, 0).as_deref(),
            Some("4px 2px")
        );
        assert_eq!(
            substitute_vars("var(--missing, var(--a))", &m, 0).as_deref(),
            Some("4px")
        );
        assert_eq!(substitute_vars("var(--missing)", &m, 0), None);
    }
}
