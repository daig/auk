//! The `auk` binary. With no subcommand it will run the macOS app; the daemon and the CLI
//! are subcommands of the same binary (spec §19). So far only the control-endpoint skeleton
//! exists.

use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "usage: auk daemon --control <socket>

  daemon --control <socket>   serve the control-endpoint skeleton on a Unix socket

The app, the CLI verbs, `web serve` and `attach --stdio` are not implemented yet.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [cmd, flag, path] if cmd == "daemon" && flag == "--control" => {
            match auk_daemon::control::serve(&PathBuf::from(path)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(err) => {
                    eprintln!("auk daemon: {err}");
                    ExitCode::FAILURE
                }
            }
        }
        [flag] if flag == "-h" || flag == "--help" => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}
