//! The app's main-thread state: its options, the GPU device, and the open windows. Windows are
//! restored from daemon state (§19): one per window key with sessions.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use auk_mirror::ClientFacts;
use auk_platform_macos::{AppContext, WindowAttributes, run_on_main};
use auk_render::Gpu;

use crate::conn::Connection;
use crate::gfx::FrameClock;
use crate::window::{Handler, WinState};
use crate::{OptionAsAltSetting, Options};

pub struct App {
    pub opts: Options,
    pub ctx: AppContext,
    pub gpu: Rc<Gpu>,
    pub clock: FrameClock,
    pub windows: Vec<Rc<RefCell<WinState>>>,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Runs `f` with the app. Keep it short: never call into AppKit while holding it (AppKit may
/// call back into a window, which may want the app).
pub fn with<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.borrow_mut().as_mut().map(f))
}

pub fn install(app: App) {
    APP.with(|a| *a.borrow_mut() = Some(app));
}

/// The open windows, in the order they opened.
pub fn windows() -> Vec<Rc<RefCell<WinState>>> {
    with(|a| a.windows.clone()).unwrap_or_default()
}

/// Opens a window on an attached connection.
pub fn open_window(conn: Connection) {
    let Some((ctx, gpu, clock, option_as_alt)) = with(|a| {
        (
            a.ctx,
            a.gpu.clone(),
            a.clock.clone(),
            platform_option_as_alt(a.opts.option_as_alt),
        )
    }) else {
        return;
    };
    let conn = Arc::new(conn);
    let state = Rc::new(RefCell::new(WinState::new(
        conn.clone(),
        gpu,
        clock,
        option_as_alt,
    )));
    with(|a| a.windows.push(state.clone()));
    let attrs = WindowAttributes {
        title: format!("Auk — {}", conn.window_key),
        option_as_alt,
        ..WindowAttributes::default()
    };
    let window = ctx.create_window(attrs, Box::new(Handler::new(state.clone())));
    if let Ok(mut s) = state.try_borrow_mut() {
        s.window = Some(window.clone());
    }
    crate::a11y::enable(&state, &window);
    let waker = window.waker();
    conn.start(move || waker.wake());
}

/// Opens a window on a new window key, with a new session (⌘N).
pub fn new_window() {
    run_on_main(|| {
        let Some(socket) = with(|a| a.opts.socket.clone()) else {
            return;
        };
        let key = fresh_window_key();
        let opened = Connection::open(&socket, ClientFacts::default()).and_then(|(mut conn, _)| {
            conn.attach(&key)?;
            Ok(conn)
        });
        match opened {
            Ok(conn) => open_window(conn),
            Err(err) => eprintln!("auk: new window: {err}"),
        }
    });
}

fn fresh_window_key() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    format!("w{ms}-{}", N.fetch_add(1, Ordering::Relaxed))
}

/// The window commands go to without a key window: the one focused last, else the first.
pub fn current_window() -> Option<Rc<RefCell<WinState>>> {
    let windows = windows();
    windows
        .iter()
        .filter_map(|w| Some((w.try_borrow().ok()?.focus_serial, w)))
        .max_by_key(|(serial, _)| *serial)
        .map(|(_, w)| w.clone())
        .or_else(|| windows.first().cloned())
}

/// Runs a registry command in the current window.
pub fn command_to_current(cmd: crate::commands::Cmd) {
    let Some(state) = current_window() else {
        return;
    };
    let Ok(mut s) = state.try_borrow_mut() else {
        return;
    };
    if let Some(window) = s.window.clone() {
        s.command(&window, cmd);
    }
}

/// A window closed: forget it.
pub fn window_closed(state: &Rc<RefCell<WinState>>) {
    with(|a| a.windows.retain(|w| !Rc::ptr_eq(w, state)));
}

fn platform_option_as_alt(s: OptionAsAltSetting) -> auk_platform_macos::OptionAsAlt {
    use auk_platform_macos::OptionAsAlt;
    match s {
        OptionAsAltSetting::None => OptionAsAlt::None,
        OptionAsAltSetting::Left => OptionAsAlt::Left,
        OptionAsAltSetting::Right => OptionAsAlt::Right,
        OptionAsAltSetting::Both => OptionAsAlt::Both,
    }
}

pub fn clipboard_get() -> Option<String> {
    with(|a| a.ctx.clipboard().text()).flatten()
}

pub fn clipboard_set(text: &str) -> bool {
    with(|a| a.ctx.clipboard().set_text(text)).unwrap_or(false)
}
