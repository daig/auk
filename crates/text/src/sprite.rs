//! Box drawing (U+2500–257F) and block elements (U+2580–259F), drawn from the cell's geometry
//! instead of a font, so lines meet across cells and blocks fill them exactly.

use crate::CellMetrics;
use crate::canvas::{Canvas, GlyphFormat, GlyphImage};

/// An arm's weight.
const N: u8 = 0;
const L: u8 = 1;
const H: u8 = 2;
const D: u8 = 3;

/// Arms (up, right, down, left) of U+2500–254B and U+2550–256C, by offset from U+2500.
/// Dashed lines (U+2504–250B, 254C–254F) are listed as plain lines and dashed separately.
const ARMS: [[u8; 4]; 0x80] = [
    [N, L, N, L],
    [N, H, N, H],
    [L, N, L, N],
    [H, N, H, N], // ─ ━ │ ┃
    [N, L, N, L],
    [N, H, N, H],
    [L, N, L, N],
    [H, N, H, N], // ┄ ┅ ┆ ┇
    [N, L, N, L],
    [N, H, N, H],
    [L, N, L, N],
    [H, N, H, N], // ┈ ┉ ┊ ┋
    [N, L, L, N],
    [N, H, L, N],
    [N, L, H, N],
    [N, H, H, N], // ┌ ┍ ┎ ┏
    [N, N, L, L],
    [N, N, L, H],
    [N, N, H, L],
    [N, N, H, H], // ┐ ┑ ┒ ┓
    [L, L, N, N],
    [L, H, N, N],
    [H, L, N, N],
    [H, H, N, N], // └ ┕ ┖ ┗
    [L, N, N, L],
    [L, N, N, H],
    [H, N, N, L],
    [H, N, N, H], // ┘ ┙ ┚ ┛
    [L, L, L, N],
    [L, H, L, N],
    [H, L, L, N],
    [L, L, H, N], // ├ ┝ ┞ ┟
    [H, L, H, N],
    [H, H, L, N],
    [L, H, H, N],
    [H, H, H, N], // ┠ ┡ ┢ ┣
    [L, N, L, L],
    [L, N, L, H],
    [H, N, L, L],
    [L, N, H, L], // ┤ ┥ ┦ ┧
    [H, N, H, L],
    [H, N, L, H],
    [L, N, H, H],
    [H, N, H, H], // ┨ ┩ ┪ ┫
    [N, L, L, L],
    [N, L, L, H],
    [N, H, L, L],
    [N, H, L, H], // ┬ ┭ ┮ ┯
    [N, L, H, L],
    [N, L, H, H],
    [N, H, H, L],
    [N, H, H, H], // ┰ ┱ ┲ ┳
    [L, L, N, L],
    [L, L, N, H],
    [L, H, N, L],
    [L, H, N, H], // ┴ ┵ ┶ ┷
    [H, L, N, L],
    [H, L, N, H],
    [H, H, N, L],
    [H, H, N, H], // ┸ ┹ ┺ ┻
    [L, L, L, L],
    [L, L, L, H],
    [L, H, L, L],
    [L, H, L, H], // ┼ ┽ ┾ ┿
    [H, L, L, L],
    [L, L, H, L],
    [H, L, H, L],
    [H, L, L, H], // ╀ ╁ ╂ ╃
    [H, H, L, L],
    [L, L, H, H],
    [L, H, H, L],
    [H, H, L, H], // ╄ ╅ ╆ ╇
    [L, H, H, H],
    [H, L, H, H],
    [H, H, H, L],
    [H, H, H, H], // ╈ ╉ ╊ ╋
    [N, L, N, L],
    [N, H, N, H],
    [L, N, L, N],
    [H, N, H, N], // ╌ ╍ ╎ ╏
    [N, D, N, D],
    [D, N, D, N],
    [N, D, L, N],
    [N, L, D, N], // ═ ║ ╒ ╓
    [N, D, D, N],
    [N, N, L, D],
    [N, N, D, L],
    [N, N, D, D], // ╔ ╕ ╖ ╗
    [L, D, N, N],
    [D, L, N, N],
    [D, D, N, N],
    [L, N, N, D], // ╘ ╙ ╚ ╛
    [D, N, N, L],
    [D, N, N, D],
    [L, D, L, N],
    [D, L, D, N], // ╜ ╝ ╞ ╟
    [D, D, D, N],
    [L, N, L, D],
    [D, N, D, L],
    [D, N, D, D], // ╠ ╡ ╢ ╣
    [N, D, L, D],
    [N, L, D, L],
    [N, D, D, D],
    [L, D, N, D], // ╤ ╥ ╦ ╧
    [D, L, N, L],
    [D, D, N, D],
    [L, D, L, D],
    [D, L, D, L], // ╨ ╩ ╪ ╫
    [D, D, D, D],
    [N, N, N, N],
    [N, N, N, N],
    [N, N, N, N], // ╬ ╭ ╮ ╯
    [N, N, N, N],
    [N, N, N, N],
    [N, N, N, N],
    [N, N, N, N], // ╰ ╱ ╲ ╳
    [N, N, N, L],
    [L, N, N, N],
    [N, L, N, N],
    [N, N, L, N], // ╴ ╵ ╶ ╷
    [N, N, N, H],
    [H, N, N, N],
    [N, H, N, N],
    [N, N, H, N], // ╸ ╹ ╺ ╻
    [N, H, N, L],
    [L, N, H, N],
    [N, L, N, H],
    [H, N, L, N], // ╼ ╽ ╾ ╿
];

/// Whether `c` is drawn here rather than by a font.
pub(crate) fn is_sprite(c: char) -> bool {
    ('\u{2500}'..='\u{259F}').contains(&c)
}

/// Draws `c` across `span` cells. `None` for characters this module doesn't draw.
pub(crate) fn draw(c: char, m: &CellMetrics, span: u8) -> Option<GlyphImage> {
    if !is_sprite(c) {
        return None;
    }
    let mut p = Painter::new(
        m.cell_width as usize * usize::from(span.max(1)),
        m.cell_height as usize,
    );
    let offset = c as u32 - 0x2500;
    if offset >= 0x80 {
        p.block(c);
    } else {
        match c {
            '\u{2504}'..='\u{250B}' | '\u{254C}'..='\u{254F}' => p.dashed(c),
            '\u{256D}'..='\u{2570}' => p.arc(c),
            '\u{2571}'..='\u{2573}' => p.diagonals(c),
            _ => p.arms(ARMS[offset as usize]),
        }
    }
    p.canvas.into_glyph(0, 0)
}

struct Painter {
    canvas: Canvas,
    w: f32,
    h: f32,
    /// Light line thickness in pixels.
    light: f32,
}

impl Painter {
    fn new(w: usize, h: usize) -> Painter {
        let light = (w.min(h * 2) as f32 / 8.0).round().max(1.0);
        Painter {
            canvas: Canvas::new(w, h, GlyphFormat::Mask),
            w: w as f32,
            h: h as f32,
            light,
        }
    }

    fn thickness(&self, weight: u8) -> f32 {
        if weight == H {
            self.light * 2.0
        } else {
            self.light
        }
    }

    /// Fills [x0, x1) × [y0, y1) (pixel edges, rounded) with coverage `alpha`.
    fn rect(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, alpha: u8) {
        let cx = |v: f32| (v.round().max(0.0) as usize).min(self.canvas.width);
        let cy = |v: f32| (v.round().max(0.0) as usize).min(self.canvas.height);
        for y in cy(y0)..cy(y1) {
            for x in cx(x0)..cx(x1) {
                let d = &mut self.canvas.data[y * self.canvas.width + x];
                *d = (*d).max(alpha);
            }
        }
    }

    /// The pixel-aligned start of a line of thickness `t` centered on `center`.
    fn line_start(center: f32, t: f32) -> f32 {
        (center - t / 2.0).round()
    }

    fn arms(&mut self, [up, right, down, left]: [u8; 4]) {
        let (cx, cy) = ((self.w / 2.0).floor(), (self.h / 2.0).floor());
        let g = self.light; // gap between double strokes, and their offset from the center
        let horizontal_double = right == D || left == D;
        let vertical_double = up == D || down == D;
        // The widest perpendicular stroke, so single and heavy arms meet without notches.
        let vt = [up, down]
            .iter()
            .map(|&a| if a == N { 0.0 } else { self.thickness(a) })
            .fold(0.0, f32::max);
        let ht = [left, right]
            .iter()
            .map(|&a| if a == N { 0.0 } else { self.thickness(a) })
            .fold(0.0, f32::max);

        // Each arm runs from its near end (at or past the center, to join what it meets) to the
        // cell's edge.
        let span = |near: f32, edge: f32| {
            if near < edge {
                (near, edge)
            } else {
                (edge, near)
            }
        };
        for (weight, dir) in [(right, 1.0f32), (left, -1.0)] {
            if weight == N {
                continue;
            }
            let edge = if dir > 0.0 { self.w } else { 0.0 };
            if weight == D {
                for side in [-1.0f32, 1.0] {
                    // The stroke stops at a double perpendicular arm on its side, and runs on to
                    // the far stroke otherwise.
                    let perpendicular = if side < 0.0 { up } else { down };
                    let reach = if perpendicular == D { -g } else { g };
                    let (a, b) = span(cx - dir * (reach + self.light / 2.0), edge);
                    let y = Self::line_start(cy + side * g, self.light);
                    self.rect(a, y, b, y + self.light, 255);
                }
            } else {
                let t = self.thickness(weight);
                let reach = if vertical_double {
                    g + self.light / 2.0
                } else {
                    vt / 2.0
                };
                let (a, b) = span(cx - dir * reach, edge);
                let y = Self::line_start(cy, t);
                self.rect(a, y, b, y + t, 255);
            }
        }
        for (weight, dir) in [(down, 1.0f32), (up, -1.0)] {
            if weight == N {
                continue;
            }
            let edge = if dir > 0.0 { self.h } else { 0.0 };
            if weight == D {
                for side in [-1.0f32, 1.0] {
                    let perpendicular = if side < 0.0 { left } else { right };
                    let reach = if perpendicular == D { -g } else { g };
                    let (a, b) = span(cy - dir * (reach + self.light / 2.0), edge);
                    let x = Self::line_start(cx + side * g, self.light);
                    self.rect(x, a, x + self.light, b, 255);
                }
            } else {
                let t = self.thickness(weight);
                let reach = if horizontal_double {
                    g + self.light / 2.0
                } else {
                    ht / 2.0
                };
                let (a, b) = span(cy - dir * reach, edge);
                let x = Self::line_start(cx, t);
                self.rect(x, a, x + t, b, 255);
            }
        }
    }

    fn dashed(&mut self, c: char) {
        let (count, heavy, vertical) = match c {
            '\u{2504}' => (3, false, false),
            '\u{2505}' => (3, true, false),
            '\u{2506}' => (3, false, true),
            '\u{2507}' => (3, true, true),
            '\u{2508}' => (4, false, false),
            '\u{2509}' => (4, true, false),
            '\u{250A}' => (4, false, true),
            '\u{250B}' => (4, true, true),
            '\u{254C}' => (2, false, false),
            '\u{254D}' => (2, true, false),
            '\u{254E}' => (2, false, true),
            _ => (2, true, true),
        };
        let t = self.thickness(if heavy { H } else { L });
        let length = if vertical { self.h } else { self.w };
        let segment = length / count as f32;
        let gap = (segment / 3.0).round().max(1.0);
        for i in 0..count {
            let a = (i as f32 * segment + gap / 2.0).round();
            let b = ((i + 1) as f32 * segment - gap / 2.0).round();
            if vertical {
                let x = Self::line_start((self.w / 2.0).floor(), t);
                self.rect(x, a, x + t, b, 255);
            } else {
                let y = Self::line_start((self.h / 2.0).floor(), t);
                self.rect(a, y, b, y + t, 255);
            }
        }
    }

    /// Rounded corners: a quarter circle joining the two arms, with straight runs to the edges.
    fn arc(&mut self, c: char) {
        let t = self.light;
        let (cx, cy) = (
            (self.w / 2.0).floor() + t / 2.0,
            (self.h / 2.0).floor() + t / 2.0,
        );
        // Which way the arms go: (+1 right / -1 left, +1 down / -1 up).
        let (sx, sy) = match c {
            '\u{256D}' => (1.0f32, 1.0f32),
            '\u{256E}' => (-1.0, 1.0),
            '\u{256F}' => (-1.0, -1.0),
            _ => (1.0, -1.0),
        };
        let r = (self.w / 2.0).min(self.h / 2.0);
        // The circle's center sits r into the quadrant the arms open toward.
        let (ox, oy) = (cx + sx * r, cy + sy * r);
        for y in 0..self.canvas.height {
            for x in 0..self.canvas.width {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                // Only the quadrant between the center and the circle's center.
                let in_quadrant = (px - ox) * sx <= 0.0 && (py - oy) * sy <= 0.0;
                let coverage = if in_quadrant {
                    let d = ((px - ox).powi(2) + (py - oy).powi(2)).sqrt();
                    (t / 2.0 + 0.5 - (d - r).abs()).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let v = (coverage * 255.0).round() as u8;
                let cell = &mut self.canvas.data[y * self.canvas.width + x];
                *cell = (*cell).max(v);
            }
        }
        // Straight runs from the arc's ends to the cell's edges.
        let hy = Self::line_start(cy - t / 2.0, t);
        if sx > 0.0 {
            self.rect(ox, hy, self.w, hy + t, 255);
        } else {
            self.rect(0.0, hy, ox, hy + t, 255);
        }
        let vx = Self::line_start(cx - t / 2.0, t);
        if sy > 0.0 {
            self.rect(vx, oy, vx + t, self.h, 255);
        } else {
            self.rect(vx, 0.0, vx + t, oy, 255);
        }
    }

    fn diagonals(&mut self, c: char) {
        let t = self.light;
        let rising = c != '\u{2572}';
        let falling = c != '\u{2571}';
        let len = (self.w * self.w + self.h * self.h).sqrt();
        for y in 0..self.canvas.height {
            for x in 0..self.canvas.width {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let mut coverage: f32 = 0.0;
                // Distance to the line from (0, h) to (w, 0), and from (0, 0) to (w, h).
                if rising {
                    let d = (self.h * px + self.w * py - self.w * self.h).abs() / len;
                    coverage = coverage.max((t / 2.0 + 0.5 - d).clamp(0.0, 1.0));
                }
                if falling {
                    let d = (self.h * px - self.w * py).abs() / len;
                    coverage = coverage.max((t / 2.0 + 0.5 - d).clamp(0.0, 1.0));
                }
                self.canvas.data[y * self.canvas.width + x] = (coverage * 255.0).round() as u8;
            }
        }
    }

    fn block(&mut self, c: char) {
        let (w, h) = (self.w, self.h);
        let eighth_h = |n: f32| (h * n / 8.0).round();
        let eighth_w = |n: f32| (w * n / 8.0).round();
        let (hw, hh) = ((w / 2.0).round(), (h / 2.0).round());
        let quadrants = |p: &mut Painter, ul: bool, ur: bool, ll: bool, lr: bool| {
            if ul {
                p.rect(0.0, 0.0, hw, hh, 255);
            }
            if ur {
                p.rect(hw, 0.0, w, hh, 255);
            }
            if ll {
                p.rect(0.0, hh, hw, h, 255);
            }
            if lr {
                p.rect(hw, hh, w, h, 255);
            }
        };
        match c {
            '\u{2580}' => self.rect(0.0, 0.0, w, hh, 255),
            '\u{2581}'..='\u{2588}' => {
                let n = (c as u32 - 0x2580) as f32;
                self.rect(0.0, h - eighth_h(n), w, h, 255);
            }
            '\u{2589}'..='\u{258F}' => {
                let n = (0x2590 - c as u32) as f32;
                self.rect(0.0, 0.0, eighth_w(n), h, 255);
            }
            '\u{2590}' => self.rect(hw, 0.0, w, h, 255),
            '\u{2591}' => self.rect(0.0, 0.0, w, h, 64),
            '\u{2592}' => self.rect(0.0, 0.0, w, h, 128),
            '\u{2593}' => self.rect(0.0, 0.0, w, h, 191),
            '\u{2594}' => self.rect(0.0, 0.0, w, eighth_h(1.0), 255),
            '\u{2595}' => self.rect(w - eighth_w(1.0), 0.0, w, h, 255),
            '\u{2596}' => quadrants(self, false, false, true, false),
            '\u{2597}' => quadrants(self, false, false, false, true),
            '\u{2598}' => quadrants(self, true, false, false, false),
            '\u{2599}' => quadrants(self, true, false, true, true),
            '\u{259A}' => quadrants(self, true, false, false, true),
            '\u{259B}' => quadrants(self, true, true, true, false),
            '\u{259C}' => quadrants(self, true, true, false, true),
            '\u{259D}' => quadrants(self, false, true, false, false),
            '\u{259E}' => quadrants(self, false, true, true, false),
            _ => quadrants(self, false, true, true, true),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: CellMetrics = CellMetrics {
        cell_width: 16,
        cell_height: 32,
        baseline: 24,
        underline_position: 26,
        underline_thickness: 2,
        strikeout_position: 16,
        strikeout_thickness: 2,
    };

    /// The full cell-sized coverage of `c`.
    fn cell(c: char) -> Vec<Vec<u8>> {
        let mut grid = vec![vec![0u8; 16]; 32];
        if let Some(g) = draw(c, &M, 1) {
            for y in 0..g.height {
                for x in 0..g.width {
                    let (cx, cy) = (g.left + x as i32, g.top + y as i32);
                    grid[cy as usize][cx as usize] = g.data[(y * g.width + x) as usize];
                }
            }
        }
        grid
    }

    fn inked_cols(rows: &[Vec<u8>], y: usize) -> Vec<usize> {
        (0..16).filter(|&x| rows[y][x] > 0).collect()
    }

    #[test]
    fn horizontal_and_vertical_lines_reach_the_edges() {
        let h = cell('─');
        let rows: Vec<usize> = (0..32).filter(|&y| h[y].iter().any(|&v| v > 0)).collect();
        assert_eq!(rows, [15, 16]);
        assert_eq!(inked_cols(&h, 15), (0..16).collect::<Vec<_>>());
        let v = cell('│');
        assert!((0..32).all(|y| inked_cols(&v, y) == [7, 8]));
        // Heavy is twice as thick.
        let heavy = cell('━');
        assert_eq!((0..32).filter(|&y| heavy[y][0] > 0).count(), 4);
    }

    #[test]
    fn corners_join_and_stop_at_the_center() {
        let c = cell('┌');
        // Right arm reaches the right edge, down arm the bottom; nothing goes up or left.
        assert!(c[15][15] > 0 && c[31][7] > 0);
        assert_eq!(c[0][7], 0);
        assert_eq!(c[15][0], 0);
        // The joint is filled: no notch at the corner.
        assert!(c[15][7] > 0 && c[16][8] > 0);
    }

    #[test]
    fn crosses_and_tees() {
        let x = cell('┼');
        assert!(x[0][7] > 0 && x[31][7] > 0 && x[15][0] > 0 && x[15][15] > 0);
        let t = cell('┤');
        assert!(t[15][0] > 0 && t[15][15] == 0);
    }

    #[test]
    fn double_lines_have_two_strokes() {
        let d = cell('═');
        let rows: Vec<usize> = (0..32).filter(|&y| d[y][0] > 0).collect();
        assert_eq!(rows.len(), 4, "{rows:?}");
        assert!(
            rows.windows(2).any(|w| w[1] > w[0] + 1),
            "a gap between strokes: {rows:?}"
        );
        // ╬ leaves the center open.
        let x = cell('╬');
        assert_eq!(x[15][7], 0);
    }

    #[test]
    fn blocks_fill_exact_fractions() {
        let full = cell('█');
        assert!(full.iter().all(|row| row.iter().all(|&v| v == 255)));
        let lower = cell('▄');
        assert!((0..16).all(|y| lower[y].iter().all(|&v| v == 0)));
        assert!((16..32).all(|y| lower[y].iter().all(|&v| v == 255)));
        let left = cell('▌');
        assert!(
            left.iter()
                .all(|row| row[..8].iter().all(|&v| v == 255) && row[8..].iter().all(|&v| v == 0))
        );
        let eighth = cell('▁');
        assert_eq!((0..32).filter(|&y| eighth[y][0] == 255).count(), 4);
        let shade = cell('▒');
        assert!(shade.iter().all(|row| row.iter().all(|&v| v == 128)));
        let quads = cell('▚');
        assert!(
            quads[0][0] == 255 && quads[0][15] == 0 && quads[31][0] == 0 && quads[31][15] == 255
        );
    }

    #[test]
    fn arcs_and_diagonals_have_ink_where_expected() {
        let arc = cell('╭');
        assert!(arc[15][15] > 0 || arc[16][15] > 0, "reaches the right edge");
        assert!(arc[31][7] > 0 || arc[31][8] > 0, "reaches the bottom edge");
        assert_eq!(arc[0][0], 0);
        let diag = cell('╱');
        assert!(diag[31][0] > 0 && diag[0][15] > 0 && diag[0][0] == 0);
        let dashed = cell('┄');
        let row = (0..32).find(|&y| dashed[y].iter().any(|&v| v > 0)).unwrap();
        let runs = (1..16)
            .filter(|&x| (dashed[row][x] > 0) != (dashed[row][x - 1] > 0))
            .count();
        assert!(runs >= 4, "{runs}");
    }

    #[test]
    fn everything_in_range_draws() {
        for c in '\u{2500}'..='\u{259F}' {
            assert!(draw(c, &M, 1).is_some(), "U+{:04X}", c as u32);
        }
        assert!(draw('a', &M, 1).is_none());
    }
}
