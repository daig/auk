//! The UI layer (§18.1): auk-ui's Vello scene (chrome, surface blocks, overlays) rendered into a
//! texture the size of the window, then composited over the grid pass in the same frame.
//!
//! Vello rasterizes with compute shaders into a storage texture, which a window's swap chain
//! can't be, so the layer keeps its own `Rgba8Unorm` texture. It is re-rendered only when the
//! scene changes ([`UiLayer::render`]); every frame blends it over the grids ([`UiLayer::draw`]).
//! Where the scene paints nothing the layer is transparent and the grids show through.

use crate::Gpu;

/// The layer's texture format: what Vello renders into.
pub const UI_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Why the layer couldn't render.
#[derive(Debug)]
pub struct UiError(pub String);

impl std::fmt::Display for UiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "UI layer: {}", self.0)
    }
}

impl std::error::Error for UiError {}

/// A Vello renderer, its target texture and the pipeline that blends it into a frame.
pub struct UiLayer {
    renderer: vello::Renderer,
    texture: Option<(wgpu::Texture, wgpu::TextureView, wgpu::BindGroup)>,
    size: (u32, u32),
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
    /// Whether the texture holds anything (a scene has been rendered at this size).
    drawn: bool,
}

impl std::fmt::Debug for UiLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UiLayer")
            .field("size", &self.size)
            .field("drawn", &self.drawn)
            .finish_non_exhaustive()
    }
}

impl UiLayer {
    /// A layer that composites into targets of `format`.
    pub fn new(gpu: &Gpu, format: wgpu::TextureFormat) -> Result<UiLayer, UiError> {
        let renderer = vello::Renderer::new(
            &gpu.device,
            vello::RendererOptions {
                use_cpu: false,
                antialiasing_support: vello::AaSupport::area_only(),
                num_init_threads: std::num::NonZeroUsize::new(1),
                pipeline_cache: None,
            },
        )
        .map_err(|e| UiError(e.to_string()))?;
        let device = &gpu.device;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ui layer"),
            source: wgpu::ShaderSource::Wgsl(include_str!("ui_layer.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ui layer"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ui layer"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ui layer"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    // The shader premultiplies Vello's straight-alpha output.
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        Ok(UiLayer {
            renderer,
            texture: None,
            size: (0, 0),
            layout,
            pipeline,
            drawn: false,
        })
    }

    /// The size the layer last rendered at, in pixels.
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// Renders `scene` (in physical pixels) into the layer's texture, `width`×`height` pixels.
    /// Areas the scene doesn't paint stay transparent. Submits its own GPU work.
    pub fn render(
        &mut self,
        gpu: &Gpu,
        scene: &vello::Scene,
        width: u32,
        height: u32,
    ) -> Result<(), UiError> {
        let (width, height) = (width.max(1), height.max(1));
        if self.texture.is_none() || self.size != (width, height) {
            let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("ui layer"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: UI_FORMAT,
                usage: wgpu::TextureUsages::STORAGE_BINDING
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("ui layer"),
                layout: &self.layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                }],
            });
            self.texture = Some((texture, view, bind_group));
            self.size = (width, height);
        }
        let (_, view, _) = self.texture.as_ref().expect("created above");
        self.renderer
            .render_to_texture(
                &gpu.device,
                &gpu.queue,
                scene,
                view,
                &vello::RenderParams {
                    base_color: vello::peniko::Color::TRANSPARENT,
                    width,
                    height,
                    antialiasing_method: vello::AaConfig::Area,
                },
            )
            .map_err(|e| UiError(e.to_string()))?;
        self.drawn = true;
        Ok(())
    }

    /// Blends the layer over whatever the pass has drawn. Draws nothing before the first
    /// [`UiLayer::render`].
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        let Some((_, _, bind_group)) = &self.texture else {
            return;
        };
        if !self.drawn {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}
