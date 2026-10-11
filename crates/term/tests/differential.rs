//! Differential tests against `alacritty_terminal` 0.26, the reference grid: the same bytes
//! and resizes go to both, and the visible cells (text, colors, attributes, links, wide-character
//! kinds, wrap flags), the cursor and the scrollback must match.
//!
//! # Intended differences
//!
//! Auk deliberately differs from `alacritty_terminal` in these places. The stream generator
//! avoids the cases (see `Gen::allowed`), or the harness applies the same rule to the
//! reference grid (`repair_reference`), and each says why:
//!
//! 1. **DCH past the end of the row** (`CSI n P` with `n` ≥ the cells right of the cursor):
//!    alacritty clamps against the whole width and then clears the entire row; Auk (like
//!    xterm) clears from the cursor to the end.
//! 2. **ED 1 with the cursor on row 1**: alacritty skips clearing row 0 (`cursor.line > 1`);
//!    Auk clears every row above the cursor, as ED 1 says.
//! 3. **CUU/CUD/CNL/CPL with a scroll region**: Auk stops at the margins as VT100 and xterm do;
//!    alacritty ignores them (and in origin mode adds the region's top twice).
//! 4. **CHA/HPA in origin mode** with a region not starting at the top: alacritty treats the
//!    cursor's absolute row as region-relative and moves it down; Auk keeps the row.
//! 5. **CBT while a wrap is pending**: alacritty keeps the pending wrap after moving left, so the
//!    next character wraps from the middle of the row; Auk (like xterm) clears it.
//! 6. **Broken wide characters**: when ICH, DCH, ECH, EL, insert-mode printing or cutting the
//!    alternate screen on resize separates a wide character from its spacer, Auk blanks the
//!    other half (as xterm and Ghostty do); alacritty leaves an orphan half that later writes
//!    misbehave around. The harness applies Auk's rule to the reference after every step, and
//!    also drops wrap flags that alacritty leaves on cells that are no longer last in the row.
//! 7. **VS16** (U+FE0F) after a character with an emoji presentation widens it to two cells
//!    (as kitty, WezTerm and Ghostty do); alacritty keeps it narrow. Not generated here; see
//!    `vs16_widens_emoji` in `tests/terminal.rs`.
//! 8. **Tabs** don't store a `\t` in the cells they cross (alacritty does, for copying); the
//!    harness compares such cells as blank.
//! 9. **Modes alacritty lacks**: 47/1047/1048 (alternate screen without clearing, save cursor),
//!    LNM (mode 20), DECST8C (`CSI ? 5 W`) and DECSTR (`CSI ! p`) are implemented by Auk and not
//!    generated here; SGR 21 is double underline (ECMA-48) rather than "not bold"; blink and
//!    overline are kept as attributes (alacritty drops them, so they aren't compared).
//! 10. **Anchor rows and TSP** have no counterpart in alacritty and are tested on their own.
//! 11. **Rows reset after reflow**: alacritty tracks how many cells of a row may be non-blank
//!     (`occ`) and resets only those, but reflow leaves that count too low, so a later reset
//!     (scrolling, ED, RIS) can leave old cells behind. The harness marks every row of the
//!     reference's active grid as fully occupied after each step; for the inactive main grid,
//!     which it can't reach, RIS is not generated while the alternate screen is active.
//! 12. **A wrap flag left in the middle of a row**: when widening pads a wrapped row whose
//!     continuation was cut off, Auk drops the flag, while alacritty keeps it on the cell and can
//!     revive it on a later widening. The harness drops it from the reference's active grid
//!     (difference 6); for the inactive main grid it allows one resize per alternate-screen
//!     session, so the reference is repaired before it reflows again.

// Uses dev-dependencies that don't build for wasm32 (see Cargo.toml).
#![cfg(not(target_family = "wasm"))]

use proptest::prelude::*;

mod common;
use common::grammar::{Step, steps};
use common::reference::{Pair, allowed};

fn run(cols: u16, rows: u16, scrollback: usize, steps: &[Step]) -> Result<(), TestCaseError> {
    let mut pair = Pair::new(cols, rows, scrollback);
    let mut log = Vec::new();
    for step in steps {
        match step {
            Step::Bytes(bytes) => {
                if !allowed(&pair, bytes) {
                    continue;
                }
                log.push(format!("{:?}", String::from_utf8_lossy(bytes)));
                pair.feed(bytes);
            }
            Step::Resize(c, r) => {
                if pair.auk.modes().alt_screen {
                    if pair.resized_in_alt {
                        continue;
                    }
                    pair.resized_in_alt = true;
                }
                log.push(format!("resize {c}x{r}"));
                pair.resize(*c, *r);
            }
        }
        if !pair.auk.modes().alt_screen {
            pair.resized_in_alt = false;
        }
        if let Err(e) = pair.compare() {
            return Err(TestCaseError::fail(format!(
                "{e}\nafter:\n{}",
                log.join("\n")
            )));
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 10000, failure_persistence: None, ..ProptestConfig::default() })]

    #[test]
    fn grids_match_reference(cols in 2u16..20, rows in 1u16..8, scrollback in 0usize..20, steps in steps()) {
        run(cols, rows, scrollback, &steps)?;
    }
}

/// Plain shell-like output with long wrapped lines, then reflow through many widths.
#[test]
fn reflow_matches_reference() {
    let mut text = Vec::new();
    for i in 0..60 {
        text.extend_from_slice(format!("line {i}: ").as_bytes());
        for j in 0..(i * 7 % 50) {
            text.extend_from_slice(if j % 11 == 3 {
                "中文".as_bytes()
            } else {
                b"ab "
            });
        }
        text.extend_from_slice(b"\r\n");
    }
    for width in [80u16, 33, 7, 2, 3, 19, 120, 41, 13, 80] {
        for height in [24u16, 5, 1, 30] {
            let mut pair = Pair::new(80, 24, 100);
            pair.feed(&text);
            pair.feed(b"prompt$ partial");
            pair.resize(width, height);
            pair.compare()
                .unwrap_or_else(|e| panic!("{width}x{height}: {e}"));
            pair.resize(80, 24);
            pair.compare()
                .unwrap_or_else(|e| panic!("back from {width}x{height}: {e}"));
        }
    }
}
