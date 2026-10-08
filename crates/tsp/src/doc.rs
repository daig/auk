//! A surface's document (§7.5, §7.6): the node tree, the 12 frame ops with their validation, the
//! settle hints, focus, reveal and scroll requests, and the change log views read.

use std::collections::{BTreeSet, HashMap};

use serde_json::{Map, Value};

/// The most nodes in one surface, root included (§7.14).
pub const MAX_NODES: usize = 200_000;
/// The deepest a node may sit; the root is at depth 1 (§7.14).
pub const MAX_DEPTH: usize = 64;
/// Region ids: the fixed children of the surface root (§7.5).
pub const REGIONS: [&str; 3] = ["main", "dock", "layer"];
/// Kinds whose primary text is `p.text` (§7.6).
pub const TEXT_KINDS: [&str; 9] = [
    "text", "md", "code", "ansi", "math", "editor", "input", "shimmer", "el",
];
/// The fixed per-node overhead of the retention estimate, in bytes (§7.10).
pub const NODE_OVERHEAD: u64 = 64;

/// Whether `kind` keeps its primary text in `p.text`.
pub fn is_text_kind(kind: &str) -> bool {
    TEXT_KINDS.contains(&kind)
}

/// Where `reveal` puts a node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RevealAt {
    Start,
    End,
    Nearest,
}

impl RevealAt {
    /// Reads a position; anything unknown reads as `nearest` (§7.6).
    fn parse(v: Option<&Value>) -> Self {
        match v.and_then(Value::as_str) {
            Some("start") => RevealAt::Start,
            Some("end") => RevealAt::End,
            _ => RevealAt::Nearest,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            RevealAt::Start => "start",
            RevealAt::End => "end",
            RevealAt::Nearest => "nearest",
        }
    }
}

/// A `scroll` step (§7.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScrollBy {
    LineUp,
    LineDown,
    PageUp,
    PageDown,
    Start,
    End,
}

impl ScrollBy {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "line-up" => ScrollBy::LineUp,
            "line-down" => ScrollBy::LineDown,
            "page-up" => ScrollBy::PageUp,
            "page-down" => ScrollBy::PageDown,
            "start" => ScrollBy::Start,
            "end" => ScrollBy::End,
            _ => return None,
        })
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            ScrollBy::LineUp => "line-up",
            ScrollBy::LineDown => "line-down",
            ScrollBy::PageUp => "page-up",
            ScrollBy::PageDown => "page-down",
            ScrollBy::Start => "start",
            ScrollBy::End => "end",
        }
    }
}

/// What an op asks of the surface beyond the document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Effect {
    None,
    Suspend,
    Resume,
}

/// What ops are checked against.
#[derive(Debug)]
pub(crate) struct OpContext<'a> {
    /// The advertised kinds; `None` skips the check (snapshot replay).
    pub kinds: Option<&'a BTreeSet<String>>,
}

#[derive(Clone, Debug)]
struct Node {
    id: String,
    kind: String,
    props: Map<String, Value>,
    children: Vec<usize>,
    parent: Option<usize>,
    settled: bool,
    cost: u64,
}

impl Node {
    fn estimate(&self) -> u64 {
        let props: u64 = self
            .props
            .iter()
            .map(|(k, v)| k.len() as u64 + value_cost(v))
            .sum();
        NODE_OVERHEAD + self.id.len() as u64 + self.kind.len() as u64 + props
    }
}

/// A rough size of a JSON value in bytes: strings by length, containers by contents.
fn value_cost(v: &Value) -> u64 {
    match v {
        Value::Null | Value::Bool(_) => 1,
        Value::Number(_) => 8,
        Value::String(s) => s.len() as u64 + 2,
        Value::Array(a) => 2 + a.iter().map(value_cost).sum::<u64>(),
        Value::Object(o) => {
            2 + o
                .iter()
                .map(|(k, v)| k.len() as u64 + 3 + value_cost(v))
                .sum::<u64>()
        }
    }
}

/// Node-level changes since the last drain.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DocChanges {
    /// Nodes whose props, text or settle hint changed.
    pub nodes: BTreeSet<String>,
    /// Nodes whose child list changed (adds, moves, deletes under them).
    pub children: BTreeSet<String>,
    /// Nodes removed (deleted, evicted, or dropped with a region on close).
    pub removed: BTreeSet<String>,
    /// The focus changed.
    pub focus: bool,
}

impl DocChanges {
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty() && self.children.is_empty() && self.removed.is_empty() && !self.focus
    }
}

/// A `reveal` request: only the latest applies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reveal {
    pub id: String,
    pub at: RevealAt,
}

/// A `scroll` request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scroll {
    pub id: String,
    pub by: ScrollBy,
}

/// A surface's node tree. The root is a `col` whose id is the surface id.
#[derive(Clone, Debug)]
pub struct Document {
    slots: Vec<Option<Node>>,
    free: Vec<usize>,
    index: HashMap<String, usize>,
    root: usize,
    count: usize,
    cost: u64,
    focus: Option<String>,
    changes: DocChanges,
    reveal: Option<Reveal>,
    scrolls: Vec<Scroll>,
}

impl Document {
    /// A document holding only the root.
    pub fn new(surface_id: &str) -> Self {
        let mut doc = Document {
            slots: Vec::new(),
            free: Vec::new(),
            index: HashMap::new(),
            root: 0,
            count: 0,
            cost: 0,
            focus: None,
            changes: DocChanges::default(),
            reveal: None,
            scrolls: Vec::new(),
        };
        doc.root = doc.insert(Node {
            id: surface_id.to_owned(),
            kind: "col".to_owned(),
            props: Map::new(),
            children: Vec::new(),
            parent: None,
            settled: false,
            cost: 0,
        });
        doc
    }

    fn insert(&mut self, mut node: Node) -> usize {
        node.cost = node.estimate();
        self.cost += node.cost;
        self.count += 1;
        let id = node.id.clone();
        let slot = match self.free.pop() {
            Some(slot) => {
                self.slots[slot] = Some(node);
                slot
            }
            None => {
                self.slots.push(Some(node));
                self.slots.len() - 1
            }
        };
        self.index.insert(id, slot);
        slot
    }

    fn node(&self, slot: usize) -> &Node {
        self.slots[slot].as_ref().expect("live slot")
    }

    fn node_mut(&mut self, slot: usize) -> &mut Node {
        self.slots[slot].as_mut().expect("live slot")
    }

    fn slot(&self, id: &str) -> Result<usize, String> {
        self.index
            .get(id)
            .copied()
            .ok_or_else(|| format!("unknown id {id}"))
    }

    /// The root node.
    pub fn root(&self) -> NodeRef<'_> {
        NodeRef {
            doc: self,
            slot: self.root,
        }
    }

    /// The node with `id`.
    pub fn get(&self, id: &str) -> Option<NodeRef<'_>> {
        self.index.get(id).map(|&slot| NodeRef { doc: self, slot })
    }

    /// The region node `main`, `dock` or `layer`, when added.
    pub fn region(&self, name: &str) -> Option<NodeRef<'_>> {
        self.root().children().find(|n| n.id() == name)
    }

    /// Nodes, root included.
    pub fn len(&self) -> usize {
        self.count
    }

    /// Whether only the root is present.
    pub fn is_empty(&self) -> bool {
        self.count == 1
    }

    /// The retention estimate of the whole tree, in bytes.
    pub fn cost(&self) -> u64 {
        self.cost
    }

    /// The `editor` or `input` that owns the caret.
    pub fn focus(&self) -> Option<&str> {
        self.focus.as_deref()
    }

    /// Every settled node, in document order.
    pub fn settled(&self) -> Vec<&str> {
        let mut out = Vec::new();
        self.walk(self.root, &mut |n| {
            if n.settled {
                out.push(n.id.as_str());
            }
        });
        out
    }

    fn walk<'a>(&'a self, slot: usize, f: &mut impl FnMut(&'a Node)) {
        let node = self.node(slot);
        f(node);
        for &c in &node.children {
            self.walk(c, f);
        }
    }

    fn depth(&self, mut slot: usize) -> usize {
        let mut depth = 1;
        while let Some(parent) = self.node(slot).parent {
            depth += 1;
            slot = parent;
        }
        depth
    }

    fn height(&self, slot: usize) -> usize {
        1 + self
            .node(slot)
            .children
            .iter()
            .map(|&c| self.height(c))
            .max()
            .unwrap_or(0)
    }

    fn is_within(&self, mut slot: usize, ancestor: usize) -> bool {
        loop {
            if slot == ancestor {
                return true;
            }
            match self.node(slot).parent {
                Some(p) => slot = p,
                None => return false,
            }
        }
    }

    /// Clears the settle hint on `slot` and its ancestors (any op inside a settled subtree).
    fn unsettle(&mut self, mut slot: usize) {
        loop {
            let node = self.node_mut(slot);
            if node.settled {
                node.settled = false;
                let id = node.id.clone();
                self.changes.nodes.insert(id);
            }
            match self.node(slot).parent {
                Some(p) => slot = p,
                None => return,
            }
        }
    }

    fn recost(&mut self, slot: usize) {
        let node = self.node_mut(slot);
        let new = node.estimate();
        let old = std::mem::replace(&mut node.cost, new);
        self.cost = self.cost - old + new;
    }

    /// Takes the node-level changes since the last call.
    pub fn take_changes(&mut self) -> DocChanges {
        std::mem::take(&mut self.changes)
    }

    /// Takes the latest `reveal` since the last call.
    pub fn take_reveal(&mut self) -> Option<Reveal> {
        self.reveal.take()
    }

    /// Takes the `scroll` requests since the last call, in order.
    pub fn take_scrolls(&mut self) -> Vec<Scroll> {
        std::mem::take(&mut self.scrolls)
    }

    /// Applies one op. On error, the document is unchanged.
    pub(crate) fn apply(&mut self, op: &Value, ctx: &OpContext<'_>) -> Result<Effect, String> {
        let Some(arr) = op.as_array() else {
            return Err("an op must be an array".into());
        };
        let Some(name) = arr.first().and_then(Value::as_str) else {
            return Err("an op must start with its name".into());
        };
        let args = &arr[1..];
        match name {
            "add" => self.op_add(args, ctx).map(|()| Effect::None),
            "set" => self.op_set(args).map(|()| Effect::None),
            "text" => self.op_text(args).map(|()| Effect::None),
            "splice" => self.op_splice(args).map(|()| Effect::None),
            "move" => self.op_move(args).map(|()| Effect::None),
            "del" => self.op_del(args).map(|()| Effect::None),
            "settle" => self.op_settle(args).map(|()| Effect::None),
            "focus" => self.op_focus(args).map(|()| Effect::None),
            "reveal" => self.op_reveal(args).map(|()| Effect::None),
            "scroll" => self.op_scroll(args).map(|()| Effect::None),
            "suspend" => Ok(Effect::Suspend),
            "resume" => Ok(Effect::Resume),
            other => Err(format!("unknown op {other}")),
        }
    }

    fn op_add(&mut self, args: &[Value], ctx: &OpContext<'_>) -> Result<(), String> {
        let [id, parent, before, node] = args else {
            return Err("add takes id, parent, before and node".into());
        };
        let id = str_arg(id, "add", "id")?;
        let parent = str_arg(parent, "add", "parent")?;
        let parent_slot = self.slot(parent)?;
        let before_slot = self.before(before, parent_slot, "add")?;
        let at_root = parent_slot == self.root;

        let mut staged = Vec::new();
        let mut seen = BTreeSet::new();
        let height = self.stage(node, at_root, ctx, &mut staged, &mut seen, 1)?;
        let staged_id = &staged[0].1.id;
        if staged_id != id {
            return Err(format!(
                "add: id {id} doesn't match the node's id {staged_id}"
            ));
        }
        if at_root && !REGIONS.contains(&id) {
            return Err(format!(
                "{id}: only main, dock and layer go under the surface root"
            ));
        }
        if self.depth(parent_slot) + height > MAX_DEPTH {
            return Err(format!("{id}: deeper than {MAX_DEPTH}"));
        }
        if self.count + staged.len() > MAX_NODES {
            return Err(format!("{id}: more than {MAX_NODES} nodes"));
        }

        // Insert: staged is pre-order, each with its parent's index in `staged`.
        let mut slots: Vec<usize> = Vec::with_capacity(staged.len());
        for (parent_index, mut node) in staged {
            let parent = parent_index.map_or(parent_slot, |i| slots[i]);
            node.parent = Some(parent);
            let slot = self.insert(node);
            if parent_index.is_some() {
                self.node_mut(parent).children.push(slot);
            }
            slots.push(slot);
        }
        let top = slots[0];
        let siblings = &mut self.node_mut(parent_slot).children;
        match before_slot.and_then(|b| siblings.iter().position(|&c| c == b)) {
            Some(pos) => siblings.insert(pos, top),
            None => siblings.push(top),
        }
        self.changes.children.insert(parent.to_owned());
        self.unsettle(parent_slot);
        Ok(())
    }

    /// Validates `node` and its subtree, appending `(parent index, node)` in pre-order. Returns
    /// the subtree's height.
    fn stage(
        &self,
        node: &Value,
        region: bool,
        ctx: &OpContext<'_>,
        staged: &mut Vec<(Option<usize>, Node)>,
        seen: &mut BTreeSet<String>,
        depth: usize,
    ) -> Result<usize, String> {
        let Some(obj) = node.as_object() else {
            return Err("a node must be an object".into());
        };
        let id = match obj.get("id") {
            Some(Value::String(id)) if !id.is_empty() => id.clone(),
            _ => return Err("a node needs a non-empty string id".into()),
        };
        if depth > MAX_DEPTH {
            return Err(format!("{id}: deeper than {MAX_DEPTH}"));
        }
        if self.index.contains_key(&id) || !seen.insert(id.clone()) {
            return Err(format!("duplicate id {id}"));
        }
        if !region && REGIONS.contains(&id.as_str()) {
            return Err(format!("{id} is a region and goes under the surface root"));
        }
        let Some(kind) = obj.get("k").and_then(Value::as_str) else {
            return Err(format!("{id}: a node needs a string k"));
        };
        // A region node's kind is ignored (§7.5).
        if !region
            && let Some(kinds) = ctx.kinds
            && !kinds.contains(kind)
        {
            return Err(format!("{id}: unknown kind {kind}"));
        }
        let props = match obj.get("p") {
            None | Some(Value::Null) => Map::new(),
            Some(Value::Object(p)) => p
                .iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            Some(_) => return Err(format!("{id}: p must be an object")),
        };
        let children: &[Value] = match obj.get("c") {
            None | Some(Value::Null) => &[],
            Some(Value::Array(c)) => c,
            Some(_) => return Err(format!("{id}: c must be an array")),
        };
        let me = staged.len();
        let parent_index = if depth == 1 { None } else { Some(usize::MAX) };
        staged.push((
            parent_index,
            Node {
                id,
                kind: kind.to_owned(),
                props,
                children: Vec::new(),
                parent: None,
                settled: false,
                cost: 0,
            },
        ));
        let mut height = 0;
        for child in children {
            let first = staged.len();
            height = height.max(self.stage(child, false, ctx, staged, seen, depth + 1)?);
            staged[first].0 = Some(me);
        }
        Ok(height + 1)
    }

    /// Checks `before`: `null`, or a child of `parent`.
    fn before(&self, before: &Value, parent: usize, op: &str) -> Result<Option<usize>, String> {
        match before {
            Value::Null => Ok(None),
            Value::String(b) => {
                let slot = self.slot(b)?;
                if self.node(slot).parent != Some(parent) {
                    return Err(format!(
                        "{op}: {b} is not a child of {}",
                        self.node(parent).id
                    ));
                }
                Ok(Some(slot))
            }
            _ => Err(format!("{op}: before must be a string or null")),
        }
    }

    fn not_root(&self, slot: usize, op: &str) -> Result<(), String> {
        if slot == self.root {
            return Err(format!("{op}: the surface root can't be changed"));
        }
        Ok(())
    }

    fn op_set(&mut self, args: &[Value]) -> Result<(), String> {
        let [id, props] = args else {
            return Err("set takes id and props".into());
        };
        let id = str_arg(id, "set", "id")?;
        let slot = self.slot(id)?;
        self.not_root(slot, "set")?;
        let Some(props) = props.as_object() else {
            return Err("set: props must be an object".into());
        };
        let node = self.node_mut(slot);
        for (k, v) in props {
            if v.is_null() {
                node.props.shift_remove(k);
            } else {
                node.props.insert(k.clone(), v.clone());
            }
        }
        self.touched(slot);
        Ok(())
    }

    /// Records a change to a node's props and clears settle hints above it.
    fn touched(&mut self, slot: usize) {
        self.recost(slot);
        self.changes.nodes.insert(self.node(slot).id.clone());
        self.unsettle(slot);
    }

    fn text_slot(&self, id: &Value, op: &str) -> Result<usize, String> {
        let id = str_arg(id, op, "id")?;
        let slot = self.slot(id)?;
        let kind = &self.node(slot).kind;
        if slot == self.root || !is_text_kind(kind) {
            return Err(format!("{op}: {id} ({kind}) has no primary text"));
        }
        Ok(slot)
    }

    fn op_text(&mut self, args: &[Value]) -> Result<(), String> {
        let [id, mode, s] = args else {
            return Err("text takes id, mode and text".into());
        };
        let slot = self.text_slot(id, "text")?;
        let append = match mode.as_str() {
            Some("append") => true,
            Some("replace") => false,
            _ => return Err("text: mode must be append or replace".into()),
        };
        let Some(s) = s.as_str() else {
            return Err("text: the text must be a string".into());
        };
        let props = &mut self.node_mut(slot).props;
        match props.get_mut("text") {
            Some(Value::String(old)) if append => old.push_str(s),
            _ => {
                props.insert("text".into(), Value::String(s.to_owned()));
            }
        }
        self.touched(slot);
        Ok(())
    }

    fn op_splice(&mut self, args: &[Value]) -> Result<(), String> {
        let [id, at, del, s] = args else {
            return Err("splice takes id, at, del and text".into());
        };
        let slot = self.text_slot(id, "splice")?;
        let (Some(at), Some(del)) = (at.as_u64(), del.as_u64()) else {
            return Err("splice: at and del must be non-negative integers".into());
        };
        let Some(s) = s.as_str() else {
            return Err("splice: the text must be a string".into());
        };
        let old = self
            .node(slot)
            .props
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("");
        let new = splice_utf16(old, at, del, s)?;
        self.node_mut(slot)
            .props
            .insert("text".into(), Value::String(new));
        self.touched(slot);
        Ok(())
    }

    fn op_move(&mut self, args: &[Value]) -> Result<(), String> {
        let [id, parent, before] = args else {
            return Err("move takes id, parent and before".into());
        };
        let id = str_arg(id, "move", "id")?;
        let parent = str_arg(parent, "move", "parent")?;
        let slot = self.slot(id)?;
        self.not_root(slot, "move")?;
        let parent_slot = self.slot(parent)?;
        if self.is_within(parent_slot, slot) {
            return Err(format!("move: {parent} is inside {id}"));
        }
        let is_region = self.node(slot).parent == Some(self.root);
        if is_region != (parent_slot == self.root) {
            return Err(format!(
                "move: {id} can't move between the surface root and a region"
            ));
        }
        let before_slot = self.before(before, parent_slot, "move")?;
        if before_slot == Some(slot) {
            return Err(format!("move: {id} can't go before itself"));
        }
        if self.depth(parent_slot) + self.height(slot) > MAX_DEPTH {
            return Err(format!("move: {id} would be deeper than {MAX_DEPTH}"));
        }
        let old_parent = self.node(slot).parent.expect("non-root has a parent");
        self.unsettle(old_parent);
        self.node_mut(old_parent).children.retain(|&c| c != slot);
        let siblings = &mut self.node_mut(parent_slot).children;
        match before_slot.and_then(|b| siblings.iter().position(|&c| c == b)) {
            Some(pos) => siblings.insert(pos, slot),
            None => siblings.push(slot),
        }
        self.node_mut(slot).parent = Some(parent_slot);
        self.unsettle(slot);
        let old_id = self.node(old_parent).id.clone();
        self.changes.children.insert(old_id);
        self.changes.children.insert(parent.to_owned());
        Ok(())
    }

    fn op_del(&mut self, args: &[Value]) -> Result<(), String> {
        let [id] = args else {
            return Err("del takes id".into());
        };
        let id = str_arg(id, "del", "id")?;
        let slot = self.slot(id)?;
        self.not_root(slot, "del")?;
        let parent = self.node(slot).parent.expect("non-root has a parent");
        self.unsettle(parent);
        self.remove(slot);
        Ok(())
    }

    /// Removes a subtree: from its parent, the index, focus and pending view requests.
    pub(crate) fn remove(&mut self, slot: usize) {
        let parent = self.node(slot).parent.expect("non-root has a parent");
        self.node_mut(parent).children.retain(|&c| c != slot);
        let parent_id = self.node(parent).id.clone();
        self.changes.children.insert(parent_id);
        let mut stack = vec![slot];
        while let Some(s) = stack.pop() {
            let node = self.slots[s].take().expect("live slot");
            stack.extend(&node.children);
            self.index.remove(&node.id);
            self.count -= 1;
            self.cost -= node.cost;
            self.free.push(s);
            if self.focus.as_deref() == Some(node.id.as_str()) {
                self.focus = None;
                self.changes.focus = true;
            }
            if self.reveal.as_ref().is_some_and(|r| r.id == node.id) {
                self.reveal = None;
            }
            self.scrolls.retain(|r| r.id != node.id);
            self.changes.nodes.remove(&node.id);
            self.changes.children.remove(&node.id);
            self.changes.removed.insert(node.id);
        }
    }

    /// Removes the node `id` and its subtree, if present (close drops `dock` and `layer`).
    pub(crate) fn remove_id(&mut self, id: &str) -> bool {
        match self.index.get(id).copied() {
            Some(slot) if slot != self.root => {
                self.remove(slot);
                true
            }
            _ => false,
        }
    }

    fn op_settle(&mut self, args: &[Value]) -> Result<(), String> {
        let [id] = args else {
            return Err("settle takes id".into());
        };
        let id = str_arg(id, "settle", "id")?;
        let slot = self.slot(id)?;
        let node = self.node_mut(slot);
        if !node.settled {
            node.settled = true;
            self.changes.nodes.insert(id.to_owned());
        }
        Ok(())
    }

    fn op_focus(&mut self, args: &[Value]) -> Result<(), String> {
        let [id] = args else {
            return Err("focus takes id or null".into());
        };
        let focus = match id {
            Value::Null => None,
            Value::String(id) => {
                self.slot(id)?;
                Some(id.clone())
            }
            _ => return Err("focus: id must be a string or null".into()),
        };
        self.set_focus(focus);
        Ok(())
    }

    pub(crate) fn set_focus(&mut self, focus: Option<String>) {
        if self.focus != focus {
            self.focus = focus;
            self.changes.focus = true;
        }
    }

    fn op_reveal(&mut self, args: &[Value]) -> Result<(), String> {
        let (id, at) = match args {
            [id] => (id, None),
            [id, at, ..] => (id, Some(at)),
            [] => return Err("reveal takes id and position".into()),
        };
        let id = str_arg(id, "reveal", "id")?;
        self.slot(id)?;
        self.reveal = Some(Reveal {
            id: id.to_owned(),
            at: RevealAt::parse(at),
        });
        Ok(())
    }

    fn op_scroll(&mut self, args: &[Value]) -> Result<(), String> {
        let [id, by] = args else {
            return Err("scroll takes id and step".into());
        };
        let id = str_arg(id, "scroll", "id")?;
        self.slot(id)?;
        let Some(by) = by.as_str().and_then(ScrollBy::parse) else {
            return Err(format!("scroll: unknown step {by}"));
        };
        self.scrolls.push(Scroll {
            id: id.to_owned(),
            by,
        });
        Ok(())
    }

    /// The form `values` for an event from node `id` (§7.8): `None` unless the node is inside an
    /// `el` `form`. `checked` gives a control's local state where the user flipped it; otherwise
    /// its `checked` prop counts.
    pub fn form_values(
        &self,
        id: &str,
        checked: &dyn Fn(&str) -> Option<bool>,
    ) -> Option<Map<String, Value>> {
        let mut slot = *self.index.get(id)?;
        let form = loop {
            if el_tag(self.node(slot)) == Some("form") {
                break slot;
            }
            slot = self.node(slot).parent?;
        };
        // Groups by name in first-appearance order: (any radio, controls as (value, checked)).
        let mut groups: Vec<Group> = Vec::new();
        let mut stack: Vec<usize> = self.node(form).children.iter().rev().copied().collect();
        while let Some(s) = stack.pop() {
            let node = self.node(s);
            if el_tag(node) == Some("form") {
                continue;
            }
            stack.extend(node.children.iter().rev());
            if el_tag(node) != Some("input") {
                continue;
            }
            let p = &node.props;
            let radio = match p.get("type").and_then(Value::as_str) {
                Some("radio") => true,
                Some("checkbox") => false,
                _ => continue,
            };
            let Some(name) = p
                .get("name")
                .and_then(Value::as_str)
                .filter(|n| !n.is_empty())
            else {
                continue;
            };
            if truthy(p.get("disabled")) {
                continue;
            }
            let value = p
                .get("value")
                .cloned()
                .unwrap_or_else(|| Value::String("on".into()));
            let on = checked(&node.id).unwrap_or_else(|| truthy(p.get("checked")));
            match groups.iter_mut().find(|(n, _, _)| n == name) {
                Some((_, any_radio, controls)) => {
                    *any_radio |= radio;
                    controls.push((value, on));
                }
                None => groups.push((name.to_owned(), radio, vec![(value, on)])),
            }
        }
        let mut values = Map::new();
        for (name, radio, controls) in groups {
            let v = if radio {
                controls
                    .into_iter()
                    .find(|(_, on)| *on)
                    .map_or(Value::Null, |(v, _)| v)
            } else if controls.len() == 1 {
                Value::Bool(controls[0].1)
            } else {
                Value::Array(
                    controls
                        .into_iter()
                        .filter(|(_, on)| *on)
                        .map(|(v, _)| v)
                        .collect(),
                )
            };
            values.insert(name, v);
        }
        Some(values)
    }

    /// Removes the oldest settled top-level child of `main`, returning its id (retention, §7.10).
    pub(crate) fn evict_settled(&mut self) -> Option<String> {
        let main = self.region("main")?.slot;
        let victim = self
            .node(main)
            .children
            .iter()
            .copied()
            .find(|&c| self.node(c).settled)?;
        let id = self.node(victim).id.clone();
        self.remove(victim);
        Some(id)
    }
}

/// A form group: its name, whether it holds a radio, and its controls as `(value, checked)`.
type Group = (String, bool, Vec<(Value, bool)>);

fn el_tag(node: &Node) -> Option<&str> {
    (node.kind == "el")
        .then(|| node.props.get("tag").and_then(Value::as_str))
        .flatten()
}

/// JSON truthiness for flags programs send as booleans.
pub(crate) fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => true,
    }
}

fn str_arg<'a>(v: &'a Value, op: &str, what: &str) -> Result<&'a str, String> {
    match v.as_str() {
        Some(s) if !s.is_empty() => Ok(s),
        _ => Err(format!("{op}: {what} must be a non-empty string")),
    }
}

/// The length of `s` in UTF-16 code units.
pub fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// Replaces `del` UTF-16 units of `old` at UTF-16 offset `at` with `with`.
pub fn splice_utf16(old: &str, at: u64, del: u64, with: &str) -> Result<String, String> {
    let byte_at = |unit: u64| -> Result<usize, String> {
        let mut units = 0u64;
        for (byte, ch) in old.char_indices() {
            if units == unit {
                return Ok(byte);
            }
            units += ch.len_utf16() as u64;
            if units > unit {
                return Err(format!("splice: offset {unit} splits a surrogate pair"));
            }
        }
        if units == unit {
            Ok(old.len())
        } else {
            Err(format!("splice: offset {unit} is past the end ({units})"))
        }
    };
    let end = at.checked_add(del).ok_or("splice: range overflows")?;
    let from = byte_at(at)?;
    let to = byte_at(end)?;
    let mut out = String::with_capacity(old.len() - (to - from) + with.len());
    out.push_str(&old[..from]);
    out.push_str(with);
    out.push_str(&old[to..]);
    Ok(out)
}

/// A node, for reading.
#[derive(Clone, Copy, Debug)]
pub struct NodeRef<'a> {
    doc: &'a Document,
    slot: usize,
}

impl<'a> NodeRef<'a> {
    fn node(&self) -> &'a Node {
        self.doc.node(self.slot)
    }

    pub fn id(&self) -> &'a str {
        &self.node().id
    }

    pub fn kind(&self) -> &'a str {
        &self.node().kind
    }

    /// Props as last set, `null`s dropped, in the order received.
    pub fn props(&self) -> &'a Map<String, Value> {
        &self.node().props
    }

    pub fn prop(&self, key: &str) -> Option<&'a Value> {
        self.node().props.get(key)
    }

    /// The primary text (`p.text`) of a text kind.
    pub fn text(&self) -> Option<&'a str> {
        self.prop("text").and_then(Value::as_str)
    }

    pub fn children(&self) -> impl Iterator<Item = NodeRef<'a>> + 'a {
        let doc = self.doc;
        self.node()
            .children
            .iter()
            .map(move |&slot| NodeRef { doc, slot })
    }

    pub fn child_count(&self) -> usize {
        self.node().children.len()
    }

    pub fn parent(&self) -> Option<NodeRef<'a>> {
        self.node().parent.map(|slot| NodeRef {
            doc: self.doc,
            slot,
        })
    }

    /// Whether the program sent `settle` for it, and nothing inside has changed since.
    pub fn settled(&self) -> bool {
        self.node().settled
    }

    /// The node as wire JSON `{id, k, p?, c?}`, subtree included.
    pub fn to_json(&self) -> Value {
        let node = self.node();
        let mut obj = Map::new();
        obj.insert("id".into(), Value::String(node.id.clone()));
        obj.insert("k".into(), Value::String(node.kind.clone()));
        if !node.props.is_empty() {
            obj.insert("p".into(), Value::Object(node.props.clone()));
        }
        if !node.children.is_empty() {
            obj.insert(
                "c".into(),
                Value::Array(self.children().map(|c| c.to_json()).collect()),
            );
        }
        Value::Object(obj)
    }

    /// Like `to_json`, with each node's props passed through `rewrite` first.
    pub fn to_json_with(
        &self,
        rewrite: &mut dyn FnMut(NodeRef<'_>, &mut Map<String, Value>),
    ) -> Value {
        let node = self.node();
        let mut props = node.props.clone();
        rewrite(*self, &mut props);
        let mut obj = Map::new();
        obj.insert("id".into(), Value::String(node.id.clone()));
        obj.insert("k".into(), Value::String(node.kind.clone()));
        if !props.is_empty() {
            obj.insert("p".into(), Value::Object(props));
        }
        if !node.children.is_empty() {
            obj.insert(
                "c".into(),
                Value::Array(self.children().map(|c| c.to_json_with(rewrite)).collect()),
            );
        }
        Value::Object(obj)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn kinds() -> BTreeSet<String> {
        ["col", "row", "text", "md", "editor", "el", "card"]
            .into_iter()
            .map(String::from)
            .collect()
    }

    fn apply(doc: &mut Document, op: Value) -> Result<(), String> {
        let k = kinds();
        doc.apply(&op, &OpContext { kinds: Some(&k) }).map(|_| ())
    }

    fn doc() -> Document {
        let mut d = Document::new("s1");
        apply(
            &mut d,
            json!(["add", "main", "s1", null, {"id": "main", "k": "col", "c": [
                {"id": "a", "k": "text", "p": {"text": "x"}},
                {"id": "b", "k": "col", "c": [{"id": "b1", "k": "text"}]}
            ]}]),
        )
        .unwrap();
        d
    }

    fn order(d: &Document, id: &str) -> Vec<String> {
        d.get(id)
            .unwrap()
            .children()
            .map(|c| c.id().to_owned())
            .collect()
    }

    #[test]
    fn add_is_all_or_nothing() {
        let mut d = doc();
        let before = d.len();
        let err = apply(
            &mut d,
            json!(["add", "c", "main", null, {"id": "c", "k": "col", "c": [
            {"id": "c1", "k": "text"}, {"id": "c2", "k": "nope"}]}]),
        );
        assert_eq!(err, Err("c2: unknown kind nope".into()));
        assert_eq!(d.len(), before);
        assert!(d.get("c1").is_none());
        assert_eq!(
            apply(
                &mut d,
                json!(["add", "c", "main", null, {"id": "c", "k": "col", "c": [{"id": "a", "k": "text"}]}])
            ),
            Err("duplicate id a".into())
        );
        assert_eq!(
            apply(
                &mut d,
                json!(["add", "c", "main", null, {"id": "c", "k": "col", "c": [{"id": "d", "k": "text"}, {"id": "d", "k": "text"}]}])
            ),
            Err("duplicate id d".into())
        );
        assert!(
            apply(&mut d, json!(["add", "c", "main", {"id": "c", "k": "col"}])).is_err(),
            "before must be present"
        );
        assert!(
            apply(
                &mut d,
                json!(["add", "c", "main", null, {"id": "x", "k": "col"}])
            )
            .is_err()
        );
        assert!(
            apply(
                &mut d,
                json!(["add", "c", "s1", null, {"id": "c", "k": "col"}])
            )
            .is_err(),
            "non-region under root"
        );
        assert_eq!(d.len(), before);
    }

    #[test]
    fn add_before_a_sibling() {
        let mut d = doc();
        apply(
            &mut d,
            json!(["add", "z", "main", "b", {"id": "z", "k": "text"}]),
        )
        .unwrap();
        assert_eq!(order(&d, "main"), ["a", "z", "b"]);
        assert!(
            apply(
                &mut d,
                json!(["add", "y", "main", "b1", {"id": "y", "k": "text"}])
            )
            .is_err()
        );
    }

    #[test]
    fn depth_limit() {
        let mut d = Document::new("s1");
        let mut node = json!({"id": "n63", "k": "col"});
        for i in (0..63).rev() {
            node = json!({"id": format!("n{i}"), "k": "col", "c": [node]});
        }
        // root + main + n1..n63 (63 nodes) = 65 levels: too deep.
        let main = json!({"id": "main", "k": "col", "c": [node["c"][0].clone()]});
        assert_eq!(
            apply(&mut d, json!(["add", "main", "s1", null, main])),
            Err("main: deeper than 64".into())
        );
        // root + main + n2..n63 (62 nodes) = 64 levels.
        let main = json!({"id": "main", "k": "col", "c": [node["c"][0]["c"][0].clone()]});
        apply(&mut d, json!(["add", "main", "s1", null, main])).unwrap();
        assert_eq!(d.len(), 1 + 1 + 62);
    }

    #[test]
    fn set_merges_and_null_deletes() {
        let mut d = doc();
        apply(&mut d, json!(["set", "a", {"tone": "info", "text": null}])).unwrap();
        assert_eq!(
            d.get("a").unwrap().props(),
            json!({"tone": "info"}).as_object().unwrap()
        );
        assert!(
            apply(&mut d, json!(["set", "s1", {"x": 1}])).is_err(),
            "root is fixed"
        );
        assert!(apply(&mut d, json!(["set", "nope", {}])).is_err());
    }

    #[test]
    fn text_append_and_replace() {
        let mut d = doc();
        apply(&mut d, json!(["text", "a", "append", "yz"])).unwrap();
        apply(&mut d, json!(["text", "b1", "append", "new"])).unwrap();
        assert_eq!(d.get("a").unwrap().text(), Some("xyz"));
        assert_eq!(d.get("b1").unwrap().text(), Some("new"));
        apply(&mut d, json!(["text", "a", "replace", "r"])).unwrap();
        assert_eq!(d.get("a").unwrap().text(), Some("r"));
        assert!(
            apply(&mut d, json!(["text", "b", "append", "x"])).is_err(),
            "col has no text"
        );
        assert!(apply(&mut d, json!(["text", "a", "prepend", "x"])).is_err());
    }

    #[test]
    fn splice_counts_utf16() {
        assert_eq!(splice_utf16("a😀b", 3, 1, "c").unwrap(), "a😀c");
        assert_eq!(splice_utf16("a😀b", 1, 2, "").unwrap(), "ab");
        assert!(splice_utf16("a😀b", 2, 0, "x").is_err(), "inside a pair");
        assert!(
            splice_utf16("a😀b", 1, 1, "x").is_err(),
            "end inside a pair"
        );
        assert!(splice_utf16("ab", 1, 2, "x").is_err(), "past the end");
        assert_eq!(splice_utf16("ab", 2, 0, "c").unwrap(), "abc");
    }

    #[test]
    fn move_rules() {
        let mut d = doc();
        assert!(
            apply(&mut d, json!(["move", "b", "b1", null])).is_err(),
            "into own subtree"
        );
        assert!(apply(&mut d, json!(["move", "b", "b", null])).is_err());
        assert!(
            apply(&mut d, json!(["move", "a", "main", "a"])).is_err(),
            "before itself"
        );
        assert!(
            apply(&mut d, json!(["move", "a", "main", "b1"])).is_err(),
            "before not a child"
        );
        assert!(
            apply(&mut d, json!(["move", "s1", "main", null])).is_err(),
            "root"
        );
        apply(&mut d, json!(["move", "b", "main", "a"])).unwrap();
        assert_eq!(order(&d, "main"), ["b", "a"]);
        apply(&mut d, json!(["move", "a", "b", null])).unwrap();
        assert_eq!(order(&d, "b"), ["b1", "a"]);
    }

    #[test]
    fn del_removes_subtree_and_focus() {
        let mut d = doc();
        apply(&mut d, json!(["focus", "b1"])).unwrap();
        assert_eq!(d.focus(), Some("b1"));
        let cost = d.cost();
        apply(&mut d, json!(["del", "b"])).unwrap();
        assert_eq!(d.focus(), None);
        assert!(d.get("b1").is_none());
        assert!(d.cost() < cost);
        assert!(apply(&mut d, json!(["del", "s1"])).is_err());
        // The ids are free again.
        apply(
            &mut d,
            json!(["add", "b", "main", null, {"id": "b", "k": "text"}]),
        )
        .unwrap();
    }

    #[test]
    fn settle_is_cleared_by_ops_inside() {
        let mut d = doc();
        apply(&mut d, json!(["settle", "b"])).unwrap();
        apply(&mut d, json!(["settle", "main"])).unwrap();
        assert_eq!(d.settled(), ["main", "b"]);
        apply(&mut d, json!(["set", "a", {"x": 1}])).unwrap();
        assert_eq!(d.settled(), ["b"], "a's ancestors are unsettled");
        apply(&mut d, json!(["text", "b1", "append", "z"])).unwrap();
        assert!(d.settled().is_empty());
    }

    #[test]
    fn cost_tracks_text() {
        let mut d = doc();
        let before = d.cost();
        apply(&mut d, json!(["text", "a", "append", "0123456789"])).unwrap();
        assert_eq!(d.cost(), before + 10);
    }

    #[test]
    fn form_values_follow_the_rules() {
        let mut d = Document::new("s1");
        let input = |id: &str, ty: &str, name: &str, value: &str, checked: bool| json!({"id": id, "k": "el", "p": {"tag": "input", "type": ty, "name": name, "value": value, "checked": checked}});
        apply(&mut d, json!(["add", "main", "s1", null, {"id": "main", "k": "col", "c": [
            {"id": "f", "k": "el", "p": {"tag": "form"}, "c": [
                input("r1", "radio", "size", "s", false),
                input("c1", "checkbox", "agree", "on", true),
                input("r2", "radio", "size", "m", true),
                input("t1", "checkbox", "tags", "a", true),
                input("t2", "checkbox", "tags", "b", false),
                input("t3", "checkbox", "tags", "c", true),
                {"id": "dis", "k": "el", "p": {"tag": "input", "type": "checkbox", "name": "off", "disabled": true}},
                {"id": "nn", "k": "el", "p": {"tag": "input", "type": "checkbox"}},
                {"id": "inner", "k": "el", "p": {"tag": "form"}, "c": [input("x", "checkbox", "nested", "on", true)]},
                input("q1", "radio", "none", "1", false),
                {"id": "go", "k": "el", "p": {"tag": "button", "actions": {"click": "submit"}}}
            ]},
            {"id": "out", "k": "text"}
        ]}]))
        .unwrap();
        let values = d.form_values("go", &|_| None).unwrap();
        assert_eq!(
            Value::Object(values.clone()),
            json!({"size": "m", "agree": true, "tags": ["a", "c"], "none": null})
        );
        assert_eq!(
            values.keys().collect::<Vec<_>>(),
            ["size", "agree", "tags", "none"]
        );
        // Local flips override the props.
        let values = d
            .form_values("go", &|id| (id == "c1").then_some(false))
            .unwrap();
        assert_eq!(values["agree"], json!(false));
        assert_eq!(d.form_values("out", &|_| None), None);
        assert_eq!(
            Value::Object(d.form_values("x", &|_| None).unwrap()),
            json!({"nested": true})
        );
    }
}
