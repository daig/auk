//! Sessions, tabs and split trees (§4.3): daemon state, replicated to every window that shows
//! the session. Focus belongs to each window and is not here.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::PaneId;

pub type SessionId = u64;
pub type TabId = u64;

/// Which way a split places its second child.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitDir {
    /// Side by side; the new pane goes right.
    Right,
    /// Stacked; the new pane goes below.
    Down,
}

/// A side of a pane (§9.8 `Dir`): where a split puts the new pane, where a move puts the moved
/// one, or which divider a resize moves. A split node keeps its children left to right or top to
/// bottom, so a pane put left of or above another becomes the split's first child.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Right,
    Down,
    Left,
    Up,
}

impl Side {
    /// The split direction along this side's axis.
    pub fn axis(self) -> SplitDir {
        match self {
            Side::Right | Side::Left => SplitDir::Right,
            Side::Down | Side::Up => SplitDir::Down,
        }
    }

    /// Whether this side comes first in a split (left or above).
    pub fn before(self) -> bool {
        matches!(self, Side::Left | Side::Up)
    }
}

impl From<SplitDir> for Side {
    fn from(dir: SplitDir) -> Side {
        match dir {
            SplitDir::Right => Side::Right,
            SplitDir::Down => Side::Down,
        }
    }
}

/// The smallest share a resize or a requested ratio leaves either side of a split, in thousandths.
pub const MIN_SHARE: u16 = 50;

/// A share in thousandths, kept within [`MIN_SHARE`] of either end.
pub fn clamp_share(share: i32) -> u16 {
    share.clamp(i32::from(MIN_SHARE), 1000 - i32::from(MIN_SHARE)) as u16
}

/// A split tree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "node", rename_all = "snake_case")]
pub enum Node {
    Pane {
        pane: PaneId,
    },
    Split {
        dir: SplitDir,
        /// The first child's share, in thousandths (integers keep replicas exact).
        ratio: u16,
        first: Box<Node>,
        second: Box<Node>,
    },
}

impl Node {
    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect(&self, out: &mut Vec<PaneId>) {
        match self {
            Node::Pane { pane } => out.push(*pane),
            Node::Split { first, second, .. } => {
                first.collect(out);
                second.collect(out);
            }
        }
    }

    pub fn contains(&self, pane: PaneId) -> bool {
        match self {
            Node::Pane { pane: p } => *p == pane,
            Node::Split { first, second, .. } => first.contains(pane) || second.contains(pane),
        }
    }

    /// Replaces the leaf `pane` with a split of it and `new`, `new` on `side`. `share` is
    /// `pane`'s share in thousandths. Returns whether it was found.
    fn split(&mut self, pane: PaneId, side: Side, new: PaneId, share: u16) -> bool {
        match self {
            Node::Pane { pane: p } if *p == pane => {
                let (old, new) = (Node::Pane { pane }, Node::Pane { pane: new });
                let (first, second, ratio) = if side.before() {
                    (new, old, 1000 - share)
                } else {
                    (old, new, share)
                };
                *self = Node::Split {
                    dir: side.axis(),
                    ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                };
                true
            }
            Node::Pane { .. } => false,
            Node::Split { first, second, .. } => {
                first.split(pane, side, new, share) || second.split(pane, side, new, share)
            }
        }
    }

    /// Exchanges the leaves `a` and `b` wherever they are in this tree.
    fn swap(&mut self, a: PaneId, b: PaneId) {
        match self {
            Node::Pane { pane } if *pane == a => *pane = b,
            Node::Pane { pane } if *pane == b => *pane = a,
            Node::Pane { .. } => {}
            Node::Split { first, second, .. } => {
                first.swap(a, b);
                second.swap(a, b);
            }
        }
    }

    /// The path (`false` for the first child, `true` for the second) to the split whose divider
    /// is `pane`'s nearest on `side`: the innermost split along that axis with `pane` on the other
    /// side of it.
    pub fn divider(&self, pane: PaneId, side: Side) -> Option<Vec<bool>> {
        let Node::Split {
            dir, first, second, ..
        } = self
        else {
            return None;
        };
        let (inside, second_side) = if first.contains(pane) {
            (first, false)
        } else if second.contains(pane) {
            (second, true)
        } else {
            return None;
        };
        if let Some(mut path) = inside.divider(pane, side) {
            path.insert(0, second_side);
            return Some(path);
        }
        // The divider is on `side` of the pane when the pane is in the child away from it.
        (*dir == side.axis() && second_side == side.before()).then(Vec::new)
    }

    /// The node at `path`.
    pub fn at(&self, path: &[bool]) -> Option<&Node> {
        match (path.split_first(), self) {
            (None, node) => Some(node),
            (Some((&second, rest)), Node::Split { first, second: s, .. }) => {
                if second { s.at(rest) } else { first.at(rest) }
            }
            (Some(_), Node::Pane { .. }) => None,
        }
    }

    fn at_mut(&mut self, path: &[bool]) -> Option<&mut Node> {
        match (path.split_first(), self) {
            (None, node) => Some(node),
            (Some((&second, rest)), Node::Split { first, second: s, .. }) => {
                if second {
                    s.at_mut(rest)
                } else {
                    first.at_mut(rest)
                }
            }
            (Some(_), Node::Pane { .. }) => None,
        }
    }

    /// Removes the leaf `pane`, collapsing its parent split. Returns the remaining tree, or
    /// `None` when nothing is left.
    fn remove(self, pane: PaneId) -> Option<Node> {
        match self {
            Node::Pane { pane: p } if p == pane => None,
            leaf @ Node::Pane { .. } => Some(leaf),
            Node::Split {
                dir,
                ratio,
                first,
                second,
            } => match (first.remove(pane), second.remove(pane)) {
                (Some(a), Some(b)) => Some(Node::Split {
                    dir,
                    ratio,
                    first: Box::new(a),
                    second: Box::new(b),
                }),
                (Some(only), None) | (None, Some(only)) => Some(only),
                (None, None) => None,
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tab {
    pub id: TabId,
    pub title: Option<String>,
    pub root: Node,
    /// The zoomed pane, if any.
    pub zoom: Option<PaneId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub name: String,
    /// A locked session refuses closing itself, its tabs and its panes (book:16896): the daemon
    /// answers such a close with a toast instead. New sessions start unlocked.
    pub locked: bool,
    /// The user gave the session its name (at creation or by renaming it), rather than taking
    /// the generated one: windows show a named session's chip even when it is the only one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub named: bool,
    pub tabs: Vec<Tab>,
}

/// Adjectives and animals for generated session names (`brisk-otter`), as Tern names new
/// sessions with two random words (book:16894).
const NAME_ADJECTIVES: &[&str] = &[
    "amber", "brisk", "calm", "clever", "cosy", "dapper", "eager", "fair", "gentle", "golden",
    "hardy", "humble", "jolly", "keen", "lively", "lucky", "mellow", "merry", "nimble", "noble",
    "plucky", "quiet", "rapid", "rosy", "rustic", "sunny", "swift", "tidy", "vivid", "wise",
    "witty", "zesty",
];
const NAME_ANIMALS: &[&str] = &[
    "badger", "beaver", "bison", "crane", "dingo", "eagle", "falcon", "ferret", "finch", "gecko",
    "heron", "ibis", "jackal", "koala", "lemur", "lynx", "marten", "moose", "newt", "ocelot",
    "otter", "panda", "puffin", "quail", "raven", "robin", "seal", "stoat", "tapir", "walrus",
    "wombat", "yak",
];

/// A random two-word name not among `taken`: re-rolled on a collision, with a number added once
/// most pairs are in use.
fn generated_name(taken: &[&str]) -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut seed = std::collections::hash_map::RandomState::new().build_hasher();
    seed.write_usize(taken.len());
    let mut x = seed.finish() | 1;
    let pairs = NAME_ADJECTIVES.len() * NAME_ANIMALS.len();
    let mut attempt = 0;
    loop {
        // xorshift64: a fresh pair per attempt.
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let pair = (x % pairs as u64) as usize;
        let mut name = format!(
            "{}-{}",
            NAME_ADJECTIVES[pair / NAME_ANIMALS.len()],
            NAME_ANIMALS[pair % NAME_ANIMALS.len()]
        );
        if attempt >= 64 {
            name = format!("{name}-{}", attempt - 62);
        }
        if !taken.contains(&name.as_str()) {
            return name;
        }
        attempt += 1;
    }
}

/// What kind of pane a layout cell holds (§5.1), so every window can badge it: an agent pane is
/// a command pane that windows show with the agent icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneTag {
    Shell,
    Command,
    Agent,
    Block,
}

impl PaneTag {
    /// The icon windows show for the pane (an icon name of `auk_plugins::icons`).
    pub fn icon(self) -> &'static str {
        match self {
            PaneTag::Shell | PaneTag::Command => "terminal",
            PaneTag::Agent => "agent",
            PaneTag::Block => "puzzle",
        }
    }
}

/// The sessions one window key names.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Window {
    pub sessions: Vec<Session>,
    /// Each pane's kind, for its badge. Panes missing here are shells. In JSON, an object keyed
    /// by pane id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty", with = "kinds_map")]
    pub kinds: BTreeMap<PaneId, PaneTag>,
}

/// `kinds` as a JSON object with string keys. Keys go through strings explicitly: a map with
/// integer keys doesn't read back inside the internally tagged messages that carry layouts.
mod kinds_map {
    use std::collections::BTreeMap;

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::PaneTag;
    use crate::PaneId;

    pub fn serialize<S: Serializer>(map: &BTreeMap<PaneId, PaneTag>, s: S) -> Result<S::Ok, S::Error> {
        let named: BTreeMap<String, PaneTag> = map.iter().map(|(k, v)| (k.to_string(), *v)).collect();
        named.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<BTreeMap<PaneId, PaneTag>, D::Error> {
        let named = BTreeMap::<String, PaneTag>::deserialize(d)?;
        named
            .into_iter()
            .map(|(k, v)| {
                k.parse::<PaneId>()
                    .map(|k| (k, v))
                    .map_err(|_| serde::de::Error::custom(format!("bad pane id `{k}`")))
            })
            .collect()
    }
}

impl Window {
    /// A pane's kind (shells when unrecorded).
    pub fn kind(&self, pane: PaneId) -> PaneTag {
        self.kinds.get(&pane).copied().unwrap_or(PaneTag::Shell)
    }

    pub fn panes(&self) -> Vec<PaneId> {
        self.sessions
            .iter()
            .flat_map(|s| s.tabs.iter().flat_map(|t| t.root.panes()))
            .collect()
    }
}

/// Every window key's sessions.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Layout {
    pub windows: BTreeMap<String, Window>,
    next_id: u64,
}

/// A layout edit named something that doesn't exist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotFound(pub &'static str, pub u64);

impl std::fmt::Display for NotFound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "no {} {}", self.0, self.1)
    }
}

impl std::error::Error for NotFound {}

impl Layout {
    fn alloc(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    pub fn window(&self, key: &str) -> Option<&Window> {
        self.windows.get(key)
    }

    /// Adds a session holding one tab with `pane`, creating the window key if needed.
    pub fn new_session(&mut self, key: &str, name: Option<&str>, pane: PaneId) -> SessionId {
        let id = self.alloc();
        let tab = self.alloc();
        let window = self.windows.entry(key.to_owned()).or_default();
        let named = name.is_some();
        let name = name.map_or_else(
            || {
                let taken: Vec<&str> = window.sessions.iter().map(|s| s.name.as_str()).collect();
                generated_name(&taken)
            },
            str::to_owned,
        );
        window.sessions.push(Session {
            id,
            name,
            locked: false,
            named,
            tabs: vec![Tab {
                id: tab,
                title: None,
                root: Node::Pane { pane },
                zoom: None,
            }],
        });
        id
    }

    /// Adds a tab holding `pane` to a session.
    pub fn new_tab(&mut self, session: SessionId, pane: PaneId) -> Result<TabId, NotFound> {
        let id = self.alloc();
        let s = self
            .session_mut(session)
            .ok_or(NotFound("session", session))?;
        s.tabs.push(Tab {
            id,
            title: None,
            root: Node::Pane { pane },
            zoom: None,
        });
        Ok(id)
    }

    /// Splits the tab cell holding `pane`, putting `new` right of or below it, half and half.
    pub fn split(&mut self, pane: PaneId, dir: SplitDir, new: PaneId) -> Result<(), NotFound> {
        self.split_side(pane, dir.into(), new, None)
    }

    /// Splits the tab cell holding `pane`, putting `new` on its `side`. `share` is `pane`'s share
    /// in thousandths (kept within [`MIN_SHARE`] of either end; half by default).
    pub fn split_side(
        &mut self,
        pane: PaneId,
        side: Side,
        new: PaneId,
        share: Option<u16>,
    ) -> Result<(), NotFound> {
        let tab = self.tab_of_mut(pane).ok_or(NotFound("pane", pane))?;
        tab.zoom = None;
        let share = share.map_or(500, |s| clamp_share(i32::from(s)));
        tab.root.split(pane, side, new, share);
        Ok(())
    }

    /// Moves `pane` to `side` of `target`, or swaps the two with `None` (§9.8 `layout:move`).
    /// Both must be in one window key; a tab the move empties goes, and then its session. Returns
    /// whether anything moved.
    pub fn move_pane(&mut self, pane: PaneId, target: PaneId, side: Option<Side>) -> bool {
        if pane == target {
            return false;
        }
        match (self.window_of(pane), self.window_of(target)) {
            (Some(a), Some(b)) if a == b => {}
            _ => return false,
        }
        let Some(side) = side else {
            for tab in self.tabs_mut() {
                if tab.root.contains(pane) || tab.root.contains(target) {
                    tab.root.swap(pane, target);
                    if tab.zoom == Some(pane) || tab.zoom == Some(target) {
                        tab.zoom = None;
                    }
                }
            }
            return true;
        };
        // Out of its cell, keeping its kind, then beside the target.
        let kind = self.window(self.window_of(pane).unwrap_or_default()).map(|w| w.kind(pane));
        self.remove_pane(pane);
        if self.split_side(target, side, pane, None).is_err() {
            return false;
        }
        if let Some(kind) = kind {
            self.set_kind(pane, kind);
        }
        true
    }

    /// Moves `pane`'s nearest divider on `side` by `delta` thousandths of its split, toward `side`
    /// for a positive delta (§9.8 `layout:resize`). Returns whether the divider moved: `false`
    /// when no split runs that way or it is already at its limit.
    pub fn resize(&mut self, pane: PaneId, side: Side, delta: i32) -> Result<bool, NotFound> {
        let tab = self.tab_of_mut(pane).ok_or(NotFound("pane", pane))?;
        let Some(path) = tab.root.divider(pane, side) else {
            return Ok(false);
        };
        let Some(Node::Split { ratio, .. }) = tab.root.at_mut(&path) else {
            return Ok(false);
        };
        let delta = if side.before() { -delta } else { delta };
        let new = clamp_share(i32::from(*ratio) + delta);
        if new == *ratio {
            return Ok(false);
        }
        *ratio = new;
        Ok(true)
    }

    /// Removes a pane, collapsing its split; an emptied tab, and then session, goes too.
    /// Returns whether the pane was in the layout.
    pub fn remove_pane(&mut self, pane: PaneId) -> bool {
        let mut found = false;
        for window in self.windows.values_mut() {
            for session in &mut window.sessions {
                let tabs = std::mem::take(&mut session.tabs);
                for mut tab in tabs {
                    if tab.root.contains(pane) {
                        found = true;
                        if tab.zoom == Some(pane) {
                            tab.zoom = None;
                        }
                        match tab.root.remove(pane) {
                            Some(root) => session.tabs.push(Tab { root, ..tab }),
                            None => continue,
                        }
                    } else {
                        session.tabs.push(tab);
                    }
                }
            }
            window.sessions.retain(|s| !s.tabs.is_empty());
            window.kinds.remove(&pane);
        }
        found
    }

    /// Records the kind of a pane already in the layout. Returns whether it was found.
    pub fn set_kind(&mut self, pane: PaneId, kind: PaneTag) -> bool {
        let Some(window) = self.windows.values_mut().find(|w| {
            w.sessions
                .iter()
                .any(|s| s.tabs.iter().any(|t| t.root.contains(pane)))
        }) else {
            return false;
        };
        if kind == PaneTag::Shell {
            window.kinds.remove(&pane);
        } else {
            window.kinds.insert(pane, kind);
        }
        true
    }

    /// Locks or unlocks a session.
    pub fn lock_session(&mut self, session: SessionId, locked: bool) -> Result<(), NotFound> {
        self.session_mut(session)
            .ok_or(NotFound("session", session))?
            .locked = locked;
        Ok(())
    }

    /// A session by id.
    pub fn session(&self, id: SessionId) -> Option<&Session> {
        self.sessions().find(|s| s.id == id)
    }

    /// The locked session holding `pane`, if its session is locked.
    pub fn locked_session_of(&self, pane: PaneId) -> Option<&Session> {
        self.sessions()
            .find(|s| s.locked && s.tabs.iter().any(|t| t.root.contains(pane)))
    }

    /// Zooms a pane in its tab, or unzooms the tab with `None`.
    pub fn zoom(&mut self, tab: TabId, pane: Option<PaneId>) -> Result<(), NotFound> {
        let t = self.tab_mut(tab).ok_or(NotFound("tab", tab))?;
        if let Some(p) = pane
            && !t.root.contains(p)
        {
            return Err(NotFound("pane", p));
        }
        t.zoom = pane;
        Ok(())
    }

    pub fn rename_session(&mut self, session: SessionId, name: &str) -> Result<(), NotFound> {
        let s = self
            .session_mut(session)
            .ok_or(NotFound("session", session))?;
        s.name = name.to_owned();
        s.named = true;
        Ok(())
    }

    pub fn rename_tab(&mut self, tab: TabId, title: Option<&str>) -> Result<(), NotFound> {
        self.tab_mut(tab).ok_or(NotFound("tab", tab))?.title = title.map(str::to_owned);
        Ok(())
    }

    /// The window key showing `pane`.
    pub fn window_of(&self, pane: PaneId) -> Option<&str> {
        self.windows
            .iter()
            .find(|(_, w)| {
                w.sessions
                    .iter()
                    .any(|s| s.tabs.iter().any(|t| t.root.contains(pane)))
            })
            .map(|(k, _)| k.as_str())
    }

    /// The session holding `pane`.
    pub fn session_of(&self, pane: PaneId) -> Option<SessionId> {
        self.sessions()
            .find(|s| s.tabs.iter().any(|t| t.root.contains(pane)))
            .map(|s| s.id)
    }

    fn sessions(&self) -> impl Iterator<Item = &Session> {
        self.windows.values().flat_map(|w| w.sessions.iter())
    }

    fn session_mut(&mut self, id: SessionId) -> Option<&mut Session> {
        self.windows
            .values_mut()
            .flat_map(|w| w.sessions.iter_mut())
            .find(|s| s.id == id)
    }

    fn tabs_mut(&mut self) -> impl Iterator<Item = &mut Tab> {
        self.windows
            .values_mut()
            .flat_map(|w| w.sessions.iter_mut())
            .flat_map(|s| s.tabs.iter_mut())
    }

    fn tab_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        self.tabs_mut().find(|t| t.id == id)
    }

    fn tab_of_mut(&mut self, pane: PaneId) -> Option<&mut Tab> {
        self.tabs_mut().find(|t| t.root.contains(pane))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_tabs_and_splits() {
        let mut l = Layout::default();
        let s = l.new_session("main", None, 1);
        let t2 = l.new_tab(s, 2).unwrap();
        l.split(2, SplitDir::Right, 3).unwrap();
        l.split(3, SplitDir::Down, 4).unwrap();
        let w = l.window("main").unwrap();
        assert_eq!(w.panes(), vec![1, 2, 3, 4]);
        assert!(!w.sessions[0].named);
        assert_eq!(l.window_of(4), Some("main"));
        assert_eq!(l.session_of(3), Some(s));

        l.zoom(t2, Some(3)).unwrap();
        assert_eq!(l.zoom(t2, Some(1)), Err(NotFound("pane", 1)));
        assert!(l.remove_pane(3));
        let tab = &l.window("main").unwrap().sessions[0].tabs[1];
        assert_eq!(tab.zoom, None);
        assert_eq!(
            tab.root,
            Node::Split {
                dir: SplitDir::Right,
                ratio: 500,
                first: Box::new(Node::Pane { pane: 2 }),
                second: Box::new(Node::Pane { pane: 4 }),
            }
        );
        assert!(l.remove_pane(2));
        assert_eq!(
            l.window("main").unwrap().sessions[0].tabs[1].root,
            Node::Pane { pane: 4 }
        );
    }

    /// New sessions get two random words (book:16894), unique in their window key; a name the
    /// user gives (or renames to) is kept and marks the session named.
    #[test]
    fn default_session_names_are_two_unique_words_and_given_names_are_kept() {
        let mut l = Layout::default();
        for pane in 0..200 {
            l.new_session("w", None, pane);
        }
        let w = l.window("w").unwrap();
        let names: std::collections::BTreeSet<&str> =
            w.sessions.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names.len(), 200, "unique within the window key");
        for s in &w.sessions[..64] {
            let words: Vec<&str> = s.name.split('-').collect();
            assert_eq!(words.len(), 2, "{}", s.name);
            assert!(NAME_ADJECTIVES.contains(&words[0]) && NAME_ANIMALS.contains(&words[1]));
            assert!(!s.named);
        }
        let s = l.new_session("w", Some("work"), 900);
        assert_eq!(l.session(s).map(|s| (s.name.as_str(), s.named)), Some(("work", true)));
        let other = l.window("w").unwrap().sessions[0].id;
        l.rename_session(other, "ops").unwrap();
        assert!(l.session(other).unwrap().named);
        // A layout saved before sessions had `named` reads back unnamed, its names as they were.
        let old = r#"{"id":4,"name":"1","locked":false,"tabs":[]}"#;
        let s: Session = serde_json::from_str(old).unwrap();
        assert_eq!((s.name.as_str(), s.named), ("1", false));
        assert!(!serde_json::to_string(&s).unwrap().contains("named"));
    }

    #[test]
    fn emptied_tabs_and_sessions_go() {
        let mut l = Layout::default();
        let s = l.new_session("w", Some("work"), 1);
        l.new_tab(s, 2).unwrap();
        l.new_session("w", None, 3);
        assert!(l.remove_pane(1));
        assert_eq!(l.window("w").unwrap().sessions[0].tabs.len(), 1);
        assert!(l.remove_pane(2));
        assert_eq!(l.window("w").unwrap().sessions.len(), 1);
        assert!(!l.remove_pane(2));
        assert_eq!(l.window_of(3), Some("w"));
    }

    #[test]
    fn sessions_lock_and_panes_carry_their_kind() {
        let mut l = Layout::default();
        let s = l.new_session("w", None, 1);
        l.split(1, SplitDir::Right, 2).unwrap();
        assert!(!l.session(s).unwrap().locked, "new sessions start unlocked");
        assert!(l.locked_session_of(1).is_none());
        l.lock_session(s, true).unwrap();
        assert_eq!(l.locked_session_of(2).map(|s| s.id), Some(s));
        assert_eq!(l.lock_session(99, true), Err(NotFound("session", 99)));
        l.lock_session(s, false).unwrap();
        assert!(l.locked_session_of(2).is_none());

        assert!(l.set_kind(2, PaneTag::Agent));
        assert!(!l.set_kind(7, PaneTag::Agent));
        let w = l.window("w").unwrap();
        assert_eq!((w.kind(1), w.kind(2)), (PaneTag::Shell, PaneTag::Agent));
        assert_eq!(PaneTag::Agent.icon(), "agent");
        let json = serde_json::to_value(w).unwrap();
        assert_eq!(json["kinds"], serde_json::json!({"2": "agent"}));
        l.remove_pane(2);
        assert!(l.window("w").unwrap().kinds.is_empty());
        // Older layouts without kinds still read.
        let old: Window = serde_json::from_str(r#"{"sessions":[]}"#).unwrap();
        assert!(old.kinds.is_empty());
    }

    fn split(dir: SplitDir, ratio: u16, a: Node, b: Node) -> Node {
        Node::Split {
            dir,
            ratio,
            first: Box::new(a),
            second: Box::new(b),
        }
    }

    fn pane(pane: PaneId) -> Node {
        Node::Pane { pane }
    }

    fn root(l: &Layout, tab: usize) -> Node {
        l.window("w").unwrap().sessions[0].tabs[tab].root.clone()
    }

    #[test]
    fn splits_left_and_up_come_first_and_keep_the_targets_share() {
        let mut l = Layout::default();
        l.new_session("w", None, 1);
        l.split_side(1, Side::Left, 2, Some(700)).unwrap();
        assert_eq!(root(&l, 0), split(SplitDir::Right, 300, pane(2), pane(1)));
        l.split_side(2, Side::Up, 3, None).unwrap();
        l.split_side(1, Side::Down, 4, Some(990)).unwrap();
        assert_eq!(
            root(&l, 0),
            split(
                SplitDir::Right,
                300,
                split(SplitDir::Down, 500, pane(3), pane(2)),
                split(SplitDir::Down, 1000 - MIN_SHARE, pane(1), pane(4)),
            )
        );
        assert_eq!(l.split_side(9, Side::Up, 5, None), Err(NotFound("pane", 9)));
    }

    #[test]
    fn dividers_resize_toward_a_side() {
        let mut l = Layout::default();
        l.new_session("w", None, 1);
        l.split(1, SplitDir::Right, 2).unwrap();
        l.split(2, SplitDir::Down, 3).unwrap();
        let r = root(&l, 0);
        assert_eq!(r.divider(1, Side::Right), Some(vec![]));
        assert_eq!(r.divider(1, Side::Left), None);
        assert_eq!(r.divider(3, Side::Left), Some(vec![]));
        assert_eq!(r.divider(3, Side::Up), Some(vec![true]));
        assert_eq!(r.divider(2, Side::Up), None);
        // Pane 1 grows right; pane 3 grows up, which moves the inner divider up.
        assert_eq!(l.resize(1, Side::Right, 100), Ok(true));
        assert_eq!(l.resize(3, Side::Up, 200), Ok(true));
        assert_eq!(
            root(&l, 0),
            split(
                SplitDir::Right,
                600,
                pane(1),
                split(SplitDir::Down, 300, pane(2), pane(3)),
            )
        );
        // A negative count moves it the other way; nothing moves past the limit or without a split.
        assert_eq!(l.resize(3, Side::Left, -50), Ok(true));
        assert_eq!(root(&l, 0).at(&[]).map(|n| matches!(n, Node::Split { ratio: 650, .. })), Some(true));
        assert_eq!(l.resize(1, Side::Right, 5000), Ok(true));
        assert_eq!(l.resize(1, Side::Right, 1), Ok(false));
        assert_eq!(l.resize(2, Side::Up, 10), Ok(false));
        assert_eq!(l.resize(8, Side::Up, 10), Err(NotFound("pane", 8)));
    }

    #[test]
    fn panes_move_beside_others_or_swap_within_a_window_key() {
        let mut l = Layout::default();
        let s = l.new_session("w", None, 1);
        l.split(1, SplitDir::Right, 2).unwrap();
        l.new_tab(s, 3).unwrap();
        l.set_kind(3, PaneTag::Agent);
        l.new_session("other", None, 9);
        // Swap within a tab.
        assert!(l.move_pane(1, 2, None));
        assert_eq!(root(&l, 0), split(SplitDir::Right, 500, pane(2), pane(1)));
        // Into another tab, left of its pane: the emptied tab goes, the kind stays.
        assert!(l.move_pane(3, 1, Some(Side::Left)));
        let w = l.window("w").unwrap();
        assert_eq!(w.sessions[0].tabs.len(), 1);
        assert_eq!(w.kind(3), PaneTag::Agent);
        assert_eq!(
            root(&l, 0),
            split(SplitDir::Right, 500, pane(2), split(SplitDir::Right, 500, pane(3), pane(1)))
        );
        // Below another pane in the same tab.
        assert!(l.move_pane(2, 1, Some(Side::Down)));
        assert_eq!(
            root(&l, 0),
            split(SplitDir::Right, 500, pane(3), split(SplitDir::Down, 500, pane(1), pane(2)))
        );
        // Swaps across tabs.
        l.new_tab(s, 4).unwrap();
        assert!(l.move_pane(4, 3, None));
        assert_eq!(root(&l, 1), pane(3));
        // Not onto itself, a missing pane or another window key's.
        assert!(!l.move_pane(1, 1, None));
        assert!(!l.move_pane(1, 77, Some(Side::Right)));
        assert!(!l.move_pane(1, 9, Some(Side::Right)));
        assert_eq!(l.window("other").unwrap().panes(), vec![9]);
    }

    #[test]
    fn layout_round_trips_as_json() {
        let mut l = Layout::default();
        let s = l.new_session("main", None, 1);
        l.split(1, SplitDir::Down, 2).unwrap();
        l.rename_session(s, "dev").unwrap();
        let json = serde_json::to_string(&l).unwrap();
        let back: Layout = serde_json::from_str(&json).unwrap();
        assert_eq!(back, l);
    }
}
