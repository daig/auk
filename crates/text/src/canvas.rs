//! The bitmap a rasterizer draws a cluster into, and the trimmed image it becomes.

/// How a glyph image's pixels are stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GlyphFormat {
    /// One byte of coverage per pixel; the renderer tints it with the cell's color.
    Mask,
    /// Premultiplied RGBA, four bytes per pixel (emoji).
    Color,
}

impl GlyphFormat {
    pub fn bytes_per_pixel(self) -> usize {
        match self {
            GlyphFormat::Mask => 1,
            GlyphFormat::Color => 4,
        }
    }
}

/// A rasterized cluster, trimmed to its ink.
#[derive(Clone, PartialEq, Eq)]
pub struct GlyphImage {
    pub width: u32,
    pub height: u32,
    /// Offset of the image's left edge from the cell's left edge, in pixels.
    pub left: i32,
    /// Offset of the image's top edge from the cell's top edge, in pixels.
    pub top: i32,
    pub format: GlyphFormat,
    /// Rows top to bottom, `width * bytes_per_pixel` bytes each, tightly packed.
    pub data: Vec<u8>,
}

impl std::fmt::Debug for GlyphImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GlyphImage")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("left", &self.left)
            .field("top", &self.top)
            .field("format", &self.format)
            .finish_non_exhaustive()
    }
}

/// A scratch bitmap, rows top to bottom.
#[derive(Debug)]
pub(crate) struct Canvas {
    pub width: usize,
    pub height: usize,
    pub format: GlyphFormat,
    pub data: Vec<u8>,
}

impl Canvas {
    pub fn new(width: usize, height: usize, format: GlyphFormat) -> Self {
        let data = vec![0; width * height * format.bytes_per_pixel()];
        Canvas {
            width,
            height,
            format,
            data,
        }
    }

    #[cfg_attr(not(target_os = "macos"), expect(dead_code))]
    pub fn stride(&self) -> usize {
        self.width * self.format.bytes_per_pixel()
    }

    /// Whether the pixel at (x, y) has any ink.
    fn inked(&self, x: usize, y: usize) -> bool {
        match self.format {
            GlyphFormat::Mask => self.data[y * self.width + x] != 0,
            GlyphFormat::Color => self.data[(y * self.width + x) * 4 + 3] != 0,
        }
    }

    /// Trims the canvas to its ink. `origin_x`/`origin_y` give the canvas's top-left corner
    /// relative to the cell's. Returns `None` for a canvas without ink.
    pub fn into_glyph(self, origin_x: i32, origin_y: i32) -> Option<GlyphImage> {
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
        for y in 0..self.height {
            for x in 0..self.width {
                if self.inked(x, y) {
                    x0 = x0.min(x);
                    x1 = x1.max(x + 1);
                    y0 = y0.min(y);
                    y1 = y1.max(y + 1);
                }
            }
        }
        if x0 == usize::MAX {
            return None;
        }
        let bpp = self.format.bytes_per_pixel();
        let (w, h) = (x1 - x0, y1 - y0);
        let mut data = Vec::with_capacity(w * h * bpp);
        for y in y0..y1 {
            let start = (y * self.width + x0) * bpp;
            data.extend_from_slice(&self.data[start..start + w * bpp]);
        }
        Some(GlyphImage {
            width: w as u32,
            height: h as u32,
            left: origin_x + x0 as i32,
            top: origin_y + y0 as i32,
            format: self.format,
            data,
        })
    }
}
