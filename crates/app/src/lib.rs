//! The macOS app (spec §19, §22): windows, the sessions UI, input routing and the window-half
//! plugin host.
//!
//! The app proper talks to the daemon and arrives with M4. Until then this crate carries the
//! development harnesses for spikes S1 and S2 under `examples/`: `local_shell` (a window on a
//! local PTY, end to end through the platform layer, the terminal core and the grid renderer)
//! and `keyprobe` (the bytes keystrokes produce, in Auk and in a reference terminal).
