//! Pure decision logic of the DPES-757 governed merge gate.
//!
//! Provides the forge reply classification and the outcome types of a merge attempt. The public
//! merge decision is [`crate::governance::govern_merge`]; the caller-asserted facts check in this
//! module is crate-private.

use std::error::Error;
use std::fmt;

/// Reasons why a governed merge is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeRefusal {
    /// Merge authority was not present.
    NoAuthority,
    /// An open decision blocks the merge.
    OpenDecision {
        /// Key identifying the blocking open decision.
        key: String,
    },
    /// A task is held, blocking the merge.
    TaskHeld {
        /// Task identifier of the held task.
        task_id: String,
    },
    /// The pull request is not open or is marked as a draft.
    PrNotOpen,
    /// The proof head commit does not match the pull request head commit.
    StaleHead {
        /// Proof head commit hash.
        proof_head: String,
        /// Pull request head commit hash.
        pr_head: String,
    },
    /// Required checks are not green.
    ChecksNotGreen,
    /// The attestation is not signed.
    InvalidAttestation,
    /// The task lock is not provably stale.
    LockNotStale,
    /// The approval record failed to persist.
    PersistenceFailure(String),
    /// The forge rejected the merge attempt.
    MergeFailed(String),
}

impl MergeRefusal {
    /// Returns the process exit code for this refusal variant (always 4).
    pub fn exit_code(&self) -> i32 {
        4
    }
}

impl fmt::Display for MergeRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoAuthority => write!(f, "merge authority absent"),
            Self::OpenDecision { key } => write!(f, "open decision {key}"),
            Self::TaskHeld { task_id } => write!(f, "task {task_id} is held"),
            Self::PrNotOpen => write!(f, "pull request is not open or is a draft"),
            Self::StaleHead {
                proof_head,
                pr_head,
            } => {
                write!(
                    f,
                    "proof head {proof_head} does not match pull request head {pr_head}"
                )
            }
            Self::ChecksNotGreen => write!(f, "required checks are not green"),
            Self::InvalidAttestation => write!(f, "attestation is not signed"),
            Self::LockNotStale => write!(f, "task lock is not provably stale"),
            Self::PersistenceFailure(reason) => {
                write!(f, "approval record not persisted: {reason}")
            }
            Self::MergeFailed(reason) => write!(f, "forge rejected the merge: {reason}"),
        }
    }
}

impl Error for MergeRefusal {}

/// Evidence regarding the state of the task lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockEvidence {
    /// The task lock is provably stale and can be superseded.
    ProvablyStale,
    /// No lock file exists.
    Missing,
    /// The lock is actively held.
    Held,
    /// The lock status could not be determined.
    Unknown,
}

/// Gate state evaluated prior to merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateState {
    /// Gate is eligible for merge.
    Eligible,
    /// Blocked by an open decision.
    OpenDecision {
        /// Key identifying the open decision.
        key: String,
    },
    /// Blocked by a held task.
    Held {
        /// Task identifier of the held task.
        task_id: String,
    },
}

/// Observed facts about the merge request and repository state, as asserted by the caller.
/// Crate-private, so it cannot back a public merge decision.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MergeFacts {
    /// Whether the actor possesses merge authority.
    pub has_authority: bool,
    /// Current gate state.
    pub gate: GateState,
    /// Whether the pull request is open.
    pub pr_is_open: bool,
    /// Whether the pull request is marked as a draft.
    pub pr_is_draft: bool,
    /// Head commit hash of the pull request.
    pub pr_head: String,
    /// Head commit hash verified in proof.
    pub proof_head: String,
    /// Whether required CI/CD checks are green.
    pub checks_green: bool,
    /// Whether the attestation has a valid signature.
    pub attestation_signed: bool,
    /// Evidence regarding the task lock.
    pub lock: LockEvidence,
}

/// Evaluates pre-merge facts in order and returns the first refusal, if any.
///
/// Refusal checks are evaluated in the following strict order:
/// 1. Absence of merge authority (`MergeRefusal::NoAuthority`).
/// 2. Active gate state (`MergeRefusal::OpenDecision` or `MergeRefusal::TaskHeld`).
/// 3. Closed or draft pull request (`MergeRefusal::PrNotOpen`).
/// 4. Proof head mismatch or empty head (`MergeRefusal::StaleHead`).
/// 5. Non-green checks (`MergeRefusal::ChecksNotGreen`).
/// 6. Unsigned attestation (`MergeRefusal::InvalidAttestation`).
/// 7. Non-stale task lock (`MergeRefusal::LockNotStale`).
///
/// Crate-private: a caller-asserted check cannot decide a merge. It has no production caller;
/// `governance::govern_merge` decides from the governance records and the live forge state, and
/// the in-crate tests pin the first-refusal precedence that governance does not implement.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn check_merge(facts: &MergeFacts) -> Result<(), MergeRefusal> {
    if !facts.has_authority {
        return Err(MergeRefusal::NoAuthority);
    }

    match &facts.gate {
        GateState::OpenDecision { key } => {
            return Err(MergeRefusal::OpenDecision { key: key.clone() });
        }
        GateState::Held { task_id } => {
            return Err(MergeRefusal::TaskHeld {
                task_id: task_id.clone(),
            });
        }
        GateState::Eligible => {}
    }

    if !facts.pr_is_open || facts.pr_is_draft {
        return Err(MergeRefusal::PrNotOpen);
    }

    if facts.pr_head.is_empty() || facts.proof_head.is_empty() || facts.pr_head != facts.proof_head
    {
        return Err(MergeRefusal::StaleHead {
            proof_head: facts.proof_head.clone(),
            pr_head: facts.pr_head.clone(),
        });
    }

    if !facts.checks_green {
        return Err(MergeRefusal::ChecksNotGreen);
    }

    if !facts.attestation_signed {
        return Err(MergeRefusal::InvalidAttestation);
    }

    match facts.lock {
        LockEvidence::ProvablyStale | LockEvidence::Missing => {}
        LockEvidence::Held | LockEvidence::Unknown => {
            return Err(MergeRefusal::LockNotStale);
        }
    }

    Ok(())
}

/// Response returned by the forge merge API call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForgeReply {
    /// Forge reported success.
    Success,
    /// Forge rejected the merge call with a failure message.
    Failed(String),
    /// Forge returned an ambiguous response with a message.
    Ambiguous(String),
}

/// Live repository state verified after attempting a merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveState {
    /// The pull request is confirmed merged at the given commit SHA.
    Merged {
        /// Commit hash of the merged commit.
        merge_sha: String,
        /// Head commit of the pull request that was merged, as the forge reports it, or `None`
        /// when the forge does not report it.
        merged_head: Option<String>,
    },
    /// The pull request is confirmed not merged.
    NotMerged,
    /// The pull request state could not be read.
    Unreadable,
}

/// Classified outcome of a merge attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    /// Merge confirmed successful.
    Merged {
        /// Commit hash of the merged commit.
        merge_sha: String,
    },
    /// Merge was refused.
    Refused(MergeRefusal),
    /// Merge outcome is uncertain.
    Uncertain(String),
}

impl MergeOutcome {
    /// Returns the process exit code corresponding to this merge outcome.
    ///
    /// - `Merged` yields `0`
    /// - `Refused` yields `4`
    /// - `Uncertain` yields `5`
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Merged { .. } => 0,
            Self::Refused(_) => 4,
            Self::Uncertain(_) => 5,
        }
    }

    /// Returns `true` if this outcome permits the task to succeed.
    ///
    /// Only `Merged` permits task success.
    pub fn permits_task_success(&self) -> bool {
        matches!(self, Self::Merged { .. })
    }
}

/// Classifies the merge outcome by reconciling the forge API reply with the live repository state.
pub fn classify_outcome(reply: &ForgeReply, live: &LiveState) -> MergeOutcome {
    match live {
        LiveState::Merged { merge_sha, .. } => MergeOutcome::Merged {
            merge_sha: merge_sha.clone(),
        },
        LiveState::NotMerged => match reply {
            ForgeReply::Success => MergeOutcome::Uncertain(
                "forge reported success but the pull request is not merged".to_string(),
            ),
            ForgeReply::Failed(m) => MergeOutcome::Refused(MergeRefusal::MergeFailed(m.clone())),
            ForgeReply::Ambiguous(m) => MergeOutcome::Uncertain(m.clone()),
        },
        LiveState::Unreadable => match reply {
            ForgeReply::Success => MergeOutcome::Uncertain(
                "forge reported success but the pull request state is unreadable".to_string(),
            ),
            ForgeReply::Failed(m) => MergeOutcome::Uncertain(m.clone()),
            ForgeReply::Ambiguous(m) => MergeOutcome::Uncertain(m.clone()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_facts() -> MergeFacts {
        MergeFacts {
            has_authority: true,
            gate: GateState::Eligible,
            pr_is_open: true,
            pr_is_draft: false,
            pr_head: "0123456789abcdef".to_string(),
            proof_head: "0123456789abcdef".to_string(),
            checks_green: true,
            attestation_signed: true,
            lock: LockEvidence::ProvablyStale,
        }
    }

    #[test]
    fn test_all_good_facts_returns_ok() {
        let facts = sample_facts();
        assert_eq!(check_merge(&facts), Ok(()));

        let mut facts_missing_lock = sample_facts();
        facts_missing_lock.lock = LockEvidence::Missing;
        assert_eq!(check_merge(&facts_missing_lock), Ok(()));
    }

    #[test]
    fn test_seven_failures_alone() {
        // 1. Authority absent
        let mut f1 = sample_facts();
        f1.has_authority = false;
        assert_eq!(check_merge(&f1), Err(MergeRefusal::NoAuthority));

        // 2a. Gate: OpenDecision
        let mut f2a = sample_facts();
        f2a.gate = GateState::OpenDecision {
            key: "DEC-100".to_string(),
        };
        assert_eq!(
            check_merge(&f2a),
            Err(MergeRefusal::OpenDecision {
                key: "DEC-100".to_string()
            })
        );

        // 2b. Gate: Held
        let mut f2b = sample_facts();
        f2b.gate = GateState::Held {
            task_id: "TASK-200".to_string(),
        };
        assert_eq!(
            check_merge(&f2b),
            Err(MergeRefusal::TaskHeld {
                task_id: "TASK-200".to_string()
            })
        );

        // 3a. PR closed
        let mut f3a = sample_facts();
        f3a.pr_is_open = false;
        assert_eq!(check_merge(&f3a), Err(MergeRefusal::PrNotOpen));

        // 3b. PR draft
        let mut f3b = sample_facts();
        f3b.pr_is_draft = true;
        assert_eq!(check_merge(&f3b), Err(MergeRefusal::PrNotOpen));

        // 4. Stale head
        let mut f4 = sample_facts();
        f4.proof_head = "mismatched_sha".to_string();
        assert_eq!(
            check_merge(&f4),
            Err(MergeRefusal::StaleHead {
                proof_head: "mismatched_sha".to_string(),
                pr_head: "0123456789abcdef".to_string(),
            })
        );

        // 5. Checks not green
        let mut f5 = sample_facts();
        f5.checks_green = false;
        assert_eq!(check_merge(&f5), Err(MergeRefusal::ChecksNotGreen));

        // 6. Attestation not signed
        let mut f6 = sample_facts();
        f6.attestation_signed = false;
        assert_eq!(check_merge(&f6), Err(MergeRefusal::InvalidAttestation));

        // 7a. Lock held
        let mut f7a = sample_facts();
        f7a.lock = LockEvidence::Held;
        assert_eq!(check_merge(&f7a), Err(MergeRefusal::LockNotStale));

        // 7b. Lock unknown
        let mut f7b = sample_facts();
        f7b.lock = LockEvidence::Unknown;
        assert_eq!(check_merge(&f7b), Err(MergeRefusal::LockNotStale));
    }

    #[test]
    fn test_refusal_priority_pairs() {
        // Authority beats Gate
        let mut f = sample_facts();
        f.has_authority = false;
        f.gate = GateState::OpenDecision {
            key: "DEC-1".to_string(),
        };
        assert_eq!(check_merge(&f), Err(MergeRefusal::NoAuthority));

        let mut f = sample_facts();
        f.has_authority = false;
        f.gate = GateState::Held {
            task_id: "T-1".to_string(),
        };
        assert_eq!(check_merge(&f), Err(MergeRefusal::NoAuthority));

        // Gate beats PR state
        let mut f = sample_facts();
        f.gate = GateState::OpenDecision {
            key: "DEC-1".to_string(),
        };
        f.pr_is_open = false;
        assert_eq!(
            check_merge(&f),
            Err(MergeRefusal::OpenDecision {
                key: "DEC-1".to_string()
            })
        );

        let mut f = sample_facts();
        f.gate = GateState::Held {
            task_id: "T-1".to_string(),
        };
        f.pr_is_draft = true;
        assert_eq!(
            check_merge(&f),
            Err(MergeRefusal::TaskHeld {
                task_id: "T-1".to_string()
            })
        );

        // PR state beats Stale Head
        let mut f = sample_facts();
        f.pr_is_open = false;
        f.proof_head = "different".to_string();
        assert_eq!(check_merge(&f), Err(MergeRefusal::PrNotOpen));

        let mut f = sample_facts();
        f.pr_is_draft = true;
        f.proof_head = "different".to_string();
        assert_eq!(check_merge(&f), Err(MergeRefusal::PrNotOpen));

        // Stale Head beats Checks
        let mut f = sample_facts();
        f.proof_head = "different".to_string();
        f.checks_green = false;
        assert_eq!(
            check_merge(&f),
            Err(MergeRefusal::StaleHead {
                proof_head: "different".to_string(),
                pr_head: "0123456789abcdef".to_string(),
            })
        );

        // Checks beat Attestation
        let mut f = sample_facts();
        f.checks_green = false;
        f.attestation_signed = false;
        assert_eq!(check_merge(&f), Err(MergeRefusal::ChecksNotGreen));

        // Attestation beats Lock
        let mut f = sample_facts();
        f.attestation_signed = false;
        f.lock = LockEvidence::Held;
        assert_eq!(check_merge(&f), Err(MergeRefusal::InvalidAttestation));

        let mut f = sample_facts();
        f.attestation_signed = false;
        f.lock = LockEvidence::Unknown;
        assert_eq!(check_merge(&f), Err(MergeRefusal::InvalidAttestation));
    }

    #[test]
    fn test_all_failure_pairs_matrix() {
        // Test all pairs (i < j) among the 7 failure types
        fn apply_failure(facts: &mut MergeFacts, step: usize) {
            match step {
                1 => facts.has_authority = false,
                2 => {
                    facts.gate = GateState::OpenDecision {
                        key: "BLOCK-1".to_string(),
                    }
                }
                3 => facts.pr_is_open = false,
                4 => facts.proof_head = "mismatch".to_string(),
                5 => facts.checks_green = false,
                6 => facts.attestation_signed = false,
                7 => facts.lock = LockEvidence::Held,
                _ => {}
            }
        }

        fn expected_refusal(step: usize, facts: &MergeFacts) -> MergeRefusal {
            match step {
                1 => MergeRefusal::NoAuthority,
                2 => MergeRefusal::OpenDecision {
                    key: "BLOCK-1".to_string(),
                },
                3 => MergeRefusal::PrNotOpen,
                4 => MergeRefusal::StaleHead {
                    proof_head: facts.proof_head.clone(),
                    pr_head: facts.pr_head.clone(),
                },
                5 => MergeRefusal::ChecksNotGreen,
                6 => MergeRefusal::InvalidAttestation,
                7 => MergeRefusal::LockNotStale,
                _ => MergeRefusal::NoAuthority,
            }
        }

        for i in 1..=6 {
            for j in (i + 1)..=7 {
                let mut facts = sample_facts();
                apply_failure(&mut facts, i);
                apply_failure(&mut facts, j);
                let expected = expected_refusal(i, &facts);
                assert_eq!(
                    check_merge(&facts),
                    Err(expected),
                    "Failure {i} must take precedence over failure {j}"
                );
            }
        }
    }

    #[test]
    fn test_draft_and_closed_pr() {
        let mut f_draft = sample_facts();
        f_draft.pr_is_open = true;
        f_draft.pr_is_draft = true;
        assert_eq!(check_merge(&f_draft), Err(MergeRefusal::PrNotOpen));

        let mut f_closed = sample_facts();
        f_closed.pr_is_open = false;
        f_closed.pr_is_draft = false;
        assert_eq!(check_merge(&f_closed), Err(MergeRefusal::PrNotOpen));

        let mut f_both = sample_facts();
        f_both.pr_is_open = false;
        f_both.pr_is_draft = true;
        assert_eq!(check_merge(&f_both), Err(MergeRefusal::PrNotOpen));
    }

    #[test]
    fn test_empty_heads() {
        // Equal but empty
        let mut f = sample_facts();
        f.pr_head = String::new();
        f.proof_head = String::new();
        assert_eq!(
            check_merge(&f),
            Err(MergeRefusal::StaleHead {
                proof_head: String::new(),
                pr_head: String::new(),
            })
        );

        // One empty, one non-empty
        let mut f1 = sample_facts();
        f1.pr_head = "abc".to_string();
        f1.proof_head = String::new();
        assert_eq!(
            check_merge(&f1),
            Err(MergeRefusal::StaleHead {
                proof_head: String::new(),
                pr_head: "abc".to_string(),
            })
        );

        let mut f2 = sample_facts();
        f2.pr_head = String::new();
        f2.proof_head = "abc".to_string();
        assert_eq!(
            check_merge(&f2),
            Err(MergeRefusal::StaleHead {
                proof_head: "abc".to_string(),
                pr_head: String::new(),
            })
        );
    }

    #[test]
    fn test_case_differing_heads() {
        let mut f = sample_facts();
        f.pr_head = "0123456789abcdef".to_string();
        f.proof_head = "0123456789ABCDEF".to_string();
        assert_eq!(
            check_merge(&f),
            Err(MergeRefusal::StaleHead {
                proof_head: "0123456789ABCDEF".to_string(),
                pr_head: "0123456789abcdef".to_string(),
            })
        );
    }

    #[test]
    fn test_lock_evidence_variants() {
        let mut f = sample_facts();
        f.lock = LockEvidence::ProvablyStale;
        assert_eq!(check_merge(&f), Ok(()));

        f.lock = LockEvidence::Missing;
        assert_eq!(check_merge(&f), Ok(()));

        f.lock = LockEvidence::Held;
        assert_eq!(check_merge(&f), Err(MergeRefusal::LockNotStale));

        f.lock = LockEvidence::Unknown;
        assert_eq!(check_merge(&f), Err(MergeRefusal::LockNotStale));
    }

    #[test]
    fn test_classify_outcome_3x3_table() {
        let sha = "deadbeef1234".to_string();
        let fail_msg = "merge conflict in Cargo.lock".to_string();
        let amb_msg = "network timeout talking to forge".to_string();

        // 1. Success + Merged => Merged
        assert_eq!(
            classify_outcome(
                &ForgeReply::Success,
                &LiveState::Merged {
                    merge_sha: sha.clone(),
                    merged_head: None,
                }
            ),
            MergeOutcome::Merged {
                merge_sha: sha.clone()
            }
        );

        // 2. Success + NotMerged => Uncertain
        assert_eq!(
            classify_outcome(&ForgeReply::Success, &LiveState::NotMerged),
            MergeOutcome::Uncertain(
                "forge reported success but the pull request is not merged".to_string()
            )
        );

        // 3. Success + Unreadable => Uncertain
        assert_eq!(
            classify_outcome(&ForgeReply::Success, &LiveState::Unreadable),
            MergeOutcome::Uncertain(
                "forge reported success but the pull request state is unreadable".to_string()
            )
        );

        // 4. Failed + Merged => Merged
        assert_eq!(
            classify_outcome(
                &ForgeReply::Failed(fail_msg.clone()),
                &LiveState::Merged {
                    merge_sha: sha.clone(),
                    merged_head: None,
                }
            ),
            MergeOutcome::Merged {
                merge_sha: sha.clone()
            }
        );

        // 5. Failed + NotMerged => Refused(MergeFailed)
        assert_eq!(
            classify_outcome(&ForgeReply::Failed(fail_msg.clone()), &LiveState::NotMerged),
            MergeOutcome::Refused(MergeRefusal::MergeFailed(fail_msg.clone()))
        );

        // 6. Failed + Unreadable => Uncertain
        assert_eq!(
            classify_outcome(
                &ForgeReply::Failed(fail_msg.clone()),
                &LiveState::Unreadable
            ),
            MergeOutcome::Uncertain(fail_msg.clone())
        );

        // 7. Ambiguous + Merged => Merged
        assert_eq!(
            classify_outcome(
                &ForgeReply::Ambiguous(amb_msg.clone()),
                &LiveState::Merged {
                    merge_sha: sha.clone(),
                    merged_head: None,
                }
            ),
            MergeOutcome::Merged {
                merge_sha: sha.clone()
            }
        );

        // 8. Ambiguous + NotMerged => Uncertain
        assert_eq!(
            classify_outcome(
                &ForgeReply::Ambiguous(amb_msg.clone()),
                &LiveState::NotMerged
            ),
            MergeOutcome::Uncertain(amb_msg.clone())
        );

        // 9. Ambiguous + Unreadable => Uncertain
        assert_eq!(
            classify_outcome(
                &ForgeReply::Ambiguous(amb_msg.clone()),
                &LiveState::Unreadable
            ),
            MergeOutcome::Uncertain(amb_msg)
        );
    }

    #[test]
    fn test_exit_codes() {
        assert_eq!(MergeRefusal::NoAuthority.exit_code(), 4);
        assert_eq!(
            MergeRefusal::OpenDecision {
                key: "k".to_string()
            }
            .exit_code(),
            4
        );
        assert_eq!(
            MergeRefusal::TaskHeld {
                task_id: "t".to_string()
            }
            .exit_code(),
            4
        );
        assert_eq!(MergeRefusal::PrNotOpen.exit_code(), 4);
        assert_eq!(
            MergeRefusal::StaleHead {
                proof_head: "a".to_string(),
                pr_head: "b".to_string(),
            }
            .exit_code(),
            4
        );
        assert_eq!(MergeRefusal::ChecksNotGreen.exit_code(), 4);
        assert_eq!(MergeRefusal::InvalidAttestation.exit_code(), 4);
        assert_eq!(MergeRefusal::LockNotStale.exit_code(), 4);
        assert_eq!(
            MergeRefusal::PersistenceFailure("err".to_string()).exit_code(),
            4
        );
        assert_eq!(MergeRefusal::MergeFailed("err".to_string()).exit_code(), 4);

        assert_eq!(
            MergeOutcome::Merged {
                merge_sha: "sha".to_string()
            }
            .exit_code(),
            0
        );
        assert_eq!(
            MergeOutcome::Refused(MergeRefusal::NoAuthority).exit_code(),
            4
        );
        assert_eq!(
            MergeOutcome::Refused(MergeRefusal::MergeFailed("err".to_string())).exit_code(),
            4
        );
        assert_eq!(
            MergeOutcome::Uncertain("unknown state".to_string()).exit_code(),
            5
        );
    }

    #[test]
    fn test_permits_task_success() {
        assert!(
            MergeOutcome::Merged {
                merge_sha: "abc".to_string()
            }
            .permits_task_success()
        );
        assert!(!MergeOutcome::Refused(MergeRefusal::NoAuthority).permits_task_success());
        assert!(!MergeOutcome::Refused(MergeRefusal::ChecksNotGreen).permits_task_success());
        assert!(!MergeOutcome::Uncertain("maybe".to_string()).permits_task_success());
    }

    #[test]
    fn test_display_text_all_variants() {
        assert_eq!(
            MergeRefusal::NoAuthority.to_string(),
            "merge authority absent"
        );
        assert_eq!(
            MergeRefusal::OpenDecision {
                key: "DEC-42".to_string()
            }
            .to_string(),
            "open decision DEC-42"
        );
        assert_eq!(
            MergeRefusal::TaskHeld {
                task_id: "TASK-99".to_string()
            }
            .to_string(),
            "task TASK-99 is held"
        );
        assert_eq!(
            MergeRefusal::PrNotOpen.to_string(),
            "pull request is not open or is a draft"
        );
        assert_eq!(
            MergeRefusal::StaleHead {
                proof_head: "sha-proof".to_string(),
                pr_head: "sha-pr".to_string(),
            }
            .to_string(),
            "proof head sha-proof does not match pull request head sha-pr"
        );
        assert_eq!(
            MergeRefusal::ChecksNotGreen.to_string(),
            "required checks are not green"
        );
        assert_eq!(
            MergeRefusal::InvalidAttestation.to_string(),
            "attestation is not signed"
        );
        assert_eq!(
            MergeRefusal::LockNotStale.to_string(),
            "task lock is not provably stale"
        );
        assert_eq!(
            MergeRefusal::PersistenceFailure("disk full".to_string()).to_string(),
            "approval record not persisted: disk full"
        );
        assert_eq!(
            MergeRefusal::MergeFailed("branch protected".to_string()).to_string(),
            "forge rejected the merge: branch protected"
        );
    }

    #[test]
    fn test_error_trait() {
        let err: Box<dyn Error> = Box::new(MergeRefusal::NoAuthority);
        assert_eq!(err.to_string(), "merge authority absent");
        assert!(err.source().is_none());
    }
}
