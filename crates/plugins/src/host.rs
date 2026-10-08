//! The daemon side of the plugin runtime: loading, the catalog, events and the spawn filter.
//!
//! The daemon waits on workers in only two places (§4.1): while plugins load or reload, and for
//! the spawn filter (at most 50 ms per plugin, counted from when the request is queued).
//! Everything else is a channel send, and worker output arrives on [`PluginHost::outputs`].

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;

use crate::catalog::{Catalog, Status};
use crate::discovery::{self, PluginSettings};
use crate::events::HostEvent;
use crate::paths;
use crate::spawn::SpawnSpec;
use crate::vm::{LogSink, tracing_log_sink};
use crate::worker::{HostOutput, Late, Outputs, Starting, Waker, WorkerHandle};

/// How long the daemon waits for each plugin's spawn filter.
pub const SPAWN_WAIT: Duration = Duration::from_millis(50);
/// How long a retiring worker gets to let go of its blocks.
pub const RETIRE_WAIT: Duration = Duration::from_secs(3);

/// Where a host finds plugins, and how it reports.
#[derive(Clone)]
pub struct HostConfig {
    /// `<config>/plugins/`.
    pub plugins_dir: PathBuf,
    /// `<state>`, which holds `plugin-data/`.
    pub state_dir: PathBuf,
    /// `plugins` and `plugins_disabled`.
    pub settings: PluginSettings,
    /// Called after a worker sends output, to wake the daemon's loop.
    pub waker: Option<Waker>,
    /// Where `tern.log` and `print` go.
    pub log: LogSink,
}

impl std::fmt::Debug for HostConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostConfig")
            .field("plugins_dir", &self.plugins_dir)
            .field("state_dir", &self.state_dir)
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

impl HostConfig {
    /// The standard locations (§9.1, D5), default settings, and logs to tracing.
    pub fn standard() -> Self {
        HostConfig {
            plugins_dir: paths::plugins_dir(&paths::config_dir()),
            state_dir: paths::state_dir(),
            settings: PluginSettings::default(),
            waker: None,
            log: tracing_log_sink(),
        }
    }
}

/// The host halves of every plugin on this machine.
pub struct PluginHost {
    config: HostConfig,
    /// Ready workers, in id order.
    workers: Vec<WorkerHandle>,
    catalog: Catalog,
    outputs: Outputs,
    rx: Receiver<HostOutput>,
}

impl std::fmt::Debug for PluginHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginHost")
            .field("config", &self.config)
            .field("catalog", &self.catalog)
            .finish_non_exhaustive()
    }
}

impl PluginHost {
    /// Scans the plugins folder and loads every allowed host half: all workers start at once,
    /// each entry gets its 2 s budget, and this waits for each to report.
    pub fn start(config: HostConfig) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        let outputs = Outputs {
            tx,
            waker: config.waker.clone(),
        };
        let mut host = PluginHost {
            config,
            workers: Vec::new(),
            catalog: Catalog::default(),
            outputs,
            rx,
        };
        host.load();
        host
    }

    fn load(&mut self) {
        let scan = discovery::scan(&self.config.plugins_dir);
        let settings = &self.config.settings;
        let mut starting: Vec<(String, Result<Starting, String>)> = Vec::new();
        for package in &scan.packages {
            let m = &package.manifest;
            if m.host.is_none() || !settings.allows(&m.id) {
                continue;
            }
            let data = paths::data_dir(&self.config.state_dir, &m.id);
            let worker = WorkerHandle::start(
                package,
                data,
                self.outputs.clone(),
                Arc::clone(&self.config.log),
            );
            starting.push((m.id.clone(), worker));
        }
        let mut statuses = Vec::with_capacity(starting.len());
        let mut jit = false;
        for (id, worker) in starting {
            let status = match worker {
                Ok(starting) => {
                    let (result, worker_jit) = starting.wait();
                    match result {
                        Ok(handle) => {
                            jit |= worker_jit;
                            self.workers.push(handle);
                            Status::Ready
                        }
                        Err(error) => Status::Failed(error),
                    }
                }
                Err(error) => Status::Failed(error),
            };
            statuses.push((id, status));
        }
        self.catalog = Catalog::build(&scan, settings, jit, |id| {
            statuses.iter().find(|(s, _)| s == id).map_or(
                Status::Failed("the plugin's worker stopped".into()),
                |(_, s)| s.clone(),
            )
        });
        for problem in &self.catalog.problems {
            tracing::warn!(dir = %problem.dir, error = %problem.error, "not a usable plugin");
        }
    }

    /// Retires every worker (each gets [`RETIRE_WAIT`] to let go) and loads again from a fresh
    /// scan. Returns the new catalog, which the caller resends to every window if it changed.
    pub fn reload(&mut self) -> &Catalog {
        self.retire_all();
        self.load();
        &self.catalog
    }

    fn retire_all(&mut self) {
        let deadline = Instant::now() + RETIRE_WAIT;
        let pending: Vec<_> = self
            .workers
            .iter_mut()
            .map(|w| (w.id.clone(), w.retire(deadline)))
            .collect();
        for (id, pending) in pending {
            if pending.wait() == Err(Late::Timeout) {
                tracing::warn!(plugin = %id, "plugin worker didn't retire in time");
            }
        }
        self.workers.clear();
    }

    /// The catalog every replica receives.
    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    /// Effects and failure toasts from every worker, for the daemon's loop to route.
    pub fn outputs(&self) -> &Receiver<HostOutput> {
        &self.rx
    }

    /// Sends a pane event to every Ready worker. Never blocks.
    pub fn dispatch(&self, event: HostEvent) {
        let event = Arc::new(event);
        for worker in &self.workers {
            worker.send_event(Arc::clone(&event));
        }
    }

    /// Runs the spawn filter: plugins with a `spawn` handler, in id order, each given the
    /// previous result and at most [`SPAWN_WAIT`] from when its request is queued. A late answer
    /// is discarded and the spec stays as it was before that plugin (fail open).
    pub fn filter_spawn(&self, mut spec: SpawnSpec) -> SpawnSpec {
        for worker in self.workers.iter().filter(|w| w.has_spawn()) {
            let pending = worker.request_spawn(spec.clone(), Instant::now() + SPAWN_WAIT);
            match pending.wait() {
                Ok(filtered) => spec = filtered,
                Err(Late::Timeout) => {
                    tracing::warn!(plugin = %worker.id, "plugin spawn filter took over 50 ms; skipped")
                }
                Err(Late::Stopped) => {}
            }
        }
        spec
    }
}

impl Drop for PluginHost {
    fn drop(&mut self) {
        // Workers exit when they see the request; nobody waits for them here.
        let deadline = Instant::now();
        for worker in &mut self.workers {
            drop(worker.retire(deadline));
        }
    }
}
