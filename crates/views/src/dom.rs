//! A thin layer over the UI engine's DOM that remembers what it wrote: inline style text (the
//! engine keeps only the parsed declarations), class strings and attributes, so that unchanged
//! values are never written again and a projected surface can be serialized back to HTML.

use auk_ui::{Document, ElementState, NodeId};
use std::collections::HashMap;

/// What the views wrote on each element, by node.
#[derive(Debug, Default)]
pub struct Written {
    pub(crate) style: HashMap<NodeId, String>,
    pub(crate) class: HashMap<NodeId, String>,
}

impl Written {
    /// The class string last written on `id`, if any.
    pub fn class(&self, id: NodeId) -> Option<&str> {
        self.class.get(&id).map(String::as_str)
    }

    /// The inline style last written on `id`, if any.
    pub fn style(&self, id: NodeId) -> Option<&str> {
        self.style.get(&id).map(String::as_str)
    }
}

/// The document plus the record of what was written to it.
pub(crate) struct Dom<'a> {
    pub doc: &'a mut Document,
    pub w: &'a mut Written,
}

impl Dom<'_> {
    /// A detached element with a class string.
    pub fn el(&mut self, tag: &str, class: &str) -> NodeId {
        let id = self.doc.create_element(tag);
        self.w.style.remove(&id);
        self.w.class.remove(&id);
        if !class.is_empty() {
            self.doc.set_attribute(id, "class", class);
            self.w.class.insert(id, class.to_owned());
        }
        id
    }

    /// An element appended to `parent`.
    pub fn child(&mut self, parent: NodeId, tag: &str, class: &str) -> NodeId {
        let id = self.el(tag, class);
        self.doc.append_child(parent, id);
        id
    }

    /// An element with text, appended to `parent`.
    pub fn child_text(&mut self, parent: NodeId, tag: &str, class: &str, text: &str) -> NodeId {
        let id = self.child(parent, tag, class);
        if !text.is_empty() {
            self.text(id, text);
        }
        id
    }

    /// A text node appended to `parent`.
    pub fn text(&mut self, parent: NodeId, text: &str) -> NodeId {
        let t = self.doc.create_text(text);
        self.doc.append_child(parent, t);
        t
    }

    /// Replace all children of `parent` with one text node (or none for empty text), reusing a
    /// lone text node.
    pub fn set_only_text(&mut self, parent: NodeId, text: &str) {
        let kids = self.doc.children(parent).to_vec();
        if kids.len() == 1 && self.doc.node(kids[0]).text().is_some() {
            if text.is_empty() {
                self.doc.remove(kids[0]);
            } else {
                self.doc.set_text(kids[0], text);
            }
            return;
        }
        for k in kids {
            self.doc.remove(k);
        }
        if !text.is_empty() {
            self.text(parent, text);
        }
    }

    pub fn set_class(&mut self, id: NodeId, class: &str) {
        if self.w.class.get(&id).map(String::as_str) == Some(class) {
            return;
        }
        if class.is_empty() && !self.w.class.contains_key(&id) {
            return;
        }
        self.doc.set_attribute(id, "class", class);
        self.w.class.insert(id, class.to_owned());
    }

    pub fn class_of(&self, id: NodeId) -> &str {
        self.w.class.get(&id).map(String::as_str).unwrap_or("")
    }

    pub fn set_style(&mut self, id: NodeId, css: &str) {
        let had = self.w.style.get(&id);
        if had.map(String::as_str) == Some(css) || (css.is_empty() && had.is_none()) {
            return;
        }
        if css.is_empty() {
            self.doc.remove_attribute(id, "style");
            self.w.style.remove(&id);
        } else {
            self.doc.set_inline_style(id, css);
            self.w.style.insert(id, css.to_owned());
        }
    }

    pub fn attr(&mut self, id: NodeId, name: &str, value: &str) {
        if self.doc.attribute(id, name) == Some(value) {
            return;
        }
        self.doc.set_attribute(id, name, value);
    }

    pub fn attr_opt(&mut self, id: NodeId, name: &str, value: Option<&str>) {
        match value {
            Some(v) => self.attr(id, name, v),
            None => {
                if self.doc.attribute(id, name).is_some() {
                    self.doc.remove_attribute(id, name);
                }
            }
        }
    }

    pub fn state(&mut self, id: NodeId, state: ElementState, on: bool) {
        let has = self
            .doc
            .node(id)
            .element()
            .is_some_and(|e| e.state.contains(state));
        if has != on {
            self.doc.set_state(id, state, on);
        }
    }

    /// Remove every child of `parent`.
    pub fn clear(&mut self, parent: NodeId) {
        for k in self.doc.children(parent).to_vec() {
            self.doc.remove(k);
        }
    }

    /// Make `parent`'s children exactly `want`, in order, with as few moves as possible. Nodes
    /// not in `want` are detached (not freed).
    pub fn reconcile(&mut self, parent: NodeId, want: &[NodeId]) {
        for (i, &c) in want.iter().enumerate() {
            let cur = self.doc.children(parent).get(i).copied();
            if cur != Some(c) {
                self.doc.insert_before(parent, c, cur);
            }
        }
        while self.doc.children(parent).len() > want.len() {
            let last = *self.doc.children(parent).last().expect("child");
            self.doc.detach(last);
        }
    }
}
