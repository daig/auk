//! GPU time of a render pass, from timestamp queries.

use std::time::Duration;

use crate::Gpu;

/// Measures how long the GPU spends on one render pass: pass it to
/// [`crate::GridRenderer::render`], call [`GpuTimer::resolve`] on the same encoder, submit, then
/// [`GpuTimer::read`].
#[derive(Debug)]
pub struct GpuTimer {
    queries: wgpu::QuerySet,
    resolved: wgpu::Buffer,
    readback: wgpu::Buffer,
    /// Nanoseconds per timestamp tick.
    period: f64,
}

impl GpuTimer {
    /// `None` when the adapter has no timestamp queries.
    pub fn new(gpu: &Gpu) -> Option<GpuTimer> {
        if !gpu.timestamps {
            return None;
        }
        let queries = gpu.device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("pass timer"),
            ty: wgpu::QueryType::Timestamp,
            count: 2,
        });
        let buffer = |label, usage| {
            gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: 16,
                usage,
                mapped_at_creation: false,
            })
        };
        Some(GpuTimer {
            queries,
            resolved: buffer(
                "pass timer resolve",
                wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            ),
            readback: buffer(
                "pass timer readback",
                wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            ),
            period: f64::from(gpu.queue.get_timestamp_period()),
        })
    }

    pub(crate) fn pass_writes(&self) -> wgpu::RenderPassTimestampWrites<'_> {
        wgpu::RenderPassTimestampWrites {
            query_set: &self.queries,
            beginning_of_pass_write_index: Some(0),
            end_of_pass_write_index: Some(1),
        }
    }

    /// Copies the pass's timestamps out; encode after the pass.
    pub fn resolve(&self, encoder: &mut wgpu::CommandEncoder) {
        encoder.resolve_query_set(&self.queries, 0..2, &self.resolved, 0);
        encoder.copy_buffer_to_buffer(&self.resolved, 0, &self.readback, 0, 16);
    }

    /// Waits for the GPU and returns the pass's duration.
    pub fn read(&self, gpu: &Gpu) -> Option<Duration> {
        let data = crate::offscreen::map_read(gpu, &self.readback).ok()?;
        let t: [u64; 2] = bytemuck::pod_read_unaligned(&data);
        t[1].checked_sub(t[0])
            .map(|t| Duration::from_nanos((t as f64 * self.period) as u64))
    }
}
