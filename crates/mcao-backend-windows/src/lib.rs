//! `mcao-backend-windows`: PROPOSED crate name, not yet confirmed by DPES-752.
//!
//! Windows-only prototype for DPES-752. Slice 752.B: a Job Object that owns a
//! process tree, and a spawn that places the child in that job before it runs.
//! Slice 752.C: a ConPTY terminal for that child, with output capture and text
//! input. Slice 752.D: an interrupt ladder that escalates from a single Escape to
//! `TerminateJobObject`, with stop evidence per rung. Slice 755.A adds [`backend`],
//! the `SessionBackend` adapter over a [`session::Session`]. Every module except [`pipe`] and
//! [`console_server`] is compiled out on other platforms. [`pipe`] and [`console_server`] build
//! everywhere, with their Windows-only parts gated, so their pure logic runs in host tests. No
//! lock-holder code exists yet.

pub mod console_server;
pub mod pipe;

#[cfg(windows)]
pub mod backend;
#[cfg(windows)]
pub mod conpty;
#[cfg(windows)]
mod ctrl_break;
#[cfg(windows)]
pub mod interrupt;
#[cfg(windows)]
pub mod job;
#[cfg(windows)]
pub mod process;
#[cfg(windows)]
pub mod session;
#[cfg(windows)]
mod window;
