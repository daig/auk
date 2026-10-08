//! The browser client and `auk web serve` (spec §10.3, §10.4, §12.5, M11).
//!
//! - **Client** (wasm32, `client`): a follower replica per pane over the replication protocol
//!   (§15.6) carried by a WebSocket, the grid drawn with `auk-render` over WebGPU and glyphs
//!   rasterized by swash (§18.3), keys encoded with `auk_term::input`. It runs no Lua: plugin
//!   state reaches it only through the catalog (§10.4). One host only: the serving machine's.
//! - **Server** (native, [`serve`]): `auk web serve` serves the client's files and bridges the
//!   WebSocket to the daemon socket, behind a one-time URL token traded for a session cookie.
//! - [`session`] and [`keys`] are the client's protocol state and key mapping, pure and tested
//!   natively.
//! - [`replay`] drives replicas through recorded streams, the same code natively and as wasm
//!   (the `replay` example, built for `wasm32-wasip1`), for the determinism check (AC2, S4).

pub mod keys;
pub mod replay;
pub mod session;

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod client;

#[cfg(not(target_arch = "wasm32"))]
pub mod serve;
