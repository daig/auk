//! The wgpu renderer (spec §18.1): the GPU device, window surfaces, the cell grid's pass with its
//! glyph atlases, and offscreen rendering with PNG readback.
//!
//! Drawing a terminal:
//! 1. [`GridSnapshot::from_terminal`] resolves the visible rows of an `auk_term::Terminal`
//!    (colors, attributes, cursor, selection) under a [`Theme`] and a [`View`].
//! 2. [`GridRenderer::prepare`] uploads the rows that changed since the last snapshot.
//! 3. [`GridRenderer::render`] encodes the pass into a window's frame ([`WindowSurface`]) or an
//!    [`OffscreenTarget`].

mod atlas;
mod gpu;
mod grid;
mod offscreen;
mod snapshot;
mod surface;
mod theme;
mod timing;

pub use gpu::{Gpu, GpuError};
pub use grid::{GridRenderer, PrepareStats};
pub use offscreen::{Image, OFFSCREEN_FORMAT, OffscreenTarget, ReadbackError};
pub use snapshot::{
    Attrs, CellText, CursorStyle, GridSnapshot, Selection, SnapCell, SnapCursor, View,
};
pub use surface::{Acquired, WindowSurface};
pub use theme::{
    OVERRIDE_BACKGROUND, OVERRIDE_CURSOR, OVERRIDE_FOREGROUND, Rgba, Theme, xterm_palette,
};
pub use timing::GpuTimer;

/// Renders `snapshot` into a new offscreen target of `width`×`height` pixels and reads it back.
/// `renderer` must have been made with [`OFFSCREEN_FORMAT`].
pub fn render_to_image(
    gpu: &Gpu,
    renderer: &mut GridRenderer,
    snapshot: &GridSnapshot,
    width: u32,
    height: u32,
) -> Result<Image, ReadbackError> {
    let target = OffscreenTarget::new(gpu, width, height);
    renderer.prepare(gpu, snapshot, (width, height));
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    renderer.render(&mut encoder, target.view(), snapshot.background, None);
    gpu.queue.submit([encoder.finish()]);
    target.read(gpu)
}
