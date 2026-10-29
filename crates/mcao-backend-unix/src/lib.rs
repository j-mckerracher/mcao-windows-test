//! `mcao-backend-unix`: PROPOSED crate name, not yet confirmed by DPES-752.
//!
//! Unix session primitives for the mcao Rust slice (DPES-756, slice A). [`tmux`] reads a tmux
//! pane and reports whether a session exists. [`holder`] implements the core `HolderProbe`
//! contract with `lsof`. Both run their program with argv and no shell. The herdr backend, the
//! backend selector, and the shell adapters arrive in later slices. The whole crate is compiled
//! out on other platforms.

#![cfg(unix)]

pub mod holder;
pub mod tmux;

pub use holder::UnixHolderProbe;
pub use tmux::TmuxBackend;
