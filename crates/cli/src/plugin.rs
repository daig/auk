//! `auk plugin …`: manages the plugins folder through
//! `auk_plugins::manage` and asks a running daemon to reload. It never runs Lua and never starts
//! a daemon.
//!
//! Exit codes: 0; 1 when a verb fails (and `reload` when the new catalog has a Failed plugin or
//! a problem); 2 for usage errors; 124 when the daemon doesn't answer `reload` within 60 s.

use std::io::Write;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use auk_plugins::Catalog;
use auk_plugins::manage::{self, Verb};
use serde_json::{Value, json};

use crate::control::{Control, ControlError};

/// How long `reload` waits for the daemon before exiting 124.
const RELOAD_WAIT: Duration = Duration::from_secs(60);

enum Fail {
    Usage(String),
    Failed(String),
    Timeout,
}

/// Runs `auk plugin ARGS…` against the daemon on `socket`, if one answers.
pub fn run(socket: &Path, args: &[String]) -> ExitCode {
    let mut out = std::io::stdout().lock();
    match run_verb(socket, args, &mut out) {
        Ok(code) => code,
        Err(Fail::Usage(message)) => {
            eprintln!("auk plugin: {message}\n{}", manage::USAGE);
            ExitCode::from(2)
        }
        Err(Fail::Failed(message)) => {
            eprintln!("auk plugin: {message}");
            ExitCode::FAILURE
        }
        Err(Fail::Timeout) => {
            eprintln!("auk plugin: the session daemon did not answer");
            ExitCode::from(crate::EXIT_TIMEOUT)
        }
    }
}

/// The daemon's control endpoint, if a daemon answers. Never starts one.
fn daemon(socket: &Path) -> Option<Control> {
    Control::connect(&auk_daemon::paths::control_socket(socket)).ok()
}

fn reload(control: &mut Control) -> Result<Catalog, Fail> {
    // The daemon answers "did not answer" itself at 60 s; one that says nothing at all gets the
    // same 124 when the 60 s are up.
    let _ = control.set_timeout(Some(RELOAD_WAIT));
    match control.request(&["plugin", "reload"]) {
        Ok(value) => serde_json::from_value(value).map_err(|e| Fail::Failed(e.to_string())),
        Err(ControlError::Io(e))
            if matches!(
                e.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ) =>
        {
            Err(Fail::Timeout)
        }
        Err(ControlError::Refused { message, .. }) if message.contains("did not answer") => {
            Err(Fail::Timeout)
        }
        Err(e) => Err(Fail::Failed(e.to_string())),
    }
}

fn print(out: &mut impl Write, json: bool, value: Value, text: &str) -> Result<(), Fail> {
    let result = if json {
        writeln!(out, "{value:#}")
    } else {
        write!(out, "{text}")
    };
    result.map_err(|e| Fail::Failed(e.to_string()))
}

/// After `install`, `remove`, `link` and `unlink`: reloads a running daemon and reports trouble
/// on stderr; the exit code doesn't change.
fn after_change(socket: &Path) {
    let Some(mut control) = daemon(socket) else {
        eprintln!("no daemon running; changes apply at next start");
        return;
    };
    match reload(&mut control) {
        Ok(catalog) => {
            for line in manage::format_list(&catalog).lines() {
                if line.contains("  failed: ") || line.starts_with("problem ") {
                    eprintln!("{line}");
                }
            }
        }
        Err(Fail::Failed(e) | Fail::Usage(e)) => {
            eprintln!("the daemon did not reload its plugins: {e}")
        }
        Err(Fail::Timeout) => {
            eprintln!("the daemon did not reload its plugins: the session daemon did not answer")
        }
    }
}

fn run_verb(socket: &Path, args: &[String], out: &mut impl Write) -> Result<ExitCode, Fail> {
    let inv = manage::parse_args(args).map_err(Fail::Usage)?;
    let plugins_dir = auk_daemon::plugins::plugins_dir();
    let json = inv.json;
    match inv.verb {
        Verb::Help => {
            writeln!(out, "{}", manage::USAGE).map_err(|e| Fail::Failed(e.to_string()))?;
        }
        Verb::List => {
            let catalog = match daemon(socket) {
                Some(mut control) => {
                    let value = control
                        .request(&["plugin", "list"])
                        .map_err(|e| Fail::Failed(e.to_string()))?;
                    serde_json::from_value(value).map_err(|e| Fail::Failed(e.to_string()))?
                }
                // `plugins` and `plugins_disabled` from the settings file, as the daemon reads
                // them.
                None => {
                    manage::offline_catalog(&plugins_dir, &auk_daemon::settings::read().plugins)
                }
            };
            print(out, json, catalog.to_json(), &manage::format_list(&catalog))?;
        }
        Verb::Reload => {
            let mut control =
                daemon(socket).ok_or_else(|| Fail::Failed("no daemon running".into()))?;
            let catalog = reload(&mut control)?;
            print(out, json, catalog.to_json(), &manage::format_list(&catalog))?;
            if manage::has_trouble(&catalog) {
                return Ok(ExitCode::FAILURE);
            }
        }
        Verb::Dir => {
            let dir = plugins_dir.display().to_string();
            print(out, json, json!({"dir": dir}), &format!("{dir}\n"))?;
        }
        Verb::Install { src, force } => {
            let got = manage::install(&plugins_dir, &src, force).map_err(Fail::Failed)?;
            let text = format!(
                "installed {} {} → {}\n",
                got.id,
                got.version,
                got.dir.display()
            );
            print(out, json, got.to_json(), &text)?;
            after_change(socket);
        }
        Verb::Remove(id) => {
            let dir = manage::remove(&plugins_dir, &id).map_err(Fail::Failed)?;
            let text = format!("removed {id} ({})\n", dir.display());
            print(out, json, json!({"removed": id}), &text)?;
            after_change(socket);
        }
        Verb::Link(dir) => {
            let got = manage::link(&plugins_dir, &dir).map_err(Fail::Failed)?;
            let text = format!(
                "linked {} {} → {}\n",
                got.id,
                got.version,
                got.dir.display()
            );
            print(out, json, got.to_json(), &text)?;
            after_change(socket);
        }
        Verb::Unlink(id) => {
            let file = manage::unlink(&plugins_dir, &id).map_err(Fail::Failed)?;
            let text = format!("unlinked {id} ({})\n", file.display());
            print(out, json, json!({"removed": id}), &text)?;
            after_change(socket);
        }
        Verb::Types(dir) => {
            let file = manage::types(Path::new(&dir)).map_err(Fail::Failed)?;
            let file = file.display().to_string();
            print(out, json, json!({"file": file}), &format!("wrote {file}\n"))?;
        }
    }
    Ok(ExitCode::SUCCESS)
}
