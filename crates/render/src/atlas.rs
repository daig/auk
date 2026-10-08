//! Glyph atlases: one coverage (R8) texture for text and one RGBA texture for color glyphs, each
//! packed in shelves. A full atlas doubles in place (a GPU copy keeps what it holds) up to the
//! device limit; past that the caller clears it and rasterizes again.

use auk_text::GlyphFormat;

/// Shelf packing: rows of glyphs of similar height.
#[derive(Debug)]
pub(crate) struct ShelfPacker {
    width: u32,
    height: u32,
    shelves: Vec<Shelf>,
    /// Top of the unused space below the last shelf.
    next_y: u32,
}

#[derive(Debug)]
struct Shelf {
    y: u32,
    height: u32,
    /// Left of the unused space on the shelf.
    x: u32,
}

/// Space between glyphs, so neighbors never bleed into each other.
const GAP: u32 = 1;

impl ShelfPacker {
    pub fn new(width: u32, height: u32) -> Self {
        ShelfPacker {
            width,
            height,
            shelves: Vec::new(),
            next_y: 0,
        }
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Grows to a larger size, keeping every placement.
    pub fn grow(&mut self, width: u32, height: u32) {
        debug_assert!(width >= self.width && height >= self.height);
        self.width = width;
        self.height = height;
    }

    pub fn clear(&mut self) {
        self.shelves.clear();
        self.next_y = 0;
    }

    /// Places a `w`×`h` rectangle; returns its top-left corner.
    pub fn alloc(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        let (w, h) = (w + GAP, h + GAP);
        if w > self.width {
            return None;
        }
        // The shortest shelf the glyph fits on, wasting at most a third of the shelf's height.
        let best = self
            .shelves
            .iter_mut()
            .filter(|s| s.height >= h && s.height * 2 <= h * 3 && s.x + w <= self.width)
            .min_by_key(|s| s.height);
        if let Some(shelf) = best {
            let at = (shelf.x, shelf.y);
            shelf.x += w;
            return Some(at);
        }
        if self.next_y + h > self.height {
            return None;
        }
        let shelf = Shelf {
            y: self.next_y,
            height: h,
            x: w,
        };
        self.next_y += h;
        let at = (0, shelf.y);
        self.shelves.push(shelf);
        Some(at)
    }
}

/// One atlas texture and its packer.
#[derive(Debug)]
pub(crate) struct AtlasTexture {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub packer: ShelfPacker,
    format: GlyphFormat,
}

pub(crate) const INITIAL_SIZE: u32 = 1024;

impl AtlasTexture {
    pub fn new(device: &wgpu::Device, format: GlyphFormat, size: u32) -> Self {
        let texture = create_texture(device, format, size, size);
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        AtlasTexture {
            texture,
            view,
            packer: ShelfPacker::new(size, size),
            format,
        }
    }

    /// Uploads a `w`×`h` image (tightly packed rows) at `at`.
    pub fn upload(&self, queue: &wgpu::Queue, at: (u32, u32), w: u32, h: u32, data: &[u8]) {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: at.0,
                    y: at.1,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * self.format.bytes_per_pixel() as u32),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
    }

    /// Doubles the texture, copying its contents on the GPU. Returns false at the device's
    /// size limit.
    pub fn grow(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> bool {
        let (w, h) = self.packer.size();
        let max = device.limits().max_texture_dimension_2d;
        if w * 2 > max || h * 2 > max {
            return false;
        }
        let texture = create_texture(device, self.format, w * 2, h * 2);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("atlas grow"),
        });
        encoder.copy_texture_to_texture(
            self.texture.as_image_copy(),
            texture.as_image_copy(),
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        // Earlier `write_texture`s to the old texture run before this submission's copy.
        queue.submit([encoder.finish()]);
        self.view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.texture = texture;
        self.packer.grow(w * 2, h * 2);
        true
    }
}

fn create_texture(device: &wgpu::Device, format: GlyphFormat, w: u32, h: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(match format {
            GlyphFormat::Mask => "glyph atlas (coverage)",
            GlyphFormat::Color => "glyph atlas (color)",
        }),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: match format {
            GlyphFormat::Mask => wgpu::TextureFormat::R8Unorm,
            GlyphFormat::Color => wgpu::TextureFormat::Rgba8Unorm,
        },
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shelves_pack_without_overlap() {
        let mut p = ShelfPacker::new(64, 64);
        let mut placed = Vec::new();
        for (w, h) in [(10, 20), (10, 20), (12, 18), (30, 8), (40, 20), (5, 5)] {
            let at = p.alloc(w, h).unwrap();
            placed.push((at.0, at.1, w, h));
        }
        for (i, a) in placed.iter().enumerate() {
            assert!(a.0 + a.2 <= 64 && a.1 + a.3 <= 64);
            for b in &placed[i + 1..] {
                let disjoint =
                    a.0 + a.2 <= b.0 || b.0 + b.2 <= a.0 || a.1 + a.3 <= b.1 || b.1 + b.3 <= a.1;
                assert!(disjoint, "{a:?} overlaps {b:?}");
            }
        }
        // Similar heights share a shelf.
        assert_eq!(placed[0].1, placed[1].1);
        assert_eq!(placed[0].1, placed[2].1);
    }

    #[test]
    fn full_packer_refuses_then_grows() {
        let mut p = ShelfPacker::new(16, 16);
        assert!(p.alloc(20, 4).is_none());
        let mut n = 0;
        while p.alloc(7, 7).is_some() {
            n += 1;
        }
        assert_eq!(n, 4);
        p.grow(32, 32);
        assert!(p.alloc(7, 7).is_some());
        p.clear();
        assert_eq!(p.alloc(7, 7), Some((0, 0)));
    }
}
