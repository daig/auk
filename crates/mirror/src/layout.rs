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

    /// Replaces the leaf `pane` with a split of it and `new`. Returns whether it was found.
    fn split(&mut self, pane: PaneId, dir: SplitDir, new: PaneId) -> bool {
        match self {
            Node::Pane { pane: p } if *p == pane => {
                *self = Node::Split {
                    dir,
                    ratio: 500,
                    first: Box::new(Node::Pane { pane }),
                    second: Box::new(Node::Pane { pane: new }),
                };
                true
            }
            Node::Pane { .. } => false,
            Node::Split { first, second, .. } => {
                first.split(pane, dir, new) || second.split(pane, dir, new)
            }
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
    pub tabs: Vec<Tab>,
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
        let name = name.map_or_else(|| (window.sessions.len() + 1).to_string(), str::to_owned);
        window.sessions.push(Session {
            id,
            name,
            locked: false,
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

    /// Splits the tab cell holding `pane`, putting `new` right of or below it.
    pub fn split(&mut self, pane: PaneId, dir: SplitDir, new: PaneId) -> Result<(), NotFound> {
        let tab = self.tab_of_mut(pane).ok_or(NotFound("pane", pane))?;
        tab.zoom = None;
        tab.root.split(pane, dir, new);
        Ok(())
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
        self.session_mut(session)
            .ok_or(NotFound("session", session))?
            .name = name.to_owned();
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
        assert_eq!(w.sessions[0].name, "1");
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
