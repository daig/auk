//! `auk attach --stdio`: the remote end of an SSH link.
//!
//! It makes sure the host's daemon is running (starting it detached if none answers, as a window
//! would), connects to its socket and writes one preamble line to stdout:
//!
//! ```text
//! AUK-BRIDGE 1 {"user":"dai","host":"build-box","os":"linux",…}
//! ```
//!
//! Every byte after that line is the daemon's: the bridge copies stdin to the socket and the
//! socket to stdout, so the replication protocol runs over SSH unchanged. The preamble
//! lets the local side skip anything a login script printed first, and tells it who it is
//! acting as on the host. When the bridge can't reach a daemon it writes `AUK-BRIDGE-ERROR` and
//! a reason instead.
//!
//! The bridge ends when either side closes. The daemon outlives it, with every pane.

use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::mpsc;
use std::thread;

use serde::{Deserialize, Serialize};

/// The preamble's first word.
pub const PREAMBLE: &str = "AUK-BRIDGE";
/// The first word of the line a bridge writes instead of the preamble when it fails.
pub const PREAMBLE_ERROR: &str = "AUK-BRIDGE-ERROR";
/// The preamble format's version.
pub const BRIDGE_VERSION: u32 = 1;

/// What the bridge says about the host in its preamble.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BridgeInfo {
    /// The user the bridge, and so the daemon and its panes, run as.
    pub user: String,
    /// The host's name.
    pub host: String,
    pub os: String,
    pub arch: String,
    /// The bridge's Auk version; the daemon checks the client's versions in the handshake.
    pub auk: String,
    /// `$SSH_CONNECTION` as sshd set it: client address and port, server address and port.
    #[serde(default)]
    pub ssh_connection: Option<String>,
    /// Whether this bridge started the daemon.
    #[serde(default)]
    pub started_daemon: bool,
}

impl BridgeInfo {
    /// This process's host and user.
    pub fn here(started_daemon: bool) -> BridgeInfo {
        BridgeInfo {
            user: current_user(),
            host: hostname(),
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
            auk: env!("CARGO_PKG_VERSION").to_owned(),
            ssh_connection: std::env::var("SSH_CONNECTION")
                .ok()
                .filter(|v| !v.is_empty()),
            started_daemon,
        }
    }

    /// `user@host`.
    pub fn acting(&self) -> String {
        format!("{}@{}", self.user, self.host)
    }

    /// The preamble line, newline included.
    pub fn preamble(&self) -> String {
        format!(
            "{PREAMBLE} {BRIDGE_VERSION} {}\n",
            serde_json::to_string(self).expect("bridge info serializes")
        )
    }
}

/// The user this process runs as: `$USER`, else `$LOGNAME`, else the uid.
pub fn current_user() -> String {
    ["USER", "LOGNAME"]
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|v| !v.is_empty()))
        .unwrap_or_else(|| rustix::process::geteuid().as_raw().to_string())
}

/// This machine's host name.
pub fn hostname() -> String {
    rustix::system::uname()
        .nodename()
        .to_string_lossy()
        .into_owned()
}

/// Runs the bridge on this process's stdio until either side closes.
///
/// `exe` is the `auk` binary to start a daemon with, `socket` the daemon socket and `state_dir`
/// the daemon's state directory.
pub fn run_stdio(exe: &Path, socket: &Path, state_dir: &Path) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    let started = !auk_daemon::spawn::answers(socket);
    let stream = auk_daemon::spawn::ensure(exe, socket, state_dir)
        .and_then(|()| UnixStream::connect(socket))
        .map_err(|err| {
            let err = io::Error::new(err.kind(), format!("{}: {err}", socket.display()));
            let _ = writeln!(stdout, "{PREAMBLE_ERROR} {err}");
            let _ = stdout.flush();
            err
        })?;
    stdout.write_all(BridgeInfo::here(started).preamble().as_bytes())?;
    stdout.flush()?;
    drop(stdout);

    let (done_tx, done_rx) = mpsc::channel::<()>();
    {
        let mut to_daemon = stream.try_clone()?;
        let done_tx = done_tx.clone();
        thread::Builder::new()
            .name("auk bridge stdin".into())
            .spawn(move || {
                let _ = pump(&mut io::stdin().lock(), &mut to_daemon);
                // The link closed: let the daemon see the end of the client's stream.
                let _ = to_daemon.shutdown(Shutdown::Write);
                let _ = done_tx.send(());
            })?;
    }
    {
        let mut from_daemon = stream;
        thread::Builder::new()
            .name("auk bridge stdout".into())
            .spawn(move || {
                let _ = pump(&mut from_daemon, &mut io::stdout());
                let _ = done_tx.send(());
            })?;
    }
    // Either direction ending ends the bridge: a closed link, or a daemon that went away.
    let _ = done_rx.recv();
    Ok(())
}

/// Copies until end of stream or an error, flushing after every read so frames never wait in a
/// buffer.
fn pump(from: &mut impl Read, to: &mut impl Write) -> io::Result<()> {
    let mut buf = vec![0u8; 64 << 10];
    loop {
        let n = match from.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };
        to.write_all(&buf[..n])?;
        to.flush()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preamble_round_trips() {
        let info = BridgeInfo::here(true);
        let line = info.preamble();
        let rest = line
            .strip_prefix(&format!("{PREAMBLE} {BRIDGE_VERSION} "))
            .expect("prefix");
        let back: BridgeInfo = serde_json::from_str(rest.trim_end()).expect("json");
        assert_eq!(back, info);
        assert!(line.ends_with('\n') && !line[..line.len() - 1].contains('\n'));
        assert!(back.acting().contains('@'));
    }
}
