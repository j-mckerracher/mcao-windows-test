//! `mcao-backend-windows`: PROPOSED crate name, not yet confirmed by DPES-752.
//!
//! Windows-only prototype for DPES-752. Slice 752.B: a Job Object that owns a
//! process tree, and a spawn that places the child in that job before it runs.
//! Slice 752.C: a ConPTY terminal for that child, with output capture and text
//! input. Slice 752.D: an interrupt ladder that escalates from a single Escape to
//! `TerminateJobObject`, with stop evidence per rung. Slice 755.A adds [`backend`],
//! the `SessionBackend` adapter over a [`session::Session`]. The whole crate is compiled
//! out on other platforms. No lock-holder code exists yet.

#![cfg(windows)]

pub mod backend;
pub mod conpty;
mod ctrl_break;
pub mod interrupt;
pub mod job;
pub mod process;
pub mod session;
