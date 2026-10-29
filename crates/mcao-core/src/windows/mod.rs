//! Windows install-support helpers. Pure std, compiled on every platform so the tests run everywhere; the Windows-only API calls (registry, reparse-tag probe) arrive in later slices.

pub mod onedrive;
pub mod retry;
