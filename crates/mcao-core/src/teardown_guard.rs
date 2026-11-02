//! Pure decision logic for DPES-882 cloud teardown safety rules.
//!
//! Evaluates whether cloud-target runs can safely proceed with teardown based on
//! their observed run status, flags, and result fetch state.

/// The observed execution status of a cloud-target run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    /// The run is still active or has not reached a terminal state.
    NonTerminal,
    /// The run completed successfully.
    Succeeded,
    /// The run failed. A failed run still returns partial work, but the fetch decision
    /// is the caller's.
    Failed,
    /// The run timed out. A failed run still returns partial work, but the fetch decision
    /// is the caller's.
    TimedOut,
    /// The run was cancelled. A failed run still returns partial work, but the fetch decision
    /// is the caller's.
    Cancelled,
    /// The status of the run could not be read or reached.
    Unreachable,
}

/// The facts gathered about a cloud-target run before teardown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TeardownFacts {
    /// The observed run status of the cloud run.
    pub status: RunStatus,
    /// Whether the `--cancel-cloud` flag was supplied.
    pub cancel_cloud_flag: bool,
    /// Whether the execution results have been fetched locally.
    pub result_fetched: bool,
}

/// The decision reached by evaluating teardown safety rules for a cloud run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TeardownDecision {
    /// Proceed with teardown immediately.
    Proceed,
    /// Cancel the active cloud run first, then proceed with teardown.
    CancelThenProceed,
    /// Teardown is refused until the specified remedy is applied.
    Refused {
        /// Short description explaining why teardown was refused.
        reason: String,
        /// Concrete command or action to resolve the refusal.
        remedy: String,
    },
    /// Teardown safety cannot be established; never guess the status.
    Uncertain {
        /// Short description explaining why the status is uncertain.
        reason: String,
    },
}

/// Decides whether teardown can proceed safely for a cloud-target run.
///
/// Rules are evaluated in the following order:
/// 1. [`RunStatus::Unreachable`] -> [`TeardownDecision::Uncertain`] (reason says the run status
///    could not be read; never guess).
/// 2. [`RunStatus::NonTerminal`]:
///    - with `cancel_cloud_flag` set -> [`TeardownDecision::CancelThenProceed`].
///    - without `cancel_cloud_flag` -> [`TeardownDecision::Refused`] with remedy
///      `"rerun with --cancel-cloud"`.
/// 3. [`RunStatus::Succeeded`]:
///    - with `result_fetched == false` -> [`TeardownDecision::Refused`] with remedy
///      `"mcao cloud fetch"`.
///    - with `result_fetched == true` -> [`TeardownDecision::Proceed`].
/// 4. [`RunStatus::Failed`], [`RunStatus::TimedOut`], [`RunStatus::Cancelled`] ->
///    [`TeardownDecision::Proceed`]. A failed run still returns partial work, but the
///    fetch decision is the caller's.
///
/// The `cancel_cloud_flag` is ignored for terminal runs. Reasons and remedies are short
/// non-empty strings.
pub fn decide(facts: &TeardownFacts) -> TeardownDecision {
    match facts.status {
        RunStatus::Unreachable => TeardownDecision::Uncertain {
            reason: "run status could not be read".to_string(),
        },
        RunStatus::NonTerminal => {
            if facts.cancel_cloud_flag {
                TeardownDecision::CancelThenProceed
            } else {
                TeardownDecision::Refused {
                    reason: "run is still active".to_string(),
                    remedy: "rerun with --cancel-cloud".to_string(),
                }
            }
        }
        RunStatus::Succeeded => {
            if facts.result_fetched {
                TeardownDecision::Proceed
            } else {
                TeardownDecision::Refused {
                    reason: "run succeeded but results have not been fetched".to_string(),
                    remedy: "mcao cloud fetch".to_string(),
                }
            }
        }
        RunStatus::Failed | RunStatus::TimedOut | RunStatus::Cancelled => TeardownDecision::Proceed,
    }
}

/// Maps a teardown decision to its process exit code.
///
/// - [`TeardownDecision::Proceed`] and [`TeardownDecision::CancelThenProceed`] map to `0`.
/// - [`TeardownDecision::Refused`] maps to `4`.
/// - [`TeardownDecision::Uncertain`] maps to `5`.
pub fn exit_code(decision: &TeardownDecision) -> i32 {
    match decision {
        TeardownDecision::Proceed | TeardownDecision::CancelThenProceed => 0,
        TeardownDecision::Refused { .. } => 4,
        TeardownDecision::Uncertain { .. } => 5,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ExpectedCase {
        status: RunStatus,
        cancel_cloud_flag: bool,
        result_fetched: bool,
        expected_decision: TeardownDecision,
        expected_exit_code: i32,
    }

    #[test]
    fn test_all_combinations_table() {
        let table = [
            // Unreachable (4 combinations): Uncertain, exit code 5
            ExpectedCase {
                status: RunStatus::Unreachable,
                cancel_cloud_flag: false,
                result_fetched: false,
                expected_decision: TeardownDecision::Uncertain {
                    reason: "run status could not be read".to_string(),
                },
                expected_exit_code: 5,
            },
            ExpectedCase {
                status: RunStatus::Unreachable,
                cancel_cloud_flag: false,
                result_fetched: true,
                expected_decision: TeardownDecision::Uncertain {
                    reason: "run status could not be read".to_string(),
                },
                expected_exit_code: 5,
            },
            ExpectedCase {
                status: RunStatus::Unreachable,
                cancel_cloud_flag: true,
                result_fetched: false,
                expected_decision: TeardownDecision::Uncertain {
                    reason: "run status could not be read".to_string(),
                },
                expected_exit_code: 5,
            },
            ExpectedCase {
                status: RunStatus::Unreachable,
                cancel_cloud_flag: true,
                result_fetched: true,
                expected_decision: TeardownDecision::Uncertain {
                    reason: "run status could not be read".to_string(),
                },
                expected_exit_code: 5,
            },
            // NonTerminal (4 combinations):
            // flag=false -> Refused (remedy "rerun with --cancel-cloud"), exit code 4
            // flag=true  -> CancelThenProceed, exit code 0
            ExpectedCase {
                status: RunStatus::NonTerminal,
                cancel_cloud_flag: false,
                result_fetched: false,
                expected_decision: TeardownDecision::Refused {
                    reason: "run is still active".to_string(),
                    remedy: "rerun with --cancel-cloud".to_string(),
                },
                expected_exit_code: 4,
            },
            ExpectedCase {
                status: RunStatus::NonTerminal,
                cancel_cloud_flag: false,
                result_fetched: true,
                expected_decision: TeardownDecision::Refused {
                    reason: "run is still active".to_string(),
                    remedy: "rerun with --cancel-cloud".to_string(),
                },
                expected_exit_code: 4,
            },
            ExpectedCase {
                status: RunStatus::NonTerminal,
                cancel_cloud_flag: true,
                result_fetched: false,
                expected_decision: TeardownDecision::CancelThenProceed,
                expected_exit_code: 0,
            },
            ExpectedCase {
                status: RunStatus::NonTerminal,
                cancel_cloud_flag: true,
                result_fetched: true,
                expected_decision: TeardownDecision::CancelThenProceed,
                expected_exit_code: 0,
            },
            // Succeeded (4 combinations):
            // fetched=false -> Refused (remedy "mcao cloud fetch"), exit code 4
            // fetched=true  -> Proceed, exit code 0
            ExpectedCase {
                status: RunStatus::Succeeded,
                cancel_cloud_flag: false,
                result_fetched: false,
                expected_decision: TeardownDecision::Refused {
                    reason: "run succeeded but results have not been fetched".to_string(),
                    remedy: "mcao cloud fetch".to_string(),
                },
                expected_exit_code: 4,
            },
            ExpectedCase {
                status: RunStatus::Succeeded,
                cancel_cloud_flag: false,
                result_fetched: true,
                expected_decision: TeardownDecision::Proceed,
                expected_exit_code: 0,
            },
            ExpectedCase {
                status: RunStatus::Succeeded,
                cancel_cloud_flag: true,
                result_fetched: false,
                expected_decision: TeardownDecision::Refused {
                    reason: "run succeeded but results have not been fetched".to_string(),
                    remedy: "mcao cloud fetch".to_string(),
                },
                expected_exit_code: 4,
            },
            ExpectedCase {
                status: RunStatus::Succeeded,
                cancel_cloud_flag: true,
                result_fetched: true,
                expected_decision: TeardownDecision::Proceed,
                expected_exit_code: 0,
            },
            // Failed (4 combinations): Proceed, exit code 0
            ExpectedCase {
                status: RunStatus::Failed,
                cancel_cloud_flag: false,
                result_fetched: false,
                expected_decision: TeardownDecision::Proceed,
                expected_exit_code: 0,
            },
            ExpectedCase {
                status: RunStatus::Failed,
                cancel_cloud_flag: false,
                result_fetched: true,
                expected_decision: TeardownDecision::Proceed,
                expected_exit_code: 0,
            },
            ExpectedCase {
                status: RunStatus::Failed,
                cancel_cloud_flag: true,
                result_fetched: false,
                expected_decision: TeardownDecision::Proceed,
                expected_exit_code: 0,
            },
            ExpectedCase {
                status: RunStatus::Failed,
                cancel_cloud_flag: true,
                result_fetched: true,
                expected_decision: TeardownDecision::Proceed,
                expected_exit_code: 0,
            },
            // TimedOut (4 combinations): Proceed, exit code 0
            ExpectedCase {
                status: RunStatus::TimedOut,
                cancel_cloud_flag: false,
                result_fetched: false,
                expected_decision: TeardownDecision::Proceed,
                expected_exit_code: 0,
            },
            ExpectedCase {
                status: RunStatus::TimedOut,
                cancel_cloud_flag: false,
                result_fetched: true,
                expected_decision: TeardownDecision::Proceed,
                expected_exit_code: 0,
            },
            ExpectedCase {
                status: RunStatus::TimedOut,
                cancel_cloud_flag: true,
                result_fetched: false,
                expected_decision: TeardownDecision::Proceed,
                expected_exit_code: 0,
            },
            ExpectedCase {
                status: RunStatus::TimedOut,
                cancel_cloud_flag: true,
                result_fetched: true,
                expected_decision: TeardownDecision::Proceed,
                expected_exit_code: 0,
            },
            // Cancelled (4 combinations): Proceed, exit code 0
            ExpectedCase {
                status: RunStatus::Cancelled,
                cancel_cloud_flag: false,
                result_fetched: false,
                expected_decision: TeardownDecision::Proceed,
                expected_exit_code: 0,
            },
            ExpectedCase {
                status: RunStatus::Cancelled,
                cancel_cloud_flag: false,
                result_fetched: true,
                expected_decision: TeardownDecision::Proceed,
                expected_exit_code: 0,
            },
            ExpectedCase {
                status: RunStatus::Cancelled,
                cancel_cloud_flag: true,
                result_fetched: false,
                expected_decision: TeardownDecision::Proceed,
                expected_exit_code: 0,
            },
            ExpectedCase {
                status: RunStatus::Cancelled,
                cancel_cloud_flag: true,
                result_fetched: true,
                expected_decision: TeardownDecision::Proceed,
                expected_exit_code: 0,
            },
        ];

        assert_eq!(table.len(), 24);

        // Verify exhaustive 6 x 2 x 2 coverage across all dimensions.
        let all_statuses = [
            RunStatus::NonTerminal,
            RunStatus::Succeeded,
            RunStatus::Failed,
            RunStatus::TimedOut,
            RunStatus::Cancelled,
            RunStatus::Unreachable,
        ];
        for status in all_statuses {
            for cancel_cloud_flag in [false, true] {
                for result_fetched in [false, true] {
                    let count = table
                        .iter()
                        .filter(|c| {
                            c.status == status
                                && c.cancel_cloud_flag == cancel_cloud_flag
                                && c.result_fetched == result_fetched
                        })
                        .count();
                    assert_eq!(
                        count, 1,
                        "table must contain exactly 1 entry for ({status:?}, {cancel_cloud_flag}, {result_fetched})"
                    );
                }
            }
        }

        // Test each case in the table against decide and exit_code.
        for case in &table {
            let facts = TeardownFacts {
                status: case.status,
                cancel_cloud_flag: case.cancel_cloud_flag,
                result_fetched: case.result_fetched,
            };
            let decision = decide(&facts);
            assert_eq!(
                decision, case.expected_decision,
                "decision mismatch for facts {facts:?}"
            );
            assert_eq!(
                exit_code(&decision),
                case.expected_exit_code,
                "exit code mismatch for decision {decision:?}"
            );
        }
    }

    #[test]
    fn test_exit_codes() {
        assert_eq!(exit_code(&TeardownDecision::Proceed), 0);
        assert_eq!(exit_code(&TeardownDecision::CancelThenProceed), 0);
        assert_eq!(
            exit_code(&TeardownDecision::Refused {
                reason: "any non-empty reason".to_string(),
                remedy: "any non-empty remedy".to_string(),
            }),
            4
        );
        assert_eq!(
            exit_code(&TeardownDecision::Uncertain {
                reason: "any non-empty reason".to_string(),
            }),
            5
        );
    }

    #[test]
    fn test_reasons_and_remedies_non_empty_and_named_remedies_exact() {
        let all_statuses = [
            RunStatus::NonTerminal,
            RunStatus::Succeeded,
            RunStatus::Failed,
            RunStatus::TimedOut,
            RunStatus::Cancelled,
            RunStatus::Unreachable,
        ];

        for status in all_statuses {
            for cancel_cloud_flag in [false, true] {
                for result_fetched in [false, true] {
                    let facts = TeardownFacts {
                        status,
                        cancel_cloud_flag,
                        result_fetched,
                    };
                    let decision = decide(&facts);
                    match decision {
                        TeardownDecision::Refused { reason, remedy } => {
                            assert!(
                                !reason.is_empty(),
                                "refused reason must be non-empty for {facts:?}"
                            );
                            assert!(
                                !remedy.is_empty(),
                                "refused remedy must be non-empty for {facts:?}"
                            );
                            if status == RunStatus::NonTerminal && !cancel_cloud_flag {
                                assert_eq!(
                                    remedy, "rerun with --cancel-cloud",
                                    "exact remedy mismatch for NonTerminal without flag"
                                );
                            } else if status == RunStatus::Succeeded && !result_fetched {
                                assert_eq!(
                                    remedy, "mcao cloud fetch",
                                    "exact remedy mismatch for Succeeded without fetch"
                                );
                            } else {
                                panic!("unexpected refusal for facts: {facts:?}");
                            }
                        }
                        TeardownDecision::Uncertain { reason } => {
                            assert!(
                                !reason.is_empty(),
                                "uncertain reason must be non-empty for {facts:?}"
                            );
                            assert!(
                                reason.contains("run status could not be read"),
                                "uncertain reason must say run status could not be read: {reason}"
                            );
                        }
                        TeardownDecision::Proceed | TeardownDecision::CancelThenProceed => {}
                    }
                }
            }
        }
    }

    #[test]
    fn test_cancel_cloud_flag_ignored_for_terminal_runs() {
        let terminal_statuses = [
            RunStatus::Succeeded,
            RunStatus::Failed,
            RunStatus::TimedOut,
            RunStatus::Cancelled,
        ];
        for status in terminal_statuses {
            for result_fetched in [false, true] {
                let without_flag = TeardownFacts {
                    status,
                    cancel_cloud_flag: false,
                    result_fetched,
                };
                let with_flag = TeardownFacts {
                    status,
                    cancel_cloud_flag: true,
                    result_fetched,
                };
                assert_eq!(
                    decide(&without_flag),
                    decide(&with_flag),
                    "cancel_cloud_flag must be ignored for terminal run status {status:?}"
                );
            }
        }
    }
}
