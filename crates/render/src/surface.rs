//! A window's surface: a wgpu surface on the window's CAMetalLayer (or, in the browser, its
//! canvas).

use crate::{Gpu, GpuError};

/// A window's swap chain.
#[derive(Debug)]
pub struct WindowSurface {
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
}

/// What [`WindowSurface::acquire`] got.
#[derive(Debug)]
pub enum Acquired {
    Frame(wgpu::SurfaceTexture),
    /// Nothing to draw into now (occluded, timed out); try on the next frame.
    Skip,
}

impl WindowSurface {
    /// A surface on a `CAMetalLayer`, `width`×`height` pixels.
    ///
    /// # Safety
    ///
    /// `layer` must point at a `CAMetalLayer` that outlives the surface.
    #[cfg(target_os = "macos")]
    pub unsafe fn from_metal_layer(
        gpu: &Gpu,
        layer: std::ptr::NonNull<std::ffi::c_void>,
        width: u32,
        height: u32,
    ) -> Result<WindowSurface, GpuError> {
        // SAFETY: the caller guarantees the layer and its lifetime.
        let surface = unsafe {
            gpu.instance
                .create_surface_unsafe(wgpu::SurfaceTargetUnsafe::CoreAnimationLayer(
                    layer.as_ptr(),
                ))
        }
        .map_err(GpuError::Surface)?;
        Ok(Self::configure_new(gpu, surface, width, height))
    }

    /// A surface on a window's raw handles.
    ///
    /// # Safety
    ///
    /// The handles must be valid and outlive the surface.
    pub unsafe fn from_raw_handles(
        gpu: &Gpu,
        display: Option<raw_window_handle::RawDisplayHandle>,
        window: raw_window_handle::RawWindowHandle,
        width: u32,
        height: u32,
    ) -> Result<WindowSurface, GpuError> {
        // SAFETY: the caller guarantees the handles and their lifetime.
        let surface = unsafe {
            gpu.instance
                .create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                    raw_display_handle: display,
                    raw_window_handle: window,
                })
        }
        .map_err(GpuError::Surface)?;
        Ok(Self::configure_new(gpu, surface, width, height))
    }

    fn configure_new(
        gpu: &Gpu,
        surface: wgpu::Surface<'static>,
        width: u32,
        height: u32,
    ) -> WindowSurface {
        let caps = surface.get_capabilities(&gpu.adapter);
        // Colors are sRGB values blended as they are (like AppKit's text), so prefer a format
        // without automatic sRGB conversion.
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .unwrap_or(caps.formats[0]);
        let mut config = surface
            .get_default_config(&gpu.adapter, width.max(1), height.max(1))
            .expect("the surface is supported by the adapter it was made with");
        config.format = format;
        config.present_mode = wgpu::PresentMode::Fifo;
        config.desired_maximum_frame_latency = 1;
        config.alpha_mode = wgpu::CompositeAlphaMode::Opaque;
        surface.configure(&gpu.device, &config);
        WindowSurface { surface, config }
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        self.config.format
    }

    pub fn size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    /// Resizes the swap chain to `width`×`height` pixels.
    pub fn resize(&mut self, gpu: &Gpu, width: u32, height: u32) {
        let (width, height) = (width.max(1), height.max(1));
        if (width, height) != (self.config.width, self.config.height) {
            self.config.width = width;
            self.config.height = height;
            self.surface.configure(&gpu.device, &self.config);
        }
    }

    /// The next frame to draw into.
    pub fn acquire(&mut self, gpu: &Gpu) -> Acquired {
        for _ in 0..2 {
            match self.surface.get_current_texture() {
                wgpu::CurrentSurfaceTexture::Success(frame) => return Acquired::Frame(frame),
                wgpu::CurrentSurfaceTexture::Suboptimal(frame) => {
                    // Draw this one; reconfigure for the next.
                    self.surface.configure(&gpu.device, &self.config);
                    return Acquired::Frame(frame);
                }
                wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                    self.surface.configure(&gpu.device, &self.config);
                }
                _ => return Acquired::Skip,
            }
        }
        Acquired::Skip
    }
}
