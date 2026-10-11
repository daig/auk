//! The grid pass: cell backgrounds, glyphs from the atlases, decorations and the cursor,
//! with damage tracking per row.

use std::collections::HashMap;
use std::time::Duration;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Instant;
// std's clock panics in the browser.
#[cfg(target_arch = "wasm32")]
use web_time::Instant;

use auk_text::{CellMetrics, GlyphFormat, GlyphStyle, GridFonts};
use bytemuck::{Pod, Zeroable};

use crate::atlas::{AtlasTexture, INITIAL_SIZE};
use crate::snapshot::{Attrs, CellText, CursorStyle, GridSnapshot, SnapCell};
use crate::theme::Rgba;
use crate::{Gpu, GpuTimer};

/// One quad: a glyph, a decoration run or a cursor part. Positions are relative to the grid's
/// origin (x) and to the top of the quad's row (y), in physical pixels.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
struct Instance {
    pos: [f32; 2],
    size: [f32; 2],
    /// Top-left texel in the atlas the kind reads from.
    uv: [f32; 2],
    color: [u8; 4],
    kind: u32,
    /// Line thickness for patterned decorations.
    param: f32,
    /// The grid row the quad belongs to; `pos.y` is relative to the row's top.
    row: u32,
}

const KIND_COVERAGE: u32 = 0;
const KIND_COLOR: u32 = 1;
const KIND_RECT: u32 = 2;
const KIND_CURLY: u32 = 3;
const KIND_DOTTED: u32 = 4;
const KIND_DASHED: u32 = 5;

/// One line of a decoration: top (from the cell's top), height and instance kind.
type DecorationLine = (f32, f32, u32);

/// Instances a row can hold per cell: a glyph and up to four decoration runs.
const SLOTS_PER_CELL: usize = 5;

/// Rows whose slot (top, height) the mixed layout can set; rows past it sit at their uniform
/// position. Matches `MAX_SLOT_ROWS` in the shader.
pub const MAX_SLOT_ROWS: usize = 1024;

/// Where one grid row is drawn, relative to the grid's origin, in physical pixels: its top and
/// height. A height of 0 hides the row.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RowSlot {
    pub top: f32,
    pub height: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct Uniforms {
    viewport: [f32; 2],
    cell: [f32; 2],
    origin: [f32; 2],
    grid: [u32; 2],
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct GlyphKey {
    text: CellText,
    style: GlyphStyle,
    span: u8,
}

/// Where a cached glyph lives.
#[derive(Clone, Copy, Debug)]
struct Glyph {
    format: GlyphFormat,
    uv: [f32; 2],
    size: [f32; 2],
    offset: [f32; 2],
}

/// The atlas ran out of room even at its largest size.
#[derive(Debug)]
struct AtlasFull;

/// What a [`GridRenderer::prepare`] call did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PrepareStats {
    /// Rows whose instances and backgrounds were rebuilt and uploaded.
    pub damaged_rows: u32,
    /// Instances drawn this frame.
    pub instances: u32,
    /// Glyphs rasterized (atlas misses).
    pub rasterized: u32,
    pub cpu_time: Duration,
}

/// Draws a [`GridSnapshot`] into a render target of one texture format.
pub struct GridRenderer {
    fonts: GridFonts,
    metrics: CellMetrics,
    format: wgpu::TextureFormat,
    bg_pipeline: wgpu::RenderPipeline,
    fg_pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    uniforms: wgpu::Buffer,
    bind_group: Option<wgpu::BindGroup>,
    coverage: AtlasTexture,
    color: AtlasTexture,
    glyphs: HashMap<GlyphKey, Option<Glyph>>,

    /// The grid size the GPU resources below are sized for.
    cols: u16,
    rows: u16,
    backgrounds: wgpu::Texture,
    /// CPU copy of `backgrounds`, row-major.
    bg_cells: Vec<[u8; 4]>,
    instances: wgpu::Buffer,
    /// Instances per row, at `row * cols * SLOTS_PER_CELL`.
    row_counts: Vec<u32>,
    cursor_buffer: wgpu::Buffer,
    cursor_count: u32,
    /// The snapshot last prepared, for damage.
    previous: Option<GridSnapshot>,
    origin: [f32; 2],
    viewport: (u32, u32),
    /// Scratch for one row's instances.
    scratch: Vec<Instance>,
    /// The row slots (the mixed layout); `None` lays rows out uniformly.
    custom_slots: Option<Vec<RowSlot>>,
    /// The slot table last uploaded, as (top, height) pairs.
    uploaded_slots: Vec<[f32; 2]>,
    slot_buffer: wgpu::Buffer,
}

impl std::fmt::Debug for GridRenderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GridRenderer")
            .field("fonts", &self.fonts)
            .field("grid", &(self.cols, self.rows))
            .field("glyphs", &self.glyphs.len())
            .finish_non_exhaustive()
    }
}

impl GridRenderer {
    /// A renderer drawing into targets of `format` (the surface's, or `Rgba8Unorm` offscreen).
    pub fn new(gpu: &Gpu, format: wgpu::TextureFormat, fonts: GridFonts) -> GridRenderer {
        let device = &gpu.device;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("grid"),
            source: wgpu::ShaderSource::Wgsl(include_str!("grid.wgsl").into()),
        });
        let texture_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("grid"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                texture_entry(1),
                texture_entry(2),
                texture_entry(3),
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("grid"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = |label, vs, fs, buffers: &[Option<wgpu::VertexBufferLayout<'_>>], blend| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(vs),
                    compilation_options: Default::default(),
                    buffers,
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleStrip,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let bg_pipeline = pipeline("grid backgrounds", "bg_vs", "bg_fs", &[], None);
        let instance_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Instance>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &wgpu::vertex_attr_array![
                0 => Float32x2, 1 => Float32x2, 2 => Float32x2, 3 => Unorm8x4, 4 => Uint32, 5 => Float32,
                6 => Uint32
            ],
        };
        let fg_pipeline = pipeline(
            "grid foreground",
            "fg_vs",
            "fg_fs",
            &[Some(instance_layout)],
            Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
        );
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("grid uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let cursor_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("grid cursor"),
            size: (std::mem::size_of::<Instance>() * 4) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let slot_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("grid row slots"),
            size: (MAX_SLOT_ROWS * 8) as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let metrics = fonts.metrics();
        let mut renderer = GridRenderer {
            fonts,
            metrics,
            format,
            bg_pipeline,
            fg_pipeline,
            layout,
            uniforms,
            bind_group: None,
            coverage: AtlasTexture::new(device, GlyphFormat::Mask, INITIAL_SIZE),
            color: AtlasTexture::new(device, GlyphFormat::Color, INITIAL_SIZE),
            glyphs: HashMap::new(),
            cols: 0,
            rows: 0,
            backgrounds: create_backgrounds(device, 1, 1),
            bg_cells: Vec::new(),
            instances: create_instances(device, 1, 1),
            row_counts: Vec::new(),
            cursor_buffer,
            cursor_count: 0,
            previous: None,
            origin: [0.0; 2],
            viewport: (1, 1),
            scratch: Vec::new(),
            custom_slots: None,
            uploaded_slots: Vec::new(),
            slot_buffer,
        };
        renderer.resize_grid(device, 1, 1);
        renderer
    }

    pub fn metrics(&self) -> CellMetrics {
        self.metrics
    }

    pub fn fonts(&self) -> &GridFonts {
        &self.fonts
    }

    /// The texture format this renderer draws into.
    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }

    /// Replaces the fonts (a size or scale change): clears the atlases and redraws everything.
    pub fn set_fonts(&mut self, fonts: GridFonts) {
        self.fonts = fonts;
        self.metrics = self.fonts.metrics();
        self.clear_atlas();
    }

    /// How many cells fit in a `width`×`height` pixel area after `padding` pixels on each side.
    pub fn grid_size_for(&self, width: u32, height: u32, padding: u32) -> (u16, u16) {
        let w = width.saturating_sub(2 * padding) / self.metrics.cell_width;
        let h = height.saturating_sub(2 * padding) / self.metrics.cell_height;
        (
            w.clamp(1, u32::from(u16::MAX)) as u16,
            h.clamp(1, u32::from(u16::MAX)) as u16,
        )
    }

    /// Where the grid's top-left corner goes in the target, in pixels.
    pub fn set_origin(&mut self, x: f32, y: f32) {
        self.origin = [x, y];
    }

    /// Places each row of the next snapshots: its top and height relative to the origin (the
    /// runs of grid rows with surface blocks between them). `None` lays rows out uniformly, one
    /// cell height apart. Rows past the slice, or past [`MAX_SLOT_ROWS`], sit at their uniform
    /// position.
    pub fn set_row_slots(&mut self, slots: Option<Vec<RowSlot>>) {
        self.custom_slots = slots;
    }

    fn upload_slots(&mut self, gpu: &Gpu) {
        let ch = self.metrics.cell_height as f32;
        let rows = usize::from(self.rows).min(MAX_SLOT_ROWS);
        let table: Vec<[f32; 2]> = (0..rows)
            .map(
                |r| match self.custom_slots.as_ref().and_then(|s| s.get(r)) {
                    Some(slot) => [slot.top, slot.height],
                    None => [r as f32 * ch, ch],
                },
            )
            .collect();
        if table != self.uploaded_slots {
            gpu.queue
                .write_buffer(&self.slot_buffer, 0, bytemuck::cast_slice(&table));
            self.uploaded_slots = table;
        }
    }

    /// Forgets the previous frame: the next `prepare` rebuilds every row.
    pub fn invalidate(&mut self) {
        self.previous = None;
    }

    /// Uploads what changed since the last snapshot. `viewport` is the target's size in pixels.
    pub fn prepare(
        &mut self,
        gpu: &Gpu,
        snap: &GridSnapshot,
        viewport: (u32, u32),
    ) -> PrepareStats {
        let start = Instant::now();
        let mut stats = PrepareStats::default();
        if (snap.cols, snap.rows) != (self.cols, self.rows) {
            self.resize_grid(&gpu.device, snap.cols, snap.rows);
        }
        self.viewport = viewport;
        let uniforms = Uniforms {
            viewport: [viewport.0 as f32, viewport.1 as f32],
            cell: [
                self.metrics.cell_width as f32,
                self.metrics.cell_height as f32,
            ],
            origin: self.origin,
            grid: [u32::from(self.cols), u32::from(self.rows)],
        };
        gpu.queue
            .write_buffer(&self.uniforms, 0, bytemuck::bytes_of(&uniforms));

        let damaged: Vec<u16> = (0..snap.rows)
            .filter(|&r| {
                self.previous
                    .as_ref()
                    .is_none_or(|p| p.row(r) != snap.row(r))
            })
            .collect();
        let rows = match self.build_rows(gpu, snap, &damaged, &mut stats) {
            Ok(()) => damaged,
            Err(AtlasFull) => {
                // Even the largest atlas can't hold this frame plus everything cached: start
                // over with only this frame's glyphs.
                self.clear_atlas();
                let all: Vec<u16> = (0..snap.rows).collect();
                // A frame that alone overflows the atlas draws without the glyphs that didn't
                // fit; nothing else can be done for it.
                let _ = self.build_rows(gpu, snap, &all, &mut stats);
                all
            }
        };
        self.upload_backgrounds(gpu, &rows);
        self.upload_slots(gpu);
        self.build_cursor(gpu, snap);
        if self.bind_group.is_none() {
            self.bind_group = Some(self.create_bind_group(&gpu.device));
        }
        self.previous = Some(snap.clone());
        stats.damaged_rows = rows.len() as u32;
        stats.instances = self.row_counts.iter().sum::<u32>() + self.cursor_count;
        stats.cpu_time = start.elapsed();
        stats
    }

    /// Encodes a render pass that clears `target` to `clear` and draws the grid prepared last.
    pub fn render(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        clear: Rgba,
        timer: Option<&GpuTimer>,
    ) {
        let c = |v: u8| f64::from(v) / 255.0;
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("grid"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: c(clear.0),
                        g: c(clear.1),
                        b: c(clear.2),
                        a: c(clear.3),
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: timer.map(GpuTimer::pass_writes),
            occlusion_query_set: None,
            multiview_mask: None,
        });
        self.draw(&mut pass);
    }

    /// Draws the grid prepared last into an open render pass.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        let Some(bind_group) = &self.bind_group else {
            return;
        };
        pass.set_bind_group(0, bind_group, &[]);
        pass.set_pipeline(&self.bg_pipeline);
        pass.draw(0..4, 0..u32::from(self.rows));
        pass.set_pipeline(&self.fg_pipeline);
        pass.set_vertex_buffer(0, self.instances.slice(..));
        let cap = self.row_capacity() as u32;
        for (r, &count) in self.row_counts.iter().enumerate() {
            if count > 0 {
                let base = r as u32 * cap;
                pass.draw(0..4, base..base + count);
            }
        }
        if self.cursor_count > 0 {
            pass.set_vertex_buffer(0, self.cursor_buffer.slice(..));
            pass.draw(0..4, 0..self.cursor_count);
        }
    }

    fn row_capacity(&self) -> usize {
        usize::from(self.cols) * SLOTS_PER_CELL
    }

    fn resize_grid(&mut self, device: &wgpu::Device, cols: u16, rows: u16) {
        let (cols, rows) = (cols.max(1), rows.max(1));
        self.cols = cols;
        self.rows = rows;
        self.backgrounds = create_backgrounds(device, cols, rows);
        self.bg_cells = vec![[0; 4]; usize::from(cols) * usize::from(rows)];
        self.instances = create_instances(device, cols, rows);
        self.row_counts = vec![0; usize::from(rows)];
        self.previous = None;
        self.bind_group = None;
    }

    fn clear_atlas(&mut self) {
        self.glyphs.clear();
        self.coverage.packer.clear();
        self.color.packer.clear();
        self.previous = None;
    }

    fn create_bind_group(&self, device: &wgpu::Device) -> wgpu::BindGroup {
        let bg_view = self
            .backgrounds
            .create_view(&wgpu::TextureViewDescriptor::default());
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("grid"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&bg_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&self.coverage.view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&self.color.view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: self.slot_buffer.as_entire_binding(),
                },
            ],
        })
    }

    fn build_rows(
        &mut self,
        gpu: &Gpu,
        snap: &GridSnapshot,
        rows: &[u16],
        stats: &mut PrepareStats,
    ) -> Result<(), AtlasFull> {
        let cols = usize::from(self.cols);
        let cap = self.row_capacity();
        let mut scratch = std::mem::take(&mut self.scratch);
        for &r in rows {
            let row = snap.row(r);
            for (c, cell) in row.iter().enumerate() {
                self.bg_cells[usize::from(r) * cols + c] = cell.bg.to_array();
            }
            scratch.clear();
            let built = self.build_row(gpu, row, r, &mut scratch, stats);
            if built.is_err() {
                self.scratch = scratch;
                return built;
            }
            scratch.truncate(cap);
            self.row_counts[usize::from(r)] = scratch.len() as u32;
            if !scratch.is_empty() {
                let offset = (usize::from(r) * cap * std::mem::size_of::<Instance>()) as u64;
                gpu.queue
                    .write_buffer(&self.instances, offset, bytemuck::cast_slice(&scratch));
            }
        }
        self.scratch = scratch;
        Ok(())
    }

    fn build_row(
        &mut self,
        gpu: &Gpu,
        row: &[SnapCell],
        r: u16,
        out: &mut Vec<Instance>,
        stats: &mut PrepareStats,
    ) -> Result<(), AtlasFull> {
        let m = self.metrics;
        let cw = m.cell_width as f32;
        for (c, cell) in row.iter().enumerate() {
            if cell.text.is_blank() || cell.attrs.has(Attrs::SPACER) {
                continue;
            }
            let span = if cell.attrs.has(Attrs::WIDE) { 2 } else { 1 };
            let style = GlyphStyle {
                bold: cell.attrs.has(Attrs::BOLD),
                italic: cell.attrs.has(Attrs::ITALIC),
            };
            let key = GlyphKey {
                text: cell.text.clone(),
                style,
                span,
            };
            let Some(glyph) = self.glyph(gpu, key, stats)? else {
                continue;
            };
            out.push(Instance {
                pos: [c as f32 * cw + glyph.offset[0], glyph.offset[1]],
                size: glyph.size,
                uv: glyph.uv,
                color: cell.fg.to_array(),
                kind: match glyph.format {
                    GlyphFormat::Mask => KIND_COVERAGE,
                    GlyphFormat::Color => KIND_COLOR,
                },
                param: 0.0,
                row: u32::from(r),
            });
        }
        self.decorations(row, r, out);
        Ok(())
    }

    /// Decoration runs: consecutive cells with the same decoration and color share one quad,
    /// so patterns run unbroken.
    fn decorations(&self, row: &[SnapCell], r: u16, out: &mut Vec<Instance>) {
        let m = self.metrics;
        let (cw, ch) = (m.cell_width as f32, m.cell_height as f32);
        let t = m.underline_thickness as f32;
        let under = m.underline_position as f32;
        let kinds: [(u16, &[DecorationLine]); 7] = [
            (Attrs::UNDERLINE, &[(under, t, KIND_RECT)]),
            (
                Attrs::DOUBLE_UNDERLINE,
                &[(under - t, t, KIND_RECT), (under + t, t, KIND_RECT)],
            ),
            (
                Attrs::CURLY_UNDERLINE,
                &[(under - t, 3.0 * t + 1.0, KIND_CURLY)],
            ),
            (Attrs::DOTTED_UNDERLINE, &[(under, t, KIND_DOTTED)]),
            (Attrs::DASHED_UNDERLINE, &[(under, t, KIND_DASHED)]),
            (
                Attrs::STRIKE,
                &[(
                    m.strikeout_position as f32,
                    m.strikeout_thickness as f32,
                    KIND_RECT,
                )],
            ),
            (Attrs::OVERLINE, &[(0.0, t, KIND_RECT)]),
        ];
        for (bit, lines) in kinds {
            let mut c = 0;
            while c < row.len() {
                if !row[c].attrs.has(bit) {
                    c += 1;
                    continue;
                }
                let color = row[c].decoration;
                let start = c;
                while c < row.len() && row[c].attrs.has(bit) && row[c].decoration == color {
                    c += 1;
                }
                for &(top, height, kind) in lines {
                    let top = top.clamp(0.0, (ch - height).max(0.0));
                    out.push(Instance {
                        pos: [start as f32 * cw, top],
                        size: [(c - start) as f32 * cw, height],
                        uv: [0.0; 2],
                        color: color.to_array(),
                        kind,
                        param: t,
                        row: u32::from(r),
                    });
                }
            }
        }
    }

    fn glyph(
        &mut self,
        gpu: &Gpu,
        key: GlyphKey,
        stats: &mut PrepareStats,
    ) -> Result<Option<Glyph>, AtlasFull> {
        if let Some(&glyph) = self.glyphs.get(&key) {
            return Ok(glyph);
        }
        let mut buf = [0; 4];
        let image = self
            .fonts
            .rasterize(key.text.as_str(&mut buf), key.style, key.span);
        stats.rasterized += 1;
        let Some(image) = image else {
            self.glyphs.insert(key, None);
            return Ok(None);
        };
        let atlas = match image.format {
            GlyphFormat::Mask => &mut self.coverage,
            GlyphFormat::Color => &mut self.color,
        };
        let at = loop {
            if let Some(at) = atlas.packer.alloc(image.width, image.height) {
                break at;
            }
            if !atlas.grow(&gpu.device, &gpu.queue) {
                return Err(AtlasFull);
            }
            self.bind_group = None;
        };
        atlas.upload(&gpu.queue, at, image.width, image.height, &image.data);
        let glyph = Glyph {
            format: image.format,
            uv: [at.0 as f32, at.1 as f32],
            size: [image.width as f32, image.height as f32],
            offset: [image.left as f32, image.top as f32],
        };
        self.glyphs.insert(key, Some(glyph));
        Ok(Some(glyph))
    }

    fn upload_backgrounds(&self, gpu: &Gpu, rows: &[u16]) {
        let (Some(&first), Some(&last)) = (rows.iter().min(), rows.iter().max()) else {
            return;
        };
        let cols = usize::from(self.cols);
        let start = usize::from(first) * cols;
        let end = (usize::from(last) + 1) * cols;
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.backgrounds,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: u32::from(first),
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&self.bg_cells[start..end]),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * u32::from(self.cols)),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: u32::from(self.cols),
                height: u32::from(last - first) + 1,
                depth_or_array_layers: 1,
            },
        );
    }

    fn build_cursor(&mut self, gpu: &Gpu, snap: &GridSnapshot) {
        self.cursor_count = 0;
        let Some(cursor) = snap.cursor else { return };
        let m = self.metrics;
        let (cw, ch) = (m.cell_width as f32, m.cell_height as f32);
        let x = f32::from(cursor.col) * cw;
        let y = 0.0;
        let w = f32::from(cursor.width) * cw;
        // Strokes about one point thick at the font's scale.
        let stroke = (m.cell_width as f32 / 7.0).round().max(1.0);
        let rect = |x: f32, y: f32, w: f32, h: f32| Instance {
            pos: [x, y],
            size: [w, h],
            uv: [0.0; 2],
            color: cursor.color.to_array(),
            kind: KIND_RECT,
            param: 0.0,
            row: u32::from(cursor.row),
        };
        let parts: Vec<Instance> = match cursor.style {
            // Drawn by the cells it covers.
            CursorStyle::Block => Vec::new(),
            CursorStyle::Bar => vec![rect(x, y, stroke, ch)],
            CursorStyle::Underline => vec![rect(x, y + ch - stroke, w, stroke)],
            CursorStyle::HollowBlock => vec![
                rect(x, y, w, stroke),
                rect(x, y + ch - stroke, w, stroke),
                rect(x, y + stroke, stroke, ch - 2.0 * stroke),
                rect(x + w - stroke, y + stroke, stroke, ch - 2.0 * stroke),
            ],
        };
        if !parts.is_empty() {
            gpu.queue
                .write_buffer(&self.cursor_buffer, 0, bytemuck::cast_slice(&parts));
        }
        self.cursor_count = parts.len() as u32;
    }
}

fn create_backgrounds(device: &wgpu::Device, cols: u16, rows: u16) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("grid backgrounds"),
        size: wgpu::Extent3d {
            width: u32::from(cols.max(1)),
            height: u32::from(rows.max(1)),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

fn create_instances(device: &wgpu::Device, cols: u16, rows: u16) -> wgpu::Buffer {
    let count = usize::from(cols.max(1)) * usize::from(rows.max(1)) * SLOTS_PER_CELL;
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("grid instances"),
        size: (count * std::mem::size_of::<Instance>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}
