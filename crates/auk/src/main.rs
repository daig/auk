//! The `auk` binary (§19, §22): the macOS app, the session daemon and the CLI, as subcommands
//! of one binary. `auk` alone is the app; `auk daemon`, `auk headless` and the CLI verbs are in
//! `auk-cli`.

use clap::Parser;

fn main() -> std::process::ExitCode {
    auk_cli::run(auk_cli::Cli::parse())
}
