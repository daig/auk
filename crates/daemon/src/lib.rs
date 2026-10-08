//! The session daemon (spec §14, §15): one per user per machine (§4.1). It owns the pane table
//! and each pane's PTY and authoritative replica, the sessions and layout, the attached clients
//! with the primary-replica policy, zsh integration, and the control endpoint.
//!
//! - [`run`] serves a daemon socket (the replication protocol of `auk-mirror`) and, beside it,
//!   the line-based control endpoint ([`commands`]).
//! - [`paths`] says where the sockets, state and logs live.
//! - [`spawn`] starts a detached daemon for a client that found none.
//!
//! The pane table is built for the state file (§16, M6): [`pane::Pane::record`] gives each
//! pane's kind, cwd and window with its snapshot, and the layout serializes as it is.

pub mod client;
pub mod commands;
pub mod control;
pub mod login;
pub mod pane;
pub mod pane_socket;
pub mod paths;
pub mod pty;
pub mod server;
pub mod spawn;
pub mod zsh;

pub use server::{Config, Daemon, NewPane, Place, run, serve};
