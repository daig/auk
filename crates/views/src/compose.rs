//! The pane flow: runs of grid rows at a fixed height, with surface blocks at their natural
//! height in place of their anchor rows. [`compose`] is pure: from the pane's line count, its
//! blocks, its tail and the scroll state it decides which buffer lines to draw, where each row
//! goes and where each visible block goes. Selection, mouse reports, links and clicks on surfaces
//! map points through the same result ([`Composed::hit`]).
//!
//! **Following the tail.** The tail is the last line with anything on it: the cursor's, the last
//! written row's or the last block's ([`tail_line`]). A following view shows the screen from its
//! top row while everything down to the tail fits; once blocks make the content taller than the
//! view, it scrolls just enough to keep the tail's bottom at the view's bottom. So blank screen
//! rows below the tail give way first, and a surface the program just drew stays in view rather
//! than being pushed off by empty rows (while the view follows the tail, the tail stays pinned).
//!
//! **Scrolled up.** Scrolling is in pixels, measured up from the following position. Once
//! scrolled, the view is anchored to the line at its top, so content above or below that changes
//! height keeps what is visible in place.
//!
//! The app window, the headless window and the web client all lay panes out through this module
//! and [`crate::surfaces`].

use auk_term::{Line, Terminal};

/// A drawn row's place in the flow area, in pixels from its top. Rows of hidden lines have
/// height 0.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Slot {
    pub top: f32,
    pub height: f32,
}

/// A surface block in the pane's flow: shown in place of its anchor row.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Block {
    /// The anchor row's buffer line.
    pub line: Line,
    /// The block's natural height, in pixels.
    pub height: f32,
}

/// What a pane's flow is made from.
#[derive(Clone, Debug, PartialEq)]
pub struct Flow<'a> {
    /// Lines in the buffer: scrollback plus the screen.
    pub lines: usize,
    /// The screen's first line (the scrollback's length).
    pub screen_top: Line,
    /// The last line with anything on it ([`tail_line`]).
    pub tail: Line,
    /// Cell height, in pixels.
    pub cell: f32,
    /// The height available for the flow (the pane's grid area less the dock), in pixels.
    pub view: f32,
    /// Blocks, in line order.
    pub blocks: &'a [Block],
    /// Lines from here on are hidden: stray output under a live inline surface.
    pub hide_from: Option<Line>,
}

impl Flow<'_> {
    /// A plain terminal's flow: no blocks, the tail at the cursor or the last written row.
    pub fn grid<'a>(term: &Terminal, cell: f32, view: f32) -> Flow<'a> {
        Flow {
            lines: term.history_len() + usize::from(term.rows()),
            screen_top: term.history_len(),
            tail: tail_line(term, &[]),
            cell,
            view,
            blocks: &[],
            hide_from: None,
        }
    }
}

/// The tail of a terminal's flow: the last line with anything on it, the cursor's line, a
/// written row's or a block's anchor, whichever is lowest.
pub fn tail_line(term: &Terminal, blocks: &[Block]) -> Line {
    let lines = term.history_len() + usize::from(term.rows());
    let cursor = term.history_len() + usize::from(term.cursor().row);
    let written = (cursor + 1..lines)
        .rev()
        .find(|&l| {
            term.line(l)
                .is_some_and(|r| r.cells.iter().any(|c| !c.text.is_empty()))
        })
        .unwrap_or(cursor);
    blocks
        .iter()
        .map(|b| b.line)
        .chain([written, cursor])
        .max()
        .unwrap_or(0)
        .min(lines.saturating_sub(1))
}

/// The view's scroll state, kept per pane by the window (view state, never replicated).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Scroll {
    /// Pixels the view is scrolled up from the following position; 0 follows the tail.
    pub from_bottom: f32,
    /// While scrolled up: the line at the top of the view and how far into it the view starts.
    pub anchor: Option<(Line, f32)>,
}

impl Scroll {
    pub fn following(&self) -> bool {
        self.anchor.is_none()
    }

    /// Scrolled all the way up (the next [`compose`] clamps it).
    pub fn top() -> Scroll {
        Scroll {
            from_bottom: f32::MAX,
            anchor: None,
        }
    }

    /// Scrolled `by` pixels further up (negative: down) from where `composed` left the view.
    pub fn by(composed: &Composed, by: f32) -> Scroll {
        Scroll {
            from_bottom: (composed.scroll.from_bottom + by).clamp(0.0, composed.max_scroll),
            anchor: None,
        }
    }
}

/// Where a pane's content goes this frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Composed {
    /// The first buffer line drawn.
    pub first: Line,
    /// Each drawn row's slot, relative to the top of the flow area; row `i` is line
    /// `first + i`.
    pub slots: Vec<Slot>,
    /// Visible blocks: (index into the flow's blocks, top relative to the flow area).
    pub blocks: Vec<(usize, f32)>,
    /// The content's total height.
    pub content: f32,
    /// How far the view can scroll up from the following position.
    pub max_scroll: f32,
    /// The scroll state, after anchoring.
    pub scroll: Scroll,
}

/// What lies under a point in the flow.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    /// A grid row: the buffer line, and the point's offset into the row.
    Line(Line, f32),
    /// A block (index into the flow's blocks), and the point's offset into it.
    Block(usize, f32),
}

impl Composed {
    /// Whether the composition is the plain grid: rows one cell apart from line `first`.
    pub fn is_uniform(&self, cell: f32) -> bool {
        self.blocks.is_empty()
            && self
                .slots
                .iter()
                .enumerate()
                .all(|(i, s)| s.height == cell && s.top == i as f32 * cell)
    }

    /// What is at `y` pixels from the top of the flow area.
    pub fn hit(&self, y: f32, blocks: &[Block]) -> Option<Hit> {
        for (i, slot) in self.slots.iter().enumerate() {
            if slot.height > 0.0 && y >= slot.top && y < slot.top + slot.height {
                let line = self.first + i;
                if let Some(b) = blocks.iter().position(|b| b.line == line) {
                    return Some(Hit::Block(b, y - slot.top));
                }
                return Some(Hit::Line(line, y - slot.top));
            }
        }
        None
    }

    /// The slot of buffer line `line`, if drawn.
    pub fn slot_of(&self, line: Line) -> Option<Slot> {
        line.checked_sub(self.first)
            .and_then(|i| self.slots.get(i))
            .copied()
    }
}

fn line_height(flow: &Flow<'_>, line: Line) -> f32 {
    if flow.hide_from.is_some_and(|h| line >= h) {
        return 0.0;
    }
    match flow.blocks.iter().find(|b| b.line == line) {
        Some(b) => b.height.max(0.0),
        None => flow.cell,
    }
}

/// Lays out the flow. `scroll` is the window's scroll state; the result carries it back after
/// anchoring (content above the anchor that changed height keeps the anchored line in place).
pub fn compose(flow: &Flow<'_>, scroll: Scroll) -> Composed {
    let lines = flow.lines;
    let mut tops = Vec::with_capacity(lines + 1);
    let mut y = 0.0f32;
    for line in 0..lines {
        tops.push(y);
        y += line_height(flow, line);
    }
    tops.push(y);
    let content = y;
    if lines == 0 {
        return Composed {
            content,
            ..Composed::default()
        };
    }

    // The following position: the screen's top, or lower when the tail would fall below the
    // view. Blank rows past the tail are what give way.
    let screen_top = tops[flow.screen_top.min(lines)];
    let tail_bottom = tops[(flow.tail + 1).min(lines)];
    let follow = screen_top.max(tail_bottom - flow.view).max(0.0);
    let max_scroll = follow;

    let mut scroll = scroll;
    let top = match scroll.anchor {
        Some((line, offset)) if line < lines => {
            let top = (tops[line] + offset).clamp(0.0, follow);
            scroll.from_bottom = follow - top;
            top
        }
        _ => {
            scroll.from_bottom = scroll.from_bottom.clamp(0.0, max_scroll);
            follow - scroll.from_bottom
        }
    };
    // The last line starting at or above the top of the view.
    let first = tops[..lines]
        .partition_point(|&t| t <= top)
        .saturating_sub(1);
    if scroll.from_bottom <= 0.0 {
        scroll = Scroll::default();
    } else {
        scroll.anchor = Some((first, top - tops[first]));
    }

    // The lines that intersect [top, top + view).
    let bottom = top + flow.view;
    let mut out = Composed {
        first,
        content,
        max_scroll,
        scroll,
        ..Composed::default()
    };
    let mut line = first;
    while line < lines && tops[line] < bottom {
        let height = tops[line + 1] - tops[line];
        let slot_top = tops[line] - top;
        out.slots.push(Slot {
            top: slot_top,
            height,
        });
        if height > 0.0
            && let Some(b) = flow.blocks.iter().position(|b| b.line == line)
        {
            out.blocks.push((b, slot_top));
        }
        line += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `history` lines of scrollback over a `rows`-row screen, the tail at the screen's last row.
    fn flow(history: usize, rows: usize, view: f32, blocks: &[Block]) -> Flow<'_> {
        Flow {
            lines: history + rows,
            screen_top: history,
            tail: history + rows - 1,
            cell: 10.0,
            view,
            blocks,
            hide_from: None,
        }
    }

    #[test]
    fn a_plain_grid_follows_the_tail() {
        let c = compose(&flow(30, 24, 240.0, &[]), Scroll::default());
        assert_eq!(c.first, 30);
        assert_eq!(c.slots.len(), 24);
        assert!(c.is_uniform(10.0));
        assert_eq!(c.max_scroll, 300.0);
        assert!(c.scroll.following());
    }

    #[test]
    fn blocks_take_their_height_in_place_of_the_anchor_row() {
        let blocks = [Block {
            line: 50,
            height: 35.0,
        }];
        let c = compose(&flow(30, 24, 240.0, &blocks), Scroll::default());
        // The content is 25 px taller, so the view starts further down.
        assert_eq!(c.content, 565.0);
        let block_slot = c.slot_of(50).unwrap();
        assert_eq!(block_slot.height, 35.0);
        assert_eq!(c.slot_of(51).unwrap().top, block_slot.top + 35.0);
        assert_eq!(c.blocks, vec![(0, block_slot.top)]);
        // The tail (the last row) ends at the bottom of the view.
        let last = c.slots.last().unwrap();
        assert_eq!(last.top + last.height, 240.0);
        assert!(!c.is_uniform(10.0));
        assert_eq!(
            c.hit(block_slot.top + 1.0, &blocks),
            Some(Hit::Block(0, 1.0))
        );
        assert_eq!(
            c.hit(block_slot.top + 36.0, &blocks),
            Some(Hit::Line(51, 1.0))
        );
    }

    #[test]
    fn blank_rows_below_the_tail_give_way_first() {
        // A 24-row screen; a 60 px block at row 3; the cursor (the tail) on row 6.
        let blocks = [Block {
            line: 3,
            height: 60.0,
        }];
        let f = Flow {
            tail: 6,
            ..flow(0, 24, 240.0, &blocks)
        };
        let c = compose(&f, Scroll::default());
        // Everything down to the tail fits: the screen shows from its top row and the blank rows
        // at the bottom fall off.
        assert_eq!(c.first, 0);
        assert_eq!(c.slots[0].top, 0.0);
        assert_eq!(c.max_scroll, 0.0);
        assert_eq!(c.slots.len(), 19);
        // A block taller than the view below the screen's top: the tail stays pinned at the
        // bottom.
        let blocks = [Block {
            line: 20,
            height: 300.0,
        }];
        let f = Flow {
            tail: 21,
            ..flow(5, 24, 240.0, &blocks)
        };
        let c = compose(&f, Scroll::default());
        let tail = c.slot_of(21).unwrap();
        assert_eq!(tail.top + tail.height, 240.0);
        assert_eq!(c.blocks.len(), 1);
    }

    #[test]
    fn scrolled_views_stay_anchored_when_content_above_grows() {
        let c = compose(
            &flow(30, 24, 240.0, &[]),
            Scroll {
                from_bottom: 100.0,
                anchor: None,
            },
        );
        assert_eq!(c.first, 20);
        let anchored = c.scroll;
        assert_eq!(anchored.anchor, Some((20, 0.0)));
        // A block above the view grows by 50 px: line 20 stays at the top.
        let blocks = [Block {
            line: 5,
            height: 60.0,
        }];
        let c2 = compose(&flow(30, 24, 240.0, &blocks), anchored);
        assert_eq!(c2.first, 20);
        assert_eq!(c2.slots[0].top, 0.0);
        assert_eq!(c2.scroll.from_bottom, 100.0);
        // New output below doesn't move the view either.
        let c3 = compose(&flow(36, 24, 240.0, &blocks), c2.scroll);
        assert_eq!(c3.first, 20);
        assert_eq!(c3.scroll.from_bottom, 160.0);
        // Scrolling back down past the end follows again.
        let c4 = compose(&flow(36, 24, 240.0, &blocks), Scroll::by(&c3, -1000.0));
        assert!(c4.scroll.following());
        assert_eq!(c4.first, 36);
    }

    #[test]
    fn hidden_lines_take_no_space_and_short_content_starts_at_the_top() {
        let blocks = [Block {
            line: 1,
            height: 50.0,
        }];
        let f = Flow {
            hide_from: Some(2),
            tail: 1,
            ..flow(0, 24, 240.0, &blocks)
        };
        let c = compose(&f, Scroll::default());
        assert_eq!(c.content, 60.0);
        assert_eq!(c.first, 0);
        assert_eq!(c.slots[0].top, 0.0);
        assert_eq!(c.slots[1].height, 50.0);
        assert!(c.slots[2..].iter().all(|s| s.height == 0.0));
        assert_eq!(c.max_scroll, 0.0);
    }

    #[test]
    fn partial_rows_at_the_top_when_scrolled_by_pixels() {
        let c = compose(
            &flow(30, 24, 240.0, &[]),
            Scroll {
                from_bottom: 15.0,
                anchor: None,
            },
        );
        assert_eq!(c.first, 28);
        assert_eq!(c.slots[0].top, -5.0);
        assert_eq!(c.slots.len(), 25);
        assert_eq!(c.scroll.anchor, Some((28, 5.0)));
    }

    #[test]
    fn the_tail_is_the_lowest_of_cursor_written_rows_and_blocks() {
        let mut term = Terminal::new(20, 6, auk_term::Config::default());
        term.feed(b"a\r\nb\x1b[5;1Hz\x1b[2;1H");
        // The cursor is on row 1, but row 4 has text.
        assert_eq!(tail_line(&term, &[]), 4);
        assert_eq!(
            tail_line(
                &term,
                &[Block {
                    line: 5,
                    height: 1.0
                }]
            ),
            5
        );
        assert_eq!(
            tail_line(
                &term,
                &[Block {
                    line: 99,
                    height: 1.0
                }]
            ),
            5
        );
    }
}
