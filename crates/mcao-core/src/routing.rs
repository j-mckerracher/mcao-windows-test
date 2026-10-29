//! Execution-target routing, capability matrix, and authority checks.
//!
//! Pure routing rules for DPES-882: resolving the execution target, the capability matrix
//! per target, and the ordered authority check.

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
}

impl fmt::Display for RouteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownTarget(target) => write!(f, "unknown target: {target}"),
            Self::Unsupported { target, op } => {
                write!(f, "target {target:?} does not support operation {op:?}")
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
/// [`Target::Local`] supports all nine operations.
/// [`Target::Gcp`] supports only [`Op::Status`], [`Op::Logs`], [`Op::Cancel`],
/// [`Op::Fetch`], and [`Op::Teardown`].
pub fn allows(target: Target, op: Op) -> bool {
    match target {
        Target::Local => true,
        Target::Gcp => matches!(
            op,
            Op::Status | Op::Logs | Op::Cancel | Op::Fetch | Op::Teardown
        ),
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
            (Target::Local, Op::Cancel, true),
            (Target::Local, Op::Fetch, true),
            (Target::Local, Op::Attach, true),
            (Target::Local, Op::Relaunch, true),
            (Target::Local, Op::Teardown, true),
            (Target::Gcp, Op::Spawn, false),
            (Target::Gcp, Op::Send, false),
            (Target::Gcp, Op::Status, true),
            (Target::Gcp, Op::Logs, true),
            (Target::Gcp, Op::Cancel, true),
            (Target::Gcp, Op::Fetch, true),
            (Target::Gcp, Op::Attach, false),
            (Target::Gcp, Op::Relaunch, false),
            (Target::Gcp, Op::Teardown, true),
        ];
        assert_eq!(expected.len(), 18);
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

        // Unsupported operations on GCP
        assert_eq!(
            check_op(Target::Gcp, Op::Spawn),
            Err(RouteError::Unsupported {
                target: Target::Gcp,
                op: Op::Spawn,
            })
        );
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
            op: Op::Spawn,
        };
        assert_eq!(
            e2.to_string(),
            "target Gcp does not support operation Spawn"
        );
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
