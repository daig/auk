//! The `auk` command line (§11.1): the CLI verbs, which talk to the daemon's control endpoint,
//! plus the `daemon` and `headless` subcommands. `auk` with no subcommand is the macOS app.

pub mod control;
pub mod headless;

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

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the session daemon in the foreground.
    Daemon,
    /// Run a headless window: attach, keep replicas, serve a control endpoint.
    Headless {
        /// The headless window's control endpoint (default: headless-<pid>.ctl beside the
        /// daemon socket).
        #[arg(long)]
        control: Option<PathBuf>,
        /// The window key whose sessions to show.
        #[arg(long, default_value = auk_daemon::server::DEFAULT_WINDOW_KEY)]
        window: String,
        /// The size to ask for each pane, as COLSxROWS.
        #[arg(long, value_parser = parse_size)]
        size: Option<(u16, u16)>,
    },
    /// List sessions, tabs and panes.
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// Open a session or a tab; prints the new pane's id. Starts the daemon if none answers.
    New {
        /// `session` or `tab`.
        #[arg(default_value = "session", value_parser = ["session", "tab"])]
        what: String,
        #[arg(long)]
        window: Option<String>,
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
    /// Print a pane's text.
    Capture {
        target: String,
        /// Include the scrollback.
        #[arg(long)]
        scrollback: bool,
        #[arg(long)]
        json: bool,
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
    match command {
        Command::Daemon => {
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
            window,
            size,
        } => {
            let opts = headless::Options {
                control: control.unwrap_or_else(|| headless::default_control(&socket)),
                socket,
                window_key: window,
                size,
                exe: current_exe(),
                state_dir: paths::state_dir(),
            };
            match headless::run(&opts) {
                Ok(()) => ExitCode::SUCCESS,
                Err(err) => fail(format!("auk headless: {err}")),
            }
        }
        verb => match run_verb(&socket, verb) {
            Ok(code) => code,
            Err(VerbError::Timeout) => ExitCode::from(EXIT_TIMEOUT),
            Err(VerbError::Failed(message)) => fail(format!("auk: {message}")),
        },
    }
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

fn run_verb(socket: &Path, verb: Command) -> Result<ExitCode, VerbError> {
    let mut out = io::stdout().lock();
    match verb {
        Command::Ls { json } => {
            let value = connect(socket, false)?.request(&["ls"])?;
            if json {
                writeln!(out, "{value:#}")?;
            } else {
                print_ls(&mut out, &value)?;
            }
        }
        Command::New {
            what,
            window,
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
            flag("window", window);
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
            writeln!(out, "{}", value["pane"])?;
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
            scrollback,
            json,
        } => {
            let mut words = vec!["capture".to_owned(), target];
            if scrollback {
                words.push("--scrollback".to_owned());
            }
            let value = connect(socket, false)?.request(&words)?;
            if json {
                writeln!(out, "{value}")?;
            } else {
                writeln!(out, "{}", value["text"].as_str().unwrap_or_default())?;
            }
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
            connect(socket, false)?.request(&words)?;
        }
        Command::Daemon | Command::Headless { .. } => unreachable!("handled by `run`"),
    }
    Ok(ExitCode::SUCCESS)
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
        let cli = Cli::parse_from(["auk", "run", "1", "echo", "-n", "hi"]);
        let Some(Command::Run { command, .. }) = cli.command else {
            panic!()
        };
        assert_eq!(command, ["echo", "-n", "hi"]);
        assert!(parse_size("0x3").is_err());
    }
}
