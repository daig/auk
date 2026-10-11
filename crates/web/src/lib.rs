//! The browser client and `auk web serve`.
//!
//! - **Client** (wasm32, `client`): a follower replica per pane over the replication protocol
//!   carried by a WebSocket, the grid drawn with `auk-render` over WebGPU and glyphs
//!   rasterized by swash, keys encoded with `auk_term::input`. It runs no Lua: plugin
//!   state reaches it through the catalog, lens views and effects. One host
//!   only: the serving machine's. A minimal tab bar (page DOM) switches tabs.
//! - **Server** (native, [`serve`]): `auk web serve` serves the client's files and bridges the
//!   WebSocket to the daemon socket, behind a one-time URL token traded for a session cookie.
//! - [`session`] and [`keys`] are the client's protocol state and key mapping, and [`pane`] how
//!   it draws a pane's surfaces, plugin blocks and command blocks with their lens views (with
//!   the catalog's plugin styles) and routes clicks to them; all pure and tested natively.
//! - [`replay`] drives replicas through recorded streams, the same code natively and as wasm
//!   (the `replay` example, built for `wasm32-wasip1`), for the determinism check.

pub mod keys;
pub mod pane;
pub mod replay;
pub mod session;

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod client;

#[cfg(not(target_arch = "wasm32"))]
pub mod serve;
