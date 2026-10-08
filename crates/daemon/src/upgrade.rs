//! In-place upgrade (§15.7, Unix): the daemon execs a new build of itself without ending any
//! program.
//!
//! 1. **Quiesce.** Every PTY reader waits in [`wait_readable`] (poll on the master and a wake
//!    pipe), never inside `read`. An upgrade raises the gate and writes to the wake pipe; each
//!    reader parks before reading again, so no output is read that the snapshots won't hold.
//!    What programs write meanwhile waits in the kernel for the new daemon.
//! 2. **Hand over.** The state, in the state file's format (§16) plus each live process pane's
//!    pid and PTY master descriptor and the two listening sockets, goes into an unlinked file
//!    whose descriptor the new daemon inherits ([`UPGRADE_FD_ENV`]). Those descriptors lose
//!    close-on-exec; everything else (clients, pane sockets, the lock) closes on exec.
//! 3. **Exec** `<exe> daemon --socket <socket>` in place: same pid, so the pane processes stay
//!    its children. Plugins stop with the old image; blocks restart from `save`.
//! 4. **The new daemon** ([`Daemon::adopt`]) keeps listening on the inherited sockets, rebuilds
//!    each pane's replica from its snapshot (no restore record: the program is still running),
//!    reads its PTY again and waits for its process by pid. Clients reconnect and get fresh
//!    snapshots (§15.3).
//!
//! Triggers: the control command `upgrade [--exe PATH]` (`auk daemon upgrade`), and a client of a
//! newer release saying hello with its binary's path (Tern since 0.5.2): that client is refused
//! with `upgrading` set, the daemon execs the client's binary once it answers `--version` with
//! the client's release, and the client reconnects. Any other version mismatch is refused (X9).

use std::fs;
use std::io::{self, Read, Seek, Write};
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags};
use rustix::io::FdFlags;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::pane::{GateStats, Pane, PaneIo, PaneKind, default_facts};
use crate::persist::{DaemonState, SavedPane};
use crate::server::{Daemon, lock};
use crate::{pane_socket, paths, pty};

/// Names the inherited descriptor holding the handoff.
pub const UPGRADE_FD_ENV: &str = "AUK_UPGRADE_FD";

/// Makes the daemon report another Auk release than its own, so a test can stage an older
/// daemon for a newer client to upgrade. An upgrade never passes it on.
pub const VERSION_OVERRIDE_ENV: &str = "AUK_TEST_DAEMON_VERSION";

/// The versions this daemon reports: its own, with the release from [`VERSION_OVERRIDE_ENV`]
/// when that is set.
pub fn reported_versions() -> auk_mirror::Versions {
    let mut versions = auk_mirror::Versions::current();
    if let Some(auk) = std::env::var(VERSION_OVERRIDE_ENV)
        .ok()
        .filter(|v| !v.is_empty())
    {
        versions.auk = auk;
    }
    versions
}

/// How long the PTY readers may take to park.
const QUIESCE_TIMEOUT: Duration = Duration::from_secs(3);

/// How long a reply may take to reach its client before the exec closes the connection.
const EXEC_DELAY: Duration = Duration::from_millis(100);

/// What the old daemon hands the new one.
#[derive(Debug, Serialize, Deserialize)]
pub struct Handoff {
    pub state: DaemonState,
    /// The daemon socket's listener.
    pub socket_fd: RawFd,
    /// The control endpoint's listener.
    pub control_fd: RawFd,
}

/// The handoff this process was started with, taken from the environment by [`take_from_env`].
static INHERITED: Mutex<Option<Handoff>> = Mutex::new(None);

/// Reads the handoff named by [`UPGRADE_FD_ENV`], if any, and removes the variable so no pane
/// inherits it. Call before any other thread starts.
pub fn take_from_env() -> io::Result<()> {
    let Some(value) = std::env::var_os(UPGRADE_FD_ENV) else {
        return Ok(());
    };
    // SAFETY: called from `run` before the runtime or any other thread exists.
    unsafe { std::env::remove_var(UPGRADE_FD_ENV) };
    let fd: RawFd = value
        .to_str()
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| io::Error::other(format!("{UPGRADE_FD_ENV} is not a descriptor")))?;
    // SAFETY: the old daemon passed this descriptor for us alone; nothing else owns it.
    let mut file = fs::File::from(unsafe { OwnedFd::from_raw_fd(fd) });
    let mut text = Vec::new();
    file.read_to_end(&mut text)?;
    let handoff: Handoff =
        serde_json::from_slice(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    *lock(&INHERITED) = Some(handoff);
    Ok(())
}

/// The handoff, once: [`serve`](crate::serve) takes it.
pub(crate) fn take_inherited() -> Option<Handoff> {
    lock(&INHERITED).take()
}

/// Takes ownership of an inherited descriptor and sets close-on-exec again, so programs started
/// later don't inherit it.
pub(crate) fn own_fd(fd: RawFd) -> io::Result<OwnedFd> {
    // SAFETY: the handoff names descriptors the old daemon left open for this process alone,
    // each once.
    let owned = unsafe { OwnedFd::from_raw_fd(fd) };
    rustix::io::fcntl_setfd(&owned, FdFlags::CLOEXEC)?;
    Ok(owned)
}

// ---------------------------------------------------------------------------------------------
// Reader gate

struct Gate {
    parking: AtomicBool,
    counts: Mutex<Counts>,
    cv: Condvar,
    wake_r: OwnedFd,
    wake_w: OwnedFd,
}

#[derive(Default)]
struct Counts {
    active: usize,
    parked: usize,
}

fn gate() -> &'static Gate {
    static GATE: OnceLock<Gate> = OnceLock::new();
    GATE.get_or_init(|| {
        let (r, w) = rustix::pipe::pipe().expect("a wake pipe for the PTY readers");
        for fd in [&r, &w] {
            let _ = rustix::io::fcntl_setfd(fd, FdFlags::CLOEXEC);
            let flags = rustix::fs::fcntl_getfl(fd).unwrap_or(rustix::fs::OFlags::empty());
            let _ = rustix::fs::fcntl_setfl(fd, flags | rustix::fs::OFlags::NONBLOCK);
        }
        Gate {
            parking: AtomicBool::new(false),
            counts: Mutex::new(Counts::default()),
            cv: Condvar::new(),
            wake_r: r,
            wake_w: w,
        }
    })
}

/// A PTY reader's registration with the gate, for as long as it reads.
#[derive(Debug)]
pub(crate) struct Reader(());

/// Registers a PTY reader.
pub(crate) fn reader() -> Reader {
    lock(&gate().counts).active += 1;
    Reader(())
}

impl Drop for Reader {
    fn drop(&mut self) {
        let g = gate();
        lock(&g.counts).active -= 1;
        g.cv.notify_all();
    }
}

impl Gate {
    fn park(&self) {
        let mut counts = lock(&self.counts);
        counts.parked += 1;
        self.cv.notify_all();
        while self.parking.load(Ordering::SeqCst) {
            counts = self.cv.wait(counts).unwrap_or_else(|e| e.into_inner());
        }
        counts.parked -= 1;
    }

    /// Parks every reader; false (and nothing parked) if they don't all park in time.
    fn quiesce(&self) -> bool {
        self.parking.store(true, Ordering::SeqCst);
        let _ = rustix::io::write(&self.wake_w, b"u");
        let deadline = Instant::now() + QUIESCE_TIMEOUT;
        let mut counts = lock(&self.counts);
        while counts.parked < counts.active {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                drop(counts);
                self.release();
                return false;
            }
            counts = self
                .cv
                .wait_timeout(counts, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        true
    }

    /// Lets the readers go on (an upgrade failed).
    fn release(&self) {
        let mut buf = [0u8; 64];
        while matches!(rustix::io::read(&self.wake_r, &mut buf), Ok(1..)) {}
        self.parking.store(false, Ordering::SeqCst);
        let _counts = lock(&self.counts);
        self.cv.notify_all();
    }
}

/// Blocks until `fd` can be read (or has hung up). While an upgrade hands over, parks instead,
/// without reading.
pub(crate) fn wait_readable(fd: BorrowedFd<'_>) {
    let g = gate();
    loop {
        if g.parking.load(Ordering::SeqCst) {
            g.park();
            continue;
        }
        let mut fds = [
            PollFd::new(&fd, PollFlags::IN),
            PollFd::new(&g.wake_r, PollFlags::IN),
        ];
        match rustix::event::poll(&mut fds, None) {
            Ok(_) => {}
            Err(rustix::io::Errno::INTR) => continue,
            // Let the read report it.
            Err(_) => return,
        }
        if g.parking.load(Ordering::SeqCst) {
            continue;
        }
        if !fds[0].revents().is_empty() {
            return;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The old daemon

/// Whether release `a` is newer than `b`: numeric dot-separated parts compare in order, and a
/// pre-release (`-…`) is older than its release.
pub fn is_newer(a: &str, b: &str) -> bool {
    fn parse(v: &str) -> (Vec<u64>, Option<&str>) {
        let (core, pre) = match v.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (v, None),
        };
        let core = core.split('.').map(|p| p.parse().unwrap_or(0)).collect();
        (core, pre)
    }
    let (ac, ap) = parse(a);
    let (bc, bp) = parse(b);
    match ac.cmp(&bc) {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Less => false,
        std::cmp::Ordering::Equal => match (ap, bp) {
            (None, Some(_)) => true,
            (Some(a), Some(b)) => a > b,
            _ => false,
        },
    }
}

/// Checks that `exe` is an Auk binary of release `auk` (or any release when `None`): it must
/// answer `--version` with it.
pub fn check_exe(exe: &Path, auk: Option<&str>) -> Result<String, String> {
    if !pty::is_executable(exe) {
        return Err(format!("{} is not executable", exe.display()));
    }
    let out = std::process::Command::new(exe)
        .arg("--version")
        .env_remove(UPGRADE_FD_ENV)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("{}: {e}", exe.display()))?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    let Some(release) = text.strip_prefix("auk ") else {
        return Err(format!(
            "{} is not auk (`--version` said {text:?})",
            exe.display()
        ));
    };
    match auk {
        Some(want) if release != want => {
            Err(format!("{} is Auk {release}, not {want}", exe.display()))
        }
        _ => Ok(release.to_owned()),
    }
}

/// An upgrade has parked the readers and is about to exec.
static UNDER_WAY: AtomicBool = AtomicBool::new(false);

/// Whether an upgrade is under way: clients are told to connect again.
pub(crate) fn under_way() -> bool {
    UNDER_WAY.load(Ordering::SeqCst)
}

impl Daemon {
    /// The control command `upgrade [--exe PATH]`: checks the binary, parks the readers and
    /// answers; the exec follows right after the answer has gone out.
    pub(crate) fn cmd_upgrade(self: &Arc<Self>, args: &[String]) -> Result<Value, String> {
        let exe = match args {
            [] => std::env::current_exe().map_err(|e| format!("this daemon's binary: {e}"))?,
            [flag, path] if flag == "--exe" => PathBuf::from(path),
            _ => return Err("upgrade: expected [--exe PATH]".into()),
        };
        let release = check_exe(&exe, None)?;
        self.start_upgrade(exe.clone())?;
        Ok(json!({
            "exe": exe,
            "from": self.versions.auk,
            "to": release,
            "upgrades": self.persist.upgrades(),
            "pid": std::process::id(),
        }))
    }

    /// Parks the readers, then execs `exe` after [`EXEC_DELAY`]. Fails, changing nothing, when
    /// an upgrade is already under way or the readers don't park.
    pub(crate) fn start_upgrade(self: &Arc<Self>, exe: PathBuf) -> Result<(), String> {
        if UNDER_WAY.swap(true, Ordering::SeqCst) {
            return Err("an upgrade is already under way".into());
        }
        if !gate().quiesce() {
            UNDER_WAY.store(false, Ordering::SeqCst);
            return Err("the PTY readers didn't stop in time".into());
        }
        eprintln!("auk daemon: upgrading to {}", exe.display());
        let daemon = self.clone();
        std::thread::spawn(move || {
            std::thread::sleep(EXEC_DELAY);
            let err = daemon.exec_upgrade(&exe);
            eprintln!("auk daemon: upgrade to {} failed: {err}", exe.display());
            gate().release();
            UNDER_WAY.store(false, Ordering::SeqCst);
        });
        Ok(())
    }

    /// Hands over and execs; returns only on failure.
    fn exec_upgrade(&self, exe: &Path) -> String {
        let Some((socket_fd, control_fd)) = self.persist.listeners() else {
            return "the listening sockets are unknown".into();
        };
        let mut state = self.collect_state(true);
        state.upgrades += 1;
        // The state file too, in case the new build fails to start.
        if let Ok(bytes) = serde_json::to_vec(&DaemonState {
            panes: state
                .panes
                .iter()
                .cloned()
                .map(|p| SavedPane { process: None, ..p })
                .collect(),
            ..state.clone()
        }) {
            let _ =
                crate::persist::write_atomic(&crate::persist::state_path(&self.state_dir), &bytes);
        }
        let mut inherited: Vec<RawFd> = vec![socket_fd, control_fd];
        inherited.extend(state.panes.iter().filter_map(|p| p.process.map(|i| i.fd)));
        let handoff = Handoff {
            state,
            socket_fd,
            control_fd,
        };
        let file = match handoff_file(&self.state_dir, &handoff) {
            Ok(file) => file,
            Err(err) => return format!("writing the handoff: {err}"),
        };
        inherited.push(file.as_raw_fd());
        for fd in &inherited {
            // SAFETY: each is a live descriptor this daemon owns (listeners, PTY masters, the
            // handoff file); only its close-on-exec flag changes.
            let fd = unsafe { BorrowedFd::borrow_raw(*fd) };
            if let Err(err) = rustix::io::fcntl_setfd(fd, FdFlags::empty()) {
                restore_cloexec(&inherited);
                return format!("keeping descriptors open: {err}");
            }
        }
        let err = std::process::Command::new(exe)
            .arg("daemon")
            .arg("--socket")
            .arg(&self.socket)
            .env(paths::STATE_DIR_ENV, &self.state_dir)
            .env(UPGRADE_FD_ENV, file.as_raw_fd().to_string())
            .env_remove(VERSION_OVERRIDE_ENV)
            .exec();
        restore_cloexec(&inherited);
        err.to_string()
    }
}

fn restore_cloexec(fds: &[RawFd]) {
    for fd in fds {
        // SAFETY: as in `exec_upgrade`.
        let fd = unsafe { BorrowedFd::borrow_raw(*fd) };
        let _ = rustix::io::fcntl_setfd(fd, FdFlags::CLOEXEC);
    }
}

/// The handoff in an unlinked file, read from the start.
fn handoff_file(state_dir: &Path, handoff: &Handoff) -> io::Result<fs::File> {
    let path = state_dir.join(format!("upgrade-{}.json", std::process::id()));
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)?;
    fs::remove_file(&path)?;
    file.write_all(&serde_json::to_vec(handoff).map_err(io::Error::other)?)?;
    file.rewind()?;
    Ok(file)
}

// ---------------------------------------------------------------------------------------------
// The new daemon

impl Daemon {
    /// Takes over the old daemon's panes (§15.7): running processes keep their PTYs; blocks
    /// restart from their last `save`.
    pub(crate) fn adopt(self: &Arc<Self>, state: DaemonState) {
        self.persist.set_upgrades(state.upgrades);
        self.set_settings(state.settings.clone());
        {
            let mut st = self.state();
            st.layout = state.layout.clone();
            st.next_pane = st.next_pane.max(state.next_pane);
        }
        let mut present = Vec::new();
        for saved in &state.panes {
            let id = saved.record.id;
            let result = match (&saved.record.kind, saved.process) {
                (PaneKind::Block { .. }, _) => self.restore_block(&saved.full_record()),
                (_, Some(inherited)) => self.adopt_process(saved, inherited.pid, inherited.fd),
                (_, None) => Err("no process to take over".into()),
            };
            match result {
                Ok(()) => present.push(id),
                Err(err) => eprintln!("auk daemon: pane {id} not taken over: {err}"),
            }
        }
        self.drop_missing_from_layout(&present);
        eprintln!(
            "auk daemon: upgrade {} took over {} of {} panes",
            state.upgrades,
            present.len(),
            state.panes.len()
        );
    }

    fn adopt_process(
        self: &Arc<Self>,
        saved: &SavedPane,
        pid: u32,
        fd: RawFd,
    ) -> Result<(), String> {
        let master = Arc::new(pty::Master::from_fd(own_fd(fd).map_err(|e| e.to_string())?));
        let record = saved.full_record();
        let id = record.id;
        let mut replica = self.replica_from(&record);
        replica.set_facts(default_facts());
        let pane = Arc::new(Mutex::new(Pane::new(
            id,
            record.kind.clone(),
            record.cwd.clone(),
            record.window_key.clone(),
            replica,
        )));
        let pane_socket = paths::pane_socket_dir(&self.socket).join(format!("{id}.sock"));
        let gate_stats = Arc::new(GateStats::default());
        let (input_tx, input_rx) = mpsc::unbounded_channel();
        let generation = {
            let mut p = lock(&pane);
            p.shell.marks = saved.shell.marks.clone();
            p.shell.prompts = saved.shell.prompts;
            p.shell.at_prompt = saved.shell.at_prompt;
            p.shell.running = saved.shell.running;
            p.resume = saved.vars.clone();
            p.plugin = Some(self.plugins.link());
            self.sync_catalog(&mut p);
            p.generation += 1;
            p.pid = Some(pid);
            let socket = pane_socket::spawn(&pane_socket)
                .map_err(|e| eprintln!("pane {id}: pane socket: {e}"))
                .ok();
            p.io = Some(PaneIo {
                master: master.clone(),
                input: input_tx,
                gate: gate_stats.clone(),
                pane_socket: socket,
            });
            p.generation
        };
        crate::server::spawn_thread(format!("pane {id} writer"), {
            let master = master.clone();
            move || crate::server::write_pty(&master, input_rx, &gate_stats)
        })?;
        let (drained_tx, drained) = std::sync::mpsc::channel::<()>();
        crate::server::spawn_thread(format!("pane {id} reader"), {
            let pane = pane.clone();
            let master = master.clone();
            move || {
                crate::server::read_pty(&pane, &master, generation);
                let _ = drained_tx.send(());
            }
        })?;
        let daemon = self.clone();
        crate::server::spawn_thread(format!("pane {id} waiter"), move || {
            let status = wait_for_exit(pid);
            let _ = drained.recv_timeout(crate::server::EXIT_DRAIN);
            daemon.pane_exited(id, generation, status);
        })?;
        {
            let mut st = self.state();
            st.next_pane = st.next_pane.max(id);
            st.panes.insert(id, pane.clone());
        }
        lock(&pane).link_info();
        Ok(())
    }
}

/// Waits for an inherited child: its exit code, or 128 plus the signal; -1 if it can't be
/// waited for.
fn wait_for_exit(pid: u32) -> i32 {
    let Some(pid) = rustix::process::Pid::from_raw(pid as i32) else {
        return -1;
    };
    loop {
        match rustix::process::waitpid(Some(pid), rustix::process::WaitOptions::empty()) {
            Ok(Some((_, status))) => {
                if let Some(code) = status.exit_status() {
                    return code;
                }
                if let Some(signal) = status.terminating_signal() {
                    return 128 + signal;
                }
            }
            Ok(None) | Err(rustix::io::Errno::INTR) => {}
            Err(_) => return -1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsFd;

    #[test]
    fn release_order() {
        assert!(is_newer("0.5.2", "0.5.1"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("0.0.0", "0.0.0-old"));
        assert!(!is_newer("0.0.0", "0.0.0"));
        assert!(!is_newer("0.4.9", "0.5.0"));
        assert!(!is_newer("1.0.0-rc1", "1.0.0"));
    }

    #[test]
    fn readers_park_and_resume() {
        let (r, w) = rustix::pipe::pipe().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let t = std::thread::spawn(move || {
            let _reader = reader();
            for _ in 0..2 {
                wait_readable(r.as_fd());
                let mut b = [0u8; 1];
                rustix::io::read(&r, &mut b).unwrap();
                tx.send(b[0]).unwrap();
            }
        });
        rustix::io::write(&w, b"a").unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), b'a');
        // Parked: data waits unread.
        assert!(gate().quiesce());
        rustix::io::write(&w, b"b").unwrap();
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
        gate().release();
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), b'b');
        t.join().unwrap();
    }
}
