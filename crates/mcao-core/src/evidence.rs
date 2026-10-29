//! The evidence record each lifecycle operation emits, and how its outcome is classified.

use serde::Serialize;

/// A lifecycle operation the probe records. The order matches DPES-752 AC1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Spawn,
    Capture,
    SendText,
    Interrupt,
    AgentState,
    Reconnect,
    Cleanup,
}

/// Where an observation came from. Only `Native` can be classified as demonstrated.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Execution {
    Mock,
    Native,
}

/// What the backend reported for one operation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome {
    Success,
    /// The backend has no such operation.
    Unsupported {
        reason: String,
    },
    /// The operation ran and did not meet its contract.
    Failed {
        reason: String,
    },
}

/// Compatibility classification. `Demonstrated` requires native execution and success.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Support {
    Demonstrated,
    MockOnly,
    Unsupported,
    Unresolved,
}

/// Classifies one outcome by how it was executed.
pub fn classify(execution: Execution, outcome: &Outcome) -> Support {
    match (execution, outcome) {
        (Execution::Native, Outcome::Success) => Support::Demonstrated,
        (Execution::Mock, Outcome::Success) => Support::MockOnly,
        (_, Outcome::Unsupported { .. }) => Support::Unsupported,
        (_, Outcome::Failed { .. }) => Support::Unresolved,
    }
}

/// Facts about one attempted operation, before classification.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Observation {
    pub backend: String,
    pub harness: String,
    pub operation: Operation,
    pub execution: Execution,
    pub platform: String,
    pub outcome: Outcome,
    pub duration_ms: u64,
    pub timestamp_unix_ms: u64,
    pub handle: Option<String>,
    pub output_sample: Option<String>,
}

/// An observation plus its classification. The classification is derived in `new` and
/// nowhere else, so a record cannot claim more support than its evidence allows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EvidenceRecord {
    #[serde(flatten)]
    observation: Observation,
    support: Support,
}

impl EvidenceRecord {
    /// Wraps `observation` and derives its support from the observation's execution and outcome.
    pub fn new(observation: Observation) -> Self {
        let support = classify(observation.execution, &observation.outcome);
        Self {
            observation,
            support,
        }
    }

    /// The observed facts behind this record.
    pub fn observation(&self) -> &Observation {
        &self.observation
    }

    /// The support classification derived when the record was created.
    pub fn support(&self) -> Support {
        self.support
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn observation(execution: Execution, outcome: Outcome) -> Observation {
        Observation {
            backend: "mock".into(),
            harness: "mock-harness".into(),
            operation: Operation::Interrupt,
            execution,
            platform: "linux-x86_64".into(),
            outcome,
            duration_ms: 3,
            timestamp_unix_ms: 1000,
            handle: Some("mock-1".into()),
            output_sample: None,
        }
    }

    #[test]
    fn only_native_success_is_demonstrated() {
        assert_eq!(
            classify(Execution::Native, &Outcome::Success),
            Support::Demonstrated
        );
        assert_eq!(
            classify(Execution::Mock, &Outcome::Success),
            Support::MockOnly
        );
    }

    #[test]
    fn failures_are_never_demonstrated() {
        let failed = Outcome::Failed {
            reason: "escape did not stop".into(),
        };
        let unsupported = Outcome::Unsupported {
            reason: "no pty".into(),
        };
        assert_eq!(classify(Execution::Native, &failed), Support::Unresolved);
        assert_eq!(classify(Execution::Mock, &failed), Support::Unresolved);
        assert_eq!(
            classify(Execution::Native, &unsupported),
            Support::Unsupported
        );
    }

    #[test]
    fn record_serializes_observation_fields_and_support_flat() {
        let record = EvidenceRecord::new(observation(
            Execution::Mock,
            Outcome::Failed {
                reason: "stuck".into(),
            },
        ));
        let value = serde_json::to_value(&record).unwrap();
        assert_eq!(
            value,
            json!({
                "backend": "mock",
                "harness": "mock-harness",
                "operation": "interrupt",
                "execution": "mock",
                "platform": "linux-x86_64",
                "outcome": { "status": "failed", "reason": "stuck" },
                "duration_ms": 3,
                "timestamp_unix_ms": 1000,
                "handle": "mock-1",
                "output_sample": null,
                "support": "unresolved"
            })
        );
    }

    #[test]
    fn record_support_is_derived_at_construction() {
        let record = EvidenceRecord::new(observation(Execution::Mock, Outcome::Success));
        assert_eq!(record.support(), Support::MockOnly);
        assert_eq!(record.observation().operation, Operation::Interrupt);
    }
}
