//! Refused spawns in the lifecycle driver (DPES-752 F6).
//!
//! A spawn that yields no session must still produce all seven records. The spawn carries the
//! refusal, and each later step is recorded as not attempted, never as support.

use mcao_core::{
    AgentState, EvidenceRecord, Execution, OpError, Operation, Outcome, SessionBackend, Support,
};
use mcao_probe::args::Scenario;
use mcao_probe::lifecycle::run_lifecycle;
use mcao_probe::mock::MockBackend;

const ORDER: [Operation; 7] = [
    Operation::Spawn,
    Operation::Capture,
    Operation::SendText,
    Operation::Interrupt,
    Operation::AgentState,
    Operation::Reconnect,
    Operation::Cleanup,
];

const NOT_ATTEMPTED: &str = "not attempted: spawn produced no session";

/// Native-declaring backend whose spawn always refuses. The other operations panic if the
/// driver reaches them, because no session exists after a refused spawn. Simulated: it proves
/// only how the driver records a refusal.
struct RefusedSpawn(OpError);

impl SessionBackend for RefusedSpawn {
    fn name(&self) -> &'static str {
        "refused-spawn"
    }

    fn execution(&self) -> Execution {
        Execution::Native
    }

    fn spawn(&mut self, _harness: &str) -> Result<String, OpError> {
        Err(self.0.clone())
    }

    fn capture(&self, _handle: &str) -> Result<String, OpError> {
        unreachable!("capture after a refused spawn")
    }

    fn send_text(&mut self, _handle: &str, _text: &str) -> Result<(), OpError> {
        unreachable!("send_text after a refused spawn")
    }

    fn interrupt(&mut self, _handle: &str) -> Result<(), OpError> {
        unreachable!("interrupt after a refused spawn")
    }

    fn agent_state(&self, _handle: &str) -> Result<AgentState, OpError> {
        unreachable!("agent_state after a refused spawn")
    }

    fn reconnect(&self, _handle: &str) -> Result<(), OpError> {
        unreachable!("reconnect after a refused spawn")
    }

    fn cleanup(&mut self, _handle: &str) -> Result<(), OpError> {
        unreachable!("cleanup after a refused spawn")
    }
}

fn assert_refused_spawn_run(
    records: &[EvidenceRecord],
    spawn_outcome: Outcome,
    spawn_support: Support,
) {
    assert_eq!(records.len(), 7, "every operation is recorded");
    let operations: Vec<Operation> = records
        .iter()
        .map(|record| record.observation().operation)
        .collect();
    assert_eq!(operations, ORDER);

    let spawn = &records[0];
    assert_eq!(spawn.observation().outcome, spawn_outcome);
    assert_eq!(spawn.support(), spawn_support);
    assert_eq!(spawn.observation().handle, None);

    for record in &records[1..] {
        assert_eq!(record.observation().handle, None);
        assert_eq!(
            record.observation().outcome,
            Outcome::Failed {
                reason: NOT_ATTEMPTED.to_owned()
            }
        );
        assert_eq!(record.support(), Support::Unresolved);
    }
}

#[test]
fn unsupported_harness_refuses_spawn_and_leaves_later_steps_not_attempted() {
    let mut backend = MockBackend::new(Scenario::Cooperative);
    let records = run_lifecycle(&mut backend, "other-harness");
    assert_refused_spawn_run(
        &records,
        Outcome::Unsupported {
            reason: "mock backend runs only the `mock` harness".to_owned(),
        },
        Support::Unsupported,
    );
}

#[test]
fn unsupported_native_spawn_is_unsupported_not_unresolved() {
    let mut backend = RefusedSpawn(OpError::Unsupported("no terminal on this host".to_owned()));
    let records = run_lifecycle(&mut backend, "simulated");
    assert_refused_spawn_run(
        &records,
        Outcome::Unsupported {
            reason: "no terminal on this host".to_owned(),
        },
        Support::Unsupported,
    );
}

#[test]
fn failed_native_spawn_is_unresolved_and_later_steps_are_not_attempted() {
    let mut backend = RefusedSpawn(OpError::Failed(
        "launcher exited before a session existed".to_owned(),
    ));
    let records = run_lifecycle(&mut backend, "simulated");
    assert_refused_spawn_run(
        &records,
        Outcome::Failed {
            reason: "launcher exited before a session existed".to_owned(),
        },
        Support::Unresolved,
    );
}
