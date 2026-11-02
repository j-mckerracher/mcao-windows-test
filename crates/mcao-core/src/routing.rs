//! Execution-target routing, capability matrix, and authority checks.
//!
//! Pure routing rules for DPES-882: resolving the execution target, the capability matrix
//! per target, and the ordered authority check.
//!
//! The matrix mirrors the shipped shell contract in `bin/fm-execution-target-lib.sh` and
//! `bin/fm-control.sh`. Each refusal quotes the shell's own remedy wording, and the tests read
//! that contract, so drift fails a test. Nothing in this crate calls these functions yet: the
//! shell is the live path, and this matrix is unit-level until a command wires it.

use std::error::Error;
use std::fmt;

/// Execution target for an operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Local machine execution target.
    Local,
    /// GCP cloud execution target.
    Gcp,
}

/// Operations that can be performed against an execution target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// Spawn a new task.
    Spawn,
    /// Send input to a running task.
    Send,
    /// Query the status of a task.
    Status,
    /// Retrieve logs for a task.
    Logs,
    /// Cancel a running task.
    Cancel,
    /// Fetch task output or artifacts.
    Fetch,
    /// Attach to an interactive task session.
    Attach,
    /// Relaunch an existing task.
    Relaunch,
    /// Teardown task resources.
    Teardown,
    /// Restart a failed cloud run in place (cloud retry).
    Retry,
}

/// Errors arising from target resolution and capability checking.
#[derive(Debug, PartialEq, Eq)]
pub enum RouteError {
    /// The specified target string is not recognized.
    UnknownTarget(String),
    /// The requested operation is not supported on the target.
    Unsupported {
        /// Execution target that received the request.
        target: Target,
        /// Operation that was rejected.
        op: Op,
    },
}

impl RouteError {
    /// Returns the process exit code for this routing error.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::UnknownTarget(_) => 2,
            Self::Unsupported { .. } => 7,
        }
    }

    /// Returns the operator-facing remedy, or `None` when the error has none.
    ///
    /// An [`RouteError::Unsupported`] pair the matrix refuses always has one.
    pub fn remedy(&self) -> Option<&'static str> {
        match self {
            Self::UnknownTarget(_) => None,
            Self::Unsupported { target, op } => refusal(*target, *op),
        }
    }
}

impl fmt::Display for RouteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownTarget(target) => write!(f, "unknown target: {target}"),
            Self::Unsupported { target, op } => {
                write!(f, "target {target:?} does not support operation {op:?}")?;
                match self.remedy() {
                    Some(remedy) => write!(f, "; {remedy}"),
                    None => Ok(()),
                }
            }
        }
    }
}

impl Error for RouteError {}

/// Resolves an execution target from an optional flag value.
///
/// If `flag` is `None`, defaults to [`Target::Local`].
/// Exact lowercase strings `"local"` and `"gcp"` resolve to their respective targets.
/// Any other value returns [`RouteError::UnknownTarget`].
///
/// # Errors
///
/// Returns [`RouteError::UnknownTarget`] if the flag value is not `"local"` or `"gcp"`.
pub fn resolve_target(flag: Option<&str>) -> Result<Target, RouteError> {
    match flag {
        None | Some("local") => Ok(Target::Local),
        Some("gcp") => Ok(Target::Gcp),
        Some(unknown) => Err(RouteError::UnknownTarget(unknown.to_owned())),
    }
}

/// Checks whether the given execution target supports the operation.
///
/// [`Target::Local`] supports every operation except [`Op::Cancel`] and [`Op::Retry`].
/// [`Target::Gcp`] supports [`Op::Spawn`] (a cloud submit, scouts only), [`Op::Status`],
/// [`Op::Logs`], [`Op::Cancel`], [`Op::Fetch`], and [`Op::Teardown`].
///
/// These gcp rows are not all shell-guarded:
///
/// - [`Op::Cancel`] is live through `bin/fm-control.sh`, which runs `mcao cloud cancel`
///   directly. `bin/fm-cloud-policy.sh` denies cancel, but no shell route calls the policy, so
///   that denial does not apply to this route (DPES-999 AC4; the policy bypass is DPES-999 AC5,
///   open).
/// - [`Op::Fetch`] has no shell route. Nothing in `bin/` dispatches it to the cloud client.
/// - [`Op::Teardown`] is allowed pending owner decision DPES-999 AC2, which is open. No gate in
///   `bin/` keys on `execution_target=gcp`. The teardown gate at `bin/fm-teardown.sh:413` matches
///   only `cloud_target=` (`bin/fm-cloud-task-lib.sh:32-33`). A gcp record has no `window=`, so
///   `fm_backend_validate_task_endpoint` refuses it at `bin/fm-teardown.sh:947`. That refusal is
///   incidental, not a gcp gate.
///
/// The scout-only limit on a gcp spawn is enforced by `bin/fm-spawn.sh`. This matrix is per
/// operation and does not model it.
pub fn allows(target: Target, op: Op) -> bool {
    refusal(target, op).is_none()
}

/// Returns the refusal remedy for an operation the target does not allow, or `None` when it does.
///
/// This is the only place the matrix is written. Each target matches every [`Op`] with no
/// wildcard, so a new operation or target does not compile until its refusal and remedy are
/// decided here. Each remedy quotes the shell contract's own wording.
fn refusal(target: Target, op: Op) -> Option<&'static str> {
    match target {
        Target::Local => match op {
            Op::Cancel => Some(
                "cancellation applies only to gcp tasks. Use 'interrupt' to stop the current turn or 'exit' to stop the agent.",
            ),
            Op::Retry => Some(
                "restart applies only to gcp tasks. Use 'relaunch' to start a fresh agent with the brief and a progress note.",
            ),
            Op::Spawn
            | Op::Send
            | Op::Status
            | Op::Logs
            | Op::Fetch
            | Op::Attach
            | Op::Relaunch
            | Op::Teardown => None,
        },
        Target::Gcp => match op {
            Op::Send | Op::Relaunch => Some(
                "Interrupt, exit, relaunch, steering, key or slash input, attach, and promotion are local-only. Follow-up work on a cloud task is a new assignment with an explicit new source commit.",
            ),
            Op::Attach => Some(
                "Interrupt, exit, relaunch, steering, key or slash input, attach, and promotion are local-only. Follow-up work on a cloud task is a new assignment with an explicit new source commit.",
            ),
            Op::Retry => Some(
                "the restart contract is not published (cloud retry argv and response shape DPES-805/814, recovery policy DPES-869/870). Nothing was changed and no cloud request was sent. No local fallback runs.",
            ),
            Op::Spawn | Op::Status | Op::Logs | Op::Cancel | Op::Fetch | Op::Teardown => None,
        },
    }
}

/// Validates that an operation is supported on the target.
///
/// # Errors
///
/// Returns [`RouteError::Unsupported`] if `allows(target, op)` is `false`.
pub fn check_op(target: Target, op: Op) -> Result<(), RouteError> {
    if allows(target, op) {
        Ok(())
    } else {
        Err(RouteError::Unsupported { target, op })
    }
}

/// Reasons why an operation is refused by the authority check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The lease is held by another actor.
    LeaseNotHeld {
        /// Entity currently holding the lease.
        held_by: String,
        /// Actor attempting the operation.
        actor: String,
    },
    /// A gating policy is refusing the operation.
    GateRefusing {
        /// Reason for the gate refusal.
        reason: String,
    },
    /// Away-spend policy blocks the operation.
    AwaySpendBlocked,
    /// Concurrency capacity has been exceeded.
    CapacityExceeded {
        /// Occupied local slots.
        local: usize,
        /// Non-terminal GCP tasks.
        gcp: usize,
        /// Maximum allowed capacity limit.
        limit: u32,
    },
}

impl Refusal {
    /// Process exit code for authority refusal.
    pub const EXIT_CODE: i32 = 4;
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LeaseNotHeld { held_by, actor } => {
                write!(f, "Lease held by {held_by}; current actor {actor}")
            }
            Self::GateRefusing { reason } => write!(f, "Gate refusing: {reason}"),
            Self::AwaySpendBlocked => write!(f, "Away-spend policy blocks operation"),
            Self::CapacityExceeded { local, gcp, limit } => {
                write!(f, "Capacity exceeded: {local} local + {gcp} gcp >= {limit}")
            }
        }
    }
}

impl Error for Refusal {}

/// Inputs for checking authority to execute an operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityInputs {
    /// The entity currently holding the lease, if any.
    pub lease_holder: Option<String>,
    /// The actor attempting the operation.
    pub actor: String,
    /// Refusal reason from the gate, if any.
    pub gate_refusal: Option<String>,
    /// Whether away-spend policy blocks execution.
    pub away_spend_blocked: bool,
    /// Current number of occupied local slots.
    pub local_occupied: usize,
    /// Current number of non-terminal GCP tasks.
    pub gcp_non_terminal: usize,
    /// Maximum capacity limit across local and GCP tasks.
    pub limit: u32,
}

/// Checks authority to proceed with an operation.
///
/// Evaluates checks in exact order:
/// 1. Lease check: if a lease is held and the holder does not match `actor`,
///    returns [`Refusal::LeaseNotHeld`].
/// 2. Gate check: if a gate refusal reason is present, returns [`Refusal::GateRefusing`].
/// 3. Away-spend check: if `away_spend_blocked` is `true`, returns [`Refusal::AwaySpendBlocked`].
/// 4. Capacity check: if `local_occupied + gcp_non_terminal >= limit`, returns
///    [`Refusal::CapacityExceeded`].
///
/// # Errors
///
/// Returns the first [`Refusal`] encountered in evaluation order.
pub fn check_authority(inputs: &AuthorityInputs) -> Result<(), Refusal> {
    if let Some(held_by) = &inputs.lease_holder
        && *held_by != inputs.actor
    {
        return Err(Refusal::LeaseNotHeld {
            held_by: held_by.clone(),
            actor: inputs.actor.clone(),
        });
    }

    if let Some(reason) = &inputs.gate_refusal {
        return Err(Refusal::GateRefusing {
            reason: reason.clone(),
        });
    }

    if inputs.away_spend_blocked {
        return Err(Refusal::AwaySpendBlocked);
    }

    let occupied = inputs
        .local_occupied
        .saturating_add(inputs.gcp_non_terminal);
    if occupied >= inputs.limit as usize {
        return Err(Refusal::CapacityExceeded {
            local: inputs.local_occupied,
            gcp: inputs.gcp_non_terminal,
            limit: inputs.limit,
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_target() {
        assert_eq!(resolve_target(None), Ok(Target::Local));
        assert_eq!(resolve_target(Some("local")), Ok(Target::Local));
        assert_eq!(resolve_target(Some("gcp")), Ok(Target::Gcp));
        assert_eq!(
            resolve_target(Some("GCP")),
            Err(RouteError::UnknownTarget("GCP".to_owned()))
        );
        assert_eq!(
            resolve_target(Some("unknown")),
            Err(RouteError::UnknownTarget("unknown".to_owned()))
        );
        assert_eq!(
            resolve_target(Some("LOCAL")),
            Err(RouteError::UnknownTarget("LOCAL".to_owned()))
        );
        assert_eq!(
            resolve_target(Some("")),
            Err(RouteError::UnknownTarget(String::new()))
        );
    }

    #[test]
    fn test_matrix_exhaustive() {
        let expected = [
            (Target::Local, Op::Spawn, true),
            (Target::Local, Op::Send, true),
            (Target::Local, Op::Status, true),
            (Target::Local, Op::Logs, true),
            (Target::Local, Op::Cancel, false),
            (Target::Local, Op::Fetch, true),
            (Target::Local, Op::Attach, true),
            (Target::Local, Op::Relaunch, true),
            (Target::Local, Op::Teardown, true),
            (Target::Local, Op::Retry, false),
            (Target::Gcp, Op::Spawn, true),
            (Target::Gcp, Op::Send, false),
            (Target::Gcp, Op::Status, true),
            (Target::Gcp, Op::Logs, true),
            (Target::Gcp, Op::Cancel, true),
            (Target::Gcp, Op::Fetch, true),
            (Target::Gcp, Op::Attach, false),
            (Target::Gcp, Op::Relaunch, false),
            (Target::Gcp, Op::Teardown, true),
            (Target::Gcp, Op::Retry, false),
        ];
        assert_eq!(expected.len(), 20);
        for (target, op, allow) in expected {
            assert_eq!(
                allows(target, op),
                allow,
                "matrix mismatch for ({target:?}, {op:?})"
            );
        }
    }

    #[test]
    fn test_check_op() {
        // Supported operations
        assert_eq!(check_op(Target::Local, Op::Spawn), Ok(()));
        assert_eq!(check_op(Target::Local, Op::Teardown), Ok(()));
        assert_eq!(check_op(Target::Gcp, Op::Status), Ok(()));
        assert_eq!(check_op(Target::Gcp, Op::Logs), Ok(()));
        assert_eq!(check_op(Target::Gcp, Op::Cancel), Ok(()));
        assert_eq!(check_op(Target::Gcp, Op::Fetch), Ok(()));
        assert_eq!(check_op(Target::Gcp, Op::Teardown), Ok(()));

        // Spawn is a scout's cloud submit on gcp, per the shell contract.
        assert_eq!(check_op(Target::Gcp, Op::Spawn), Ok(()));
        assert_eq!(
            check_op(Target::Local, Op::Cancel),
            Err(RouteError::Unsupported {
                target: Target::Local,
                op: Op::Cancel,
            })
        );
        assert_eq!(
            check_op(Target::Local, Op::Retry),
            Err(RouteError::Unsupported {
                target: Target::Local,
                op: Op::Retry,
            })
        );
        assert_eq!(
            check_op(Target::Gcp, Op::Retry),
            Err(RouteError::Unsupported {
                target: Target::Gcp,
                op: Op::Retry,
            })
        );

        // Unsupported operations on GCP
        assert_eq!(
            check_op(Target::Gcp, Op::Send),
            Err(RouteError::Unsupported {
                target: Target::Gcp,
                op: Op::Send,
            })
        );
        assert_eq!(
            check_op(Target::Gcp, Op::Attach),
            Err(RouteError::Unsupported {
                target: Target::Gcp,
                op: Op::Attach,
            })
        );
        assert_eq!(
            check_op(Target::Gcp, Op::Relaunch),
            Err(RouteError::Unsupported {
                target: Target::Gcp,
                op: Op::Relaunch,
            })
        );
    }

    fn passing_inputs() -> AuthorityInputs {
        AuthorityInputs {
            lease_holder: None,
            actor: "alice".to_owned(),
            gate_refusal: None,
            away_spend_blocked: false,
            local_occupied: 2,
            gcp_non_terminal: 3,
            limit: 10,
        }
    }

    #[test]
    fn test_check_authority_success() {
        let inputs = passing_inputs();
        assert_eq!(check_authority(&inputs), Ok(()));

        // Lease held by the same actor passes
        let inputs = AuthorityInputs {
            lease_holder: Some("alice".to_owned()),
            ..passing_inputs()
        };
        assert_eq!(check_authority(&inputs), Ok(()));
    }

    #[test]
    fn test_check_authority_each_failure_alone() {
        // Lease failure alone
        let inputs = AuthorityInputs {
            lease_holder: Some("bob".to_owned()),
            ..passing_inputs()
        };
        assert_eq!(
            check_authority(&inputs),
            Err(Refusal::LeaseNotHeld {
                held_by: "bob".to_owned(),
                actor: "alice".to_owned(),
            })
        );

        // Gate failure alone
        let inputs = AuthorityInputs {
            gate_refusal: Some("maintenance underway".to_owned()),
            ..passing_inputs()
        };
        assert_eq!(
            check_authority(&inputs),
            Err(Refusal::GateRefusing {
                reason: "maintenance underway".to_owned(),
            })
        );

        // Away-spend failure alone
        let inputs = AuthorityInputs {
            away_spend_blocked: true,
            ..passing_inputs()
        };
        assert_eq!(check_authority(&inputs), Err(Refusal::AwaySpendBlocked));

        // Capacity failure alone
        let inputs = AuthorityInputs {
            local_occupied: 6,
            gcp_non_terminal: 5,
            limit: 10,
            ..passing_inputs()
        };
        assert_eq!(
            check_authority(&inputs),
            Err(Refusal::CapacityExceeded {
                local: 6,
                gcp: 5,
                limit: 10,
            })
        );
    }

    #[test]
    fn test_check_authority_failure_order() {
        // Lease beats gate, away-spend, capacity
        let inputs = AuthorityInputs {
            lease_holder: Some("bob".to_owned()),
            actor: "alice".to_owned(),
            gate_refusal: Some("gate blocked".to_owned()),
            away_spend_blocked: true,
            local_occupied: 10,
            gcp_non_terminal: 10,
            limit: 5,
        };
        assert_eq!(
            check_authority(&inputs),
            Err(Refusal::LeaseNotHeld {
                held_by: "bob".to_owned(),
                actor: "alice".to_owned(),
            })
        );

        // Gate beats away-spend and capacity
        let inputs = AuthorityInputs {
            lease_holder: Some("alice".to_owned()),
            actor: "alice".to_owned(),
            gate_refusal: Some("gate blocked".to_owned()),
            away_spend_blocked: true,
            local_occupied: 10,
            gcp_non_terminal: 10,
            limit: 5,
        };
        assert_eq!(
            check_authority(&inputs),
            Err(Refusal::GateRefusing {
                reason: "gate blocked".to_owned(),
            })
        );

        // Away-spend beats capacity
        let inputs = AuthorityInputs {
            lease_holder: None,
            actor: "alice".to_owned(),
            gate_refusal: None,
            away_spend_blocked: true,
            local_occupied: 10,
            gcp_non_terminal: 10,
            limit: 5,
        };
        assert_eq!(check_authority(&inputs), Err(Refusal::AwaySpendBlocked));

        // Lease beats gate
        let inputs = AuthorityInputs {
            lease_holder: Some("bob".to_owned()),
            gate_refusal: Some("gate blocked".to_owned()),
            ..passing_inputs()
        };
        assert_eq!(
            check_authority(&inputs),
            Err(Refusal::LeaseNotHeld {
                held_by: "bob".to_owned(),
                actor: "alice".to_owned(),
            })
        );

        // Lease beats away-spend
        let inputs = AuthorityInputs {
            lease_holder: Some("bob".to_owned()),
            away_spend_blocked: true,
            ..passing_inputs()
        };
        assert_eq!(
            check_authority(&inputs),
            Err(Refusal::LeaseNotHeld {
                held_by: "bob".to_owned(),
                actor: "alice".to_owned(),
            })
        );

        // Gate beats capacity
        let inputs = AuthorityInputs {
            gate_refusal: Some("gate blocked".to_owned()),
            local_occupied: 10,
            gcp_non_terminal: 10,
            limit: 5,
            ..passing_inputs()
        };
        assert_eq!(
            check_authority(&inputs),
            Err(Refusal::GateRefusing {
                reason: "gate blocked".to_owned(),
            })
        );
    }

    #[test]
    fn test_check_authority_capacity_boundaries() {
        // Exactly at limit refused
        let inputs = AuthorityInputs {
            local_occupied: 5,
            gcp_non_terminal: 5,
            limit: 10,
            ..passing_inputs()
        };
        assert_eq!(
            check_authority(&inputs),
            Err(Refusal::CapacityExceeded {
                local: 5,
                gcp: 5,
                limit: 10,
            })
        );

        // One below limit passes
        let inputs = AuthorityInputs {
            local_occupied: 5,
            gcp_non_terminal: 4,
            limit: 10,
            ..passing_inputs()
        };
        assert_eq!(check_authority(&inputs), Ok(()));

        // Limit 0 refused even when occupied is 0
        let inputs = AuthorityInputs {
            local_occupied: 0,
            gcp_non_terminal: 0,
            limit: 0,
            ..passing_inputs()
        };
        assert_eq!(
            check_authority(&inputs),
            Err(Refusal::CapacityExceeded {
                local: 0,
                gcp: 0,
                limit: 0,
            })
        );

        // Saturating addition does not overflow
        let inputs = AuthorityInputs {
            local_occupied: usize::MAX,
            gcp_non_terminal: 1,
            limit: 100,
            ..passing_inputs()
        };
        assert_eq!(
            check_authority(&inputs),
            Err(Refusal::CapacityExceeded {
                local: usize::MAX,
                gcp: 1,
                limit: 100,
            })
        );
    }

    #[test]
    fn test_display_texts() {
        let r1 = Refusal::LeaseNotHeld {
            held_by: "carol".to_owned(),
            actor: "dave".to_owned(),
        };
        assert_eq!(r1.to_string(), "Lease held by carol; current actor dave");

        let r2 = Refusal::GateRefusing {
            reason: "policy disallows".to_owned(),
        };
        assert_eq!(r2.to_string(), "Gate refusing: policy disallows");

        let r3 = Refusal::AwaySpendBlocked;
        assert_eq!(r3.to_string(), "Away-spend policy blocks operation");

        let r4 = Refusal::CapacityExceeded {
            local: 3,
            gcp: 2,
            limit: 4,
        };
        assert_eq!(r4.to_string(), "Capacity exceeded: 3 local + 2 gcp >= 4");

        let e1 = RouteError::UnknownTarget("slurm".to_owned());
        assert_eq!(e1.to_string(), "unknown target: slurm");

        let e2 = RouteError::Unsupported {
            target: Target::Gcp,
            op: Op::Send,
        };
        assert_eq!(
            e2.to_string(),
            "target Gcp does not support operation Send; Interrupt, exit, relaunch, steering, key or slash input, attach, and promotion are local-only. Follow-up work on a cloud task is a new assignment with an explicit new source commit."
        );
    }

    /// The shipped shell contract. Remedies must say what it says, so these tests check each
    /// clause against this text rather than against the strings `refusal` returns.
    const SHELL_CONTRACT: &str = include_str!("../../../bin/fm-execution-target-lib.sh");
    /// The shipped control script. Its top-level `case "$VERB"` arms are the verbs a remedy may
    /// tell the operator to run, and its cancel arm is the live gcp cancel route.
    const SHELL_CONTROL: &str = include_str!("../../../bin/fm-control.sh");
    /// The shell routes the gcp pair tests cite, and the endpoint validator teardown calls.
    const SHELL_SPAWN: &str = include_str!("../../../bin/fm-spawn.sh");
    const SHELL_OBSERVE: &str = include_str!("../../../bin/fm-cloud-observe-lib.sh");
    const SHELL_PEEK: &str = include_str!("../../../bin/fm-peek.sh");
    const SHELL_TEARDOWN: &str = include_str!("../../../bin/fm-teardown.sh");
    const SHELL_CLOUD_TASK: &str = include_str!("../../../bin/fm-cloud-task-lib.sh");
    const SHELL_BACKEND: &str = include_str!("../../../bin/fm-backend.sh");

    /// Every operation, so the checks below cover the whole matrix.
    const ALL_OPS: [Op; 10] = [
        Op::Spawn,
        Op::Send,
        Op::Status,
        Op::Logs,
        Op::Cancel,
        Op::Fetch,
        Op::Attach,
        Op::Relaunch,
        Op::Teardown,
        Op::Retry,
    ];

    /// The shell's remedy sentences, copied verbatim from `bin/fm-execution-target-lib.sh`. Each
    /// refused pair's remedy must equal one of these exactly.
    const SHELL_LOCAL_CANCEL: &str = "cancellation applies only to gcp tasks. Use 'interrupt' to stop the current turn or 'exit' to stop the agent.";
    const SHELL_LOCAL_RETRY: &str = "restart applies only to gcp tasks. Use 'relaunch' to start a fresh agent with the brief and a progress note.";
    const SHELL_LOCAL_ONLY: &str = "Interrupt, exit, relaunch, steering, key or slash input, attach, and promotion are local-only. Follow-up work on a cloud task is a new assignment with an explicit new source commit.";
    const SHELL_GCP_RESTART: &str = "the restart contract is not published (cloud retry argv and response shape DPES-805/814, recovery policy DPES-869/870). Nothing was changed and no cloud request was sent. No local fallback runs.";

    /// The refused pairs, written out by hand rather than derived from `allows`.
    const REFUSED: [(Target, Op); 6] = [
        (Target::Local, Op::Cancel),
        (Target::Local, Op::Retry),
        (Target::Gcp, Op::Send),
        (Target::Gcp, Op::Relaunch),
        (Target::Gcp, Op::Attach),
        (Target::Gcp, Op::Retry),
    ];

    #[test]
    fn every_refused_pair_has_a_remedy_and_every_allowed_pair_has_none() {
        for target in [Target::Local, Target::Gcp] {
            for op in ALL_OPS {
                let refused = REFUSED.contains(&(target, op));
                let remedy = RouteError::Unsupported { target, op }.remedy();
                assert_eq!(remedy.is_some(), refused, "{target:?} {op:?}");
                assert_eq!(allows(target, op), !refused, "{target:?} {op:?}");
            }
        }
    }

    #[test]
    fn refusals_quote_the_shell_contract() {
        let cases = [
            (Target::Local, Op::Cancel, SHELL_LOCAL_CANCEL),
            (Target::Local, Op::Retry, SHELL_LOCAL_RETRY),
            (Target::Gcp, Op::Send, SHELL_LOCAL_ONLY),
            (Target::Gcp, Op::Relaunch, SHELL_LOCAL_ONLY),
            (Target::Gcp, Op::Attach, SHELL_LOCAL_ONLY),
            (Target::Gcp, Op::Retry, SHELL_GCP_RESTART),
        ];
        for (target, op) in REFUSED {
            assert!(
                cases.iter().any(|&(t, o, _)| (t, o) == (target, op)),
                "{target:?} {op:?} has no expected remedy"
            );
        }
        for (target, op, sentence) in cases {
            assert!(
                SHELL_CONTRACT.contains(sentence),
                "shell lacks {sentence:?}"
            );
            let err = RouteError::Unsupported { target, op };
            assert_eq!(err.remedy(), Some(sentence), "{target:?} {op:?}");
        }
    }

    /// The verbs with a `verb)` arm in the top-level `case "$VERB" in` dispatch of a control
    /// script. Nested cases are indented, so only the dispatch's own arms match.
    fn dispatched_verbs(script: &str) -> Vec<&str> {
        let mut verbs = Vec::new();
        let mut in_dispatch = false;
        for line in script.lines() {
            if line == "case \"$VERB\" in" {
                in_dispatch = true;
            } else if line == "esac" {
                in_dispatch = false;
            } else if in_dispatch
                && let Some(verb) = line
                    .strip_prefix("  ")
                    .and_then(|rest| rest.strip_suffix(')'))
                && !verb.is_empty()
                && verb.bytes().all(|b| b.is_ascii_lowercase())
            {
                verbs.push(verb);
            }
        }
        verbs
    }

    /// The verbs a remedy sentence names: its single-quoted words, so `Use 'interrupt' ... 'exit'`
    /// names `interrupt` and `exit`.
    fn named_verbs(remedy: &str) -> Vec<&str> {
        remedy.split('\'').skip(1).step_by(2).collect()
    }

    #[test]
    fn remedy_verbs_are_shipped_control_verbs() {
        let dispatched = dispatched_verbs(SHELL_CONTROL);
        let mut named = Vec::new();
        for (target, op) in REFUSED {
            let remedy = RouteError::Unsupported { target, op }
                .remedy()
                .expect("a refused pair has a remedy");
            named.extend(named_verbs(remedy));
        }
        assert!(!named.is_empty(), "no refusal remedy names a verb");
        for verb in named {
            assert!(
                dispatched.contains(&verb),
                "a remedy names '{verb}', but fm-control.sh has no `{verb})` arm in its case \"$VERB\" dispatch"
            );
        }
    }

    /// The gcp operations the matrix allows, written out by hand. Each pair has its own test
    /// below, which cites its shell route or names it as unguarded in shell. Teardown is allowed
    /// pending owner decision DPES-999 AC2.
    const GCP_ALLOWED: [Op; 6] = [
        Op::Spawn,
        Op::Status,
        Op::Logs,
        Op::Cancel,
        Op::Fetch,
        Op::Teardown,
    ];

    #[test]
    fn gcp_allowed_set_is_exactly_the_listed_operations() {
        for op in ALL_OPS {
            assert_eq!(allows(Target::Gcp, op), GCP_ALLOWED.contains(&op), "{op:?}");
        }
    }

    #[test]
    fn gcp_spawn_cites_the_shell_submit_route() {
        assert!(allows(Target::Gcp, Op::Spawn));
        // fm-spawn.sh:1765-1766 dispatches a gcp record to spawn_cloud_submit; fm-spawn.sh:1744
        // calls fm_exec_cloud_submit, which runs `mcao cloud submit` (fm-execution-target-lib.sh:237).
        assert!(SHELL_SPAWN.contains(
            r#"if [ "$EXEC_TARGET" = "$FM_EXEC_TARGET_GCP" ]; then
  spawn_cloud_submit"#
        ));
        assert!(SHELL_SPAWN.contains(r#"fm_exec_cloud_submit "$SPAWN_CLOUD_PROJECT""#));
        assert!(SHELL_CONTRACT.contains(r#"fm_exec_cloud_run submit --repo "$1""#));
    }

    #[test]
    fn gcp_status_cites_the_observe_read_route() {
        assert!(allows(Target::Gcp, Op::Status));
        // fm-cloud-observe-lib.sh:134 reads `mcao cloud status --run-id` through
        // fm_exec_cloud_run_timed.
        assert!(SHELL_OBSERVE.contains(
            r#"fm_exec_cloud_run_timed "$FM_CLOUD_READ_TIMEOUT_SECONDS" status --run-id "$run""#
        ));
    }

    #[test]
    fn gcp_logs_cites_the_fm_peek_route() {
        assert!(allows(Target::Gcp, Op::Logs));
        // fm-peek.sh:38 reads `mcao cloud logs --run-id` through fm_exec_cloud_run.
        assert!(SHELL_PEEK.contains(r#"fm_exec_cloud_run logs --run-id "$PEEK_RUN""#));
    }

    #[test]
    fn gcp_cancel_cites_the_fm_control_route() {
        assert!(allows(Target::Gcp, Op::Cancel));
        // fm-control.sh:365-368 runs `mcao cloud cancel` directly. fm-cloud-policy.sh denies
        // cancel, but no shell route runs the policy, so this route bypasses it.
        assert!(SHELL_CONTROL.contains(r#"fm_exec_cloud_run cancel --run-id "$CLOUD_RUN_ID""#));
    }

    #[test]
    fn gcp_fetch_is_unguarded_in_shell() {
        assert!(allows(Target::Gcp, Op::Fetch));
        // The lib lists fetch as a cloud op (fm-execution-target-lib.sh:123), but only
        // tests/fm-execution-target.test.sh calls fm_exec_cloud_op_supported. fm-control-lib.sh:60
        // is the control-verb allow-list (interrupt|exit|relaunch|cancel|retry), which omits fetch.
        // fm-control.sh and fm-peek.sh have no fetch route.
        assert!(SHELL_CONTRACT.contains("status|logs|cancel|fetch) return 0"));
        assert!(!SHELL_CONTROL.contains("fetch"));
        assert!(!SHELL_PEEK.contains("fetch"));
    }

    #[test]
    fn gcp_teardown_is_refused_only_by_endpoint_validation() {
        assert!(allows(Target::Gcp, Op::Teardown));
        // Allowed pending owner decision DPES-999 AC2, which is open. No gate keys on
        // execution_target=gcp. The cloud_target gate at fm-teardown.sh:413 does not match a gcp
        // record (fm-cloud-task-lib.sh:32-33). A gcp record is written with no window= or worktree=
        // (fm-spawn.sh:1672 and the lines around it). fm-teardown.sh:947 calls
        // fm_backend_validate_task_endpoint, which refuses a record with no window=
        // (fm-backend.sh:248), so the refusal is incidental.
        assert!(SHELL_TEARDOWN.contains(
            "  fm_backend_validate_task_endpoint \"$META\" \"$ID\" || exit 1\n  BACKEND=$FM_BACKEND_VALIDATED_BACKEND"
        ));
        assert!(
            SHELL_BACKEND.contains(r#"window=$(fm_backend_meta_exact_value "$meta" window) || {"#)
        );
        assert!(
            SHELL_TEARDOWN
                .contains(r#"if fm_cloud_task_refuse_teardown "$STATE/$ID.meta" "$ID"; then"#)
        );
        assert!(SHELL_CLOUD_TASK.contains(r#"grep -q '^cloud_target=' "$1""#));
        assert!(!SHELL_TEARDOWN.contains("execution_target"));
    }

    #[test]
    fn unsupported_display_names_the_remedy() {
        let err = check_op(Target::Gcp, Op::Send).expect_err("gcp refuses send");
        assert_eq!(err.exit_code(), 7);
        let remedy = err.remedy().expect("refusal has a remedy");
        assert_eq!(
            err.to_string(),
            format!("target Gcp does not support operation Send; {remedy}")
        );
        assert_eq!(RouteError::UnknownTarget("slurm".to_owned()).remedy(), None);
    }

    #[test]
    fn test_exit_codes() {
        assert_eq!(Refusal::EXIT_CODE, 4);

        let e1 = RouteError::UnknownTarget("slurm".to_owned());
        assert_eq!(e1.exit_code(), 2);

        let e2 = RouteError::Unsupported {
            target: Target::Gcp,
            op: Op::Spawn,
        };
        assert_eq!(e2.exit_code(), 7);
    }
}
