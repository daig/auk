//! The pane compositor (§6.2): a pane draws runs of grid rows at a fixed height, with surface
//! blocks at their natural height in between, each in place of its anchor row. A dock sits at
//! the pane's bottom, a layer overlays everything, and a screen surface covers the pane.
//!
//! [`compose`] is pure: it takes the pane's line count, the blocks and the scroll state, and
//! returns which buffer lines to draw, where each row goes, and where each visible block goes.
//! Selection, mouse reports and links map points through the same result ([`Composed::hit`]).
//!
//! Scrolling is in pixels. While the view follows the tail, the bottom of the content stays at
//! the bottom of the view. Once scrolled up, the view is anchored to the line at its top, so
//! content that changes height above or below keeps what is visible in place.

use auk_render::RowSlot;
use auk_term::Line;

/// A surface block in the pane's flow: shown in place of its anchor row.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Block {
    /// The anchor row's buffer line.
    pub line: Line,
    /// The block's natural height, in pixels.
    pub height: f32,
}

/// What a pane's compositor works from.
#[derive(Clone, Debug, PartialEq)]
pub struct Input<'a> {
    /// Lines in the buffer: scrollback plus the screen.
    pub lines: usize,
    /// Cell height, in pixels.
    pub cell: f32,
    /// The height available for the flow (the pane's grid area less the dock), in pixels.
    pub view: f32,
    /// Blocks, in line order.
    pub blocks: &'a [Block],
    /// Lines from here on are hidden: stray output under a live inline surface (§7.3).
    pub hide_from: Option<Line>,
}

/// The view's scroll state, kept per pane by the window (view state, never replicated).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Scroll {
    /// Pixels between the bottom of the content and the bottom of the view; 0 follows the tail.
    pub from_bottom: f32,
    /// While scrolled up: the line at the top of the view and how far into it the view starts.
    pub anchor: Option<(Line, f32)>,
}

impl Scroll {
    pub fn following(&self) -> bool {
        self.anchor.is_none()
    }
}

/// Where a pane's content goes this frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Composed {
    /// The first buffer line drawn.
    pub first: Line,
    /// Each drawn row's slot, relative to the top of the flow area; row `i` is line
    /// `first + i`.
    pub slots: Vec<RowSlot>,
    /// Visible blocks: (index into the input blocks, top relative to the flow area).
    pub blocks: Vec<(usize, f32)>,
    /// The content's total height.
    pub content: f32,
    /// How far the view can scroll up.
    pub max_scroll: f32,
    /// The scroll state, after anchoring.
    pub scroll: Scroll,
}

/// What lies under a point in the flow.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    /// A grid row: the buffer line, and the point's offset into the row.
    Line(Line, f32),
    /// A block (index into the input blocks), and the point's offset into it.
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
    pub fn slot_of(&self, line: Line) -> Option<RowSlot> {
        line.checked_sub(self.first)
            .and_then(|i| self.slots.get(i))
            .copied()
    }
}

fn line_height(input: &Input<'_>, line: Line) -> f32 {
    if input.hide_from.is_some_and(|h| line >= h) {
        return 0.0;
    }
    match input.blocks.iter().find(|b| b.line == line) {
        Some(b) => b.height.max(0.0),
        None => input.cell,
    }
}

/// Lays out the flow. `scroll` is the window's scroll state; the result carries it back after
/// anchoring (content above the anchor that changed height keeps the anchored line in place).
pub fn compose(input: &Input<'_>, scroll: Scroll) -> Composed {
    // Line tops, computed once.
    let mut tops = Vec::with_capacity(input.lines + 1);
    let mut y = 0.0f32;
    for line in 0..input.lines {
        tops.push(y);
        y += line_height(input, line);
    }
    tops.push(y);
    let content = y;
    let max_scroll = (content - input.view).max(0.0);

    // Where the top of the view is, in content pixels.
    let mut scroll = scroll;
    let top = match scroll.anchor {
        Some((line, offset)) if line < input.lines => {
            let top = (tops[line] + offset).clamp(0.0, max_scroll);
            scroll.from_bottom = max_scroll - top;
            top
        }
        _ => {
            let from_bottom = scroll.from_bottom.clamp(0.0, max_scroll);
            scroll.from_bottom = from_bottom;
            max_scroll - from_bottom
        }
    };
    // The last line starting at or above the top of the view.
    let first = tops[..input.lines]
        .partition_point(|&t| t <= top)
        .saturating_sub(1);
    if scroll.from_bottom <= 0.0 {
        scroll = Scroll::default();
    } else {
        scroll.anchor = Some((first, top - tops[first]));
    }

    // The lines that intersect [top, top + view).
    let bottom = top + input.view;
    let mut out = Composed {
        first,
        content,
        max_scroll,
        scroll,
        ..Composed::default()
    };
    let mut line = first;
    while line < input.lines && tops[line] < bottom {
        let height = tops[line + 1] - tops[line];
        let slot_top = tops[line] - top;
        out.slots.push(RowSlot {
            top: slot_top,
            height,
        });
        if let Some(b) = input.blocks.iter().position(|b| b.line == line)
            && height > 0.0
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

    fn input(lines: usize, view: f32, blocks: &[Block]) -> Input<'_> {
        Input {
            lines,
            cell: 10.0,
            view,
            blocks,
            hide_from: None,
        }
    }

    #[test]
    fn a_plain_grid_follows_the_tail() {
        // 30 lines of scrollback plus a 24-row screen, viewed at 24 rows.
        let c = compose(&input(54, 240.0, &[]), Scroll::default());
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
        let c = compose(&input(54, 240.0, &blocks), Scroll::default());
        // The content is 25 px taller, so the view starts further down.
        assert_eq!(c.content, 565.0);
        let block_slot = c.slot_of(50).unwrap();
        assert_eq!(block_slot.height, 35.0);
        assert_eq!(c.slot_of(51).unwrap().top, block_slot.top + 35.0);
        assert_eq!(c.blocks, vec![(0, block_slot.top)]);
        // The last row ends at the bottom of the view.
        let last = c.slots.last().unwrap();
        assert_eq!(last.top + last.height, 240.0);
        assert!(!c.is_uniform(10.0));
        assert_eq!(c.hit(block_slot.top + 1.0, &blocks), Some(Hit::Block(0, 1.0)));
        assert_eq!(
            c.hit(block_slot.top + 36.0, &blocks),
            Some(Hit::Line(51, 1.0))
        );
    }

    #[test]
    fn scrolled_views_stay_anchored_when_content_above_grows() {
        let c = compose(
            &input(54, 240.0, &[]),
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
        let c2 = compose(&input(54, 240.0, &blocks), anchored);
        assert_eq!(c2.first, 20);
        assert_eq!(c2.slots[0].top, 0.0);
        assert_eq!(c2.scroll.from_bottom, 100.0);
        // New output below doesn't move the view either.
        let c3 = compose(&input(60, 240.0, &blocks), c2.scroll);
        assert_eq!(c3.first, 20);
        assert_eq!(c3.scroll.from_bottom, 160.0);
    }

    #[test]
    fn hidden_lines_take_no_space_and_short_content_starts_at_the_top() {
        let blocks = [Block {
            line: 1,
            height: 50.0,
        }];
        let i = Input {
            hide_from: Some(2),
            ..input(24, 240.0, &blocks)
        };
        let c = compose(&i, Scroll::default());
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
            &input(54, 240.0, &[]),
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
}
