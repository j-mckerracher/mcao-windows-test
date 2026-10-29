//! Windows governance-toolchain qualification (DPES-890).
//!
//! [`qualify`] probes the governance tools and returns an [`Evidence`] record. [`check_required_tools`]
//! is the refusal a delivery path applies to that record. Every probe reaches the machine through
//! [`Host`], so the logic runs against a fake host in tests. Probes start only resolved executables
//! with argv: `cmd`, `sh`, `powershell`, and `npm` are never started, and `.cmd`, `.bat`, and `.ps1`
//! shims are reported as rejected and never run.

mod discovery;
mod host;
mod npm;
mod preflight;
mod probe;
mod record;

pub use host::{Host, Platform, RunOutput};
pub use preflight::{ALL_SUPPORTED_MESSAGE, check_required_tools, required_tools_for};
pub use probe::{Evidence, GhAuth, RunContext, qualify};
pub use record::{ToolRecord, Verdict};
