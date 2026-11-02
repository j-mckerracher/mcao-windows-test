//! `mcao-backend-unix`: PROPOSED crate name, not yet confirmed by DPES-752.
//!
//! Unix session primitives for the mcao Rust slice (DPES-756). [`tmux`] reads a tmux pane and
//! reports whether a session exists. [`herdr`] reads an agent's state from the Herdr control
//! socket. [`holder`] implements the core `HolderProbe` contract with `lsof`. [`selector`] builds
//! a backend by name. The tmux and holder paths run their program with argv and no shell. The
//! shell adapters arrive in later slices. The whole crate is compiled out on other platforms.

#![cfg(unix)]

pub mod herdr;
pub mod holder;
pub mod selector;
pub mod tmux;

pub use herdr::HerdrBackend;
pub use holder::UnixHolderProbe;
pub use selector::{SelectorError, backend_for};
pub use tmux::TmuxBackend;
