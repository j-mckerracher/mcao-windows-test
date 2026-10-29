//! The boundary the lifecycle probe drives, and the failures a backend reports across it.

use crate::evidence::{Execution, Outcome};

/// Why a backend could not perform an operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpError {
    Unsupported(String),
    Failed(String),
}

impl From<OpError> for Outcome {
    fn from(error: OpError) -> Self {
        match error {
            OpError::Unsupported(reason) => Outcome::Unsupported { reason },
            OpError::Failed(reason) => Outcome::Failed { reason },
        }
    }
}

/// Agent state as seen through the backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentState {
    Running,
    Idle,
    Missing,
}

impl AgentState {
    /// The lowercase name used in evidence output.
    pub fn as_str(self) -> &'static str {
        match self {
            AgentState::Running => "running",
            AgentState::Idle => "idle",
            AgentState::Missing => "missing",
        }
    }
}

/// What `interrupt` means on a backend. The lifecycle probe judges the interrupt step by it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InterruptSemantics {
    /// One Escape stops the running turn, and the session stays up. The agent must leave
    /// Running, settle Idle, and print the stop marker for the step to pass.
    StopTurn,
    /// The backend ends the worker within its own bounded wait, and terminates the worker's job
    /// if the worker has not stopped. A session that is Missing afterward is a stop.
    StopOrKill,
}

/// The boundary the probe drives. Platform backends implement it. The probe itself
/// never starts a shell or a command-line program.
pub trait SessionBackend {
    /// The backend name recorded in each observation.
    fn name(&self) -> &'static str;
    /// Whether this backend runs natively or is a mock.
    fn execution(&self) -> Execution;
    /// Starts a session for `harness` and returns its handle.
    fn spawn(&mut self, harness: &str) -> Result<String, OpError>;
    /// Returns the text currently visible in the session behind `handle`.
    fn capture(&self, handle: &str) -> Result<String, OpError>;
    /// Sends `text` to the session behind `handle`.
    fn send_text(&mut self, handle: &str, text: &str) -> Result<(), OpError>;
    /// Interrupts the session behind `handle`.
    fn interrupt(&mut self, handle: &str) -> Result<(), OpError>;
    /// What `interrupt` means on this backend. The default is a turn stop.
    fn interrupt_semantics(&self) -> InterruptSemantics {
        InterruptSemantics::StopTurn
    }
    /// Reports the agent state of the session behind `handle`.
    fn agent_state(&self, handle: &str) -> Result<AgentState, OpError>;
    /// Reattaches to the session behind `handle`.
    fn reconnect(&self, handle: &str) -> Result<(), OpError>;
    /// Releases the session behind `handle`.
    fn cleanup(&mut self, handle: &str) -> Result<(), OpError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_errors_keep_their_reason_in_the_outcome() {
        assert_eq!(
            Outcome::from(OpError::Unsupported("no pty".into())),
            Outcome::Unsupported {
                reason: "no pty".into()
            }
        );
        assert_eq!(
            Outcome::from(OpError::Failed("stuck".into())),
            Outcome::Failed {
                reason: "stuck".into()
            }
        );
    }
}
