//! Where the daemon lives on disk.
//!
//! | What | Path |
//! | --- | --- |
//! | Daemon socket | `$AUK_DAEMON_SOCKET`, else `<runtime>/daemon.sock` |
//! | Control endpoint | the daemon socket with the extension `ctl` (`daemon.ctl`) |
//! | Single-daemon lock | the daemon socket with the extension `lock` |
//! | Pane sockets | `<socket dir>/panes/<pane>.sock` |
//! | State dir | `$AUK_STATE_DIR`, else `$AUK_CONFIG_DIR`, else the settings dir |
//! | Daemon log | `<state>/daemon.log` |
//! | zsh integration | `<state>/shell/zsh` |
//! | Blob cache | `<state>/blobs` |
//! | State file | `<state>/daemon.state` |
//!
//! The runtime dir is `$XDG_RUNTIME_DIR/auk` when that is set. Otherwise it is the settings dir:
//! on macOS `~/Library/Application Support/Auk` (stable across reboots, unlike `$TMPDIR`, whose
//! files the system deletes after a few idle days, and short enough for a socket path); on
//! Linux `$XDG_STATE_HOME/auk`, else `~/.local/state/auk`.

use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Names the daemon socket. A different socket means a separate daemon.
pub const DAEMON_SOCKET_ENV: &str = "AUK_DAEMON_SOCKET";
/// Names the settings directory (`TERN_CONFIG_DIR`).
pub const CONFIG_DIR_ENV: &str = "AUK_CONFIG_DIR";
/// Names the state directory; tests point it at a temporary directory.
pub const STATE_DIR_ENV: &str = "AUK_STATE_DIR";

fn env_path(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn home() -> PathBuf {
    env_path("HOME").unwrap_or_else(|| crate::login::passwd_home().unwrap_or_else(|| "/".into()))
}

/// The settings directory.
pub fn settings_dir() -> PathBuf {
    if let Some(dir) = env_path(CONFIG_DIR_ENV) {
        return dir;
    }
    if cfg!(target_os = "macos") {
        home().join("Library/Application Support/Auk")
    } else if let Some(dir) = env_path("XDG_STATE_HOME") {
        dir.join("auk")
    } else {
        home().join(".local/state/auk")
    }
}

/// The state directory.
pub fn state_dir() -> PathBuf {
    env_path(STATE_DIR_ENV).unwrap_or_else(settings_dir)
}

/// The directory for sockets.
pub fn runtime_dir() -> PathBuf {
    match env_path("XDG_RUNTIME_DIR") {
        Some(dir) => dir.join("auk"),
        None => settings_dir(),
    }
}

/// The daemon socket: `$AUK_DAEMON_SOCKET`, else `daemon.sock` in the runtime dir.
pub fn daemon_socket() -> PathBuf {
    env_path(DAEMON_SOCKET_ENV).unwrap_or_else(|| runtime_dir().join("daemon.sock"))
}

fn with_extension(socket: &Path, ext: &str) -> PathBuf {
    let mut name: OsString = socket.file_stem().unwrap_or(socket.as_os_str()).to_owned();
    name.push(".");
    name.push(ext);
    socket.with_file_name(name)
}

/// The control endpoint beside a daemon socket.
pub fn control_socket(socket: &Path) -> PathBuf {
    with_extension(socket, "ctl")
}

/// The lock file that keeps one daemon per socket.
pub fn lock_file(socket: &Path) -> PathBuf {
    with_extension(socket, "lock")
}

/// The directory of pane sockets beside a daemon socket.
pub fn pane_socket_dir(socket: &Path) -> PathBuf {
    socket.parent().unwrap_or(Path::new(".")).join("panes")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sibling_paths() {
        let sock = Path::new("/run/auk/daemon.sock");
        assert_eq!(control_socket(sock), Path::new("/run/auk/daemon.ctl"));
        assert_eq!(lock_file(sock), Path::new("/run/auk/daemon.lock"));
        assert_eq!(pane_socket_dir(sock), Path::new("/run/auk/panes"));
        assert_eq!(
            control_socket(Path::new("/x/plain")),
            Path::new("/x/plain.ctl")
        );
    }
}
