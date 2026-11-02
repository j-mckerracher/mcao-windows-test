//! DPES-757 governance port: the synchronous merge gate in front of the forge.
//!
//! [`govern_merge`] is the only public entry. It reads the task facts from
//! [`GovernanceRecords`] and the live pull request from [`ForgeClient`], decides from those reads
//! alone, and calls [`ForgeClient::merge`] once, and only after the decision allows it. The
//! caller supplies identifiers and explicit waivers only. Authority, gate state, proof,
//! attestation, lock, persistence, and forge state are not parameters; they come from the two
//! traits. Its refusal conditions follow the guards in `bin/fm-pr-merge.sh`.
//!
//! Trust boundary: the gate cannot verify the traits' answers. [`GovernanceRecords`] must read task
//! facts from the durable store, never fabricate them, and return a failed read as `Err`. An
//! adapter that reports authority its store does not hold gets [`GovernanceResult::Completed`] and
//! one forge merge call, and nothing in this module detects it. [`check_records_conformance`]
//! shows only that a store refuses the fixtures in [`refusal_fixtures`]; it does not show the store
//! is honest for any other state.
//!
//! Both traits are versioned and verified against in-memory mocks only. They are not wired to
//! any CLI command, and no native or integrated forge evidence exists for them yet. The head
//! binding of a merge is a [`ForgeClient`] obligation that [`check_head_binding`] exercises.

use crate::merge_gate::{
    ForgeReply, GateState, LiveState, LockEvidence, MergeOutcome, classify_outcome,
};

/// Contract version of [`ForgeClient`] that this gate was written against. Version 2 requires
/// `confirm_merged` to report the merged head.
pub const FORGE_CLIENT_VERSION: u32 = 2;

/// Contract version of [`GovernanceRecords`] that this gate was written against.
pub const GOVERNANCE_RECORDS_VERSION: u32 = 1;

/// A failed read from a governance record store or from the forge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadError(pub String);

/// Live state of one check run or status context at the snapshot's head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckObservation {
    /// Check name, matched exactly by a waiver.
    pub name: String,
    /// Whether the check's current run is green.
    pub green: bool,
    /// Producer of this observation, which decides how a producer binding reads it.
    pub producer: CheckProducer,
}

/// Producer of one observed check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckProducer {
    /// A commit status. It carries no producer app id, so a producer binding matches it by name.
    CommitStatus,
    /// A check run. `app_id` is `None` when the forge did not report the producing app.
    CheckRun {
        /// Id of the app that produced the check run, if it was reported.
        app_id: Option<u64>,
    },
}

/// A required check context, as the forge's branch protection or rulesets demand it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredContext {
    /// Required context name, matched exactly by a waiver.
    pub name: String,
    /// Id of the app that must produce the same-named check run. `None` matches by name alone.
    pub app_id: Option<u64>,
}

/// Live pull request state read from the forge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestSnapshot {
    /// Whether the pull request is open.
    pub is_open: bool,
    /// Whether the pull request is a draft.
    pub is_draft: bool,
    /// Whether the forge reports the pull request as mergeable with no conflicts. An adapter sets
    /// this false for GitHub `mergeable` UNKNOWN and for `mergeStateStatus` DIRTY, because the
    /// gate has no separate conflict field. `bin/fm-pr-merge.sh` refuses DIRTY on its own check.
    pub is_mergeable: bool,
    /// Live head commit of the pull request.
    pub head_sha: String,
    /// Checks reported at the live head.
    pub checks: Vec<CheckObservation>,
    /// Required contexts for the base branch.
    pub required_contexts: Vec<RequiredContext>,
}

/// The pull request a task is bound to, read from its recorded identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundPr {
    /// Canonical URL of the recorded pull request.
    pub url: String,
    /// Whether that pull request's merge has already been recorded. A merged binding frees the
    /// task for its next pull request.
    pub merged: bool,
}

/// Authoritative task facts read from the governance record store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRecord {
    /// Whether the record grants merge authority.
    pub has_merge_authority: bool,
    /// Open decision or hold recorded against the task.
    pub gate: GateState,
    /// Evidence about the task lock.
    pub lock: LockEvidence,
    /// Head commit recorded in the proof of work.
    pub proof_head: String,
    /// Whether the attestation carries a valid signature.
    pub attestation_signed: bool,
    /// Whether the away-posture record is present, which makes waivers attended-only.
    pub away_posture: bool,
    /// Pull request recorded as the task's identity, if one is bound.
    pub bound_pr: Option<BoundPr>,
}

/// Explicit, narrowly scoped waivers for one merge attempt.
///
/// Each names one check or context exactly, as `bin/fm-pr-merge.sh --allow-red` and
/// `--allow-missing` do. A waiver never relaxes the head binding or any other check.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MergeWaivers {
    /// Failing check run to waive by exact name.
    pub allow_red: Option<String>,
    /// Required context that has not reported, to waive by exact name.
    pub allow_missing: Option<String>,
}

/// One merge attempt requested by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeRequest<'a> {
    /// Task whose records govern the merge.
    pub task_id: &'a str,
    /// Canonical URL of the pull request to merge.
    pub pr_url: &'a str,
    /// Head commit the caller verified, which the merge must bind to.
    pub expected_head: &'a str,
    /// Waivers requested for this attempt.
    pub waivers: MergeWaivers,
}

/// Versioned forge contract. Every method is synchronous.
///
/// Adapter contract, relied on by [`govern_merge`] and exercised by [`check_head_binding`]:
/// - `read_pull_request` reports every check run and commit status at the live head in `checks`,
///   red or green. A check run carries its producer app id as `CheckProducer::CheckRun`; an
///   unreadable producer is `app_id: None`, which refuses any required context bound to an app. A
///   required context omitted from `checks` counts as unreported and refuses, so a red required
///   context cannot pass by omission. An omitted red check that is not required is not detectable
///   by the gate.
/// - `read_pull_request` `required_contexts` is the base branch's full required set from classic
///   branch protection and rulesets. When that set cannot be read, return `Err`, never an empty
///   list, which the gate reads as no requirement.
/// - `merge` completes only while the forge's live head equals `expected_head`, as
///   `gh pr merge --match-head-commit` enforces. The gate binds the head only through that
///   argument, so a forge that ignores it can merge a head the gate never verified.
/// - `confirm_merged` reports `LiveState::Merged` with `merged_head` set to the pull request head
///   commit that was merged (GitHub `headRefOid` of the merged pull request), or `None` when the
///   forge does not expose it. Only a `merged_head` equal to the verified head completes; `None`
///   and any other head are uncertain outcomes, never completions.
/// - `is_mergeable` maps GitHub `mergeStateStatus` DIRTY (conflicts) to false; see the field.
/// - Check states are reduced to `green` by the adapter, including neutral, skipped, pending, and
///   stale runs. The gate sees only `green` and trusts it as given, so that mapping is an adapter
///   obligation and is not decided here.
pub trait ForgeClient {
    /// Contract version the implementation speaks.
    fn version(&self) -> u32;
    /// Reads the live pull request at `pr_url`.
    fn read_pull_request(&self, pr_url: &str) -> Result<PullRequestSnapshot, ReadError>;
    /// Asks the forge to merge `pr_url` only while its head is `expected_head`.
    fn merge(&self, pr_url: &str, expected_head: &str) -> ForgeReply;
    /// Reads back whether `pr_url` is merged after a merge attempt.
    fn confirm_merged(&self, pr_url: &str) -> LiveState;
}

/// Versioned governance record contract, backed by durable state.
///
/// This trait is the only source of merge authority, gate state, proof, attestation, lock, away
/// posture, and bound pull request for [`govern_merge`]. An implementation must read these facts
/// from the durable store, never fabricate or default a field, and return a failed read as `Err`.
/// It must also fail `probe_persistence` when the approval record cannot be written.
///
/// The gate cannot detect an implementation that reports authority its store does not hold; such
/// an implementation gets [`GovernanceResult::Completed`] with one forge merge call. Run
/// [`check_records_conformance`] against an implementation to test that it refuses the fixtures in
/// [`refusal_fixtures`].
pub trait GovernanceRecords {
    /// Contract version the implementation speaks.
    fn version(&self) -> u32;
    /// Reads the authoritative facts for `task_id`.
    fn read_task(&self, task_id: &str) -> Result<TaskRecord, ReadError>;
    /// Confirms that the approval record for `task_id` can be persisted.
    fn probe_persistence(&self, task_id: &str) -> Result<(), ReadError>;
}

/// A condition that refuses a merge. Every failing condition is reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GovernanceRefusal {
    /// The forge client speaks a different contract version.
    ForgeVersionMismatch {
        /// Version this gate requires.
        expected: u32,
        /// Version the client reported.
        found: u32,
    },
    /// The record store speaks a different contract version.
    RecordsVersionMismatch {
        /// Version this gate requires.
        expected: u32,
        /// Version the store reported.
        found: u32,
    },
    /// The task record could not be read.
    RecordUnreadable(String),
    /// The pull request could not be read.
    ForgeUnreadable(String),
    /// The record grants no merge authority.
    NoAuthority,
    /// An open decision blocks the merge.
    OpenDecision {
        /// Key of the open decision.
        key: String,
    },
    /// The task is held pending an approval decision.
    TaskHeld {
        /// Held task.
        task_id: String,
    },
    /// The task lock is held or its status is unknown.
    LockNotStale,
    /// The attestation is not signed.
    InvalidAttestation,
    /// The task is bound to a different pull request that has not merged yet.
    TaskBoundToOtherPr {
        /// Pull request the task is bound to.
        bound_url: String,
        /// Pull request the caller requested.
        requested_url: String,
    },
    /// No head commit was supplied to bind the merge to.
    UnboundHead,
    /// A head is not a 40- or 64-character lowercase hex commit id, so the merge cannot be bound
    /// to it. Matches `fm_pr_head_valid` in bin/fm-pr-lib.sh.
    MalformedHead {
        /// Head that failed the check.
        head: String,
    },
    /// The proof was recorded for a different head than the one requested.
    ProofMismatch {
        /// Head recorded in the proof.
        proof_head: String,
        /// Head the caller requested.
        expected_head: String,
    },
    /// The live pull request head is not the requested head.
    HeadMismatch {
        /// Head the caller requested.
        expected_head: String,
        /// Live head of the pull request.
        live_head: String,
    },
    /// The pull request is closed or a draft.
    PrNotOpen,
    /// The forge does not report the pull request as mergeable.
    NotMergeable,
    /// A failing check is not covered by a waiver.
    UnwaivedFailingCheck {
        /// Failing check name.
        name: String,
    },
    /// A required context has not reported and is not covered by a waiver.
    MissingRequiredContext {
        /// Required context name.
        name: String,
    },
    /// A check run for a bound required context has an unreadable producer, so it cannot be
    /// matched to the app the requirement names. Not waivable.
    ProducerUnreadable {
        /// Required context name.
        name: String,
    },
    /// A waiver names no check: its name is empty or whitespace-only.
    EmptyWaiverName {
        /// Command-line flag that carried the blank name.
        flag: &'static str,
    },
    /// A waiver was requested while the away-posture record is present.
    WaiverWhileAway,
    /// The approval record cannot be persisted, so the merge is not allowed to start.
    PersistenceUnavailable(String),
}

/// Outcome of the crate-private decision that [`govern_merge`] applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GovernanceDecision {
    /// Every condition holds for the requested head.
    Allow,
    /// At least one condition fails. Never empty.
    Refuse(Vec<GovernanceRefusal>),
}

/// Result of [`govern_merge`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GovernanceResult {
    /// Refused before any forge merge call.
    Refused(Vec<GovernanceRefusal>),
    /// The forge merge was called but no merge is confirmed: a forge refusal or an uncertain
    /// outcome. Never a completion, and never retried by the gate.
    Attempted(MergeOutcome),
    /// The pull request is confirmed merged by a live read-back.
    Completed {
        /// Commit hash of the merged commit.
        merge_sha: String,
    },
}

/// Whether `head` is a full commit id: 40 or 64 lowercase hex characters, as
/// `fm_pr_head_valid` in bin/fm-pr-lib.sh requires.
fn is_commit_id(head: &str) -> bool {
    matches!(head.len(), 40 | 64)
        && head
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn push_once(refusals: &mut Vec<GovernanceRefusal>, refusal: GovernanceRefusal) {
    if !refusals.contains(&refusal) {
        refusals.push(refusal);
    }
}

/// Required contexts the gate enforces. A name-only entry is dropped when the same name also has
/// an app-bound entry, so a duplicate name-only requirement cannot weaken the binding.
fn effective_requirements(required: &[RequiredContext]) -> Vec<&RequiredContext> {
    required
        .iter()
        .filter(|candidate| {
            candidate.app_id.is_some()
                || !required
                    .iter()
                    .any(|other| other.name == candidate.name && other.app_id.is_some())
        })
        .collect()
}

/// Whether `checks` reports `required` at the live head. A commit status matches by name because
/// it carries no producer app id. A check run must come from the required app when one is named,
/// so a check run whose producer is unreadable does not satisfy a bound requirement.
fn reported_by(checks: &[CheckObservation], required: &RequiredContext) -> bool {
    checks.iter().any(|check| {
        check.name == required.name
            && match (check.producer, required.app_id) {
                (_, None) | (CheckProducer::CommitStatus, Some(_)) => true,
                (
                    CheckProducer::CheckRun {
                        app_id: Some(producer),
                    },
                    Some(bound),
                ) => producer == bound,
                (CheckProducer::CheckRun { app_id: None }, Some(_)) => false,
            }
    })
}

/// Whether a check run named by a bound `required` has an unreadable producer.
fn has_unreadable_producer(checks: &[CheckObservation], required: &RequiredContext) -> bool {
    required.app_id.is_some()
        && checks.iter().any(|check| {
            check.name == required.name
                && check.producer == CheckProducer::CheckRun { app_id: None }
        })
}

/// Decides whether `request` may merge, from the record and the live snapshot. Pure.
///
/// Crate-private so no public caller can supply its own record or snapshot as authority.
pub(crate) fn decide_merge(
    record: &TaskRecord,
    snapshot: &PullRequestSnapshot,
    request: &MergeRequest<'_>,
) -> GovernanceDecision {
    let mut refusals = Vec::new();

    if !record.has_merge_authority {
        push_once(&mut refusals, GovernanceRefusal::NoAuthority);
    }
    match &record.gate {
        GateState::OpenDecision { key } => push_once(
            &mut refusals,
            GovernanceRefusal::OpenDecision { key: key.clone() },
        ),
        GateState::Held { task_id } => push_once(
            &mut refusals,
            GovernanceRefusal::TaskHeld {
                task_id: task_id.clone(),
            },
        ),
        GateState::Eligible => {}
    }
    match record.lock {
        LockEvidence::ProvablyStale | LockEvidence::Missing => {}
        LockEvidence::Held | LockEvidence::Unknown => {
            push_once(&mut refusals, GovernanceRefusal::LockNotStale);
        }
    }
    if !record.attestation_signed {
        push_once(&mut refusals, GovernanceRefusal::InvalidAttestation);
    }
    // bin/fm-pr-merge.sh require_recorded_pr_identity: an existing pr= must equal the requested
    // URL unless that bound PR has already merged.
    if let Some(bound) = record
        .bound_pr
        .as_ref()
        .filter(|bound| !bound.merged && bound.url != request.pr_url)
    {
        push_once(
            &mut refusals,
            GovernanceRefusal::TaskBoundToOtherPr {
                bound_url: bound.url.clone(),
                requested_url: request.pr_url.to_string(),
            },
        );
    }

    if request.expected_head.is_empty() {
        push_once(&mut refusals, GovernanceRefusal::UnboundHead);
    } else {
        for head in [
            request.expected_head,
            record.proof_head.as_str(),
            snapshot.head_sha.as_str(),
        ] {
            if !is_commit_id(head) {
                push_once(
                    &mut refusals,
                    GovernanceRefusal::MalformedHead {
                        head: head.to_string(),
                    },
                );
            }
        }
        if record.proof_head != request.expected_head {
            push_once(
                &mut refusals,
                GovernanceRefusal::ProofMismatch {
                    proof_head: record.proof_head.clone(),
                    expected_head: request.expected_head.to_string(),
                },
            );
        }
        if snapshot.head_sha != request.expected_head {
            push_once(
                &mut refusals,
                GovernanceRefusal::HeadMismatch {
                    expected_head: request.expected_head.to_string(),
                    live_head: snapshot.head_sha.clone(),
                },
            );
        }
    }

    if !snapshot.is_open || snapshot.is_draft {
        push_once(&mut refusals, GovernanceRefusal::PrNotOpen);
    }
    if !snapshot.is_mergeable {
        push_once(&mut refusals, GovernanceRefusal::NotMergeable);
    }

    // bin/fm-pr-merge.sh refuses an empty --allow-red or --allow-missing name. A whitespace-only
    // name is refused here too, because no real check or context is named by blank text.
    for (flag, waiver) in [
        ("--allow-red", &request.waivers.allow_red),
        ("--allow-missing", &request.waivers.allow_missing),
    ] {
        if waiver.as_deref().is_some_and(|name| name.trim().is_empty()) {
            push_once(&mut refusals, GovernanceRefusal::EmptyWaiverName { flag });
        }
    }
    if record.away_posture && request.waivers != MergeWaivers::default() {
        push_once(&mut refusals, GovernanceRefusal::WaiverWhileAway);
    }
    for check in snapshot.checks.iter().filter(|check| !check.green) {
        if request.waivers.allow_red.as_deref() != Some(check.name.as_str()) {
            push_once(
                &mut refusals,
                GovernanceRefusal::UnwaivedFailingCheck {
                    name: check.name.clone(),
                },
            );
        }
    }
    for required in effective_requirements(&snapshot.required_contexts) {
        if reported_by(&snapshot.checks, required) {
            continue;
        }
        // bin/fm-pr-merge.sh refuses an unreadable producer whatever it waives, so this refusal
        // is not subject to allow_missing.
        if has_unreadable_producer(&snapshot.checks, required) {
            push_once(
                &mut refusals,
                GovernanceRefusal::ProducerUnreadable {
                    name: required.name.clone(),
                },
            );
        } else if request.waivers.allow_missing.as_deref() != Some(required.name.as_str()) {
            push_once(
                &mut refusals,
                GovernanceRefusal::MissingRequiredContext {
                    name: required.name.clone(),
                },
            );
        }
    }

    if refusals.is_empty() {
        GovernanceDecision::Allow
    } else {
        GovernanceDecision::Refuse(refusals)
    }
}

/// Reads governance state and the live pull request, decides, and only then merges.
///
/// The forge merge is called at most once. Its outcome is confirmed by a live read. Only a
/// confirmed merge is [`GovernanceResult::Completed`]; an unconfirmed or ambiguous outcome is
/// [`GovernanceResult::Attempted`] carrying [`MergeOutcome::Uncertain`].
///
/// The decision is not public, so a caller cannot pass its own record and snapshot to it:
///
/// ```compile_fail,E0603
/// use mcao_core::governance::decide_merge;
/// ```
///
/// The caller-asserted facts check in `merge_gate` is crate-private too, so no caller can decide a
/// merge from booleans it supplies:
///
/// ```compile_fail,E0603
/// use mcao_core::merge_gate::check_merge;
/// ```
pub fn govern_merge(
    records: &dyn GovernanceRecords,
    forge: &dyn ForgeClient,
    request: &MergeRequest<'_>,
) -> GovernanceResult {
    let mut refusals = Vec::new();
    if forge.version() != FORGE_CLIENT_VERSION {
        refusals.push(GovernanceRefusal::ForgeVersionMismatch {
            expected: FORGE_CLIENT_VERSION,
            found: forge.version(),
        });
    }
    if records.version() != GOVERNANCE_RECORDS_VERSION {
        refusals.push(GovernanceRefusal::RecordsVersionMismatch {
            expected: GOVERNANCE_RECORDS_VERSION,
            found: records.version(),
        });
    }
    if !refusals.is_empty() {
        return GovernanceResult::Refused(refusals);
    }

    let record = match records.read_task(request.task_id) {
        Ok(record) => record,
        Err(ReadError(reason)) => {
            return GovernanceResult::Refused(vec![GovernanceRefusal::RecordUnreadable(reason)]);
        }
    };
    let snapshot = match forge.read_pull_request(request.pr_url) {
        Ok(snapshot) => snapshot,
        Err(ReadError(reason)) => {
            return GovernanceResult::Refused(vec![GovernanceRefusal::ForgeUnreadable(reason)]);
        }
    };

    if let GovernanceDecision::Refuse(decided) = decide_merge(&record, &snapshot, request) {
        refusals.extend(decided);
    }
    if let Err(ReadError(reason)) = records.probe_persistence(request.task_id) {
        push_once(
            &mut refusals,
            GovernanceRefusal::PersistenceUnavailable(reason),
        );
    }
    if !refusals.is_empty() {
        return GovernanceResult::Refused(refusals);
    }

    let reply = forge.merge(request.pr_url, request.expected_head);
    let live = forge.confirm_merged(request.pr_url);
    match classify_against_head(&reply, &live, request.expected_head) {
        MergeOutcome::Merged { merge_sha } => GovernanceResult::Completed { merge_sha },
        outcome => GovernanceResult::Attempted(outcome),
    }
}

/// A forge that breaks a contract the gate depends on and cannot observe itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConformanceViolation {
    /// The pull request could not be read, so the head binding could not be exercised.
    Unreadable(String),
    /// `merge` completed a pull request while the requested head was not its live head.
    MergeIgnoredExpectedHead {
        /// Head the merge was requested at.
        expected_head: String,
        /// Live head of the pull request.
        live_head: String,
    },
    /// The merge at the stale head could not be read back, so conformance is not shown.
    BindingUnconfirmed {
        /// Head the merge was requested at.
        expected_head: String,
    },
}

/// Classifies a merge reply and read-back. A merged read-back completes only when it reports the
/// verified head; another merged head, or none, is an uncertain outcome.
fn classify_against_head(
    reply: &ForgeReply,
    live: &LiveState,
    expected_head: &str,
) -> MergeOutcome {
    if let LiveState::Merged { merged_head, .. } = live {
        return match merged_head {
            Some(head) if head.as_str() == expected_head => classify_outcome(reply, live),
            Some(head) => MergeOutcome::Uncertain(format!(
                "pull request merged at head {head}, not the verified head {expected_head}"
            )),
            None => MergeOutcome::Uncertain(
                "pull request merged but the merged head was not reported".to_string(),
            ),
        };
    }
    classify_outcome(reply, live)
}

/// Pull request that [`refusal_fixtures`] request a merge of.
const CONFORMANCE_PR_URL: &str = "https://github.com/example/repo/pull/1";
/// Head that [`refusal_fixtures`] request a merge at, and that their conforming record names.
const CONFORMANCE_HEAD: &str = "0123456789abcdef0123456789abcdef01234567";

/// A durable record state that a conforming [`GovernanceRecords`] must refuse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusalFixture {
    /// Name of the refused state, reported in a violation.
    pub label: &'static str,
    /// Task whose record the store must hold as `record`.
    pub task_id: &'static str,
    /// Pull request the merge is requested for.
    pub pr_url: &'static str,
    /// Head the merge is requested at.
    pub expected_head: &'static str,
    /// Durable state the store must hold under `task_id`.
    pub record: TaskRecord,
}

/// Record that authorizes a merge of [`CONFORMANCE_PR_URL`] at [`CONFORMANCE_HEAD`]. Each refusal
/// fixture is this record with one condition broken.
fn conforming_record() -> TaskRecord {
    TaskRecord {
        has_merge_authority: true,
        gate: GateState::Eligible,
        lock: LockEvidence::ProvablyStale,
        proof_head: CONFORMANCE_HEAD.to_string(),
        attestation_signed: true,
        away_posture: false,
        bound_pr: None,
    }
}

/// The durable states a conforming [`GovernanceRecords`] must refuse: no merge authority, a held
/// task, missing proof, an unsigned attestation, and a task bound to another pull request.
///
/// An adapter seeds its store so each fixture's `record` is held under its `task_id`, then passes
/// the table to [`check_records_conformance`].
pub fn refusal_fixtures() -> Vec<RefusalFixture> {
    let fixture = |label, task_id, record| RefusalFixture {
        label,
        task_id,
        pr_url: CONFORMANCE_PR_URL,
        expected_head: CONFORMANCE_HEAD,
        record,
    };
    let mut without_authority = conforming_record();
    without_authority.has_merge_authority = false;
    let mut held = conforming_record();
    held.gate = GateState::Held {
        task_id: "DPES-held".to_string(),
    };
    let mut missing_proof = conforming_record();
    missing_proof.proof_head = String::new();
    let mut unsigned = conforming_record();
    unsigned.attestation_signed = false;
    let mut wrong_pr = conforming_record();
    wrong_pr.bound_pr = Some(BoundPr {
        url: "https://github.com/example/repo/pull/2".to_string(),
        merged: false,
    });
    vec![
        fixture("no authority", "DPES-no-authority", without_authority),
        fixture("held task", "DPES-held-task", held),
        fixture("missing proof", "DPES-missing-proof", missing_proof),
        fixture("bad attestation", "DPES-bad-attestation", unsigned),
        fixture("wrong pr", "DPES-wrong-pr", wrong_pr),
    ]
}

/// A condition that a conformance check found the records authorized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordsViolation {
    /// The records did not refuse a fixture, so the merge was allowed or attempted.
    Authorized {
        /// Label of the fixture that must have been refused.
        label: &'static str,
    },
}

/// Honest forge for conformance: green, at one head, and merges that head.
struct ConformingForge<'a> {
    head: &'a str,
}

impl ForgeClient for ConformingForge<'_> {
    fn version(&self) -> u32 {
        FORGE_CLIENT_VERSION
    }
    fn read_pull_request(&self, _pr_url: &str) -> Result<PullRequestSnapshot, ReadError> {
        Ok(PullRequestSnapshot {
            is_open: true,
            is_draft: false,
            is_mergeable: true,
            head_sha: self.head.to_string(),
            checks: Vec::new(),
            required_contexts: Vec::new(),
        })
    }
    fn merge(&self, _pr_url: &str, _expected_head: &str) -> ForgeReply {
        ForgeReply::Success
    }
    fn confirm_merged(&self, _pr_url: &str) -> LiveState {
        LiveState::Merged {
            merge_sha: "conformance-merge".to_string(),
            merged_head: Some(self.head.to_string()),
        }
    }
}

/// Checks that `records` refuses every fixture in `fixtures`, and returns each fixture it does not.
///
/// For each fixture, [`govern_merge`] runs against an honest green forge at the fixture's head. A
/// fixture that is not refused is a [`RecordsViolation::Authorized`]. A read error counts as a
/// refusal. Passing shows only that the store refuses these fixtures, not that it is honest in any
/// other state (see [`GovernanceRecords`]).
pub fn check_records_conformance(
    records: &dyn GovernanceRecords,
    fixtures: &[RefusalFixture],
) -> Result<(), Vec<RecordsViolation>> {
    let violations: Vec<RecordsViolation> = fixtures
        .iter()
        .filter(|fixture| {
            let forge = ConformingForge {
                head: fixture.expected_head,
            };
            let request = MergeRequest {
                task_id: fixture.task_id,
                pr_url: fixture.pr_url,
                expected_head: fixture.expected_head,
                waivers: MergeWaivers::default(),
            };
            !matches!(
                govern_merge(records, &forge, &request),
                GovernanceResult::Refused(_)
            )
        })
        .map(|fixture| RecordsViolation::Authorized {
            label: fixture.label,
        })
        .collect();
    if violations.is_empty() {
        Ok(())
    } else {
        Err(violations)
    }
}

/// Checks the head-binding obligation of [`ForgeClient`]: a merge requested at a head other than
/// the live head must not complete. Adapter tests reuse this helper against their own forge.
///
/// It makes one merge call at a deliberately stale head, so run it only against a disposable
/// fixture or a mock. A conforming forge refuses that call without merging, and a read-back that
/// cannot be read is [`ConformanceViolation::BindingUnconfirmed`], never a pass.
pub fn check_head_binding(
    forge: &dyn ForgeClient,
    pr_url: &str,
) -> Result<(), ConformanceViolation> {
    let live_head = match forge.read_pull_request(pr_url) {
        Ok(snapshot) => snapshot.head_sha,
        Err(ReadError(reason)) => return Err(ConformanceViolation::Unreadable(reason)),
    };
    let expected_head = format!("{live_head}-stale");
    forge.merge(pr_url, &expected_head);
    match forge.confirm_merged(pr_url) {
        LiveState::Merged { .. } => Err(ConformanceViolation::MergeIgnoredExpectedHead {
            expected_head,
            live_head,
        }),
        LiveState::NotMerged => Ok(()),
        LiveState::Unreadable => Err(ConformanceViolation::BindingUnconfirmed { expected_head }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";
    const OTHER_HEAD: &str = "fedcba9876543210fedcba9876543210fedcba98";
    const PR_URL: &str = "https://github.com/example/repo/pull/1";
    const OTHER_PR_URL: &str = "https://github.com/example/repo/pull/2";
    const TASK: &str = "DPES-757";
    const BOUND_APP: u64 = 15;
    const OTHER_APP: u64 = 99;

    struct MockForge {
        version: u32,
        snapshot: Result<PullRequestSnapshot, ReadError>,
        reply: ForgeReply,
        live: LiveState,
        merge_calls: Cell<u32>,
        merge_heads: RefCell<Vec<String>>,
    }

    impl MockForge {
        fn new(snapshot: PullRequestSnapshot) -> Self {
            Self {
                version: FORGE_CLIENT_VERSION,
                snapshot: Ok(snapshot),
                reply: ForgeReply::Success,
                live: LiveState::Merged {
                    merge_sha: "merge-sha".to_string(),
                    merged_head: Some(HEAD.to_string()),
                },
                merge_calls: Cell::new(0),
                merge_heads: RefCell::new(Vec::new()),
            }
        }
    }

    impl ForgeClient for MockForge {
        fn version(&self) -> u32 {
            self.version
        }
        fn read_pull_request(&self, _pr_url: &str) -> Result<PullRequestSnapshot, ReadError> {
            self.snapshot.clone()
        }
        fn merge(&self, _pr_url: &str, expected_head: &str) -> ForgeReply {
            self.merge_calls.set(self.merge_calls.get() + 1);
            self.merge_heads
                .borrow_mut()
                .push(expected_head.to_string());
            self.reply.clone()
        }
        fn confirm_merged(&self, _pr_url: &str) -> LiveState {
            self.live.clone()
        }
    }

    struct MockRecords {
        version: u32,
        record: Result<TaskRecord, ReadError>,
        persistence: Result<(), ReadError>,
    }

    impl MockRecords {
        fn new(record: TaskRecord) -> Self {
            Self {
                version: GOVERNANCE_RECORDS_VERSION,
                record: Ok(record),
                persistence: Ok(()),
            }
        }
    }

    impl GovernanceRecords for MockRecords {
        fn version(&self) -> u32 {
            self.version
        }
        fn read_task(&self, _task_id: &str) -> Result<TaskRecord, ReadError> {
            self.record.clone()
        }
        fn probe_persistence(&self, _task_id: &str) -> Result<(), ReadError> {
            self.persistence.clone()
        }
    }

    /// Forge whose merge either honors `expected_head` as `--match-head-commit` does, or ignores
    /// it. Used only by the adapter conformance tests.
    struct HeadBindingForge {
        readable: bool,
        readback_readable: bool,
        live_head: String,
        honors_expected_head: bool,
        merged: Cell<bool>,
    }

    impl HeadBindingForge {
        fn new(honors_expected_head: bool) -> Self {
            Self {
                readable: true,
                readback_readable: true,
                live_head: HEAD.to_string(),
                honors_expected_head,
                merged: Cell::new(false),
            }
        }
    }

    impl ForgeClient for HeadBindingForge {
        fn version(&self) -> u32 {
            FORGE_CLIENT_VERSION
        }
        fn read_pull_request(&self, _pr_url: &str) -> Result<PullRequestSnapshot, ReadError> {
            if !self.readable {
                return Err(ReadError("gh timeout".to_string()));
            }
            Ok(PullRequestSnapshot {
                head_sha: self.live_head.clone(),
                ..green_snapshot()
            })
        }
        fn merge(&self, _pr_url: &str, expected_head: &str) -> ForgeReply {
            if self.honors_expected_head && expected_head != self.live_head {
                return ForgeReply::Failed("head out of date".to_string());
            }
            self.merged.set(true);
            ForgeReply::Success
        }
        fn confirm_merged(&self, _pr_url: &str) -> LiveState {
            if !self.readback_readable {
                LiveState::Unreadable
            } else if self.merged.get() {
                LiveState::Merged {
                    merge_sha: "merge-sha".to_string(),
                    merged_head: Some(self.live_head.clone()),
                }
            } else {
                LiveState::NotMerged
            }
        }
    }

    fn green_record() -> TaskRecord {
        TaskRecord {
            has_merge_authority: true,
            gate: GateState::Eligible,
            lock: LockEvidence::ProvablyStale,
            proof_head: HEAD.to_string(),
            attestation_signed: true,
            away_posture: false,
            bound_pr: None,
        }
    }

    fn green_snapshot() -> PullRequestSnapshot {
        PullRequestSnapshot {
            is_open: true,
            is_draft: false,
            is_mergeable: true,
            head_sha: HEAD.to_string(),
            checks: vec![CheckObservation {
                name: "build".to_string(),
                green: true,
                producer: CheckProducer::CommitStatus,
            }],
            required_contexts: vec![RequiredContext {
                name: "build".to_string(),
                app_id: None,
            }],
        }
    }

    fn request(waivers: MergeWaivers) -> MergeRequest<'static> {
        MergeRequest {
            task_id: TASK,
            pr_url: PR_URL,
            expected_head: HEAD,
            waivers,
        }
    }

    fn refused(result: GovernanceResult) -> Vec<GovernanceRefusal> {
        match result {
            GovernanceResult::Refused(refusals) => refusals,
            other => panic!("expected refusal, got {other:?}"),
        }
    }

    #[test]
    fn clean_facts_merge_once_at_the_verified_head_and_confirm() {
        let forge = MockForge::new(green_snapshot());
        let records = MockRecords::new(green_record());

        let result = govern_merge(&records, &forge, &request(MergeWaivers::default()));

        assert_eq!(
            result,
            GovernanceResult::Completed {
                merge_sha: "merge-sha".to_string()
            }
        );
        assert_eq!(forge.merge_calls.get(), 1);
        assert_eq!(forge.merge_heads.borrow().as_slice(), [HEAD.to_string()]);
    }

    #[test]
    fn every_refusal_path_makes_zero_merge_calls() {
        type Mutation = fn(&mut TaskRecord, &mut PullRequestSnapshot);
        let cases: Vec<(&str, Mutation)> = vec![
            ("no authority", |r, _| r.has_merge_authority = false),
            ("open decision", |r, _| {
                r.gate = GateState::OpenDecision {
                    key: "DEC-1".to_string(),
                }
            }),
            ("task held", |r, _| {
                r.gate = GateState::Held {
                    task_id: "DPES-1".to_string(),
                }
            }),
            ("lock held", |r, _| r.lock = LockEvidence::Held),
            ("lock unknown", |r, _| r.lock = LockEvidence::Unknown),
            ("unsigned attestation", |r, _| r.attestation_signed = false),
            ("bound to another unmerged pr", |r, _| {
                r.bound_pr = Some(BoundPr {
                    url: OTHER_PR_URL.to_string(),
                    merged: false,
                })
            }),
            ("proof head stale", |r, _| {
                r.proof_head = OTHER_HEAD.to_string()
            }),
            ("live head moved", |_, s| {
                s.head_sha = OTHER_HEAD.to_string()
            }),
            ("pr closed", |_, s| s.is_open = false),
            ("pr draft", |_, s| s.is_draft = true),
            ("not mergeable", |_, s| s.is_mergeable = false),
            ("failing check", |_, s| {
                s.checks.push(CheckObservation {
                    name: "lint".to_string(),
                    green: false,
                    producer: CheckProducer::CommitStatus,
                })
            }),
            ("missing required context", |_, s| {
                s.required_contexts.push(RequiredContext {
                    name: "security".to_string(),
                    app_id: None,
                })
            }),
        ];

        for (label, mutate) in cases {
            let mut record = green_record();
            let mut snapshot = green_snapshot();
            mutate(&mut record, &mut snapshot);
            let forge = MockForge::new(snapshot);
            let records = MockRecords::new(record);

            let refusals = refused(govern_merge(
                &records,
                &forge,
                &request(MergeWaivers::default()),
            ));

            assert!(!refusals.is_empty(), "{label}: expected refusal");
            assert_eq!(
                forge.merge_calls.get(),
                0,
                "{label}: merge must not be called"
            );
        }
    }

    #[test]
    fn unreadable_records_or_forge_make_zero_merge_calls() {
        let forge = MockForge::new(green_snapshot());
        let records = MockRecords {
            version: GOVERNANCE_RECORDS_VERSION,
            record: Err(ReadError("missing meta".to_string())),
            persistence: Ok(()),
        };
        assert_eq!(
            refused(govern_merge(
                &records,
                &forge,
                &request(MergeWaivers::default())
            )),
            vec![GovernanceRefusal::RecordUnreadable(
                "missing meta".to_string()
            )]
        );
        assert_eq!(forge.merge_calls.get(), 0);

        let mut unreadable_forge = MockForge::new(green_snapshot());
        unreadable_forge.snapshot = Err(ReadError("gh timeout".to_string()));
        let records = MockRecords::new(green_record());
        assert_eq!(
            refused(govern_merge(
                &records,
                &unreadable_forge,
                &request(MergeWaivers::default())
            )),
            vec![GovernanceRefusal::ForgeUnreadable("gh timeout".to_string())]
        );
        assert_eq!(unreadable_forge.merge_calls.get(), 0);
    }

    #[test]
    fn persistence_probe_failure_makes_zero_merge_calls() {
        let forge = MockForge::new(green_snapshot());
        let mut records = MockRecords::new(green_record());
        records.persistence = Err(ReadError("state dir read-only".to_string()));

        assert_eq!(
            refused(govern_merge(
                &records,
                &forge,
                &request(MergeWaivers::default())
            )),
            vec![GovernanceRefusal::PersistenceUnavailable(
                "state dir read-only".to_string()
            )]
        );
        assert_eq!(forge.merge_calls.get(), 0);
    }

    #[test]
    fn version_mismatch_makes_zero_merge_calls() {
        let mut forge = MockForge::new(green_snapshot());
        forge.version = FORGE_CLIENT_VERSION + 1;
        let records = MockRecords::new(green_record());

        assert_eq!(
            refused(govern_merge(
                &records,
                &forge,
                &request(MergeWaivers::default())
            )),
            vec![GovernanceRefusal::ForgeVersionMismatch {
                expected: FORGE_CLIENT_VERSION,
                found: FORGE_CLIENT_VERSION + 1,
            }]
        );
        assert_eq!(forge.merge_calls.get(), 0);
    }

    #[test]
    fn exact_head_mismatch_refuses_and_names_both_heads() {
        let mut snapshot = green_snapshot();
        snapshot.head_sha = OTHER_HEAD.to_string();

        let refusals = refused(govern_merge(
            &MockRecords::new(green_record()),
            &MockForge::new(snapshot),
            &request(MergeWaivers::default()),
        ));

        assert_eq!(
            refusals,
            vec![GovernanceRefusal::HeadMismatch {
                expected_head: HEAD.to_string(),
                live_head: OTHER_HEAD.to_string(),
            }]
        );
    }

    #[test]
    fn malformed_heads_refuse_with_zero_merge_calls() {
        // bin/fm-pr-lib.sh fm_pr_head_valid accepts only 40 or 64 lowercase hex characters.
        let sixty_four = "0123456789abcdef".repeat(4);
        let uppercase = HEAD.to_uppercase();
        let short = HEAD[..39].to_string();
        let long = format!("{HEAD}0");
        let cases = [
            "not-a-sha".to_string(),
            uppercase,
            short,
            long,
            sixty_four[..63].to_string(),
        ];
        for head in cases {
            let mut record = green_record();
            record.proof_head = head.clone();
            let mut snapshot = green_snapshot();
            snapshot.head_sha = head.clone();
            let forge = MockForge::new(snapshot);
            let merge = MergeRequest {
                task_id: TASK,
                pr_url: PR_URL,
                expected_head: &head,
                waivers: MergeWaivers::default(),
            };

            let refusals = refused(govern_merge(&MockRecords::new(record), &forge, &merge));

            assert!(
                refusals.contains(&GovernanceRefusal::MalformedHead { head: head.clone() }),
                "{head:?}: expected MalformedHead, got {refusals:?}"
            );
            assert_eq!(forge.merge_calls.get(), 0, "{head:?}");
        }
    }

    #[test]
    fn full_lowercase_commit_ids_of_forty_and_sixty_four_chars_may_merge() {
        for head in [HEAD.to_string(), "0123456789abcdef".repeat(4)] {
            let mut record = green_record();
            record.proof_head = head.clone();
            let mut snapshot = green_snapshot();
            snapshot.head_sha = head.clone();
            let mut forge = MockForge::new(snapshot);
            forge.live = LiveState::Merged {
                merge_sha: "merge-sha".to_string(),
                merged_head: Some(head.clone()),
            };
            let merge = MergeRequest {
                task_id: TASK,
                pr_url: PR_URL,
                expected_head: &head,
                waivers: MergeWaivers::default(),
            };

            assert_eq!(
                govern_merge(&MockRecords::new(record), &forge, &merge),
                GovernanceResult::Completed {
                    merge_sha: "merge-sha".to_string()
                }
            );
            assert_eq!(forge.merge_calls.get(), 1);
        }
    }

    #[test]
    fn empty_expected_head_is_unbound_and_refused() {
        let forge = MockForge::new(green_snapshot());
        let records = MockRecords::new(green_record());
        let unbound = MergeRequest {
            task_id: TASK,
            pr_url: PR_URL,
            expected_head: "",
            waivers: MergeWaivers::default(),
        };

        assert!(
            refused(govern_merge(&records, &forge, &unbound))
                .contains(&GovernanceRefusal::UnboundHead)
        );
        assert_eq!(forge.merge_calls.get(), 0);
    }

    #[test]
    fn empty_proof_or_live_head_refuses_with_zero_merge_calls() {
        for (proof_head, live_head) in [("", HEAD), (HEAD, ""), ("", "")] {
            let mut record = green_record();
            record.proof_head = proof_head.to_string();
            let mut snapshot = green_snapshot();
            snapshot.head_sha = live_head.to_string();
            let forge = MockForge::new(snapshot);

            let refusals = refused(govern_merge(
                &MockRecords::new(record),
                &forge,
                &request(MergeWaivers::default()),
            ));

            assert!(
                refusals.contains(&GovernanceRefusal::MalformedHead {
                    head: String::new()
                }),
                "proof {proof_head:?} live {live_head:?}: expected MalformedHead, got {refusals:?}"
            );
            assert_eq!(
                forge.merge_calls.get(),
                0,
                "proof {proof_head:?} live {live_head:?}"
            );
        }
    }

    #[test]
    fn lock_missing_or_provably_stale_may_merge_and_held_or_unknown_refuse() {
        for (lock, may_merge) in [
            (LockEvidence::ProvablyStale, true),
            (LockEvidence::Missing, true),
            (LockEvidence::Held, false),
            (LockEvidence::Unknown, false),
        ] {
            let mut record = green_record();
            record.lock = lock;
            let forge = MockForge::new(green_snapshot());

            let result = govern_merge(
                &MockRecords::new(record),
                &forge,
                &request(MergeWaivers::default()),
            );

            if may_merge {
                assert!(
                    matches!(result, GovernanceResult::Completed { .. }),
                    "{lock:?}: expected Completed, got {result:?}"
                );
            } else {
                assert!(
                    refused(result).contains(&GovernanceRefusal::LockNotStale),
                    "{lock:?}: expected LockNotStale"
                );
                assert_eq!(forge.merge_calls.get(), 0, "{lock:?}");
            }
        }
    }

    #[test]
    fn unwaived_failing_check_refuses_and_is_named() {
        let mut snapshot = green_snapshot();
        snapshot.checks.push(CheckObservation {
            name: "lint".to_string(),
            green: false,
            producer: CheckProducer::CommitStatus,
        });
        let forge = MockForge::new(snapshot);

        let refusals = refused(govern_merge(
            &MockRecords::new(green_record()),
            &forge,
            &request(MergeWaivers::default()),
        ));

        assert_eq!(
            refusals,
            vec![GovernanceRefusal::UnwaivedFailingCheck {
                name: "lint".to_string()
            }]
        );
        assert_eq!(forge.merge_calls.get(), 0);
    }

    #[test]
    fn missing_required_context_refuses_and_is_named() {
        let mut snapshot = green_snapshot();
        snapshot.required_contexts.push(RequiredContext {
            name: "security".to_string(),
            app_id: None,
        });
        let forge = MockForge::new(snapshot);

        let refusals = refused(govern_merge(
            &MockRecords::new(green_record()),
            &forge,
            &request(MergeWaivers::default()),
        ));

        assert_eq!(
            refusals,
            vec![GovernanceRefusal::MissingRequiredContext {
                name: "security".to_string()
            }]
        );
        assert_eq!(forge.merge_calls.get(), 0);
    }

    #[test]
    fn required_context_omitted_from_checks_refuses_as_unreported() {
        // Pins the adapter contract: a required context that is red but left out of `checks`
        // cannot pass, because the gate reads it as unreported.
        let mut snapshot = green_snapshot();
        snapshot.required_contexts.push(RequiredContext {
            name: "security".to_string(),
            app_id: None,
        });
        snapshot.checks.retain(|check| check.name != "security");
        let forge = MockForge::new(snapshot);

        assert_eq!(
            refused(govern_merge(
                &MockRecords::new(green_record()),
                &forge,
                &request(MergeWaivers::default()),
            )),
            vec![GovernanceRefusal::MissingRequiredContext {
                name: "security".to_string()
            }]
        );
        assert_eq!(forge.merge_calls.get(), 0);
    }

    #[test]
    fn app_bound_requirement_refuses_a_same_named_check_from_another_producer() {
        // bin/fm-pr-merge.sh github_required_checks_missing: a bound requirement needs a check run
        // from the bound app, so a same-named run from another app does not satisfy it.
        let mut snapshot = green_snapshot();
        snapshot.required_contexts = vec![RequiredContext {
            name: "build".to_string(),
            app_id: Some(BOUND_APP),
        }];
        snapshot.checks = vec![CheckObservation {
            name: "build".to_string(),
            green: true,
            producer: CheckProducer::CheckRun {
                app_id: Some(OTHER_APP),
            },
        }];
        let forge = MockForge::new(snapshot);

        assert_eq!(
            refused(govern_merge(
                &MockRecords::new(green_record()),
                &forge,
                &request(MergeWaivers::default()),
            )),
            vec![GovernanceRefusal::MissingRequiredContext {
                name: "build".to_string()
            }]
        );
        assert_eq!(forge.merge_calls.get(), 0);
    }

    #[test]
    fn app_bound_requirement_refuses_a_check_run_with_an_unreadable_producer_even_if_waived() {
        // bin/fm-pr-merge.sh: a check run whose producer cannot be read does not match a bound
        // requirement, and its producers-unreadable refusal is not waived by --allow-missing.
        let mut snapshot = green_snapshot();
        snapshot.required_contexts = vec![RequiredContext {
            name: "build".to_string(),
            app_id: Some(BOUND_APP),
        }];
        snapshot.checks = vec![CheckObservation {
            name: "build".to_string(),
            green: true,
            producer: CheckProducer::CheckRun { app_id: None },
        }];
        let forge = MockForge::new(snapshot);

        for waivers in [
            MergeWaivers::default(),
            MergeWaivers {
                allow_red: None,
                allow_missing: Some("build".to_string()),
            },
        ] {
            assert_eq!(
                refused(govern_merge(
                    &MockRecords::new(green_record()),
                    &forge,
                    &request(waivers),
                )),
                vec![GovernanceRefusal::ProducerUnreadable {
                    name: "build".to_string()
                }]
            );
        }
        assert_eq!(forge.merge_calls.get(), 0);
    }

    #[test]
    fn app_bound_requirement_is_met_by_its_producer_or_a_same_named_status() {
        let mut from_bound_app = green_snapshot();
        from_bound_app.required_contexts = vec![RequiredContext {
            name: "build".to_string(),
            app_id: Some(BOUND_APP),
        }];
        from_bound_app.checks[0].producer = CheckProducer::CheckRun {
            app_id: Some(BOUND_APP),
        };

        // A commit status carries no app id, so bin/fm-pr-merge.sh matches it by name.
        let mut from_status = from_bound_app.clone();
        from_status.checks[0].producer = CheckProducer::CommitStatus;

        for snapshot in [from_bound_app, from_status] {
            let forge = MockForge::new(snapshot);
            assert_eq!(
                govern_merge(
                    &MockRecords::new(green_record()),
                    &forge,
                    &request(MergeWaivers::default()),
                ),
                GovernanceResult::Completed {
                    merge_sha: "merge-sha".to_string()
                }
            );
            assert_eq!(forge.merge_calls.get(), 1);
        }
    }

    #[test]
    fn duplicate_name_only_requirement_cannot_weaken_an_app_bound_one() {
        // Shell: when a context has any app-bound entry, its name-only entries are dropped.
        let mut snapshot = green_snapshot();
        snapshot.required_contexts = vec![
            RequiredContext {
                name: "build".to_string(),
                app_id: None,
            },
            RequiredContext {
                name: "build".to_string(),
                app_id: Some(BOUND_APP),
            },
        ];
        snapshot.checks = vec![CheckObservation {
            name: "build".to_string(),
            green: true,
            producer: CheckProducer::CheckRun {
                app_id: Some(OTHER_APP),
            },
        }];
        let forge = MockForge::new(snapshot);

        assert_eq!(
            refused(govern_merge(
                &MockRecords::new(green_record()),
                &forge,
                &request(MergeWaivers::default()),
            )),
            vec![GovernanceRefusal::MissingRequiredContext {
                name: "build".to_string()
            }]
        );
        assert_eq!(forge.merge_calls.get(), 0);
    }

    #[test]
    fn all_failing_conditions_are_reported_not_just_the_first() {
        let mut snapshot = green_snapshot();
        snapshot.checks.push(CheckObservation {
            name: "lint".to_string(),
            green: false,
            producer: CheckProducer::CommitStatus,
        });
        snapshot.checks.push(CheckObservation {
            name: "test".to_string(),
            green: false,
            producer: CheckProducer::CommitStatus,
        });
        snapshot.required_contexts.push(RequiredContext {
            name: "security".to_string(),
            app_id: None,
        });

        let refusals = refused(govern_merge(
            &MockRecords::new(green_record()),
            &MockForge::new(snapshot),
            &request(MergeWaivers::default()),
        ));

        assert_eq!(refusals.len(), 3);
    }

    #[test]
    fn allow_red_waives_only_the_named_check_on_the_bound_head() {
        let mut snapshot = green_snapshot();
        snapshot.checks.push(CheckObservation {
            name: "lint".to_string(),
            green: false,
            producer: CheckProducer::CommitStatus,
        });
        let waivers = MergeWaivers {
            allow_red: Some("lint".to_string()),
            allow_missing: None,
        };
        let forge = MockForge::new(snapshot.clone());
        assert_eq!(
            govern_merge(
                &MockRecords::new(green_record()),
                &forge,
                &request(waivers.clone())
            ),
            GovernanceResult::Completed {
                merge_sha: "merge-sha".to_string()
            }
        );
        assert_eq!(forge.merge_calls.get(), 1);

        snapshot.checks.push(CheckObservation {
            name: "test".to_string(),
            green: false,
            producer: CheckProducer::CommitStatus,
        });
        let forge = MockForge::new(snapshot.clone());
        assert_eq!(
            refused(govern_merge(
                &MockRecords::new(green_record()),
                &forge,
                &request(waivers)
            )),
            vec![GovernanceRefusal::UnwaivedFailingCheck {
                name: "test".to_string()
            }]
        );
        assert_eq!(forge.merge_calls.get(), 0);

        let mut moved = green_snapshot();
        moved.head_sha = OTHER_HEAD.to_string();
        moved.checks.push(CheckObservation {
            name: "lint".to_string(),
            green: false,
            producer: CheckProducer::CommitStatus,
        });
        let forge = MockForge::new(moved);
        let refusals = refused(govern_merge(
            &MockRecords::new(green_record()),
            &forge,
            &request(MergeWaivers {
                allow_red: Some("lint".to_string()),
                allow_missing: None,
            }),
        ));
        assert!(refusals.contains(&GovernanceRefusal::HeadMismatch {
            expected_head: HEAD.to_string(),
            live_head: OTHER_HEAD.to_string(),
        }));
        assert_eq!(forge.merge_calls.get(), 0);
    }

    #[test]
    fn allow_missing_waives_only_the_named_required_context() {
        let mut snapshot = green_snapshot();
        snapshot.required_contexts.push(RequiredContext {
            name: "security".to_string(),
            app_id: None,
        });
        let forge = MockForge::new(snapshot.clone());
        assert_eq!(
            govern_merge(
                &MockRecords::new(green_record()),
                &forge,
                &request(MergeWaivers {
                    allow_red: None,
                    allow_missing: Some("security".to_string()),
                }),
            ),
            GovernanceResult::Completed {
                merge_sha: "merge-sha".to_string()
            }
        );

        snapshot.required_contexts.push(RequiredContext {
            name: "codeql".to_string(),
            app_id: None,
        });
        let forge = MockForge::new(snapshot);
        assert_eq!(
            refused(govern_merge(
                &MockRecords::new(green_record()),
                &forge,
                &request(MergeWaivers {
                    allow_red: None,
                    allow_missing: Some("security".to_string()),
                }),
            )),
            vec![GovernanceRefusal::MissingRequiredContext {
                name: "codeql".to_string()
            }]
        );
        assert_eq!(forge.merge_calls.get(), 0);
    }

    #[test]
    fn blank_waiver_names_are_refused_with_zero_merge_calls() {
        // bin/fm-pr-merge.sh refuses an empty --allow-red (L243) or --allow-missing (L253) name.
        // The green snapshot has no red check, so a blank waiver must not be the only reason
        // a merge proceeds, and must not waive a check named like the blank text.
        let mut snapshot = green_snapshot();
        snapshot.checks.push(CheckObservation {
            name: String::new(),
            green: false,
            producer: CheckProducer::CommitStatus,
        });
        let cases = [
            (
                MergeWaivers {
                    allow_red: Some(String::new()),
                    allow_missing: None,
                },
                "--allow-red",
            ),
            (
                MergeWaivers {
                    allow_red: Some("   ".to_string()),
                    allow_missing: None,
                },
                "--allow-red",
            ),
            (
                MergeWaivers {
                    allow_red: None,
                    allow_missing: Some(String::new()),
                },
                "--allow-missing",
            ),
        ];
        for (waivers, flag) in cases {
            let forge = MockForge::new(snapshot.clone());
            let refusals = refused(govern_merge(
                &MockRecords::new(green_record()),
                &forge,
                &request(waivers),
            ));

            assert!(
                refusals.contains(&GovernanceRefusal::EmptyWaiverName { flag }),
                "{flag}: expected EmptyWaiverName, got {refusals:?}"
            );
            assert_eq!(forge.merge_calls.get(), 0);
        }
    }

    #[test]
    fn task_bound_to_another_unmerged_pr_refuses_with_zero_merge_calls() {
        // bin/fm-pr-merge.sh require_recorded_pr_identity: pr= differs from the requested URL and
        // that PR has no recorded merge, so the task refuses the new URL.
        let mut record = green_record();
        record.bound_pr = Some(BoundPr {
            url: OTHER_PR_URL.to_string(),
            merged: false,
        });
        let forge = MockForge::new(green_snapshot());

        assert_eq!(
            refused(govern_merge(
                &MockRecords::new(record),
                &forge,
                &request(MergeWaivers::default()),
            )),
            vec![GovernanceRefusal::TaskBoundToOtherPr {
                bound_url: OTHER_PR_URL.to_string(),
                requested_url: PR_URL.to_string(),
            }]
        );
        assert_eq!(forge.merge_calls.get(), 0);
    }

    #[test]
    fn task_bound_to_the_requested_pr_or_to_a_merged_pr_may_merge() {
        // require_recorded_pr_identity accepts an identical pr= and a bound PR whose merge is
        // already notified, so several PRs from one task can each merge in turn.
        let same = BoundPr {
            url: PR_URL.to_string(),
            merged: false,
        };
        let already_merged = BoundPr {
            url: OTHER_PR_URL.to_string(),
            merged: true,
        };
        for bound in [same, already_merged] {
            let mut record = green_record();
            record.bound_pr = Some(bound);
            let forge = MockForge::new(green_snapshot());

            assert_eq!(
                govern_merge(
                    &MockRecords::new(record),
                    &forge,
                    &request(MergeWaivers::default()),
                ),
                GovernanceResult::Completed {
                    merge_sha: "merge-sha".to_string()
                }
            );
            assert_eq!(forge.merge_calls.get(), 1);
        }
    }

    #[test]
    fn waivers_are_refused_while_away() {
        let mut record = green_record();
        record.away_posture = true;
        let mut snapshot = green_snapshot();
        snapshot.checks.push(CheckObservation {
            name: "lint".to_string(),
            green: false,
            producer: CheckProducer::CommitStatus,
        });
        let forge = MockForge::new(snapshot);

        let refusals = refused(govern_merge(
            &MockRecords::new(record),
            &forge,
            &request(MergeWaivers {
                allow_red: Some("lint".to_string()),
                allow_missing: None,
            }),
        ));

        assert_eq!(refusals, vec![GovernanceRefusal::WaiverWhileAway]);
        assert_eq!(forge.merge_calls.get(), 0);
    }

    #[test]
    fn uncertain_outcome_is_not_completion_and_is_not_retried() {
        let cases = [
            (
                ForgeReply::Success,
                LiveState::NotMerged,
                "forge reported success but the pull request is not merged",
            ),
            (
                ForgeReply::Ambiguous("502 from forge".to_string()),
                LiveState::Unreadable,
                "502 from forge",
            ),
        ];
        for (reply, live, message) in cases {
            let mut forge = MockForge::new(green_snapshot());
            forge.reply = reply;
            forge.live = live;

            let result = govern_merge(
                &MockRecords::new(green_record()),
                &forge,
                &request(MergeWaivers::default()),
            );

            match result {
                GovernanceResult::Attempted(outcome) => {
                    assert!(!outcome.permits_task_success());
                    assert_eq!(outcome, MergeOutcome::Uncertain(message.to_string()));
                }
                other => panic!("expected uncertain attempt, got {other:?}"),
            }
            assert_eq!(forge.merge_calls.get(), 1);
        }
    }

    #[test]
    fn forge_failure_with_unmerged_readback_is_refused_not_completed() {
        let mut forge = MockForge::new(green_snapshot());
        forge.reply = ForgeReply::Failed("merge conflict".to_string());
        forge.live = LiveState::NotMerged;

        assert_eq!(
            govern_merge(
                &MockRecords::new(green_record()),
                &forge,
                &request(MergeWaivers::default())
            ),
            GovernanceResult::Attempted(MergeOutcome::Refused(
                crate::merge_gate::MergeRefusal::MergeFailed("merge conflict".to_string())
            ))
        );
    }

    #[test]
    fn success_with_unreadable_readback_is_attempted_not_completed() {
        // A merge reply that cannot be confirmed by a live read is uncertain, never a completed
        // merge, and the gate does not retry the merge.
        let cases = [
            (
                ForgeReply::Success,
                "forge reported success but the pull request state is unreadable",
            ),
            (
                ForgeReply::Failed("502 from forge".to_string()),
                "502 from forge",
            ),
        ];
        for (reply, message) in cases {
            let mut forge = MockForge::new(green_snapshot());
            forge.reply = reply;
            forge.live = LiveState::Unreadable;

            assert_eq!(
                govern_merge(
                    &MockRecords::new(green_record()),
                    &forge,
                    &request(MergeWaivers::default()),
                ),
                GovernanceResult::Attempted(MergeOutcome::Uncertain(message.to_string()))
            );
            assert_eq!(forge.merge_calls.get(), 1);
        }
    }

    #[test]
    fn human_merge_at_another_head_is_uncertain_not_completed() {
        // A person merged the pull request at OTHER_HEAD after the verified snapshot. The gate's
        // own merge call then fails or reports success, and the read-back is Merged at OTHER_HEAD.
        // That merge is not the verified head, so it must not complete.
        for reply in [
            ForgeReply::Failed("already merged".to_string()),
            ForgeReply::Success,
        ] {
            let mut forge = MockForge::new(green_snapshot());
            forge.reply = reply;
            forge.live = LiveState::Merged {
                merge_sha: "human-merge".to_string(),
                merged_head: Some(OTHER_HEAD.to_string()),
            };

            assert_eq!(
                govern_merge(
                    &MockRecords::new(green_record()),
                    &forge,
                    &request(MergeWaivers::default()),
                ),
                GovernanceResult::Attempted(MergeOutcome::Uncertain(format!(
                    "pull request merged at head {OTHER_HEAD}, not the verified head {HEAD}"
                )))
            );
            assert_eq!(forge.merge_calls.get(), 1);
        }
    }

    #[test]
    fn merged_state_without_a_reported_head_is_uncertain_not_completed() {
        let mut forge = MockForge::new(green_snapshot());
        forge.reply = ForgeReply::Failed("already merged".to_string());
        forge.live = LiveState::Merged {
            merge_sha: "human-merge".to_string(),
            merged_head: None,
        };

        assert_eq!(
            govern_merge(
                &MockRecords::new(green_record()),
                &forge,
                &request(MergeWaivers::default()),
            ),
            GovernanceResult::Attempted(MergeOutcome::Uncertain(
                "pull request merged but the merged head was not reported".to_string()
            ))
        );
        assert_eq!(forge.merge_calls.get(), 1);
    }

    #[test]
    fn merged_at_the_verified_head_completes_even_when_a_person_merged_it() {
        let mut forge = MockForge::new(green_snapshot());
        forge.reply = ForgeReply::Failed("already merged".to_string());
        forge.live = LiveState::Merged {
            merge_sha: "human-merge".to_string(),
            merged_head: Some(HEAD.to_string()),
        };

        assert_eq!(
            govern_merge(
                &MockRecords::new(green_record()),
                &forge,
                &request(MergeWaivers::default()),
            ),
            GovernanceResult::Completed {
                merge_sha: "human-merge".to_string()
            }
        );
        assert_eq!(forge.merge_calls.get(), 1);
    }

    #[test]
    fn shell_reduced_failing_names_each_refuse_unless_waived() {
        // Names from `github_checks_not_green` in bin/fm-pr-merge.sh, run offline on rollup
        // fixtures: a failing check run, a failing status context, a pending check run, and an
        // unnamed failing check run.
        let names = ["lint", "ci/x", "test", "(unnamed check)"];
        let mut snapshot = green_snapshot();
        for name in names {
            snapshot.checks.push(CheckObservation {
                name: name.to_string(),
                green: false,
                producer: CheckProducer::CommitStatus,
            });
        }
        let forge = MockForge::new(snapshot);

        let refusals = refused(govern_merge(
            &MockRecords::new(green_record()),
            &forge,
            &request(MergeWaivers::default()),
        ));

        assert_eq!(
            refusals,
            names
                .map(|name| GovernanceRefusal::UnwaivedFailingCheck {
                    name: name.to_string(),
                })
                .to_vec()
        );
        assert_eq!(forge.merge_calls.get(), 0);
    }

    #[test]
    fn green_forge_snapshot_cannot_override_a_failing_record() {
        let mut record = green_record();
        record.gate = GateState::OpenDecision {
            key: "DEC-9".to_string(),
        };
        record.has_merge_authority = false;
        let forge = MockForge::new(green_snapshot());

        let refusals = refused(govern_merge(
            &MockRecords::new(record),
            &forge,
            &request(MergeWaivers::default()),
        ));

        assert_eq!(
            refusals,
            vec![
                GovernanceRefusal::NoAuthority,
                GovernanceRefusal::OpenDecision {
                    key: "DEC-9".to_string()
                },
            ]
        );
        assert_eq!(forge.merge_calls.get(), 0);
    }

    /// Store that holds each fixture's record under its task id, as an honest adapter does.
    struct SeededRecords {
        records: Vec<(&'static str, TaskRecord)>,
    }

    impl GovernanceRecords for SeededRecords {
        fn version(&self) -> u32 {
            GOVERNANCE_RECORDS_VERSION
        }
        fn read_task(&self, task_id: &str) -> Result<TaskRecord, ReadError> {
            self.records
                .iter()
                .find(|(id, _)| *id == task_id)
                .map(|(_, record)| record.clone())
                .ok_or_else(|| ReadError(format!("no record for {task_id}")))
        }
        fn probe_persistence(&self, _task_id: &str) -> Result<(), ReadError> {
            Ok(())
        }
    }

    /// Store that reports the conforming, authorized record for every task, whatever its durable
    /// state.
    struct LyingRecords;

    impl GovernanceRecords for LyingRecords {
        fn version(&self) -> u32 {
            GOVERNANCE_RECORDS_VERSION
        }
        fn read_task(&self, _task_id: &str) -> Result<TaskRecord, ReadError> {
            Ok(conforming_record())
        }
        fn probe_persistence(&self, _task_id: &str) -> Result<(), ReadError> {
            Ok(())
        }
    }

    #[test]
    fn records_conformance_passes_an_honest_store_and_fails_a_lying_one() {
        let fixtures = refusal_fixtures();
        let honest = SeededRecords {
            records: fixtures
                .iter()
                .map(|fixture| (fixture.task_id, fixture.record.clone()))
                .collect(),
        };
        assert_eq!(check_records_conformance(&honest, &fixtures), Ok(()));

        assert_eq!(
            check_records_conformance(&LyingRecords, &fixtures),
            Err(fixtures
                .iter()
                .map(|fixture| RecordsViolation::Authorized {
                    label: fixture.label
                })
                .collect())
        );
    }

    #[test]
    fn conforming_forge_passes_the_head_binding_check() {
        assert_eq!(
            check_head_binding(&HeadBindingForge::new(true), PR_URL),
            Ok(())
        );
    }

    #[test]
    fn head_ignoring_forge_fails_the_head_binding_check() {
        // A forge that merges at a head other than the one requested must be rejected, because
        // the gate's head binding depends on this argument alone.
        assert_eq!(
            check_head_binding(&HeadBindingForge::new(false), PR_URL),
            Err(ConformanceViolation::MergeIgnoredExpectedHead {
                expected_head: format!("{HEAD}-stale"),
                live_head: HEAD.to_string(),
            })
        );
    }

    #[test]
    fn unreadable_readback_after_a_stale_head_merge_is_unconfirmed_not_conforming() {
        // An unreadable read-back cannot show the stale-head merge was refused, so the binding is
        // not confirmed and the helper must not report a pass.
        let mut forge = HeadBindingForge::new(true);
        forge.readback_readable = false;

        assert_eq!(
            check_head_binding(&forge, PR_URL),
            Err(ConformanceViolation::BindingUnconfirmed {
                expected_head: format!("{HEAD}-stale"),
            })
        );
    }

    #[test]
    fn unreadable_forge_fails_the_head_binding_check() {
        let mut forge = HeadBindingForge::new(true);
        forge.readable = false;

        assert_eq!(
            check_head_binding(&forge, PR_URL),
            Err(ConformanceViolation::Unreadable("gh timeout".to_string()))
        );
        assert!(!forge.merged.get());
    }
}
