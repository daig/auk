//! Measures the grid pass on a full 1200×800-point window at 2× (2400×1600 pixels), offscreen:
//! CPU frame time (snapshot upload, encoding and submission) and GPU time (timestamp queries)
//! for full redraws and for single-row damage.
//!
//! `cargo run --release -p auk-render --example grid_bench [frames]`

use std::time::{Duration, Instant};

use auk_render::{
    Attrs, Gpu, GpuTimer, GridRenderer, GridSnapshot, OFFSCREEN_FORMAT, OffscreenTarget, Rgba,
    Theme,
};
use auk_text::{FontConfig, GridFonts};

const WIDTH_PT: u32 = 1200;
const HEIGHT_PT: u32 = 800;
const SCALE: f32 = 2.0;

fn main() {
    let frames: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(300);
    let gpu = Gpu::new_blocking().expect("GPU");
    let fonts = GridFonts::new(&FontConfig {
        size_pt: 13.0,
        scale: SCALE,
        ..FontConfig::default()
    })
    .expect("font");
    let (w, h) = (
        (WIDTH_PT as f32 * SCALE) as u32,
        (HEIGHT_PT as f32 * SCALE) as u32,
    );
    let mut renderer = GridRenderer::new(&gpu, OFFSCREEN_FORMAT, fonts);
    let (cols, rows) = renderer.grid_size_for(w, h, 8);
    renderer.set_origin(8.0, 8.0);
    let target = OffscreenTarget::new(&gpu, w, h);
    let timer = GpuTimer::new(&gpu);
    let theme = Theme::dark();
    println!(
        "{}; {} {}x{} px cells; grid {cols}x{rows} in {w}x{h} px; {frames} frames per case",
        gpu.adapter.get_info().name,
        renderer.fonts().family_name(),
        renderer.metrics().cell_width,
        renderer.metrics().cell_height,
    );

    // Warm the atlas with every glyph the cases use.
    let mut snap = page(cols, rows, &theme, 0);
    frame(&gpu, &mut renderer, &snap, &target, timer.as_ref(), (w, h));

    let mut full = Samples::default();
    for i in 0..frames {
        // Every row changes: what a scroll or a full-screen program repaint costs.
        snap = page(cols, rows, &theme, i + 1);
        full.push(frame(
            &gpu,
            &mut renderer,
            &snap,
            &target,
            timer.as_ref(),
            (w, h),
        ));
    }
    let mut row = Samples::default();
    for i in 0..frames {
        // One row changes: typing at a prompt.
        let r = (i % usize::from(rows)) as u16;
        let text = format!("$ echo typing {i:>6} ");
        snap.write(0, r, &text, theme.foreground, theme.background, 0);
        row.push(frame(
            &gpu,
            &mut renderer,
            &snap,
            &target,
            timer.as_ref(),
            (w, h),
        ));
    }
    let mut idle = Samples::default();
    for _ in 0..frames {
        idle.push(frame(
            &gpu,
            &mut renderer,
            &snap,
            &target,
            timer.as_ref(),
            (w, h),
        ));
    }
    // From a terminal: each frame scrolls one line of colored output in, so every row changes,
    // and the snapshot is built from the terminal core (as the app does).
    let mut term = auk_term::Terminal::new(cols, rows, auk_term::Config::default());
    let mut scroll = Samples::default();
    let mut snapshot_times = Vec::new();
    for i in 0..frames {
        let line = format!(
            "\x1b[3{}m{i:>6}\x1b[0m \x1b[1msrc/render/grid.rs\x1b[0m:{}: \x1b[4mwarning\x1b[0m: the quick brown fox jumps over the lazy dog 漢字 {}\r\n",
            1 + i % 6,
            i * 7 % 997,
            "-".repeat(i % 40)
        );
        term.feed(line.as_bytes());
        let start = Instant::now();
        let snap = GridSnapshot::from_terminal(&term, &theme, &auk_render::View::default());
        let built = start.elapsed();
        snapshot_times.push(built);
        let mut f = frame(&gpu, &mut renderer, &snap, &target, timer.as_ref(), (w, h));
        f.cpu += built;
        scroll.push(f);
    }
    let mut snapshots = Samples {
        cpu: snapshot_times,
        ..Samples::default()
    };
    full.report("full redraw (all rows changed)");
    scroll.report("terminal scroll (snapshot incl.)");
    snapshots.report("  of which snapshot build");
    row.report("single-row damage");
    idle.report("no damage");
}

/// A screen of mixed text: varied colors, bold, underline, wide characters on some rows.
fn page(cols: u16, rows: u16, theme: &Theme, seed: usize) -> GridSnapshot {
    let mut snap = GridSnapshot::new(cols, rows, theme);
    let words = [
        "fn",
        "main()",
        "{",
        "let",
        "x",
        "=",
        "vec![1,",
        "2,",
        "3];",
        "println!(\"{x:?}\");",
        "}",
        "//",
        "src/lib.rs:42:7",
        "warning:",
        "unused",
        "ok",
    ];
    for r in 0..rows {
        let mut col = 0u16;
        let mut k = seed * 7 + usize::from(r) * 13;
        while col < cols {
            let word = words[k % words.len()];
            let fg = theme.palette[1 + k % 14];
            let attrs = match k % 11 {
                0 => Attrs::BOLD,
                1 => Attrs::UNDERLINE,
                2 => Attrs::ITALIC,
                _ => 0,
            };
            let bg = if k.is_multiple_of(17) {
                theme.palette[8]
            } else {
                theme.background
            };
            snap.write(col, r, word, fg, bg, attrs);
            col += word.chars().count() as u16 + 1;
            k += 3;
        }
        if r % 5 == 0 {
            snap.write(
                cols.saturating_sub(12),
                r,
                "漢字かな🙂",
                Rgba::rgb(220, 220, 220),
                theme.background,
                0,
            );
        }
    }
    snap
}

struct Frame {
    cpu: Duration,
    gpu: Option<Duration>,
    damaged: u32,
}

fn frame(
    gpu: &Gpu,
    renderer: &mut GridRenderer,
    snap: &GridSnapshot,
    target: &OffscreenTarget,
    timer: Option<&GpuTimer>,
    size: (u32, u32),
) -> Frame {
    let start = Instant::now();
    let stats = renderer.prepare(gpu, snap, size);
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    renderer.render(&mut encoder, target.view(), snap.background, timer);
    if let Some(timer) = timer {
        timer.resolve(&mut encoder);
    }
    gpu.queue.submit([encoder.finish()]);
    let cpu = start.elapsed();
    let gpu_time = match timer {
        Some(t) => t.read(gpu),
        None => {
            gpu.wait_idle();
            None
        }
    };
    Frame {
        cpu,
        gpu: gpu_time,
        damaged: stats.damaged_rows,
    }
}

#[derive(Default)]
struct Samples {
    cpu: Vec<Duration>,
    gpu: Vec<Duration>,
    damaged: Vec<u32>,
}

impl Samples {
    fn push(&mut self, f: Frame) {
        self.cpu.push(f.cpu);
        self.gpu.extend(f.gpu);
        self.damaged.push(f.damaged);
    }

    fn report(&mut self, name: &str) {
        let pct = |v: &mut Vec<Duration>, p: f64| -> f64 {
            v.sort();
            if v.is_empty() {
                return f64::NAN;
            }
            let i = ((v.len() - 1) as f64 * p).round() as usize;
            v[i].as_secs_f64() * 1000.0
        };
        let damaged = self.damaged.iter().sum::<u32>() as f64 / self.damaged.len().max(1) as f64;
        println!(
            "{name:32} rows damaged {damaged:5.1} | CPU ms p50 {:.3} p95 {:.3} max {:.3} | GPU ms p50 {:.3} p95 {:.3} max {:.3}",
            pct(&mut self.cpu, 0.5),
            pct(&mut self.cpu, 0.95),
            pct(&mut self.cpu, 1.0),
            pct(&mut self.gpu, 0.5),
            pct(&mut self.gpu, 0.95),
            pct(&mut self.gpu, 1.0),
        );
    }
}
