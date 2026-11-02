//! Windows install-support helpers. Pure std, compiled on every platform so the tests run everywhere.
//! The user `PATH` registry editor ([`registry`]) keeps its Win32 calls behind `cfg(windows)`; the
//! reparse-tag probe arrives in a later slice.

pub mod onedrive;
pub mod registry;
pub mod retry;
