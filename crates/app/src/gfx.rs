//! A window's GPU side (§18.1): its swap chain on the view's `CAMetalLayer`, a grid renderer per
//! visible pane, and the UI layer (chrome and surfaces through Vello) composited over the grids
//! in the same frame. Also the frame clock: how long each frame took from the start of its work
//! to the GPU finishing it.

use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use auk_mirror::PaneId;
use auk_platform_macos::Window;
use auk_render::{
    Acquired, Gpu, GridRenderer, GridSnapshot, Rgba, RowSlot, UiLayer, WindowSurface,
};
use auk_text::{FontConfig, GridFonts};

/// The grid's font size, in points.
pub const FONT_SIZE: f32 = 13.0;

/// One pane's draw this frame, in physical pixels.
#[derive(Debug)]
pub struct PaneDraw {
    pub pane: PaneId,
    /// Where the grid's origin goes.
    pub origin: (f32, f32),
    /// The pane's clip rectangle: x, y, width, height.
    pub clip: (u32, u32, u32, u32),
    pub snapshot: GridSnapshot,
    /// Row slots for a mixed layout; `None` for a plain grid.
    pub slots: Option<Vec<RowSlot>>,
}

/// What one frame cost.
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameCost {
    /// CPU time preparing and encoding (snapshots excluded; the window measures those).
    pub encode: Duration,
    /// Time waiting for a drawable (pacing, not work).
    pub drawable_wait: Duration,
    /// Rows rebuilt across panes.
    pub damaged_rows: u32,
    pub presented: bool,
}

pub struct Gfx {
    gpu: Rc<Gpu>,
    surface: WindowSurface,
    renderers: BTreeMap<PaneId, GridRenderer>,
    ui: UiLayer,
    scale: f64,
    size: (u32, u32),
    /// A renderer kept around for metrics and to hand to the next new pane.
    spare: Option<GridRenderer>,
}

impl std::fmt::Debug for Gfx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Gfx")
            .field("size", &self.size)
            .field("scale", &self.scale)
            .field("panes", &self.renderers.len())
            .finish()
    }
}

pub fn grid_fonts(scale: f64) -> GridFonts {
    GridFonts::new(&FontConfig {
        size_pt: FONT_SIZE,
        scale: scale as f32,
        ..FontConfig::default()
    })
    .expect("a system monospace font")
}

impl Gfx {
    pub fn new(gpu: Rc<Gpu>, window: &Window, width: u32, height: u32, scale: f64) -> Gfx {
        // SAFETY: the layer belongs to the window, which outlives this (the window state that
        // owns it is dropped when the window closes).
        let surface =
            unsafe { WindowSurface::from_metal_layer(&gpu, window.metal_layer(), width, height) }
                .expect("a surface on the window's layer");
        let ui = UiLayer::new(&gpu, surface.format()).expect("the UI layer's pipelines");
        let spare = GridRenderer::new(&gpu, surface.format(), grid_fonts(scale));
        Gfx {
            gpu,
            surface,
            renderers: BTreeMap::new(),
            ui,
            scale,
            size: (width, height),
            spare: Some(spare),
        }
    }

    pub fn gpu(&self) -> &Rc<Gpu> {
        &self.gpu
    }

    /// The grid's cell metrics at the window's scale.
    pub fn metrics(&self) -> auk_text::CellMetrics {
        self.spare
            .as_ref()
            .or_else(|| self.renderers.values().next())
            .map(|r| r.metrics())
            .expect("a renderer exists")
    }

    pub fn resize(&mut self, width: u32, height: u32, scale: f64) {
        self.surface.resize(&self.gpu, width, height);
        self.size = (width, height);
        if scale != self.scale {
            self.scale = scale;
            for r in self.renderers.values_mut() {
                r.set_fonts(grid_fonts(scale));
            }
            if let Some(r) = self.spare.as_mut() {
                r.set_fonts(grid_fonts(scale));
            }
        }
    }

    /// Renders the UI layer's scene (only when it changed).
    pub fn render_ui(&mut self, scene: &auk_render::vello::Scene) {
        if let Err(err) = self.ui.render(&self.gpu, scene, self.size.0, self.size.1) {
            eprintln!("auk: {err}");
        }
    }

    /// Whether the UI layer needs rendering at the current size.
    pub fn ui_stale(&self) -> bool {
        self.ui.size() != self.size
    }

    /// Draws a frame: the background, each pane's grid clipped to its rectangle, then the UI
    /// layer over them. `on_done` runs once the GPU finishes the frame's work.
    pub fn draw(
        &mut self,
        background: Rgba,
        panes: &[PaneDraw],
        on_done: impl FnOnce() + Send + 'static,
    ) -> FrameCost {
        let start = Instant::now();
        let mut cost = FrameCost::default();
        // Renderers for panes no longer drawn go back to being spare (one) or are dropped.
        let drawn: Vec<PaneId> = panes.iter().map(|p| p.pane).collect();
        let gone: Vec<PaneId> = self
            .renderers
            .keys()
            .copied()
            .filter(|p| !drawn.contains(p))
            .collect();
        for p in gone {
            let r = self.renderers.remove(&p);
            if self.spare.is_none() {
                self.spare = r;
            }
        }
        for p in panes {
            if !self.renderers.contains_key(&p.pane) {
                let mut r = match self.spare.take() {
                    Some(r) => r,
                    None => {
                        GridRenderer::new(&self.gpu, self.surface.format(), grid_fonts(self.scale))
                    }
                };
                r.invalidate();
                self.renderers.insert(p.pane, r);
            }
            let r = self.renderers.get_mut(&p.pane).expect("inserted above");
            r.set_origin(p.origin.0, p.origin.1);
            r.set_row_slots(p.slots.clone());
            cost.damaged_rows += r.prepare(&self.gpu, &p.snapshot, self.size).damaged_rows;
        }
        if self.spare.is_none() {
            self.spare = Some(GridRenderer::new(
                &self.gpu,
                self.surface.format(),
                grid_fonts(self.scale),
            ));
        }
        let wait_start = Instant::now();
        let acquired = self.surface.acquire(&self.gpu);
        cost.drawable_wait = wait_start.elapsed();
        let Acquired::Frame(frame) = acquired else {
            cost.encode = start.elapsed() - cost.drawable_wait;
            return cost;
        };
        let target = frame.texture.create_view(&Default::default());
        let mut encoder = self.gpu.device.create_command_encoder(&Default::default());
        {
            use auk_render::wgpu;
            let c = |v: u8| f64::from(v) / 255.0;
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("window"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: c(background.0),
                            g: c(background.1),
                            b: c(background.2),
                            a: c(background.3),
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            for p in panes {
                let (x, y, w, h) = p.clip;
                let (x, y) = (x.min(self.size.0), y.min(self.size.1));
                let w = w.min(self.size.0 - x);
                let h = h.min(self.size.1 - y);
                if w == 0 || h == 0 {
                    continue;
                }
                pass.set_scissor_rect(x, y, w, h);
                if let Some(r) = self.renderers.get(&p.pane) {
                    r.draw(&mut pass);
                }
            }
            pass.set_scissor_rect(0, 0, self.size.0, self.size.1);
            self.ui.draw(&mut pass);
        }
        self.gpu.queue.submit([encoder.finish()]);
        self.gpu.queue.on_submitted_work_done(on_done);
        self.gpu.queue.present(frame);
        cost.presented = true;
        cost.encode = start.elapsed() - cost.drawable_wait;
        cost
    }
}

/// Frame times from the start of a frame's work to the GPU finishing it, kept for the last
/// frames. A thread waits on the GPU so completions are timed as they happen.
#[derive(Debug, Clone)]
pub struct FrameClock {
    inner: Arc<ClockInner>,
}

#[derive(Debug)]
struct ClockInner {
    times: Mutex<VecDeque<Duration>>,
    pending: Mutex<u64>,
    wake: Condvar,
}

/// How many frame times the clock keeps.
const KEEP: usize = 4096;

impl FrameClock {
    pub fn new(gpu: &Gpu) -> FrameClock {
        let inner = Arc::new(ClockInner {
            times: Mutex::new(VecDeque::new()),
            pending: Mutex::new(0),
            wake: Condvar::new(),
        });
        let device = gpu.device.clone();
        let waiter = inner.clone();
        let _ = std::thread::Builder::new()
            .name("auk gpu clock".into())
            .spawn(move || {
                loop {
                    {
                        let mut pending = waiter.pending.lock().unwrap_or_else(|e| e.into_inner());
                        while *pending == 0 {
                            pending = waiter.wake.wait(pending).unwrap_or_else(|e| e.into_inner());
                        }
                        *pending = 0;
                    }
                    let _ = device.poll(auk_render::wgpu::PollType::Wait {
                        submission_index: None,
                        timeout: Some(Duration::from_secs(2)),
                    });
                }
            });
        FrameClock { inner }
    }

    /// A callback for [`Gfx::draw`]'s `on_done`: records the time since `start`.
    pub fn done_callback(&self, start: Instant) -> impl FnOnce() + Send + 'static {
        let inner = self.inner.clone();
        move || {
            let elapsed = start.elapsed();
            let mut times = inner.times.lock().unwrap_or_else(|e| e.into_inner());
            times.push_back(elapsed);
            if times.len() > KEEP {
                times.pop_front();
            }
        }
    }

    /// Tells the waiter thread a frame was submitted.
    pub fn submitted(&self) {
        *self.inner.pending.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        self.inner.wake.notify_one();
    }

    pub fn times(&self) -> Vec<Duration> {
        self.inner
            .times
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .copied()
            .collect()
    }

    pub fn reset(&self) {
        self.inner
            .times
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }
}
