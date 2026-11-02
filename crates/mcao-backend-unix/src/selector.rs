//! Picks a Unix session backend by name, as `config/backend` and the `mcao` CLI spell them.
//!
//! [`backend_for`] only builds a backend. It opens no session and starts no worker, so an unknown
//! name fails before anything runs.

use std::fmt;

use mcao_core::backend::{AgentState, OpError, SessionBackend};
use mcao_core::evidence::Execution;

use crate::herdr::HerdrBackend;
use crate::tmux::TmuxBackend;

/// Why [`backend_for`] could not pick a backend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SelectorError {
    /// The name matches no Unix backend.
    UnknownBackend(String),
}

impl fmt::Display for SelectorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownBackend(name) => write!(f, "unknown backend {name:?}"),
        }
    }
}

impl std::error::Error for SelectorError {}

/// Builds a backend by name, trimming the surrounding whitespace that `config/backend` may carry.
/// The match is case-sensitive, as the shell's `fm_backend_is_known` is.
pub fn backend_for(name: &str) -> Result<Box<dyn SessionBackend>, SelectorError> {
    let name = name.trim();
    match name {
        "tmux" => Ok(Box::new(TmuxBackend::new())),
        "herdr" => Ok(Box::new(HerdrBackend::from_env())),
        other => Err(SelectorError::UnknownBackend(other.to_owned())),
    }
}

fn not_implemented(operation: &str) -> OpError {
    OpError::Unsupported(format!(
        "the Rust tmux backend does not implement {operation} yet (DPES-756)"
    ))
}

/// Only `name`, `execution`, and `reconnect` are real in this slice. The other operations need
/// work outside it: `capture` has no line count in the trait, and `send_text` must not be
/// confused with the verified submit that [`TmuxBackend::send_text_submit`] performs.
impl SessionBackend for TmuxBackend {
    fn name(&self) -> &'static str {
        "tmux"
    }

    fn execution(&self) -> Execution {
        Execution::Native
    }

    fn spawn(&mut self, _harness: &str) -> Result<String, OpError> {
        Err(not_implemented("spawn"))
    }

    fn capture(&self, _handle: &str) -> Result<String, OpError> {
        Err(not_implemented("capture"))
    }

    fn send_text(&mut self, _handle: &str, _text: &str) -> Result<(), OpError> {
        Err(not_implemented("send_text"))
    }

    fn interrupt(&mut self, _handle: &str) -> Result<(), OpError> {
        Err(not_implemented("interrupt"))
    }

    fn agent_state(&self, _handle: &str) -> Result<AgentState, OpError> {
        Err(not_implemented("agent_state"))
    }

    fn reconnect(&self, handle: &str) -> Result<(), OpError> {
        match self.session_exists(handle) {
            Ok(true) => Ok(()),
            Ok(false) => Err(OpError::Failed(format!("no tmux session {handle:?}"))),
            Err(error) => Err(OpError::Failed(error.to_string())),
        }
    }

    fn cleanup(&mut self, _handle: &str) -> Result<(), OpError> {
        Err(not_implemented("cleanup"))
    }
}
