//! The Luau plugin runtime (§9, §20): manifests, discovery and the catalog; sandboxed Luau VMs
//! with per-call budgets, the 256 MiB cap and native codegen; one worker thread per host half;
//! and the `tern.*` API surface (with `auk` as its alias).
//!
//! - [`manifest`], [`discovery`] and [`catalog`] are plain data and build everywhere (the
//!   catalog is what replicas receive). The rest needs the default `runtime` feature.
//! - [`Vm`] is one plugin VM. A window half builds one per plugin on its UI thread.
//! - [`PluginHost`] runs the host halves for the daemon: it loads plugins, builds the
//!   [`Catalog`], dispatches [`HostEvent`]s, runs the spawn filter, and hands worker output
//!   ([`HostOutput`]) back over a channel.

pub mod catalog;
pub mod discovery;
pub mod events;
pub mod icons;
pub mod manifest;
pub mod paths;
pub mod spawn;

#[cfg(feature = "runtime")]
pub mod api;
#[cfg(feature = "runtime")]
mod budget;
#[cfg(feature = "runtime")]
mod host;
#[cfg(feature = "runtime")]
mod require;
#[cfg(feature = "runtime")]
mod vm;
#[cfg(feature = "runtime")]
mod worker;

pub use catalog::{Catalog, Status};
pub use discovery::{Package, PluginSettings, Problem, Scan};
pub use events::{HostEvent, PaneId};
pub use manifest::Manifest;
pub use spawn::{SpawnPatch, SpawnSpec};

#[cfg(feature = "runtime")]
pub use api::effects::{Effect, EffectTarget, ToastLevel};
#[cfg(feature = "runtime")]
pub use budget::Budget;
#[cfg(feature = "runtime")]
pub use host::{HostConfig, PluginHost, RETIRE_WAIT, SPAWN_WAIT};
#[cfg(feature = "runtime")]
pub use mlua;
#[cfg(feature = "runtime")]
pub use vm::{
    CallError, Context, LogLevel, LogRecord, LogSink, MEMORY_LIMIT, PluginInfo, Vm, VmOptions,
    codegen_supported, tracing_log_sink,
};
#[cfg(feature = "runtime")]
pub use worker::{HostOutput, Late, Pending, Waker};
