//! A surface (§7.3): its mode, buffer, lifetime state, anchor, palette, sheets and document.

use crate::doc::{DocChanges, Document, Reveal, Scroll};
use crate::grid::AnchorKey;
use crate::style::{Palette, Sheets};

/// A surface's identity within one replica: unique, never reused. Replica-local; programs and the
/// wire name surfaces by their string id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SurfaceKey(pub u64);

/// Where a surface sits (§7.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mode {
    /// Anchored at the cursor row on the main screen; owns the pane while live.
    Inline,
    /// Covers the pane.
    Screen,
    /// Anchored at the cursor row among the output.
    Flow,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "inline" => Mode::Inline,
            "screen" => Mode::Screen,
            "flow" => Mode::Flow,
            _ => return None,
        })
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Mode::Inline => "inline",
            Mode::Screen => "screen",
            Mode::Flow => "flow",
        }
    }
}

/// The screen buffer a surface belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Buffer {
    Main,
    Alt,
}

impl Buffer {
    pub const fn from_alt(alt: bool) -> Self {
        if alt { Buffer::Alt } else { Buffer::Main }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Buffer::Main => "main",
            Buffer::Alt => "alt",
        }
    }
}

/// A surface's lifetime state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum State {
    /// Open: takes frames.
    Live,
    /// Closed with `keep`: `main` stays, inert, until the surface ends.
    Closed,
}

/// What the replying replica tracks per surface for acks and `resize`. Not replicated.
#[derive(Clone, Debug, Default)]
pub(crate) struct Tracking {
    /// Frames applied (any `s`).
    pub applied: u64,
    /// The last ack sent: `(s, applied at the time)`.
    pub acked: Option<(u64, u64)>,
    /// The width and cell size last reported in a `resize`; `None` until first drawn.
    pub reported: Option<(u16, f64, f64)>,
}

/// Surface-level changes since the last drain.
#[derive(Clone, Debug, Default)]
pub(crate) struct Flags {
    pub opened: bool,
    pub meta: bool,
    pub palette: bool,
    pub sheets: bool,
}

/// A surface and its document.
#[derive(Clone, Debug)]
pub struct Surface {
    pub(crate) key: SurfaceKey,
    pub(crate) id: String,
    pub(crate) mode: Mode,
    pub(crate) buffer: Buffer,
    pub(crate) state: State,
    pub(crate) listen: bool,
    pub(crate) title: Option<String>,
    pub(crate) role: Option<String>,
    pub(crate) anchor: Option<AnchorKey>,
    pub(crate) suspended: bool,
    pub(crate) palette: Option<Palette>,
    pub(crate) sheets: Sheets,
    pub(crate) doc: Document,
    pub(crate) last_s: Option<u64>,
    /// Came back from the state file after a daemon restart (§5.4): closed and read-only, and
    /// the prompt rule leaves it alone, like a flow surface, until its anchor row ends.
    pub(crate) restored: bool,
    pub(crate) delivery: Tracking,
    pub(crate) flags: Flags,
}

impl Surface {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        key: SurfaceKey,
        id: String,
        mode: Mode,
        buffer: Buffer,
        listen: bool,
        title: Option<String>,
        role: Option<String>,
        anchor: Option<AnchorKey>,
    ) -> Self {
        let doc = Document::new(&id);
        Surface {
            key,
            id,
            mode,
            buffer,
            state: State::Live,
            listen,
            title,
            role,
            anchor,
            suspended: false,
            palette: None,
            sheets: Sheets::default(),
            doc,
            last_s: None,
            restored: false,
            delivery: Tracking::default(),
            flags: Flags {
                opened: true,
                ..Flags::default()
            },
        }
    }

    pub fn key(&self) -> SurfaceKey {
        self.key
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn buffer(&self) -> Buffer {
        self.buffer
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn is_live(&self) -> bool {
        self.state == State::Live
    }

    /// Whether the program reads events about it (`listen`, default true).
    pub fn listens(&self) -> bool {
        self.listen
    }

    /// Whether the user can act on it: live, listening and not suspended. Controls in closed or
    /// `listen:false` surfaces don't flip.
    pub fn interactive(&self) -> bool {
        self.is_live() && self.listen && !self.suspended
    }

    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// `o.role`: `data-surface` on the region elements.
    pub fn role(&self) -> Option<&str> {
        self.role.as_deref()
    }

    /// The anchor row of an inline or flow surface.
    pub fn anchor(&self) -> Option<AnchorKey> {
        self.anchor
    }

    /// Whether it came back from the state file after a daemon restart (§5.4).
    pub fn restored(&self) -> bool {
        self.restored
    }

    /// Whether the program handed the pane back to the grid (`suspend`).
    pub fn suspended(&self) -> bool {
        self.suspended
    }

    pub fn palette(&self) -> Option<&Palette> {
        self.palette.as_ref()
    }

    pub fn sheets(&self) -> &Sheets {
        &self.sheets
    }

    pub fn document(&self) -> &Document {
        &self.doc
    }

    /// The `s` of the last frame applied.
    pub fn last_s(&self) -> Option<u64> {
        self.last_s
    }

    /// The retention estimate: document, palette and sheets.
    pub fn cost(&self) -> u64 {
        self.doc.cost()
            + self.palette.as_ref().map_or(0, Palette::cost)
            + self.sheets.bytes() as u64
    }

    pub(crate) fn take_changes(&mut self) -> Option<SurfaceChanges> {
        let doc = self.doc.take_changes();
        let reveal = self.doc.take_reveal();
        let scrolls = self.doc.take_scrolls();
        let flags = std::mem::take(&mut self.flags);
        let changes = SurfaceChanges {
            key: self.key,
            opened: flags.opened,
            meta: flags.meta,
            palette: flags.palette,
            sheets: flags.sheets,
            doc,
            reveal,
            scrolls,
        };
        (!changes.is_empty()).then_some(changes)
    }
}

/// What changed in one surface since the last drain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SurfaceChanges {
    pub key: SurfaceKey,
    /// The surface is new to this drain.
    pub opened: bool,
    /// State, listen, title, role or suspension changed.
    pub meta: bool,
    pub palette: bool,
    pub sheets: bool,
    /// Node-level changes.
    pub doc: DocChanges,
    /// The latest `reveal`, if any.
    pub reveal: Option<Reveal>,
    /// `scroll` requests, in order.
    pub scrolls: Vec<Scroll>,
}

impl SurfaceChanges {
    pub fn is_empty(&self) -> bool {
        !self.opened
            && !self.meta
            && !self.palette
            && !self.sheets
            && self.doc.is_empty()
            && self.reveal.is_none()
            && self.scrolls.is_empty()
    }
}
