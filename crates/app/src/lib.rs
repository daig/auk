//! The macOS app: `auk` with no subcommand.
//!
//! The app makes sure a daemon runs (spawning one detached, as the CLI does), then opens a window
//! per window key with sessions in the daemon's state (windows restore from daemon state,
//! not AppKit's). Each window is its own client of the daemon: it attaches to its window
//! key, keeps a follower replica per pane, and draws the shown tab's panes with the grid
//! renderer, with the chrome (tab bar, sessions, dividers) drawn by the UI engine in the
//! same frame. Layout edits go to the daemon, and every window showing the key follows.
//!
//! - [`conn`]: the daemon connection and the replicas it feeds.
//! - [`window`]: one window: focus, sessions UI, input routing, scrolling, selection, drawing,
//!   acks.
//! - Panes composite through `auk_views::compose` and `auk_views::PaneSurfaces`: grid rows
//!   and surface blocks, docks, layers and screen surfaces, as the headless window and the web
//!   client do.
//! - [`geometry`]: split trees laid out in a window.
//! - [`chrome`]: the tab bar and dividers as a UI document.
//! - [`commands`]: the command registry the menu bar is generated from.
//! - [`a11y`]: the accessibility tree for AccessKit.
//! - [`control`]: the automation endpoint tests drive the app through.
//! - [`s1`]: the reference recorder (Ghostty), shared by the `keyprobe` example and the app
//!   suite.

pub mod geometry;

#[cfg(target_os = "macos")]
pub mod a11y;
#[cfg(target_os = "macos")]
mod app;
#[cfg(target_os = "macos")]
pub mod chrome;
#[cfg(target_os = "macos")]
pub mod commands;
#[cfg(target_os = "macos")]
pub mod conn;
#[cfg(target_os = "macos")]
pub mod control;
#[cfg(target_os = "macos")]
pub mod gfx;
#[cfg(target_os = "macos")]
pub mod palette;
#[cfg(target_os = "macos")]
pub mod s1;
#[cfg(target_os = "macos")]
pub mod select;
#[cfg(target_os = "macos")]
pub mod settings;
#[cfg(target_os = "macos")]
pub mod window;

use std::path::PathBuf;
use std::process::ExitCode;

/// How the app starts.
#[derive(Clone, Debug)]
pub struct Options {
    /// The daemon socket.
    pub socket: PathBuf,
    /// The daemon's state dir, for spawning one.
    pub state_dir: PathBuf,
    /// The `auk` binary, to spawn the daemon with.
    pub exe: PathBuf,
    /// The automation endpoint to serve, if any.
    pub control: Option<PathBuf>,
    /// The window keys to open. Empty restores every window key with sessions (or opens the
    /// default one).
    pub window_keys: Vec<String>,
    /// Which Option keys act as Alt: `none`, `left`, `right` or `both`.
    pub option_as_alt: OptionAsAltSetting,
    /// Attach every window to this host's daemon over SSH instead.
    pub remote: Option<RemoteHost>,
}

/// A remote host to attach to: `ssh [-F CONFIG] DESTINATION <remote command>`.
#[derive(Clone, Debug, Default)]
pub struct RemoteHost {
    pub destination: String,
    pub ssh_config: Option<PathBuf>,
    /// The command the remote shell runs (default `auk attach --stdio`).
    pub remote_command: Option<String>,
}

/// The Option-as-Alt setting, portable.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OptionAsAltSetting {
    #[default]
    None,
    Left,
    Right,
    Both,
}

impl OptionAsAltSetting {
    pub fn parse(s: &str) -> Option<OptionAsAltSetting> {
        Some(match s {
            "none" => OptionAsAltSetting::None,
            "left" => OptionAsAltSetting::Left,
            "right" => OptionAsAltSetting::Right,
            "both" => OptionAsAltSetting::Both,
            _ => return None,
        })
    }
}

/// Runs the app until it quits.
#[cfg(target_os = "macos")]
pub fn run(opts: Options) -> ExitCode {
    // A remote window's daemon is the host's; `auk attach --stdio` starts it there.
    if opts.remote.is_none()
        && let Err(err) = auk_daemon::spawn::ensure(&opts.exe, &opts.socket, &opts.state_dir)
    {
        eprintln!("auk: starting the daemon: {err}");
        return ExitCode::FAILURE;
    }
    let (first, welcome) = match app::connect(&opts) {
        Ok(opened) => opened,
        Err(err) => {
            eprintln!("auk: connecting to the daemon: {err}");
            return ExitCode::FAILURE;
        }
    };
    let keys = if opts.window_keys.is_empty() {
        let mut keys: Vec<String> = welcome
            .sessions
            .windows
            .iter()
            .filter(|(_, w)| !w.sessions.is_empty())
            .map(|(k, _)| k.clone())
            .collect();
        if keys.is_empty() {
            keys.push(auk_daemon::server::DEFAULT_WINDOW_KEY.to_owned());
        }
        keys
    } else {
        opts.window_keys.clone()
    };
    let gpu = match auk_render::Gpu::new_blocking() {
        Ok(gpu) => std::rc::Rc::new(gpu),
        Err(err) => {
            eprintln!("auk: {err}");
            return ExitCode::FAILURE;
        }
    };
    auk_platform_macos::run(move |ctx| {
        // The user's keybinds take their keys from the menu.
        ctx.set_main_menu(&commands::menu_bar_without(
            &settings::Settings::load().user_chords(),
        ));
        // Menu commands no key window took (the app isn't active): the window used last.
        ctx.set_command_handler(|_, id| match commands::by_id(id) {
            Some(commands::Cmd::NewWindow) => app::new_window(),
            Some(cmd) => app::command_to_current(cmd),
            None => {}
        });
        let clock = gfx::FrameClock::new(&gpu);
        app::install(app::App {
            opts: opts.clone(),
            ctx: *ctx,
            gpu,
            clock,
            windows: Vec::new(),
        });
        let mut first = Some(first);
        for key in &keys {
            let conn = match first.take() {
                Some(c) => Ok(c),
                None => app::connect(&opts).map(|(c, _)| c),
            };
            match conn.and_then(|mut c| c.attach(key).map(|()| c)) {
                Ok(conn) => app::open_window(conn),
                Err(err) => eprintln!("auk: window {key}: {err}"),
            }
        }
        if let Some(path) = &opts.control {
            match control::serve(path) {
                Ok(()) => println!("ready {}", path.display()),
                Err(err) => eprintln!("auk: control endpoint {}: {err}", path.display()),
            }
        }
    });
    ExitCode::SUCCESS
}

/// The app runs only on macOS; elsewhere `auk` is the daemon and the CLI.
#[cfg(not(target_os = "macos"))]
pub fn run(_opts: Options) -> ExitCode {
    eprintln!("auk: the app runs on macOS; use `auk daemon`, `auk headless` or a CLI verb");
    ExitCode::from(2)
}
