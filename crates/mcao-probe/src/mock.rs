//! Deterministic in-memory backend for the mock-only probe run.
//!
//! Nothing here starts a process or touches a terminal. It lets the probe's evidence
//! shape, operation order, and classification be tested before any native backend exists.

use std::collections::BTreeMap;

use mcao_core::{AgentState, Execution, OpError, SessionBackend};

use crate::args::Scenario;

/// The only harness this slice knows.
pub const HARNESS: &str = "mock";

#[derive(Debug)]
struct Session {
    state: AgentState,
    output: String,
}

#[derive(Debug)]
pub struct MockBackend {
    scenario: Scenario,
    sessions: BTreeMap<String, Session>,
    next_id: u64,
}

impl MockBackend {
    pub fn new(scenario: Scenario) -> Self {
        Self {
            scenario,
            sessions: BTreeMap::new(),
            next_id: 1,
        }
    }

    fn session(&self, handle: &str) -> Result<&Session, OpError> {
        self.sessions.get(handle).ok_or_else(|| missing(handle))
    }

    fn session_mut(&mut self, handle: &str) -> Result<&mut Session, OpError> {
        self.sessions.get_mut(handle).ok_or_else(|| missing(handle))
    }
}

fn missing(handle: &str) -> OpError {
    OpError::Failed(format!("no session `{handle}`"))
}

impl SessionBackend for MockBackend {
    fn name(&self) -> &'static str {
        "mock"
    }

    fn execution(&self) -> Execution {
        Execution::Mock
    }

    fn spawn(&mut self, harness: &str) -> Result<String, OpError> {
        if harness != HARNESS {
            return Err(OpError::Unsupported(format!(
                "mock backend runs only the `{HARNESS}` harness"
            )));
        }
        let handle = format!("mock-{}", self.next_id);
        self.next_id += 1;
        self.sessions.insert(
            handle.clone(),
            Session {
                state: AgentState::Idle,
                output: "mock harness ready\n".to_owned(),
            },
        );
        Ok(handle)
    }

    fn capture(&self, handle: &str) -> Result<String, OpError> {
        Ok(self.session(handle)?.output.clone())
    }

    fn send_text(&mut self, handle: &str, text: &str) -> Result<(), OpError> {
        let session = self.session_mut(handle)?;
        session.output.push_str(&format!("> {text}\n"));
        session.state = AgentState::Running;
        Ok(())
    }

    /// Sends one Escape. A cooperative mock stops its running turn. An uncooperative mock
    /// ignores the key. The probe judges the stop from `agent_state`, not from this return.
    fn interrupt(&mut self, handle: &str) -> Result<(), OpError> {
        let cooperative = self.scenario == Scenario::Cooperative;
        let session = self.session_mut(handle)?;
        if cooperative && session.state == AgentState::Running {
            session.state = AgentState::Idle;
            session.output.push_str("[turn interrupted]\n");
        }
        Ok(())
    }

    fn agent_state(&self, handle: &str) -> Result<AgentState, OpError> {
        Ok(self.session(handle)?.state)
    }

    fn reconnect(&self, handle: &str) -> Result<(), OpError> {
        self.session(handle).map(|_| ())
    }

    fn cleanup(&mut self, handle: &str) -> Result<(), OpError> {
        self.sessions
            .remove(handle)
            .map(|_| ())
            .ok_or_else(|| missing(handle))
    }
}
