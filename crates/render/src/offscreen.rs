//! Rendering to a texture and reading it back: golden tests, headless mode, benchmarks.

use std::path::Path;

use crate::Gpu;
use crate::theme::Rgba;

/// The format offscreen targets use (and so the format a [`crate::GridRenderer`] for them must
/// be made with).
pub const OFFSCREEN_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// A texture to render into instead of a window.
#[derive(Debug)]
pub struct OffscreenTarget {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    width: u32,
    height: u32,
}

impl OffscreenTarget {
    pub fn new(gpu: &Gpu, width: u32, height: u32) -> OffscreenTarget {
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offscreen target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: OFFSCREEN_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        OffscreenTarget {
            texture,
            view,
            width,
            height,
        }
    }

    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Reads the texture back, after all submitted work.
    pub fn read(&self, gpu: &Gpu) -> Result<Image, ReadbackError> {
        let row_bytes = self.width * 4;
        let padded = row_bytes.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("offscreen readback"),
            size: u64::from(padded * self.height),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            self.texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(self.height),
                },
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
        gpu.queue.submit([encoder.finish()]);
        let mapped = map_read(gpu, &buffer)?;
        let mut data = Vec::with_capacity((row_bytes * self.height) as usize);
        for row in mapped.chunks(padded as usize) {
            data.extend_from_slice(&row[..row_bytes as usize]);
        }
        Ok(Image {
            width: self.width,
            height: self.height,
            data,
        })
    }
}

/// Reading GPU memory back failed (the device was lost).
#[derive(Debug)]
pub struct ReadbackError(pub String);

impl std::fmt::Display for ReadbackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GPU readback failed: {}", self.0)
    }
}

impl std::error::Error for ReadbackError {}

/// Maps a `MAP_READ` buffer after all submitted work and copies its contents out.
pub(crate) fn map_read(gpu: &Gpu, buffer: &wgpu::Buffer) -> Result<Vec<u8>, ReadbackError> {
    let (tx, rx) = std::sync::mpsc::channel();
    buffer.map_async(wgpu::MapMode::Read, .., move |result| {
        let _ = tx.send(result);
    });
    gpu.wait_idle();
    rx.recv()
        .map_err(|e| ReadbackError(e.to_string()))?
        .map_err(|e| ReadbackError(e.to_string()))?;
    let data = buffer
        .get_mapped_range(..)
        .map_err(|e| ReadbackError(e.to_string()))?
        .to_vec();
    buffer.unmap();
    Ok(data)
}

/// An RGBA8 image, rows top to bottom.
#[derive(Clone, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl std::fmt::Debug for Image {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Image")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish()
    }
}

impl Image {
    pub fn pixel(&self, x: u32, y: u32) -> Rgba {
        let i = ((y * self.width + x) * 4) as usize;
        Rgba(
            self.data[i],
            self.data[i + 1],
            self.data[i + 2],
            self.data[i + 3],
        )
    }

    /// The image as PNG bytes.
    pub fn encode_png(&self) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, self.width, self.height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder
                .write_header()
                .expect("writing to a Vec cannot fail");
            writer
                .write_image_data(&self.data)
                .expect("data matches the header");
        }
        out
    }

    pub fn save_png(&self, path: impl AsRef<Path>) -> std::io::Result<()> {
        std::fs::write(path, self.encode_png())
    }

    /// Decodes an RGBA8 PNG (as written by [`Image::encode_png`]).
    pub fn decode_png(bytes: &[u8]) -> Result<Image, png::DecodingError> {
        let mut reader = png::Decoder::new(std::io::Cursor::new(bytes)).read_info()?;
        let mut data = vec![0; reader.output_buffer_size().unwrap_or(0)];
        let info = reader.next_frame(&mut data)?;
        data.truncate(info.buffer_size());
        Ok(Image {
            width: info.width,
            height: info.height,
            data,
        })
    }
}
