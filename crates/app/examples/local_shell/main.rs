//! The development harness: one window running a local login shell in a PTY, parsed by
//! `auk_term::Terminal`, drawn by the grid renderer, with keys encoded by `auk_term::input`.
//! The real app talks to the daemon instead; this exists to exercise the platform layer,
//! text stack and renderer end to end.
//!
//! `cargo run -p auk-app --example local_shell [--option-as-alt left|right|both]`
//!
//! Set `AUK_FRAME_STATS=1` to print CPU frame times (snapshot, upload, encode, present) every
//! 120 frames. `--smoke` redraws every frame for three seconds, prints what it presented and
//! quits: a check that the window, display link and surface work.

mod pty;

use std::io::Read;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use auk_platform_macos::{
    Appearance, Clipboard, Command, Event, OptionAsAlt, Preedit, Size, Window, WindowAttributes,
    WindowHandler, run,
};
use auk_render::{Acquired, Attrs, Gpu, GridRenderer, GridSnapshot, Theme, View, WindowSurface};
use auk_term::{Config, Terminal, input};
use auk_text::{FontConfig, GridFonts};

/// Space around the grid, in points.
const PADDING: f64 = 8.0;
const FONT_SIZE: f32 = 13.0;

fn main() {
    let option_as_alt = match std::env::args()
        .skip_while(|a| a != "--option-as-alt")
        .nth(1)
        .as_deref()
    {
        Some("left") => OptionAsAlt::Left,
        Some("right") => OptionAsAlt::Right,
        Some("both") => OptionAsAlt::Both,
        _ => OptionAsAlt::None,
    };
    let gpu = Gpu::new_blocking().unwrap_or_else(|e| {
        eprintln!("local_shell: {e}");
        std::process::exit(1);
    });
    run(move |app| {
        let attrs = WindowAttributes {
            title: "Auk (local shell)".into(),
            option_as_alt,
            ..Default::default()
        };
        app.create_window(attrs, Box::new(Shell::new(gpu, app.clipboard())));
    });
}

/// Output from the shell, handed from the reader thread to the main thread.
#[derive(Default)]
struct Output {
    bytes: Vec<u8>,
    eof: bool,
}

struct Shell {
    gpu: Gpu,
    clipboard: Clipboard,
    /// Created on the first resize, when the window and its scale are known.
    gfx: Option<Gfx>,
    term: Terminal,
    pty: Option<pty::Pty>,
    output: Arc<Mutex<Output>>,
    theme: Theme,
    view: View,
    preedit: Option<Preedit>,
    size: Option<Size>,
    /// Fractional scroll, in rows.
    scroll_rows: f64,
    /// (work, drawable wait) per frame, for `AUK_FRAME_STATS`.
    frame_times: Vec<(Duration, Duration)>,
    print_stats: bool,
    /// `--smoke`: when the run started, and frames presented so far.
    smoke: Option<(Instant, u32)>,
}

struct Gfx {
    surface: WindowSurface,
    renderer: GridRenderer,
    scale: f64,
}

impl Shell {
    fn new(gpu: Gpu, clipboard: Clipboard) -> Shell {
        Shell {
            gpu,
            clipboard,
            gfx: None,
            term: Terminal::new(80, 24, Config::default()),
            pty: None,
            output: Arc::default(),
            theme: Theme::dark(),
            view: View::default(),
            preedit: None,
            size: None,
            scroll_rows: 0.0,
            frame_times: Vec::new(),
            print_stats: std::env::var_os("AUK_FRAME_STATS").is_some()
                || std::env::args().any(|a| a == "--smoke"),
            smoke: std::env::args()
                .any(|a| a == "--smoke")
                .then(|| (Instant::now(), 0)),
        }
    }

    fn send(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if let Some(pty) = self.pty.as_mut()
            && let Err(e) = pty.write(bytes)
        {
            eprintln!("local_shell: write to the shell: {e}");
        }
    }

    fn resized(&mut self, window: &Window, size: Size) {
        self.size = Some(size);
        let (w, h) = size.physical();
        let fonts = || {
            GridFonts::new(&FontConfig {
                size_pt: FONT_SIZE,
                scale: size.scale as f32,
                ..FontConfig::default()
            })
            .expect("a system monospace font")
        };
        match self.gfx.as_mut() {
            None => {
                // SAFETY: the layer belongs to the window, which outlives this handler.
                let surface = unsafe {
                    WindowSurface::from_metal_layer(&self.gpu, window.metal_layer(), w, h)
                }
                .expect("a surface on the window's layer");
                let renderer = GridRenderer::new(&self.gpu, surface.format(), fonts());
                self.gfx = Some(Gfx {
                    surface,
                    renderer,
                    scale: size.scale,
                });
            }
            Some(gfx) => {
                gfx.surface.resize(&self.gpu, w, h);
                if gfx.scale != size.scale {
                    gfx.renderer.set_fonts(fonts());
                    gfx.scale = size.scale;
                }
            }
        }
        let gfx = self.gfx.as_mut().expect("just created");
        let pad = (PADDING * size.scale).round() as u32;
        gfx.renderer.set_origin(pad as f32, pad as f32);
        let (cols, rows) = gfx.renderer.grid_size_for(w, h, pad);
        let m = gfx.renderer.metrics();
        let cell_px = (m.cell_width as u16, m.cell_height as u16);
        self.term.set_facts(self.theme.facts(cell_px));
        if (cols, rows) != (self.term.cols(), self.term.rows()) {
            self.term.resize(cols, rows);
        }
        match &self.pty {
            Some(pty) => {
                if let Err(e) = pty.resize(cols, rows, cell_px) {
                    eprintln!("local_shell: resize: {e}");
                }
            }
            None => self.start_shell(window, cols, rows, cell_px),
        }
        window.request_frame();
    }

    fn start_shell(&mut self, window: &Window, cols: u16, rows: u16, cell_px: (u16, u16)) {
        let pty = match pty::Pty::spawn(cols, rows, cell_px) {
            Ok(pty) => pty,
            Err(e) => {
                eprintln!("local_shell: spawning the shell: {e}");
                window.close();
                return;
            }
        };
        let mut reader = pty.reader().expect("a second handle on the PTY");
        let output = Arc::clone(&self.output);
        let waker = window.waker();
        std::thread::spawn(move || {
            let mut buf = vec![0; 64 * 1024];
            loop {
                let n = reader.read(&mut buf).unwrap_or(0);
                let mut out = output.lock().expect("reader lock");
                if n == 0 {
                    out.eof = true;
                } else {
                    out.bytes.extend_from_slice(&buf[..n]);
                }
                drop(out);
                waker.wake();
                if n == 0 {
                    break;
                }
            }
        });
        self.pty = Some(pty);
    }

    fn shell_output(&mut self, window: &Window) {
        let (bytes, eof) = {
            let mut out = self.output.lock().expect("reader lock");
            (std::mem::take(&mut out.bytes), out.eof)
        };
        if !bytes.is_empty() {
            let mut replies = false;
            for event in self.term.feed(&bytes) {
                match event {
                    auk_term::Event::Title(title) => window.set_title(&title),
                    auk_term::Event::Replies => replies = true,
                    _ => {}
                }
            }
            if replies {
                let answer = self.term.take_replies();
                self.send(&answer);
            }
            window.request_frame();
        }
        if eof {
            window.close();
        }
    }

    fn frame(&mut self, window: &Window) {
        let start = Instant::now();
        let Some(gfx) = self.gfx.as_mut() else { return };
        let mut snap = GridSnapshot::from_terminal(&self.term, &self.theme, &self.view);
        let cursor = self.term.cursor();
        if let Some(preedit) = &self.preedit {
            // The composition draws at the cursor, underlined, until it's committed.
            let fg = self.theme.foreground;
            let bg = self.theme.background;
            snap.write(
                cursor.col,
                cursor.row.min(snap.rows.saturating_sub(1)),
                &preedit.text,
                fg,
                bg,
                Attrs::UNDERLINE,
            );
            snap.cursor = None;
        }
        let (w, h) = self.size.map_or((1, 1), |s| s.physical());
        gfx.renderer.prepare(&self.gpu, &snap, (w, h));
        let acquire_start = Instant::now();
        let acquired = gfx.surface.acquire(&self.gpu);
        // Waiting for a free drawable is pacing, not frame work.
        let waited = acquire_start.elapsed();
        match acquired {
            Acquired::Frame(frame) => {
                let target = frame.texture.create_view(&Default::default());
                let mut encoder = self.gpu.device.create_command_encoder(&Default::default());
                gfx.renderer
                    .render(&mut encoder, &target, snap.background, None);
                self.gpu.queue.submit([encoder.finish()]);
                self.gpu.queue.present(frame);
                if let Some((_, presented)) = self.smoke.as_mut() {
                    *presented += 1;
                }
            }
            Acquired::Skip => window.request_frame(),
        }
        // Input methods put their candidates under the cursor's cell.
        if let Some(size) = self.size {
            let m = gfx.renderer.metrics();
            let (cw, ch) = (
                f64::from(m.cell_width) / size.scale,
                f64::from(m.cell_height) / size.scale,
            );
            window.set_ime_cursor_area(
                PADDING + f64::from(cursor.col) * cw,
                PADDING + f64::from(cursor.row) * ch,
                cw,
                ch,
            );
        }
        self.record_frame(start.elapsed() - waited, waited);
        if let Some((started, presented)) = self.smoke {
            if started.elapsed() < Duration::from_secs(3) {
                // Every row changes, as in a full-screen repaint.
                self.view.focused = presented % 2 == 0;
                self.theme = if presented % 2 == 0 {
                    Theme::dark()
                } else {
                    Theme::light()
                };
                window.request_frame();
            } else {
                let (w, h) = self.size.map_or((0, 0), |s| s.physical());
                println!(
                    "smoke: presented {presented} frames in 3 s at {w}x{h} px, grid {}x{}",
                    self.term.cols(),
                    self.term.rows()
                );
                window.close();
            }
        }
    }

    fn record_frame(&mut self, work: Duration, waited: Duration) {
        if !self.print_stats {
            return;
        }
        self.frame_times.push((work, waited));
        if self.frame_times.len() == 120 {
            let ms = |d: Duration| d.as_secs_f64() * 1000.0;
            let mut work: Vec<Duration> = self.frame_times.iter().map(|f| f.0).collect();
            let mut waited: Vec<Duration> = self.frame_times.iter().map(|f| f.1).collect();
            work.sort();
            waited.sort();
            eprintln!(
                "frame CPU work ms: p50 {:.3} p95 {:.3} max {:.3} | drawable wait ms: p50 {:.3} p95 {:.3}",
                ms(work[60]),
                ms(work[114]),
                ms(work[119]),
                ms(waited[60]),
                ms(waited[114]),
            );
            self.frame_times.clear();
        }
    }

    fn scroll(&mut self, window: &Window, dy: f64, precise: bool) {
        let Some(gfx) = &self.gfx else { return };
        let rows = if precise {
            let size = self.size.map_or(1.0, |s| s.scale);
            dy * size / f64::from(gfx.renderer.metrics().cell_height)
        } else {
            dy
        };
        self.scroll_rows += rows;
        let whole = self.scroll_rows.trunc();
        self.scroll_rows -= whole;
        let scroll = (self.view.scroll as f64 + whole).clamp(0.0, self.term.history_len() as f64);
        if scroll as usize != self.view.scroll {
            self.view.scroll = scroll as usize;
            window.request_frame();
        }
    }
}

impl WindowHandler for Shell {
    fn event(&mut self, window: &Window, event: Event) {
        match event {
            Event::Resized(size) => self.resized(window, size),
            Event::Wake => self.shell_output(window),
            Event::Frame => self.frame(window),
            Event::Key(key) => {
                // ⌘ keys belong to the app, not the program.
                if key.event.mods.meta {
                    return;
                }
                let bytes = input::encode_key(&key.event, &self.term.modes());
                self.send(&bytes);
                self.view.scroll = 0;
            }
            Event::Text(text) => {
                self.send(text.as_bytes());
                self.view.scroll = 0;
            }
            Event::Preedit(preedit) => {
                self.preedit = preedit;
                window.request_frame();
            }
            Event::Command(Command::Paste) => {
                if let Some(text) = self.clipboard.text() {
                    let bytes = input::encode_paste(&text, &self.term.modes());
                    self.send(&bytes);
                }
            }
            Event::Command(_) => {}
            Event::Scroll(s) => self.scroll(window, s.dy, s.precise),
            Event::Focus(focused) => {
                self.view.focused = focused;
                let bytes = input::encode_focus(focused, &self.term.modes());
                self.send(&bytes);
                window.request_frame();
            }
            Event::Appearance(appearance) => {
                self.theme = match appearance {
                    Appearance::Dark => Theme::dark(),
                    Appearance::Light => Theme::light(),
                };
                window.request_frame();
            }
            Event::CloseRequested => window.close(),
            Event::Closed => self.pty = None,
            Event::ModifiersChanged(_)
            | Event::Mouse(_)
            | Event::Menu(_)
            | Event::SystemSettings => {}
        }
    }
}
