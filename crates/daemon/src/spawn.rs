//! Starting the daemon from a client (§4.1, §14.2): the first client that finds no daemon
//! spawns `auk daemon` detached, in its own session and process group, with its output going to
//! `<state>/daemon.log`.

use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::paths;

/// How long a freshly spawned daemon may take to answer.
pub const START_TIMEOUT: Duration = Duration::from_secs(15);

/// Whether a daemon answers `ping` on the control endpoint beside `socket`.
pub fn answers(socket: &Path) -> bool {
    let Ok(stream) = UnixStream::connect(paths::control_socket(socket)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut writer = &stream;
    if writer.write_all(b"ping\n").is_err() {
        return false;
    }
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line).is_ok() && line.contains("pong")
}

/// Makes sure a daemon serves `socket`, spawning `exe daemon --socket <socket>` if none
/// answers, and waits until it does.
pub fn ensure(exe: &Path, socket: &Path, state_dir: &Path) -> io::Result<()> {
    if answers(socket) {
        return Ok(());
    }
    fs::create_dir_all(state_dir)?;
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(state_dir.join("daemon.log"))?;
    let mut cmd = Command::new(exe);
    cmd.arg("daemon")
        .arg("--socket")
        .arg(socket)
        .env(paths::STATE_DIR_ENV, state_dir)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    // SAFETY: setsid(2) is async-signal-safe; the child leaves the client's session and
    // process group, so it outlives the client and its terminal.
    unsafe {
        cmd.pre_exec(|| {
            rustix::process::setsid()?;
            Ok(())
        });
    }
    let mut child = cmd.spawn()?;
    let deadline = Instant::now() + START_TIMEOUT;
    while Instant::now() < deadline {
        if answers(socket) {
            // The daemon keeps running; reap it if it ever exits while we live.
            std::thread::spawn(move || child.wait());
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            // Another client may have won the race to start one.
            if answers(socket) {
                return Ok(());
            }
            return Err(io::Error::other(format!(
                "auk daemon exited with {status}; see {}",
                state_dir.join("daemon.log").display()
            )));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "the daemon didn't start in time",
    ))
}
