//! `mcao-probe`: PROPOSED crate name, not yet confirmed by DPES-752.
//!
//! Lifecycle probe. It drives a `SessionBackend` through the seven DPES-752 operations and emits one
//! `EvidenceRecord` per operation. The mock backend is deterministic and records `mock_only`. The
//! native backend starts the stub harness as a real child process; it is never a real agent harness,
//! so nothing here claims that Claude Code or any pilot harness was exercised.

pub mod args;
pub mod lifecycle;
pub mod mock;
pub mod native;
pub mod redact;
pub mod stub;

// `mcao-backend-unix` is an empty skeleton, so no code calls it. It stays declared for this target so
// the leakage test can check the dependency graph. `mcao-backend-windows` is used by `native`.
#[cfg(unix)]
use mcao_backend_unix as _;
