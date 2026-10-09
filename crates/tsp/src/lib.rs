//! The Tern Surface Protocol, terminal side (spec §7): wire codec and chunking, handshake,
//! surfaces, documents and their ops, events and the listening rules, credits and acks, retention,
//! palettes, stylesheets, blobs, the snapshot encoder and the replicated-state hash.
//!
//! A pure, deterministic library: no I/O, clocks or threads, and it builds for wasm32. A pane's
//! replica owns one [`TspPane`] and calls it in stream order:
//!
//! - [`TspPane::on_apc`] with each `auk_term::Event::Tsp` payload;
//! - the grid hooks ([`TspPane::on_prompt`], [`on_exit`](TspPane::on_exit),
//!   [`on_reset`](TspPane::on_reset), [`on_alt_screen`](TspPane::on_alt_screen),
//!   [`on_screen_cleared`](TspPane::on_screen_cleared),
//!   [`on_anchor_gone`](TspPane::on_anchor_gone), [`on_title`](TspPane::on_title),
//!   [`on_resize`](TspPane::on_resize));
//! - records: [`TspPane::set_config`], [`TspPane::expire_chunks`] (the daemon's 1 s chunk timeout);
//! - from the primary client: [`TspPane::set_facts`], [`TspPane::ack`], [`TspPane::view_event`].
//!
//! The grid is reached through [`Grid`], which `auk_term::Terminal` implements. Messages for the
//! program queue up in [`TspPane::take_output`]; the daemon writes them subject to the tty mode
//! gate (§12.1), and other replicas drop them. Views read [`TspPane::surfaces`] and drain
//! [`TspPane::take_changes`].

pub mod blob;
pub mod doc;
pub mod events;
pub mod grid;
mod hash;
pub mod pane;
mod snapshot;
pub mod style;
pub mod surface;
pub mod wire;

pub use blob::{Blob, BlobStore, MemoryBlobs};
pub use doc::{Document, EL_TAGS, NodeRef, Reveal, RevealAt, Scroll, ScrollBy, el_tag_error};
pub use events::{ActValue, Delivery, Mods, Outgoing, Refused, ViewEvent};
pub use grid::{AnchorError, AnchorKey, Grid, Line};
pub use hash::{AGE_PROPS, AGENT_AGE_OBJECTS};
pub use pane::{
    ALL_FEATURES, ASIDE_COLS, Changes, Config, Facts, Gate, Hello, Outside, T0_KINDS, T1_KINDS,
    T2_KINDS, TitleSource, TspPane, aside_reserve,
};
pub use snapshot::Rewrite;
pub use style::{AcceptAll, Palette, SheetChecker, Sheets};
pub use surface::{Buffer, Mode, State, Surface, SurfaceChanges, SurfaceKey};
pub use wire::{Message, ToProgram};
