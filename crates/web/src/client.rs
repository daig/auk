//! The wasm entry point (§10.3): the page's WebSocket, WebGPU canvas and keyboard.
//!
//! [`start`] fetches the grid font from the server, opens WebGPU on the page's canvas, says
//! hello over `/ws` and attaches to the window key (`?window=`, default `main`). Each frame draws
//! the shown tab's panes (`?pane=` picks the tab holding that pane) with `auk-render`'s grid
//! pass, glyphs rasterized by swash (§18.3). Keys are encoded for the pane's modes and sent as
//! input; paste, input-method text and resizes go through the same session ([`Session`]).

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use auk_mirror::{ClientFacts, PaneId};
use auk_render::{Acquired, Gpu, GridRenderer, GridSnapshot, Theme, View, WindowSurface, wgpu};
use auk_text::{Backend, FontConfig, GridFonts};
use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WebCanvasWindowHandle, WebDisplayHandle,
};
use serde_json::json;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{
    BinaryType, ClipboardEvent, CloseEvent, CompositionEvent, Event, HtmlCanvasElement,
    HtmlElement, HtmlTextAreaElement, InputEvent, KeyboardEvent, MessageEvent, WebSocket,
    WheelEvent,
};

use crate::keys::{DomKey, KeyAction, map_key};
use crate::session::{Rect, Session, Status, layout};

/// The grid font's size in CSS pixels (points, as in the app).
const FONT_PT: f32 = 13.0;
/// Space around each pane's grid, in CSS pixels.
const PADDING: f32 = 4.0;
/// Space between split panes, in CSS pixels.
const GAP: f32 = 2.0;

/// Families tried for the grid: the app's defaults, then common Linux monospace faces (the
/// server sends whichever font its machine has).
fn families() -> Vec<String> {
    auk_text::DEFAULT_FAMILIES
        .iter()
        .chain(&["DejaVu Sans Mono", "Liberation Mono", "Noto Sans Mono"])
        .map(|s| s.to_string())
        .collect()
}

struct PaneView {
    renderer: GridRenderer,
    /// Rows scrolled back (view state, never replicated).
    scroll: usize,
}

struct App {
    session: Session,
    ws: WebSocket,
    gpu: Gpu,
    surface: WindowSurface,
    font: Arc<Vec<u8>>,
    /// Physical pixels per CSS pixel the fonts were made for.
    scale: f32,
    cell: (u32, u32),
    dark: bool,
    views: BTreeMap<PaneId, PaneView>,
    canvas: HtmlCanvasElement,
    input: HtmlTextAreaElement,
    status: HtmlElement,
    /// `?pane=`: show the tab holding this pane and focus it.
    want_pane: Option<PaneId>,
    /// The focused pane; keys go here.
    focused: Option<PaneId>,
    /// Where each shown pane was drawn last, in physical pixels.
    rects: Vec<(PaneId, Rect)>,
    frame_requested: bool,
    frames: u64,
    composing: bool,
    /// Text a composition just committed: an `input` event repeating it is the same text.
    composed: Option<String>,
    adapter: String,
}

thread_local! {
    static APP: RefCell<Option<Rc<RefCell<App>>>> = const { RefCell::new(None) };
    static START_ERROR: RefCell<Option<String>> = const { RefCell::new(None) };
}

fn window() -> web_sys::Window {
    web_sys::window().expect("a browser window")
}

fn js_err(e: JsValue) -> String {
    e.as_string()
        .or_else(|| js_sys::JSON::stringify(&e).ok().and_then(|s| s.as_string()))
        .unwrap_or_else(|| format!("{e:?}"))
}

fn element<T: JsCast>(id: &str) -> Result<T, String> {
    window()
        .document()
        .and_then(|d| d.get_element_by_id(id))
        .ok_or_else(|| format!("the page has no #{id}"))?
        .dyn_into::<T>()
        .map_err(|_| format!("#{id} has the wrong type"))
}

fn param(name: &str) -> Option<String> {
    let search = window().location().search().ok()?;
    web_sys::UrlSearchParams::new_with_str(&search)
        .ok()?
        .get(name)
        .filter(|v| !v.is_empty())
}

fn prefers_dark() -> bool {
    window()
        .match_media("(prefers-color-scheme: dark)")
        .ok()
        .flatten()
        .is_some_and(|m| m.matches())
}

async fn fetch_bytes(url: &str) -> Result<Vec<u8>, String> {
    let resp = wasm_bindgen_futures::JsFuture::from(window().fetch_with_str(url))
        .await
        .map_err(js_err)?;
    let resp: web_sys::Response = resp.dyn_into().map_err(js_err)?;
    if !resp.ok() {
        return Err(format!("{url}: HTTP {}", resp.status()));
    }
    let buf = wasm_bindgen_futures::JsFuture::from(resp.array_buffer().map_err(js_err)?)
        .await
        .map_err(js_err)?;
    Ok(js_sys::Uint8Array::new(&buf).to_vec())
}

fn grid_fonts(font: &Arc<Vec<u8>>, scale: f32) -> Result<GridFonts, String> {
    GridFonts::new(&FontConfig {
        families: families(),
        size_pt: FONT_PT,
        scale,
        system_fonts: false,
        extra_fonts: vec![font.clone()],
        backend: Backend::Swash,
    })
    .map_err(|e| e.to_string())
}

/// Starts the client. Called once by the page's boot script.
#[wasm_bindgen]
pub async fn start() -> Result<(), JsValue> {
    match start_inner().await {
        Ok(()) => Ok(()),
        Err(err) => {
            START_ERROR.with(|e| *e.borrow_mut() = Some(err.clone()));
            if let Ok(status) = element::<HtmlElement>("status") {
                status.set_text_content(Some(&format!("The Auk client couldn't start: {err}")));
            }
            Err(JsValue::from_str(&err))
        }
    }
}

async fn start_inner() -> Result<(), String> {
    let canvas: HtmlCanvasElement = element("screen")?;
    let input: HtmlTextAreaElement = element("input")?;
    let status: HtmlElement = element("status")?;
    status.set_text_content(Some("Connecting…"));

    let font = Arc::new(fetch_bytes("/font/0").await?);
    let gpu = Gpu::new()
        .await
        .map_err(|e| format!("WebGPU isn't available in this browser: {e}"))?;
    let adapter = format!("{:?}", gpu.adapter.get_info());
    let scale = window().device_pixel_ratio() as f32;
    let (w, h) = canvas_size(&canvas, scale);
    canvas.set_width(w);
    canvas.set_height(h);
    // wgpu reads the canvas from this value while it makes the surface.
    let canvas_value: JsValue = canvas.clone().into();
    let handle = WebCanvasWindowHandle::new(std::ptr::NonNull::from(&canvas_value).cast());
    // SAFETY: the handle points at a live JsValue holding the canvas, which the page keeps.
    let surface = unsafe {
        WindowSurface::from_raw_handles(
            &gpu,
            Some(RawDisplayHandle::Web(WebDisplayHandle::new())),
            RawWindowHandle::WebCanvas(handle),
            w,
            h,
        )
    }
    .map_err(|e| format!("WebGPU canvas: {e}"))?;
    let metrics = grid_fonts(&font, scale)?.metrics();
    let cell = (metrics.cell_width, metrics.cell_height);
    let dark = prefers_dark();

    let window_key = param("window").unwrap_or_else(|| "main".to_owned());
    let want_pane = param("pane").and_then(|p| p.parse().ok());
    let session = Session::new(&window_key, facts(cell, dark));

    let location = window().location();
    let host = location.host().map_err(js_err)?;
    let ws = WebSocket::new(&format!("ws://{host}/ws")).map_err(js_err)?;
    ws.set_binary_type(BinaryType::Arraybuffer);

    let app = Rc::new(RefCell::new(App {
        session,
        ws: ws.clone(),
        gpu,
        surface,
        font,
        scale,
        cell,
        dark,
        views: BTreeMap::new(),
        canvas: canvas.clone(),
        input: input.clone(),
        status,
        want_pane,
        focused: None,
        rects: Vec::new(),
        frame_requested: false,
        frames: 0,
        composing: false,
        composed: None,
        adapter,
    }));
    APP.with(|a| *a.borrow_mut() = Some(app.clone()));

    // The WebSocket.
    on(&ws, "open", {
        let app = app.clone();
        move |_: Event| app.borrow_mut().flush()
    });
    on(&ws, "message", {
        let app = app.clone();
        move |e: MessageEvent| {
            let data = e.data();
            if let Ok(buf) = data.dyn_into::<js_sys::ArrayBuffer>() {
                let bytes = js_sys::Uint8Array::new(&buf).to_vec();
                let mut a = app.borrow_mut();
                a.session.on_message(&bytes);
                a.flush();
                a.request_frame(&app);
            }
        }
    });
    on(&ws, "close", {
        let app = app.clone();
        move |e: CloseEvent| {
            let mut a = app.borrow_mut();
            a.session.on_close(&e.reason());
            a.request_frame(&app);
        }
    });

    // Keyboard, text input and paste go through the hidden text field.
    on(&input, "keydown", {
        let app = app.clone();
        move |e: KeyboardEvent| app.borrow_mut().key_down(&e)
    });
    on(&input, "compositionstart", {
        let app = app.clone();
        move |_: CompositionEvent| app.borrow_mut().composing = true
    });
    on(&input, "compositionend", {
        let app = app.clone();
        move |e: CompositionEvent| {
            let mut a = app.borrow_mut();
            a.composing = false;
            if let Some(text) = e.data() {
                a.type_text(&text);
                a.composed = Some(text);
            }
            a.input.set_value("");
        }
    });
    on(&input, "input", {
        let app = app.clone();
        move |e: InputEvent| {
            let mut a = app.borrow_mut();
            if a.composing || e.is_composing() {
                return;
            }
            // Text the browser typed itself: a dead key's composition.
            let composed = a.composed.take();
            if e.input_type() == "insertText"
                && let Some(text) = e.data()
                && composed.as_deref() != Some(text.as_str())
            {
                a.type_text(&text);
            }
            a.input.set_value("");
        }
    });
    on(&input, "paste", {
        let app = app.clone();
        move |e: ClipboardEvent| {
            e.prevent_default();
            let text = e
                .clipboard_data()
                .and_then(|d| d.get_data("text/plain").ok())
                .unwrap_or_default();
            let mut a = app.borrow_mut();
            if let Some(pane) = a.focused {
                a.session.paste(pane, &text);
                a.flush();
            }
        }
    });
    on(&canvas, "mousedown", {
        let app = app.clone();
        move |e: web_sys::MouseEvent| {
            e.prevent_default();
            let mut a = app.borrow_mut();
            let (x, y) = (e.offset_x() as f32 * a.scale, e.offset_y() as f32 * a.scale);
            let hit = a
                .rects
                .iter()
                .find(|(_, r)| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h)
                .map(|(p, _)| *p);
            if let Some(pane) = hit {
                a.focus(pane);
            }
            let _ = a.input.focus();
            a.request_frame(&app);
        }
    });
    on(&canvas, "wheel", {
        let app = app.clone();
        move |e: WheelEvent| {
            e.prevent_default();
            let mut a = app.borrow_mut();
            a.wheel(&e);
            a.request_frame(&app);
        }
    });
    let win = window();
    on(&win, "resize", {
        let app = app.clone();
        move |_: Event| app.borrow_mut().request_frame(&app)
    });
    on(&win, "focus", {
        let app = app.clone();
        move |_: Event| {
            let mut a = app.borrow_mut();
            if let Some(pane) = a.focused {
                a.focus(pane);
            }
            let _ = a.input.focus();
            a.request_frame(&app);
        }
    });
    on(&win, "blur", {
        let app = app.clone();
        move |_: Event| app.borrow_mut().request_frame(&app)
    });
    let _ = input.focus();
    app.borrow_mut().request_frame(&app);
    Ok(())
}

/// Adds an event listener for the page's lifetime.
fn on<E: JsCast + 'static>(
    target: &web_sys::EventTarget,
    name: &str,
    mut f: impl FnMut(E) + 'static,
) {
    let closure = Closure::<dyn FnMut(Event)>::new(move |e: Event| {
        if let Ok(e) = e.dyn_into::<E>() {
            f(e);
        }
    });
    let _ = target.add_event_listener_with_callback(name, closure.as_ref().unchecked_ref());
    closure.forget();
}

fn canvas_size(canvas: &HtmlCanvasElement, scale: f32) -> (u32, u32) {
    let w = (canvas.client_width() as f32 * scale).round().max(1.0) as u32;
    let h = (canvas.client_height() as f32 * scale).round().max(1.0) as u32;
    (w, h)
}

fn facts(cell: (u32, u32), dark: bool) -> ClientFacts {
    ClientFacts {
        cell_px: [
            cell.0.min(u16::MAX.into()) as u16,
            cell.1.min(u16::MAX.into()) as u16,
        ],
        dark,
        reduce_motion: window()
            .match_media("(prefers-reduced-motion: reduce)")
            .ok()
            .flatten()
            .is_some_and(|m| m.matches()),
        hour12: false,
    }
}

impl App {
    /// Sends whatever the session queued.
    fn flush(&mut self) {
        if self.ws.ready_state() != WebSocket::OPEN {
            return;
        }
        for frame in self.session.take_outbox() {
            let _ = self.ws.send_with_u8_array(&frame);
        }
    }

    fn request_frame(&mut self, app: &Rc<RefCell<App>>) {
        if self.frame_requested {
            return;
        }
        self.frame_requested = true;
        let app = app.clone();
        let closure = Closure::once_into_js(move |_: f64| {
            let mut a = app.borrow_mut();
            a.frame_requested = false;
            a.frame();
            a.flush();
        });
        let _ = window().request_animation_frame(closure.unchecked_ref());
    }

    fn focus(&mut self, pane: PaneId) {
        self.focused = Some(pane);
        self.session.focus(pane);
        self.flush();
    }

    fn key_down(&mut self, e: &KeyboardEvent) {
        let Some(pane) = self.focused else {
            return;
        };
        let key = e.key();
        let code = e.code();
        let dom = DomKey {
            key: &key,
            code: &code,
            shift: e.shift_key(),
            ctrl: e.ctrl_key(),
            alt: e.alt_key(),
            meta: e.meta_key(),
            composing: e.is_composing() || e.key_code() == 229 || self.composing,
        };
        match map_key(&dom, false) {
            KeyAction::Send(ev) => {
                e.prevent_default();
                if let Some(v) = self.views.get_mut(&pane) {
                    v.scroll = 0;
                }
                self.session.key(pane, &ev);
                self.flush();
            }
            KeyAction::Compose | KeyAction::Ignore => {}
        }
    }

    fn type_text(&mut self, text: &str) {
        if let Some(pane) = self.focused
            && !text.is_empty()
        {
            self.session.text(pane, text);
            self.flush();
        }
    }

    fn wheel(&mut self, e: &WheelEvent) {
        let Some(pane) = self.focused else {
            return;
        };
        let lines = match e.delta_mode() {
            WheelEvent::DOM_DELTA_LINE => e.delta_y(),
            WheelEvent::DOM_DELTA_PAGE => e.delta_y() * 20.0,
            _ => e.delta_y() * f64::from(self.scale) / f64::from(self.cell.1.max(1)),
        };
        let history = self
            .session
            .panes
            .get(&pane)
            .map_or(0, |p| p.replica.terminal().history_len());
        if let Some(v) = self.views.get_mut(&pane) {
            let next = v.scroll as f64 - lines.round();
            v.scroll = next.clamp(0.0, history as f64) as usize;
        }
    }

    fn set_status(&self, text: &str) {
        if self.status.text_content().as_deref() != Some(text) {
            self.status.set_text_content(Some(text));
        }
    }

    /// Draws the shown tab.
    fn frame(&mut self) {
        let scale = window().device_pixel_ratio() as f32;
        if scale != self.scale {
            self.scale = scale;
            self.views.clear();
            if let Ok(fonts) = grid_fonts(&self.font, scale) {
                let m = fonts.metrics();
                self.cell = (m.cell_width, m.cell_height);
            }
            self.session.facts(facts(self.cell, self.dark));
        }
        let dark = prefers_dark();
        if dark != self.dark {
            self.dark = dark;
            self.session.facts(facts(self.cell, dark));
        }
        let (w, h) = canvas_size(&self.canvas, scale);
        if (self.canvas.width(), self.canvas.height()) != (w, h) {
            self.canvas.set_width(w);
            self.canvas.set_height(h);
        }
        self.surface.resize(&self.gpu, w, h);

        match &self.session.status {
            Status::Connecting => self.set_status("Connecting…"),
            Status::Refused(msg) => {
                self.set_status(&format!("The daemon refused this client: {msg}"))
            }
            Status::Closed(msg) => {
                self.set_status(&format!("Disconnected: {msg}. Reload to reconnect."))
            }
            Status::Attached => {}
        }
        let theme = if self.dark {
            Theme::dark()
        } else {
            Theme::light()
        };
        let tab = self.session.tab(self.want_pane.or(self.focused)).cloned();
        let area = Rect {
            x: 0.0,
            y: 0.0,
            w: w as f32,
            h: h as f32,
        };
        self.rects = tab
            .as_ref()
            .map(|t| layout(t, area, (GAP * scale).round()))
            .unwrap_or_default();
        if self.session.status == Status::Attached {
            if tab.is_none() {
                self.set_status(&format!(
                    "No sessions in window key `{}` yet.",
                    self.session.window_key
                ));
            } else {
                self.set_status("");
            }
        }
        // Focus the wanted pane, else keep focus in the tab, else its first pane.
        let shown: Vec<PaneId> = self.rects.iter().map(|(p, _)| *p).collect();
        let want = self
            .want_pane
            .filter(|p| shown.contains(p))
            .or(self.focused.filter(|p| shown.contains(p)))
            .or(shown.first().copied());
        if want != self.focused
            && let Some(pane) = want
            && self.session.panes.contains_key(&pane)
        {
            self.focused = Some(pane);
            if window()
                .document()
                .is_some_and(|d| d.has_focus().unwrap_or(false))
            {
                self.session.focus(pane);
            }
        }

        let frame = match self.surface.acquire(&self.gpu) {
            Acquired::Frame(frame) => frame,
            Acquired::Skip => return,
        };
        let target = frame.texture.create_view(&Default::default());
        let pad = (PADDING * scale).round();
        let doc_focused = window()
            .document()
            .is_some_and(|d| d.has_focus().unwrap_or(false));
        let mut drawn = Vec::new();
        for &(pane, rect) in &self.rects {
            let Some(p) = self.session.panes.get(&pane) else {
                continue;
            };
            if !self.views.contains_key(&pane) {
                let Ok(fonts) = grid_fonts(&self.font, scale) else {
                    continue;
                };
                self.views.insert(
                    pane,
                    PaneView {
                        renderer: GridRenderer::new(&self.gpu, self.surface.format(), fonts),
                        scroll: 0,
                    },
                );
            }
            let view = self.views.get_mut(&pane).expect("inserted");
            let (cols, rows) =
                view.renderer
                    .grid_size_for(rect.w as u32, rect.h as u32, pad as u32);
            let term = p.replica.terminal();
            let snap = GridSnapshot::from_terminal(
                term,
                &theme.with_overrides(term.color_overrides()),
                &View {
                    scroll: view.scroll,
                    selection: None,
                    focused: doc_focused && self.focused == Some(pane),
                    cursor_on: true,
                },
            );
            view.renderer.set_origin(rect.x + pad, rect.y + pad);
            view.renderer.prepare(&self.gpu, &snap, (w, h));
            drawn.push((pane, rect, cols, rows));
        }
        for &(pane, _, cols, rows) in &drawn {
            self.session.resize(pane, cols, rows);
        }

        let c = |v: u8| f64::from(v) / 255.0;
        let bg = theme.background;
        let mut encoder = self.gpu.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("auk web frame"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: c(bg.0),
                            g: c(bg.1),
                            b: c(bg.2),
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            for &(pane, rect, _, _) in &drawn {
                let x = rect.x.max(0.0) as u32;
                let y = rect.y.max(0.0) as u32;
                let rw = (rect.w as u32).min(w.saturating_sub(x));
                let rh = (rect.h as u32).min(h.saturating_sub(y));
                if rw == 0 || rh == 0 {
                    continue;
                }
                pass.set_scissor_rect(x, y, rw, rh);
                self.views[&pane].renderer.draw(&mut pass);
            }
        }
        self.gpu.queue.submit([encoder.finish()]);
        self.gpu.queue.present(frame);
        self.frames += 1;
        for &(pane, ..) in &drawn {
            self.session.drew(pane);
        }
        self.place_input(pad);
    }

    /// Keeps the text field at the focused pane's cursor, so input-method windows open there.
    fn place_input(&self, pad: f32) {
        let Some(pane) = self.focused else {
            return;
        };
        let (Some(p), Some((_, rect))) = (
            self.session.panes.get(&pane),
            self.rects.iter().find(|(id, _)| *id == pane),
        ) else {
            return;
        };
        let cursor = p.replica.terminal().cursor();
        let x = (rect.x + pad + f32::from(cursor.col) * self.cell.0 as f32) / self.scale;
        let y = (rect.y + pad + f32::from(cursor.row) * self.cell.1 as f32) / self.scale;
        let style = self.input.style();
        let _ = style.set_property("left", &format!("{x}px"));
        let _ = style.set_property("top", &format!("{y}px"));
    }

    fn debug(&self) -> serde_json::Value {
        let panes: Vec<serde_json::Value> = self
            .session
            .panes
            .iter()
            .map(|(id, p)| {
                let r = &p.replica;
                let hash: String = r.state_hash().iter().map(|b| format!("{b:02x}")).collect();
                let rect = self
                    .rects
                    .iter()
                    .find(|(pane, _)| pane == id)
                    .map(|(_, r)| {
                        json!({
                            "x": r.x / self.scale, "y": r.y / self.scale,
                            "w": r.w / self.scale, "h": r.h / self.scale,
                        })
                    });
                json!({
                    "id": id,
                    "seq": r.seq(),
                    "hash": hash,
                    "cols": r.terminal().cols(),
                    "rows": r.terminal().rows(),
                    "screen": r.screen_text(),
                    "primary": p.primary,
                    "exit": p.exit,
                    "rect": rect,
                })
            })
            .collect();
        let (status, message) = match &self.session.status {
            Status::Refused(m) | Status::Closed(m) => (self.session.status.name(), m.clone()),
            other => (other.name(), String::new()),
        };
        json!({
            "status": status,
            "message": message,
            "window_key": self.session.window_key,
            "client": self.session.client_id,
            "focused": self.focused,
            "frames": self.frames,
            "scale": self.scale,
            "cell": [self.cell.0, self.cell.1],
            "adapter": self.adapter,
            "format": format!("{:?}", self.surface.format()),
            "panes": panes,
            "errors": self.session.errors,
        })
    }
}

/// The client's state as JSON, for tests and debugging (`window.aukDebug()` on the page): the
/// connection status, frames drawn, and each pane's stream position, state hash, screen text
/// and where it is drawn (CSS pixels).
#[wasm_bindgen]
pub fn debug_state() -> String {
    let state = APP.with(|a| a.borrow().as_ref().map(|app| app.borrow().debug()));
    let state = state.unwrap_or_else(|| {
        let err = START_ERROR.with(|e| e.borrow().clone());
        json!({
            "status": if err.is_some() { "failed" } else { "starting" },
            "message": err.unwrap_or_default(),
        })
    });
    state.to_string()
}
