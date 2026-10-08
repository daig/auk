//! A pseudo-terminal running a login shell: the dev harness's stand-in for the daemon's panes.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};

pub struct Pty {
    master: File,
    child: Child,
}

impl Pty {
    /// Spawns the user's shell as a login shell on a new PTY of `cols`×`rows` cells.
    pub fn spawn(cols: u16, rows: u16, cell_px: (u16, u16)) -> io::Result<Pty> {
        let (mut master, mut slave) = (-1, -1);
        let mut size = winsize(cols, rows, cell_px);
        // SAFETY: valid out-pointers; null name and termios use the defaults.
        if unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut size,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: openpty returned two new descriptors that we now own.
        let (master, slave) =
            unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
        set_cloexec(&master)?;

        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
        let name = std::path::Path::new(&shell)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "sh".into());
        let mut command = Command::new(&shell);
        // A leading dash makes it a login shell.
        command
            .arg0(format!("-{name}"))
            .env("TERM", "xterm-256color")
            .env("COLORTERM", "truecolor")
            .env("TERM_PROGRAM", "Auk")
            .stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave));
        if let Some(home) = std::env::var_os("HOME") {
            command.current_dir(home);
        }
        // SAFETY: only async-signal-safe calls between fork and exec.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(io::Error::last_os_error());
                }
                // stdin is the slave: make it the controlling terminal.
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn()?;
        Ok(Pty {
            master: File::from(master),
            child,
        })
    }

    /// A reader for the shell's output, for another thread.
    pub fn reader(&self) -> io::Result<impl Read + Send + 'static> {
        self.master.try_clone()
    }

    pub fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.master.write_all(bytes)
    }

    pub fn resize(&self, cols: u16, rows: u16, cell_px: (u16, u16)) -> io::Result<()> {
        let size = winsize(cols, rows, cell_px);
        // SAFETY: a valid descriptor and winsize.
        if unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &size) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        // The shell gets SIGHUP when the master closes; reap it.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn winsize(cols: u16, rows: u16, cell_px: (u16, u16)) -> libc::winsize {
    libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: cols.saturating_mul(cell_px.0),
        ws_ypixel: rows.saturating_mul(cell_px.1),
    }
}

fn set_cloexec(fd: &OwnedFd) -> io::Result<()> {
    // SAFETY: fcntl on a descriptor we own.
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
    if flags < 0
        || unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
