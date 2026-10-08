//! The wasm entry point (§10.3): the page's WebSocket, WebGPU canvas and keyboard.
//!
//! [`start`] fetches the grid font from the server, opens WebGPU on the page's canvas, says
//! hello over `/ws` and attaches to the window key (`?window=`, default `main`). Each frame draws
//! the shown tab's panes (`?pane=` picks the tab holding that pane) with `auk-render`'s grid
//! pass, glyphs rasterized by swash (§18.3). Keys are encoded for the pane's modes and sent as
//! input; paste, input-method text and resizes go through the same session ([`Session`]). The
//! tab bar, dividers and focus ring are the app's chrome widgets (D3, §13.1), drawn by the UI
//! engine over the panes.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use auk_mirror::layout::SessionId;
use auk_mirror::{ClientFacts, PaneId};
use auk_render::{
    Acquired, Gpu, GridRenderer, GridSnapshot, RowSlot, Theme, UiLayer, View, WindowSurface, wgpu,
};
use auk_text::{Backend, FontConfig, GridFonts};
use auk_views::chrome::{BAR_HEIGHT, ChromeAction, ChromeDoc};
use auk_views::compose::Scroll;
use auk_views::{Place, ViewSettings};
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
use crate::pane::{Action, PaneDraw, Press};
use crate::session::{Rect, Session, Status, dividers, layout};

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

/// A pane as this client shows it: its surfaces and command blocks (kept for every pane, shown
/// or not) and the grid renderer once shown.
struct PaneView {
    renderer: Option<GridRenderer>,
    draw: PaneDraw,
}

/// How long a toast stays up, in milliseconds (§9.7: two lines, a few seconds).
const TOAST_MS: i32 = 4000;

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
    /// The facts last told the daemon: the page's look, motion and clock, followed as they
    /// change (§4.5).
    facts: ClientFacts,
    views: BTreeMap<PaneId, PaneView>,
    /// Surfaces, drawn with Vello over the grids (§6.2, §18.1).
    ui: UiLayer,
    /// The UI layer holds surfaces from the last frame.
    ui_painted: bool,
    canvas: HtmlCanvasElement,
    input: HtmlTextAreaElement,
    status: HtmlElement,
    /// The tab bar with its session chip and menu, the dividers and the focus ring: the app's
    /// chrome widgets (D3, §13.1), painted into the UI layer over the panes. The browser views
    /// and switches; it creates no tabs or sessions.
    chrome: ChromeDoc,
    /// The chrome changed since the UI layer last rendered.
    chrome_changed: bool,
    /// The session chip's menu is open.
    session_menu: bool,
    /// The pane last shown in each session, to come back to.
    session_panes: BTreeMap<SessionId, PaneId>,
    /// The chrome's roles, mirrored into an offscreen element for assistive technology (the
    /// canvas has no accessibility tree of its own).
    chrome_a11y: HtmlElement,
    /// Where toasts show.
    toast: HtmlElement,
    /// Effects performed, newest last (the last 32), for the debug state.
    effects_done: Vec<serde_json::Value>,
    /// What the last press on the canvas landed on and did, for the debug state.
    last_press: serde_json::Value,
    /// `?pane=`: show the tab holding this pane and focus it.
    want_pane: Option<PaneId>,
    /// The focused pane; keys go here.
    focused: Option<PaneId>,
    /// Where each shown pane was drawn last, in physical pixels.
    rects: Vec<(PaneId, Rect)>,
    frame_requested: bool,
    /// A surface is animating: draw again next frame.
    animating: bool,
    frames: u64,
    composing: bool,
    /// Text a composition just committed: an `input` event repeating it is the same text.
    composed: Option<String>,
    adapter: String,
}

/// Where a press on the canvas lands ([`App::locate`]).
enum Located {
    Chrome(ChromeAction),
    /// The tab bar's strip, or anywhere while the session menu is open.
    Bar,
    NoPane,
    /// In a pane's padding.
    Padding(PaneId),
    /// A pane the client has no state or drawing for yet.
    NoView(PaneId),
    /// In a pane's content area, `(x, y)` from its top-left in physical pixels.
    Pane {
        pane: PaneId,
        x: f32,
        y: f32,
        content_h: f32,
    },
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

/// Whether a media query matches.
fn media(query: &str) -> bool {
    window()
        .match_media(query)
        .ok()
        .flatten()
        .is_some_and(|m| m.matches())
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
    let chrome_a11y: HtmlElement = element("chrome-a11y")?;
    let toast: HtmlElement = element("toast")?;
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
    let ui = UiLayer::new(&gpu, surface.format()).map_err(|e| e.to_string())?;
    let mut chrome = ChromeDoc::new(scale, "");
    // The browser has no system fonts: the chrome draws in the server's font.
    chrome.use_font(font.to_vec());
    let metrics = grid_fonts(&font, scale)?.metrics();
    let cell = (metrics.cell_width, metrics.cell_height);
    let dark = prefers_dark();

    let window_key = param("window").unwrap_or_else(|| "main".to_owned());
    let want_pane = param("pane").and_then(|p| p.parse().ok());
    let first_facts = facts(cell, dark);
    let session = Session::new(&window_key, first_facts.clone());

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
        facts: first_facts,
        views: BTreeMap::new(),
        ui,
        ui_painted: false,
        canvas: canvas.clone(),
        input: input.clone(),
        status,
        chrome,
        chrome_changed: true,
        session_menu: false,
        session_panes: BTreeMap::new(),
        chrome_a11y,
        toast,
        effects_done: Vec::new(),
        last_press: serde_json::Value::Null,
        want_pane,
        focused: None,
        rects: Vec::new(),
        frame_requested: false,
        animating: false,
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
                a.session.set_time(js_sys::Date::now() / 1000.0);
                a.session.on_message(&bytes);
                a.effects();
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
            a.mouse_down(&e);
            let _ = a.input.focus();
            a.request_frame(&app);
        }
    });
    on(&canvas, "contextmenu", |e: web_sys::MouseEvent| {
        e.prevent_default()
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
        hour12: browser_hour12(),
    }
}

/// Whether the browser's locale shows a 12-hour clock: the resolved `hourCycle` (`h11`/`h12`) of
/// an hour format for the default locale, which follows the system's 12/24-hour setting.
fn browser_hour12() -> bool {
    let opts = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&opts, &"hour".into(), &"numeric".into());
    let resolved =
        js_sys::Intl::DateTimeFormat::new(&js_sys::Array::new(), &opts).resolved_options();
    js_sys::Reflect::get(&resolved, &"hourCycle".into())
        .ok()
        .and_then(|v| v.as_string())
        .is_some_and(|c| matches!(c.as_str(), "h11" | "h12"))
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
        let app2 = app.clone();
        let closure = Closure::once_into_js(move |_: f64| {
            let mut a = app.borrow_mut();
            a.frame_requested = false;
            a.frame();
            a.flush();
            if a.animating {
                let app = app2.clone();
                a.request_frame(&app);
            }
        });
        let _ = window().request_animation_frame(closure.unchecked_ref());
    }

    fn focus(&mut self, pane: PaneId) {
        self.focused = Some(pane);
        self.session.focus(pane);
        self.flush();
    }

    fn key_down(&mut self, e: &KeyboardEvent) {
        if self.session_menu && e.key() == "Escape" {
            e.prevent_default();
            self.session_menu = false;
            return;
        }
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
                    v.draw.scroll = Scroll::default();
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
        // Pixels, in the flow's pixel scrolling (§6.2).
        let cell = f64::from(self.cell.1.max(1));
        let px = match e.delta_mode() {
            WheelEvent::DOM_DELTA_LINE => e.delta_y() * cell,
            WheelEvent::DOM_DELTA_PAGE => e.delta_y() * 20.0 * cell,
            _ => e.delta_y() * f64::from(self.scale),
        };
        if let Some(v) = self.views.get_mut(&pane) {
            v.draw.scroll = Scroll::by(&v.draw.arranged.composed, -px as f32);
        }
    }

    /// Where a press at `(cx, cy)` (CSS pixels from the canvas's top-left) lands. `primary`:
    /// the chrome takes only the primary button.
    fn locate(&self, cx: f32, cy: f32, primary: bool) -> Located {
        // The chrome first: the tab bar, and the session menu over everything while open.
        if primary && let Some(action) = self.chrome.action_at(cx, cy) {
            return Located::Chrome(action);
        }
        if f64::from(cy) < BAR_HEIGHT || self.session_menu {
            return Located::Bar;
        }
        let (x, y) = (cx * self.scale, cy * self.scale);
        let Some((pane, rect)) = self
            .rects
            .iter()
            .find(|(_, r)| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h)
            .copied()
        else {
            return Located::NoPane;
        };
        let pad = (PADDING * self.scale).round();
        let (px, py) = (x - rect.x - pad, y - rect.y - pad);
        if px < 0.0 || py < 0.0 {
            return Located::Padding(pane);
        }
        if !self.session.panes.contains_key(&pane) || !self.views.contains_key(&pane) {
            return Located::NoView(pane);
        }
        Located::Pane {
            pane,
            x: px,
            y: py,
            content_h: (rect.h - 2.0 * pad).max(0.0),
        }
    }

    /// What a press at `(cx, cy)` lands on, as JSON: `outcome` (`chrome`, `bar`, `no-pane`,
    /// `padding`, `no-view`, `no-node` or `node`), the pane, and the node's `sf` and `id`.
    fn describe(&self, located: &Located) -> serde_json::Value {
        match located {
            Located::Chrome(action) => {
                json!({"outcome": "chrome", "action": format!("{action:?}")})
            }
            Located::Bar => json!({"outcome": "bar"}),
            Located::NoPane => json!({"outcome": "no-pane"}),
            Located::Padding(pane) => json!({"outcome": "padding", "pane": pane}),
            Located::NoView(pane) => json!({"outcome": "no-view", "pane": pane}),
            &Located::Pane {
                pane,
                x,
                y,
                content_h,
            } => {
                let press = Press {
                    x,
                    y,
                    clicks: 1,
                    ..Press::default()
                };
                let hit = match (self.session.panes.get(&pane), self.views.get(&pane)) {
                    (Some(p), Some(v)) => v.draw.hit(p.replica.tsp(), press, content_h),
                    _ => None,
                };
                match hit {
                    Some((sf, id)) => json!({
                        "outcome": "node", "pane": pane, "sf": sf, "id": id, "press": [x, y],
                    }),
                    None => json!({"outcome": "no-node", "pane": pane, "press": [x, y]}),
                }
            }
        }
    }

    /// A press on the canvas: it focuses the pane under it, and a surface or command block there
    /// takes it as a view event (§7.8). What it landed on is kept for the debug state.
    fn mouse_down(&mut self, e: &web_sys::MouseEvent) {
        let (cx, cy) = (e.offset_x() as f32, e.offset_y() as f32);
        let located = self.locate(cx, cy, e.button() == 0);
        let mut record = self.describe(&located);
        // Counts presses, so a test can tell its press from an earlier one.
        record["n"] = json!(self.last_press["n"].as_u64().unwrap_or(0) + 1);
        record["client"] = json!([e.client_x(), e.client_y()]);
        record["offset"] = json!([cx, cy]);
        record["button"] = json!(e.button());
        record["scale"] = json!(self.scale);
        record["canvas"] = json!([self.canvas.width(), self.canvas.height()]);
        record["dpr"] = json!(window().device_pixel_ratio());
        let actions = match located {
            Located::Chrome(action) => {
                self.chrome_action(action);
                None
            }
            Located::Bar | Located::NoPane => None,
            Located::Padding(pane) | Located::NoView(pane) => {
                self.focus(pane);
                None
            }
            Located::Pane {
                pane,
                x,
                y,
                content_h,
            } => {
                self.focus(pane);
                let press = Press {
                    x,
                    y,
                    clicks: e.detail().clamp(1, 2) as u8,
                    mods: auk_tsp::Mods {
                        shift: e.shift_key(),
                        ctrl: e.ctrl_key(),
                        alt: e.alt_key(),
                        meta: e.meta_key(),
                    },
                    secondary: e.button() == 2,
                };
                match (self.session.panes.get(&pane), self.views.get_mut(&pane)) {
                    (Some(p), Some(view)) => view.draw.click(pane, &p.replica, press, content_h),
                    _ => None,
                }
            }
        };
        record["actions"] = json!(
            actions
                .iter()
                .flatten()
                .map(|a| match a {
                    Action::Send(auk_mirror::ClientMsg::ViewEvent { sf, event, .. }) => {
                        json!({"view_event": sf, "ev": event["ev"]})
                    }
                    Action::Send(_) => json!("send"),
                    Action::Copy(_) => json!("copy"),
                    Action::Open(target) => json!({"open": target}),
                })
                .collect::<Vec<_>>()
        );
        self.last_press = record;
        let Some(actions) = actions else {
            return;
        };
        for action in actions {
            match action {
                Action::Send(auk_mirror::ClientMsg::ViewEvent { pane, sf, event }) => {
                    self.session.view_event(pane, &sf, event);
                }
                Action::Send(_) => {}
                Action::Copy(text) => self.copy(&text),
                Action::Open(target) => self.open(&target),
            }
        }
        self.flush();
    }

    /// [`App::describe`] for a point of the page, in CSS pixels (`window.aukHitTest`).
    fn hit_at(&self, x: f64, y: f64) -> serde_json::Value {
        let origin = self.canvas.get_bounding_client_rect();
        let (cx, cy) = ((x - origin.left()) as f32, (y - origin.top()) as f32);
        self.describe(&self.locate(cx, cy, true))
    }

    /// A click on the chrome: the chip toggles the session menu, a session or a tab shows it.
    /// Creating, renaming and locking are the app's; the browser offers none of them.
    fn chrome_action(&mut self, action: ChromeAction) {
        let menu_was_open = self.session_menu;
        self.session_menu = false;
        let pane = match action {
            ChromeAction::SessionMenu => {
                self.session_menu = !menu_was_open;
                None
            }
            ChromeAction::Session(id) => self
                .session
                .session_pane(id, self.session_panes.get(&id).copied()),
            ChromeAction::Tab(id) => self
                .session
                .window
                .sessions
                .iter()
                .flat_map(|s| s.tabs.iter())
                .find(|t| t.id == id)
                .and_then(|t| Session::tab_pane(t, self.focused)),
            _ => None,
        };
        if let Some(pane) = pane {
            self.want_pane = Some(pane);
            self.focus(pane);
        }
    }

    /// Mirrors the chrome's roles into the offscreen element assistive technology reads.
    fn mirror_chrome_a11y(&self) {
        let Some(doc) = window().document() else {
            return;
        };
        self.chrome_a11y.set_text_content(None);
        for n in self.chrome.accessible() {
            if n.role == "toolbar" {
                continue;
            }
            if let Ok(el) = doc.create_element("div") {
                let _ = el.set_attribute("role", &n.role);
                let _ = el.set_attribute("aria-label", &n.label);
                match n.role.as_str() {
                    "tab" => {
                        let _ = el.set_attribute(
                            "aria-selected",
                            if n.selected { "true" } else { "false" },
                        );
                    }
                    "menuitemradio" => {
                        let _ = el.set_attribute(
                            "aria-checked",
                            if n.selected { "true" } else { "false" },
                        );
                    }
                    _ => {}
                }
                if let Some(a) = &n.action {
                    let _ = el.set_attribute("data-action", a);
                }
                let _ = self.chrome_a11y.append_child(&el);
            }
        }
    }

    /// Performs the effects the serving host sent (§9.7, §10.3): toasts show over the page,
    /// copies go to the clipboard, and links open in a new tab.
    fn effects(&mut self) {
        for effect in self.session.take_effects() {
            self.effects_done.push(json!(effect));
            if self.effects_done.len() > 32 {
                self.effects_done.remove(0);
            }
            match effect {
                auk_mirror::Effect::Toast { level, text, sub } => {
                    self.show_toast(&level, &text, sub.as_deref());
                }
                auk_mirror::Effect::Copy { text } => self.copy(&text),
                auk_mirror::Effect::Open { target, .. } => self.open(&target),
            }
        }
    }

    fn show_toast(&self, level: &str, text: &str, sub: Option<&str>) {
        let t = &self.toast;
        t.set_text_content(None);
        let _ = t.set_attribute("data-level", level);
        if let Some(doc) = window().document() {
            for (class, line) in [("text", Some(text)), ("sub", sub)] {
                if let Some(line) = line
                    && let Ok(el) = doc.create_element("div")
                {
                    el.set_class_name(class);
                    el.set_text_content(Some(line));
                    let _ = t.append_child(&el);
                }
            }
        }
        let _ = t.class_list().add_1("shown");
        let toast = t.clone();
        let hide = Closure::once_into_js(move || {
            let _ = toast.class_list().remove_1("shown");
        });
        let _ = window()
            .set_timeout_with_callback_and_timeout_and_arguments_0(hide.unchecked_ref(), TOAST_MS);
    }

    fn copy(&self, text: &str) {
        let _ = window().navigator().clipboard().write_text(text);
    }

    /// Opens a link in a new tab. A path names a file on the serving host, which a browser
    /// can't open: the client says so.
    fn open(&self, target: &str) {
        let lower = target.to_ascii_lowercase();
        if ["http://", "https://", "mailto:"]
            .iter()
            .any(|p| lower.starts_with(p))
        {
            let _ = window().open_with_url_and_target(target, "_blank");
        } else {
            self.show_toast(
                "info",
                &format!("Open {target} on the host"),
                Some("The browser can't open files on the serving machine."),
            );
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
            // New glyphs at the new scale; the surfaces keep their state.
            for v in self.views.values_mut() {
                v.renderer = None;
            }
            if let Ok(fonts) = grid_fonts(&self.font, scale) {
                let m = fonts.metrics();
                self.cell = (m.cell_width, m.cell_height);
            }
        }
        self.dark = prefers_dark();
        // Each frame: a changed look, Reduce Motion or 12-hour clock reaches the daemon.
        let now = facts(self.cell, self.dark);
        if now != self.facts {
            self.facts = now.clone();
            self.session.facts(now);
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
        // The panes sit below the tab bar.
        let bar_h = (BAR_HEIGHT as f32 * scale).round();
        let area = Rect {
            x: 0.0,
            y: bar_h,
            w: w as f32,
            h: (h as f32 - bar_h).max(0.0),
        };
        let gap = (GAP * scale).round();
        self.rects = tab
            .as_ref()
            .map(|t| layout(t, area, gap))
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

        // Each session remembers the pane it showed.
        if let (Some(t), Some(pane)) = (&tab, self.focused)
            && t.root.contains(pane)
            && let Some(s) = self
                .session
                .window
                .sessions
                .iter()
                .find(|s| s.tabs.iter().any(|x| x.id == t.id))
        {
            self.session_panes.insert(s.id, pane);
        }
        self.session_panes
            .retain(|id, _| self.session.window.sessions.iter().any(|s| s.id == *id));
        // The chrome, rebuilt when what it shows changed.
        let mut bar = self
            .session
            .chrome_bar(self.want_pane.or(self.focused), self.focused);
        let points =
            |r: &Rect| auk_ui::Rect::new(r.x / scale, r.y / scale, r.w / scale, r.h / scale);
        bar.width = f64::from(w) / f64::from(scale);
        bar.height = f64::from(h) / f64::from(scale);
        bar.dark = self.dark;
        bar.media = (
            self.facts.reduce_motion,
            media("(prefers-reduced-transparency: reduce)"),
            media("(prefers-contrast: more)"),
        );
        bar.dividers = tab
            .as_ref()
            .map(|t| dividers(t, area, gap).iter().map(points).collect())
            .unwrap_or_default();
        bar.focus = self
            .focused
            .and_then(|f| self.rects.iter().find(|(p, _)| *p == f))
            .map(|(_, r)| points(r));
        bar.session_menu = self.session_menu;
        self.chrome.doc.set_device_scale(scale);
        if self.session.status == Status::Attached && self.chrome.update(&bar) {
            self.chrome_changed = true;
            self.mirror_chrome_a11y();
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
        // The views' clock, in ms.
        let now = js_sys::Date::now();
        let settings = ViewSettings {
            font_px: FONT_PT,
            cell: (self.cell.0 as f32 / scale, self.cell.1 as f32 / scale),
            dark: self.dark,
            ..ViewSettings::default()
        };
        // Every pane's surfaces take their changes, shown or not.
        let ids: Vec<PaneId> = self.session.panes.keys().copied().collect();
        self.views.retain(|id, _| ids.contains(id));
        for pane in ids {
            let (reset, changes) = self.session.take_changes(pane);
            if reset {
                self.views.remove(&pane);
            }
            let tsp = self.session.panes[&pane].replica.tsp();
            let font = &self.font;
            let view = self.views.entry(pane).or_insert_with(|| PaneView {
                renderer: None,
                draw: PaneDraw::new(settings.clone(), scale, Some(font.clone())),
            });
            for c in &changes {
                view.draw.sync(tsp, c, now);
            }
            view.draw.configure(tsp, settings.clone(), scale);
            // The catalog's plugin styles (§9.4, §10.4).
            view.draw
                .set_styles(self.session.styles_serial, &self.session.styles);
        }
        let mut scene = auk_render::vello::Scene::new();
        let mut painted = false;
        let mut animating = false;
        let mut drawn = Vec::new();
        for &(pane, rect) in &self.rects {
            let Some(p) = self.session.panes.get(&pane) else {
                continue;
            };
            let Some(view) = self.views.get_mut(&pane) else {
                continue;
            };
            if view.renderer.is_none() {
                let Ok(fonts) = grid_fonts(&self.font, scale) else {
                    continue;
                };
                view.renderer = Some(GridRenderer::new(&self.gpu, self.surface.format(), fonts));
            }
            let renderer = view.renderer.as_mut().expect("made above");
            let (cols, rows) = renderer.grid_size_for(rect.w as u32, rect.h as u32, pad as u32);
            let term = p.replica.terminal();
            let content_w = (rect.w - 2.0 * pad).max(0.0);
            let content_h = (rect.h - 2.0 * pad).max(0.0);
            let pane_focused = doc_focused && self.focused == Some(pane);
            // The pane's flow (§6.2): grid rows, and surface and command blocks at their anchor
            // rows.
            if view.draw.layout(
                &p.replica,
                (content_w / scale, content_h / scale),
                pane_focused,
                now,
            ) {
                animating = true;
            }
            let arranged = view
                .draw
                .arrange(&p.replica, self.cell.1 as f32, content_h)
                .clone();
            if view.draw.paint(
                &mut scene,
                Place {
                    x: f64::from(rect.x + pad),
                    y: f64::from(rect.y + pad),
                    w: f64::from(content_w),
                    h: f64::from(content_h),
                    scale: f64::from(scale),
                },
            ) {
                painted = true;
            }
            let composed = &arranged.composed;
            let lines = composed.slots.len().clamp(1, usize::from(u16::MAX)) as u16;
            let snap = GridSnapshot::from_lines(
                term,
                &theme.with_overrides(term.color_overrides()),
                &View {
                    scroll: 0,
                    selection: None,
                    focused: pane_focused,
                    cursor_on: true,
                    ..View::default()
                },
                composed.first,
                lines,
            );
            let slots = (!composed.is_uniform(self.cell.1 as f32)).then(|| {
                composed
                    .slots
                    .iter()
                    .map(|s| RowSlot {
                        top: s.top,
                        height: s.height,
                    })
                    .collect()
            });
            renderer.set_origin(rect.x + pad, rect.y + pad);
            renderer.set_row_slots(slots);
            renderer.prepare(&self.gpu, &snap, (w, h));
            let grid_shown = arranged.grid_shown();
            drawn.push((pane, rect, cols, rows, grid_shown));
        }
        // The chrome over everything, in points.
        let chrome_shown = self.chrome.shown().is_some();
        if chrome_shown {
            auk_ui::paint::paint_with_transform(
                &self.chrome.doc,
                &mut scene,
                vello::kurbo::Affine::scale(f64::from(scale)),
            );
        }
        if painted || self.ui_painted || self.chrome_changed || self.ui.size() != (w, h) {
            if let Err(err) = self.ui.render(&self.gpu, &scene, w, h) {
                self.session.errors.push(err.to_string());
            }
            self.chrome_changed = false;
        }
        self.ui_painted = painted;
        self.animating = animating;
        for &(pane, _, cols, rows, _) in &drawn {
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
            for &(pane, rect, _, _, grid_shown) in &drawn {
                if !grid_shown {
                    continue;
                }
                let x = rect.x.max(0.0) as u32;
                let y = rect.y.max(0.0) as u32;
                let rw = (rect.w as u32).min(w.saturating_sub(x));
                let rh = (rect.h as u32).min(h.saturating_sub(y));
                if rw == 0 || rh == 0 {
                    continue;
                }
                pass.set_scissor_rect(x, y, rw, rh);
                if let Some(r) = &self.views[&pane].renderer {
                    r.draw(&mut pass);
                }
            }
            pass.set_scissor_rect(0, 0, w, h);
            self.ui.draw(&mut pass);
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
        // Page coordinates: the canvas fills the page, the tab bar drawn at its top.
        let origin = self.canvas.get_bounding_client_rect();
        let (ox, oy) = (origin.left() as f32, origin.top() as f32);
        let pad = (PADDING * self.scale).round();
        let panes: Vec<serde_json::Value> = self
            .session
            .panes
            .iter()
            .map(|(id, p)| {
                let r = &p.replica;
                let hash: String = r.state_hash().iter().map(|b| format!("{b:02x}")).collect();
                let at = self
                    .rects
                    .iter()
                    .find(|(pane, _)| pane == id)
                    .map(|(_, r)| *r);
                let rect = at.map(|r| {
                    json!({
                        "x": ox + r.x / self.scale, "y": oy + r.y / self.scale,
                        "w": r.w / self.scale, "h": r.h / self.scale,
                    })
                });
                // What the pane drew last, in page coordinates: surfaces (a plugin block's
                // screen surface among them), command blocks and whether their lens view
                // showed, and the plugin sheets installed.
                let (surfaces, lens, styles) = match (self.views.get(id), at) {
                    (Some(v), Some(at)) => {
                        let top = |y: f32| oy + (at.y + pad + y) / self.scale;
                        let surfaces: Vec<serde_json::Value> = v
                            .draw
                            .surfaces_placed(r.tsp())
                            .into_iter()
                            .map(|(sf, y, h)| {
                                json!({
                                    "sf": sf, "x": ox + (at.x + pad) / self.scale, "y": top(y),
                                    "w": (at.w - 2.0 * pad) / self.scale, "h": h / self.scale,
                                })
                            })
                            .collect();
                        let lens: Vec<serde_json::Value> = v
                            .draw
                            .lens_placed
                            .iter()
                            .map(|l| {
                                let block = r.lens_blocks().iter().find(|b| b.id == l.block);
                                json!({
                                    "block": l.block,
                                    "view": block.is_some_and(|b| b.view.is_some()),
                                    "x": ox + (at.x + pad) / self.scale, "y": top(l.y),
                                    "w": (at.w - 2.0 * pad) / self.scale,
                                    "h": l.height / self.scale,
                                })
                            })
                            .collect();
                        (
                            surfaces,
                            lens,
                            v.draw.style_ids().into_iter().map(str::to_owned).collect(),
                        )
                    }
                    _ => (Vec::new(), Vec::new(), Vec::<String>::new()),
                };
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
                    "surfaces": surfaces,
                    "lens": lens,
                    "styles": styles,
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
            // The chrome's clickable parts, in page coordinates.
            "chrome": self.chrome.actions().into_iter().map(|(a, r)| json!({
                "action": a, "x": ox + r.x, "y": oy + r.y, "w": r.w, "h": r.h,
            })).collect::<Vec<_>>(),
            "session_menu": self.session_menu,
            "effects": self.effects_done,
            "last_press": self.last_press,
            "errors": self.session.errors,
        })
    }
}

/// Where a node of a pane's surface (or of a command block, `lens:<block>`) was drawn last, as
/// `{"x", "y", "w", "h"}` in page CSS pixels, or `null` (`window.aukNodeRect` on the page).
#[wasm_bindgen]
pub fn node_rect(pane: f64, sf: &str, id: &str) -> String {
    let rect = APP.with(|a| {
        let app = a.borrow().clone()?;
        let app = app.borrow();
        let pane = pane as PaneId;
        let (_, at) = app.rects.iter().find(|(p, _)| *p == pane)?;
        let p = app.session.panes.get(&pane)?;
        let [x, y, w, h] = app
            .views
            .get(&pane)?
            .draw
            .node_rect(p.replica.tsp(), sf, id)?;
        let origin = app.canvas.get_bounding_client_rect();
        let pad = (PADDING * app.scale).round();
        let s = app.scale;
        Some(json!({
            "x": origin.left() as f32 + (at.x + pad + x) / s,
            "y": origin.top() as f32 + (at.y + pad + y) / s,
            "w": w / s,
            "h": h / s,
        }))
    });
    rect.unwrap_or(serde_json::Value::Null).to_string()
}

/// What a press at a point of the page (CSS pixels) would land on, without pressing
/// (`window.aukHitTest` on the page): `{"outcome", "pane", "sf", "id"}` (see `App::describe`),
/// or `null` before the client starts.
#[wasm_bindgen]
pub fn hit_test(x: f64, y: f64) -> String {
    let hit = APP.with(|a| {
        let app = a.borrow().clone()?;
        let app = app.borrow();
        Some(app.hit_at(x, y))
    });
    hit.unwrap_or(serde_json::Value::Null).to_string()
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
