//! Platform notification surface: opening URIs and paths, and posting the daemon wedge alarm.
//!
//! [`opener`] and [`daemon_visibility`] define the traits the CLI calls. Each platform supplies
//! the implementation. Only Unix has one so far; the Windows channels wait for DPES-752.

pub mod daemon_visibility;
pub mod opener;
#[cfg(unix)]
pub mod unix;
