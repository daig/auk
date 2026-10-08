//! The `auk` binary (§19, §22): the macOS app, the session daemon and the CLI, as subcommands
//! of one binary. `auk` alone is the app (`auk-app`); `auk daemon`, `auk headless` and the CLI
//! verbs are in `auk-cli`.

use std::path::PathBuf;

use clap::Parser;

/// The command line: the CLI's, plus the app's own flags.
#[derive(Debug, Parser)]
#[command(
    name = "auk",
    version,
    about = "Auk, a terminal whose sessions outlive their windows"
)]
struct Top {
    #[command(flatten)]
    cli: auk_cli::Cli,

    /// App: serve the automation endpoint on this socket (also $AUK_APP_CONTROL).
    #[arg(long)]
    control: Option<PathBuf>,

    /// App: open this window key's sessions (repeatable). Default: every window key with
    /// sessions in the daemon, else the default one.
    #[arg(long = "window-key")]
    window_keys: Vec<String>,

    /// App: which Option keys act as Alt (also $AUK_OPTION_AS_ALT).
    #[arg(long, value_parser = ["none", "left", "right", "both"])]
    option_as_alt: Option<String>,
}

fn main() -> std::process::ExitCode {
    // `auk web serve` (§10.3) parses its own arguments.
    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    if args.get(1).is_some_and(|a| a == "web") {
        return auk_web::serve::main(args);
    }
    let top = Top::parse();
    if top.cli.command.is_some() {
        return auk_cli::run(top.cli);
    }
    let env = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
    let option_as_alt = top
        .option_as_alt
        .or_else(|| env("AUK_OPTION_AS_ALT").map(|v| v.to_string_lossy().into_owned()))
        .and_then(|s| auk_app::OptionAsAltSetting::parse(&s))
        .unwrap_or_default();
    let opts = auk_app::Options {
        socket: top
            .cli
            .socket
            .unwrap_or_else(auk_daemon::paths::daemon_socket),
        state_dir: auk_daemon::paths::state_dir(),
        exe: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("auk")),
        control: top.control.or_else(|| env("AUK_APP_CONTROL").map(PathBuf::from)),
        window_keys: top.window_keys,
        option_as_alt,
    };
    auk_app::run(opts)
}
