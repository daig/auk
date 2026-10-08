//! What the TSP engine needs from the pane's grid: anchor rows (§7.4), the active screen and the
//! width. It mirrors `auk_term::Terminal`'s anchor API, which implements it.

pub use auk_term::{AnchorError, AnchorKey, Line};

/// The grid side of a pane, as the TSP engine sees it.
pub trait Grid {
    /// Opens an anchor at the cursor (§7.4). Fails on the alternate screen.
    fn open_anchor(&mut self, key: AnchorKey) -> Result<Line, AnchorError>;
    /// Undoes an anchor row if nothing was written below it; returns whether it was undone.
    fn undo_anchor(&mut self, key: AnchorKey) -> bool;
    /// Ends an anchor without touching the grid.
    fn drop_anchor(&mut self, key: AnchorKey);
    /// The anchor's line in the main screen's buffer, if it still exists.
    fn anchor_line(&self, key: AnchorKey) -> Option<Line>;
    /// Whether the alternate screen is active.
    fn alt_screen(&self) -> bool;
    /// The pane width in columns.
    fn cols(&self) -> u16;
}

impl Grid for auk_term::Terminal {
    fn open_anchor(&mut self, key: AnchorKey) -> Result<Line, AnchorError> {
        auk_term::Terminal::open_anchor(self, key)
    }

    fn undo_anchor(&mut self, key: AnchorKey) -> bool {
        auk_term::Terminal::undo_anchor(self, key)
    }

    fn drop_anchor(&mut self, key: AnchorKey) {
        auk_term::Terminal::drop_anchor(self, key);
    }

    fn anchor_line(&self, key: AnchorKey) -> Option<Line> {
        auk_term::Terminal::anchor_line(self, key)
    }

    fn alt_screen(&self) -> bool {
        self.modes().alt_screen
    }

    fn cols(&self) -> u16 {
        auk_term::Terminal::cols(self)
    }
}
