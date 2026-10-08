//! Offscreen tests of the grid pass: synthetic grids rendered to images, checked pixel by pixel
//! against the cell geometry and colors they were built from. Each test also writes its image to
//! `$CARGO_TARGET_TMPDIR/grid-*.png` for inspection.

use std::sync::OnceLock;

use auk_render::{
    Attrs, CursorStyle, Gpu, GridRenderer, GridSnapshot, Image, OFFSCREEN_FORMAT, Rgba, SnapCursor,
    Theme, render_to_image,
};
use auk_text::{CellMetrics, FontConfig, GridFonts};

fn gpu() -> &'static Gpu {
    static GPU: OnceLock<Gpu> = OnceLock::new();
    GPU.get_or_init(|| Gpu::new_blocking().expect("a Metal adapter"))
}

fn renderer() -> GridRenderer {
    let fonts = GridFonts::new(&FontConfig {
        size_pt: 13.0,
        scale: 2.0,
        ..FontConfig::default()
    })
    .expect("system monospace font");
    GridRenderer::new(gpu(), OFFSCREEN_FORMAT, fonts)
}

const PAD: u32 = 6;

/// Renders with the grid at (`PAD`, `PAD`) in an image just large enough for it.
fn render(r: &mut GridRenderer, snap: &GridSnapshot, name: &str) -> Image {
    let m = r.metrics();
    let w = u32::from(snap.cols) * m.cell_width + 2 * PAD;
    let h = u32::from(snap.rows) * m.cell_height + 2 * PAD;
    r.set_origin(PAD as f32, PAD as f32);
    let image = render_to_image(gpu(), r, snap, w, h).expect("readback");
    let dir = env!("CARGO_TARGET_TMPDIR");
    image
        .save_png(format!("{dir}/grid-{name}.png"))
        .expect("write png");
    image
}

/// The pixel rectangle of cells `col..col+span` in `row`: (x0, y0, x1, y1), exclusive ends.
fn cell_rect(m: CellMetrics, col: u16, row: u16, span: u16) -> (u32, u32, u32, u32) {
    let x0 = PAD + u32::from(col) * m.cell_width;
    let y0 = PAD + u32::from(row) * m.cell_height;
    (
        x0,
        y0,
        x0 + u32::from(span) * m.cell_width,
        y0 + m.cell_height,
    )
}

/// Pixels in `rect` that differ from `bg`.
fn ink(image: &Image, rect: (u32, u32, u32, u32), bg: Rgba) -> Vec<(u32, u32, Rgba)> {
    let mut out = Vec::new();
    for y in rect.1..rect.3 {
        for x in rect.0..rect.2 {
            let p = image.pixel(x, y);
            if p != bg {
                out.push((x, y, p));
            }
        }
    }
    out
}

fn whole(image: &Image) -> (u32, u32, u32, u32) {
    (0, 0, image.width, image.height)
}

#[test]
fn backgrounds_fill_their_cells_exactly() {
    let theme = Theme::dark();
    let mut r = renderer();
    let m = r.metrics();
    let mut snap = GridSnapshot::new(8, 3, &theme);
    for row in 0..3u16 {
        for col in 0..8u16 {
            snap.cell_mut(col, row).bg = Rgba::rgb(col as u8 * 30, row as u8 * 80, 200);
        }
    }
    let image = render(&mut r, &snap, "backgrounds");
    for row in 0..3u16 {
        for col in 0..8u16 {
            let (x0, y0, x1, y1) = cell_rect(m, col, row, 1);
            let want = snap.row(row)[usize::from(col)].bg;
            // Every pixel of the cell, corners included.
            for (x, y) in [
                (x0, y0),
                (x1 - 1, y0),
                (x0, y1 - 1),
                (x1 - 1, y1 - 1),
                ((x0 + x1) / 2, (y0 + y1) / 2),
            ] {
                assert_eq!(image.pixel(x, y), want, "cell ({col},{row}) at ({x},{y})");
            }
        }
    }
    // The padding is the snapshot's background.
    assert_eq!(image.pixel(0, 0), theme.background);
    assert_eq!(
        image.pixel(image.width - 1, image.height - 1),
        theme.background
    );
}

#[test]
fn glyphs_land_in_their_cells_in_their_colors() {
    let theme = Theme::dark();
    let mut r = renderer();
    let m = r.metrics();
    let mut snap = GridSnapshot::new(10, 3, &theme);
    let red = Rgba::rgb(255, 40, 40);
    let green = Rgba::rgb(40, 255, 40);
    snap.write(3, 1, "A", red, theme.background, 0);
    snap.write(7, 2, "█", green, theme.background, 0);
    let image = render(&mut r, &snap, "glyphs");

    // All ink in the image belongs to the two cells.
    let a_cell = cell_rect(m, 3, 1, 1);
    let block_cell = cell_rect(m, 7, 2, 1);
    let all = ink(&image, whole(&image), theme.background);
    assert!(!all.is_empty());
    for &(x, y, p) in &all {
        let in_a = x >= a_cell.0 && x < a_cell.2 && y >= a_cell.1 && y < a_cell.3;
        let in_block =
            x >= block_cell.0 && x < block_cell.2 && y >= block_cell.1 && y < block_cell.3;
        assert!(in_a || in_block, "stray ink {p:?} at ({x},{y})");
    }
    // Fully covered pixels carry the exact foreground color.
    let a_ink = ink(&image, a_cell, theme.background);
    assert!(
        a_ink.iter().any(|&(_, _, p)| p == red),
        "no solid red in the A"
    );
    // The full block's interior is solid green.
    let (x0, y0, x1, y1) = block_cell;
    assert_eq!(image.pixel((x0 + x1) / 2, (y0 + y1) / 2), green);
    // Antialiased edges blend toward the background, never past either color.
    for &(_, _, p) in &a_ink {
        assert!(p.1 <= red.1.max(theme.background.1) + 1 && p.0 >= theme.background.0.min(red.0));
    }
}

#[test]
fn wide_characters_and_emoji_span_two_cells() {
    let theme = Theme::dark();
    let mut r = renderer();
    let m = r.metrics();
    let mut snap = GridSnapshot::new(10, 1, &theme);
    snap.write(0, 0, "漢", theme.foreground, theme.background, 0);
    snap.write(4, 0, "😀", theme.foreground, theme.background, 0);
    snap.write(8, 0, "x", theme.foreground, theme.background, 0);
    let image = render(&mut r, &snap, "wide");

    // The CJK glyph has ink in both of its cells and none outside them.
    let left = ink(&image, cell_rect(m, 0, 0, 1), theme.background);
    let right = ink(&image, cell_rect(m, 1, 0, 1), theme.background);
    assert!(
        left.len() > 20 && right.len() > 20,
        "{} {}",
        left.len(),
        right.len()
    );
    assert!(ink(&image, cell_rect(m, 2, 0, 2), theme.background).is_empty());

    // The emoji is in color (yellow, which no text color here is) across cells 4 and 5.
    let emoji = ink(&image, cell_rect(m, 4, 0, 2), theme.background);
    let yellow = |p: &Rgba| p.0 > 200 && p.1 > 150 && p.2 < 110;
    let in_left = emoji
        .iter()
        .filter(|(x, _, p)| *x < cell_rect(m, 5, 0, 1).0 && yellow(p))
        .count();
    let in_right = emoji
        .iter()
        .filter(|(x, _, p)| *x >= cell_rect(m, 5, 0, 1).0 && yellow(p))
        .count();
    assert!(in_left > 50 && in_right > 50, "{in_left} {in_right}");
    assert!(
        ink(&image, cell_rect(m, 6, 0, 2), theme.background)
            .iter()
            .all(|(x, _, _)| *x >= cell_rect(m, 8, 0, 1).0)
    );
}

#[test]
fn decorations_draw_at_the_font_positions() {
    let theme = Theme::dark();
    let mut r = renderer();
    let m = r.metrics();
    let deco = Rgba::rgb(255, 0, 255);
    let rows: [(u16, &str); 7] = [
        (Attrs::UNDERLINE, "underline"),
        (Attrs::DOUBLE_UNDERLINE, "double"),
        (Attrs::CURLY_UNDERLINE, "curly"),
        (Attrs::DOTTED_UNDERLINE, "dotted"),
        (Attrs::DASHED_UNDERLINE, "dashed"),
        (Attrs::STRIKE, "strike"),
        (Attrs::OVERLINE, "overline"),
    ];
    let mut snap = GridSnapshot::new(6, rows.len() as u16, &theme);
    for (i, (attr, _)) in rows.iter().enumerate() {
        // Blank cells: only the decoration draws.
        snap.write(0, i as u16, "      ", deco, theme.background, *attr);
    }
    let image = render(&mut r, &snap, "decorations");
    let t = m.underline_thickness;
    let row_of = |i: u16| cell_rect(m, 0, i, 6);
    let line_rows = |i: u16| -> Vec<u32> {
        let (x0, y0, x1, y1) = row_of(i);
        (y0..y1)
            .filter(|&y| {
                (x0..x1).filter(|&x| image.pixel(x, y) == deco).count() > ((x1 - x0) / 3) as usize
            })
            .map(|y| y - y0)
            .collect()
    };
    // Solid lines span the whole run at the font's positions.
    assert_eq!(
        line_rows(0),
        (m.underline_position..m.underline_position + t).collect::<Vec<_>>()
    );
    assert_eq!(
        line_rows(5),
        (m.strikeout_position..m.strikeout_position + m.strikeout_thickness).collect::<Vec<_>>()
    );
    assert_eq!(line_rows(6), (0..t).collect::<Vec<_>>());
    // Double: two separate lines.
    let double = line_rows(1);
    assert_eq!(double.len() as u32, 2 * t, "{double:?}");
    assert!(double.windows(2).any(|w| w[1] > w[0] + 1), "{double:?}");
    // Curly: ink at several heights, varying along x.
    let (x0, y0, x1, y1) = row_of(2);
    let heights: std::collections::BTreeSet<u32> = (y0..y1)
        .filter(|&y| (x0..x1).any(|x| image.pixel(x, y) != theme.background))
        .collect();
    assert!(heights.len() as u32 > 2 * t, "{heights:?}");
    // Dotted and dashed: the line's row alternates between ink and gaps.
    for i in [3u16, 4] {
        let (x0, y0, x1, _) = row_of(i);
        let y = y0 + m.underline_position;
        let on = (x0..x1).filter(|&x| image.pixel(x, y) == deco).count();
        let off = (x0..x1)
            .filter(|&x| image.pixel(x, y) == theme.background)
            .count();
        assert!(on > 0 && off > 0, "row {i}: {on} on, {off} off");
        let runs = (x0 + 1..x1)
            .filter(|&x| (image.pixel(x, y) == deco) != (image.pixel(x - 1, y) == deco))
            .count();
        assert!(runs >= 4, "row {i}: {runs} transitions");
    }
}

#[test]
fn cursor_shapes() {
    let theme = Theme::dark();
    let mut r = renderer();
    let m = r.metrics();
    let shapes = [
        CursorStyle::Block,
        CursorStyle::HollowBlock,
        CursorStyle::Bar,
        CursorStyle::Underline,
    ];
    for style in shapes {
        let mut snap = GridSnapshot::new(4, 1, &theme);
        snap.write(1, 0, "M", theme.foreground, theme.background, 0);
        snap.set_cursor(
            SnapCursor {
                col: 1,
                row: 0,
                width: 1,
                style,
                color: theme.cursor,
            },
            &theme,
        );
        let image = render(&mut r, &snap, &format!("cursor-{style:?}"));
        let (x0, y0, x1, y1) = cell_rect(m, 1, 0, 1);
        let (cx, cy) = (x0 + 1, (y0 + y1) / 2);
        match style {
            CursorStyle::Block => {
                // The cell is the cursor color and its glyph the cursor text color.
                assert_eq!(image.pixel(x0, y0), theme.cursor);
                assert_eq!(image.pixel(x1 - 1, y1 - 1), theme.cursor);
                let text = ink(&image, (x0, y0, x1, y1), theme.cursor);
                assert!(text.iter().any(|&(_, _, p)| p == theme.cursor_text));
            }
            CursorStyle::HollowBlock => {
                for (x, y) in [
                    (x0, y0),
                    (x1 - 1, y0),
                    (x0, y1 - 1),
                    (x1 - 1, y1 - 1),
                    (x0, cy),
                    (x1 - 1, cy),
                ] {
                    assert_eq!(image.pixel(x, y), theme.cursor, "({x},{y})");
                }
                assert_ne!(image.pixel((x0 + x1) / 2, y0 + 3), theme.cursor);
            }
            CursorStyle::Bar => {
                assert_eq!(image.pixel(x0, y0), theme.cursor);
                assert_eq!(image.pixel(x0, y1 - 1), theme.cursor);
                assert_eq!(image.pixel(cx, cy).3, 255);
                assert_ne!(image.pixel(x1 - 1, y0), theme.cursor);
            }
            CursorStyle::Underline => {
                assert_eq!(image.pixel(x0, y1 - 1), theme.cursor);
                assert_eq!(image.pixel(x1 - 1, y1 - 1), theme.cursor);
                assert_ne!(image.pixel(x0, y0), theme.cursor);
            }
        }
    }
}

#[test]
fn bold_italic_and_dim_render_differently() {
    let theme = Theme::dark();
    let mut r = renderer();
    let m = r.metrics();
    let mut snap = GridSnapshot::new(4, 1, &theme);
    snap.write(0, 0, "H", theme.foreground, theme.background, 0);
    snap.write(1, 0, "H", theme.foreground, theme.background, Attrs::BOLD);
    snap.write(2, 0, "H", theme.foreground, theme.background, Attrs::ITALIC);
    snap.write(
        3,
        0,
        "H",
        theme.foreground.mix(theme.background, 0.4),
        theme.background,
        0,
    );
    let image = render(&mut r, &snap, "styles");
    let coverage = |col| -> u32 {
        ink(&image, cell_rect(m, col, 0, 1), theme.background)
            .iter()
            .map(|&(_, _, p)| u32::from(p.0.abs_diff(theme.background.0)))
            .sum()
    };
    let (regular, bold, dim) = (coverage(0), coverage(1), coverage(3));
    assert!(bold > regular * 11 / 10, "bold {bold} regular {regular}");
    assert!(dim < regular * 8 / 10, "dim {dim} regular {regular}");
    let shape = |col| -> Vec<(u32, u32)> {
        let (x0, y0, ..) = cell_rect(m, col, 0, 1);
        ink(&image, cell_rect(m, col, 0, 1), theme.background)
            .iter()
            .map(|&(x, y, _)| (x - x0, y - y0))
            .collect()
    };
    assert_ne!(shape(0), shape(2), "italic draws a different shape");
}

#[test]
fn row_damage_redraws_only_changed_rows_and_matches_a_full_redraw() {
    let theme = Theme::dark();
    let mut r = renderer();
    let mut snap = GridSnapshot::new(20, 6, &theme);
    for row in 0..6 {
        snap.write(
            0,
            row,
            &format!("row {row}: hello, world"),
            theme.foreground,
            theme.background,
            0,
        );
    }
    let first = r.prepare(gpu(), &snap, (800, 400));
    assert_eq!(first.damaged_rows, 6);
    let same = r.prepare(gpu(), &snap, (800, 400));
    assert_eq!(same.damaged_rows, 0);
    assert_eq!(same.rasterized, 0);

    snap.write(
        5,
        3,
        "CHANGED",
        Rgba::rgb(255, 200, 0),
        Rgba::rgb(0, 0, 90),
        Attrs::UNDERLINE,
    );
    let one = r.prepare(gpu(), &snap, (800, 400));
    assert_eq!(one.damaged_rows, 1);
    let incremental = render(&mut r, &snap, "damage-incremental");

    let mut fresh = renderer();
    let full = render(&mut fresh, &snap, "damage-full");
    assert!(incremental == full, "incremental and full redraws differ");
}

#[test]
fn atlas_growth_keeps_glyphs_correct() {
    // Enough distinct CJK glyphs (each ~50×50 px at 2x) to outgrow the initial 1024² atlas.
    let theme = Theme::dark();
    let mut r = renderer();
    let mut snap = GridSnapshot::new(80, 10, &theme);
    let mut ch = 0x4E00u32;
    let mut images = Vec::new();
    for frame in 0..2 {
        for row in 0..10u16 {
            let text: String = (0..40)
                .map(|_| {
                    ch += 1;
                    char::from_u32(ch).unwrap()
                })
                .collect();
            snap.write(0, row, &text, theme.foreground, theme.background, 0);
        }
        images.push(render(&mut r, &snap, &format!("atlas-{frame}")));
    }
    let mut fresh = renderer();
    let expected = render(&mut fresh, &snap, "atlas-fresh");
    assert!(
        images[1] == expected,
        "after growth the grid differs from a fresh render"
    );
}

#[test]
fn png_round_trip() {
    let theme = Theme::light();
    let mut r = renderer();
    let mut snap = GridSnapshot::new(6, 2, &theme);
    snap.write(0, 0, "Auk 🐧", theme.foreground, theme.background, 0);
    let image = render(&mut r, &snap, "png");
    let decoded = Image::decode_png(&image.encode_png()).unwrap();
    assert!(decoded == image);
}

#[test]
fn snapshot_from_terminal_carries_text_colors_and_cursor() {
    let theme = Theme::dark();
    let mut term = auk_term::Terminal::new(20, 4, auk_term::Config::default());
    term.feed(b"plain \x1b[31;1mred\x1b[0m\r\n\x1b[4munder\x1b[0m \xe6\xbc\xa2!");
    let snap = GridSnapshot::from_terminal(&term, &theme, &auk_render::View::default());
    assert_eq!((snap.cols, snap.rows), (20, 4));
    let row0 = snap.row(0);
    assert_eq!(row0[0].text, auk_render::CellText::Char('p'));
    assert_eq!(row0[6].fg, theme.palette[1]);
    assert!(row0[6].attrs.has(Attrs::BOLD));
    let row1 = snap.row(1);
    assert!(row1[0].attrs.has(Attrs::UNDERLINE));
    assert!(row1[6].attrs.has(Attrs::WIDE));
    assert!(row1[7].attrs.has(Attrs::SPACER));
    // The cursor sits after "!" as a focused block: its cell takes the cursor color.
    let cursor = snap.cursor.expect("visible cursor");
    assert_eq!(
        (cursor.col, cursor.row, cursor.style),
        (9, 1, CursorStyle::Block)
    );
    assert_eq!(snap.row(1)[9].bg, theme.cursor);
}
