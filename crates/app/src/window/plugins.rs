//! The window half's host: each window runs
//! [`auk_plugins::WindowPlugins`] on its UI thread and implements its [`WindowHost`] here.
//!
//! - Commands join the palette and the action names; binds join the keymap (chords, sequences
//!   and modifier taps), leaving out chords the menu's key equivalents already take.
//! - Overrides run before every built-in command, however it was started.
//! - `route.open` sees `auk open` (origin `cli`), "Open file…" in the palette (`palette`) and
//!   clicked paths and host `cx:open` effects (`link`); `route.link` sees link clicks first.
//! - Manifest styles and `tern.css` sheets install into the chrome document.
//! - Chrome formatters title the tabs and the window and fill the status line.
//! - Window events are queued from the replicas and the layout and delivered after each frame.
//!
//! **Recursion guard.** While a handler runs, the runtime is out of [`PluginState::rt`]: a
//! `cx:command` runs the built-in, `cx:open` skips routes, and `cx:action` on a plugin action
//! queues it until the handler returns.
//!
//! Layout operations that answer (`new_tab`, `split`, `new_block`, `tab`, `move`, `resize`) are
//! requests on the window's own link to its host's daemon, local or remote: the daemon
//! answers with the new pane or the outcome after the replicated layout, which brings the change
//! to every window, so the window's layout already shows it.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use auk_mirror::layout::{Node, PaneTag, SessionId, Side, SplitDir, TabId};
use auk_mirror::{ClientMsg, LayoutEdit, OpenPlace, PaneId};
use auk_platform_macos::{KeyInput, Waker, Window};
use auk_plugins::chord::{self, Chord, Step};
use auk_plugins::window::{
    Dir, How, Launch, LinkMods, LinkRequest, OpenRequest, Reveal, RouteDecision, StatusPane,
    TabInfo, TabSpec, WindowConfig, WindowEvent, WindowPane,
};
use auk_plugins::{Catalog, Completer, ToastLevel, Watcher, WindowHost, WindowPlugins};
use auk_term::input::Key;
use serde_json::{Value, json};

use super::WinState;
use crate::chrome::{StatusItem, ToastItem};
use crate::commands::{self, Cmd};
use crate::conn::PaneNote;

/// How long a toast stays up.
pub const TOAST_FOR: Duration = Duration::from_secs(6);

/// What a window shows of one pane, from its stream.
#[derive(Clone, Debug, Default)]
pub struct Track {
    pub busy: bool,
    /// The running command's line and when the window saw it start.
    pub running: Option<(String, Instant)>,
    /// The last finished command: line, status, took.
    pub last: Option<(String, i32, f64)>,
}

/// A toast on screen.
#[derive(Clone, Debug)]
pub struct Toast {
    pub item: ToastItem,
    pub until: Instant,
}

/// A block open that failed: what Retry opens again, and why it failed.
#[derive(Clone, Debug)]
pub struct BlockCard {
    pub kind: String,
    pub args: Vec<String>,
    pub how: How,
    pub reveal: Reveal,
    pub cwd: Option<String>,
    pub error: String,
}

/// A `cx.session:settle` waiting for its command.
struct Settle {
    pane: PaneId,
    deadline: Instant,
    finished_before: u64,
    done: Completer,
}

/// A thread that wakes the window at the earliest requested instant (plugin timers, toasts,
/// settle deadlines).
pub struct Ticker {
    state: Arc<TickerState>,
}

/// The next wake, whether the thread should keep running, and its signal.
type TickerState = (Mutex<(Option<Instant>, bool)>, Condvar);

impl std::fmt::Debug for Ticker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ticker").finish_non_exhaustive()
    }
}

impl Ticker {
    fn new(waker: Waker) -> Ticker {
        let state: Arc<TickerState> = Arc::new((Mutex::new((None, true)), Condvar::new()));
        let shared = state.clone();
        let _ = std::thread::Builder::new()
            .name("auk window ticker".into())
            .spawn(move || {
                let (lock, cv) = &*shared;
                let mut g = lock.lock().unwrap_or_else(|e| e.into_inner());
                while g.1 {
                    match g.0 {
                        None => g = cv.wait(g).unwrap_or_else(|e| e.into_inner()),
                        Some(t) if Instant::now() >= t => {
                            g.0 = None;
                            drop(g);
                            waker.wake();
                            g = lock.lock().unwrap_or_else(|e| e.into_inner());
                        }
                        Some(t) => {
                            let wait = t.saturating_duration_since(Instant::now());
                            g = cv
                                .wait_timeout(g, wait)
                                .unwrap_or_else(|e| e.into_inner())
                                .0;
                        }
                    }
                }
            });
        Ticker { state }
    }

    /// Wakes the window at `t` (or earlier, if something else asked for earlier).
    pub fn at(&self, t: Instant) {
        let (lock, cv) = &*self.state;
        let mut g = lock.lock().unwrap_or_else(|e| e.into_inner());
        if g.0.is_none_or(|d| t < d) {
            g.0 = Some(t);
            cv.notify_all();
        }
    }
}

impl Drop for Ticker {
    fn drop(&mut self) {
        let (lock, cv) = &*self.state;
        lock.lock().unwrap_or_else(|e| e.into_inner()).1 = false;
        cv.notify_all();
    }
}

/// How soon each tap of a tap sequence (`shift>shift`) follows the last.
const TAP_WITHIN: Duration = Duration::from_millis(500);

/// The window's plugin state.
#[derive(Default)]
pub struct PluginState {
    /// The runtime; `None` before loading and while a handler runs (the recursion guard).
    pub rt: Option<WindowPlugins>,
    loaded: bool,
    started: bool,
    watcher: Option<Watcher>,
    reload: Arc<AtomicBool>,
    ticker: Option<Ticker>,
    /// Window events waiting for the next frame.
    events: Vec<WindowEvent>,
    pub toasts: Vec<Toast>,
    /// Every toast this window showed, for the automation endpoint.
    pub toast_log: Vec<Value>,
    /// Built-in opens (the system opener), for the automation endpoint.
    pub opened: Vec<Value>,
    /// Links refused (for the automation endpoint).
    pub refused: Vec<Value>,
    /// Route decisions taken, for the automation endpoint.
    pub routed: Vec<Value>,
    /// The plugin keymap layer: chords and actions.
    pub keymap: Vec<(Chord, String)>,
    /// The preset layer (`keymap`, `keymap_prefix`), below the plugins'.
    pub preset: Vec<(Chord, String)>,
    /// The user's `keybinds`, above everything: a chord and its actions in priority order.
    pub user: Vec<(Chord, Vec<String>)>,
    /// The plugin sheets installed, in cascade order.
    pub sheets: Vec<(String, String)>,
    /// A block that could not start, for the card's Retry.
    pub block_card: Option<BlockCard>,
    /// Chords left out because a default binds them.
    pub dropped_binds: Vec<String>,
    prefix: Vec<Step>,
    tap: Option<chord::Mods>,
    /// When the last tap of a tap sequence under way ended.
    last_tap: Option<Instant>,
    pub track: BTreeMap<PaneId, Track>,
    /// Commands finished per pane, for settles.
    finished: BTreeMap<PaneId, u64>,
    settles: Vec<Settle>,
    last_focus: Option<(PaneId, TabId)>,
    seen_panes: BTreeSet<PaneId>,
    seen_tabs: BTreeSet<TabId>,
    /// Requests this window sent that add a pane, not answered yet.
    creating: Vec<u64>,
    /// Panes this window added (from the answers), until the layout shows them: only these fire
    /// `pane_created`, and their new tabs `tab_created`.
    own_panes: BTreeSet<PaneId>,
    queued: Vec<String>,
    catalog_generation: u64,
    pub catalog: Catalog,
    /// Pane kinds from the daemon: `(kind, block)`.
    kinds: BTreeMap<PaneId, (String, Option<String>)>,
    /// A pane a plugin opened with `reveal.focus`: focused once the layout shows it.
    pub focus_when_shown: Option<PaneId>,
    pub status: Vec<StatusItem>,
    pub tab_titles: BTreeMap<TabId, String>,
    pub window_title: Option<String>,
}

impl std::fmt::Debug for PluginState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginState")
            .field("rt", &self.rt)
            .field("keymap", &self.keymap.len())
            .finish_non_exhaustive()
    }
}

/// The window, as plugin code sees it.
pub(super) struct Host<'a> {
    pub w: &'a mut WinState,
}

fn level_name(level: ToastLevel) -> &'static str {
    match level {
        ToastLevel::Success => "success",
        ToastLevel::Error => "error",
        _ => "info",
    }
}

/// An OSC 7 report's path: `file://host/path`, percent-decoded.
pub fn osc7_path(cwd: &str) -> String {
    let rest = match cwd.strip_prefix("file://") {
        Some(r) => match r.find('/') {
            Some(i) => &r[i..],
            None => "",
        },
        None => cwd,
    };
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3])
            && let Ok(b) = u8::from_str_radix(hex, 16)
        {
            out.push(b);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The local path a `file:` link names (`file://` links are limited to paths): only
/// `file:` with no host or `localhost`, an absolute path, and no query or fragment. Anything else
/// (another machine's file, a URL with parts that aren't a path) is `None`, and is refused.
pub fn file_url_path(url: &str) -> Option<String> {
    let rest = url
        .get(..5)
        .filter(|s| s.eq_ignore_ascii_case("file:"))
        .map(|_| &url[5..])?;
    let path = match rest.strip_prefix("//") {
        Some(r) => {
            let i = r.find('/')?;
            let host = &r[..i];
            if !(host.is_empty() || host.eq_ignore_ascii_case("localhost")) {
                return None;
            }
            &r[i..]
        }
        None => rest,
    };
    if !path.starts_with('/') || path.contains(['?', '#']) {
        return None;
    }
    let decoded = osc7_path(path);
    (!decoded.contains('\0')).then_some(decoded)
}

/// URL schemes the system hands to a terminal program or a remote shell: a link never opens them
/// (nothing runs a command).
const COMMAND_SCHEMES: &[&str] = &["ssh", "telnet", "rlogin", "tn3270", "x-man-page"];

/// Whether opening `path` with the system opener would run something: an executable file or a
/// bundle or document type the system launches.
fn runs_something(path: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let p = Path::new(path.trim_end_matches('/'));
    let launches = p.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        [
            "app", "command", "tool", "terminal", "workflow", "action", "prefpane",
        ]
        .iter()
        .any(|x| e.eq_ignore_ascii_case(x))
    });
    launches
        || std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// The program word of a command line (no assignments or directories).
fn program_of(line: &str) -> String {
    line.split_whitespace()
        .find(|w| !w.contains('='))
        .map(|w| w.rsplit('/').next().unwrap_or(w).to_owned())
        .unwrap_or_default()
}

fn shell_name() -> String {
    std::env::var("SHELL")
        .ok()
        .and_then(|s| s.rsplit('/').next().map(str::to_owned))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "zsh".to_owned())
}

/// The system opener: `$AUK_OPENER`, else `open`.
pub fn system_open(target: &str) {
    run_opener(&[target]);
}

/// Shows `path` selected in the Finder (`open -R`) instead of opening it: how a link reaches a
/// program or an app bundle, so that following a link never runs a command.
pub fn system_reveal(path: &str) {
    run_opener(&["-R", path]);
}

fn run_opener(args: &[&str]) {
    let opener = std::env::var_os("AUK_OPENER")
        .filter(|v| !v.is_empty())
        .map_or_else(|| PathBuf::from("/usr/bin/open"), PathBuf::from);
    match std::process::Command::new(&opener).args(args).spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(err) => eprintln!("auk: {}: {err}", opener.display()),
    }
}

/// One request to the daemon's control endpoint.
/// Whether this app's windows are on a remote host's daemon (`--host`).
fn is_remote() -> bool {
    crate::app::with(|a| a.opts.remote.is_some()).unwrap_or(false)
}

fn control(words: &[String]) -> Result<Value, String> {
    let (socket, remote) =
        crate::app::with(|a| (a.opts.socket.clone(), a.opts.remote.is_some())).ok_or("no app")?;
    // A remote host's daemon has no control endpoint here; its panes are reached
    // through the window's link.
    if remote {
        return Err("the remote host's daemon takes no control requests from this machine".into());
    }
    let path = auk_daemon::paths::control_socket(&socket);
    let mut stream = UnixStream::connect(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let line = shlex::try_join(words.iter().map(String::as_str)).map_err(|e| e.to_string())?;
    stream
        .write_all(format!("{line}\n").as_bytes())
        .map_err(|e| e.to_string())?;
    let mut reply = String::new();
    BufReader::new(stream)
        .read_line(&mut reply)
        .map_err(|e| e.to_string())?;
    let v: Value = serde_json::from_str(&reply).map_err(|e| e.to_string())?;
    if v["ok"] == true {
        Ok(v["value"].clone())
    } else {
        Err(v["error"]
            .as_str()
            .unwrap_or("the daemon refused")
            .to_owned())
    }
}

fn side(dir: Dir) -> Side {
    match dir {
        Dir::Right => Side::Right,
        Dir::Down => Side::Down,
        Dir::Left => Side::Left,
        Dir::Up => Side::Up,
    }
}

/// A plugin `Launch` as the daemon takes it.
fn launch_of(launch: &Launch, cwd: Option<&str>) -> auk_mirror::Launch {
    auk_mirror::Launch {
        cwd: launch.cwd.as_deref().or(cwd).map(str::to_owned),
        command: launch.command.clone().filter(|c| !c.trim().is_empty()),
        block: launch.block.clone(),
        args: launch.args.clone().unwrap_or_default(),
    }
}

/// The step a key press is, and the physical step when the key has one.
fn key_steps(key: &KeyInput) -> Vec<Step> {
    let m = key.event.mods;
    let mods = chord::Mods {
        ctrl: m.ctrl,
        alt: m.alt || key.option,
        shift: m.shift,
        cmd: m.meta,
    };
    let named = |n: &str| Some(chord::Key::Named(n.to_owned()));
    let k = match key.event.key {
        Key::Char(c) => Some(chord::Key::Char(c.to_ascii_lowercase())),
        Key::Enter | Key::Keypad('\r') => named("enter"),
        Key::Tab => named("tab"),
        Key::Backspace => named("backspace"),
        Key::Escape => named("escape"),
        Key::Up => named("up"),
        Key::Down => named("down"),
        Key::Left => named("left"),
        Key::Right => named("right"),
        Key::Home => named("home"),
        Key::End => named("end"),
        Key::PageUp => named("page_up"),
        Key::PageDown => named("page_down"),
        Key::Insert => named("insert"),
        Key::Delete => named("delete"),
        Key::F(n) => Some(chord::Key::Named(format!("f{n}"))),
        Key::Keypad(c) => c
            .is_ascii_digit()
            .then(|| chord::Key::Physical(format!("numpad_{c}"))),
    };
    let mut out: Vec<Step> = k.into_iter().map(|k| Step { mods, key: Some(k) }).collect();
    // A shifted symbol is its character as the layout types it (`%`, not `shift+5`).
    let typed = key.event.text.as_deref().and_then(|t| {
        let mut cs = t.chars();
        cs.next().filter(|_| cs.next().is_none())
    });
    if let Some(c) = typed
        && mods.shift
        && !c.is_ascii_alphabetic()
        && !c.is_whitespace()
    {
        out.push(Step {
            mods: chord::Mods {
                shift: false,
                ..mods
            },
            key: Some(chord::Key::Char(c)),
        });
    }
    if let Some(p) = physical(key.keycode) {
        out.push(Step {
            mods,
            key: Some(chord::Key::Physical(p.to_owned())),
        });
    }
    out
}

/// The physical key name of an ANSI virtual key code.
fn physical(code: u16) -> Option<&'static str> {
    const TABLE: &[(u16, &str)] = &[
        (0x12, "digit_1"),
        (0x13, "digit_2"),
        (0x14, "digit_3"),
        (0x15, "digit_4"),
        (0x17, "digit_5"),
        (0x16, "digit_6"),
        (0x1A, "digit_7"),
        (0x1C, "digit_8"),
        (0x19, "digit_9"),
        (0x1D, "digit_0"),
        (0x00, "key_a"),
        (0x0B, "key_b"),
        (0x08, "key_c"),
        (0x02, "key_d"),
        (0x0E, "key_e"),
        (0x03, "key_f"),
        (0x05, "key_g"),
        (0x04, "key_h"),
        (0x22, "key_i"),
        (0x26, "key_j"),
        (0x28, "key_k"),
        (0x25, "key_l"),
        (0x2E, "key_m"),
        (0x2D, "key_n"),
        (0x1F, "key_o"),
        (0x23, "key_p"),
        (0x0C, "key_q"),
        (0x0F, "key_r"),
        (0x01, "key_s"),
        (0x11, "key_t"),
        (0x20, "key_u"),
        (0x09, "key_v"),
        (0x0D, "key_w"),
        (0x07, "key_x"),
        (0x10, "key_y"),
        (0x06, "key_z"),
        (0x21, "bracket_left"),
        (0x1E, "bracket_right"),
        (0x4C, "numpad_enter"),
    ];
    TABLE.iter().find(|(c, _)| *c == code).map(|(_, n)| *n)
}

impl WinState {
    // -----------------------------------------------------------------------------------------
    // Loading and reloading

    /// Loads the window halves once the window's session settled, then runs
    /// `window_start`.
    pub(super) fn plugins_settled(&mut self, window: &Window) {
        if !self.plugins.loaded {
            self.plugins.loaded = true;
            // What exists now came before any window half: those events are dropped.
            let sessions = self.sessions();
            self.plugins.seen_panes = sessions.panes().into_iter().collect();
            self.plugins.seen_tabs = sessions
                .sessions
                .iter()
                .flat_map(|s| s.tabs.iter().map(|t| t.id))
                .collect();
            self.plugins.own_panes.clear();
            let waker = window.waker();
            let ticker_waker = waker.clone();
            self.plugins.ticker = Some(Ticker::new(ticker_waker));
            let wake_runtime = waker.clone();
            let mut config = WindowConfig::standard(Arc::new(commands::builtin_ids()));
            config.waker = Some(Arc::new(move || wake_runtime.wake()));
            let dir = config.plugins_dir.clone();
            let flag = self.plugins.reload.clone();
            let watch_waker = waker;
            self.plugins.watcher = Some(Watcher::start(dir, move || {
                flag.store(true, Ordering::SeqCst);
                watch_waker.wake();
            }));
            let rt = {
                let mut host = Host { w: self };
                WindowPlugins::load(config, &mut host)
            };
            self.plugins.rt = Some(rt);
            self.plugins_catalog();
            self.plugins_rebuilt();
        }
        if !self.plugins.started {
            self.plugins.started = true;
            self.with_rt(|rt, host| rt.window_start(host));
        }
    }

    /// The keymap layer, sheets and formatter caches after a load or reload.
    pub(super) fn plugins_rebuilt(&mut self) {
        // The preset and user layers, from the settings; plugins fill the layer between.
        let preset = self.settings.preset();
        let user: Vec<(Chord, Vec<String>)> = self
            .settings
            .keybinds
            .iter()
            .map(|b| (b.chord.clone(), b.actions.clone()))
            .collect();
        let taken = |c: &Chord| user.iter().any(|(u, _)| u == c);
        self.plugins.preset = preset.into_iter().filter(|(c, _)| !taken(c)).collect();
        // `unbind` removes the keys without binding them again.
        self.plugins.user = user
            .iter()
            .filter(|(_, a)| !a.is_empty())
            .cloned()
            .collect();
        self.plugins.prefix.clear();
        self.refresh_sheets();
        let Some(rt) = self.plugins.rt.as_ref() else {
            return;
        };
        let defaults: Vec<Chord> = commands::default_chords()
            .iter()
            .filter_map(|c| Chord::parse(c).ok())
            .chain(self.plugins.preset.iter().map(|(c, _)| c.clone()))
            .collect();
        let mut keymap: Vec<(Chord, String)> = Vec::new();
        let mut dropped = Vec::new();
        for bind in rt.binds() {
            let Ok(chord) = Chord::parse(&bind.chord) else {
                dropped.push(bind.chord.clone());
                continue;
            };
            // Plugins only add keys: the defaults, and binds before them, keep theirs.
            if defaults.iter().any(|d| d.overlaps(&chord))
                || keymap.iter().any(|(c, _)| c.overlaps(&chord))
            {
                dropped.push(bind.chord.clone());
                continue;
            }
            // A user keybind replaces every binding of its keys.
            if taken(&chord) {
                continue;
            }
            keymap.push((chord, bind.action.clone()));
        }
        self.plugins.keymap = keymap;
        self.plugins.dropped_binds = dropped;
        if let Some(w) = &self.window {
            w.request_frame();
        }
    }

    /// The plugin sheets: the host's catalog styles (only
    /// Ready plugins contribute), local manifest styles too in a remote window, then `tern.css`
    /// sheets. They install into the chrome and every pane's documents.
    pub(super) fn refresh_sheets(&mut self) {
        let host = self.conn.host_name();
        // A plugin file's `url()` in manifest styles becomes a `data:` URL at install, as in
        // `tern.css` sheets. A local plugin resolves against its package directory; a
        // remote host's files aren't here, so only `data:` URLs pass in its styles.
        let dirs: BTreeMap<String, PathBuf> = if host == "local" {
            let root = auk_plugins::paths::plugins_dir(&auk_plugins::paths::config_dir());
            auk_plugins::discovery::scan(&root)
                .packages
                .into_iter()
                .map(|p| (p.manifest.id, p.dir))
                .collect()
        } else {
            BTreeMap::new()
        };
        let mut sheets: Vec<(String, String)> = self
            .plugins
            .catalog
            .styles()
            .map(|(id, css)| {
                let css = auk_plugins::css_url::inline_urls_logged(
                    css,
                    dirs.get(id).map(PathBuf::as_path),
                    id,
                );
                (format!("plugin:{host}:{id}:styles"), css)
            })
            .collect();
        let local = self
            .plugins
            .rt
            .as_ref()
            .map(WindowPlugins::sheets)
            .unwrap_or_default();
        if host != "local" {
            sheets.extend(
                local
                    .iter()
                    .filter(|(n, _)| n.ends_with(":styles"))
                    .cloned(),
            );
        }
        sheets.extend(local.into_iter().filter(|(n, _)| !n.ends_with(":styles")));
        if self.chrome.set_plugin_sheets(&sheets) {
            self.ui_dirty = true;
        }
        self.plugins.sheets = sheets;
        if let Some(w) = &self.window {
            w.request_frame();
        }
    }

    /// Reloads every window half.
    pub(super) fn plugins_reload(&mut self) {
        if self.plugins.rt.is_none() {
            return;
        }
        self.with_rt(|rt, host| rt.reload(host));
        self.plugins_catalog();
        self.plugins_rebuilt();
        // A reloaded runtime runs window_start for plugins that haven't run it in this window.
        self.with_rt(|rt, host| rt.window_start(host));
    }

    /// The host's catalog, when a new one arrived.
    pub(super) fn plugins_catalog(&mut self) {
        let (generation, catalog) = {
            let shared = self.conn.shared();
            (shared.catalog_generation, shared.catalog.clone())
        };
        if generation == self.plugins.catalog_generation {
            return;
        }
        self.plugins.catalog_generation = generation;
        let catalog: Catalog = serde_json::from_value(catalog).unwrap_or_default();
        self.plugins.catalog = catalog.clone();
        if let Some(rt) = self.plugins.rt.as_mut() {
            rt.set_catalog(catalog);
        }
        self.refresh_sheets();
    }

    /// Lends the runtime to `f` with this window as its host; queued plugin actions run after.
    pub(super) fn with_rt<R>(
        &mut self,
        f: impl FnOnce(&mut WindowPlugins, &mut dyn WindowHost) -> R,
    ) -> Option<R> {
        let mut rt = self.plugins.rt.take()?;
        let r = {
            let mut host = Host { w: self };
            f(&mut rt, &mut host)
        };
        self.plugins.rt = Some(rt);
        let queued = std::mem::take(&mut self.plugins.queued);
        for action in queued {
            self.run_plugin_action(&action);
        }
        Some(r)
    }

    // -----------------------------------------------------------------------------------------
    // Actions, overrides, keys

    /// Runs an action by name: a plugin's, else a built-in command (with its overrides).
    pub(super) fn run_action_named(&mut self, name: &str) -> Result<(), String> {
        if chord::plugin_action(name).is_some() {
            return match self.with_rt(|rt, host| rt.run_action(host, name)) {
                Some(Ok(true)) => Ok(()),
                Some(Ok(false)) | None => {
                    self.toast(ToastLevel::Error, "No plugin has this action", Some(name));
                    Err(format!("no plugin has the action `{name}`"))
                }
                Some(Err(e)) => Err(e),
            };
        }
        let cmd =
            commands::by_builtin_id(name).ok_or_else(|| format!("unknown command `{name}`"))?;
        let window = self.window.clone().ok_or("no window")?;
        self.command(&window, cmd);
        Ok(())
    }

    fn run_plugin_action(&mut self, name: &str) {
        if let Err(e) = self.run_action_named(name) {
            eprintln!("auk: action {name}: {e}");
        }
    }

    /// Runs the overrides of a built-in command; true skips the built-in.
    pub(super) fn run_override(&mut self, cmd: Cmd) -> bool {
        let id = commands::builtin_id(cmd);
        self.with_rt(|rt, host| rt.run_override(host, &id))
            .unwrap_or(false)
    }

    /// A modifier change: taps for `cmd>cmd`-style chords.
    pub(super) fn plugin_modifiers(&mut self, mods: auk_term::input::Mods) {
        let now = chord::Mods {
            ctrl: mods.ctrl,
            alt: mods.alt,
            shift: mods.shift,
            cmd: mods.meta,
        };
        let count =
            u8::from(now.ctrl) + u8::from(now.alt) + u8::from(now.shift) + u8::from(now.cmd);
        if count == 1 {
            self.plugins.tap = Some(now);
            return;
        }
        if count == 0
            && let Some(tapped) = self.plugins.tap.take()
        {
            // Taps in a sequence come within 0.5 s of each other; a later one starts over.
            let now = Instant::now();
            let in_time = self
                .plugins
                .last_tap
                .is_some_and(|t| now.duration_since(t) <= TAP_WITHIN);
            let taps_pending = !self.plugins.prefix.is_empty()
                && self.plugins.prefix.iter().all(|s| s.key.is_none());
            if taps_pending && !in_time {
                self.plugins.prefix.clear();
            }
            self.plugins.last_tap = Some(now);
            let step = Step {
                mods: tapped,
                key: None,
            };
            self.chord_step(vec![step]);
            return;
        }
        self.plugins.tap = None;
    }

    /// A key press against the plugin keymap. Returns whether the keymap took it.
    pub(super) fn plugin_key(&mut self, key: &KeyInput) -> bool {
        self.plugins.tap = None;
        // Taps come only in sequences of taps: a key ends one under way, and goes on as a key.
        if !self.plugins.prefix.is_empty() && self.plugins.prefix.iter().all(|s| s.key.is_none()) {
            self.plugins.prefix.clear();
        }
        if self.plugins.keymap.is_empty()
            && self.plugins.preset.is_empty()
            && self.plugins.user.is_empty()
        {
            return false;
        }
        self.chord_step(key_steps(key))
    }

    fn chord_step(&mut self, candidates: Vec<Step>) -> bool {
        let prefix = self.plugins.prefix.clone();
        let mut partial = None;
        for step in &candidates {
            let mut seq = prefix.clone();
            seq.push(step.clone());
            // The user layer, then plugins, then the preset.
            let found: Option<Vec<String>> = self
                .plugins
                .user
                .iter()
                .find(|(c, _)| c.steps == seq)
                .map(|(_, a)| a.clone())
                .or_else(|| {
                    self.plugins
                        .keymap
                        .iter()
                        .chain(&self.plugins.preset)
                        .find(|(c, _)| c.steps == seq)
                        .map(|(_, a)| vec![a.clone()])
                });
            if let Some(actions) = found {
                self.plugins.prefix.clear();
                // A list runs the first action that applies.
                for action in &actions {
                    match self.run_keymap_action(action) {
                        Ok(()) => break,
                        Err(e) => eprintln!("auk: action {action}: {e}"),
                    }
                }
                return true;
            }
            let longer = |c: &Chord| c.steps.len() > seq.len() && c.steps[..seq.len()] == seq[..];
            if partial.is_none()
                && (self.plugins.user.iter().any(|(c, _)| longer(c))
                    || self
                        .plugins
                        .keymap
                        .iter()
                        .chain(&self.plugins.preset)
                        .any(|(c, _)| longer(c)))
            {
                partial = Some(seq);
            }
        }
        if let Some(seq) = partial {
            self.plugins.prefix = seq;
            return true;
        }
        // A sequence that went nowhere swallows its last key.
        let was_pending = !self.plugins.prefix.is_empty();
        self.plugins.prefix.clear();
        was_pending && candidates.iter().any(|s| s.key.is_some())
    }

    // -----------------------------------------------------------------------------------------
    // Opens and routes

    /// Opens a path or URL as a route would see it: `route.open` for a path (origin `cli`,
    /// `palette` or `link`), `route.link` first for a URL or a link click.
    pub(super) fn open_routed(&mut self, target: &str, origin: &str, pane: Option<PaneId>) {
        let is_url = target.contains("://") || target.starts_with("mailto:");
        if is_url || origin == "link" && pane.is_some() && target.contains(':') {
            let req = LinkRequest {
                url: target.to_owned(),
                pane: pane.or(self.focused_pane()).unwrap_or(0),
                mods: LinkMods::default(),
            };
            let decision = self.with_rt(|rt, host| rt.route_link(host, &req)).flatten();
            if let Some(d) = decision {
                self.apply_link_decision(target, d);
                return;
            }
            if target
                .get(..5)
                .is_some_and(|s| s.eq_ignore_ascii_case("file:"))
            {
                match file_url_path(target) {
                    Some(path) => self.open_path_routed(&path, origin, How::Default, None),
                    None => self.refuse_link(target, "Only local file paths open from a link"),
                }
                return;
            }
            if is_url {
                self.builtin_open(target, None, "link");
                return;
            }
        }
        self.open_path_routed(target, origin, How::Default, None);
    }

    /// ⌘-click on the grid: an OSC 8 link goes to `route.link` (then `route.open` for a
    /// file), a printed path straight to `route.open` with origin `link`. Returns whether
    /// there was something to open.
    pub(super) fn grid_link_click(&mut self, pane: PaneId, line: auk_term::Line, col: u16) -> bool {
        let found = {
            let shared = self.conn.shared();
            let Some(data) = shared.panes.get(&pane) else {
                return false;
            };
            let term = data.replica.terminal();
            let Some(row) = term.line(line) else {
                return false;
            };
            let link = row
                .cells
                .get(usize::from(col))
                .map(|c| c.link)
                .filter(|l| *l != 0)
                .and_then(|l| term.links().get(l as usize).cloned());
            match link {
                Some(url) => Some((url, true)),
                None => {
                    let (lo, hi) = crate::select::word_bounds(term, line, col);
                    let word: String = row.cells
                        [usize::from(lo)..=usize::from(hi).min(row.cells.len() - 1)]
                        .iter()
                        .map(|c| {
                            if c.text.is_empty() {
                                " "
                            } else {
                                c.text.as_str()
                            }
                        })
                        .collect();
                    let word = word
                        .trim()
                        .trim_end_matches([',', ';', ')', '.', ':'])
                        .to_owned();
                    (!word.is_empty()).then_some((word, false))
                }
            }
        };
        let Some((target, is_link)) = found else {
            return false;
        };
        if is_link {
            self.open_routed(&target, "link", Some(pane));
            return true;
        }
        // A printed path, with an optional `:line[:col]`: only one that names a file.
        let mut parts = target.splitn(3, ':');
        let path = parts.next().unwrap_or_default().to_owned();
        let line_no = parts.next().and_then(|l| l.parse::<u64>().ok());
        let full = match path.strip_prefix("~/") {
            Some(rest) => format!("{}/{rest}", std::env::var("HOME").unwrap_or_default()),
            None if path.starts_with('/') => path.clone(),
            None => match self.pane_cwd(pane) {
                Some(cwd) => format!("{}/{path}", cwd.trim_end_matches('/')),
                None => return false,
            },
        };
        if !Path::new(&full).exists() {
            return false;
        }
        self.open_path_routed(&full, "link", How::Default, line_no);
        true
    }

    /// "Open file…" in the palette: `route.open` with origin `palette`.
    pub(super) fn open_path_from_palette(&mut self, path: &str) {
        self.open_path_routed(path, "palette", How::Default, None);
    }

    fn open_path_routed(&mut self, path: &str, origin: &str, how: How, line: Option<u64>) {
        let path = self.absolute(path);
        let req = OpenRequest {
            path: path.clone(),
            line,
            col: None,
            host: "local".into(),
            how,
            origin: origin.into(),
        };
        let decision = self.with_rt(|rt, host| rt.route_open(host, &req)).flatten();
        match decision {
            None => self.builtin_open(&path, Some(how), origin),
            Some(RouteDecision::Block { kind, args }) => {
                self.plugins
                    .routed
                    .push(json!({"path": path, "origin": origin, "block": kind, "args": args}));
                let how = if how == How::Default {
                    How::Beside
                } else {
                    how
                };
                let cwd = Path::new(&path).parent().map(|p| p.display().to_string());
                if self
                    .open_block(&kind, &args, how, Reveal { focus: true }, cwd)
                    .is_err()
                {
                    self.toast(
                        ToastLevel::Error,
                        "Could not open the block",
                        Some(&format!("No plugin on local defines block type {kind}")),
                    );
                }
            }
            Some(RouteDecision::Url(url)) => {
                self.plugins
                    .routed
                    .push(json!({"path": path, "origin": origin, "url": url}));
                let req = LinkRequest {
                    url: url.clone(),
                    pane: self.focused_pane().unwrap_or(0),
                    mods: LinkMods::default(),
                };
                match self.with_rt(|rt, host| rt.route_link(host, &req)).flatten() {
                    Some(d) => self.apply_link_decision(&url, d),
                    None => self.builtin_open(&url, None, origin),
                }
            }
            Some(RouteDecision::Path { path: to, how: h }) => {
                self.plugins
                    .routed
                    .push(json!({"path": path, "origin": origin, "rewritten": to}));
                self.builtin_open(&to, h.or(Some(how)), origin);
            }
            Some(RouteDecision::Handled) => {
                self.plugins
                    .routed
                    .push(json!({"path": path, "origin": origin, "handled": true}));
            }
        }
    }

    fn apply_link_decision(&mut self, url: &str, d: RouteDecision) {
        match d {
            RouteDecision::Block { kind, args } => {
                self.plugins
                    .routed
                    .push(json!({"url": url, "block": kind, "args": args}));
                let cwd = self.focused_cwd();
                if self
                    .open_block(&kind, &args, How::Default, Reveal { focus: true }, cwd)
                    .is_err()
                {
                    self.toast(
                        ToastLevel::Error,
                        "Could not open the block",
                        Some(&format!("No plugin on local defines block type {kind}")),
                    );
                }
            }
            RouteDecision::Url(to) => {
                self.plugins
                    .routed
                    .push(json!({"url": url, "rewritten": to}));
                self.builtin_open(&to, None, "link");
            }
            RouteDecision::Path { path, how } => {
                self.plugins.routed.push(json!({"url": url, "path": path}));
                self.builtin_open(&path, how, "link");
            }
            RouteDecision::Handled => {
                self.plugins
                    .routed
                    .push(json!({"url": url, "handled": true}));
            }
        }
    }

    /// The built-in open: with no file blocks in v1, the system opener.
    pub(super) fn builtin_open(&mut self, target: &str, how: Option<How>, origin: &str) {
        let is_url = target.contains("://") || target.starts_with("mailto:");
        let mut target = if is_url {
            target.to_owned()
        } else {
            self.absolute(target)
        };
        let mut reveal = false;
        // A link (from a program's output or a surface, or a route's answer to one) opens a page
        // or a path, and never runs a command.
        if origin == "link" {
            if is_url {
                let scheme = target
                    .split(':')
                    .next()
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if scheme == "file" {
                    match file_url_path(&target) {
                        Some(path) => target = path,
                        None => {
                            return self
                                .refuse_link(&target, "Only local file paths open from a link");
                        }
                    }
                } else if COMMAND_SCHEMES.contains(&scheme.as_str()) {
                    return self.refuse_link(&target, "Links don't run commands");
                }
            }
            reveal = !target.contains("://") && runs_something(&target);
        }
        self.plugins.opened.push(json!({
            "target": target,
            "how": if reveal { Some("reveal".to_owned()) } else { how.map(|h| format!("{h:?}").to_lowercase()) },
            "origin": origin,
        }));
        if reveal {
            system_reveal(&target);
        } else {
            system_open(&target);
        }
    }

    /// A link that may not open: a toast, and a note for the automation endpoint.
    fn refuse_link(&mut self, target: &str, why: &str) {
        self.plugins
            .refused
            .push(json!({"target": target, "why": why}));
        self.toast(ToastLevel::Error, why, Some(target));
    }

    fn absolute(&self, path: &str) -> String {
        if let Some(rest) = path.strip_prefix("~/") {
            return format!("{}/{rest}", std::env::var("HOME").unwrap_or_default());
        }
        if path.starts_with('/') {
            return path.to_owned();
        }
        match self.focused_cwd() {
            Some(cwd) => format!("{}/{path}", cwd.trim_end_matches('/')),
            None => path.to_owned(),
        }
    }

    pub(super) fn focused_cwd(&self) -> Option<String> {
        let pane = self.focused_pane()?;
        self.pane_cwd(pane)
    }

    fn pane_cwd(&self, pane: PaneId) -> Option<String> {
        let shared = self.conn.shared();
        shared
            .panes
            .get(&pane)
            .and_then(|p| p.replica.cwd().map(osc7_path))
            .filter(|c| !c.is_empty())
    }

    /// Opens a block pane through the window's host's daemon, placed by `how`: its pane id. `Err`
    /// (with the "Could not start the block" card up) when no Ready plugin defines the kind, or
    /// the daemon refused.
    pub(super) fn open_block(
        &mut self,
        kind: &str,
        args: &[String],
        how: How,
        reveal: Reveal,
        cwd: Option<String>,
    ) -> Result<u64, ()> {
        match self.try_open_block(kind, args, how, reveal, cwd.clone()) {
            Ok(pane) => {
                self.plugins.block_card = None;
                Ok(pane)
            }
            Err(error) => {
                // "Could not start the block", with Retry.
                eprintln!("auk: block {kind}: {error}");
                self.plugins.block_card = Some(BlockCard {
                    kind: kind.to_owned(),
                    args: args.to_vec(),
                    how,
                    reveal,
                    cwd,
                    error,
                });
                self.ui_dirty = true;
                if let Some(w) = &self.window {
                    w.request_frame();
                }
                Err(())
            }
        }
    }

    fn try_open_block(
        &mut self,
        kind: &str,
        args: &[String],
        how: How,
        reveal: Reveal,
        cwd: Option<String>,
    ) -> Result<u64, String> {
        self.plugins_catalog();
        if self.plugins.catalog.block_type(kind).is_none() {
            return Err(format!("No plugin offers the block type {kind}"));
        }
        let place = match (how, self.focused_pane()) {
            (How::Below, Some(pane)) => OpenPlace::Split {
                pane,
                side: Side::Down,
                share: None,
            },
            (How::Beside | How::Split | How::Default | How::Replace | How::Preview, Some(pane)) => {
                OpenPlace::Split {
                    pane,
                    side: Side::Right,
                    share: None,
                }
            }
            _ => match self.active_session {
                Some(session) => OpenPlace::Tab { session },
                None => OpenPlace::Session { name: None },
            },
        };
        let launch = auk_mirror::Launch {
            cwd,
            command: None,
            block: Some(kind.to_owned()),
            args: args.to_vec(),
        };
        let pane = self.open_pane(place, launch)?;
        if reveal.focus {
            self.plugins.focus_when_shown = Some(pane);
        }
        Ok(pane)
    }

    /// Opens a pane through the window's host's daemon: its id, once the window's layout shows
    /// it.
    fn open_pane(&mut self, place: OpenPlace, launch: auk_mirror::Launch) -> Result<u64, String> {
        let v = self.conn.request(LayoutEdit::Open { place, launch })?;
        let pane = v["pane"]
            .as_u64()
            .ok_or_else(|| format!("the daemon answered without a pane: {v}"))?;
        // This window added it (for `pane_created`).
        self.plugins.own_panes.insert(pane);
        Ok(pane)
    }

    // -----------------------------------------------------------------------------------------
    // Toasts

    pub(super) fn toast(&mut self, level: ToastLevel, text: &str, sub: Option<&str>) {
        let item = ToastItem {
            level: level_name(level).to_owned(),
            text: text.to_owned(),
            sub: sub.map(str::to_owned),
        };
        self.plugins
            .toast_log
            .push(json!({"level": item.level, "text": item.text, "sub": item.sub}));
        let until = Instant::now() + TOAST_FOR;
        self.plugins.toasts.push(Toast { item, until });
        if self.plugins.toasts.len() > 4 {
            self.plugins.toasts.remove(0);
        }
        if let Some(t) = &self.plugins.ticker {
            t.at(until);
        }
        self.ui_dirty = true;
        if let Some(w) = &self.window {
            w.request_frame();
        }
    }

    // -----------------------------------------------------------------------------------------
    // News: effects, layout, streams, timers

    /// After the daemon's news (layout, effects, catalog): queues window events, carries out
    /// effects, reloads on change, runs due callbacks.
    pub(super) fn plugins_wake(&mut self, window: &Window) {
        if !self.plugins.loaded {
            return;
        }
        if self.plugins.reload.swap(false, Ordering::SeqCst) {
            self.plugins_reload();
        }
        self.plugins_catalog();
        self.plugins_layout(window);
        let effects = std::mem::take(&mut self.conn.shared().effects);
        for effect in effects {
            match effect {
                auk_mirror::Effect::Toast { level, text, sub } => {
                    let level = match level.as_str() {
                        "success" => ToastLevel::Success,
                        "error" | "warn" | "warning" => ToastLevel::Error,
                        _ => ToastLevel::Info,
                    };
                    self.toast(level, &text, sub.as_deref());
                }
                auk_mirror::Effect::Copy { text } => {
                    crate::app::clipboard_set(&text);
                }
                auk_mirror::Effect::Open { target, origin } => match origin.as_deref() {
                    // `auk open`: route.open with origin cli.
                    None => self.open_path_routed(&target, "cli", How::Default, None),
                    Some(o) => self.open_routed(&target, o, None),
                },
            }
        }
        self.plugins_pump();
    }

    /// Runs due plugin callbacks and settles, and schedules the next wake.
    fn plugins_pump(&mut self) {
        self.check_settles();
        let due = self
            .plugins
            .rt
            .as_ref()
            .and_then(WindowPlugins::next_wake)
            .is_some_and(|t| t <= Instant::now());
        if due {
            self.with_rt(|rt, host| rt.pump(host));
        }
        let now = Instant::now();
        let before = self.plugins.toasts.len();
        self.plugins.toasts.retain(|t| t.until > now);
        if self.plugins.toasts.len() != before {
            self.ui_dirty = true;
            if let Some(w) = &self.window {
                w.request_frame();
            }
        }
        if let Some(t) = &self.plugins.ticker {
            if let Some(next) = self.plugins.rt.as_ref().and_then(WindowPlugins::next_wake) {
                t.at(next);
            }
            if let Some(next) = self.plugins.toasts.iter().map(|t| t.until).min() {
                t.at(next);
            }
            if let Some(next) = self.plugins.settles.iter().map(|s| s.deadline).min() {
                t.at(next);
            }
        }
        if self
            .plugins
            .rt
            .as_mut()
            .is_some_and(WindowPlugins::take_chrome_dirty)
        {
            self.ui_dirty = true;
            if let Some(w) = &self.window {
                w.request_frame();
            }
        }
    }

    /// A request this window sent that adds a pane ([`WinState::create`]).
    pub(super) fn plugins_creating(&mut self, id: u64) {
        self.plugins.creating.push(id);
    }

    /// Pane and tab events from a layout change, and the pane kinds behind them. `pane_created`
    /// and `tab_created` fire only for what this window added: panes restored from the daemon
    /// or added by another window don't.
    fn plugins_layout(&mut self, window: &Window) {
        // The answers that came: the panes this window added.
        let creating = std::mem::take(&mut self.plugins.creating);
        for id in creating {
            match self.conn.take_reply(id) {
                Some(Ok(v)) => {
                    if let Some(pane) = v["pane"].as_u64() {
                        self.plugins.own_panes.insert(pane);
                    }
                }
                Some(Err(_)) => {}
                None => self.plugins.creating.push(id),
            }
        }
        let sessions = self.sessions();
        let all: BTreeSet<PaneId> = sessions.panes().into_iter().collect();
        // A new pane nobody has claimed yet, while this window still waits for an answer, may be
        // this window's: it counts once the answer says.
        let waiting = !self.plugins.creating.is_empty();
        let panes: BTreeSet<PaneId> = all
            .iter()
            .copied()
            .filter(|p| {
                self.plugins.seen_panes.contains(p)
                    || self.plugins.own_panes.contains(p)
                    || !waiting
            })
            .collect();
        let tab_list: Vec<(TabId, Vec<PaneId>)> = sessions
            .sessions
            .iter()
            .flat_map(|s| s.tabs.iter().map(|t| (t.id, t.root.panes())))
            .collect();
        let tabs: BTreeSet<TabId> = tab_list
            .iter()
            .filter(|(_, ps)| ps.iter().all(|p| panes.contains(p)))
            .map(|(t, _)| *t)
            .chain(
                self.plugins
                    .seen_tabs
                    .iter()
                    .copied()
                    .filter(|t| tab_list.iter().any(|(x, _)| x == t)),
            )
            .collect();
        if panes != self.plugins.seen_panes || tabs != self.plugins.seen_tabs {
            for &tab in tabs.difference(&self.plugins.seen_tabs) {
                let ours = tab_list
                    .iter()
                    .find(|(t, _)| *t == tab)
                    .is_some_and(|(_, ps)| ps.iter().any(|p| self.plugins.own_panes.contains(p)));
                if ours {
                    self.plugins.events.push(WindowEvent::TabCreated { tab });
                }
            }
            for &pane in panes.difference(&self.plugins.seen_panes) {
                if self.plugins.own_panes.contains(&pane) {
                    self.plugins.events.push(WindowEvent::PaneCreated { pane });
                }
            }
            for &pane in self.plugins.seen_panes.difference(&panes) {
                self.plugins.events.push(WindowEvent::PaneClosed { pane });
                self.plugins.track.remove(&pane);
            }
            for &tab in self.plugins.seen_tabs.difference(&tabs) {
                self.plugins.events.push(WindowEvent::TabClosed { tab });
            }
            self.plugins.own_panes.retain(|p| !panes.contains(p));
            self.plugins.seen_panes = panes.clone();
            self.plugins.seen_tabs = tabs;
            self.refresh_kinds();
            window.request_frame();
        }
        if let Some(p) = self.plugins.focus_when_shown
            && panes.contains(&p)
        {
            self.plugins.focus_when_shown = None;
            self.focus_pane(p);
        }
    }

    /// Learns which panes are blocks, from the daemon; a remote window, which has no control
    /// endpoint to ask, from the layout's pane tags (block types aren't named there).
    fn refresh_kinds(&mut self) {
        if is_remote() {
            let sessions = self.sessions();
            self.plugins.kinds = sessions
                .kinds
                .iter()
                .map(|(&pane, &tag)| {
                    let kind = match tag {
                        PaneTag::Shell => "shell",
                        PaneTag::Command => "command",
                        PaneTag::Agent => "agent",
                        PaneTag::Block => "block",
                    };
                    (pane, (kind.to_owned(), None))
                })
                .collect();
            return;
        }
        let Ok(ls) = control(&["ls".to_owned()]) else {
            return;
        };
        let mut kinds = BTreeMap::new();
        for p in ls["panes"].as_array().into_iter().flatten() {
            let Some(id) = p["id"].as_u64() else { continue };
            let kind = p["kind"]["kind"].as_str().unwrap_or("shell").to_owned();
            let block = p["kind"]["block"].as_str().map(str::to_owned);
            kinds.insert(id, (kind, block));
        }
        self.plugins.kinds = kinds;
    }

    /// Takes the streams' shell-integration news into pane tracking and window events.
    fn drain_notes(&mut self) {
        let notes: Vec<(PaneId, Vec<PaneNote>)> = {
            let mut shared = self.conn.shared();
            shared
                .panes
                .iter_mut()
                .filter(|(_, d)| !d.notes.is_empty())
                .map(|(id, d)| (*id, std::mem::take(&mut d.notes)))
                .collect()
        };
        let loaded = self.plugins.loaded;
        for (pane, notes) in notes {
            for note in notes {
                let track = self.plugins.track.entry(pane).or_default();
                let event = match note {
                    PaneNote::CommandStarted(line) => {
                        track.busy = true;
                        track.running = Some((line.clone(), Instant::now()));
                        WindowEvent::CommandStarted { pane, line }
                    }
                    PaneNote::CommandFinished(status) => {
                        track.busy = false;
                        let (line, took) = track
                            .running
                            .take()
                            .map(|(l, t)| (l, t.elapsed().as_secs_f64() * 1000.0))
                            .unwrap_or_default();
                        track.last = Some((line.clone(), status, took));
                        *self.plugins.finished.entry(pane).or_default() += 1;
                        WindowEvent::CommandFinished {
                            pane,
                            line,
                            status,
                            took_ms: took,
                        }
                    }
                    PaneNote::Cwd(cwd) => WindowEvent::Cwd {
                        pane,
                        path: osc7_path(&cwd),
                    },
                    PaneNote::Title(title) => WindowEvent::Title { pane, title },
                };
                if loaded {
                    self.plugins.events.push(event);
                }
            }
        }
    }

    /// After each frame: the focus event, queued window events, due callbacks.
    pub(super) fn plugins_after_frame(&mut self) {
        self.drain_notes();
        if !self.plugins.loaded {
            return;
        }
        if let (Some(pane), Some(tab)) = (self.focused_pane(), self.active_tab_id())
            && self.plugins.last_focus != Some((pane, tab))
        {
            self.plugins.last_focus = Some((pane, tab));
            self.plugins.events.push(WindowEvent::Focus { pane, tab });
        }
        let events = std::mem::take(&mut self.plugins.events);
        if !events.is_empty() {
            self.with_rt(|rt, host| rt.deliver(host, &events));
        }
        let toasts = self
            .plugins
            .rt
            .as_mut()
            .map(WindowPlugins::take_toasts)
            .unwrap_or_default();
        for (text, sub) in toasts {
            self.toast(ToastLevel::Error, &text, Some(&sub));
        }
        self.plugins_pump();
    }

    fn check_settles(&mut self) {
        if self.plugins.settles.is_empty() {
            return;
        }
        let now = Instant::now();
        let settles = std::mem::take(&mut self.plugins.settles);
        for s in settles {
            let alive = self.known_panes.contains(&s.pane);
            let finished = self.plugins.finished.get(&s.pane).copied().unwrap_or(0);
            if !alive {
                s.done.resolve(Err("the pane closed".into()));
            } else if finished > s.finished_before || now >= s.deadline {
                let text = {
                    let shared = self.conn.shared();
                    shared
                        .panes
                        .get(&s.pane)
                        .map(|p| p.replica.screen_text())
                        .unwrap_or_default()
                };
                s.done.resolve(Ok(json!(text.trim_end())));
            } else {
                self.plugins.settles.push(s);
            }
        }
    }

    // -----------------------------------------------------------------------------------------
    // Chrome formatters

    /// The pane as window plugins see it.
    pub(super) fn window_pane(&self, pane: PaneId, tab: Option<TabId>) -> WindowPane {
        let (title, cwd) = {
            let shared = self.conn.shared();
            match shared.panes.get(&pane) {
                Some(p) => (
                    p.replica.terminal().title().to_owned(),
                    p.replica.cwd().map(osc7_path).unwrap_or_default(),
                ),
                None => (String::new(), String::new()),
            }
        };
        let track = self.plugins.track.get(&pane);
        let (kind, block) = self
            .plugins
            .kinds
            .get(&pane)
            .cloned()
            .unwrap_or_else(|| ("shell".to_owned(), None));
        let is_block = kind == "block";
        let busy = !is_block && track.is_some_and(|t| t.busy);
        let program = if is_block {
            String::new()
        } else {
            track
                .and_then(|t| t.running.as_ref())
                .map(|(l, _)| program_of(l))
                .filter(|p| !p.is_empty())
                .unwrap_or_else(shell_name)
        };
        WindowPane {
            pane,
            cwd,
            program,
            title,
            busy,
            kind: if is_block {
                "plugin".into()
            } else {
                "terminal".into()
            },
            tab,
            focused: self.focused_pane() == Some(pane),
            block,
        }
    }

    fn tab_infos(&self) -> Vec<TabInfo> {
        let sessions = self.sessions();
        let mut out = Vec::new();
        for s in &sessions.sessions {
            for (i, t) in s.tabs.iter().enumerate() {
                let panes = t.root.panes();
                let focus = self
                    .tab_focus
                    .get(&t.id)
                    .copied()
                    .or_else(|| panes.first().copied())
                    .unwrap_or(0);
                let info = self.window_pane(focus, Some(t.id));
                let busy = panes
                    .iter()
                    .any(|p| self.plugins.track.get(p).is_some_and(|t| t.busy));
                out.push(TabInfo {
                    id: t.id,
                    session: s.id,
                    color: None,
                    position: i as u64 + 1,
                    pane: focus,
                    name: t.title.clone(),
                    cwd: info.cwd,
                    program: info.program,
                    title: info.title,
                    panes: panes.len() as u64,
                    busy,
                });
            }
        }
        out
    }

    /// Runs the chrome formatters (4 ms each, cached by input) for the frame.
    pub(super) fn plugins_chrome(&mut self) {
        if self.plugins.rt.is_none() {
            self.plugins.status.clear();
            self.plugins.tab_titles.clear();
            return;
        }
        let status_input = self.focused_pane().map(|p| {
            let info = self.window_pane(p, self.active_tab_id());
            StatusPane {
                pane: p,
                cwd: info.cwd,
                program: info.program,
                title: info.title,
                busy: info.busy,
            }
        });
        let tabs: Vec<TabInfo> = self
            .tab_infos()
            .into_iter()
            .filter(|t| Some(t.session) == self.active_session)
            .collect();
        let win = json!({
            "key": self.conn.window_key,
            "session": self.active_session,
            "tab": self.active_tab_id(),
            "pane": self.focused_pane(),
            "title": status_input.as_ref().map(|s| s.title.clone()),
            "cwd": status_input.as_ref().map(|s| s.cwd.clone()),
        });
        let rt = self.plugins.rt.as_mut().expect("checked");
        let status = match &status_input {
            Some(input) => rt.status(input),
            None => Vec::new(),
        };
        let mut titles = BTreeMap::new();
        for t in &tabs {
            if let Some(title) = rt.tab_title(t) {
                titles.insert(t.id, title);
            }
        }
        let window_title = rt.window_title(&win);
        self.plugins.status = status
            .into_iter()
            .map(|s| StatusItem {
                plugin: s.plugin,
                text: s.text,
                icon: s.icon,
                tone: s.tone,
                command: s.command,
            })
            .collect();
        self.plugins.tab_titles = titles;
        // The window applies it with the frame, over its default (the session's name).
        self.plugins.window_title = window_title;
    }

    // -----------------------------------------------------------------------------------------
    // Layout operations for the window cx

    /// Whether a block kind is one this window's host can start: a Ready plugin defines it.
    fn check_block(&mut self, launch: &Launch) -> Result<(), String> {
        match &launch.block {
            Some(kind) => {
                self.plugins_catalog();
                if self.plugins.catalog.block_type(kind).is_none() {
                    return Err(format!("could not start block `{kind}`"));
                }
                Ok(())
            }
            None => Ok(()),
        }
    }

    /// A new tab in the shown session running `launch`; the new pane.
    fn new_tab_launch(&mut self, launch: &Launch, cwd: Option<&str>) -> Result<u64, String> {
        let session = self.active_session.ok_or("the window shows no session")?;
        self.check_block(launch)?;
        self.open_pane(OpenPlace::Tab { session }, launch_of(launch, cwd))
    }

    /// Splits `target`, running `launch` in the new pane on its `dir` side. `share` is the
    /// target's share of the split, in thousandths (half by default).
    fn split_launch(
        &mut self,
        target: u64,
        dir: Dir,
        launch: &Launch,
        cwd: Option<&str>,
        share: Option<u16>,
    ) -> Result<u64, String> {
        self.check_block(launch)?;
        let place = OpenPlace::Split {
            pane: target,
            side: side(dir),
            share,
        };
        self.open_pane(place, launch_of(launch, cwd))
    }

    /// The first leaf of a tab spec.
    fn first_leaf(spec: &TabSpec) -> &TabSpec {
        match &spec.first {
            Some(first) if spec.split.is_some() || spec.second.is_some() => Self::first_leaf(first),
            _ => spec,
        }
    }

    /// Builds the rest of `spec` around its first leaf, already open as `pane`: `[2]` goes on the
    /// `split` side of `[1]` (right by default), `[1]` keeping `ratio` of it.
    fn fill(&mut self, spec: &TabSpec, pane: u64, cwd: Option<&str>, problems: &mut Vec<String>) {
        let cwd = spec.cwd.as_deref().or(cwd).map(str::to_owned);
        let (Some(first), Some(second)) = (&spec.first, &spec.second) else {
            return;
        };
        let dir = spec.split.unwrap_or(Dir::Right);
        let share = spec
            .ratio
            .filter(|r| r.is_finite())
            .map(|r| (r * 1000.0).round().clamp(0.0, 1000.0) as u16);
        let leaf = Self::first_leaf(second);
        let leaf_cwd = leaf
            .cwd
            .clone()
            .or_else(|| second.cwd.clone())
            .or_else(|| cwd.clone());
        let launch = leaf.launch.clone().unwrap_or_default();
        match self.split_launch(pane, dir, &launch, leaf_cwd.as_deref(), share) {
            Ok(new) => {
                self.fill(first, pane, cwd.as_deref(), problems);
                self.fill(second, new, cwd.as_deref(), problems);
            }
            Err(e) => {
                problems.push(e);
                self.fill(first, pane, cwd.as_deref(), problems);
            }
        }
    }

    fn tab_of_pane(&self, pane: u64) -> Option<(SessionId, TabId)> {
        self.sessions().sessions.iter().find_map(|s| {
            s.tabs
                .iter()
                .find(|t| t.root.contains(pane))
                .map(|t| (s.id, t.id))
        })
    }

    /// The thousandths of `pane`'s nearest split on `side` that `cells` of its cells make, as
    /// this window lays the split out; `None` when no split runs that way.
    fn resize_delta(&self, pane: u64, side: Side, cells: f64) -> Option<i32> {
        let sessions = self.sessions();
        let tab = sessions
            .sessions
            .iter()
            .flat_map(|s| s.tabs.iter())
            .find(|t| t.root.contains(pane))?;
        let path = tab.root.divider(pane, side)?;
        let area = crate::geometry::node_area(
            &tab.root,
            self.content_area(),
            crate::chrome::DIVIDER,
            &path,
        )?;
        let scale = self.size.map_or(2.0, |s| s.scale);
        let (cell_w, cell_h) = self.gfx.as_ref().map_or((8.0, 16.0), |g| {
            let m = g.metrics();
            (
                f64::from(m.cell_width) / scale,
                f64::from(m.cell_height) / scale,
            )
        });
        let (extent, cell) = match side.axis() {
            SplitDir::Right => (area.w, cell_w),
            SplitDir::Down => (area.h, cell_h),
        };
        let avail = (extent - crate::chrome::DIVIDER).max(1.0);
        Some((cells * cell / avail * 1000.0).round() as i32)
    }
}

impl WindowHost for Host<'_> {
    fn run(&mut self, pane: u64, text: &str) -> Result<(), String> {
        let running = {
            let shared = self.w.conn.shared();
            shared.panes.get(&pane).is_some_and(|p| p.exit.is_none())
        };
        if !running || !self.w.known_panes.contains(&pane) {
            return Err("no program runs in that pane".into());
        }
        self.w.conn.input(pane, text.as_bytes().to_vec());
        Ok(())
    }

    fn open(&mut self, target: &str, how: Option<How>, _reveal: Reveal) -> Result<(), String> {
        self.w.builtin_open(target, how, "plugin");
        Ok(())
    }

    fn new_block(&mut self, kind: &str, args: &[String], how: How, reveal: Reveal) -> Option<u64> {
        let cwd = self.w.focused_cwd();
        self.w.open_block(kind, args, how, reveal, cwd).ok()
    }

    fn command(&mut self, id: &str) -> Result<(), String> {
        let cmd = commands::by_builtin_id(id).ok_or_else(|| format!("unknown command `{id}`"))?;
        let window = self.w.window.clone().ok_or("no window")?;
        self.w.run_builtin(&window, cmd);
        Ok(())
    }

    fn action(&mut self, name: &str) -> Result<(), String> {
        if chord::plugin_action(name).is_some() {
            // The runtime is out of the window while this handler runs: the action runs after.
            self.w.plugins.queued.push(name.to_owned());
            return Ok(());
        }
        self.command(name)
    }

    fn toast(&mut self, level: ToastLevel, text: &str, sub: Option<&str>) {
        self.w.toast(level, text, sub);
    }

    fn copy(&mut self, text: &str) {
        crate::app::clipboard_set(text);
    }

    fn panes(&self) -> Vec<WindowPane> {
        let sessions = self.w.sessions();
        let mut out = Vec::new();
        for s in &sessions.sessions {
            for t in &s.tabs {
                for p in t.root.panes() {
                    out.push(self.w.window_pane(p, Some(t.id)));
                }
            }
        }
        out
    }

    fn tabs(&self) -> Vec<TabInfo> {
        self.w.tab_infos()
    }

    fn focused(&self) -> Option<u64> {
        self.w.focused_pane()
    }

    fn resolve(&self, spec: &str) -> Option<u64> {
        if spec == "@focused" {
            return self.w.focused_pane();
        }
        let panes = self.panes();
        if let Ok(id) = spec.parse::<u64>() {
            return panes.iter().any(|p| p.pane == id).then_some(id);
        }
        panes
            .iter()
            .find(|p| p.title == spec)
            .or_else(|| panes.iter().find(|p| p.title.eq_ignore_ascii_case(spec)))
            .or_else(|| panes.iter().find(|p| p.program == spec))
            .map(|p| p.pane)
    }

    fn sessions(&self) -> Value {
        let sessions = self.w.sessions();
        json!(
            sessions
                .sessions
                .iter()
                .map(|s| json!({
                    "id": s.id,
                    "name": s.name,
                    "current": Some(s.id) == self.w.active_session,
                    "locked": s.locked,
                    "tabs": s.tabs.iter().map(|t| t.id).collect::<Vec<_>>(),
                }))
                .collect::<Vec<_>>()
        )
    }

    fn layout(&self, tab: u64) -> Option<Value> {
        let sessions = self.w.sessions();
        let t = sessions
            .sessions
            .iter()
            .flat_map(|s| s.tabs.iter())
            .find(|t| t.id == tab)?;
        fn tree(n: &Node) -> Value {
            match n {
                Node::Pane { pane } => json!({"pane": pane}),
                Node::Split {
                    dir,
                    ratio,
                    first,
                    second,
                } => {
                    let d = match dir {
                        SplitDir::Right => "right",
                        SplitDir::Down => "down",
                    };
                    json!({
                        "split": d,
                        "ratio": f64::from(*ratio) / 1000.0,
                        "1": tree(first),
                        "2": tree(second),
                    })
                }
            }
        }
        let focus = self
            .w
            .tab_focus
            .get(&tab)
            .copied()
            .or_else(|| t.root.panes().first().copied());
        Some(json!({
            "tab": tab,
            "root": tree(&t.root),
            "floats": [],
            "zoomed": t.zoom.is_some(),
            "focus": focus,
        }))
    }

    fn read(&self, pane: u64, opts: &Value) -> Option<Value> {
        if !self.w.known_panes.contains(&pane) {
            return None;
        }
        let tab = self
            .w
            .sessions()
            .sessions
            .iter()
            .flat_map(|s| s.tabs.iter())
            .find(|t| t.root.contains(pane))
            .map(|t| t.id);
        let info = self.w.window_pane(pane, tab);
        let max = opts["lines"].as_u64().unwrap_or(60).clamp(1, 2000) as usize;
        let scrollback = opts["scrollback"].as_bool().unwrap_or(true);
        let (text, surface) = {
            let shared = self.w.conn.shared();
            let p = shared.panes.get(&pane)?;
            let text = if scrollback {
                p.replica.text()
            } else {
                p.replica.screen_text()
            };
            let surface = p.replica.tsp().surfaces().iter().any(|s| s.is_live());
            (text, surface)
        };
        let all: Vec<&str> = text.trim_end_matches('\n').lines().collect();
        let start = all.len().saturating_sub(max);
        let lines: Vec<&str> = all[start..].to_vec();
        if opts["as"] == "text" {
            return Some(json!(lines.join("\n")));
        }
        let info = serde_json::to_value(&info).unwrap_or(Value::Null);
        if info["kind"] == "plugin" {
            return Some(
                json!({"info": info, "kind": "plugin", "title": info["title"], "text": lines.join("\n")}),
            );
        }
        Some(
            json!({"info": info, "kind": "terminal", "lines": lines, "more": start, "surface": surface}),
        )
    }

    fn surface(&self, pane: u64, _opts: &Value) -> Option<Value> {
        let shared = self.w.conn.shared();
        let p = shared.panes.get(&pane)?;
        let s = p
            .replica
            .tsp()
            .surfaces()
            .iter()
            .rev()
            .find(|s| s.is_live())?;
        Some(json!({
            "surface": {
                "id": s.id(),
                "title": s.title(),
                "role": s.role(),
                "mode": format!("{:?}", s.mode()).to_lowercase(),
            },
            "main": s.document().root().to_json(),
        }))
    }

    fn event(&mut self, pane: u64, ev: &Value) -> Result<(), String> {
        let sf = {
            let shared = self.w.conn.shared();
            let p = shared
                .panes
                .get(&pane)
                .ok_or("no such pane in this window")?;
            p.replica
                .tsp()
                .surfaces()
                .iter()
                .rev()
                .find(|s| s.is_live())
                .map(|s| s.id().to_owned())
                .ok_or("the pane has no live surface")?
        };
        let mut event = ev.clone();
        if let Some(o) = event.as_object_mut() {
            o.remove("sf");
        }
        self.w
            .conn
            .send(&ClientMsg::ViewEvent { pane, sf, event })
            .map_err(|e| e.to_string())
    }

    fn settle(&mut self, pane: u64, ms: f64, done: Completer) {
        if !self.w.known_panes.contains(&pane) {
            done.resolve(Err("the pane closed".into()));
            return;
        }
        let ms = if ms > 0.0 {
            ms.min(120_000.0)
        } else {
            30_000.0
        };
        let deadline = Instant::now() + Duration::from_secs_f64(ms / 1000.0);
        let finished_before = self.w.plugins.finished.get(&pane).copied().unwrap_or(0);
        if let Some(t) = &self.w.plugins.ticker {
            t.at(deadline);
        }
        self.w.plugins.settles.push(Settle {
            pane,
            deadline,
            finished_before,
            done,
        });
    }

    fn new_tab(&mut self, launch: &Launch, reveal: Reveal) -> Option<u64> {
        match self.w.new_tab_launch(launch, None) {
            Ok(pane) => {
                if reveal.focus {
                    self.w.plugins.focus_when_shown = Some(pane);
                }
                Some(pane)
            }
            Err(e) => {
                eprintln!("auk: new tab: {e}");
                None
            }
        }
    }

    fn split(&mut self, target: u64, dir: Dir, launch: &Launch, reveal: Reveal) -> Option<u64> {
        match self.w.split_launch(target, dir, launch, None, None) {
            Ok(pane) => {
                if reveal.focus {
                    self.w.plugins.focus_when_shown = Some(pane);
                }
                Some(pane)
            }
            Err(e) => {
                eprintln!("auk: split: {e}");
                None
            }
        }
    }

    fn focus(&mut self, pane: u64) {
        self.w.focus_pane(pane);
    }

    fn close(&mut self, pane: u64) {
        self.w.edit(LayoutEdit::Close { pane });
    }

    fn move_pane(&mut self, pane: u64, target: u64, dir: Option<Dir>, reveal: Reveal) -> bool {
        let edit = LayoutEdit::Move {
            pane,
            target,
            side: dir.map(side),
        };
        let moved = match self.w.conn.request(edit) {
            Ok(v) => v == true,
            Err(e) => {
                eprintln!("auk: move: {e}");
                false
            }
        };
        if moved && reveal.focus {
            self.w.focus_pane(pane);
        }
        moved
    }

    fn resize(&mut self, pane: u64, dir: Dir, cells: f64) -> bool {
        let side = side(dir);
        let Some(delta) = self.w.resize_delta(pane, side, cells) else {
            return false;
        };
        if delta == 0 {
            return false;
        }
        match self
            .w
            .conn
            .request(LayoutEdit::Resize { pane, side, delta })
        {
            Ok(v) => v == true,
            Err(e) => {
                eprintln!("auk: resize: {e}");
                false
            }
        }
    }

    fn tab(&mut self, spec: &TabSpec, reveal: Reveal) -> (Option<u64>, Option<String>) {
        let leaf = WinState::first_leaf(spec);
        let cwd = leaf.cwd.clone().or_else(|| spec.cwd.clone());
        let launch = leaf.launch.clone().unwrap_or_default();
        let first = match self.w.new_tab_launch(&launch, cwd.as_deref()) {
            Ok(p) => p,
            Err(e) => return (None, Some(e)),
        };
        let mut problems = Vec::new();
        self.w.fill(spec, first, spec.cwd.as_deref(), &mut problems);
        let Some((_, tab)) = self.w.tab_of_pane(first) else {
            return (None, Some("the new tab closed".into()));
        };
        if let Some(name) = &spec.name {
            self.w.edit(LayoutEdit::RenameTab {
                tab,
                title: Some(name.clone()),
            });
        }
        if reveal.focus {
            self.w.plugins.focus_when_shown = Some(first);
        }
        let problem = (!problems.is_empty()).then(|| problems.join(", "));
        (Some(tab), problem)
    }
}

#[cfg(test)]
mod tests {
    use super::{file_url_path, runs_something};

    #[test]
    fn file_links_are_limited_to_local_paths() {
        assert_eq!(
            file_url_path("file:///tmp/a%20b.txt").as_deref(),
            Some("/tmp/a b.txt")
        );
        assert_eq!(
            file_url_path("FILE://localhost/etc/hosts").as_deref(),
            Some("/etc/hosts")
        );
        assert_eq!(
            file_url_path("file:/usr/share").as_deref(),
            Some("/usr/share")
        );
        // Another machine's file, parts that aren't a path, or no path at all: refused.
        for refused in [
            "file://server/share/x",
            "file:///tmp/x?query=1",
            "file:///tmp/x#frag",
            "file://",
            "file:relative/path",
            "file:///tmp/%00x",
            "https://example.com/",
        ] {
            assert_eq!(file_url_path(refused), None, "{refused}");
        }
    }

    #[test]
    fn executables_and_launching_bundles_run_something() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("auk-runs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("go.sh");
        let text = dir.join("notes.txt");
        std::fs::write(&script, "#!/bin/sh\n").unwrap();
        std::fs::write(&text, "hi").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(runs_something(&script.to_string_lossy()));
        assert!(!runs_something(&text.to_string_lossy()));
        assert!(
            !runs_something(&dir.to_string_lossy()),
            "a directory opens in the Finder"
        );
        assert!(runs_something("/Applications/Nothing.app/"));
        assert!(runs_something("/tmp/x.command"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
