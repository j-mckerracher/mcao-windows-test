//! Rust core of bin/fm-assignment-reassess.py for a fresh decision: the change collector, the
//! classifier and the decision record. docs/assignment-profile-policy.md owns the profiles and
//! the Python reference owns the observable behavior. The CLI does not call this module yet.
//!
//! Ported: `collect` (coverage categories, gaps, changed paths and change identity), the prose
//! line checks of a changed prose path, request and previous-record validation, and `reassess` for
//! a fresh decision or a superseding one. [`KNOWN_GAPS`] lists what is not ported.

mod sequence_matcher;

use crate::canonical_json::escape_string;
use crate::sha256::{self, Sha256};
use sequence_matcher::{Tag, get_opcodes};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Divergences from the Python reference that this module does not close. A differing fixture
/// is a finding and belongs here only after it has been reported.
pub const KNOWN_GAPS: &[(&str, &str)] = &[
    (
        "previous-unchanged",
        "a previous record with no transition reason returns Unported: Python echoes the record in its own key order, and serde_json preserve_order is not enabled in this crate",
    ),
    (
        "inventory-entry-types",
        "a docs/documentation-audiences.json entry with a non-string path or audience makes the inventory unavailable; Python keeps it",
    ),
    (
        "non-utf8-paths",
        "git output is decoded lossily; Python decodes with surrogateescape. Not verified: this macOS volume refuses to create a non-UTF-8 file name (Illegal byte sequence), so the scenario could not be run on either side",
    ),
    (
        "fixture-coverage",
        "the four recorded reassess steps exercise only the unresolved high-risk path (inventory absent at the base); standard, small-low-risk and behavioral branches are pinned by unit tests only",
    ),
    (
        "unreadable-worktree-entries",
        "a worktree entry that cannot be read is classified as other; Python raises",
    ),
    (
        "cli-wiring",
        "no mcao assignment reassess command, stdout writer or exit-code mapping is wired; Unreadable maps to exit 2 and Blocked to exit 1 when wired",
    ),
    ("windows", "git and path handling are not verified off unix"),
];

const RECORD_KEYS: &[&str] = &[
    "contract_version",
    "decision_id",
    "task_id",
    "assignment_intent",
    "expected_surfaces",
    "pinned_policy_revision",
    "profile",
    "decision_stage",
    "classification_status",
    "risk_triggers",
    "change_coverage",
    "reasons",
    "required_stages",
    "evidence_obligations",
    "target_branch",
    "comparison_base_commit",
];
const OPTIONAL_RECORD_KEYS: &[&str] = &["evaluated_change_identity", "supersedes"];
const REQUIRED_REQUEST_KEYS: &[&str] = &[
    "task_id",
    "pinned_policy_revision",
    "target_branch",
    "comparison_base_commit",
    "assignment_intent",
    "expected_surfaces",
];
const OPTIONAL_REQUEST_KEYS: &[&str] = &["risk_triggers", "small_low_risk_conditions", "previous"];
const RISK_TRIGGERS: &[&str] = &[
    "security-authorization-secrets-or-sensitive-data",
    "destructive-operation-or-data-migration",
    "dispatch-lifecycle-worktree-delivery-merge-or-proof-gate-authority",
    "breaking-public-contract",
    "cross-component-shared-state-concurrency-or-recovery",
    "broad-change-needing-coordinated-design-and-verification",
];
const COVERAGE_CATEGORIES: &[&str] = &[
    "committed",
    "tracked-working-tree",
    "untracked",
    "renamed",
    "deleted",
];
const SMALL_LOW_RISK_CONDITIONS: &[&str] = &[
    "single_bounded_behavior_change",
    "no_security_or_authorization_change",
    "no_sensitive_data_change",
    "no_destructive_operation",
    "no_merge_delivery_authority_change",
    "no_dispatch_lifecycle_worktree_or_proof_gate_change",
    "no_public_contract_incompatibility",
    "no_data_migration",
    "no_unresolved_cross_component_impact",
    "relevant_focused_verification_identified",
];
const PROSE_EXTENSIONS: &[&str] = &[".md", ".mdx", ".rst", ".txt"];
const PROSE_AUDIENCES: &[&str] = &[
    "public-product",
    "operator-current",
    "maintainer-architecture",
    "maintainer-verification",
];
const CODE_EXTENSIONS: &[&str] = &[".sh", ".bash", ".py", ".json", ".toml", ".yaml", ".yml"];
const EXAMPLE_PREFIX: &str = "docs/examples/";
const INVENTORY_PATH: &str = "docs/documentation-audiences.json";
const CONTRACT_VERSION: i64 = 1;

/// The profiles a classification can produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    DocumentationOnly,
    Standard,
    SmallLowRisk,
    HighRisk,
}

impl Profile {
    pub fn as_str(self) -> &'static str {
        match self {
            Profile::DocumentationOnly => "documentation-only",
            Profile::Standard => "standard",
            Profile::SmallLowRisk => "small-low-risk",
            Profile::HighRisk => "high-risk",
        }
    }

    fn stages(self) -> &'static [&'static str] {
        match self {
            Profile::DocumentationOnly => &[
                "ground-scope",
                "implement",
                "focused-review",
                "verify",
                "change-reassessment",
                "delivery-handoff",
            ],
            Profile::Standard => &[
                "ground-scope",
                "design",
                "implement",
                "focused-review",
                "verify",
                "change-reassessment",
                "delivery-handoff",
            ],
            Profile::SmallLowRisk => &[
                "ground-scope",
                "short-plan",
                "implement",
                "focused-review",
                "verify",
                "change-reassessment",
                "delivery-handoff",
            ],
            Profile::HighRisk => &[
                "ground-scope",
                "design",
                "design-critique",
                "implement",
                "independent-change-review",
                "risk-verify",
                "change-reassessment",
                "delivery-handoff",
            ],
        }
    }
}

/// The worktree state of one changed path, as `read_worktree` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeKind {
    File,
    Absent,
    Other,
}

impl WorktreeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            WorktreeKind::File => "file",
            WorktreeKind::Absent => "absent",
            WorktreeKind::Other => "other",
        }
    }
}

/// One changed path, read from the comparison base and the worktree. `base` is the blob at the
/// comparison base and `worktree` the bytes of a regular file; each is `None` when absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedPath {
    pub path: String,
    pub base: Option<Vec<u8>>,
    pub worktree: Option<Vec<u8>>,
    pub kind: WorktreeKind,
}

/// The result of `collect`: coverage, gaps, changed paths, the inventory and the change identity.
/// The identity is present only when collection is complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collection {
    pub base_ok: bool,
    pub complete: bool,
    pub categories: Vec<&'static str>,
    pub gaps: Vec<String>,
    pub identity: Option<String>,
    pub audiences: Option<BTreeMap<String, String>>,
    pub paths: Vec<ChangedPath>,
}

/// A validated reassess request. Only the fields the Python request validator accepts are here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub task_id: String,
    pub pinned_policy_revision: String,
    pub target_branch: String,
    pub comparison_base_commit: String,
    pub assignment_intent: String,
    pub expected_surfaces: Vec<String>,
    pub risk_triggers: Option<Vec<String>>,
    pub small_low_risk_conditions: Option<BTreeMap<String, bool>>,
    pub previous: Option<PreviousRecord>,
}

/// A previous decision record that passed `validate_record`, with the fields a superseding decision
/// reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviousRecord {
    pub decision_id: String,
    pub profile: Profile,
    pub decision_stage: String,
    pub classification_status: String,
    pub risk_triggers: Vec<String>,
    pub coverage_complete: bool,
    pub coverage_categories: Vec<String>,
    pub reasons: Vec<String>,
    pub evaluated_change_identity: Option<String>,
    pub comparison_base_commit: String,
    pub pinned_policy_revision: String,
}

/// The transition fields of a decision, as the Python `transition` object carries them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    pub previous_profile: Option<Profile>,
    pub effective_profile: Profile,
    pub escalated: bool,
    pub newly_required_stages: Vec<&'static str>,
    pub evidence_invalidated: bool,
}

/// A fresh decision record, or the superseding record of a previous decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub decision_id: String,
    pub task_id: String,
    pub assignment_intent: String,
    pub expected_surfaces: Vec<String>,
    pub pinned_policy_revision: String,
    pub profile: Profile,
    pub resolved: bool,
    pub risk_triggers: Vec<&'static str>,
    pub coverage_complete: bool,
    pub coverage_categories: Vec<&'static str>,
    pub reasons: Vec<String>,
    pub target_branch: String,
    pub comparison_base_commit: String,
    pub identity: Option<String>,
    pub transition: Transition,
    /// The previous decision id and the transition reasons, when this record supersedes one.
    pub supersedes: Option<(String, Vec<String>)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestError {
    /// The request is not readable JSON. The CLI exits 2 for this.
    Unreadable(String),
    /// The request breaks the validation rules. The CLI exits 1 and prints the reasons.
    Blocked(Vec<String>),
    /// The request carries a feature this module does not port.
    Unported(&'static str),
}

/// Collects the change set of `repo` against `base`, as `fm-assignment-reassess.py collect` does.
pub fn collect(repo: &Path, base: &str) -> Collection {
    let mut result = Collection {
        base_ok: false,
        complete: false,
        categories: Vec::new(),
        gaps: Vec::new(),
        identity: None,
        audiences: None,
        paths: Vec::new(),
    };
    let top = match git(repo, &["rev-parse", "--show-toplevel"]) {
        Ok(out) => PathBuf::from(lossy(&out).trim()),
        Err(err) => {
            result.gaps.push(format!("not a git worktree: {err}"));
            return result;
        }
    };
    let commit_spec = format!("{base}^{{commit}}");
    let base_ok = is_sha(base) && git(&top, &["cat-file", "-e", commit_spec.as_str()]).is_ok();
    result.base_ok = base_ok;

    let committed = if base_ok {
        query(
            &mut result.gaps,
            "committed",
            git(
                &top,
                &[
                    "diff",
                    "--name-status",
                    "-z",
                    "-M",
                    "--no-ext-diff",
                    base,
                    "HEAD",
                    "--",
                ],
            ),
            parse_name_status,
        )
    } else {
        None
    };
    let tracked = query(
        &mut result.gaps,
        "tracked-working-tree",
        git(
            &top,
            &[
                "diff",
                "--name-status",
                "-z",
                "-M",
                "--no-ext-diff",
                "HEAD",
                "--",
            ],
        ),
        parse_name_status,
    );
    let untracked = query(
        &mut result.gaps,
        "untracked",
        git(&top, &["ls-files", "-z", "--others", "--exclude-standard"]),
        parse_listing,
    );

    let mut covered = BTreeSet::new();
    if committed.is_some() {
        covered.insert("committed");
    }
    if tracked.is_some() {
        covered.insert("tracked-working-tree");
    }
    if untracked.is_some() {
        covered.insert("untracked");
    }
    if committed.is_some() && tracked.is_some() {
        covered.extend(["renamed", "deleted"]);
    }
    result.categories = COVERAGE_CATEGORIES
        .iter()
        .copied()
        .filter(|name| covered.contains(name))
        .collect();
    result.complete = result.categories.len() == COVERAGE_CATEGORIES.len();

    let mut paths = BTreeSet::new();
    for found in [&committed, &tracked, &untracked].into_iter().flatten() {
        paths.extend(found.iter().cloned());
    }
    for path in paths {
        let (kind, worktree) = read_worktree(&top.join(&path));
        let base_bytes = if base_ok {
            base_blob(&top, base, &path)
        } else {
            None
        };
        result.paths.push(ChangedPath {
            path,
            base: base_bytes,
            worktree,
            kind,
        });
    }

    if base_ok {
        result.audiences = load_audiences(&top, base);
        if let (true, Some(untracked)) = (result.complete, &untracked) {
            result.identity = change_identity(&top, base, untracked);
        }
    }
    result
}

/// Classifies a validated request against the collected change set and returns the decision. A
/// request with a previous record returns its superseding record, and `Unported` when nothing
/// changed since that record, because Python then echoes the previous record as it was written.
pub fn reassess(collection: &Collection, request: &Request) -> Result<Decision, RequestError> {
    let classification = classify(
        collection,
        request.risk_triggers.as_deref(),
        request.small_low_risk_conditions.as_ref(),
    );
    let previous = request.previous.as_ref();
    let requested = request.risk_triggers.clone().unwrap_or_default();
    let inherited = previous
        .map(|prior| prior.risk_triggers.clone())
        .unwrap_or_default();
    let risk_triggers: Vec<&'static str> = RISK_TRIGGERS
        .iter()
        .copied()
        .filter(|name| {
            requested
                .iter()
                .chain(inherited.iter())
                .any(|item| item.as_str() == *name)
        })
        .collect();
    let profile = if !risk_triggers.is_empty() {
        Profile::HighRisk
    } else {
        match previous {
            Some(prior) if prior.profile.rank() > classification.profile.rank() => prior.profile,
            _ => classification.profile,
        }
    };
    let stage = if classification.resolved {
        "final"
    } else {
        "provisional"
    };
    let status = if classification.resolved {
        "resolved"
    } else {
        "unresolved"
    };
    let identity = collection.identity.clone();

    let mut transition_reasons = Vec::new();
    if let Some(prior) = previous {
        if profile != prior.profile {
            transition_reasons.push(format!(
                "profile escalated from {} to {}",
                prior.profile.as_str(),
                profile.as_str()
            ));
        }
        let added: Vec<&str> = risk_triggers
            .iter()
            .copied()
            .filter(|name| !prior.risk_triggers.iter().any(|item| item == name))
            .collect();
        if !added.is_empty() {
            transition_reasons.push(format!("risk triggers recorded: {}", added.join(", ")));
        }
        if (stage, status)
            != (
                prior.decision_stage.as_str(),
                prior.classification_status.as_str(),
            )
        {
            transition_reasons.push(format!("decision is now {stage} and {status}"));
        }
        if identity != prior.evaluated_change_identity {
            transition_reasons.push("change identity changed".to_string());
        }
        let mut categories = collection.categories.clone();
        categories.sort_unstable();
        let mut prior_categories: Vec<&str> = prior
            .coverage_categories
            .iter()
            .map(String::as_str)
            .collect();
        prior_categories.sort_unstable();
        if (collection.complete, categories) != (prior.coverage_complete, prior_categories) {
            transition_reasons.push("change coverage changed".to_string());
        }
        if request.comparison_base_commit != prior.comparison_base_commit {
            transition_reasons.push("comparison base changed".to_string());
        }
        if request.pinned_policy_revision != prior.pinned_policy_revision {
            transition_reasons.push("pinned policy revision changed".to_string());
        }
        if transition_reasons.is_empty() {
            return Err(RequestError::Unported("unchanged previous record"));
        }
    }

    let transition = Transition {
        previous_profile: previous.map(|prior| prior.profile),
        effective_profile: profile,
        escalated: previous.is_some_and(|prior| profile.rank() > prior.profile.rank()),
        newly_required_stages: profile
            .stages()
            .iter()
            .copied()
            .filter(|name| previous.is_none_or(|prior| !prior.profile.stages().contains(name)))
            .collect(),
        evidence_invalidated: previous.is_some_and(|prior| {
            prior.evaluated_change_identity.is_some() && prior.evaluated_change_identity != identity
        }),
    };
    let (decision_id, supersedes) = match previous {
        None => (format!("{}/1", request.task_id), None),
        Some(prior) => (
            format!(
                "{}/{}",
                request.task_id,
                next_sequence(decision_sequence(&prior.decision_id).unwrap_or_default())
            ),
            Some((prior.decision_id.clone(), transition_reasons.clone())),
        ),
    };
    let mut reasons: Vec<String> = previous
        .map(|prior| prior.reasons.clone())
        .unwrap_or_default();
    reasons.extend(classification.reasons);
    reasons.extend(transition_reasons);

    Ok(Decision {
        decision_id,
        task_id: request.task_id.clone(),
        assignment_intent: request.assignment_intent.clone(),
        expected_surfaces: request.expected_surfaces.clone(),
        pinned_policy_revision: request.pinned_policy_revision.clone(),
        profile,
        resolved: classification.resolved,
        risk_triggers,
        coverage_complete: collection.complete,
        coverage_categories: collection.categories.clone(),
        reasons: dedupe(reasons),
        target_branch: request.target_branch.clone(),
        comparison_base_commit: request.comparison_base_commit.clone(),
        identity,
        transition,
        supersedes,
    })
}

/// Parses and validates request JSON as `fm-assignment-reassess.py reassess` does. A non-null
/// `previous` record is validated after the request's own problems, as the Python validator does.
pub fn parse_request(text: &str) -> Result<Request, RequestError> {
    let value: Value =
        serde_json::from_str(text).map_err(|err| RequestError::Unreadable(err.to_string()))?;
    let Some(object) = value.as_object() else {
        return Err(RequestError::Blocked(vec![
            "request must be a JSON object".to_string(),
        ]));
    };

    let mut problems = Vec::new();
    let unknown: Vec<&str> = object
        .keys()
        .map(String::as_str)
        .filter(|key| !REQUIRED_REQUEST_KEYS.contains(key) && !OPTIONAL_REQUEST_KEYS.contains(key))
        .collect();
    if !unknown.is_empty() {
        problems.push(format!("request has unknown keys: {}", unknown.join(", ")));
    }
    for key in REQUIRED_REQUEST_KEYS {
        if !object.contains_key(*key) {
            problems.push(format!("request is missing {key}"));
        }
    }
    let text_of = |key: &str| object.get(key).and_then(Value::as_str);
    if !text_of("task_id").is_some_and(|id| is_nonempty(id) && !id.contains('/')) {
        problems.push("task_id must be a non-empty string without '/'".to_string());
    }
    for key in [
        "pinned_policy_revision",
        "target_branch",
        "assignment_intent",
    ] {
        if object.contains_key(key) && !text_of(key).is_some_and(is_nonempty) {
            problems.push(format!("{key} must be a non-empty string"));
        }
    }
    if !text_of("comparison_base_commit").is_some_and(is_sha) {
        problems.push("comparison_base_commit must be a 40-hex commit".to_string());
    }
    let surfaces = object.get("expected_surfaces").and_then(Value::as_array);
    let surfaces_valid = surfaces.is_some_and(|items| {
        let names: Vec<&str> = items.iter().filter_map(Value::as_str).collect();
        names.len() == items.len() && names.iter().all(|name| is_nonempty(name)) && unique(&names)
    });
    if object.contains_key("expected_surfaces") && !surfaces_valid {
        problems.push("expected_surfaces must be a unique list of non-empty paths".to_string());
    }
    let triggers = match object.get("risk_triggers") {
        None | Some(Value::Null) => None,
        Some(value) => match value.as_array() {
            Some(items) if triggers_valid(items) => Some(
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_string))
                    .collect::<Vec<String>>(),
            ),
            _ => {
                problems.push(
                    "risk_triggers must be null or a unique list of known trigger names"
                        .to_string(),
                );
                None
            }
        },
    };
    let conditions = match object.get("small_low_risk_conditions") {
        None | Some(Value::Null) => None,
        Some(Value::Object(map)) => {
            let extra: Vec<&str> = map
                .keys()
                .map(String::as_str)
                .filter(|key| !SMALL_LOW_RISK_CONDITIONS.contains(key))
                .collect();
            if !extra.is_empty() {
                problems.push(format!(
                    "small_low_risk_conditions has unknown keys: {}",
                    extra.join(", ")
                ));
            }
            if map.values().any(|value| !value.is_boolean()) {
                problems.push("small_low_risk_conditions values must be booleans".to_string());
            }
            Some(
                map.iter()
                    .filter_map(|(key, value)| value.as_bool().map(|flag| (key.clone(), flag)))
                    .collect::<BTreeMap<String, bool>>(),
            )
        }
        Some(_) => {
            problems.push("small_low_risk_conditions must be an object".to_string());
            None
        }
    };
    let mut previous = None;
    if let Some(record) = object.get("previous").filter(|record| !record.is_null()) {
        let record_problems = previous_problems(record, object);
        if record_problems.is_empty() {
            previous = Some(previous_record(record));
        }
        problems.extend(record_problems);
    }
    if !problems.is_empty() {
        return Err(RequestError::Blocked(problems));
    }
    let owned = |key: &str| text_of(key).unwrap_or_default().to_string();
    Ok(Request {
        task_id: owned("task_id"),
        pinned_policy_revision: owned("pinned_policy_revision"),
        target_branch: owned("target_branch"),
        comparison_base_commit: owned("comparison_base_commit"),
        assignment_intent: owned("assignment_intent"),
        expected_surfaces: surfaces
            .into_iter()
            .flatten()
            .filter_map(|item| item.as_str().map(str::to_string))
            .collect(),
        risk_triggers: triggers,
        small_low_risk_conditions: conditions,
        previous,
    })
}

/// Renders `collect` stdout: the same members in the same order and layout as the Python output.
pub fn render_collect(collection: &Collection) -> String {
    let paths = collection
        .paths
        .iter()
        .map(|item| {
            Out::Object(vec![
                ("path", Out::Str(item.path.clone())),
                ("in_base", Out::Bool(item.base.is_some())),
                ("in_worktree", Out::Bool(item.kind == WorktreeKind::File)),
                ("kind", Out::Str(item.kind.as_str().to_string())),
            ])
        })
        .collect();
    let payload = Out::Object(vec![
        ("complete", Out::Bool(collection.complete)),
        ("categories", strings(collection.categories.iter().copied())),
        ("gaps", strings(collection.gaps.iter().map(String::as_str))),
        ("base_ok", Out::Bool(collection.base_ok)),
        ("identity", optional(collection.identity.as_deref())),
        ("paths", Out::Array(paths)),
    ]);
    payload.render()
}

/// Renders `reassess` stdout for a fresh decision.
pub fn render_reassess(decision: &Decision) -> String {
    let mut members = vec![
        ("contract_version", Out::Int(CONTRACT_VERSION)),
        ("decision_id", Out::Str(decision.decision_id.clone())),
        ("task_id", Out::Str(decision.task_id.clone())),
        (
            "assignment_intent",
            Out::Str(decision.assignment_intent.clone()),
        ),
        (
            "expected_surfaces",
            strings(decision.expected_surfaces.iter().map(String::as_str)),
        ),
        (
            "pinned_policy_revision",
            Out::Str(decision.pinned_policy_revision.clone()),
        ),
        ("profile", Out::Str(decision.profile.as_str().to_string())),
        (
            "decision_stage",
            Out::Str(
                if decision.resolved {
                    "final"
                } else {
                    "provisional"
                }
                .to_string(),
            ),
        ),
        (
            "classification_status",
            Out::Str(
                if decision.resolved {
                    "resolved"
                } else {
                    "unresolved"
                }
                .to_string(),
            ),
        ),
        (
            "risk_triggers",
            strings(decision.risk_triggers.iter().copied()),
        ),
        (
            "change_coverage",
            Out::Object(vec![
                ("complete", Out::Bool(decision.coverage_complete)),
                (
                    "categories",
                    strings(decision.coverage_categories.iter().copied()),
                ),
            ]),
        ),
        (
            "reasons",
            strings(decision.reasons.iter().map(String::as_str)),
        ),
        (
            "required_stages",
            strings(decision.profile.stages().iter().copied()),
        ),
        (
            "evidence_obligations",
            Out::Object(vec![
                (
                    "tests_required",
                    Out::Bool(decision.profile != Profile::DocumentationOnly),
                ),
                (
                    "independent_change_review_required",
                    Out::Bool(decision.profile == Profile::HighRisk),
                ),
            ]),
        ),
        ("target_branch", Out::Str(decision.target_branch.clone())),
        (
            "comparison_base_commit",
            Out::Str(decision.comparison_base_commit.clone()),
        ),
    ];
    if let Some(identity) = &decision.identity {
        members.push(("evaluated_change_identity", Out::Str(identity.clone())));
    }
    if let Some((previous_id, escalation_reasons)) = &decision.supersedes {
        members.push((
            "supersedes",
            Out::Object(vec![
                ("decision_id", Out::Str(previous_id.clone())),
                (
                    "escalation_reasons",
                    strings(escalation_reasons.iter().map(String::as_str)),
                ),
            ]),
        ));
    }
    let transition = &decision.transition;
    let transition = Out::Object(vec![
        (
            "previous_profile",
            optional(transition.previous_profile.map(Profile::as_str)),
        ),
        (
            "effective_profile",
            Out::Str(transition.effective_profile.as_str().to_string()),
        ),
        ("escalated", Out::Bool(transition.escalated)),
        (
            "newly_required_stages",
            strings(transition.newly_required_stages.iter().copied()),
        ),
        (
            "evidence_invalidated",
            Out::Bool(transition.evidence_invalidated),
        ),
    ]);
    Out::Object(vec![
        ("status", Out::Str("recorded".to_string())),
        ("decision", Out::Object(members)),
        ("transition", transition),
    ])
    .render()
}

impl Profile {
    /// The Python `PROFILE_RANK` order: documentation-only, small-low-risk, standard, high-risk.
    fn rank(self) -> u8 {
        match self {
            Profile::DocumentationOnly => 0,
            Profile::SmallLowRisk => 1,
            Profile::Standard => 2,
            Profile::HighRisk => 3,
        }
    }
}

fn profile_named(name: &str) -> Option<Profile> {
    [
        Profile::DocumentationOnly,
        Profile::SmallLowRisk,
        Profile::Standard,
        Profile::HighRisk,
    ]
    .into_iter()
    .find(|profile| profile.as_str() == name)
}

/// Renders the `blocked` outcome of `fm-assignment-reassess.py reassess`, exit 1.
pub fn render_blocked(reasons: &[String]) -> String {
    Out::Object(vec![
        ("status", Out::Str("blocked".to_string())),
        ("reasons", strings(reasons.iter().map(String::as_str))),
    ])
    .render()
}

/// Python `validate_record`: every problem with a previous decision record, in the reference's
/// order. `request` is the request object that the record is checked against.
fn previous_problems(record: &Value, request: &Map<String, Value>) -> Vec<String> {
    let Some(object) = record.as_object() else {
        return vec!["previous must be a decision record object".to_string()];
    };
    let mut problems = Vec::new();
    let mut unknown: Vec<&str> = object
        .keys()
        .map(String::as_str)
        .filter(|key| !RECORD_KEYS.contains(key) && !OPTIONAL_RECORD_KEYS.contains(key))
        .collect();
    unknown.sort_unstable();
    if !unknown.is_empty() {
        problems.push(format!("previous has unknown keys: {}", unknown.join(", ")));
    }
    let mut missing: Vec<&str> = RECORD_KEYS
        .iter()
        .copied()
        .filter(|key| !object.contains_key(*key))
        .collect();
    missing.sort_unstable();
    if !missing.is_empty() {
        problems.push(format!("previous is missing keys: {}", missing.join(", ")));
    }

    let null = Value::Null;
    let field = |key: &str| object.get(key).unwrap_or(&null);
    let request_field = |key: &str| request.get(key).unwrap_or(&null);
    let request_task = request_field("task_id").as_str();
    if field("contract_version").as_f64() != Some(1.0) {
        problems.push(format!(
            "previous contract_version must be {CONTRACT_VERSION}"
        ));
    }
    let id_matches = field("decision_id").as_str().is_some_and(|id| {
        decision_sequence(id).is_some() && id.split_once('/').map(|(left, _)| left) == request_task
    });
    if !id_matches {
        problems.push(
            "previous decision_id must be <task_id>/<sequence> for the request task_id".to_string(),
        );
    }
    if field("task_id") != request_field("task_id") {
        problems.push("previous task_id differs from the request task_id".to_string());
    }
    if field("assignment_intent") != request_field("assignment_intent") {
        problems.push("previous assignment_intent differs from the request".to_string());
    }
    if field("expected_surfaces") != request_field("expected_surfaces") {
        problems.push("previous expected_surfaces differ from the request".to_string());
    }
    if field("target_branch") != request_field("target_branch") {
        problems
            .push("previous target_branch differs from the host-pinned target_branch".to_string());
    }
    if !field("pinned_policy_revision")
        .as_str()
        .is_some_and(is_nonempty)
    {
        problems.push("previous pinned_policy_revision must be a non-empty string".to_string());
    }
    if !field("comparison_base_commit").as_str().is_some_and(is_sha) {
        problems.push("previous comparison_base_commit must be a 40-hex commit".to_string());
    }

    let profile = field("profile").as_str().and_then(profile_named);
    if profile.is_none() {
        problems.push("previous profile is not a known profile id".to_string());
    }
    let stage = field("decision_stage").as_str();
    if !matches!(stage, Some("provisional" | "final")) {
        problems.push("previous decision_stage must be provisional or final".to_string());
    }
    let status = field("classification_status").as_str();
    if !matches!(status, Some("resolved" | "unresolved")) {
        problems.push("previous classification_status must be resolved or unresolved".to_string());
    }
    match field("risk_triggers").as_array() {
        Some(items) if triggers_valid(items) => {
            if !items.is_empty() && field("profile").as_str() != Some("high-risk") {
                problems.push("previous risk_triggers require profile high-risk".to_string());
            }
        }
        _ => problems.push(
            "previous risk_triggers must be a unique list of known trigger names".to_string(),
        ),
    }

    let coverage = field("change_coverage").as_object().filter(|map| {
        map.len() == 2 && map.contains_key("complete") && map.contains_key("categories")
    });
    let complete = coverage
        .and_then(|map| map.get("complete"))
        .and_then(Value::as_bool);
    let categories = coverage
        .and_then(|map| map.get("categories"))
        .and_then(Value::as_array)
        .filter(|items| category_list_valid(items));
    match (complete, categories) {
        (Some(complete), Some(categories)) => {
            if complete != (categories.len() == COVERAGE_CATEGORIES.len()) {
                problems.push(
                    "previous change_coverage.complete disagrees with its categories".to_string(),
                );
            }
            if status == Some("resolved") && !complete {
                problems.push(
                    "previous resolved decision cannot have incomplete change_coverage".to_string(),
                );
            }
        }
        _ => problems.push(
            "previous change_coverage must be {complete, categories} with known categories"
                .to_string(),
        ),
    }

    let reasons_valid = field("reasons").as_array().is_some_and(|items| {
        !items.is_empty()
            && items
                .iter()
                .all(|item| item.as_str().is_some_and(is_nonempty))
    });
    if !reasons_valid {
        problems.push("previous reasons must be a non-empty list of strings".to_string());
    }
    if let Some(profile) = profile {
        let stages = Value::Array(
            profile
                .stages()
                .iter()
                .map(|name| Value::String((*name).to_string()))
                .collect(),
        );
        if field("required_stages") != &stages {
            problems.push("previous required_stages do not match its profile".to_string());
        }
        let obligations = json!({
            "tests_required": profile != Profile::DocumentationOnly,
            "independent_change_review_required": profile == Profile::HighRisk,
        });
        if field("evidence_obligations") != &obligations {
            problems.push("previous evidence_obligations do not match its profile".to_string());
        }
    }

    let identity = field("evaluated_change_identity").as_str();
    if stage == Some("final") {
        if status != Some("resolved") {
            problems.push("final decision must be resolved".to_string());
        }
        if !identity.is_some_and(is_nonempty) {
            problems.push("final decision requires evaluated_change_identity".to_string());
        }
    }
    if status == Some("unresolved")
        && (stage != Some("provisional") || profile != Some(Profile::HighRisk))
    {
        problems.push("unresolved decision must be provisional high-risk".to_string());
    }
    if object.contains_key("evaluated_change_identity") && !identity.is_some_and(is_nonempty) {
        problems.push("previous evaluated_change_identity must be a non-empty string".to_string());
    }
    if object.contains_key("supersedes") && !supersedes_valid(field("supersedes")) {
        problems.push("previous supersedes is malformed".to_string());
    }
    problems
}

/// Python's `supersedes` check: exactly `decision_id` and a non-empty `escalation_reasons`.
fn supersedes_valid(value: &Value) -> bool {
    let Some(map) = value.as_object() else {
        return false;
    };
    map.len() == 2
        && map
            .get("decision_id")
            .and_then(Value::as_str)
            .is_some_and(|id| decision_sequence(id).is_some())
        && map
            .get("escalation_reasons")
            .and_then(Value::as_array)
            .is_some_and(|items| {
                !items.is_empty()
                    && items
                        .iter()
                        .all(|item| item.as_str().is_some_and(is_nonempty))
            })
}

/// The previous record a validated `previous` value describes. Call only after
/// [`previous_problems`] returned no problem.
fn previous_record(record: &Value) -> PreviousRecord {
    let text = |key: &str| record[key].as_str().unwrap_or_default().to_string();
    let coverage = &record["change_coverage"];
    PreviousRecord {
        decision_id: text("decision_id"),
        profile: profile_named(record["profile"].as_str().unwrap_or_default())
            .expect("validated profile"),
        decision_stage: text("decision_stage"),
        classification_status: text("classification_status"),
        risk_triggers: strings_of(&record["risk_triggers"]),
        coverage_complete: coverage["complete"].as_bool().unwrap_or(false),
        coverage_categories: strings_of(&coverage["categories"]),
        reasons: strings_of(&record["reasons"]),
        evaluated_change_identity: record["evaluated_change_identity"]
            .as_str()
            .map(str::to_string),
        comparison_base_commit: text("comparison_base_commit"),
        pinned_policy_revision: text("pinned_policy_revision"),
    }
}

fn strings_of(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item.as_str().map(str::to_string))
        .collect()
}

/// The sequence digits of a decision id `<task>/<n>`, `n` being `[1-9][0-9]*`. Python's `$` also
/// accepts one trailing newline; that newline stays in the id but is not part of the number.
fn decision_sequence(id: &str) -> Option<&str> {
    sequence_digits(id).or_else(|| id.strip_suffix('\n').and_then(sequence_digits))
}

fn sequence_digits(text: &str) -> Option<&str> {
    let (left, right) = text.split_once('/')?;
    let well_formed = !left.is_empty()
        && !right.is_empty()
        && !right.starts_with('0')
        && right.bytes().all(|byte| byte.is_ascii_digit());
    well_formed.then_some(right)
}

/// The decimal successor of a digit string without a leading zero, of any length.
fn next_sequence(digits: &str) -> String {
    let mut bytes = digits.as_bytes().to_vec();
    let mut index = bytes.len();
    loop {
        if index == 0 {
            bytes.insert(0, b'1');
            break;
        }
        index -= 1;
        if bytes[index] == b'9' {
            bytes[index] = b'0';
        } else {
            bytes[index] += 1;
            break;
        }
    }
    String::from_utf8(bytes).expect("ASCII digits")
}

fn classify(
    collection: &Collection,
    triggers: Option<&[String]>,
    conditions: Option<&BTreeMap<String, bool>>,
) -> Classification {
    let mut unresolved: Vec<String> = Vec::new();
    let mut reasons: Vec<String> = Vec::new();
    let mut all_prose = !collection.paths.is_empty();
    let mut prose_clean = true;
    for item in &collection.paths {
        let class = path_class(&item.path, collection.audiences.as_ref());
        if class != PathClass::Prose {
            all_prose = false;
        }
        if item.kind == WorktreeKind::Other {
            unresolved.push(format!("non-regular worktree path: {}", item.path));
        } else if class == PathClass::Prose {
            match prose_verdict(item.base.as_deref(), item.worktree.as_deref()) {
                ProseVerdict::Clean => {}
                ProseVerdict::Unverifiable => unresolved.push(format!(
                    "prose cannot be decoded for line checks: {}",
                    item.path
                )),
                ProseVerdict::Executable => {
                    prose_clean = false;
                    reasons.push(format!("{}: executable change in prose", item.path));
                }
            }
        } else if class == PathClass::Unknown {
            unresolved.push(format!("unknown path: {}", item.path));
        } else {
            reasons.push(format!("{}: {} change", item.path, class.as_str()));
        }
    }
    if collection.paths.is_empty() {
        unresolved.push("no changed paths were collected".to_string());
    }
    if !collection.base_ok {
        unresolved.push("comparison base is not resolvable in the worktree".to_string());
    }
    let missing: Vec<&str> = COVERAGE_CATEGORIES
        .iter()
        .copied()
        .filter(|name| !collection.categories.contains(name))
        .collect();
    if !missing.is_empty() {
        unresolved.push(format!(
            "change collection incomplete, missing: {}",
            missing.join(", ")
        ));
    }
    unresolved.extend(
        collection
            .gaps
            .iter()
            .map(|gap| format!("collection gap: {gap}")),
    );
    if collection.audiences.is_none() && collection.base_ok {
        unresolved.push("policy inventory is unavailable at the comparison base".to_string());
    }
    if collection.complete && collection.base_ok && collection.identity.is_none() {
        unresolved.push("change identity could not be computed".to_string());
    }
    if triggers.is_none() {
        unresolved.push("risk is not assessed: risk_triggers is absent".to_string());
    }

    let triggers = triggers.unwrap_or(&[]);
    let gaps = small_low_risk_gaps(conditions);
    let (profile, resolved) = if !unresolved.is_empty() {
        (Profile::HighRisk, false)
    } else if !triggers.is_empty() {
        (Profile::HighRisk, true)
    } else if all_prose && prose_clean {
        (Profile::DocumentationOnly, true)
    } else if !collection.paths.is_empty() && gaps.is_empty() {
        (Profile::SmallLowRisk, true)
    } else {
        (Profile::Standard, true)
    };

    let mut all = unresolved;
    all.extend(triggers.iter().map(|name| format!("risk trigger: {name}")));
    all.extend(reasons);
    match profile {
        Profile::DocumentationOnly => all.push(
            "every changed path is prose with no executable or unverifiable lines".to_string(),
        ),
        Profile::SmallLowRisk => all
            .push("all small-low-risk conditions attested for a single bounded change".to_string()),
        Profile::Standard => all.push(format!(
            "default standard; small-low-risk not met: {}",
            gaps.join(", ")
        )),
        Profile::HighRisk => {}
    }
    Classification {
        profile,
        resolved,
        reasons: all,
    }
}

struct Classification {
    profile: Profile,
    resolved: bool,
    reasons: Vec<String>,
}

/// The line-level verdict of a changed prose path, as `prose_verdict` in the Python reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProseVerdict {
    Clean,
    Executable,
    Unverifiable,
}

/// Compares the base and worktree versions of a prose path line by line. A changed line inside a
/// fenced block, on either side, makes the change executable.
fn prose_verdict(base: Option<&[u8]>, worktree: Option<&[u8]>) -> ProseVerdict {
    let (Some(old_lines), Some(new_lines)) = (prose_lines(base), prose_lines(worktree)) else {
        return ProseVerdict::Unverifiable;
    };
    let old_fenced = fenced_lines(&old_lines);
    let new_fenced = fenced_lines(&new_lines);
    for opcode in get_opcodes(&old_lines, &new_lines) {
        if opcode.tag == Tag::Equal {
            continue;
        }
        if old_fenced[opcode.i1..opcode.i2]
            .iter()
            .any(|fenced| *fenced)
        {
            return ProseVerdict::Executable;
        }
        if new_fenced[opcode.j1..opcode.j2]
            .iter()
            .any(|fenced| *fenced)
        {
            return ProseVerdict::Executable;
        }
    }
    ProseVerdict::Clean
}

/// The UTF-8 lines of one side; an absent side has no lines. Undecodable bytes give `None`.
fn prose_lines(bytes: Option<&[u8]>) -> Option<Vec<&str>> {
    match bytes {
        None => Some(Vec::new()),
        Some(bytes) => std::str::from_utf8(bytes).ok().map(py_splitlines),
    }
}

/// Python `str.splitlines()`: breaks at `\n`, `\r`, `\r\n`, `\v`, `\f`, `\x1c`, `\x1d`, `\x1e`,
/// `\x85`, `\u2028` and `\u2029`, and adds no empty line after a final break.
fn py_splitlines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((index, character)) = chars.next() {
        let end = match character {
            '\r' => match chars.peek() {
                Some(&(_, '\n')) => {
                    chars.next();
                    index + 2
                }
                _ => index + 1,
            },
            '\n' | '\u{b}' | '\u{c}' | '\u{1c}' | '\u{1d}' | '\u{1e}' | '\u{85}' | '\u{2028}'
            | '\u{2029}' => index + character.len_utf8(),
            _ => continue,
        };
        lines.push(&text[start..index]);
        start = end;
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// Python `str.isspace()`, which the reference's `\s` and `str.strip()` use: Unicode white space
/// plus the file and group separators `\x1c` to `\x1f`.
fn py_isspace(character: char) -> bool {
    character.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&character)
}

/// The fence run that `FENCE_RE` (`^\s*(`{3,}|~{3,})`) captures at the start of a line, if any.
fn fence_run(line: &str) -> Option<&str> {
    let rest = line.trim_start_matches(py_isspace);
    let marker = rest.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let run = rest.chars().take_while(|c| *c == marker).count();
    (run >= 3).then(|| &rest[..run])
}

/// For each line (0-based), whether it lies inside or on the boundary of a fenced block, as
/// `fenced_lines` in the Python reference computes it. A fence opens on a line that starts with a
/// run of three or more backticks or tildes, and closes on a line holding only a run of the same
/// character that is at least as long.
fn fenced_lines(lines: &[&str]) -> Vec<bool> {
    let mut fenced = vec![false; lines.len()];
    let mut marker: Option<&str> = None;
    for (index, line) in lines.iter().enumerate() {
        let run = fence_run(line);
        match (marker, run) {
            (None, Some(run)) => {
                marker = Some(run);
                fenced[index] = true;
            }
            (None, None) => {}
            (Some(open), run) => {
                fenced[index] = true;
                if let Some(run) = run
                    && run.as_bytes()[0] == open.as_bytes()[0]
                    && run.len() >= open.len()
                    && line
                        .trim_matches(py_isspace)
                        .get(run.len()..)
                        .unwrap_or("")
                        .is_empty()
                {
                    marker = None;
                }
            }
        }
    }
    fenced
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathClass {
    Prose,
    Behavioral,
    ExecutableExample,
    Code,
    Unknown,
}

impl PathClass {
    fn as_str(self) -> &'static str {
        match self {
            PathClass::Prose => "prose",
            PathClass::Behavioral => "behavioral",
            PathClass::ExecutableExample => "executable-example",
            PathClass::Code => "code",
            PathClass::Unknown => "unknown",
        }
    }
}

fn path_class(path: &str, audiences: Option<&BTreeMap<String, String>>) -> PathClass {
    if path.starts_with(EXAMPLE_PREFIX) {
        return PathClass::ExecutableExample;
    }
    let audience = audiences.and_then(|map| map.get(path)).map(String::as_str);
    if audience == Some("agent-runtime") {
        return PathClass::Behavioral;
    }
    let extension = path_extension(path);
    if PROSE_EXTENSIONS.contains(&extension.as_str())
        && audience.is_some_and(|a| PROSE_AUDIENCES.contains(&a))
    {
        return PathClass::Prose;
    }
    if CODE_EXTENSIONS.contains(&extension.as_str()) {
        return PathClass::Code;
    }
    PathClass::Unknown
}

/// Python `os.path.splitext(path)[1].lower()`: the text from the last dot of the final path
/// component after its leading dots, or empty when there is none, so `..md` has no extension.
/// `Path::extension` is not used because it returns `md` for `..md`.
fn path_extension(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    let stem = name.trim_start_matches('.');
    stem.rfind('.')
        .map(|index| stem[index..].to_lowercase())
        .unwrap_or_default()
}

fn small_low_risk_gaps(conditions: Option<&BTreeMap<String, bool>>) -> Vec<&'static str> {
    SMALL_LOW_RISK_CONDITIONS
        .iter()
        .copied()
        .filter(|name| conditions.and_then(|map| map.get(*name)) != Some(&true))
        .collect()
}

fn dedupe(items: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    items
        .into_iter()
        .filter(|item| seen.insert(item.clone()))
        .collect()
}

fn triggers_valid(items: &[Value]) -> bool {
    let names: Vec<&str> = items.iter().filter_map(Value::as_str).collect();
    names.len() == items.len()
        && names.iter().all(|name| RISK_TRIGGERS.contains(name))
        && unique(&names)
}

fn category_list_valid(items: &[Value]) -> bool {
    let names: Vec<&str> = items.iter().filter_map(Value::as_str).collect();
    names.len() == items.len()
        && names.iter().all(|name| COVERAGE_CATEGORIES.contains(name))
        && unique(&names)
}

fn unique(names: &[&str]) -> bool {
    names.iter().collect::<BTreeSet<_>>().len() == names.len()
}

fn is_nonempty(text: &str) -> bool {
    !text.trim_matches(py_isspace).is_empty()
}

/// Python's `SHA_RE` (`^[0-9a-f]{40}$`) accepts one trailing newline, because `$` matches before a
/// final newline. The port keeps that acceptance; `collect` then fails the commit check for it.
fn is_sha(text: &str) -> bool {
    let hex = text.strip_suffix('\n').unwrap_or(text);
    hex.len() == 40
        && hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn query<T>(
    gaps: &mut Vec<String>,
    label: &str,
    output: Result<Vec<u8>, String>,
    parse: impl Fn(&[u8]) -> T,
) -> Option<T> {
    match output {
        Ok(bytes) => Some(parse(&bytes)),
        Err(err) => {
            gaps.push(format!("{label}: {err}"));
            None
        }
    }
}

fn parse_name_status(output: &[u8]) -> BTreeSet<String> {
    let tokens: Vec<&[u8]> = output.split(|byte| *byte == 0).collect();
    let mut paths = BTreeSet::new();
    let mut index = 0;
    while index < tokens.len() {
        let status = tokens[index];
        index += 1;
        if status.is_empty() {
            continue;
        }
        let width = if matches!(status.first(), Some(b'R' | b'C')) {
            2
        } else {
            1
        };
        let end = (index + width).min(tokens.len());
        for token in &tokens[index..end] {
            paths.insert(lossy(token));
        }
        index += width;
    }
    paths
}

fn parse_listing(output: &[u8]) -> BTreeSet<String> {
    output
        .split(|byte| *byte == 0)
        .filter(|token| !token.is_empty())
        .map(lossy)
        .collect()
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn git(dir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|err| format!("git unavailable: {err}"))?;
    if !output.status.success() {
        let code = output.status.code().unwrap_or(-1);
        return Err(format!("git {} exited {code}", args[0]));
    }
    Ok(output.stdout)
}

/// The worktree state of `full` and, for a regular file, its bytes. A file that cannot be read is
/// `Other`, which the Python reference reports as an error instead.
fn read_worktree(full: &Path) -> (WorktreeKind, Option<Vec<u8>>) {
    match fs::symlink_metadata(full) {
        Ok(info) if info.file_type().is_file() => match fs::read(full) {
            Ok(bytes) => (WorktreeKind::File, Some(bytes)),
            Err(_) => (WorktreeKind::Other, None),
        },
        Ok(_) => (WorktreeKind::Other, None),
        Err(err) if matches!(err.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            (WorktreeKind::Absent, None)
        }
        Err(_) => (WorktreeKind::Other, None),
    }
}

fn base_blob(top: &Path, base: &str, path: &str) -> Option<Vec<u8>> {
    let spec = format!("{base}:{path}");
    let kind = git(top, &["cat-file", "-t", spec.as_str()]).ok()?;
    if kind.trim_ascii() != b"blob" {
        return None;
    }
    git(top, &["cat-file", "blob", spec.as_str()]).ok()
}

fn load_audiences(top: &Path, base: &str) -> Option<BTreeMap<String, String>> {
    let spec = format!("{base}:{INVENTORY_PATH}");
    let bytes = git(top, &["show", spec.as_str()]).ok()?;
    let document: Value = serde_json::from_slice(&bytes).ok()?;
    let mut map = BTreeMap::new();
    for entry in document.get("surfaces")?.as_array()? {
        let path = entry.get("path")?.as_str()?;
        let audience = entry.get("audience")?.as_str()?;
        map.insert(path.to_string(), audience.to_string());
    }
    Some(map)
}

fn change_identity(top: &Path, base: &str, untracked: &BTreeSet<String>) -> Option<String> {
    let patch = git(
        top,
        &[
            "diff",
            "--binary",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "-M",
            base,
            "--",
        ],
    )
    .ok()?;
    let mut digest = Sha256::new();
    digest.update(b"assignment-change-identity/v1\0");
    digest.update(base.as_bytes());
    digest.update(b"\0");
    digest.update(b"patch\0");
    digest.update(&sha256::digest(&patch));
    for path in untracked {
        let (kind, data) = read_worktree(&top.join(path));
        let data = data.filter(|_| kind == WorktreeKind::File)?;
        digest.update(b"untracked\0");
        digest.update(path.as_bytes());
        digest.update(b"\0");
        digest.update(&sha256::digest(&data));
    }
    let hex: String = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Some(format!("sha256:{hex}"))
}

/// Output values in the order the Python reference inserts them, rendered as `json.dump(indent=2)`.
enum Out {
    Null,
    Bool(bool),
    Int(i64),
    Str(String),
    Array(Vec<Out>),
    Object(Vec<(&'static str, Out)>),
}

impl Out {
    fn render(&self) -> String {
        let mut text = String::new();
        write_out(&mut text, self, 0);
        text.push('\n');
        text
    }
}

fn write_out(text: &mut String, value: &Out, depth: usize) {
    match value {
        Out::Null => text.push_str("null"),
        Out::Bool(flag) => text.push_str(if *flag { "true" } else { "false" }),
        Out::Int(number) => {
            let _ = write!(text, "{number}");
        }
        Out::Str(string) => text.push_str(&escape_string(string)),
        Out::Array(items) if items.is_empty() => text.push_str("[]"),
        Out::Array(items) => {
            text.push_str("[\n");
            for (index, item) in items.iter().enumerate() {
                push_indent(text, depth + 1);
                write_out(text, item, depth + 1);
                text.push_str(if index + 1 < items.len() { ",\n" } else { "\n" });
            }
            push_indent(text, depth);
            text.push(']');
        }
        Out::Object(members) if members.is_empty() => text.push_str("{}"),
        Out::Object(members) => {
            text.push_str("{\n");
            for (index, (key, item)) in members.iter().enumerate() {
                push_indent(text, depth + 1);
                text.push_str(&escape_string(key));
                text.push_str(": ");
                write_out(text, item, depth + 1);
                text.push_str(if index + 1 < members.len() {
                    ",\n"
                } else {
                    "\n"
                });
            }
            push_indent(text, depth);
            text.push('}');
        }
    }
}

fn push_indent(text: &mut String, depth: usize) {
    for _ in 0..depth {
        text.push_str("  ");
    }
}

fn strings<'a>(items: impl Iterator<Item = &'a str>) -> Out {
    Out::Array(items.map(|item| Out::Str(item.to_string())).collect())
}

fn optional(value: Option<&str>) -> Out {
    value.map_or(Out::Null, |text| Out::Str(text.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reassess(collection: &Collection, request: &Request) -> Decision {
        super::reassess(collection, request).expect("a fresh request is ported")
    }

    fn file(path: &str) -> ChangedPath {
        ChangedPath {
            path: path.to_string(),
            base: None,
            worktree: None,
            kind: WorktreeKind::File,
        }
    }

    fn clean_collection(paths: Vec<ChangedPath>) -> Collection {
        Collection {
            base_ok: true,
            complete: true,
            categories: COVERAGE_CATEGORIES.to_vec(),
            gaps: Vec::new(),
            identity: Some("sha256:test".to_string()),
            audiences: Some(BTreeMap::new()),
            paths,
        }
    }

    fn request(
        triggers: Option<Vec<String>>,
        conditions: Option<BTreeMap<String, bool>>,
    ) -> Request {
        Request {
            task_id: "dpes-1".to_string(),
            pinned_policy_revision: "rev-1".to_string(),
            target_branch: "main".to_string(),
            comparison_base_commit: "a".repeat(40),
            assignment_intent: "intent".to_string(),
            expected_surfaces: vec!["scripts/x.sh".to_string()],
            risk_triggers: triggers,
            small_low_risk_conditions: conditions,
            previous: None,
        }
    }

    fn all_conditions(value: bool) -> BTreeMap<String, bool> {
        SMALL_LOW_RISK_CONDITIONS
            .iter()
            .map(|name| (name.to_string(), value))
            .collect()
    }

    // Python-derived. Reference: bin/fm-assignment-reassess.py at d80bcd15, run offline as
    // `python3 -I -B -X utf8 <ref>/bin/fm-assignment-reassess.py reassess --repo <repo> --request <request>`
    // with GIT_CONFIG_GLOBAL=/dev/null and GIT_CONFIG_NOSYSTEM=1. The repo has README.md and
    // docs/documentation-audiences.json (`{"surfaces": []}`) at the base, an untracked scripts/x.sh, and a
    // request with risk_triggers [] and all ten small_low_risk_conditions true. Output: profile
    // small-low-risk, classification_status resolved, exactly the two reasons asserted below; exit 0.
    #[test]
    fn attested_single_code_change_is_small_low_risk() {
        let collection = clean_collection(vec![file("scripts/x.sh")]);
        let decision = reassess(
            &collection,
            &request(Some(Vec::new()), Some(all_conditions(true))),
        );
        assert_eq!(decision.profile, Profile::SmallLowRisk);
        assert!(decision.resolved);
        assert_eq!(
            decision.reasons,
            vec![
                "scripts/x.sh: code change",
                "all small-low-risk conditions attested for a single bounded change",
            ]
        );
    }

    // Python-derived: the same repo with no_data_migration false. Output: profile standard, reasons
    // ["scripts/x.sh: code change", "default standard; small-low-risk not met: no_data_migration"]; exit 0.
    #[test]
    fn unattested_condition_keeps_standard() {
        let mut conditions = all_conditions(true);
        conditions.insert("no_data_migration".to_string(), false);
        let collection = clean_collection(vec![file("scripts/x.sh")]);
        let decision = reassess(&collection, &request(Some(Vec::new()), Some(conditions)));
        assert_eq!(decision.profile, Profile::Standard);
        assert!(decision.resolved);
        assert_eq!(
            decision.reasons.last().map(String::as_str),
            Some("default standard; small-low-risk not met: no_data_migration")
        );
    }

    // Python-derived: the attested repo with risk_triggers ["breaking-public-contract"]. Output: profile
    // high-risk, resolved, reasons ["risk trigger: breaking-public-contract", "scripts/x.sh: code change"]; exit 0.
    #[test]
    fn risk_trigger_forces_high_risk_and_lists_before_path_reasons() {
        let collection = clean_collection(vec![file("scripts/x.sh")]);
        let decision = reassess(
            &collection,
            &request(
                Some(vec!["breaking-public-contract".to_string()]),
                Some(all_conditions(true)),
            ),
        );
        assert_eq!(decision.profile, Profile::HighRisk);
        assert!(decision.resolved);
        assert_eq!(decision.risk_triggers, vec!["breaking-public-contract"]);
        assert_eq!(
            decision.reasons,
            vec![
                "risk trigger: breaking-public-contract",
                "scripts/x.sh: code change",
            ]
        );
    }

    // Python-derived: the attested request with an untracked notes.xyz instead of scripts/x.sh. Output:
    // profile high-risk, classification_status unresolved, reasons ["unknown path: notes.xyz"]; exit 0.
    #[test]
    fn unknown_path_is_unresolved_high_risk() {
        let collection = clean_collection(vec![file("notes.xyz")]);
        let decision = reassess(
            &collection,
            &request(Some(Vec::new()), Some(all_conditions(true))),
        );
        assert_eq!(decision.profile, Profile::HighRisk);
        assert!(!decision.resolved);
        assert_eq!(decision.reasons, vec!["unknown path: notes.xyz"]);
    }

    // Python-derived: each pair is the output of `prose_verdict(old, new)` in the reference, run
    // offline with `python3 -I -B /tmp/dpes884-prose/gen_verdicts.py` (bin/fm-assignment-reassess.py
    // at d80bcd15). The byte literals are the same bytes the script printed.
    type ProseCase = (&'static str, Option<Vec<u8>>, Option<Vec<u8>>, ProseVerdict);

    #[test]
    fn prose_verdicts_match_the_reference() {
        let long_a = "a".repeat(10000);
        let long_b = "b".repeat(10000);
        let cases: Vec<ProseCase> = vec![
            ("both-absent", None, None, ProseVerdict::Clean),
            (
                "unchanged-line",
                Some(b"a\n".to_vec()),
                Some(b"a\n".to_vec()),
                ProseVerdict::Clean,
            ),
            (
                "trailing-whitespace",
                Some(b"text\n".to_vec()),
                Some(b"text  \n".to_vec()),
                ProseVerdict::Clean,
            ),
            (
                "fenced-line-changed",
                Some(b"```\ncode\n```\n".to_vec()),
                Some(b"```\ncode2\n```\n".to_vec()),
                ProseVerdict::Executable,
            ),
            (
                "fence-opened-by-edit",
                Some(b"x\n".to_vec()),
                Some(b"```\ny\n".to_vec()),
                ProseVerdict::Executable,
            ),
            (
                "change-after-closing-fence",
                Some(b"```\ncode\n```\nafter\n".to_vec()),
                Some(b"```\ncode\n```\nafter2\n".to_vec()),
                ProseVerdict::Clean,
            ),
            (
                "closing-fence-gains-text",
                Some(b"```\ncode\n```\n".to_vec()),
                Some(b"```\ncode\n```text\n".to_vec()),
                ProseVerdict::Executable,
            ),
            (
                "crlf-only",
                Some(b"a\nb\n".to_vec()),
                Some(b"a\r\nb\r\n".to_vec()),
                ProseVerdict::Clean,
            ),
            (
                "unicode-prose",
                Some("caf\u{e9}\n".as_bytes().to_vec()),
                Some("caf\u{e9} \u{2615}\n".as_bytes().to_vec()),
                ProseVerdict::Clean,
            ),
            (
                "invalid-utf8-new",
                Some(b"a\n".to_vec()),
                Some(b"\xff\n".to_vec()),
                ProseVerdict::Unverifiable,
            ),
            (
                "deleted-fenced-file",
                Some(b"```\nx\n```\n".to_vec()),
                None,
                ProseVerdict::Executable,
            ),
            (
                "tilde-inside-backtick-fence",
                Some(b"```\n~~~\nx\n```\n".to_vec()),
                Some(b"```\n~~~\ny\n```\n".to_vec()),
                ProseVerdict::Executable,
            ),
            (
                "longer-fence-closes",
                Some(b"```\nx\n````\nafter\n".to_vec()),
                Some(b"```\nx\n````\nafter2\n".to_vec()),
                ProseVerdict::Clean,
            ),
            (
                "form-feed-splits-fence-line",
                Some(b"```\x0cx\n```\n".to_vec()),
                Some(b"```\x0cy\n```\n".to_vec()),
                ProseVerdict::Executable,
            ),
            (
                "indented-fence",
                Some(b"   ```\nx\n   ```\n".to_vec()),
                Some(b"   ```\ny\n   ```\n".to_vec()),
                ProseVerdict::Executable,
            ),
            (
                "very-long-line",
                Some(format!("{long_a}\n").into_bytes()),
                Some(format!("{long_b}\n").into_bytes()),
                ProseVerdict::Clean,
            ),
            (
                "blank-lines-added",
                Some(b"a\n".to_vec()),
                Some(b"a\n\n\nb\n".to_vec()),
                ProseVerdict::Clean,
            ),
            (
                "unicode-line-separator",
                Some("a\u{2028}b\n".as_bytes().to_vec()),
                Some("a\u{2028}c\n".as_bytes().to_vec()),
                ProseVerdict::Clean,
            ),
        ];
        for (name, base, worktree, expected) in cases {
            assert_eq!(
                prose_verdict(base.as_deref(), worktree.as_deref()),
                expected,
                "{name}"
            );
        }
    }

    // Python-derived: each expectation is the output of `str.splitlines()` in Python 3.14.6, run as
    // `python3 -I -B -c` on the same literal.
    #[test]
    fn splitlines_breaks_where_python_does() {
        assert_eq!(py_splitlines(""), Vec::<&str>::new());
        assert_eq!(py_splitlines("\n"), vec![""]);
        assert_eq!(py_splitlines("a\r\nb\rc\nd"), vec!["a", "b", "c", "d"]);
        assert_eq!(
            py_splitlines("a\u{b}b\u{c}c\u{1c}d\u{1d}e\u{1e}f\u{85}g\u{2028}h\u{2029}i"),
            vec!["a", "b", "c", "d", "e", "f", "g", "h", "i"]
        );
        assert_eq!(py_splitlines("a\u{1f}b"), vec!["a\u{1f}b"]);
        assert_eq!(py_splitlines("a\n\n"), vec!["a", ""]);
    }

    // Python-derived: the expected list is `[hex(c) for c in range(0x110000) if chr(c).isspace()]`
    // from Python 3.14.6; the reference `\s` matches the same set (checked with `re.match`). Rust
    // `char::is_whitespace` lacks U+001C to U+001F, which `py_isspace` adds.
    #[test]
    fn py_isspace_matches_python_isspace_over_all_code_points() {
        let python: Vec<u32> = vec![
            0x9, 0xa, 0xb, 0xc, 0xd, 0x1c, 0x1d, 0x1e, 0x1f, 0x20, 0x85, 0xa0, 0x1680, 0x2000,
            0x2001, 0x2002, 0x2003, 0x2004, 0x2005, 0x2006, 0x2007, 0x2008, 0x2009, 0x200a, 0x2028,
            0x2029, 0x202f, 0x205f, 0x3000,
        ];
        let rust: Vec<u32> = (0..0x110000u32)
            .filter(|code| char::from_u32(*code).is_some_and(py_isspace))
            .collect();
        assert_eq!(rust, python);
    }

    // Python-derived: each expectation is `path_class(path, {path: "public-product"})` from the
    // reference bin/fm-assignment-reassess.py at d80bcd15, loaded with importlib.util and run as
    // `python3 -I -B -X utf8 -c <load and print>` on the same literal. The reference class follows
    // `os.path.splitext(path)[1].lower()`, so leading dots, a trailing dot and a case-folded
    // extension are all covered.
    #[test]
    fn path_class_follows_python_splitext() {
        let cases = [
            (".md", PathClass::Unknown),
            ("..md", PathClass::Unknown),
            ("...md", PathClass::Unknown),
            ("..MD", PathClass::Unknown),
            ("x/..md", PathClass::Unknown),
            ("..sh", PathClass::Unknown),
            ("..\u{e9}", PathClass::Unknown),
            ("md", PathClass::Unknown),
            ("README", PathClass::Unknown),
            ("Makefile", PathClass::Unknown),
            ("dir.md/file", PathClass::Unknown),
            ("a.", PathClass::Unknown),
            ("docs/guide.md.", PathClass::Unknown),
            (".a.b.", PathClass::Unknown),
            ("a.tar.GZ", PathClass::Unknown),
            ("x/.md", PathClass::Unknown),
            (".yaml", PathClass::Unknown),
            ("a.b.md", PathClass::Prose),
            (".hidden.md", PathClass::Prose),
            ("a..md", PathClass::Prose),
            ("UPPER.MD", PathClass::Prose),
            ("docs/\u{fc}ber.MD", PathClass::Prose),
            ("caf\u{e9}.md", PathClass::Prose),
            ("..r\u{e9}sum\u{e9}.md", PathClass::Prose),
            ("x.py", PathClass::Code),
            ("a/b.yml", PathClass::Code),
        ];
        for (path, expected) in cases {
            let audiences = BTreeMap::from([(path.to_string(), "public-product".to_string())]);
            assert_eq!(path_class(path, Some(&audiences)), expected, "{path}");
        }
    }

    // The blocked-request list is Python-derived: the same JSON through `reassess --request` gives
    // status blocked, exit 1, and these five reasons in this order. The `previous` assertion is not:
    // Python reports fourteen previous-record problems for a partial record, while Rust refuses it as
    // Unported (KNOWN_GAPS "previous-record").
    // Python-derived: `reassess --request` with comparison_base_commit = <40 hex>\n gives exit 0,
    // status recorded, profile high-risk, classification_status unresolved, and these three reasons;
    // `collect --base-commit <sha>\n` gives base_ok false and categories [tracked-working-tree, untracked].
    #[test]
    fn base_commit_with_trailing_newline_is_accepted_like_python() {
        let text = format!(
            r#"{{"task_id":"t-1","pinned_policy_revision":"r1","target_branch":"main",
            "comparison_base_commit":"{}\n","assignment_intent":"i","expected_surfaces":["a"],
            "risk_triggers":[]}}"#,
            "a".repeat(40)
        );
        let request = parse_request(&text).expect("python accepts the trailing newline");
        assert_eq!(
            request.comparison_base_commit,
            format!("{}\n", "a".repeat(40))
        );
        let collection = Collection {
            base_ok: false,
            complete: false,
            categories: vec!["tracked-working-tree", "untracked"],
            gaps: Vec::new(),
            identity: None,
            audiences: None,
            paths: Vec::new(),
        };
        let decision = reassess(&collection, &request);
        assert_eq!(decision.profile, Profile::HighRisk);
        assert!(!decision.resolved);
        assert_eq!(
            decision.reasons,
            vec![
                "no changed paths were collected",
                "comparison base is not resolvable in the worktree",
                "change collection incomplete, missing: committed, renamed, deleted",
            ]
        );
    }

    #[test]
    fn request_validation_reports_every_problem_including_previous() {
        let blocked = parse_request(
            r#"{"task_id":"a/b","pinned_policy_revision":"","target_branch":"main",
                "comparison_base_commit":"XYZ","assignment_intent":"x",
                "expected_surfaces":["a","a"],"extra":1}"#,
        );
        assert_eq!(
            blocked,
            Err(RequestError::Blocked(vec![
                "request has unknown keys: extra".to_string(),
                "task_id must be a non-empty string without '/'".to_string(),
                "pinned_policy_revision must be a non-empty string".to_string(),
                "comparison_base_commit must be a 40-hex commit".to_string(),
                "expected_surfaces must be a unique list of non-empty paths".to_string(),
            ]))
        );
        assert!(matches!(
            parse_request("{not json"),
            Err(RequestError::Unreadable(_))
        ));
        // Python-derived: the same JSON through `reassess --request` (d80bcd15) gives status blocked,
        // exit 1, and these fourteen previous-record reasons in this order.
        let with_previous = r#"{"task_id":"t","pinned_policy_revision":"r","target_branch":"main",
            "comparison_base_commit":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "assignment_intent":"i","expected_surfaces":["a"],"previous":{"contract_version":1}}"#;
        assert_eq!(
            parse_request(with_previous),
            Err(RequestError::Blocked(vec![
                "previous is missing keys: assignment_intent, change_coverage, classification_status, comparison_base_commit, decision_id, decision_stage, evidence_obligations, expected_surfaces, pinned_policy_revision, profile, reasons, required_stages, risk_triggers, target_branch, task_id".to_string(),
                "previous decision_id must be <task_id>/<sequence> for the request task_id".to_string(),
                "previous task_id differs from the request task_id".to_string(),
                "previous assignment_intent differs from the request".to_string(),
                "previous expected_surfaces differ from the request".to_string(),
                "previous target_branch differs from the host-pinned target_branch".to_string(),
                "previous pinned_policy_revision must be a non-empty string".to_string(),
                "previous comparison_base_commit must be a 40-hex commit".to_string(),
                "previous profile is not a known profile id".to_string(),
                "previous decision_stage must be provisional or final".to_string(),
                "previous classification_status must be resolved or unresolved".to_string(),
                "previous risk_triggers must be a unique list of known trigger names".to_string(),
                "previous change_coverage must be {complete, categories} with known categories".to_string(),
                "previous reasons must be a non-empty list of strings".to_string(),
            ]))
        );
    }
}
