//! The Luau plugin runtime: manifests, discovery and the catalog; lens claims and
//! capture; sandboxed Luau VMs with per-call budgets, the 256 MiB cap and native codegen; one
//! worker thread per host half running hooks, blocks and lenses; the window half's runtime; the
//! `tern.*` API surface (with `auk` as its alias); reloading on change; and the backing functions
//! of the `auk plugin` verbs.
//!
//! - [`manifest`], [`discovery`], [`catalog`], [`claim`], [`capture`] and [`manage`] are plain
//!   data and build everywhere: replicas receive the catalog and make lens claims and captures
//!   themselves, without Lua. The rest needs the default `runtime` feature.
//! - [`Vm`] is one plugin VM.
//! - [`PluginHost`] runs the host halves for the daemon: it loads plugins, builds the
//!   [`Catalog`], dispatches [`HostEvent`]s, runs the spawn filter, runs blocks
//!   ([`PluginHost::start_block`]) and lens captures ([`PluginHost::lens_open`]), and hands
//!   worker output ([`HostOutput`]) back over a channel.
//! - [`WindowPlugins`] runs the window halves of one window on its UI thread, behind the
//!   [`WindowHost`] trait the app implements.
//! - [`Watcher`] reloads on change (FSEvents, inotify, or a 2 s poll).
//! - [`jit::probe`] checks that Luau's native code really runs in this process (`auk doctor`).

pub mod capture;
pub mod catalog;
pub mod chord;
pub mod claim;
pub mod discovery;
pub mod events;
pub mod icons;
pub mod manage;
pub mod manifest;
pub mod paths;
pub mod spawn;

#[cfg(feature = "runtime")]
pub mod api;
#[cfg(feature = "runtime")]
mod blocks;
#[cfg(feature = "runtime")]
mod budget;
#[cfg(feature = "runtime")]
pub mod css_url;
#[cfg(feature = "runtime")]
pub mod diagnostics;
#[cfg(feature = "runtime")]
mod host;
#[cfg(feature = "runtime")]
pub mod jit;
#[cfg(feature = "runtime")]
mod lenses;
#[cfg(feature = "runtime")]
mod require;
#[cfg(feature = "runtime")]
mod vm;
#[cfg(feature = "runtime")]
mod watch;
#[cfg(feature = "runtime")]
pub mod window;
#[cfg(feature = "runtime")]
mod worker;

pub use catalog::{Catalog, Status};
pub use claim::{Claim, Command, LensRun, Preconditions};
pub use discovery::{Package, PluginSettings, Problem, Scan};
pub use events::{HostEvent, PaneDirectory, PaneId, PaneInfo, PaneTable};
pub use manifest::Manifest;
pub use spawn::{SpawnPatch, SpawnSpec};

#[cfg(feature = "runtime")]
pub use api::block::BlockRequest;
#[cfg(feature = "runtime")]
pub use api::effects::{Effect, EffectTarget, ToastLevel};
#[cfg(feature = "runtime")]
pub use api::tasks::{Callback, Completer, Wake};
#[cfg(feature = "runtime")]
pub use blocks::{BlockOutput, BlockRecord};
#[cfg(feature = "runtime")]
pub use budget::Budget;
#[cfg(feature = "runtime")]
pub use host::{BlockLaunch, HostConfig, LENS_OWNERS, PluginHost, RETIRE_WAIT, SPAWN_WAIT};
#[cfg(feature = "runtime")]
pub use lenses::{LensId, LensRecord, MAX_CAPTURES, VIEW_INTERVAL};
#[cfg(feature = "runtime")]
pub use mlua;
#[cfg(feature = "runtime")]
pub use vm::{
    CallError, Context, LogLevel, LogRecord, LogSink, MEMORY_LIMIT, PluginInfo, Vm, VmOptions,
    codegen_supported, tracing_log_sink,
};
#[cfg(feature = "runtime")]
pub use watch::Watcher;
#[cfg(feature = "runtime")]
pub use window::{WindowHost, WindowPlugins};
#[cfg(feature = "runtime")]
pub use worker::{EffectOutput, HostOutput, Late, Pending, Waker};
