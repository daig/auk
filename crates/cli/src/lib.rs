//! The `auk` command line (§11.1): the CLI verbs, which talk to the daemon's control endpoint,
//! plus the `daemon` and `headless` subcommands. `auk` with no subcommand is the macOS app.

pub mod control;
pub mod headless;
pub mod plugin;
pub mod scenario;

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use auk_daemon::paths;
use clap::{Parser, Subcommand};
use serde_json::Value;

use crate::control::{Control, ControlError};

#[derive(Debug, Parser)]
#[command(
    name = "auk",
    version,
    about = "Auk, a terminal whose sessions outlive their windows"
)]
pub struct Cli {
    /// The daemon socket (default: $AUK_DAEMON_SOCKET, else daemon.sock in the runtime dir).
    #[arg(long, global = true)]
    pub socket: Option<PathBuf>,

    /// The window key a verb acts in (§11.1): where `new` opens panes, which windows `open`
    /// asks. Default: `main`.
    #[arg(long, global = true, value_name = "KEY")]
    pub window: Option<String>,

    /// Print the verb's result as JSON.
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the session daemon in the foreground; `auk daemon upgrade` hands the running one to
    /// a new build in place (§15.7).
    Daemon {
        #[command(subcommand)]
        action: Option<DaemonAction>,
    },
    /// Run a headless window: attach, keep replicas, serve a control endpoint.
    Headless {
        /// The headless window's control endpoint (default: headless-<pid>.ctl beside the
        /// daemon socket).
        #[arg(long)]
        control: Option<PathBuf>,
        /// The size to ask for each pane, as COLSxROWS.
        #[arg(long, value_parser = parse_size)]
        size: Option<(u16, u16)>,
        /// Attach to this host's daemon over SSH (`ssh HOST auk attach --stdio`, §10.2).
        #[arg(long)]
        host: Option<String>,
        /// With --host: the SSH config file (`ssh -F`).
        #[arg(long, requires = "host")]
        ssh_config: Option<PathBuf>,
        /// With --host: the command the remote shell runs (default: `auk attach --stdio`).
        #[arg(long, requires = "host")]
        remote_command: Option<String>,
    },
    /// The remote end of an SSH link: bridge stdio to this host's daemon, starting it if none
    /// answers (§10.2). Run by `ssh HOST auk attach --stdio`.
    Attach {
        /// Speak the replication protocol on stdin and stdout (the only mode).
        #[arg(long, required = true)]
        stdio: bool,
    },
    /// List sessions, tabs and panes.
    Ls,
    /// Open a session or a tab; prints the new pane's id. Starts the daemon if none answers.
    New {
        /// `session` or `tab`.
        #[arg(default_value = "session", value_parser = ["session", "tab"])]
        what: String,
        /// The session for a new tab (default: the window's last session).
        #[arg(long)]
        session: Option<u64>,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// COLSxROWS.
        #[arg(long, value_parser = parse_size)]
        size: Option<(u16, u16)>,
        /// A command to run instead of a login shell.
        #[arg(last = true)]
        command: Vec<String>,
    },
    /// Split a pane: the new pane goes right of it or below it; prints the new pane's id.
    Split {
        target: String,
        #[arg(value_parser = ["right", "down"])]
        dir: String,
        /// The new pane's directory (default: the split pane's).
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// A command to run instead of a login shell.
        #[arg(last = true)]
        command: Vec<String>,
    },
    /// Close a pane: hang up its process and take it out of the layout.
    Close { target: String },
    /// Stream daemon events as JSON lines, such as
    /// `{"event":"pane_spawned","pane":3,"window":"main","by":"cli"}`.
    Events {
        /// Only these kinds, comma-separated (`pane_spawned,layout_changed`).
        #[arg(long, value_delimiter = ',')]
        filter: Vec<String>,
    },
    /// Open a file: a connected window showing the window key routes it (its `route.open`
    /// plugins, then its opener); with no window, the system opener ($AUK_OPENER, else `open`).
    Open { path: PathBuf },
    /// Run scenario scripts offscreen in a private daemon (§11.3); writes a PNG and a layout
    /// JSON for every `shot` command into OUT/<scenario>/.
    Shot {
        #[arg(required = true)]
        scenarios: Vec<PathBuf>,
        /// Where shots go (default: target/shots/auk).
        #[arg(long)]
        out: Option<PathBuf>,
        /// The window size in points, with an optional scale: WxH[@S] (default 960x600@1).
        #[arg(long)]
        size: Option<String>,
        /// light or dark (default: dark).
        #[arg(long, value_parser = ["light", "dark"])]
        appearance: Option<String>,
    },
    /// A deterministic headless window on a private daemon, serving a control endpoint until
    /// `quit` (§11.3).
    Serve {
        /// The control endpoint (default: serve.ctl in the runtime dir).
        #[arg(long)]
        control: Option<PathBuf>,
        /// Where `shot` writes (default: target/shots/auk).
        #[arg(long)]
        out: Option<PathBuf>,
        /// WxH[@S] (default 960x600@1).
        #[arg(long)]
        size: Option<String>,
        #[arg(long, value_parser = ["light", "dark"])]
        appearance: Option<String>,
    },
    /// Send commands to a headless window's control endpoint: one command (its words), or a
    /// scenario script's commands one by one (`--file`). Prints each JSON reply.
    Ctl {
        /// The control endpoint (default: serve.ctl in the runtime dir).
        #[arg(long)]
        control: Option<PathBuf>,
        /// A scenario script.
        #[arg(long, conflicts_with = "command")]
        file: Option<PathBuf>,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Type a command line and Enter into a pane; doesn't wait.
    Run {
        target: String,
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Send input: `text WORDS…`, `paste WORDS…` or `keys KEY…` (Enter, C-c, Up, F5, …).
    Send {
        target: String,
        #[arg(value_parser = ["text", "paste", "keys"])]
        mode: String,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        input: Vec<String>,
    },
    /// Print a pane's content: text, or with --ansi its attributes as SGR, or with --html as
    /// HTML; with --surfaces, surfaces' text in place of their anchor rows.
    Capture {
        target: String,
        #[arg(long, conflicts_with = "html")]
        ansi: bool,
        #[arg(long)]
        html: bool,
        /// Include the scrollback.
        #[arg(long)]
        scrollback: bool,
        /// Include surfaces' text.
        #[arg(long)]
        surfaces: bool,
    },
    /// Play a TSP recording into a pane (§7.15): `surface-play FILE [from N] [until N]
    /// [paced [TIMES]]`. The first play restarts the pane as a stand-in program.
    SurfacePlay {
        file: PathBuf,
        /// The pane (default: the focused one).
        #[arg(long, default_value = "@focused")]
        pane: String,
        /// `from N`, `until N`, `paced [TIMES]`.
        #[arg(trailing_var_arg = true)]
        options: Vec<String>,
    },
    /// The stand-in program `surface-play` restarts a pane as: raw mode, reads and drops input.
    #[command(hide = true)]
    StandIn,
    /// Manage plugins: `list|reload|dir|install SRC [--force]|remove ID|link DIR|unlink ID|types
    /// DIR [--json] [--window KEY]` (§9.11). Never starts a daemon.
    #[command(disable_help_flag = true)]
    Plugin {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Open a plugin block pane (§9.5): `block KIND [ARG…]`, KIND being `<plugin>.<block>`.
    /// Prints the new pane's id.
    Block {
        kind: String,
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// COLSxROWS.
        #[arg(long, value_parser = parse_size)]
        size: Option<(u16, u16)>,
        /// Launch arguments.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Wait for a pane: `prompt`, `exit` or `idle=MS`. Exits 124 on timeout.
    Wait {
        target: String,
        #[arg(long)]
        until: String,
        /// Seconds.
        #[arg(long)]
        timeout: Option<f64>,
    },
}

fn parse_size(s: &str) -> Result<(u16, u16), String> {
    s.split_once('x')
        .and_then(|(c, r)| Some((c.parse().ok()?, r.parse().ok()?)))
        .filter(|&(c, r): &(u16, u16)| c > 0 && r > 0)
        .ok_or_else(|| format!("expected COLSxROWS, got `{s}`"))
}

/// The exit status `wait` uses for a timeout (§11.1).
pub const EXIT_TIMEOUT: u8 = 124;

/// Runs the command line.
pub fn run(cli: Cli) -> ExitCode {
    let socket = cli.socket.unwrap_or_else(paths::daemon_socket);
    let Some(command) = cli.command else {
        eprintln!(
            "auk: the macOS app isn't part of this build yet. Use `auk daemon`, `auk headless` \
             or a CLI verb; see `auk --help`."
        );
        return ExitCode::from(2);
    };
    let globals = Globals {
        window: cli.window,
        json: cli.json,
    };
    match command {
        Command::Daemon {
            action: Some(DaemonAction::Upgrade { exe }),
        } => match upgrade_daemon(&socket, exe) {
            Ok(()) => ExitCode::SUCCESS,
            Err(VerbError::Timeout) => fail("auk daemon upgrade: timed out".into()),
            Err(VerbError::Failed(message)) => fail(format!("auk daemon upgrade: {message}")),
        },
        Command::Daemon { action: None } => {
            let config = auk_daemon::Config {
                socket,
                state_dir: paths::state_dir(),
            };
            match auk_daemon::run(config) {
                Ok(()) => ExitCode::SUCCESS,
                Err(err) => fail(format!("auk daemon: {err}")),
            }
        }
        Command::Headless {
            control,
            size,
            host,
            ssh_config,
            remote_command,
        } => {
            let remote = host.map(|host| {
                let mut target = auk_remote::SshTarget::new(host);
                target.config = ssh_config;
                if let Some(command) = remote_command {
                    target.remote_command = command;
                }
                target
            });
            let opts = headless::Options {
                control: control.unwrap_or_else(|| headless::default_control(&socket)),
                socket,
                window_key: globals
                    .window
                    .unwrap_or_else(|| auk_daemon::server::DEFAULT_WINDOW_KEY.to_owned()),
                size,
                exe: current_exe(),
                state_dir: paths::state_dir(),
                remote,
                scenario: None,
            };
            match headless::run(&opts) {
                Ok(()) => ExitCode::SUCCESS,
                Err(err) => fail(format!("auk headless: {err}")),
            }
        }
        Command::Attach { .. } => {
            match auk_remote::bridge::run_stdio(&current_exe(), &socket, &paths::state_dir()) {
                Ok(()) => ExitCode::SUCCESS,
                Err(err) => fail(format!("auk attach: {err}")),
            }
        }
        Command::Plugin { mut args } => {
            if globals.json && !args.iter().any(|a| a == "--json") {
                args.push("--json".to_owned());
            }
            if let Some(window) = globals.window
                && !args.iter().any(|a| a == "--window")
            {
                args.extend(["--window".to_owned(), window]);
            }
            plugin::run(&socket, &args)
        }
        Command::StandIn => match auk_daemon::play::stand_in() {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => fail(format!("auk stand-in: {err}")),
        },
        Command::Shot {
            scenarios,
            out,
            size,
            appearance,
        } => scenario::shot(&scenario::ShotOptions {
            exe: current_exe(),
            scenarios,
            out,
            size,
            appearance,
            json: globals.json,
        }),
        Command::Serve {
            control,
            out,
            size,
            appearance,
        } => scenario::serve(&scenario::ServeOptions {
            exe: current_exe(),
            control: control.unwrap_or_else(scenario::default_control),
            out,
            size,
            appearance,
        }),
        Command::Ctl {
            control,
            file,
            command,
        } => scenario::ctl(
            &control.unwrap_or_else(scenario::default_control),
            file.as_deref(),
            &command,
        ),
        verb => match run_verb(&socket, &globals, verb) {
            Ok(code) => code,
            Err(VerbError::Timeout) => ExitCode::from(EXIT_TIMEOUT),
            Err(VerbError::Failed(message)) => fail(format!("auk: {message}")),
        },
    }
}

/// Flags every verb takes.
#[derive(Debug, Default)]
struct Globals {
    window: Option<String>,
    json: bool,
}

/// What `auk daemon` can do besides running one.
#[derive(Debug, Subcommand)]
pub enum DaemonAction {
    /// Hand the running daemon to a new build without ending any program (§15.7): the daemon
    /// execs it, passing its sockets, PTYs and state; windows reconnect.
    Upgrade {
        /// The new build (default: this `auk`).
        #[arg(long)]
        exe: Option<PathBuf>,
    },
}

/// `auk daemon upgrade`: asks the daemon to exec the new build, then waits until the new one
/// answers and reports it.
fn upgrade_daemon(socket: &Path, exe: Option<PathBuf>) -> Result<(), VerbError> {
    let exe = absolute(&exe.unwrap_or_else(current_exe));
    let words = [
        "upgrade".to_owned(),
        "--exe".to_owned(),
        exe.to_string_lossy().into_owned(),
    ];
    let started = connect(socket, false)?.request(&words)?;
    let before = started["upgrades"].as_u64().unwrap_or(0);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let state = connect(socket, false).and_then(|mut c| Ok(c.request(&["state"])?));
        if let Ok(state) = state
            && state["upgrades"].as_u64().unwrap_or(0) > before
        {
            println!(
                "upgraded the daemon (pid {}) from Auk {} to {}",
                state["pid"],
                started["from"].as_str().unwrap_or("?"),
                state["auk"].as_str().unwrap_or("?")
            );
            return Ok(());
        }
    }
    Err(VerbError::Failed(format!(
        "the daemon didn't come back upgraded; see {}",
        paths::state_dir().join("daemon.log").display()
    )))
}

fn current_exe() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("auk"))
}

fn fail(message: String) -> ExitCode {
    eprintln!("{message}");
    ExitCode::FAILURE
}

#[derive(Debug)]
enum VerbError {
    Timeout,
    Failed(String),
}

impl From<ControlError> for VerbError {
    fn from(err: ControlError) -> Self {
        match err {
            ControlError::Refused { timeout: true, .. } => VerbError::Timeout,
            other => VerbError::Failed(other.to_string()),
        }
    }
}

impl From<io::Error> for VerbError {
    fn from(err: io::Error) -> Self {
        VerbError::Failed(err.to_string())
    }
}

fn connect(socket: &Path, start: bool) -> Result<Control, VerbError> {
    if start {
        auk_daemon::spawn::ensure(&current_exe(), socket, &paths::state_dir())?;
    }
    let path = paths::control_socket(socket);
    Control::connect(&path).map_err(|err| {
        VerbError::Failed(format!("no daemon answers on {} ({err})", path.display()))
    })
}

/// Prints a verb's result: the JSON value with `--json`, else `text` (if any).
fn report(
    out: &mut impl Write,
    g: &Globals,
    value: &Value,
    text: Option<String>,
) -> io::Result<()> {
    if g.json {
        writeln!(out, "{value}")
    } else if let Some(text) = text {
        writeln!(out, "{text}")
    } else {
        Ok(())
    }
}

fn run_verb(socket: &Path, g: &Globals, verb: Command) -> Result<ExitCode, VerbError> {
    let mut out = io::stdout().lock();
    match verb {
        Command::Ls => {
            let value = connect(socket, false)?.request(&["ls"])?;
            let value = match &g.window {
                Some(key) => filter_window(value, key),
                None => value,
            };
            if g.json {
                writeln!(out, "{value:#}")?;
            } else {
                print_ls(&mut out, &value)?;
            }
        }
        Command::New {
            what,
            session,
            name,
            cwd,
            size,
            command,
        } => {
            let mut words = vec!["new".to_owned(), what];
            let mut flag = |name: &str, value: Option<String>| {
                if let Some(value) = value {
                    words.push(format!("--{name}"));
                    words.push(value);
                }
            };
            flag("window", g.window.clone());
            flag("session", session.map(|s| s.to_string()));
            flag("name", name);
            flag(
                "cwd",
                cwd.map(|c| absolute(&c).to_string_lossy().into_owned()),
            );
            flag("size", size.map(|(c, r)| format!("{c}x{r}")));
            if !command.is_empty() {
                words.push("--".to_owned());
                words.extend(command);
            }
            let value = connect(socket, true)?.request(&words)?;
            report(&mut out, g, &value, Some(value["pane"].to_string()))?;
        }
        Command::Split {
            target,
            dir,
            cwd,
            command,
        } => {
            let mut words = vec!["split".to_owned(), target, dir];
            if let Some(cwd) = cwd {
                words.extend([
                    "--cwd".to_owned(),
                    absolute(&cwd).to_string_lossy().into_owned(),
                ]);
            }
            if !command.is_empty() {
                words.push("--".to_owned());
                words.extend(command);
            }
            let value = connect(socket, false)?.request(&words)?;
            report(&mut out, g, &value, Some(value["pane"].to_string()))?;
        }
        Command::Close { target } => {
            connect(socket, false)?.request(&["close", &target])?;
            report(&mut out, g, &serde_json::json!({"closed": target}), None)?;
        }
        Command::Events { filter } => {
            let mut control = connect(socket, false)?;
            let mut words = vec!["events".to_owned()];
            if !filter.is_empty() {
                words.extend(["--filter".to_owned(), filter.join(",")]);
            }
            control.request(&words)?;
            // Events go out as they come, until the daemon stops or the reader goes away.
            while let Some(line) = control.read_line()? {
                if writeln!(out, "{}", line.trim_end())
                    .and_then(|()| out.flush())
                    .is_err()
                {
                    break;
                }
            }
        }
        Command::Open { path } => {
            let path = absolute(&path).to_string_lossy().into_owned();
            let mut words = vec!["open".to_owned(), path.clone()];
            if let Some(window) = &g.window {
                words.extend(["--window".to_owned(), window.clone()]);
            }
            // Routes first: a window showing the key takes it; else the system opener.
            let routed = match connect(socket, false) {
                Ok(mut control) => control.request(&words)?["delivered"].as_u64().unwrap_or(0),
                Err(_) => 0,
            };
            let value = if routed > 0 {
                serde_json::json!({"path": path, "routed": "window", "windows": routed})
            } else {
                let opener = system_opener();
                let status = std::process::Command::new(&opener)
                    .arg(&path)
                    .status()
                    .map_err(|e| VerbError::Failed(format!("{}: {e}", opener.display())))?;
                if !status.success() {
                    return Err(VerbError::Failed(format!(
                        "{} {path}: {status}",
                        opener.display()
                    )));
                }
                serde_json::json!({"path": path, "routed": "system", "opener": opener})
            };
            report(&mut out, g, &value, None)?;
        }
        Command::Run { target, command } => {
            let mut words = vec!["run".to_owned(), target];
            words.extend(command);
            connect(socket, false)?.request(&words)?;
        }
        Command::Send {
            target,
            mode,
            input,
        } => {
            let mut words = vec!["send".to_owned(), target, mode];
            words.extend(input);
            connect(socket, false)?.request(&words)?;
        }
        Command::Capture {
            target,
            ansi,
            html,
            scrollback,
            surfaces,
        } => {
            let mut words = vec!["capture".to_owned(), target];
            for (on, flag) in [
                (ansi, "--ansi"),
                (html, "--html"),
                (scrollback, "--scrollback"),
                (surfaces, "--surfaces"),
            ] {
                if on {
                    words.push(flag.to_owned());
                }
            }
            let value = connect(socket, false)?.request(&words)?;
            let text = value["text"].as_str().unwrap_or_default().to_owned();
            report(&mut out, g, &value, Some(text))?;
        }
        Command::Wait {
            target,
            until,
            timeout,
        } => {
            let mut words = vec!["wait".to_owned(), target, "--until".to_owned(), until];
            if let Some(t) = timeout {
                words.push("--timeout".to_owned());
                words.push(t.to_string());
            }
            let value = connect(socket, false)?.request(&words)?;
            report(&mut out, g, &value, None)?;
        }
        Command::Block {
            kind,
            cwd,
            size,
            args,
        } => {
            let mut words = vec!["block".to_owned(), kind];
            if let Some(window) = &g.window {
                words.extend(["--window".to_owned(), window.clone()]);
            }
            if let Some(cwd) = cwd {
                words.extend([
                    "--cwd".to_owned(),
                    absolute(&cwd).to_string_lossy().into_owned(),
                ]);
            }
            if let Some((c, r)) = size {
                words.extend(["--size".to_owned(), format!("{c}x{r}")]);
            }
            if !args.is_empty() {
                words.push("--".to_owned());
                words.extend(args);
            }
            let value = connect(socket, true)?.request(&words)?;
            report(&mut out, g, &value, Some(value["pane"].to_string()))?;
        }
        Command::SurfacePlay {
            file,
            pane,
            options,
        } => {
            let mut words = vec![
                "surface-play".to_owned(),
                "--pane".to_owned(),
                pane,
                absolute(&file).to_string_lossy().into_owned(),
            ];
            words.extend(options);
            let value = connect(socket, false)?.request(&words)?;
            writeln!(out, "{value}")?;
        }
        Command::Daemon { .. }
        | Command::Headless { .. }
        | Command::Attach { .. }
        | Command::StandIn
        | Command::Shot { .. }
        | Command::Serve { .. }
        | Command::Ctl { .. }
        | Command::Plugin { .. } => {
            unreachable!("handled by `run`")
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// The system opener: `$AUK_OPENER`, else `open` (macOS) or `xdg-open`.
fn system_opener() -> PathBuf {
    match std::env::var_os("AUK_OPENER").filter(|v| !v.is_empty()) {
        Some(o) => PathBuf::from(o),
        None if cfg!(target_os = "macos") => PathBuf::from("/usr/bin/open"),
        None => PathBuf::from("xdg-open"),
    }
}

/// `ls --window KEY`: only that window key's panes and layout.
fn filter_window(mut value: Value, key: &str) -> Value {
    if let Some(panes) = value["panes"].as_array_mut() {
        panes.retain(|p| p["window"] == key);
    }
    if let Some(layout) = value["layout"].as_object_mut() {
        layout.retain(|k, _| k == key);
    }
    value
}

fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_owned())
}

fn print_ls(out: &mut impl Write, value: &Value) -> io::Result<()> {
    writeln!(
        out,
        "{:>4}  {:<8}  {:>7}  {:>7}  {:<8}  {:<20}  CWD",
        "PANE", "WINDOW", "SESSION", "SIZE", "STATE", "TITLE"
    )?;
    for p in value["panes"].as_array().into_iter().flatten() {
        let state = match (&p["exit"], p["running"].as_bool(), p["at_prompt"].as_bool()) {
            (Value::Number(n), _, _) => format!("exit {n}"),
            (_, Some(true), _) => "running".to_owned(),
            (_, _, Some(true)) => "prompt".to_owned(),
            _ => "-".to_owned(),
        };
        let text = |v: &Value| {
            v.as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| v.to_string())
        };
        writeln!(
            out,
            "{:>4}  {:<8}  {:>7}  {:>7}  {:<8}  {:<20}  {}",
            p["id"],
            text(&p["window"]),
            if p["session"].is_null() {
                "-".to_owned()
            } else {
                p["session"].to_string()
            },
            format!("{}x{}", p["cols"], p["rows"]),
            state,
            text(&p["title"]),
            text(&p["cwd"]),
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn the_command_line_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_verbs() {
        let cli = Cli::parse_from([
            "auk",
            "wait",
            "3",
            "--until",
            "idle=200",
            "--timeout",
            "1.5",
        ]);
        assert!(
            matches!(cli.command, Some(Command::Wait { ref until, timeout: Some(t), .. }) if until == "idle=200" && t == 1.5)
        );
        let cli = Cli::parse_from([
            "auk", "--socket", "/x.sock", "new", "tab", "--size", "90x20", "--", "ls", "-l",
        ]);
        assert_eq!(cli.socket.as_deref(), Some(Path::new("/x.sock")));
        let Some(Command::New {
            what,
            size,
            command,
            ..
        }) = cli.command
        else {
            panic!()
        };
        assert_eq!(
            (what.as_str(), size, command),
            ("tab", Some((90, 20)), vec!["ls".into(), "-l".into()])
        );
        let cli = Cli::parse_from([
            "auk", "--json", "split", "@focused", "down", "--window", "w", "--", "htop",
        ]);
        assert!(cli.json);
        assert_eq!(cli.window.as_deref(), Some("w"));
        assert!(
            matches!(cli.command, Some(Command::Split { ref dir, ref command, .. }) if dir == "down" && command == &["htop"])
        );
        let cli = Cli::parse_from(["auk", "events", "--filter", "pane_spawned,layout_changed"]);
        assert!(matches!(cli.command, Some(Command::Events { ref filter }) if filter.len() == 2));
        let cli = Cli::parse_from(["auk", "run", "1", "echo", "-n", "hi"]);
        let Some(Command::Run { command, .. }) = cli.command else {
            panic!()
        };
        assert_eq!(command, ["echo", "-n", "hi"]);
        assert!(parse_size("0x3").is_err());
    }
}
