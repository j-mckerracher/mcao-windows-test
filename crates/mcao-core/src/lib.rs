//! `mcao-core`: PROPOSED crate name, not yet confirmed by DPES-752.
//!
//! Platform-neutral core of the mcao Rust slice. [`backend`] is the boundary the lifecycle
//! probe drives, [`evidence`] is the record each operation emits, and [`error`] holds the
//! failure and exit-code contract. [`home`] and [`config`] resolve the operational home,
//! [`herdr`] holds the herdr control-socket wire and `eventwait`, [`resources`] and [`util`] hold
//! helpers, and [`state`] and [`lock`] wait for later slices. [`windows`] provides Windows
//! install-support helpers.

pub mod arm_shell_forms;
pub mod arm_verdict;
pub mod assignment_profile;
pub mod assignment_reassess;
pub mod attach;
pub mod audit_activity;
pub mod audit_envelope;
pub mod audit_hmac;
pub mod audit_path;
pub mod audit_redact_pii;
pub mod audit_redact_tokens;
pub mod audit_sanitize;
pub mod audit_writer;
pub mod backend;
pub mod canonical_json;
pub mod cd_policy;
pub mod cloud_meta;
pub mod config;
pub mod console;
pub mod env_expand;
pub mod error;
pub mod evidence;
pub mod governance;
pub mod herdr;
pub mod home;
pub mod hook_source;
pub mod install_manifest;
pub mod lock;
pub mod mail_poll;
pub mod merge_gate;
pub mod notify;
pub mod path_edit;
pub mod peek;
pub mod preflight;
pub mod proc_info;
pub mod process_identity;
pub mod resources;
pub mod routing;
pub mod sha256;
pub mod shell_command_pos;
pub mod staleness;
pub mod state;
pub mod steer;
pub mod teardown_guard;
pub mod terminal;
pub mod toolchain;
pub mod util;
pub mod windows;

pub use backend::{AgentState, InterruptSemantics, OpError, SessionBackend};
pub use error::McaoError;
pub use evidence::{EvidenceRecord, Execution, Observation, Operation, Outcome, Support, classify};
