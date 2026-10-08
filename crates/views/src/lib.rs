//! TSP kinds as UI components, the built-in stylesheets and the per-window view layer (spec §8,
//! §18.2–§18.4, research/elements.md).
//!
//! A [`PaneView`] projects one pane's TSP surfaces (auk-tsp) into the UI engine's DOM (auk-ui),
//! incrementally from [`auk_tsp::TspPane::take_changes`]: each node becomes `.sf.sf-<kind>` with
//! its data attributes, regions become `div.sf-region`, program sheets install scoped to their
//! surface and palettes become `--sf-p-*` variables. The view owns everything that is local to a
//! window and never replicated: folds and control flips, scroll positions, hover, native
//! selection and IME preedit in fields, the motion clock, the toast layer and the accessibility
//! tree. Pointer input maps to [`auk_tsp::ViewEvent`]s; keys stay keys except `edit`, `undo` and
//! `send`.
//!
//! Hosts: install the built-in sheets once per document ([`install_sheets`]), create a
//! [`PaneView`] per pane, call [`PaneView::sync`] after draining changes, tell it what part of
//! the document the window shows ([`PaneView::set_view_rect`], for virtualized content the pane
//! scrolls), call [`PaneView::layout`] to lay out (it runs the measure-then-layout loops) and
//! [`PaneView::tick`] at display rate while it asks for frames, and route pointer and field input
//! through [`PaneView::click`] and friends. Build the pane's engine with [`sheet_checker`] so
//! programs get the engine's CSS errors. [`PaneView::to_html`] serializes the projection with
//! every sheet that applies, for laying the same page out in WebKit.
//!
//! Windows composite a pane through [`compose`] (the §6.2 flow: grid rows and surface blocks,
//! following the tail, scroll anchoring) and [`surfaces`] ([`PaneSurfaces`]: the document, the
//! blocks, docks, screen surfaces and layers, painted into a Vello scene). The app, the headless
//! window and the web client share both.

mod a11y;
mod ansi;
mod checker;
mod common;
pub mod compose;
mod control;
pub mod diff;
mod dom;
mod events;
pub mod highlight;
pub mod html;
mod icon_table;
pub mod icons;
pub mod image;
mod kinds;
mod md;
mod measure;
pub mod motion;
pub mod palette;
pub mod sheets;
pub mod spans;
mod state;
pub mod surfaces;
mod view;

pub use a11y::AxNode;
pub use checker::{UiSheetChecker, sheet_checker};
pub use dom::Written;
pub use events::{Button, FieldKey, MenuEntry, Outcome, PointerEvent, Zoom, ZoomImage};
pub use kinds::KINDS;
pub use sheets::install_sheets;
pub use state::{Preedit, Selection};
pub use surfaces::{Arranged, PaneSurfaces, Place, SurfaceBlock};
pub use view::PaneView;

/// The regions of a surface (§7.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Region {
    Main = 0,
    Dock = 1,
    Layer = 2,
}

/// Window and user settings the views read. Changing them re-renders
/// ([`PaneView::set_settings`]).
#[derive(Clone, Debug, PartialEq)]
pub struct ViewSettings {
    /// The terminal font size in px; regions zoom by `font_px / 13` (§8.3).
    pub font_px: f32,
    /// The cell size in px at that font.
    pub cell: (f32, f32),
    pub dark: bool,
    /// Reduce Motion (the system setting or ours): still equivalents (§7.13).
    pub reduce_motion: bool,
    /// "Use program colors": apply program palettes (§7.12).
    pub program_colors: bool,
    /// The "Edits" setting: split diffs wider than 112 cells when `mode` is absent or `auto`.
    pub split_diffs: bool,
    /// Native editing in program fields (§7.9).
    pub native_editing: bool,
    /// "Open links with" plain click (otherwise ⌘-click).
    pub links_plain_click: bool,
    /// Markdown headings: annotated (body size with `#` markers) or sized.
    pub annotated_headings: bool,
}

impl Default for ViewSettings {
    fn default() -> Self {
        ViewSettings {
            font_px: 13.0,
            cell: (8.0, 17.0),
            dark: true,
            reduce_motion: false,
            program_colors: true,
            split_diffs: false,
            native_editing: true,
            links_plain_click: false,
            annotated_headings: true,
        }
    }
}
