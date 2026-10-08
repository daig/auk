//! Prints the grid font's family, metrics and a few glyph boxes.
use auk_text::{Backend, FontConfig, GlyphStyle, GridFonts};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let config = FontConfig {
        families: args,
        size_pt: 13.0,
        scale: 2.0,
        ..FontConfig::default()
    };
    for backend in [Backend::CoreText, Backend::Swash] {
        let mut fonts = GridFonts::new(&FontConfig {
            backend,
            ..config.clone()
        })
        .expect("font");
        println!("{backend:?}: {} {:?}", fonts.family_name(), fonts.metrics());
        for text in ["A", "g", "漢", "😀", "█", "─"] {
            let span = if text == "漢" || text == "😀" {
                2
            } else {
                1
            };
            let face = fonts.face_family(text, GlyphStyle::REGULAR);
            println!(
                "  {text}: {face}: {:?}",
                fonts.rasterize(text, GlyphStyle::REGULAR, span)
            );
        }
    }
}
