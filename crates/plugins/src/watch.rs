//! Reloading on change (§9.3 "Watching"): FSEvents on macOS and inotify on Linux, with a 2 s
//! poll of a tree fingerprint (paths, sizes, mtimes; 8 levels; 10,000 entries) as the fallback.
//! Changes are batched over 300 ms, and any change reloads every plugin in the watching context.
//! The plugins folder and every linked or symlinked package outside it are watched.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use notify::Watcher as _;

/// Changes within this window are one reload.
pub const BATCH: Duration = Duration::from_millis(300);
/// The fallback poll interval.
pub const POLL: Duration = Duration::from_secs(2);
const DEPTH: usize = 8;
const ENTRIES: usize = 10_000;

type Fingerprint = Vec<(PathBuf, u64, Option<SystemTime>)>;

/// The plugins folder and the package directories it links to outside itself.
fn roots(plugins_dir: &Path) -> Vec<PathBuf> {
    let mut roots = vec![plugins_dir.to_path_buf()];
    let base = plugins_dir
        .canonicalize()
        .unwrap_or_else(|_| plugins_dir.to_path_buf());
    if let Ok(entries) = std::fs::read_dir(plugins_dir) {
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            let target = if path.extension().is_some_and(|e| e == "path") && path.is_file() {
                std::fs::read_to_string(&path)
                    .ok()
                    .map(|t| PathBuf::from(t.trim()))
            } else if entry.file_type().is_ok_and(|t| t.is_symlink()) {
                path.canonicalize().ok()
            } else {
                None
            };
            if let Some(t) = target
                && let Ok(t) = t.canonicalize()
                && !t.starts_with(&base)
                && t.is_dir()
            {
                roots.push(t);
            }
        }
    }
    roots
}

fn walk(dir: &Path, depth: usize, out: &mut Fingerprint) {
    if depth > DEPTH || out.len() >= ENTRIES {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        if out.len() >= ENTRIES {
            return;
        }
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        out.push((path.clone(), meta.len(), meta.modified().ok()));
        if meta.is_dir() {
            walk(&path, depth + 1, out);
        }
    }
}

/// A fingerprint of everything under the watched roots.
fn fingerprint(plugins_dir: &Path) -> Fingerprint {
    let mut out = Vec::new();
    for root in roots(plugins_dir) {
        if let Ok(meta) = std::fs::metadata(&root) {
            out.push((root.clone(), meta.len(), meta.modified().ok()));
        }
        walk(&root, 1, &mut out);
    }
    out
}

/// Watches a plugins folder and calls back (on its own thread) after each batch of changes.
pub struct Watcher {
    stop: Arc<AtomicBool>,
    wake: crossbeam_channel::Sender<()>,
    thread: Option<JoinHandle<()>>,
    native: bool,
}

impl std::fmt::Debug for Watcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watcher")
            .field("native", &self.native)
            .finish_non_exhaustive()
    }
}

impl Watcher {
    /// Watches `plugins_dir` with the platform's notifications, falling back to the poll.
    pub fn start(plugins_dir: PathBuf, on_change: impl Fn() + Send + 'static) -> Watcher {
        Self::spawn(plugins_dir, Box::new(on_change), true)
    }

    /// Watches by polling only (where native watches aren't live, and in tests of the fallback).
    pub fn polling(plugins_dir: PathBuf, on_change: impl Fn() + Send + 'static) -> Watcher {
        Self::spawn(plugins_dir, Box::new(on_change), false)
    }

    /// Whether native notifications are in use.
    pub fn is_native(&self) -> bool {
        self.native
    }

    fn spawn(plugins_dir: PathBuf, on_change: Box<dyn Fn() + Send>, native: bool) -> Watcher {
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = crossbeam_channel::unbounded::<()>();
        let mut watcher = if native {
            let tx = tx.clone();
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                if let Ok(event) = res
                    && !matches!(event.kind, notify::EventKind::Access(_))
                {
                    let _ = tx.send(());
                }
            })
            .ok()
        } else {
            None
        };
        let watch_roots = |w: &mut notify::RecommendedWatcher, roots: &[PathBuf]| -> bool {
            roots
                .iter()
                .all(|r| w.watch(r, notify::RecursiveMode::Recursive).is_ok())
        };
        let mut watched = roots(&plugins_dir);
        if let Some(w) = watcher.as_mut()
            && !watch_roots(w, &watched)
        {
            watcher = None;
        }
        let is_native = watcher.is_some();
        let flag = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("plugin watch".into())
            .spawn(move || {
                let mut last = fingerprint(&plugins_dir);
                while !flag.load(Ordering::Acquire) {
                    let tick = if watcher.is_some() {
                        Duration::from_millis(200)
                    } else {
                        POLL
                    };
                    let woke = rx.recv_timeout(tick).is_ok();
                    if flag.load(Ordering::Acquire) {
                        return;
                    }
                    if watcher.is_some() && !woke {
                        continue;
                    }
                    // Batch: wait out the rest of the window, then compare.
                    let until = Instant::now() + BATCH;
                    while let Some(left) = until.checked_duration_since(Instant::now()) {
                        let _ = rx.recv_timeout(left);
                    }
                    while rx.try_recv().is_ok() {}
                    let now = fingerprint(&plugins_dir);
                    if now == last {
                        continue;
                    }
                    last = now;
                    if let Some(w) = watcher.as_mut() {
                        let fresh = roots(&plugins_dir);
                        if fresh != watched {
                            for r in &watched {
                                let _ = w.unwatch(r);
                            }
                            let _ = watch_roots(w, &fresh);
                            watched = fresh;
                        }
                    }
                    on_change();
                }
            })
            .ok();
        Watcher {
            stop,
            wake: tx,
            thread,
            native: is_native,
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.wake.send(());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
