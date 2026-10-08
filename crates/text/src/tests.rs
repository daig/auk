use super::*;

fn fonts(backend: Backend) -> GridFonts {
    GridFonts::new(&FontConfig {
        size_pt: 13.0,
        scale: 2.0,
        backend,
        ..FontConfig::default()
    })
    .expect("default monospace family")
}

fn backends() -> Vec<Backend> {
    let mut all = vec![Backend::Swash];
    #[cfg(target_os = "macos")]
    all.push(Backend::CoreText);
    all
}

fn ink(image: &GlyphImage) -> u64 {
    match image.format {
        GlyphFormat::Mask => image.data.iter().map(|&b| u64::from(b)).sum(),
        GlyphFormat::Color => image.data.chunks(4).map(|p| u64::from(p[3])).sum(),
    }
}

/// How far right the ink's centroid in the top quarter is of the bottom quarter's, in pixels.
fn lean(image: &GlyphImage) -> f64 {
    let centroid = |rows: std::ops::Range<u32>| {
        let (mut sum, mut weight) = (0.0, 0.0);
        for y in rows {
            for x in 0..image.width {
                let a = f64::from(image.data[(y * image.width + x) as usize]);
                sum += a * f64::from(x);
                weight += a;
            }
        }
        sum / weight
    };
    let q = image.height / 4;
    centroid(0..q) - centroid(image.height - q..image.height)
}

#[test]
fn default_family_is_a_system_monospace() {
    let f = fonts(Backend::Swash);
    assert!(
        DEFAULT_FAMILIES.contains(&f.family_name()),
        "{}",
        f.family_name()
    );
}

#[test]
fn metrics_are_monospace_shaped() {
    let m = fonts(Backend::Swash).metrics();
    // 13 pt at 2x is 26 px: monospace advances are about 0.6 em, line heights about 1.2 em.
    assert!((13..=18).contains(&m.cell_width), "{m:?}");
    assert!((28..=36).contains(&m.cell_height), "{m:?}");
    assert!(
        m.baseline > m.cell_height / 2 && m.baseline < m.cell_height,
        "{m:?}"
    );
    assert!(
        m.underline_position > m.baseline && m.underline_position < m.cell_height,
        "{m:?}"
    );
    assert!(m.strikeout_position < m.baseline, "{m:?}");
    assert!(m.underline_thickness >= 1);
}

#[test]
fn unknown_family_falls_through_to_the_next() {
    let config = FontConfig {
        families: vec!["No Such Family 1234".into(), "Menlo".into()],
        ..FontConfig::default()
    };
    assert_eq!(GridFonts::new(&config).unwrap().family_name(), "Menlo");
    let config = FontConfig {
        families: vec!["No Such Family 1234".into()],
        ..FontConfig::default()
    };
    assert!(matches!(
        GridFonts::new(&config),
        Err(FontError::NoFamily(_))
    ));
}

#[test]
fn blank_cells_draw_nothing() {
    for backend in backends() {
        let mut f = fonts(backend);
        assert!(f.rasterize("", GlyphStyle::REGULAR, 1).is_none());
        assert!(f.rasterize(" ", GlyphStyle::REGULAR, 1).is_none());
    }
}

#[test]
fn letters_sit_in_their_cell() {
    for backend in backends() {
        let mut f = fonts(backend);
        let m = f.metrics();
        let a = f.rasterize("A", GlyphStyle::REGULAR, 1).unwrap();
        assert_eq!(a.format, GlyphFormat::Mask);
        // Ink starts inside the cell, ends on the baseline, and doesn't spill sideways.
        assert!(
            a.left >= 0 && a.left + a.width as i32 <= m.cell_width as i32 + 1,
            "{backend:?} {a:?}"
        );
        assert!(a.top > 0, "{backend:?} {a:?}");
        let bottom = a.top + a.height as i32;
        assert!(
            (bottom - m.baseline as i32).abs() <= 1,
            "{backend:?} {a:?} {m:?}"
        );
        // Descenders go below the baseline; underscores sit near the bottom.
        let g = f.rasterize("g", GlyphStyle::REGULAR, 1).unwrap();
        assert!(
            g.top + g.height as i32 > m.baseline as i32 + 2,
            "{backend:?} {g:?}"
        );
        let dash = f.rasterize("-", GlyphStyle::REGULAR, 1).unwrap();
        assert!(
            dash.height <= 4 && dash.top < m.baseline as i32,
            "{backend:?} {dash:?}"
        );
    }
}

#[test]
fn backends_agree_on_glyph_boxes() {
    #[cfg(target_os = "macos")]
    {
        let mut ct = fonts(Backend::CoreText);
        let mut sw = fonts(Backend::Swash);
        for text in ["A", "g", "W", "@", "|", "Q"] {
            let a = ct.rasterize(text, GlyphStyle::REGULAR, 1).unwrap();
            let b = sw.rasterize(text, GlyphStyle::REGULAR, 1).unwrap();
            for (x, y, what) in [
                (a.left, b.left, "left"),
                (a.top, b.top, "top"),
                (a.width as i32, b.width as i32, "width"),
                (a.height as i32, b.height as i32, "height"),
            ] {
                assert!(
                    (x - y).abs() <= 1,
                    "{text:?} {what}: CoreText {a:?}, swash {b:?}"
                );
            }
        }
    }
}

#[test]
fn bold_has_more_ink_and_italic_leans() {
    for backend in backends() {
        let mut f = fonts(backend);
        let regular = f.rasterize("H", GlyphStyle::REGULAR, 1).unwrap();
        let bold = f
            .rasterize(
                "H",
                GlyphStyle {
                    bold: true,
                    italic: false,
                },
                1,
            )
            .unwrap();
        assert!(
            ink(&bold) as f64 > ink(&regular) as f64 * 1.15,
            "{backend:?}"
        );
        // An italic stem leans right: its top row's ink sits right of its bottom row's.
        let italic = f
            .rasterize(
                "I",
                GlyphStyle {
                    bold: false,
                    italic: true,
                },
                1,
            )
            .unwrap();
        let upright = f.rasterize("I", GlyphStyle::REGULAR, 1).unwrap();
        assert!(
            lean(&italic) > lean(&upright) + 2.0,
            "{backend:?} {italic:?} {upright:?}"
        );
    }
}

#[test]
fn emoji_are_color_and_fill_two_cells() {
    for backend in backends() {
        let mut f = fonts(backend);
        let m = f.metrics();
        assert_eq!(
            f.face_family("😀", GlyphStyle::REGULAR),
            "Apple Color Emoji"
        );
        let e = f.rasterize("😀", GlyphStyle::REGULAR, 2).unwrap();
        assert_eq!(e.format, GlyphFormat::Color, "{backend:?}");
        let two = 2 * m.cell_width as i32;
        assert!(
            e.left >= 0 && e.left + e.width as i32 <= two + 1,
            "{backend:?} {e:?}"
        );
        assert!(e.width as i32 >= two * 3 / 4, "{backend:?} {e:?}");
        // Mostly yellow: the mean color of solid pixels has high red and green, low blue.
        let solid: Vec<&[u8]> = e.data.chunks(4).filter(|p| p[3] >= 128).collect();
        assert!(
            solid.len() * 3 > (e.width * e.height) as usize,
            "{backend:?} {e:?}"
        );
        let mean = |i: usize| {
            solid
                .iter()
                .map(|p| u32::from(p[i]) * 255 / u32::from(p[3]))
                .sum::<u32>()
                / solid.len() as u32
        };
        let (r, g, b) = (mean(0), mean(1), mean(2));
        assert!(
            r > 180 && g > 130 && b < 110,
            "{backend:?}: mean color {r} {g} {b}"
        );
        // A ZWJ sequence is one image too.
        let family = f.rasterize("👩\u{200D}💻", GlyphStyle::REGULAR, 2).unwrap();
        assert_eq!(family.format, GlyphFormat::Color);
    }
}

#[test]
fn cjk_falls_back_and_fits_two_cells() {
    for backend in backends() {
        let mut f = fonts(backend);
        let m = f.metrics();
        let family = f.face_family("漢", GlyphStyle::REGULAR);
        assert_ne!(family, f.family_name());
        let g = f.rasterize("漢", GlyphStyle::REGULAR, 2).unwrap();
        assert_eq!(g.format, GlyphFormat::Mask);
        assert!(
            g.left >= 0 && g.left + g.width as i32 <= 2 * m.cell_width as i32 + 1,
            "{g:?}"
        );
        assert!(g.width > m.cell_width, "{backend:?} {g:?}");
        let kana = f.face_family("あ", GlyphStyle::REGULAR);
        assert_ne!(kana, f.family_name());
    }
}

#[test]
fn combining_marks_draw_above_the_base() {
    for backend in backends() {
        let mut f = fonts(backend);
        let plain = f.rasterize("e", GlyphStyle::REGULAR, 1).unwrap();
        let acute = f.rasterize("e\u{301}", GlyphStyle::REGULAR, 1).unwrap();
        assert!(acute.top < plain.top - 2, "{backend:?} {acute:?} {plain:?}");
    }
}

#[test]
fn box_drawing_and_symbols_resolve() {
    for backend in backends() {
        let mut f = fonts(backend);
        for text in ["─", "│", "█", "→", "✓", "⌘"] {
            assert!(
                f.rasterize(text, GlyphStyle::REGULAR, 1).is_some(),
                "{backend:?} {text}"
            );
        }
        // Full block fills the cell's width.
        let m = f.metrics();
        let block = f.rasterize("█", GlyphStyle::REGULAR, 1).unwrap();
        assert!(
            block.width + 1 >= m.cell_width,
            "{backend:?} {block:?} {m:?}"
        );
    }
}

#[test]
fn resizing_rescales_metrics() {
    let mut f = fonts(Backend::Swash);
    let before = f.metrics();
    let after = f.set_size(13.0, 1.0).unwrap();
    assert!(
        after.cell_width * 2 >= before.cell_width - 1
            && after.cell_width * 2 <= before.cell_width + 1
    );
    assert!(f.size_px() == 13.0);
}

#[test]
fn registered_fonts_work_without_system_fonts() {
    // The browser client's path: no system fonts, only registered data.
    let data = std::fs::read("/System/Library/Fonts/Menlo.ttc").expect("Menlo ships with macOS");
    let config = FontConfig {
        families: vec!["Menlo".into()],
        system_fonts: false,
        extra_fonts: vec![Arc::new(data)],
        backend: Backend::Swash,
        size_pt: 13.0,
        scale: 2.0,
    };
    let mut f = GridFonts::new(&config).unwrap();
    assert_eq!(f.family_name(), "Menlo");
    assert!(f.rasterize("x", GlyphStyle::REGULAR, 1).is_some());
}
