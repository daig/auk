//! The wgpu device a process renders with, shared by its windows and offscreen targets.

use std::fmt;

/// An adapter, device and queue. One per process; every window and offscreen target uses it.
pub struct Gpu {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    /// Whether render passes can write GPU timestamps ([`crate::GpuTimer`]).
    pub timestamps: bool,
}

impl fmt::Debug for Gpu {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Gpu")
            .field("adapter", &self.adapter.get_info().name)
            .field("backend", &self.adapter.get_info().backend)
            .field("timestamps", &self.timestamps)
            .finish()
    }
}

#[derive(Debug)]
pub enum GpuError {
    NoAdapter(wgpu::RequestAdapterError),
    Device(wgpu::RequestDeviceError),
    Surface(wgpu::CreateSurfaceError),
}

impl fmt::Display for GpuError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GpuError::NoAdapter(e) => write!(f, "no GPU adapter: {e}"),
            GpuError::Device(e) => write!(f, "GPU device: {e}"),
            GpuError::Surface(e) => write!(f, "surface: {e}"),
        }
    }
}

impl std::error::Error for GpuError {}

impl Gpu {
    /// The high-performance adapter, with timestamps when it has them. Native callers block on
    /// this with `pollster`; the browser awaits it.
    pub async fn new() -> Result<Gpu, GpuError> {
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        desc.backends =
            wgpu::Backends::METAL | wgpu::Backends::BROWSER_WEBGPU | wgpu::Backends::VULKAN;
        let instance = wgpu::Instance::new(desc);
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: None,
                ..Default::default()
            })
            .await
            .map_err(GpuError::NoAdapter)?;
        let timestamps = adapter.features().contains(wgpu::Features::TIMESTAMP_QUERY);
        let required_features = if timestamps {
            wgpu::Features::TIMESTAMP_QUERY
        } else {
            wgpu::Features::empty()
        };
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("auk"),
                required_features,
                required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
                ..Default::default()
            })
            .await
            .map_err(GpuError::Device)?;
        Ok(Gpu {
            instance,
            adapter,
            device,
            queue,
            timestamps,
        })
    }

    /// [`Gpu::new`], blocking. Native only.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn new_blocking() -> Result<Gpu, GpuError> {
        pollster::block_on(Gpu::new())
    }

    /// Blocks until the GPU has finished all submitted work.
    pub fn wait_idle(&self) {
        // A wait without a timeout only fails if the device is lost, which later calls report.
        let _ = self.device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        });
    }
}
