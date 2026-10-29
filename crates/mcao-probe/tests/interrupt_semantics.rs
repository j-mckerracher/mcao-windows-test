//! The interrupt step judges a session by the semantics its backend declares.
//!
//! A backend whose session ends on interrupt passes the step only when it declares
//! stop-or-kill. The same session under the default turn-stop semantics still fails, so the
//! criterion is not loosened for every backend.

use std::cell::Cell;

use mcao_core::{
    AgentState, EvidenceRecord, Execution, InterruptSemantics, OpError, Operation, Outcome,
};
use mcao_probe::lifecycle::run_lifecycle;

const HARNESS: &str = "ends-on-interrupt";

/// A backend whose one session is Running until an interrupt, then Missing, as a stop-or-kill
/// backend's session is once the worker has been ended.
struct EndsOnInterrupt {
    semantics: InterruptSemantics,
    running: Cell<bool>,
}

impl EndsOnInterrupt {
    fn new(semantics: InterruptSemantics) -> Self {
        Self {
            semantics,
            running: Cell::new(true),
        }
    }
}

impl mcao_core::SessionBackend for EndsOnInterrupt {
    fn name(&self) -> &'static str {
        "ends-on-interrupt"
    }

    fn execution(&self) -> Execution {
        Execution::Mock
    }

    fn spawn(&mut self, _harness: &str) -> Result<String, OpError> {
        Ok("ends-on-interrupt-1".to_owned())
    }

    fn capture(&self, _handle: &str) -> Result<String, OpError> {
        Ok(String::new())
    }

    fn send_text(&mut self, _handle: &str, _text: &str) -> Result<(), OpError> {
        Ok(())
    }

    fn interrupt(&mut self, _handle: &str) -> Result<(), OpError> {
        self.running.set(false);
        Ok(())
    }

    fn agent_state(&self, _handle: &str) -> Result<AgentState, OpError> {
        Ok(if self.running.get() {
            AgentState::Running
        } else {
            AgentState::Missing
        })
    }

    fn reconnect(&self, _handle: &str) -> Result<(), OpError> {
        Ok(())
    }

    fn cleanup(&mut self, _handle: &str) -> Result<(), OpError> {
        Ok(())
    }

    fn interrupt_semantics(&self) -> InterruptSemantics {
        self.semantics
    }
}

fn interrupt_record(records: &[EvidenceRecord]) -> &EvidenceRecord {
    records
        .iter()
        .find(|record| record.observation().operation == Operation::Interrupt)
        .expect("the interrupt step is recorded")
}

#[test]
fn a_stop_or_kill_backend_passes_the_interrupt_step_when_the_session_is_missing() {
    let mut backend = EndsOnInterrupt::new(InterruptSemantics::StopOrKill);
    let records = run_lifecycle(&mut backend, HARNESS);
    assert_eq!(
        interrupt_record(&records).observation().outcome,
        Outcome::Success
    );
}

#[test]
fn a_turn_stop_backend_still_fails_the_interrupt_step_when_the_session_is_missing() {
    let mut backend = EndsOnInterrupt::new(InterruptSemantics::StopTurn);
    let records = run_lifecycle(&mut backend, HARNESS);
    let outcome = &interrupt_record(&records).observation().outcome;
    assert!(
        matches!(outcome, Outcome::Failed { reason } if reason.contains("no stop observed")),
        "got {outcome:?}"
    );
}
