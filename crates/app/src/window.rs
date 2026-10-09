//! One app window (§4.2, §4.3): a window key's sessions, shown through follower replicas over
//! the window's own daemon connection. Layout is daemon state, replicated to every window; focus,
//! the shown session and tab, scrolling and selection belong to the window.

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use auk_mirror::layout::{Session, SessionId, SplitDir, Tab, TabId, Window as Sessions};
use auk_mirror::{ClientFacts, ClientMsg, LayoutEdit, PaneId, ThemeColors};
use auk_platform_macos::{
    Appearance, Command, Event, KeyInput, MouseButton, MouseEvent, MouseKind, OptionAsAlt, Preedit,
    ScrollEvent, Size, Window, WindowHandler,
};
use auk_render::{Attrs, Gpu, GridSnapshot, RowSlot, Theme, View};
use auk_term::input::{
    self, Key, KeyEvent, Mods, MouseAction, MouseButton as TermButton, encode_focus, encode_key,
    encode_mouse, encode_paste,
};
use auk_term::{Line, Modes, MouseMode};
use auk_views::compose::{Hit, Scroll};
use auk_views::{Arranged, Outcome, PaneSurfaces, Place, ViewSettings};

use crate::chrome::{
    BAR_HEIGHT, BarModel, Chrome, ChromeAction, ChromeModel, DIVIDER, SessionItem, TabItem,
};
use crate::commands::{self, Cmd};
use crate::conn::{Connection, Shared};
use crate::geometry::{self, Rect, TabLayout};
use crate::gfx::{FrameClock, FrameCost, Gfx, PaneDraw};
use crate::select::{Selection, Unit};

mod extras;
mod find;
mod overlays;
mod plugins;
mod pointer;

pub use plugins::osc7_path;

/// Space around a pane's grid, in points.
pub const PADDING: f64 = 6.0;

/// A layout edit this window asked for, whose result it should show when it arrives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pending {
    Session,
    Tab(SessionId),
    Split(PaneId),
}

/// Per-pane view state (§4.4): never replicated.
#[derive(Clone, Debug, Default)]
pub struct PaneView {
    pub scroll: Scroll,
    pub selection: Option<Selection>,
    /// The pane as last drawn (rows, blocks, dock), for mapping points to lines and surfaces.
    pub arranged: Arranged,
    /// The content area's height last drawn, in pixels.
    pub content_px: f32,
    /// The grid's origin last drawn, in points from the window's top-left.
    pub grid_origin: (f64, f64),
    /// Wheel movement not yet sent as whole lines (mouse reports, alternate scroll), in pixels.
    pub wheel: f64,
}

/// A button held over a pane.
#[derive(Clone, Copy, Debug)]
struct Press {
    pane: PaneId,
    /// The press went to the program as a mouse report.
    reported: bool,
}

/// Frame statistics for the automation endpoint.
#[derive(Debug, Default)]
pub struct Stats {
    pub frames: u64,
    /// CPU work per frame: snapshots, chrome and encoding, without the drawable wait.
    pub cpu: Vec<Duration>,
    pub drawable_wait: Vec<Duration>,
    /// CPU time per frame by phase ([`Phase`]), in frame order.
    pub phases: BTreeMap<&'static str, Vec<Duration>>,
    /// The surfaces' layout work per frame (`auk_views::LayoutWork`, summed over panes): measure
    /// passes, measure targets visited, elements restyled, inline contexts rebuilt, boxes laid
    /// out.
    pub work: BTreeMap<&'static str, Vec<f64>>,
}

/// A frame's CPU time split by phase, for `frame-stats`: `blobs` (asking for missing blobs),
/// `sync` (the surfaces taking the replica's changes, settings, plugin sheets, lenses),
/// `layout` (the views' tick, restyle, layout and measure loops), `paint` (arranging the flow
/// and painting surfaces), `grid` (grid snapshots), `chrome` (the chrome's update and the UI
/// layer's render), `draw` (encoding and submitting) and `tail` (acks, accessibility).
#[derive(Debug, Default)]
struct Phase {
    at: Option<Instant>,
    spent: BTreeMap<&'static str, Duration>,
}

impl Phase {
    /// Charges the time since the last lap to `phase`.
    fn lap(&mut self, phase: &'static str) {
        let now = Instant::now();
        if let Some(at) = self.at {
            *self.spent.entry(phase).or_default() += now - at;
        }
        self.at = Some(now);
    }
}

pub struct WinState {
    pub window: Option<Window>,
    pub conn: Arc<Connection>,
    gpu: Rc<Gpu>,
    pub clock: FrameClock,
    pub size: Option<Size>,
    gfx: Option<Gfx>,
    pub chrome: Chrome,
    ui_dirty: bool,
    pub dark: bool,
    theme: Theme,
    pub active_session: Option<SessionId>,
    pub active_tab: BTreeMap<SessionId, TabId>,
    pub tab_focus: BTreeMap<TabId, PaneId>,
    pub views: BTreeMap<PaneId, PaneView>,
    /// Each pane's surfaces (§6.2), kept for every pane the window follows, shown or not.
    pub surfaces: BTreeMap<PaneId, PaneSurfaces>,
    /// The surfaces painted something last frame, so the UI layer has them to clear.
    surfaces_painted: bool,
    /// The views' clock (ms) starts here.
    epoch: Instant,
    layout_generation: u64,
    known_sessions: BTreeSet<SessionId>,
    known_tabs: BTreeSet<TabId>,
    known_panes: BTreeSet<PaneId>,
    pending: Option<Pending>,
    asked_for_session: bool,
    had_panes: bool,
    pub preedit: Option<Preedit>,
    pub key_window: bool,
    /// When the window last became key (larger is later), for commands without a key window.
    pub focus_serial: u64,
    press: Option<Press>,
    /// Where a single-click press went down: a drag from it starts a selection.
    press_anchor: Option<(PaneId, Line, u16, bool)>,
    /// The last cell a motion report named, so motion reports only on cell changes.
    last_motion: Option<(PaneId, u16, u16)>,
    sent_sizes: BTreeMap<PaneId, (u16, u16)>,
    facts_sent: Option<ClientFacts>,
    /// The tab's layout last computed, in points.
    pub tab_layout: TabLayout,
    pub stats: Stats,
    pub option_as_alt: OptionAsAlt,
    /// The window is going away; ignore further work.
    closing: bool,
    /// Surface frames this window drew and acked: (pane, surface id) → `s`.
    acked: BTreeMap<(PaneId, String), u64>,
    /// The focused pane's text and the chrome, as the accessibility tree last saw them.
    a11y_seen: Option<(u64, u64)>,
    pub a11y_generation: u64,
    /// The window half of every plugin (§9.8), and what it shows.
    pub plugins: plugins::PluginState,
    /// The palette, context menu and image viewer.
    pub overlays: overlays::Overlays,
    /// The viewer's picture size, in CSS px.
    pub(crate) viewer_size: (f64, f64),
    /// The viewer's picture is an SVG (it scales up to fit).
    pub(crate) viewer_svg: bool,
    /// Bumped whenever the chrome document is rebuilt, for the accessibility tree.
    chrome_updates: u64,
    /// Find (§6.2): the bar, the matches and the current one.
    pub find: Option<find::Find>,
    /// The target of the link under the pointer, shown while hovering (§7.14, §12.2).
    pub hover_link: Option<String>,
    /// The title the window shows.
    pub title_shown: Option<String>,
    /// The grid link under the pointer (pane, terminal link id), opening or not.
    pub hover_grid: Option<(PaneId, u32)>,
    /// ⌘ is down, as the last pointer or modifier event said.
    pub pointer_cmd: bool,
    /// The pointer shape last set.
    pub pointer_shape: auk_platform_macos::PointerShape,
    /// The `title` tooltip of the surface node under the pointer, and where the pointer is, in
    /// window points (§8.1).
    pub hover_title: Option<(String, f64, f64)>,
    /// A drag selecting in a program field: the pane and the offset it started at (§7.9).
    field_drag: Option<(PaneId, u64)>,
    /// The pane whose `prefs` ordered list item is being dragged by its grip.
    reorder_drag: Option<PaneId>,
    /// Where the input method's window was last told the cursor is, in window points.
    pub ime_area: Option<(f64, f64, f64, f64)>,
    /// The presence item as of the frame's look at the shared state, so the chrome doesn't
    /// lock it again (the reader thread may hold it, applying a batch).
    presence_shown: Option<String>,
    /// The replica generation each pane's command blocks were last drawn from.
    lens_seen: BTreeMap<PaneId, u64>,
    /// The settings file, as last read (§7.8, §7.9, §7.12, §7.13, §9.8).
    pub settings: crate::settings::Settings,
}

impl std::fmt::Debug for WinState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WinState")
            .field("window_key", &self.conn.window_key)
            .field("active_session", &self.active_session)
            .finish_non_exhaustive()
    }
}

/// The window's handler: forwards to the shared state, which the automation endpoint also
/// reaches. AppKit sometimes calls back while the state is busy with an event (closing or
/// resizing a window from inside a handler); those events wait in `queue` and run right after.
#[derive(Debug)]
pub struct Handler {
    pub state: Rc<std::cell::RefCell<WinState>>,
    queue: Rc<std::cell::RefCell<std::collections::VecDeque<Event>>>,
}

impl Handler {
    pub fn new(state: Rc<std::cell::RefCell<WinState>>) -> Handler {
        Handler {
            state,
            queue: Rc::default(),
        }
    }
}

impl WindowHandler for Handler {
    fn event(&mut self, window: &Window, event: Event) {
        let Ok(mut state) = self.state.try_borrow_mut() else {
            self.queue.borrow_mut().push_back(event);
            return;
        };
        if state.window.is_none() {
            state.window = Some(window.clone());
        }
        let mut closed = false;
        let mut next = Some(event);
        while let Some(event) = next {
            closed |= matches!(event, Event::Closed);
            state.event(window, event);
            next = self.queue.borrow_mut().pop_front();
        }
        drop(state);
        if closed {
            crate::app::window_closed(&self.state);
        }
    }
}

impl WinState {
    pub fn new(
        conn: Arc<Connection>,
        gpu: Rc<Gpu>,
        clock: FrameClock,
        option_as_alt: OptionAsAlt,
    ) -> WinState {
        WinState {
            window: None,
            conn,
            gpu,
            clock,
            size: None,
            gfx: None,
            chrome: Chrome::new(2.0),
            ui_dirty: true,
            dark: true,
            theme: Theme::dark(),
            active_session: None,
            active_tab: BTreeMap::new(),
            tab_focus: BTreeMap::new(),
            views: BTreeMap::new(),
            surfaces: BTreeMap::new(),
            surfaces_painted: false,
            epoch: Instant::now(),
            layout_generation: 0,
            known_sessions: BTreeSet::new(),
            known_tabs: BTreeSet::new(),
            known_panes: BTreeSet::new(),
            pending: None,
            asked_for_session: false,
            had_panes: false,
            preedit: None,
            key_window: false,
            focus_serial: 0,
            press: None,
            press_anchor: None,
            last_motion: None,
            sent_sizes: BTreeMap::new(),
            facts_sent: None,
            tab_layout: TabLayout::default(),
            stats: Stats::default(),
            option_as_alt,
            closing: false,
            acked: BTreeMap::new(),
            a11y_seen: None,
            a11y_generation: 0,
            plugins: plugins::PluginState::default(),
            overlays: overlays::Overlays::default(),
            viewer_size: (1.0, 1.0),
            viewer_svg: false,
            chrome_updates: 0,
            find: None,
            hover_link: None,
            title_shown: None,
            hover_grid: None,
            pointer_cmd: false,
            pointer_shape: auk_platform_macos::PointerShape::Arrow,
            hover_title: None,
            field_drag: None,
            reorder_drag: None,
            ime_area: None,
            presence_shown: None,
            lens_seen: BTreeMap::new(),
            settings: crate::settings::Settings::load(),
        }
    }

    fn event(&mut self, window: &Window, event: Event) {
        if self.closing && !matches!(event, Event::Closed) {
            return;
        }
        match event {
            Event::Resized(size) => self.resized(window, size),
            Event::Appearance(appearance) => {
                self.dark = appearance == Appearance::Dark;
                self.theme = if self.dark {
                    Theme::dark()
                } else {
                    Theme::light()
                };
                self.ui_dirty = true;
                self.send_facts();
                window.request_frame();
            }
            Event::SystemSettings => {
                // The 12-hour clock or Reduce Motion may have changed (§4.5, §7.13).
                self.ui_dirty = true;
                self.send_facts();
                window.request_frame();
            }
            Event::Wake => self.wake(window),
            Event::Frame => self.frame(window),
            Event::Key(key) => self.key(window, key),
            Event::Text(text) => {
                if self.palette_text(&text) || self.find_text(&text) {
                    if let Some(w) = &self.window {
                        w.request_frame();
                    }
                    return;
                }
                if let Some(pane) = self.focused_pane()
                    && !self.surface_text(window, pane, &text)
                {
                    self.typed(window, pane, text.into_bytes());
                }
            }
            Event::Preedit(preedit) => {
                // A focused program field draws the composition at its caret (§7.9).
                let in_field = self.focused_pane().is_some_and(|pane| {
                    let (text, cursor) = preedit.as_ref().map_or((String::new(), 0), |p| {
                        (p.text.clone(), p.text[..p.selection.start.min(p.text.len())].chars().count())
                    });
                    self.field_preedit(pane, &text, cursor)
                });
                self.preedit = if in_field { None } else { preedit };
                window.request_frame();
            }
            Event::ModifiersChanged(mods) => {
                // ⌘ arms the link under the pointer (A13).
                if self.track_grid_link(window, None, mods.meta) {
                    window.request_frame();
                }
                self.plugin_modifiers(mods);
            }
            Event::Mouse(m) => self.mouse(window, m),
            Event::Scroll(s) => self.scroll(window, s),
            Event::Focus(focused) => {
                self.key_window = focused;
                // The system's Reduce Motion may have changed while the window was away.
                self.send_facts();
                if focused {
                    self.focus_serial = next_focus_serial();
                }
                if let Some(pane) = self.focused_pane() {
                    let modes = self.modes(pane);
                    self.conn.input(pane, encode_focus(focused, &modes));
                    if focused {
                        let _ = self.conn.send(&ClientMsg::Focus { pane });
                    }
                }
                window.request_frame();
            }
            Event::Command(c) => match c {
                Command::Copy => self.command(window, Cmd::Copy),
                Command::Paste => self.command(window, Cmd::Paste),
                Command::SelectAll => self.command(window, Cmd::SelectAll),
                Command::NewWindow => self.command(window, Cmd::NewWindow),
            },
            Event::Menu(id) => {
                if let Some(cmd) = commands::by_id(id) {
                    self.command(window, cmd);
                }
            }
            Event::CloseRequested => {
                self.closing = true;
                self.conn.detach();
                window.close();
            }
            Event::Closed => {
                self.closing = true;
                self.conn.detach();
                self.gfx = None;
            }
        }
    }

    // Layout and focus.

    /// The sessions as the daemon last sent them.
    fn sessions(&self) -> Sessions {
        self.conn.shared().sessions.clone()
    }

    fn session<'a>(&self, sessions: &'a Sessions) -> Option<&'a Session> {
        let id = self.active_session?;
        sessions.sessions.iter().find(|s| s.id == id)
    }

    fn tab<'a>(&self, sessions: &'a Sessions) -> Option<&'a Tab> {
        let session = self.session(sessions)?;
        let id = self.active_tab.get(&session.id)?;
        session.tabs.iter().find(|t| t.id == *id)
    }

    pub fn active_tab_id(&self) -> Option<TabId> {
        self.active_tab.get(&self.active_session?).copied()
    }

    pub fn focused_pane(&self) -> Option<PaneId> {
        self.tab_focus.get(&self.active_tab_id()?).copied()
    }

    /// Makes the shown session, tab and focus valid for the current layout, and shows what this
    /// window asked to create.
    fn reconcile(&mut self, sessions: &Sessions) {
        let new_sessions: Vec<SessionId> = sessions
            .sessions
            .iter()
            .map(|s| s.id)
            .filter(|id| !self.known_sessions.contains(id))
            .collect();
        let all_tabs: Vec<(SessionId, &Tab)> = sessions
            .sessions
            .iter()
            .flat_map(|s| s.tabs.iter().map(move |t| (s.id, t)))
            .collect();
        let new_tabs: Vec<(SessionId, TabId)> = all_tabs
            .iter()
            .filter(|(_, t)| !self.known_tabs.contains(&t.id))
            .map(|(s, t)| (*s, t.id))
            .collect();
        let new_panes: Vec<(TabId, PaneId)> = all_tabs
            .iter()
            .flat_map(|(_, t)| t.root.panes().into_iter().map(move |p| (t.id, p)))
            .filter(|(_, p)| !self.known_panes.contains(p))
            .collect();

        match self.pending {
            Some(Pending::Session) if !new_sessions.is_empty() => {
                self.active_session = new_sessions.last().copied();
                self.pending = None;
            }
            Some(Pending::Tab(session)) => {
                if let Some((_, tab)) = new_tabs.iter().find(|(s, _)| *s == session) {
                    self.active_session = Some(session);
                    self.active_tab.insert(session, *tab);
                    self.pending = None;
                }
            }
            Some(Pending::Split(from)) => {
                if let Some((tab, pane)) = new_panes.iter().find(|(t, _)| {
                    all_tabs
                        .iter()
                        .any(|(_, x)| x.id == *t && x.root.contains(from))
                }) {
                    self.tab_focus.insert(*tab, *pane);
                    self.pending = None;
                }
            }
            _ => {}
        }

        // The shown session: still there, else the first.
        if !sessions
            .sessions
            .iter()
            .any(|s| Some(s.id) == self.active_session)
        {
            self.active_session = sessions.sessions.first().map(|s| s.id);
        }
        for s in &sessions.sessions {
            let tab_ok = self
                .active_tab
                .get(&s.id)
                .is_some_and(|t| s.tabs.iter().any(|x| x.id == *t));
            if !tab_ok && let Some(first) = s.tabs.first() {
                self.active_tab.insert(s.id, first.id);
            }
            for t in &s.tabs {
                let focus_ok = self
                    .tab_focus
                    .get(&t.id)
                    .is_some_and(|p| t.root.contains(*p));
                if !focus_ok {
                    let panes = t.root.panes();
                    let pick = t.zoom.or_else(|| panes.first().copied());
                    if let Some(p) = pick {
                        self.tab_focus.insert(t.id, p);
                    }
                }
            }
        }
        self.known_sessions = sessions.sessions.iter().map(|s| s.id).collect();
        self.known_tabs = all_tabs.iter().map(|(_, t)| t.id).collect();
        self.known_panes = sessions.panes().into_iter().collect();
        self.active_tab
            .retain(|s, _| self.known_sessions.contains(s));
        let tabs = self.known_tabs.clone();
        self.tab_focus.retain(|t, _| tabs.contains(t));
        let panes = self.known_panes.clone();
        self.views.retain(|p, _| panes.contains(p));
        self.surfaces.retain(|p, _| panes.contains(p));
        self.sent_sizes.retain(|p, _| panes.contains(p));
        self.ui_dirty = true;
    }

    /// News from the daemon: layout changes, output, a closed connection.
    fn wake(&mut self, window: &Window) {
        let (generation, layout_seen, empty, closed) = {
            let shared = self.conn.shared();
            (
                shared.layout_generation,
                shared.layout_seen,
                shared.sessions.sessions.is_empty(),
                shared.closed.clone(),
            )
        };
        // A link that gave up keeps its window open, showing `failed` and why in the status line
        // (§10.1); a detach or a local daemon that went away closes it.
        let failed = matches!(self.conn.link_status().state, auk_remote::State::Failed { .. });
        if let Some(reason) = closed.filter(|_| !failed) {
            eprintln!("auk: window {}: {reason}", self.conn.window_key);
            self.closing = true;
            window.close();
            return;
        }
        if generation != self.layout_generation {
            self.layout_generation = generation;
            let sessions = self.sessions();
            if layout_seen && empty {
                if self.had_panes {
                    // Every pane ended: the window has nothing left to show.
                    self.closing = true;
                    self.conn.detach();
                    window.close();
                    return;
                }
                if !self.asked_for_session {
                    self.asked_for_session = true;
                    self.pending = Some(Pending::Session);
                    self.create(LayoutEdit::NewSession { name: None });
                }
            } else {
                self.had_panes = true;
            }
            self.reconcile(&sessions);
            self.relayout();
        }
        if layout_seen && !empty {
            self.plugins_settled(window);
        }
        self.plugins_wake(window);
        window.request_frame();
    }

    pub fn edit(&self, edit: LayoutEdit) {
        if let Err(err) = self.conn.send(&ClientMsg::LayoutEdit(edit)) {
            eprintln!("auk: layout edit: {err}");
        }
    }

    /// Sends an edit that adds a pane (a session, a tab, a split) as a request, so the pane it
    /// adds is known to be this window's when the answer comes (§9.8 `pane_created`).
    pub fn create(&mut self, edit: LayoutEdit) {
        match self.conn.request_async(edit) {
            Some(id) => self.plugins_creating(id),
            None => eprintln!("auk: layout edit: the link to the daemon is gone"),
        }
    }

    /// Focuses a pane in this window (its session and tab come forward) and makes this window
    /// its primary (§15.5).
    pub fn focus_pane(&mut self, pane: PaneId) -> bool {
        let sessions = self.sessions();
        let Some((session, tab)) = sessions.sessions.iter().find_map(|s| {
            s.tabs
                .iter()
                .find(|t| t.root.contains(pane))
                .map(|t| (s.id, t.id))
        }) else {
            return false;
        };
        let before = self.focused_pane();
        self.active_session = Some(session);
        self.active_tab.insert(session, tab);
        self.tab_focus.insert(tab, pane);
        if before != Some(pane) {
            if let Some(old) = before {
                let modes = self.modes(old);
                self.conn.input(old, encode_focus(false, &modes));
            }
            if self.key_window {
                let modes = self.modes(pane);
                self.conn.input(pane, encode_focus(true, &modes));
            }
        }
        let _ = self.conn.send(&ClientMsg::Focus { pane });
        self.ui_dirty = true;
        self.relayout();
        if let Some(w) = &self.window {
            w.request_frame();
        }
        true
    }

    fn select_session(&mut self, session: SessionId) {
        self.active_session = Some(session);
        self.after_switch();
    }

    fn select_tab(&mut self, tab: TabId) {
        let sessions = self.sessions();
        if let Some(s) = sessions
            .sessions
            .iter()
            .find(|s| s.tabs.iter().any(|t| t.id == tab))
        {
            self.active_session = Some(s.id);
            self.active_tab.insert(s.id, tab);
        }
        self.after_switch();
    }

    fn after_switch(&mut self) {
        self.ui_dirty = true;
        self.relayout();
        if let Some(pane) = self.focused_pane() {
            let _ = self.conn.send(&ClientMsg::Focus { pane });
        }
        if let Some(w) = &self.window {
            w.request_frame();
        }
    }

    /// The content area below the tab bar, in points.
    fn content_area(&self) -> Rect {
        let size = self.size.unwrap_or(Size {
            width: 1200.0,
            height: 800.0,
            scale: 2.0,
        });
        Rect::new(
            0.0,
            BAR_HEIGHT,
            size.width,
            (size.height - BAR_HEIGHT - crate::chrome::STATUS_HEIGHT).max(0.0),
        )
    }

    /// Recomputes the shown tab's layout and asks the daemon for each pane's size.
    fn relayout(&mut self) {
        let sessions = self.sessions();
        self.tab_layout = match self.tab(&sessions) {
            Some(tab) => geometry::layout_tab(tab, self.content_area(), DIVIDER),
            None => TabLayout::default(),
        };
        let Some(gfx) = &self.gfx else { return };
        let scale = self.size.map_or(2.0, |s| s.scale);
        let m = gfx.metrics();
        for (pane, rect) in self.tab_layout.panes.clone() {
            let w = ((rect.w - 2.0 * PADDING) * scale).max(0.0) as u32;
            let h = ((rect.h - 2.0 * PADDING) * scale).max(0.0) as u32;
            let cols = (w / m.cell_width).clamp(2, u32::from(u16::MAX)) as u16;
            let rows = (h / m.cell_height).clamp(1, u32::from(u16::MAX)) as u16;
            if self.sent_sizes.get(&pane) != Some(&(cols, rows)) {
                self.sent_sizes.insert(pane, (cols, rows));
                let _ = self.conn.send(&ClientMsg::Resize { pane, cols, rows });
            }
        }
    }

    fn send_facts(&mut self) {
        let Some(gfx) = &self.gfx else { return };
        let m = gfx.metrics();
        let facts = ClientFacts {
            cell_px: [m.cell_width as u16, m.cell_height as u16],
            dark: self.dark,
            reduce_motion: self.reduce_motion(),
            hour12: auk_platform_macos::system_hour12(),
            // OSC 10/11/4 answers: the colors this window draws panes in.
            colors: Some(ThemeColors::from(&self.theme.facts((0, 0)))),
        };
        if self.facts_sent.as_ref() != Some(&facts) {
            let _ = self.conn.send(&ClientMsg::Facts(facts.clone()));
            self.facts_sent = Some(facts);
        }
    }

    fn resized(&mut self, window: &Window, size: Size) {
        self.size = Some(size);
        let (w, h) = size.physical();
        match &mut self.gfx {
            Some(gfx) => gfx.resize(w, h, size.scale),
            None => self.gfx = Some(Gfx::new(self.gpu.clone(), window, w, h, size.scale)),
        }
        self.ui_dirty = true;
        self.send_facts();
        self.relayout();
        window.request_frame();
    }

    // Drawing.

    fn frame(&mut self, window: &Window) {
        let start = Instant::now();
        if self.gfx.is_none() || self.size.is_none() {
            return;
        }
        let size = self.size.expect("checked");
        let scale = size.scale;
        let metrics = self.gfx.as_ref().expect("checked").metrics();
        let (cw, ch) = (
            f64::from(metrics.cell_width),
            f64::from(metrics.cell_height),
        );
        self.find_before_frame();
        // An armed link's pointing hand outlives AppKit resetting the cursor (a menu bar
        // rebuilt on a settings reload, a window change): put it back.
        self.reassert_pointer(window);
        // The image viewer's picture, once its background decode is done (A9).
        if self.poll_viewer() {
            window.request_frame();
        }
        let sessions = self.sessions();
        let focused = self.focused_pane();
        let mut draws = Vec::new();
        let mut ime_area = None;
        let mut titles: BTreeMap<PaneId, String> = BTreeMap::new();
        let mut a11y_text_gen = 0;
        let mut to_ack: Vec<(PaneId, String, u64)> = Vec::new();
        // The views' clock is in ms (toast durations, spinners, caret blink, `elapsed`).
        let now = self.epoch.elapsed().as_secs_f64() * 1000.0;
        let settings = ViewSettings {
            font_px: crate::gfx::FONT_SIZE,
            cell: ((cw / scale) as f32, (ch / scale) as f32),
            dark: self.dark,
            reduce_motion: self.reduce_motion(),
            reduce_transparency: auk_platform_macos::system_reduce_transparency(),
            more_contrast: auk_platform_macos::system_increase_contrast(),
            program_colors: self.settings.program_colors,
            native_editing: self.settings.native_editing,
            links_plain_click: self.settings.links_plain_click,
            split_diffs: self.settings.split_diffs,
            ..ViewSettings::default()
        };
        // Surfaces paint into the UI layer's scene, under the chrome.
        let mut scene = auk_render::vello::Scene::new();
        let mut painted = false;
        let mut animating = false;
        let mut find_synced = false;
        // The grid link a click would open now draws solid (A13).
        let armed = self.armed_link();
        let link_style = self.settings.hyperlinks;
        let mut phase = Phase::default();
        let mut work = auk_views::LayoutWork::default();
        phase.lap("");
        {
            let conn = self.conn.clone();
            let mut shared = conn.shared();
            self.presence_shown = extras::presence_of(&shared, conn.client_id);
            // Every pane's surfaces take their changes, shown or not.
            for (pane, data) in shared.panes.iter_mut() {
                if std::mem::take(&mut data.reset) {
                    self.surfaces.remove(pane);
                    self.lens_seen.remove(pane);
                }
                let missing: Vec<String> = data
                    .replica
                    .tsp()
                    .missing_blobs()
                    .into_iter()
                    .filter(|id| data.asked.insert(id.clone()))
                    .collect();
                if !missing.is_empty() {
                    let _ = conn.send(&ClientMsg::Blobs {
                        pane: *pane,
                        ids: missing,
                    });
                }
                phase.lap("blobs");
                let changes = data.replica.take_tsp_changes();
                let tsp = data.replica.tsp();
                let sf = self
                    .surfaces
                    .entry(*pane)
                    .or_insert_with(|| PaneSurfaces::new(settings.clone(), scale as f32));
                if !changes.is_empty() {
                    sf.sync(tsp, &changes, now);
                    find_synced |= self.find.as_ref().is_some_and(|f| f.pane == *pane);
                }
                sf.configure(tsp, settings.clone(), scale as f32);
                // Plugin sheets cascade after the built-in ones (§8.4 step 3).
                sf.set_plugin_sheets(&self.plugins.sheets);
                // Command blocks (§9.6), when the replica changed.
                if self.lens_seen.get(pane) != Some(&data.generation) {
                    self.lens_seen.insert(*pane, data.generation);
                    let blocks = lens_inputs(&data.replica);
                    sf.sync_lenses(&blocks, now);
                }
                phase.lap("sync");
            }
            // The views may have rebuilt marked elements: mark the matches again.
            if find_synced {
                self.find_show(false);
            }
            for (pane, title) in titles_of(&shared, &sessions) {
                titles.insert(pane, title);
            }
            for (pane, rect) in self.tab_layout.panes.clone() {
                let Some(data) = shared.panes.get_mut(&pane) else {
                    continue;
                };
                if Some(pane) == focused {
                    a11y_text_gen = data.generation;
                }
                let term = data.replica.terminal();
                let tsp = data.replica.tsp();
                let view = self.views.entry(pane).or_default();
                let content_w = (rect.w - 2.0 * PADDING).max(0.0);
                let content_h = (rect.h - 2.0 * PADDING).max(0.0);
                let area_h = (content_h * scale) as f32;
                let origin = (rect.x + PADDING, rect.y + PADDING);
                view.grid_origin = origin;
                let px = |v: f64| (v * scale).round();

                // The pane's flow (§6.2): grid rows, and surface blocks at their anchor rows.
                let sf = self.surfaces.get_mut(&pane).expect("synced above");
                let pane_focused = self.key_window && Some(pane) == focused;
                phase.lap("sync");
                if sf
                    .layout(
                        term,
                        tsp,
                        (content_w as f32, content_h as f32),
                        pane_focused,
                        now,
                    )
                    .is_some()
                {
                    animating = true;
                }
                let w = sf.view().last_work();
                work.passes += w.passes;
                work.measured += w.measured;
                work.restyled += w.restyled;
                work.inline_rebuilt += w.inline_rebuilt;
                work.laid_out += w.laid_out;
                phase.lap("layout");
                let arranged =
                    sf.arrange_shown(term, tsp, ch as f32, area_h, scale as f32, view.scroll);
                view.scroll = arranged.composed.scroll;
                if sf.paint(
                    &arranged,
                    &mut scene,
                    Place {
                        x: px(origin.0),
                        y: px(origin.1),
                        w: content_w * scale,
                        h: content_h * scale,
                        scale,
                    },
                ) {
                    painted = true;
                }
                phase.lap("paint");
                let composed = &arranged.composed;
                let render_view = View {
                    scroll: 0,
                    selection: view.selection.map(|s| s.for_render(term)),
                    focused: pane_focused,
                    cursor_on: true,
                    links: link_style,
                    hover_link: armed
                        .filter(|(p, _)| *p == pane)
                        .map(|(_, link)| link),
                };
                let rows = composed.slots.len().clamp(1, usize::from(u16::MAX)) as u16;
                let mut snap =
                    GridSnapshot::from_lines(term, &self.theme, &render_view, composed.first, rows);
                find::tint(self.find.as_ref(), pane, composed.first, &mut snap);
                // The input method's composition draws over the cursor's cells (§19).
                let cursor = term.cursor();
                let cursor_line = term.history_len() + usize::from(cursor.row);
                if Some(pane) == focused
                    && let Some(slot_row) = cursor_line.checked_sub(composed.first)
                    && slot_row < usize::from(snap.rows)
                {
                    let slot = composed.slots[slot_row];
                    let x = origin.0 + f64::from(cursor.col) * cw / scale;
                    let y = origin.1 + f64::from(slot.top) / scale;
                    ime_area = Some((x, y, cw / scale, ch / scale));
                    if let Some(p) = &self.preedit {
                        snap.write(
                            cursor.col,
                            slot_row as u16,
                            &p.text,
                            self.theme.foreground,
                            self.theme.background,
                            Attrs::UNDERLINE,
                        );
                        snap.cursor = None;
                    }
                }
                let slots = (!composed.is_uniform(ch as f32)).then(|| {
                    composed
                        .slots
                        .iter()
                        .map(|s| RowSlot {
                            top: s.top,
                            height: s.height,
                        })
                        .collect()
                });
                let grid_shown = arranged.grid_shown();
                view.arranged = arranged;
                view.content_px = area_h;
                if grid_shown {
                    draws.push(PaneDraw {
                        pane,
                        origin: (px(origin.0) as f32, px(origin.1) as f32),
                        clip: (
                            px(rect.x) as u32,
                            px(rect.y) as u32,
                            px(rect.w) as u32,
                            px(rect.h) as u32,
                        ),
                        snapshot: snap,
                        slots,
                    });
                }
                phase.lap("grid");
                // Acks (§7.10, §15.5): the surfaces this frame draws, only as the pane's primary.
                if data.primary {
                    for s in data.replica.tsp().surfaces() {
                        if s.listens()
                            && s.is_live()
                            && let Some(last) = s.last_s()
                        {
                            to_ack.push((pane, s.id().to_owned(), last));
                        }
                    }
                }
            }
        }
        phase.lap("tail");
        if let Some((x, y, w, h)) = ime_area {
            window.set_ime_cursor_area(x, y, w, h);
        }
        self.ime_area = ime_area;
        // A match just stepped to may need another look once virtualized content near it is
        // laid out.
        if self.find_reveal_again() {
            window.request_frame();
        }

        // The chrome, over the surfaces.
        let mut model = self.chrome_model(&sessions, &titles, size);
        self.decorate_chrome(&mut model);
        self.apply_window_title(window, &sessions);
        if self.chrome.update(&model, scale as f32) {
            self.ui_dirty = true;
            self.chrome_updates += 1;
        }
        let gfx = self.gfx.as_mut().expect("checked");
        if self.ui_dirty || painted || self.surfaces_painted || gfx.ui_stale() {
            auk_ui::paint::paint_with_transform(
                self.chrome.doc(),
                &mut scene,
                auk_render::vello::kurbo::Affine::scale(scale),
            );
            gfx.render_ui(&scene);
            self.ui_dirty = false;
        }
        self.surfaces_painted = painted;
        if animating {
            window.request_frame();
        }
        let background = self.theme.background;
        phase.lap("chrome");
        let (ticket, on_done) = self.clock.begin(start);
        let cost: FrameCost = gfx.draw(background, &draws, on_done);
        phase.lap("draw");
        if cost.presented {
            self.clock.submitted();
            for (pane, sf, s) in to_ack {
                let key = (pane, sf.clone());
                if self.acked.get(&key) != Some(&s) {
                    self.acked.insert(key, s);
                    let _ = self.conn.send(&ClientMsg::Ack { pane, sf, s });
                }
            }
        } else {
            window.request_frame();
        }
        let a11y_key = (
            a11y_text_gen ^ focused.unwrap_or(0).rotate_left(32),
            self.chrome_generation(),
        );
        if self.a11y_seen != Some(a11y_key) {
            self.a11y_seen = Some(a11y_key);
            self.a11y_generation += 1;
            crate::a11y::update(self, window);
        }
        if cost.presented {
            self.clock.cpu_done(ticket);
        } else {
            self.clock.abandon(ticket);
        }
        self.stats.frames += 1;
        let cpu = start.elapsed().saturating_sub(cost.drawable_wait);
        push_capped(&mut self.stats.cpu, cpu);
        push_capped(&mut self.stats.drawable_wait, cost.drawable_wait);
        phase.lap("tail");
        // The drawable wait is pacing, not drawing.
        if let Some(d) = phase.spent.get_mut("draw") {
            *d = d.saturating_sub(cost.drawable_wait);
        }
        for name in [
            "blobs", "sync", "layout", "paint", "grid", "chrome", "draw", "tail",
        ] {
            let spent = phase.spent.get(name).copied().unwrap_or_default();
            push_capped(self.stats.phases.entry(name).or_default(), spent);
        }
        for (name, n) in [
            ("passes", work.passes),
            ("measured", work.measured),
            ("restyled", work.restyled),
            ("inline_rebuilt", work.inline_rebuilt),
            ("laid_out", work.laid_out),
        ] {
            let v = self.stats.work.entry(name).or_default();
            if v.len() >= 4096 {
                v.remove(0);
            }
            v.push(n as f64);
        }
        // Window events go to plugins after each render (§9.8).
        self.plugins_after_frame();
    }

    /// The window's title: a plugin's (`tern.chrome.window_title`), else the shown session's
    /// name, with the host's name for a remote window.
    fn window_title(&self, sessions: &Sessions) -> Option<String> {
        if let Some(t) = &self.plugins.window_title {
            return Some(t.clone());
        }
        let name = self.session(sessions)?.name.clone();
        Some(match &self.conn.host {
            Some(host) => format!("{name} \u{2014} {host}"),
            None => name,
        })
    }

    fn apply_window_title(&mut self, window: &Window, sessions: &Sessions) {
        if let Some(title) = self.window_title(sessions)
            && self.title_shown.as_ref() != Some(&title)
        {
            window.set_title(&title);
            self.title_shown = Some(title);
        }
    }

    fn chrome_generation(&self) -> u64 {
        // The chrome model's identity is enough: a hash of its debug form.
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        format!("{:?}", self.chrome.doc().viewport()).hash(&mut h);
        self.active_session.hash(&mut h);
        self.active_tab_id().hash(&mut h);
        // Toasts, the status line and overlays change the chrome's tree too.
        self.chrome_updates.hash(&mut h);
        self.known_tabs.hash(&mut h);
        h.finish()
    }

    fn chrome_model(
        &self,
        sessions: &Sessions,
        titles: &BTreeMap<PaneId, String>,
        size: Size,
    ) -> ChromeModel {
        let active_tab = self.active_tab_id();
        let (tab_apps, pane_app) = (self.chrome_tab_apps_in(sessions), self.chrome_pane_app());
        let tab_agents = self.chrome_tab_agents_in(sessions);
        let tabs = self
            .session(sessions)
            .map(|s| {
                s.tabs
                    .iter()
                    .map(|t| {
                        let title = t.title.clone().unwrap_or_else(|| {
                            let pane = self
                                .tab_focus
                                .get(&t.id)
                                .copied()
                                .or_else(|| t.root.panes().first().copied());
                            pane.and_then(|p| titles.get(&p).cloned())
                                .unwrap_or_else(|| "shell".to_owned())
                        });
                        TabItem {
                            id: t.id,
                            title,
                            active: Some(t.id) == active_tab,
                            zoomed: t.zoom.is_some(),
                            app: tab_apps.get(&t.id).cloned(),
                            agent: tab_agents.contains(&t.id),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let rect = |r: &crate::geometry::Rect| {
            auk_ui::Rect::new(r.x as f32, r.y as f32, r.w as f32, r.h as f32)
        };
        ChromeModel {
            bar: BarModel {
                width: size.width,
                height: size.height,
                dark: self.dark,
                media: (
                    self.reduce_motion(),
                    auk_platform_macos::system_reduce_transparency(),
                    auk_platform_macos::system_increase_contrast(),
                ),
                sessions: sessions
                    .sessions
                    .iter()
                    .map(|s| SessionItem {
                        id: s.id,
                        name: s.name.clone(),
                        active: Some(s.id) == self.active_session,
                        locked: s.locked,
                        named: s.named,
                        tabs: s.tabs.len(),
                    })
                    .collect(),
                tabs,
                dividers: self.tab_layout.dividers.iter().map(rect).collect(),
                focus: self
                    .focused_pane()
                    .and_then(|p| self.tab_layout.rect(p))
                    .map(|r| rect(&r)),
                can_create: true,
                session_menu: self.overlays.session_menu,
            },
            pane_app,
            hover_link: self.hover_link.clone(),
            tooltip: self.hover_title.clone(),
            find: self.find.as_ref().filter(|f| f.open).map(|f| crate::chrome::FindView {
                query: f.query.clone(),
                current: f.current,
                count: f.matches.len(),
            }),
            ..ChromeModel::default()
        }
    }

    // Input.

    pub fn modes(&self, pane: PaneId) -> Modes {
        self.conn
            .shared()
            .panes
            .get(&pane)
            .map(|p| p.replica.terminal().modes())
            .unwrap_or_default()
    }

    /// Sends bytes the user typed into a pane: the view returns to the tail and the selection
    /// clears.
    fn typed(&mut self, window: &Window, pane: PaneId, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        self.conn.input(pane, bytes);
        // Typing scrolls to the bottom and clears the selection. When neither changes anything,
        // the frame waits for the program's echo, so that frame can be drawn the moment it comes
        // (the first frame after a pause draws at once).
        if let Some(v) = self.views.get_mut(&pane)
            && (v.scroll != Scroll::default() || v.selection.is_some())
        {
            v.scroll = Scroll::default();
            v.selection = None;
            window.request_frame();
        }
    }

    fn key(&mut self, window: &Window, key: KeyInput) {
        // Overlays first, then the plugin keymap layer (§9.8 "Binds").
        if self.overlay_key(window, &key) || self.find_key(&key) || self.plugin_key(&key) {
            window.request_frame();
            return;
        }
        let Some(pane) = self.focused_pane() else {
            return;
        };
        // Native editing in a program field (§7.9).
        if self.field_key_input(window, pane, &key) {
            return;
        }
        // ⌘ keys belong to the app: the menu took the ones it binds.
        if key.event.mods.meta {
            return;
        }
        let modes = self.modes(pane);
        let bytes = encode_key(&key.event, &modes);
        self.typed(window, pane, bytes);
    }

    /// Runs a command however it was started (menu, key, palette, status line, plugin):
    /// plugin overrides first, which may skip the built-in (§9.8 "Overrides").
    pub fn command(&mut self, window: &Window, cmd: Cmd) {
        if self.run_override(cmd) {
            window.request_frame();
            return;
        }
        self.run_builtin(window, cmd);
    }

    /// A built-in command, without overrides (`cx:command`).
    fn run_builtin(&mut self, window: &Window, cmd: Cmd) {
        let sessions = self.sessions();
        let focused = self.focused_pane();
        match cmd {
            Cmd::NewWindow => crate::app::new_window(),
            Cmd::ToggleFullScreen => window.toggle_full_screen(),
            Cmd::NewSession => {
                self.pending = Some(Pending::Session);
                self.create(LayoutEdit::NewSession { name: None });
            }
            Cmd::NewTab => {
                if let Some(session) = self.active_session {
                    self.pending = Some(Pending::Tab(session));
                    self.create(LayoutEdit::NewTab { session });
                } else {
                    self.pending = Some(Pending::Session);
                    self.create(LayoutEdit::NewSession { name: None });
                }
            }
            Cmd::SplitRight | Cmd::SplitDown => {
                if let Some(pane) = focused {
                    let dir = if cmd == Cmd::SplitRight {
                        SplitDir::Right
                    } else {
                        SplitDir::Down
                    };
                    self.pending = Some(Pending::Split(pane));
                    self.create(LayoutEdit::Split { pane, dir });
                }
            }
            Cmd::ClosePane => {
                if let Some(pane) = focused {
                    self.edit(LayoutEdit::Close { pane });
                }
            }
            Cmd::ToggleZoom => {
                if let (Some(tab), Some(pane)) = (self.tab(&sessions), focused) {
                    let zoom = if tab.zoom.is_some() { None } else { Some(pane) };
                    self.edit(LayoutEdit::Zoom {
                        tab: tab.id,
                        pane: zoom,
                    });
                }
            }
            Cmd::NextTab | Cmd::PreviousTab => {
                if let Some(s) = self.session(&sessions) {
                    let ids: Vec<TabId> = s.tabs.iter().map(|t| t.id).collect();
                    if let Some(next) = cycle(&ids, self.active_tab_id(), cmd == Cmd::NextTab) {
                        self.select_tab(next);
                    }
                }
            }
            Cmd::SelectTab(n) => {
                if let Some(s) = self.session(&sessions) {
                    let pick = if n >= 9 {
                        s.tabs.last()
                    } else {
                        s.tabs.get(usize::from(n.saturating_sub(1)))
                    };
                    if let Some(t) = pick.map(|t| t.id) {
                        self.select_tab(t);
                    }
                }
            }
            Cmd::NextSession | Cmd::PreviousSession => {
                let ids: Vec<SessionId> = sessions.sessions.iter().map(|s| s.id).collect();
                if let Some(next) = cycle(&ids, self.active_session, cmd == Cmd::NextSession) {
                    self.select_session(next);
                }
            }
            Cmd::Focus(dir) => {
                if let Some(from) = focused
                    && let Some(to) = geometry::neighbor(&self.tab_layout, from, dir)
                {
                    self.focus_pane(to);
                }
            }
            Cmd::Copy => {
                // A program field's native selection first (§7.9), then the grid's.
                let field = focused.and_then(|pane| self.field_selection_text(pane));
                if let Some(text) = field.or_else(|| self.selection_text()) {
                    crate::app::clipboard_set(&text);
                }
            }
            Cmd::Paste => {
                // Into a program field with a native selection, a paste is an `edit` (§7.9).
                if let (Some(pane), Some(text)) = (focused, crate::app::clipboard_get())
                    && !self.surface_text(window, pane, &text)
                {
                    let modes = self.modes(pane);
                    self.typed(window, pane, encode_paste(&text, &modes));
                }
            }
            Cmd::Find => self.find_open(),
            Cmd::FindNext => self.find_step(true),
            Cmd::FindPrevious => self.find_step(false),
            Cmd::PreviousPrompt => self.jump_to_prompt(false),
            Cmd::NextPrompt => self.jump_to_prompt(true),
            Cmd::SelectAll => {
                let field = |s: &mut Self, pane| {
                    s.field_key_input(
                        window,
                        pane,
                        &KeyInput {
                            event: KeyEvent {
                                key: Key::Char('a'),
                                mods: Mods {
                                    meta: true,
                                    ..Mods::default()
                                },
                                text: None,
                            },
                            repeat: false,
                            keycode: 0,
                            option: false,
                        },
                    )
                };
                if let Some(pane) = focused
                    && !field(self, pane)
                {
                    let lines = {
                        let shared = self.conn.shared();
                        shared.panes.get(&pane).map(|p| {
                            let t = p.replica.terminal();
                            (t.history_len() + usize::from(t.rows()), t.cols())
                        })
                    };
                    if let Some((lines, cols)) = lines {
                        let v = self.views.entry(pane).or_default();
                        v.selection = Some(Selection {
                            anchor: (0, 0),
                            head: (lines.saturating_sub(1), cols.saturating_sub(1)),
                            unit: Unit::Cell,
                            block: false,
                        });
                    }
                }
            }
            Cmd::ClearScrollback => {
                // A pane operation (§5.5): the daemon records it in-band, so every replica clears
                // and surfaces anchored in the scrollback end.
                if let Some(pane) = focused {
                    if let Some(v) = self.views.get_mut(&pane) {
                        v.selection = None;
                    }
                    let _ = self.conn.send(&ClientMsg::ClearScrollback { pane });
                    window.request_frame();
                }
            }
            Cmd::CommandPalette => self.open_palette(),
            Cmd::SwitchSession => self.open_session_switcher(),
            Cmd::RenameSession => self.open_session_rename(),
            Cmd::ReloadPlugins => self.plugins_reload(),
            Cmd::ReloadSettings => self.reload_settings(),
            // Locks the shown session, or unlocks it (§4.3).
            Cmd::LockSession => {
                if let Some(s) = self.session(&sessions) {
                    self.edit(LayoutEdit::LockSession {
                        session: s.id,
                        locked: !s.locked,
                    });
                }
            }
            Cmd::ScrollToTop | Cmd::ScrollToBottom => {
                if let Some(pane) = focused
                    && let Some(v) = self.views.get_mut(&pane)
                {
                    v.scroll = if cmd == Cmd::ScrollToTop {
                        Scroll::top()
                    } else {
                        Scroll::default()
                    };
                }
            }
        }
        window.request_frame();
    }

    /// The focused pane's selected text.
    pub fn selection_text(&self) -> Option<String> {
        let pane = self.focused_pane()?;
        let selection = self.views.get(&pane)?.selection?;
        let shared = self.conn.shared();
        let term = shared.panes.get(&pane)?.replica.terminal();
        Some(selection.text(term))
    }

    /// The pane, buffer line and column under a point, through the pane's mixed layout.
    fn cell_at(&self, x: f64, y: f64) -> Option<(PaneId, Line, u16)> {
        let pane = self.tab_layout.pane_at(x, y)?;
        let view = self.views.get(&pane)?;
        let scale = self.size.map_or(2.0, |s| s.scale);
        let m = self.gfx.as_ref()?.metrics();
        let gy = ((y - view.grid_origin.1) * scale) as f32;
        let gx = (x - view.grid_origin.0) * scale;
        let line = match view.arranged.hit(gy.max(0.0))? {
            Hit::Line(line, _) => line,
            Hit::Block(..) => return None,
        };
        let col = (gx.max(0.0) / f64::from(m.cell_width)) as u16;
        Some((pane, line, col))
    }

    fn mouse(&mut self, window: &Window, m: MouseEvent) {
        match m.kind {
            MouseKind::Down(button) => {
                self.field_drag = None;
                if self.chrome_press(window, &m, button) {
                    return;
                }
                let Some(pane) = self.tab_layout.pane_at(m.x, m.y) else {
                    return;
                };
                // A secondary press over a surface opens its context menu (§7.8).
                if button == MouseButton::Right && self.context_menu_at(pane, &m) {
                    window.request_frame();
                    return;
                }
                if self.focused_pane() != Some(pane) {
                    self.focus_pane(pane);
                } else {
                    let _ = self.conn.send(&ClientMsg::Focus { pane });
                }
                // A press on an ordered list's grip drags its item (book: prefs).
                if button == MouseButton::Left && m.clicks <= 1 && self.reorder_press(pane, &m) {
                    window.request_frame();
                    return;
                }
                if matches!(button, MouseButton::Left | MouseButton::Right)
                    && self.surface_click(window, pane, &m, button)
                {
                    if button == MouseButton::Left && m.clicks <= 1 {
                        self.field_press(pane, &m);
                    }
                    return;
                }
                // ⌘-click opens a link or a printed path (routes first).
                // With "links_plain_click", a plain click on a link opens it too (§7.8).
                if button == MouseButton::Left
                    && let Some((p, line, col)) = self.cell_at(m.x, m.y)
                    && p == pane
                    && (m.mods.meta || self.plain_click_link(pane, line, col, &m))
                    && self.grid_link_click(pane, line, col)
                {
                    return;
                }
                let reported = self.report_mouse(pane, &m, button, MouseAction::Press);
                self.press = Some(Press { pane, reported });
                if !reported
                    && button == MouseButton::Left
                    && let Some((p, line, col)) = self.cell_at(m.x, m.y)
                    && p == pane
                {
                    let unit = match m.clicks {
                        0 | 1 => Unit::Cell,
                        2 => Unit::Word,
                        _ => Unit::Line,
                    };
                    let view = self.views.entry(pane).or_default();
                    view.selection = (m.clicks >= 2).then_some(Selection {
                        anchor: (line, col),
                        head: (line, col),
                        unit,
                        block: m.mods.alt,
                    });
                    if m.clicks < 2 {
                        // A single click starts a selection only once the mouse drags.
                        view.selection = None;
                        self.press_anchor = Some((pane, line, col, m.mods.alt));
                    }
                }
                window.request_frame();
            }
            MouseKind::Dragged(button) => {
                if self.reorder_dragged(window, &m) || self.field_dragged(window, &m) {
                    return;
                }
                let Some(press) = self.press else { return };
                if press.reported {
                    self.report_mouse(press.pane, &m, button, MouseAction::Motion);
                    return;
                }
                if let Some((p, line, col)) = self.cell_at(m.x, m.y.max(BAR_HEIGHT + 1.0))
                    && p == press.pane
                {
                    let anchor = self.press_anchor.filter(|a| a.0 == p);
                    let view = self.views.entry(p).or_default();
                    match (&mut view.selection, anchor) {
                        (Some(s), _) => s.head = (line, col),
                        (None, Some((_, l0, c0, block))) => {
                            view.selection = Some(Selection {
                                anchor: (l0, c0),
                                head: (line, col),
                                unit: Unit::Cell,
                                block,
                            })
                        }
                        (None, None) => {}
                    }
                    window.request_frame();
                }
            }
            MouseKind::Up(button) => {
                self.field_drag = None;
                if self.reorder_released(window, &m) {
                    return;
                }
                if let Some(press) = self.press.take()
                    && press.reported
                {
                    self.report_mouse(press.pane, &m, button, MouseAction::Release);
                }
                self.press_anchor = None;
            }
            MouseKind::Moved => {
                if self.hover_at(m.x, m.y)
                    | self.hover_link_at(m.x, m.y)
                    | self.track_grid_link(window, Some((m.x, m.y)), m.mods.meta)
                {
                    window.request_frame();
                }
                self.hover_events(window);
                if let Some(pane) = self.tab_layout.pane_at(m.x, m.y)
                    && self.modes(pane).mouse == MouseMode::AnyMotion
                {
                    self.report_motion(pane, &m);
                }
            }
            MouseKind::Exited => {
                // Off the window, no link is under the pointer.
                let was_armed = self.armed_link().is_some();
                self.hover_grid = None;
                let cmd = self.pointer_cmd;
                self.track_grid_link(window, None, cmd);
                if was_armed {
                    window.request_frame();
                }
            }
            MouseKind::Entered => {}
        }
    }

    /// A press over a surface (a block, the dock, a screen surface or a layer) goes to the pane's
    /// view (§7.8): local flips apply at once and events go to the daemon. Returns whether a
    /// surface took it.
    fn surface_click(
        &mut self,
        window: &Window,
        pane: PaneId,
        m: &MouseEvent,
        button: MouseButton,
    ) -> bool {
        let scale = self.size.map_or(2.0, |s| s.scale);
        let Some(view) = self.views.get(&pane) else {
            return false;
        };
        let x = ((m.x - view.grid_origin.0) * scale) as f32;
        let y = ((m.y - view.grid_origin.1) * scale) as f32;
        if x < 0.0 || y < 0.0 {
            return false;
        }
        let Some(sf) = self.surfaces.get_mut(&pane) else {
            return false;
        };
        let lens_at = sf.lens_point(&view.arranged, x, y, scale as f32);
        let Some((dx, dy)) =
            lens_at.or_else(|| sf.doc_point(&view.arranged, x, y, view.content_px, scale as f32))
        else {
            return false;
        };
        let ev = auk_views::PointerEvent {
            x: dx,
            y: dy,
            clicks: m.clicks.clamp(1, 2) as u8,
            mods: auk_tsp::Mods {
                shift: m.mods.shift,
                ctrl: m.mods.ctrl,
                alt: m.mods.alt,
                meta: m.mods.meta,
            },
            button: if button == MouseButton::Right {
                auk_views::Button::Secondary
            } else {
                auk_views::Button::Primary
            },
        };
        let outcomes = if lens_at.is_some() {
            // A command block (§9.6): Native/Raw and Copy are local; lens view events go to
            // `lens:<block>`.
            sf.lens_click(ev)
        } else {
            let conn = self.conn.clone();
            let shared = conn.shared();
            let Some(data) = shared.panes.get(&pane) else {
                return false;
            };
            let (doc, pv) = sf.parts();
            pv.click(doc, data.replica.tsp(), ev)
        };
        let took = lens_at.is_some() || !outcomes.iter().all(|o| matches!(o, Outcome::Keys));
        self.outcomes(window, pane, outcomes);
        took
    }

    /// Text typed while a program field has focus and a native selection goes into the field as an
    /// `edit` (§7.9); otherwise it is keys.
    fn surface_text(&mut self, window: &Window, pane: PaneId, text: &str) -> bool {
        let Some(sf) = self.surfaces.get_mut(&pane) else {
            return false;
        };
        let outcome = {
            let conn = self.conn.clone();
            let shared = conn.shared();
            let Some(data) = shared.panes.get(&pane) else {
                return false;
            };
            let (doc, pv) = sf.parts();
            pv.type_text(doc, data.replica.tsp(), text)
        };
        let took = !matches!(outcome, Outcome::Keys);
        self.outcomes(window, pane, vec![outcome]);
        took
    }

    /// Carries out what the view asked for after input.
    fn outcomes(&mut self, window: &Window, pane: PaneId, outcomes: Vec<Outcome>) {
        for outcome in outcomes {
            match outcome {
                Outcome::Event { sf, event, .. } => {
                    let mut body = event.body("");
                    if let Some(obj) = body.as_object_mut() {
                        obj.remove("sf");
                    }
                    let _ = self.conn.send(&ClientMsg::ViewEvent {
                        pane,
                        sf,
                        event: body,
                    });
                }
                Outcome::Copy(text) => {
                    crate::app::clipboard_set(&text);
                }
                // Paths and links a surface opens go through the routes (§9.8).
                Outcome::Open(target) => self.open_from_surface(pane, &target),
                Outcome::Zoom(zoom) => self.show_viewer(pane, *zoom),
                Outcome::Keys | Outcome::Redraw => {}
            }
        }
        window.request_frame();
    }

    fn report_motion(&mut self, pane: PaneId, m: &MouseEvent) {
        let Some((p, line, col)) = self.cell_at(m.x, m.y) else {
            return;
        };
        if p != pane {
            return;
        }
        let Some(row) = self.screen_row(pane, line) else {
            return;
        };
        if self.last_motion == Some((pane, col, row)) {
            return;
        }
        self.last_motion = Some((pane, col, row));
        let modes = self.modes(pane);
        let bytes = encode_mouse(
            TermButton::None,
            MouseAction::Motion,
            m.mods,
            col,
            row,
            &modes,
        );
        self.conn.input(pane, bytes);
    }

    /// The screen row of a buffer line, if it's on the screen.
    fn screen_row(&self, pane: PaneId, line: Line) -> Option<u16> {
        let shared = self.conn.shared();
        let term = shared.panes.get(&pane)?.replica.terminal();
        let row = line.checked_sub(term.history_len())?;
        (row < usize::from(term.rows())).then_some(row as u16)
    }

    /// Sends a mouse report when the program asked for mouse events (and Shift isn't held, which
    /// keeps selection available). Returns whether the event went to the program.
    fn report_mouse(
        &mut self,
        pane: PaneId,
        m: &MouseEvent,
        button: MouseButton,
        action: MouseAction,
    ) -> bool {
        let modes = self.modes(pane);
        if modes.mouse == MouseMode::Off || m.mods.shift {
            return false;
        }
        let Some((p, line, col)) = self.cell_at(m.x, m.y) else {
            return action != MouseAction::Press;
        };
        if p != pane {
            return true;
        }
        let Some(row) = self.screen_row(pane, line) else {
            return true;
        };
        let b = match button {
            MouseButton::Left => TermButton::Left,
            MouseButton::Right => TermButton::Right,
            MouseButton::Middle => TermButton::Middle,
            MouseButton::Other(_) => return false,
        };
        let mods = Mods {
            meta: false,
            ..m.mods
        };
        let bytes = encode_mouse(b, action, mods, col, row, &modes);
        self.conn.input(pane, bytes);
        true
    }

    fn scroll(&mut self, window: &Window, s: ScrollEvent) {
        let Some(pane) = self.tab_layout.pane_at(s.x, s.y).or(self.focused_pane()) else {
            return;
        };
        let scale = self.size.map_or(2.0, |sz| sz.scale);
        let Some(m) = self.gfx.as_ref().map(Gfx::metrics) else {
            return;
        };
        let ch = f64::from(m.cell_height);
        // Pixels: trackpads report points, wheels report lines.
        let dy = if s.precise { s.dy * scale } else { s.dy * ch };
        // Over a surface, its scroll containers take the wheel first: a screen surface's `main`
        // takes the pane's height and scrolls itself (§7.5), as do lists and fields that scroll.
        // A screen surface covers the grid, so the wheel never scrolls the hidden grid under it.
        if let Some((dx, sy)) = self.surface_point(pane, s.x, s.y) {
            let moved = self.surfaces.get_mut(&pane).is_some_and(|sf| {
                let (doc, pv) = sf.parts();
                pv.wheel(doc, dx, sy, (dy / scale) as f32)
            });
            if moved {
                self.ui_dirty = true;
                window.request_frame();
                return;
            }
            if self.views.get(&pane).is_some_and(|v| v.arranged.screen.is_some()) {
                return;
            }
        }
        let modes = self.modes(pane);
        let following = self.views.get(&pane).is_none_or(|v| v.scroll.following());
        if modes.mouse != MouseMode::Off && !s.mods.shift {
            // Wheel reports, a line at a time.
            let view = self.views.entry(pane).or_default();
            view.wheel += dy;
            let lines = (view.wheel / ch).trunc();
            view.wheel -= lines * ch;
            let Some((_, line, col)) = self.cell_at(s.x, s.y) else {
                return;
            };
            let Some(row) = self.screen_row(pane, line) else {
                return;
            };
            let button = if lines > 0.0 {
                TermButton::WheelUp
            } else {
                TermButton::WheelDown
            };
            let mut bytes = Vec::new();
            for _ in 0..(lines.abs() as usize).min(20) {
                bytes.extend(encode_mouse(
                    button,
                    MouseAction::Press,
                    s.mods,
                    col,
                    row,
                    &modes,
                ));
            }
            self.conn.input(pane, bytes);
            return;
        }
        if modes.alt_screen && following {
            // The alternate screen has no scrollback: wheel turns into arrow keys.
            let view = self.views.entry(pane).or_default();
            view.wheel += dy;
            let lines = (view.wheel / ch).trunc();
            view.wheel -= lines * ch;
            let key = if lines > 0.0 { Key::Up } else { Key::Down };
            let ev = KeyEvent {
                key,
                mods: Mods::default(),
                text: None,
            };
            let mut bytes = Vec::new();
            for _ in 0..(lines.abs() as usize).min(20) {
                bytes.extend(input::encode_key(&ev, &modes));
            }
            self.conn.input(pane, bytes);
            return;
        }
        let view = self.views.entry(pane).or_default();
        view.scroll = Scroll::by(&view.arranged.composed, dy as f32);
        window.request_frame();
    }

    fn chrome_action(&mut self, action: ChromeAction) {
        // A choice in the session menu closes it; the chip toggles it.
        let menu_was_open = self.overlays.session_menu;
        self.overlays.session_menu = false;
        match action {
            ChromeAction::SessionMenu => {
                self.overlays.palette = None;
                self.overlays.menu = None;
                self.overlays.session_menu = !menu_was_open;
            }
            ChromeAction::Session(id) => self.select_session(id),
            ChromeAction::Tab(id) => self.select_tab(id),
            // The bar's and the session menu's buttons run the commands, overrides first.
            ChromeAction::NewSession
            | ChromeAction::NewTab
            | ChromeAction::RenameSession
            | ChromeAction::LockSession => {
                if let Some(window) = self.window.clone() {
                    let cmd = match action {
                        ChromeAction::NewTab => Cmd::NewTab,
                        ChromeAction::NewSession => Cmd::NewSession,
                        ChromeAction::RenameSession => Cmd::RenameSession,
                        _ => Cmd::LockSession,
                    };
                    self.command(&window, cmd);
                }
            }
            other => self.overlay_action(other),
        }
    }

    pub fn set_option_as_alt(&mut self, setting: OptionAsAlt) {
        self.option_as_alt = setting;
        if let Some(w) = &self.window {
            w.set_option_as_alt(setting);
        }
    }
}

fn next_focus_serial() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(1);
    N.fetch_add(1, Ordering::Relaxed)
}

fn push_capped(v: &mut Vec<Duration>, d: Duration) {
    if v.len() >= 4096 {
        v.remove(0);
    }
    v.push(d);
}

/// The next (or previous) id after `current`, wrapping.
fn cycle<T: Copy + PartialEq>(ids: &[T], current: Option<T>, forward: bool) -> Option<T> {
    if ids.is_empty() {
        return None;
    }
    let i = current
        .and_then(|c| ids.iter().position(|x| *x == c))
        .unwrap_or(0);
    let n = ids.len();
    Some(
        ids[if forward {
            (i + 1) % n
        } else {
            (i + n - 1) % n
        }],
    )
}

/// A pane's title: the program's (OSC 0/2), else its cwd's last component.
/// A replica's command blocks as the views draw them (§9.6).
fn lens_inputs(replica: &auk_replica::Replica) -> Vec<auk_views::LensInput> {
    replica
        .lens_blocks()
        .iter()
        .map(|b| {
            let raw = replica.lens_raw(b.id).unwrap_or_default();
            let mut node = b.node(raw);
            // `cmd` is the line as typed: the label's tooltip.
            node["p"]["cmd"] = b.typed.clone().into();
            auk_views::LensInput {
                id: b.id,
                anchor: auk_term::AnchorKey(b.anchor),
                node,
                cols: b.cols,
            }
        })
        .collect()
}

fn titles_of(shared: &Shared, sessions: &Sessions) -> Vec<(PaneId, String)> {
    sessions
        .panes()
        .into_iter()
        .filter_map(|p| {
            let data = shared.panes.get(&p)?;
            let title = data.replica.terminal().title().trim().to_owned();
            let title = if title.is_empty() {
                data.replica
                    .cwd()
                    .and_then(|c| {
                        c.trim_end_matches('/')
                            .rsplit('/')
                            .next()
                            .map(str::to_owned)
                    })
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "shell".to_owned())
            } else {
                title
            };
            Some((p, title))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycling_wraps() {
        assert_eq!(cycle(&[1, 2, 3], Some(3), true), Some(1));
        assert_eq!(cycle(&[1, 2, 3], Some(1), false), Some(3));
        assert_eq!(cycle(&[1, 2, 3], None, true), Some(2));
        assert_eq!(cycle::<u64>(&[], None, true), None);
    }
}
