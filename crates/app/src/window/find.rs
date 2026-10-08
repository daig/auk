//! Find (§6.2, §8.3) and prompt navigation (§5.3), both view state of the window.
//!
//! **Find** searches the focused pane's whole buffer (scrollback included, soft-wrapped rows
//! joined) and the text of its surfaces' documents. It reads the replica's documents, not the
//! drawn elements, so virtualized content that isn't laid out is found too ("Copy and Find still
//! see everything"). Matches are ordered by their place in the pane's flow: a surface block sits
//! at its anchor row, a screen surface over everything. The current grid match becomes the
//! selection; the other grid matches on screen are tinted in the drawn rows, and matching surface
//! nodes carry `data-auk-find` (`current` or `match`), which the window's find sheet highlights.
//! Stepping to a match scrolls the pane's flow, and any scroll box inside a surface, to show it.
//!
//! **Prompt navigation** jumps between the rows where prompts started (OSC 133 `A`), which the
//! terminal keeps on its rows (replicated, so every window and a late one see the same marks).

use auk_mirror::PaneId;
use auk_render::{GridSnapshot, Rgba};
use auk_term::{Flags, Line, Terminal};
use auk_tsp::{Mode, NodeRef, SurfaceKey, TspPane};
use auk_ui::{Document, NodeId, SheetKind};
use auk_views::compose::Scroll;
use serde_json::{Value, json};

use super::WinState;
use crate::select::{Selection, Unit};

/// The highlight for found surface nodes, installed in a pane's document while Find is open.
const FIND_CSS: &str = "\
[data-auk-find] { background-color: rgba(229, 185, 60, 0.28); border-radius: 3px; }\n\
[data-auk-find=\"current\"] { background-color: rgba(240, 150, 40, 0.55); \
outline: 1px solid rgb(240, 150, 40); }";
const FIND_SHEET: &str = "auk-find";

/// The tint of grid matches that aren't the current one.
const GRID_MATCH: Rgba = Rgba(150, 120, 30, 255);

/// One match.
#[derive(Clone, Debug, PartialEq)]
pub enum Match {
    /// Grid text from `start` to `end`, both inclusive, as (buffer line, column).
    Grid {
        start: (Line, u16),
        end: (Line, u16),
    },
    /// Text in a surface node: the `nth` occurrence in it.
    Surface {
        key: SurfaceKey,
        sf: String,
        node: String,
        /// The anchor row's line; `None` for a screen surface (over the whole flow).
        line: Option<Line>,
        nth: usize,
    },
}

impl Match {
    /// Where the match sits in the pane's flow, for ordering.
    fn order(&self, doc_order: usize) -> (Line, u8, usize, u16) {
        match self {
            Match::Grid { start, .. } => (start.0, 0, 0, start.1),
            Match::Surface { line, .. } => (line.unwrap_or(Line::MAX), 1, doc_order, 0),
        }
    }

    pub fn json(&self) -> Value {
        match self {
            Match::Grid { start, end } => {
                json!({"kind": "grid", "line": start.0, "col": start.1, "end": [end.0, end.1]})
            }
            Match::Surface {
                sf,
                node,
                line,
                nth,
                ..
            } => json!({"kind": "surface", "sf": sf, "id": node, "line": line, "nth": nth}),
        }
    }
}

/// The find bar's state.
#[derive(Debug, Default)]
pub struct Find {
    /// The pane searched: the focused one when Find opened.
    pub pane: PaneId,
    pub query: String,
    pub matches: Vec<Match>,
    pub current: Option<usize>,
    /// The bar has the keyboard.
    pub open: bool,
    /// The replica generation last searched.
    searched: u64,
    /// The surface elements marked, to unmark.
    marked: Vec<NodeId>,
    /// Frames left to check that the current match is in view (estimated heights of
    /// virtualized content settle once it is laid out).
    reveal: u8,
}

impl Find {
    pub fn json(&self) -> Value {
        json!({
            "open": self.open,
            "pane": self.pane,
            "query": self.query,
            "count": self.matches.len(),
            "current": self.current,
            "match": self.current.and_then(|i| self.matches.get(i)).map(Match::json),
            "matches": self.matches.iter().take(200).map(Match::json).collect::<Vec<_>>(),
            "marked": self.marked.len(),
        })
    }
}

/// Characters folded for comparison: smart case (an uppercase letter in the query matches case).
fn fold(c: char, exact: bool) -> char {
    if exact {
        c
    } else {
        c.to_lowercase().next().unwrap_or(c)
    }
}

/// Start indices of every occurrence of `needle` in `hay` (non-overlapping).
fn occurrences(hay: &[char], needle: &[char]) -> Vec<usize> {
    let mut out = Vec::new();
    if needle.is_empty() || hay.len() < needle.len() {
        return out;
    }
    let mut i = 0;
    while i + needle.len() <= hay.len() {
        if hay[i..i + needle.len()] == *needle {
            out.push(i);
            i += needle.len();
        } else {
            i += 1;
        }
    }
    out
}

/// Grid matches: each logical line (soft-wrapped rows joined) of the active screen's buffer.
pub fn search_grid(term: &Terminal, query: &str) -> Vec<Match> {
    let exact = query.chars().any(char::is_uppercase);
    let needle: Vec<char> = query.chars().map(|c| fold(c, exact)).collect();
    let lines = term.history_len() + usize::from(term.rows());
    let mut out = Vec::new();
    let mut line = 0;
    while line < lines {
        // (folded char, its line, column, cells wide)
        let mut chars: Vec<char> = Vec::new();
        let mut map: Vec<(Line, u16, u16)> = Vec::new();
        let mut l = line;
        loop {
            let Some(row) = term.line(l) else { break };
            for (c, cell) in row.cells.iter().enumerate() {
                if cell.flags.has(Flags::SPACER) || cell.flags.has(Flags::LEADING_SPACER) {
                    continue;
                }
                let wide = if cell.flags.has(Flags::WIDE) { 2 } else { 1 };
                let text = if cell.text.is_empty() {
                    " "
                } else {
                    cell.text.as_str()
                };
                // Combining marks fold into their base character's cell.
                if let Some(base) = text.chars().next() {
                    chars.push(fold(base, exact));
                    map.push((l, c as u16, wide));
                }
            }
            if row.wrapped && l + 1 < lines {
                l += 1;
            } else {
                break;
            }
        }
        for at in occurrences(&chars, &needle) {
            let (sl, sc, _) = map[at];
            let (el, ec, w) = map[at + needle.len() - 1];
            out.push(Match::Grid {
                start: (sl, sc),
                end: (el, ec + w - 1),
            });
        }
        line = l + 1;
    }
    out
}

/// Props that hold no text a reader sees.
const NOT_TEXT: &[&str] = &[
    "actions",
    "style",
    "class",
    "css",
    "s",
    "blob",
    "href",
    "path",
    "icon",
    "tone",
    "role",
    "mode",
    "id",
    "key",
    "lang",
    "variant",
    "status",
    "state",
    "kind",
    "fx",
    "cursor",
    "targetKind",
    "name",
    "value",
    "token",
    "size",
    "theme",
    "builtin",
    "selected",
    "sendable",
    "readonly",
    "hidden",
    "collapsed",
    "collapsible",
    "preview",
    "w",
    "h",
    "max",
    "min",
    "align",
    "width",
    "cols",
];

/// The text a node shows, from its props (escapes stripped from `ansi`).
fn node_text(node: NodeRef) -> String {
    fn walk(v: &Value, out: &mut String) {
        match v {
            Value::String(s) => {
                out.push_str(s);
                out.push('\n');
            }
            Value::Number(n) => {
                out.push_str(&n.to_string());
                out.push('\n');
            }
            Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            Value::Object(o) => {
                // A span: its text only.
                if let Some(t) = o.get("t") {
                    walk(t, out);
                    return;
                }
                for (k, x) in o {
                    if !NOT_TEXT.contains(&k.as_str()) {
                        walk(x, out);
                    }
                }
            }
            _ => {}
        }
    }
    let mut out = String::new();
    for (k, v) in node.props() {
        if NOT_TEXT.contains(&k.as_str()) {
            continue;
        }
        if k == "text" && node.kind() == "ansi" {
            out.push_str(&strip_escapes(v.as_str().unwrap_or_default()));
            out.push('\n');
            continue;
        }
        walk(v, &mut out);
    }
    out
}

/// Text without escape sequences (CSI, OSC and two-byte escapes) or other control characters but
/// newlines and tabs.
pub fn strip_escapes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\x1b' => match it.next() {
                Some('[') => {
                    for d in it.by_ref() {
                        if ('\x40'..='\x7e').contains(&d) {
                            break;
                        }
                    }
                }
                Some(']') | Some('P') | Some('_') => {
                    while let Some(d) = it.next() {
                        if d == '\x07' {
                            break;
                        }
                        if d == '\x1b' {
                            it.next();
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\n' | '\t' => out.push(c),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Surface matches: every node of the pane's shown surfaces, in document order.
pub fn search_surfaces(term: &Terminal, tsp: &TspPane, query: &str) -> Vec<(Match, usize)> {
    let exact = query.chars().any(char::is_uppercase);
    let needle: Vec<char> = query.chars().map(|c| fold(c, exact)).collect();
    let main_screen = !term.modes().alt_screen;
    let mut out = Vec::new();
    let mut order = 0;
    for sf in tsp.surfaces() {
        let line = match sf.mode() {
            Mode::Screen if sf.is_live() => None,
            Mode::Screen => continue,
            _ if !main_screen => continue,
            _ => match sf.anchor().and_then(|a| term.anchor_line(a)) {
                Some(l) => Some(l),
                None => continue,
            },
        };
        let mut stack = vec![sf.document().root()];
        while let Some(node) = stack.pop() {
            order += 1;
            if node.kind() != "block" {
                let hay: Vec<char> = node_text(node).chars().map(|c| fold(c, exact)).collect();
                for (nth, _) in occurrences(&hay, &needle).into_iter().enumerate() {
                    out.push((
                        Match::Surface {
                            key: sf.key(),
                            sf: sf.id().to_owned(),
                            node: node.id().to_owned(),
                            line,
                            nth,
                        },
                        order,
                    ));
                }
            }
            let mut kids: Vec<NodeRef> = node.children().collect();
            kids.reverse();
            stack.extend(kids);
        }
    }
    out
}

/// Every match in the pane, in flow order.
pub fn search(term: &Terminal, tsp: &TspPane, query: &str) -> Vec<Match> {
    if query.is_empty() {
        return Vec::new();
    }
    let mut all: Vec<(Match, usize)> = search_grid(term, query)
        .into_iter()
        .map(|m| (m, 0))
        .collect();
    all.extend(search_surfaces(term, tsp, query));
    all.sort_by_key(|(m, o)| m.order(*o));
    all.into_iter().map(|(m, _)| m).collect()
}

/// Scrolls every scroll box between `el` and the pane container so `el` shows in it.
fn reveal_in_scrollers(doc: &mut Document, el: NodeId) {
    let Some(target) = doc.border_box(el) else {
        return;
    };
    let mut cur = doc.parent(el);
    while let Some(p) = cur {
        if doc
            .computed_style(p)
            .is_some_and(|s| s.is_scroll_container())
            && let Some(r) = doc.border_box(p)
        {
            let (x, y) = doc.scroll_offset(p);
            if target.y < r.y {
                doc.set_scroll_offset(p, x, y - (r.y - target.y));
            } else if target.y + target.h > r.y + r.h {
                let by = (target.y + target.h - r.y - r.h).min(target.y - r.y);
                doc.set_scroll_offset(p, x, y + by);
            }
        }
        cur = doc.parent(p);
    }
}

impl WinState {
    /// Opens the find bar on the focused pane (Edit > Find…).
    pub(super) fn find_open(&mut self) {
        let Some(pane) = self.focused_pane() else {
            return;
        };
        let query = match &self.find {
            Some(f) if f.pane == pane => f.query.clone(),
            _ => String::new(),
        };
        self.find_close();
        self.overlays.palette = None;
        self.overlays.menu = None;
        self.find = Some(Find {
            pane,
            query: query.clone(),
            open: true,
            ..Find::default()
        });
        self.find_set_query(&query);
    }

    /// Closes the find bar and drops its highlights; the selection stays.
    pub(super) fn find_close(&mut self) {
        if let Some(f) = self.find.take()
            && let Some(sf) = self.surfaces.get_mut(&f.pane)
        {
            let (doc, _) = sf.parts();
            for el in f.marked {
                if doc.try_node(el).is_some() {
                    doc.remove_attribute(el, "data-auk-find");
                }
            }
            doc.remove_stylesheet(FIND_SHEET, None);
        }
        self.ui_dirty = true;
    }

    /// Searches for `query` and shows the match nearest the tail.
    pub(super) fn find_set_query(&mut self, query: &str) {
        let Some(f) = self.find.as_mut() else { return };
        f.query = query.to_owned();
        let pane = f.pane;
        self.find_research(pane);
        if let Some(f) = self.find.as_mut() {
            f.current = f.matches.len().checked_sub(1);
            f.reveal = 4;
        }
        self.find_show(true);
    }

    /// Steps to the next match (later in the flow) or the previous one, wrapping. Opens Find on
    /// the focused pane's last query if it isn't up.
    pub(super) fn find_step(&mut self, forward: bool) {
        if self.find.is_none() {
            self.find_open();
            if let Some(f) = self.find.as_mut() {
                f.open = false;
            }
        }
        let Some(f) = self.find.as_mut() else { return };
        let n = f.matches.len();
        if n == 0 {
            return;
        }
        f.current = Some(match f.current {
            Some(i) if forward => (i + 1) % n,
            Some(i) => (i + n - 1) % n,
            None if forward => 0,
            None => n - 1,
        });
        f.reveal = 4;
        self.find_show(true);
    }

    /// Runs the search again over the pane's current content, keeping the current match when it
    /// is still there.
    fn find_research(&mut self, pane: PaneId) {
        let found = {
            let shared = self.conn.shared();
            let Some(data) = shared.panes.get(&pane) else {
                return;
            };
            let query = self
                .find
                .as_ref()
                .map(|f| f.query.clone())
                .unwrap_or_default();
            (
                search(data.replica.terminal(), data.replica.tsp(), &query),
                data.generation,
            )
        };
        let Some(f) = self.find.as_mut() else { return };
        let keep = f.current.and_then(|i| f.matches.get(i)).cloned();
        f.matches = found.0;
        f.searched = found.1;
        f.current = match keep {
            Some(m) => f
                .matches
                .iter()
                .position(|x| *x == m)
                .or_else(|| f.current.map(|i| i.min(f.matches.len().saturating_sub(1))))
                .filter(|_| !f.matches.is_empty()),
            None => f.current.filter(|i| *i < f.matches.len()),
        };
    }

    /// Before each frame: a search over content that changed runs again, and the surface marks
    /// are put back on elements the views rebuilt.
    pub(super) fn find_before_frame(&mut self) {
        let Some(f) = &self.find else { return };
        let pane = f.pane;
        let generation = self.conn.shared().panes.get(&pane).map(|d| d.generation);
        if generation.is_some_and(|g| g != f.searched) {
            self.find_research(pane);
            self.find_show(false);
        }
    }

    /// Selects the current match and marks the surface matches; with `reveal`, scrolls to the
    /// current one.
    pub(super) fn find_show(&mut self, reveal: bool) {
        let Some(f) = self.find.as_mut() else { return };
        let pane = f.pane;
        let current = f.current.and_then(|i| f.matches.get(i)).cloned();
        // Surface marks.
        let surface_matches: Vec<(SurfaceKey, String, bool)> = f
            .matches
            .iter()
            .enumerate()
            .filter_map(|(i, m)| match m {
                Match::Surface { key, node, .. } => {
                    Some((*key, node.clone(), Some(i) == f.current))
                }
                Match::Grid { .. } => None,
            })
            .collect();
        let old = std::mem::take(&mut f.marked);
        let mut marked = Vec::new();
        let mut current_el = None;
        if let Some(sf) = self.surfaces.get_mut(&pane) {
            let (doc, pv) = sf.parts();
            for el in old {
                if doc.try_node(el).is_some() {
                    doc.remove_attribute(el, "data-auk-find");
                }
            }
            doc.set_stylesheet(FIND_SHEET, SheetKind::BuiltIn, None, FIND_CSS);
            for (key, node, is_current) in surface_matches {
                let Some(el) = pv.node_element(key, &node) else {
                    continue;
                };
                let value = if is_current { "current" } else { "match" };
                // A node with several matches keeps `current` once it has it.
                if is_current || doc.attribute(el, "data-auk-find") != Some("current") {
                    doc.set_attribute(el, "data-auk-find", value);
                }
                if is_current {
                    current_el = Some(el);
                }
                marked.push(el);
            }
            if reveal && let Some(el) = current_el {
                reveal_in_scrollers(doc, el);
            }
        }
        if let Some(f) = self.find.as_mut() {
            f.marked = marked;
        }
        self.ui_dirty = true;
        let Some(m) = current else { return };
        match m {
            Match::Grid { start, end } => {
                let view = self.views.entry(pane).or_default();
                view.selection = Some(Selection {
                    anchor: start,
                    head: end,
                    unit: Unit::Cell,
                    block: false,
                });
                if reveal {
                    self.reveal_line(pane, start.0, 0.0, None);
                }
            }
            Match::Surface {
                line: Some(line), ..
            } if reveal => {
                // The node's offset into its block, from the drawn document.
                let offset = current_el.and_then(|el| {
                    let sf = self.surfaces.get(&pane)?;
                    let view = self.views.get(&pane)?;
                    let block = view.arranged.blocks.iter().find(|b| b.line == line)?;
                    let top = sf.doc().border_box(block.el)?.y;
                    let r = sf.doc().border_box(el)?;
                    Some(((r.y - top).max(0.0), r.h))
                });
                let scale = self.size.map_or(2.0, |s| s.scale) as f32;
                let (off, h) = offset.unwrap_or((0.0, 0.0));
                self.reveal_line(pane, line, off * scale, Some(h * scale));
            }
            Match::Surface { .. } => {}
        }
    }

    /// Scrolls the pane's flow so that `line` (plus `offset` pixels into it, `height` pixels
    /// tall) is in view, unless it already is.
    pub(super) fn reveal_line(
        &mut self,
        pane: PaneId,
        line: Line,
        offset: f32,
        height: Option<f32>,
    ) {
        let Some(view) = self.views.get_mut(&pane) else {
            return;
        };
        let a = &view.arranged;
        let cell = a.cell_px.max(1.0);
        let h = height.unwrap_or(cell);
        let shown = a.composed.slot_of(line).is_some_and(|slot| {
            let top = slot.top + offset;
            slot.height > 0.0 && top >= 0.0 && top + h.min(a.flow_px) <= a.flow_px
        });
        if shown {
            return;
        }
        // Some context above it: two rows.
        let context = 2.0 * cell;
        view.scroll = if offset >= context {
            Scroll {
                from_bottom: 1.0,
                anchor: Some((line, offset - context)),
            }
        } else {
            Scroll {
                from_bottom: 1.0,
                anchor: Some((line.saturating_sub(2), 0.0)),
            }
        };
        if let Some(w) = &self.window {
            w.request_frame();
        }
    }

    /// A key while the find bar is open: Return steps (⇧ backwards), Escape closes, Delete
    /// edits the query. Other keys without ⌘ stay with the bar. Returns whether it took the key.
    pub(super) fn find_key(&mut self, key: &auk_platform_macos::KeyInput) -> bool {
        use auk_term::input::Key;
        if !self.find.as_ref().is_some_and(|f| f.open) {
            return false;
        }
        let m = key.event.mods;
        if m.meta {
            return false;
        }
        match key.event.key {
            Key::Escape => self.find_close(),
            Key::Enter | Key::Keypad('\r') => self.find_step(!m.shift),
            Key::Backspace => {
                let mut q = self
                    .find
                    .as_ref()
                    .map(|f| f.query.clone())
                    .unwrap_or_default();
                q.pop();
                self.find_set_query(&q);
            }
            _ => {}
        }
        self.ui_dirty = true;
        true
    }

    /// Text typed while the find bar is open goes into the query.
    pub(super) fn find_text(&mut self, text: &str) -> bool {
        let Some(f) = self.find.as_ref().filter(|f| f.open) else {
            return false;
        };
        let typed: String = text.chars().filter(|c| !c.is_control()).collect();
        if !typed.is_empty() {
            let q = format!("{}{typed}", f.query);
            self.find_set_query(&q);
        }
        true
    }

    /// After a frame: shows the current match again while its reveal is settling. Returns
    /// whether another frame should look.
    pub(super) fn find_reveal_again(&mut self) -> bool {
        let Some(f) = self.find.as_mut().filter(|f| f.reveal > 0) else {
            return false;
        };
        f.reveal -= 1;
        let again = f.reveal > 0;
        self.find_show(true);
        again
    }

    /// Prompt navigation (§5.3): scrolls the focused pane so the previous (or next) prompt row
    /// is at the top of the view; past the last prompt, the view follows the tail again.
    pub(super) fn jump_to_prompt(&mut self, forward: bool) {
        let Some(pane) = self.focused_pane() else {
            return;
        };
        let prompts = {
            let shared = self.conn.shared();
            let Some(data) = shared.panes.get(&pane) else {
                return;
            };
            data.replica.terminal().prompt_lines()
        };
        let view = self.views.entry(pane).or_default();
        let composed = &view.arranged.composed;
        // The line at the top of the view; when scrolled into a line, that line counts as passed.
        let top = composed.first;
        let into = composed.scroll.anchor.is_some_and(|(_, off)| off > 0.5);
        let target = if forward {
            prompts.iter().copied().find(|&p| p > top)
        } else {
            prompts
                .iter()
                .copied()
                .rev()
                .find(|&p| p < top || (into && p == top))
        };
        view.scroll = match target {
            Some(line) => Scroll {
                from_bottom: 1.0,
                anchor: Some((line, 0.0)),
            },
            None if forward => Scroll::default(),
            None => return,
        };
        if let Some(w) = &self.window {
            w.request_frame();
        }
    }
}

/// Tints the grid matches on the drawn rows other than the current one (which is the
/// selection). `first` is the first buffer line the snapshot holds.
pub(super) fn tint(find: Option<&Find>, pane: PaneId, first: Line, snap: &mut GridSnapshot) {
    let Some(f) = find else { return };
    if f.pane != pane || snap.rows == 0 || snap.cols == 0 {
        return;
    }
    let last = first + usize::from(snap.rows) - 1;
    for (i, m) in f.matches.iter().enumerate() {
        let Match::Grid { start, end } = m else {
            continue;
        };
        if Some(i) == f.current || end.0 < first || start.0 > last {
            continue;
        }
        for line in start.0.max(first)..=end.0.min(last) {
            let row = (line - first) as u16;
            let c0 = if line == start.0 { start.1 } else { 0 };
            let c1 = if line == end.0 { end.1 } else { snap.cols - 1 };
            for col in c0..=c1.min(snap.cols - 1) {
                snap.cell_mut(col, row).bg = GRID_MATCH;
            }
        }
    }
}
