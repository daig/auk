//! The screen lock: GUI tests (the app, Safari, Ghostty) take turns on the Mac's one screen,
//! keyboard focus and input source, whatever runs them.
//!
//! nextest's `screen` group already serializes them within one run. This lock also holds when
//! they run as threads of one process (`cargo test -- --test-threads 4`) or in two runs at once
//! (two agents): a test waits for the screen, printing which process and test holds it.
//!
//! Two layers:
//! - In one process, a lock owned by a test, reentrant for it. The owner is the current
//!   thread's name, which is the test's name under both libtest and nextest, so a test that
//!   starts an app from a thread it spawns must name that thread after itself.
//! - Across processes, an advisory file lock (`flock`) on `$TMPDIR/auk-screen.lock`, which
//!   holds the owner's pid and test name for the waiting message. The OS releases it when the
//!   process dies, so a stale file never wedges anything.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Condvar, LazyLock, Mutex, MutexGuard};

/// A lock on a screen: one per process for the real one ([`lock`]); tests make their own.
#[derive(Debug)]
pub struct ScreenLock {
    path: PathBuf,
    state: Mutex<State>,
    freed: Condvar,
}

#[derive(Debug, Default)]
struct State {
    owner: Option<String>,
    depth: usize,
    file: Option<File>,
}

/// Holds the screen until dropped.
#[derive(Debug)]
#[must_use = "the screen is released when the guard drops"]
pub struct ScreenGuard {
    lock: &'static ScreenLock,
}

static SCREEN: LazyLock<ScreenLock> =
    LazyLock::new(|| ScreenLock::new(std::env::temp_dir().join("auk-screen.lock")));

/// Takes the screen for the current test, waiting while another test (in this process or
/// another) has it. `what` names what's about to use it, for the waiting message.
pub fn lock(what: &str) -> ScreenGuard {
    SCREEN.acquire(what, |holder| {
        eprintln!("{what}: waiting for the screen, held by {holder}");
    })
}

/// The current thread's owner key: its name (the test's), else its id.
fn owner() -> String {
    let t = std::thread::current();
    match t.name() {
        Some(name) => name.to_owned(),
        None => format!("{:?}", t.id()),
    }
}

impl ScreenLock {
    /// A lock whose cross-process half is the file at `path`.
    pub fn new(path: impl Into<PathBuf>) -> ScreenLock {
        ScreenLock {
            path: path.into(),
            state: Mutex::new(State::default()),
            freed: Condvar::new(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn state(&self) -> MutexGuard<'_, State> {
        // A test that panicked while holding the state mutex leaves nothing half-done in it.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Takes the lock for the current thread's test. `waiting(holder)` runs once if it has to
    /// wait, naming the holder.
    pub fn acquire(&'static self, what: &str, mut waiting: impl FnMut(&str)) -> ScreenGuard {
        let me = owner();
        let mut told = false;
        let mut state = self.state();
        loop {
            match &state.owner {
                Some(o) if *o == me => {
                    state.depth += 1;
                    return ScreenGuard { lock: self };
                }
                None => break,
                Some(o) => {
                    if !told {
                        waiting(&format!("test {o} in this process"));
                        told = true;
                    }
                    state = self.freed.wait(state).unwrap_or_else(|e| e.into_inner());
                }
            }
        }
        // Ours in this process; the guard gives it back if taking the file lock fails.
        state.owner = Some(me.clone());
        state.depth = 1;
        drop(state);
        let guard = ScreenGuard { lock: self };
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&self.path)
            .unwrap_or_else(|e| panic!("open the screen lock {}: {e}", self.path.display()));
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                if !told {
                    let mut holder = String::new();
                    let _ = file.read_to_string(&mut holder);
                    let holder = holder.trim();
                    waiting(if holder.is_empty() {
                        "another process"
                    } else {
                        holder
                    });
                }
                file.lock().unwrap_or_else(|e| {
                    panic!("lock the screen lock {}: {e}", self.path.display())
                });
            }
            Err(TryLockError::Error(e)) => {
                panic!("lock the screen lock {}: {e}", self.path.display())
            }
        }
        // Who holds it, for anyone who waits.
        let _ = file.set_len(0);
        let _ = file.seek(SeekFrom::Start(0));
        let _ = write!(file, "pid {} ({me}: {what})", std::process::id());
        let _ = file.flush();
        self.state().file = Some(file);
        guard
    }

    /// Whether some test in this process holds the lock (for tests of the lock).
    pub fn held(&self) -> bool {
        self.state().owner.is_some()
    }
}

impl Drop for ScreenGuard {
    fn drop(&mut self) {
        let mut state = self.lock.state();
        state.depth = state.depth.saturating_sub(1);
        if state.depth == 0 {
            // Closing the file releases the advisory lock.
            state.file = None;
            state.owner = None;
            self.lock.freed.notify_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    fn temp_lock(name: &str) -> &'static ScreenLock {
        let path = std::env::temp_dir().join(format!(
            "auk-screen-test-{name}-{}.lock",
            std::process::id()
        ));
        Box::leak(Box::new(ScreenLock::new(path)))
    }

    #[test]
    fn the_lock_is_reentrant_for_a_test_and_makes_another_wait() {
        let lock = temp_lock("threads");
        let outer = lock.acquire("outer", |h| panic!("the first take waited for {h}"));
        let inner = lock.acquire("inner", |h| panic!("a reentrant take waited for {h}"));
        drop(inner);
        assert!(lock.held(), "the outer guard still holds it");
        let (tx, rx) = mpsc::channel();
        let other = std::thread::Builder::new()
            .name("another-test".into())
            .spawn(move || {
                let waited = std::cell::Cell::new(None);
                let g = lock.acquire("other", |h| waited.set(Some(h.to_owned())));
                tx.send(waited.take()).unwrap();
                drop(g);
            })
            .unwrap();
        // The other test can't have it while the outer guard lives.
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
        drop(outer);
        let waited = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("taken once freed");
        let holder = waited.expect("the other test was told it waited");
        let me = owner();
        assert!(holder.contains(&me), "{holder}");
        other.join().unwrap();
        assert!(!lock.held());
        let _ = std::fs::remove_file(lock.path());
    }

    /// `flock` locks belong to an open file, so two locks on one path in this process contend
    /// exactly as two processes do.
    #[test]
    fn another_holder_of_the_lock_file_makes_a_test_wait_naming_it() {
        let a = temp_lock("files");
        let b: &'static ScreenLock = Box::leak(Box::new(ScreenLock::new(a.path())));
        let held = a.acquire("the holder", |h| panic!("the first take waited for {h}"));
        let (tx, rx) = mpsc::channel();
        let waiter = std::thread::Builder::new()
            .name("the-waiter".into())
            .spawn(move || {
                let started = Instant::now();
                let told = std::cell::RefCell::new(String::new());
                let g = b.acquire("the waiter", |h| *told.borrow_mut() = h.to_owned());
                tx.send((told.take(), started.elapsed())).unwrap();
                drop(g);
            })
            .unwrap();
        assert!(rx.recv_timeout(Duration::from_millis(500)).is_err());
        drop(held);
        let (told, waited) = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("taken once freed");
        assert!(waited >= Duration::from_millis(400), "{waited:?}");
        let me = owner();
        assert!(
            told.contains(&format!("pid {}", std::process::id()))
                && told.contains(&me)
                && told.contains("the holder"),
            "{told}"
        );
        waiter.join().unwrap();
        let _ = std::fs::remove_file(a.path());
    }
}
