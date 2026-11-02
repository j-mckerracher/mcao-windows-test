//! Herdr control-socket support (DPES-864, slices D and E).
//!
//! [`wire`] holds the newline-delimited JSON protocol as pure functions with no I/O. [`eventwait`]
//! runs one bounded `pane.agent_status_changed` subscription over the Unix control socket and
//! reports the explicit unsupported result on Windows. [`workspace_move`] sends one
//! `workspace.move` request the same way. Both share the bounded connect and line reader in the
//! private `control_socket` module. Nothing here starts, stops, or drives a herdr session.

#[cfg(unix)]
mod control_socket;
pub mod eventwait;
pub mod wire;
pub mod workspace_move;
