//! PTYs (§5.2): opening a pair, spawning a program as a session leader with the PTY as its
//! controlling terminal, resizing, and the tty mode gate (§12.1, X1).

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd};
use std::path::Path;
use std::process::Stdio;

use rustix::fs::{Mode, OFlags};
use rustix::pty::OpenptFlags;
use rustix::termios::{self, InputModes, LocalModes, Winsize};
use tokio::process::{Child, Command};

/// A PTY's master side.
#[derive(Debug)]
pub struct Master {
    fd: OwnedFd,
}

/// What to run in a PTY.
#[derive(Clone, Debug)]
pub struct SpawnSpec {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub cwd: std::path::PathBuf,
    /// The complete environment.
    pub env: BTreeMap<String, String>,
    pub cols: u16,
    pub rows: u16,
}

/// The tty's line discipline as the mode gate sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TtyMode {
    /// Canonical (line-editing) mode.
    pub canonical: bool,
    pub echo: bool,
}

impl TtyMode {
    /// Whether the terminal may write TSP input (§12.1): only in raw mode, non-canonical with
    /// echo off, so nothing lands on a shell's command line.
    pub fn accepts_tsp_input(self) -> bool {
        !self.canonical && !self.echo
    }
}

fn winsize(cols: u16, rows: u16) -> Winsize {
    Winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    }
}

/// Opens a PTY pair, spawns `spec` on it, and returns the master and the child.
///
/// The child runs in a new session with the PTY as its controlling terminal; the daemon keeps
/// no copy of the slave, so the master reads EOF once every process holding it is gone.
pub fn spawn(spec: &SpawnSpec) -> io::Result<(Master, Child)> {
    let master = rustix::pty::openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY)?;
    rustix::io::fcntl_setfd(&master, rustix::io::FdFlags::CLOEXEC)?;
    rustix::pty::grantpt(&master)?;
    rustix::pty::unlockpt(&master)?;
    let name = rustix::pty::ptsname(&master, Vec::new())?;
    let slave: OwnedFd = rustix::fs::open(
        name.as_c_str(),
        OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;

    let mut attrs = termios::tcgetattr(&slave)?;
    attrs.input_modes |= InputModes::IUTF8;
    termios::tcsetattr(&slave, termios::OptionalActions::Now, &attrs)?;
    termios::tcsetwinsize(&slave, winsize(spec.cols, spec.rows))?;

    let mut cmd = Command::new(&spec.program);
    cmd.args(&spec.args)
        .current_dir(&spec.cwd)
        .env_clear()
        .envs(&spec.env)
        .stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave))
        .kill_on_drop(false);
    // SAFETY: the closure runs in the child between fork and exec and only makes the
    // async-signal-safe calls setsid(2) and ioctl(2).
    unsafe {
        cmd.pre_exec(|| {
            rustix::process::setsid()?;
            rustix::process::ioctl_tiocsctty(rustix::stdio::stdin())?;
            Ok(())
        });
    }
    let child = cmd.spawn()?;
    // `cmd` holds the slave copies; dropping it here leaves only the child's.
    drop(cmd);
    Ok((Master { fd: master }, child))
}

impl Master {
    /// Sets the window size; the kernel sends SIGWINCH to the foreground process group.
    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        Ok(termios::tcsetwinsize(&self.fd, winsize(cols, rows))?)
    }

    /// The slave's line discipline, read through the master.
    pub fn tty_mode(&self) -> io::Result<TtyMode> {
        tty_mode(self.fd.as_fd())
    }

    /// The process group in the foreground of the tty.
    pub fn foreground_pgid(&self) -> io::Result<i32> {
        Ok(termios::tcgetpgrp(&self.fd)?.as_raw_nonzero().get())
    }

    pub fn into_fd(self) -> OwnedFd {
        self.fd
    }
}

impl AsRawFd for Master {
    fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}

impl AsFd for Master {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

/// The mode gate's reading of a PTY (§12.1): `tcgetattr` on the master.
pub fn tty_mode(fd: BorrowedFd<'_>) -> io::Result<TtyMode> {
    let attrs = termios::tcgetattr(fd)?;
    Ok(TtyMode {
        canonical: attrs.local_modes.contains(LocalModes::ICANON),
        echo: attrs.local_modes.contains(LocalModes::ECHO),
    })
}

/// Whether `path` is an executable file.
pub fn is_executable(path: &Path) -> bool {
    rustix::fs::access(path, rustix::fs::Access::EXEC_OK).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    fn spec(script: &str) -> SpawnSpec {
        SpawnSpec {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            cwd: std::env::temp_dir(),
            env: BTreeMap::from([("PATH".to_owned(), "/usr/bin:/bin".to_owned())]),
            cols: 91,
            rows: 33,
        }
    }

    /// Reads the master on a thread, as the daemon does, and hands the bytes over.
    fn reader(master: &std::sync::Arc<Master>) -> mpsc::Receiver<Vec<u8>> {
        let (tx, rx) = mpsc::channel();
        let master = master.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n @ 1..) = rustix::io::read(&*master, &mut buf) {
                if tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        rx
    }

    fn read_until(rx: &mpsc::Receiver<Vec<u8>>, seen: &mut String, needle: &str) {
        while !seen.contains(needle) {
            let chunk = rx
                .recv_timeout(Duration::from_secs(10))
                .unwrap_or_else(|_| panic!("no {needle:?} from the pty; got {seen:?}"));
            seen.push_str(&String::from_utf8_lossy(&chunk));
        }
    }

    #[tokio::test]
    async fn spawns_with_a_controlling_tty_size_and_session() {
        let (master, mut child) = spawn(&spec(
            "stty size; tty; ps -o sess= -o pgid= -p $$; echo done",
        ))
        .unwrap();
        let pid = child.id().unwrap();
        let rx = reader(&std::sync::Arc::new(master));
        let mut out = String::new();
        read_until(&rx, &mut out, "done");
        assert!(out.contains("33 91"), "{out}");
        // macOS names ptys /dev/ttysN; Linux /dev/pts/N.
        assert!(out.contains("/dev/tty") || out.contains("/dev/pts/"), "{out}");
        // The child leads its own process group (setsid).
        assert!(out.contains(&pid.to_string()), "{out}");
        assert!(child.wait().await.unwrap().success());
    }

    #[tokio::test]
    async fn mode_gate_tracks_raw_mode() {
        let (master, mut child) = spawn(&spec(
            "echo cooked; read x; stty raw -echo; echo raw; read y",
        ))
        .unwrap();
        let master = std::sync::Arc::new(master);
        let rx = reader(&master);
        let mut out = String::new();
        read_until(&rx, &mut out, "cooked");
        let mode = master.tty_mode().unwrap();
        assert!(mode.canonical && mode.echo && !mode.accepts_tsp_input());
        rustix::io::write(&*master, b"\n").unwrap();
        read_until(&rx, &mut out, "raw");
        let mode = master.tty_mode().unwrap();
        assert!(mode.accepts_tsp_input(), "{mode:?}");
        rustix::io::write(&*master, b"\n").unwrap();
        child.wait().await.unwrap();
    }

    #[tokio::test]
    async fn resize_reaches_the_program() {
        let (master, mut child) = spawn(&spec(
            "trap 'stty size; exit 0' WINCH; echo ready; while :; do sleep 0.05; done",
        ))
        .unwrap();
        let master = std::sync::Arc::new(master);
        let rx = reader(&master);
        let mut out = String::new();
        read_until(&rx, &mut out, "ready");
        master.resize(120, 40).unwrap();
        read_until(&rx, &mut out, "40 120");
        child.wait().await.unwrap();
    }
}
