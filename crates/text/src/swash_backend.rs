//! The swash rasterizer: pure Rust, used by the browser client (wasm32) and available natively.

use swash::scale::image::Content;
use swash::scale::{Render, ScaleContext, Source, StrikeWith};
use swash::shape::ShapeContext;
use swash::zeno::{Angle, Format, Transform, Vector};

use crate::canvas::{Canvas, GlyphFormat};
use crate::resolve::{Face, normalized_coords};

#[derive(Default)]
pub(crate) struct SwashRasterizer {
    shape: ShapeContext,
    scale: ScaleContext,
}

/// A shaped glyph: id and pen position relative to the cluster's origin.
struct Placed {
    id: u16,
    x: f32,
    y: f32,
}

impl SwashRasterizer {
    fn shape(&mut self, face: &Face, size_px: f32, text: &str) -> (Vec<Placed>, f32) {
        let Some(font) = face.font_ref() else {
            return (Vec::new(), 0.0);
        };
        let coords = normalized_coords(&font, &face.variations);
        let mut shaper = self
            .shape
            .builder(font)
            .size(size_px)
            .normalized_coords(coords.iter())
            .build();
        shaper.add_str(text);
        let mut glyphs = Vec::new();
        let mut pen = 0.0;
        shaper.shape_with(|cluster| {
            for g in cluster.glyphs {
                glyphs.push(Placed {
                    id: g.id,
                    x: pen + g.x,
                    y: g.y,
                });
                pen += g.advance;
            }
        });
        (glyphs, pen)
    }
}

impl crate::Rasterizer for SwashRasterizer {
    fn advance(&mut self, face: &Face, size_px: f32, text: &str) -> f32 {
        self.shape(face, size_px, text).1
    }

    fn draw(
        &mut self,
        face: &Face,
        size_px: f32,
        text: &str,
        pen_x: f32,
        baseline_y: f32,
        canvas: &mut Canvas,
    ) {
        let (glyphs, _) = self.shape(face, size_px, text);
        let Some(font) = face.font_ref() else { return };
        let coords = normalized_coords(&font, &face.variations);
        let mut scaler = self
            .scale
            .builder(font)
            .size(size_px)
            .hint(false)
            .normalized_coords(coords.iter())
            .build();
        let sources = match canvas.format {
            GlyphFormat::Color => &[
                Source::ColorOutline(0),
                Source::ColorBitmap(StrikeWith::BestFit),
                Source::Outline,
            ][..],
            GlyphFormat::Mask => &[Source::Outline, Source::Bitmap(StrikeWith::BestFit)][..],
        };
        let mut render = Render::new(sources);
        render.format(Format::Alpha);
        if face.embolden {
            render.embolden(size_px / 24.0);
        }
        if let Some(skew) = face.skew {
            render.transform(Some(Transform::skew(
                Angle::from_degrees(skew),
                Angle::ZERO,
            )));
        }
        for glyph in glyphs {
            let x = pen_x + glyph.x;
            let y = baseline_y - glyph.y;
            // Whole pixels go into placement; the fraction goes to the rasterizer.
            let (ix, iy) = (x.floor(), y.floor());
            render.offset(Vector::new(x - ix, iy - y));
            let Some(image) = render.render(&mut scaler, glyph.id) else {
                continue;
            };
            let left = ix as i32 + image.placement.left;
            let top = iy as i32 - image.placement.top;
            let (w, h) = (image.placement.width as i32, image.placement.height as i32);
            for row in 0..h {
                let cy = top + row;
                if cy < 0 || cy >= canvas.height as i32 {
                    continue;
                }
                for col in 0..w {
                    let cx = left + col;
                    if cx < 0 || cx >= canvas.width as i32 {
                        continue;
                    }
                    let src = (row * w + col) as usize;
                    let dst = cy as usize * canvas.width + cx as usize;
                    composite(canvas, dst, &image.content, &image.data, src);
                }
            }
        }
    }

    fn clear(&mut self) {
        self.scale = ScaleContext::new();
    }
}

/// Composites one source pixel over the canvas pixel at `dst`.
fn composite(canvas: &mut Canvas, dst: usize, content: &Content, data: &[u8], src: usize) {
    match (canvas.format, content) {
        (GlyphFormat::Mask, Content::Mask) => {
            let d = &mut canvas.data[dst];
            *d = d.saturating_add(data[src]);
        }
        (GlyphFormat::Mask, Content::SubpixelMask | Content::Color) => {
            let d = &mut canvas.data[dst];
            *d = d.saturating_add(data[src * 4 + 3]);
        }
        (GlyphFormat::Color, Content::Mask) => {
            // A monochrome glyph in a color face: white, premultiplied.
            let a = data[src];
            over(&mut canvas.data[dst * 4..dst * 4 + 4], [a, a, a, a]);
        }
        (GlyphFormat::Color, Content::Color | Content::SubpixelMask) => {
            let p = &data[src * 4..src * 4 + 4];
            let a = u16::from(p[3]);
            let premul = |c: u8| ((u16::from(c) * a + 127) / 255) as u8;
            over(
                &mut canvas.data[dst * 4..dst * 4 + 4],
                [premul(p[0]), premul(p[1]), premul(p[2]), p[3]],
            );
        }
    }
}

/// Premultiplied source-over.
fn over(dst: &mut [u8], src: [u8; 4]) {
    let inv = 255 - u16::from(src[3]);
    for i in 0..4 {
        dst[i] = (u16::from(src[i]) + (u16::from(dst[i]) * inv + 127) / 255).min(255) as u8;
    }
}
