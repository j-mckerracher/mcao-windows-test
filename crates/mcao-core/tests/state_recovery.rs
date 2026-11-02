//! Restart recovery for a task whose recorded worker may be stale or reused.
//!
//! Each case feeds what the platform observed about the recorded PID and job into
//! `classify_process`, then into `recovery_decision`. Only proven-gone workers reconcile, only a
//! proven-live worker reattaches, and every other case refuses so no unrelated process is touched.

use mcao_core::process_identity::{JobProbe, PidProbe, ProcessIdentity, classify_process};
use mcao_core::state::{RecoveryDecision, recovery_decision};

/// The identity recorded in the task's `TaskMeta` when the worker started.
const RECORDED: ProcessIdentity = ProcessIdentity {
    pid: 4242,
    start: 133_000_000_000_000_000,
};

/// The recorded PID, held by a process that started at the recorded creation time.
fn same_process(running: bool) -> PidProbe {
    PidProbe::Found {
        start: RECORDED.start,
        running,
    }
}

/// The recorded PID, now held by a later process that reused the number.
fn reused_pid() -> PidProbe {
    PidProbe::Found {
        start: RECORDED.start + 1,
        running: true,
    }
}

fn decide(pid: PidProbe, job: JobProbe) -> RecoveryDecision {
    recovery_decision(classify_process(RECORDED, pid, job))
}

#[test]
fn a_live_worker_in_its_job_reattaches() {
    assert_eq!(
        decide(same_process(true), JobProbe::Members(3)),
        RecoveryDecision::Reattach
    );
}

#[test]
fn an_exited_worker_with_an_empty_job_reconciles() {
    assert_eq!(
        decide(same_process(false), JobProbe::Members(0)),
        RecoveryDecision::Reconcile
    );
}

#[test]
fn a_worker_with_no_pid_and_no_job_reconciles_as_missing() {
    assert_eq!(
        decide(PidProbe::Absent, JobProbe::Absent),
        RecoveryDecision::Reconcile
    );
}

#[test]
fn a_reused_pid_with_an_empty_job_is_not_the_worker_and_reconciles() {
    assert_eq!(
        decide(reused_pid(), JobProbe::Members(0)),
        RecoveryDecision::Reconcile
    );
}

#[test]
fn a_reused_pid_with_live_job_members_refuses_so_nothing_is_controlled() {
    assert_eq!(
        decide(reused_pid(), JobProbe::Members(5)),
        RecoveryDecision::Refuse
    );
}

#[test]
fn a_live_pid_outside_its_job_refuses_because_identity_is_unproven() {
    assert_eq!(
        decide(same_process(true), JobProbe::Absent),
        RecoveryDecision::Refuse
    );
}

#[test]
fn unreadable_pid_or_job_refuses() {
    assert_eq!(
        decide(PidProbe::Unreadable, JobProbe::Members(1)),
        RecoveryDecision::Refuse
    );
    assert_eq!(
        decide(PidProbe::Absent, JobProbe::Unreadable),
        RecoveryDecision::Refuse
    );
}
