//! The restart gate that decides what recovery may do with a task, from the evidence about its
//! recorded worker process.
//!
//! The gate is pure. It reads a [`ProcessEvidence`] verdict and returns a [`RecoveryDecision`]. It
//! never signals, terminates, or reattaches to a process, so a refusal leaves every process and
//! every record untouched.

use crate::process_identity::ProcessEvidence;

/// What a restart may do with a task, given the evidence about its recorded worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryDecision {
    /// The worker is gone, so recovery may reconcile the task.
    Reconcile,
    /// The worker is running, so recovery reattaches to it.
    Reattach,
    /// The evidence is ambiguous or unreadable. Recovery changes nothing and reports the state
    /// unresolved.
    Refuse,
}

/// Maps the verdict on a task's recorded worker to the action a restart may take.
///
/// Only `Dead` and `Missing` allow reconciliation, and only `Alive` allows reattach. `Ambiguous`
/// and `Unreadable` refuse, so a process that cannot be proven to be the recorded worker is never
/// reconciled or controlled.
pub fn recovery_decision(evidence: ProcessEvidence) -> RecoveryDecision {
    match evidence {
        ProcessEvidence::Dead | ProcessEvidence::Missing => RecoveryDecision::Reconcile,
        ProcessEvidence::Alive => RecoveryDecision::Reattach,
        ProcessEvidence::Ambiguous | ProcessEvidence::Unreadable => RecoveryDecision::Refuse,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_dead_and_missing_allow_reconciliation() {
        assert_eq!(
            recovery_decision(ProcessEvidence::Dead),
            RecoveryDecision::Reconcile
        );
        assert_eq!(
            recovery_decision(ProcessEvidence::Missing),
            RecoveryDecision::Reconcile
        );
    }

    #[test]
    fn only_alive_allows_reattach() {
        assert_eq!(
            recovery_decision(ProcessEvidence::Alive),
            RecoveryDecision::Reattach
        );
    }

    #[test]
    fn ambiguous_and_unreadable_evidence_refuses() {
        assert_eq!(
            recovery_decision(ProcessEvidence::Ambiguous),
            RecoveryDecision::Refuse
        );
        assert_eq!(
            recovery_decision(ProcessEvidence::Unreadable),
            RecoveryDecision::Refuse
        );
    }
}
