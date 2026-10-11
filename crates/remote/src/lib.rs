//! Remote hosts: panes on another machine, reached over SSH.
//!
//! - **Bridge** ([`bridge`]): `auk attach --stdio` runs on the remote host. It starts that host's
//!   daemon if none answers, writes one preamble line naming the host and user, then copies
//!   bytes between its stdio and the daemon socket. The replication protocol runs over
//!   it unchanged.
//! - **SSH** ([`ssh`]): the local side runs `ssh <host> auk attach --stdio` with the system
//!   `ssh`, so authentication, `~/.ssh/config` and `known_hosts` are SSH's own. It reads
//!   `ssh -v`'s report of how it authenticated for the identity chain.
//! - **Link** ([`link`]): a mirror connection over a local socket or SSH that windows use in
//!   place of a bare socket. Over SSH it reconnects after the link drops and resyncs from fresh
//!   snapshots, holding typing until the resync completes, and reports the connection
//!   state: connecting, connected, stalled, reconnecting (attempt n) or failed.
//! - **Identity** ([`identity`]): the host's chain of hops (principal, method, origin machine,
//!   acting user); an SSH link shows an `ssh-key` hop.
//!
//! QUIC and predictive echo are later.

pub mod bridge;
pub mod identity;
pub mod link;
pub mod ssh;

pub use bridge::BridgeInfo;
pub use identity::Hop;
pub use link::{Delivery, Link, LinkError, LinkReader, State, Status, Target};
pub use ssh::SshTarget;
