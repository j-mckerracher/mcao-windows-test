//! Rust core of the assignment-profile decision validator. It ports bin/fm-assignment-profile.py:
//! `validate` checks a record against the decision schema and the approved revision registry,
//! and `render` and `meta` read a validated record. docs/assignment-profile-policy.md owns the
//! contract. The CLI does not call this module yet.

mod json;
mod schema;

use json::{CANONICAL, Json, ParseError, dump};
use sha2::{Digest, Sha256};
use std::fmt;
use std::fs;
use std::io;
use std::path::Path;

const SCHEMA_REL: &str = "docs/assignment-profile-decision.v1.schema.json";
const REGISTRY_REL: &str = "docs/assignment-profile-revisions.json";
const REGISTRY_SCHEMA: &str = "assignment-profile-revisions.v1";

/// Host pins a record is checked against. A new launch sets `require_current`; a relaunch does not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pins {
    pub task: Option<String>,
    pub target_branch: Option<String>,
    pub base_commit: Option<String>,
    pub require_current: bool,
}

/// The exit class of a failed check. `Refusal` is exit 1, `Usage` is exit 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitClass {
    Refusal,
    Usage,
}

/// A failed check: the exit class and the reason, which is the text Python prints on stderr.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    class: ExitClass,
    reason: String,
}

impl Refusal {
    fn refuse(reason: impl Into<String>) -> Self {
        Self {
            class: ExitClass::Refusal,
            reason: reason.into(),
        }
    }

    fn usage(reason: impl Into<String>) -> Self {
        Self {
            class: ExitClass::Usage,
            reason: reason.into(),
        }
    }

    pub fn class(&self) -> ExitClass {
        self.class
    }

    pub fn reason(&self) -> &str {
        &self.reason
    }

    pub fn exit_code(&self) -> i32 {
        match self.class {
            ExitClass::Refusal => 1,
            ExitClass::Usage => 2,
        }
    }

    /// The stderr line, without its newline, exactly as bin/fm-assignment-profile.py prints it.
    pub fn stderr_line(&self) -> String {
        match self.class {
            ExitClass::Refusal => format!("error: assignment profile refused: {}", self.reason),
            ExitClass::Usage => format!(
                "error: assignment profile check could not run: {}",
                self.reason
            ),
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.stderr_line())
    }
}

impl std::error::Error for Refusal {}

/// An accepted record: its canonical text (the stdout of `validate`) and its parsed value.
#[derive(Debug, Clone, PartialEq)]
#[must_use]
pub struct CanonicalJson {
    text: String,
    value: Json,
}

impl CanonicalJson {
    /// The canonical record JSON, with a trailing newline.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    fn text_field(&self, key: &str) -> &str {
        self.value
            .get(key)
            .and_then(Json::as_str)
            .unwrap_or_default()
    }

    fn text_list(&self, key: &str) -> Vec<&str> {
        self.value
            .get(key)
            .and_then(Json::as_array)
            .map(|items| items.iter().filter_map(Json::as_str).collect())
            .unwrap_or_default()
    }

    fn flag(&self, object: &str, key: &str) -> bool {
        matches!(
            self.value.get(object).and_then(|o| o.get(key)),
            Some(Json::Bool(true))
        )
    }
}

/// Validates `record` against the schema and registry under `code_root`, then applies `pins`.
pub fn validate(code_root: &Path, record: &[u8], pins: &Pins) -> Result<CanonicalJson, Refusal> {
    let schema_path = code_root.join(SCHEMA_REL);
    let schema_text = read_text(&schema_path).map_err(|err| {
        Refusal::usage(format!(
            "the decision schema {SCHEMA_REL} is unreadable: {}",
            strerror(&err)
        ))
    })?;
    let schema = parse_strict(&schema_text, "the decision schema")?;
    let record_text = std::str::from_utf8(record)
        .map_err(|_| Refusal::refuse("the assignment profile record is not valid UTF-8"))?;
    let value = parse_strict(record_text, "the assignment profile record")?;

    let mut errors = schema::Errors::new();
    schema::evaluate(&schema, &value, &schema, "", &mut errors)?;
    if let Some((where_, what)) = errors.first() {
        let where_ = if where_.is_empty() { "(root)" } else { where_ };
        return Err(Refusal::refuse(format!("record field '{where_}' {what}")));
    }

    let revision = value
        .get("pinned_policy_revision")
        .and_then(Json::as_str)
        .unwrap_or_default();
    check_revision(code_root, revision, pins.require_current)?;

    let record = CanonicalJson {
        text: format!("{}\n", dump(&value, CANONICAL)),
        value,
    };
    check_pins(&record, pins)?;
    Ok(record)
}

/// The obligations as Markdown bullet lines, exactly as `render` prints them.
pub fn render(record: &CanonicalJson) -> String {
    let mut lines = vec![
        format!(
            "- Profile `{}`, decision stage `{}`, classification `{}`, decision `{}`.",
            record.text_field("profile"),
            record.text_field("decision_stage"),
            record.text_field("classification_status"),
            record.text_field("decision_id"),
        ),
        format!(
            "- Policy revision `{}`; target branch `{}`; comparison base `{}`.",
            record.text_field("pinned_policy_revision"),
            record.text_field("target_branch"),
            record.text_field("comparison_base_commit"),
        ),
        format!(
            "- Required stages, in order: {}.",
            record.text_list("required_stages").join(", ")
        ),
    ];
    if record.flag("evidence_obligations", "tests_required") {
        lines.push(
            "- Tests: required by this profile; run the tests that cover your change.".to_string(),
        );
    } else {
        lines.push(
            "- Tests: this profile requires zero tests executed; run only the applicable non-test checks, \
             and do not run or add tests to satisfy it."
                .to_string(),
        );
    }
    if record.flag("evidence_obligations", "independent_change_review_required") {
        lines.push(
            "- Independent change review: required; the selected delivery path supplies it, with no extra reviewer."
                .to_string(),
        );
    } else {
        lines.push("- Independent change review: not required by this profile.".to_string());
    }
    let triggers = record.text_list("risk_triggers");
    if !triggers.is_empty() {
        lines.push(format!("- Risk triggers: {}.", triggers.join(", ")));
    }
    lines.push(format!(
        "- Reasons: {}.",
        record.text_list("reasons").join("; ")
    ));
    if let Some(identity) = record
        .value
        .get("evaluated_change_identity")
        .and_then(Json::as_str)
    {
        lines.push(format!("- Evaluated change identity: `{identity}`."));
    }
    format!("{}\n", lines.join("\n"))
}

/// The task-record keys a launch persists beside the canonical decision file.
pub fn meta(record: &CanonicalJson) -> String {
    [
        ("profile_decision_id", "decision_id"),
        ("profile_policy_revision", "pinned_policy_revision"),
        ("profile_name", "profile"),
        ("profile_decision_stage", "decision_stage"),
        ("profile_classification_status", "classification_status"),
        ("profile_target_branch", "target_branch"),
        ("profile_comparison_base_commit", "comparison_base_commit"),
    ]
    .iter()
    .map(|(key, field)| format!("{key}={}\n", record.text_field(field)))
    .collect()
}

fn check_pins(record: &CanonicalJson, pins: &Pins) -> Result<(), Refusal> {
    if let Some(task) = &pins.task {
        let record_task = record.text_field("task_id");
        if record_task != task {
            return Err(Refusal::refuse(format!(
                "task_id is '{record_task}' but this spawn launches task '{task}'; contradictory decision"
            )));
        }
        let decision_id = record.text_field("decision_id");
        if decision_id.split('/').next() != Some(task.as_str()) {
            return Err(Refusal::refuse(format!(
                "decision_id '{decision_id}' does not begin with the task id '{task}'"
            )));
        }
    }
    if let Some(branch) = &pins.target_branch {
        let record_branch = record.text_field("target_branch");
        if record_branch != branch {
            return Err(Refusal::refuse(format!(
                "target_branch is '{record_branch}' but the host pins '{branch}'; contradictory decision"
            )));
        }
    }
    if let Some(base) = &pins.base_commit {
        let record_base = record.text_field("comparison_base_commit");
        if record_base != base {
            return Err(Refusal::refuse(format!(
                "comparison_base_commit is {record_base} but the host pins {base}; \
                 the decision was written against another base"
            )));
        }
    }
    Ok(())
}

fn check_revision(
    code_root: &Path,
    revision_id: &str,
    require_current: bool,
) -> Result<(), Refusal> {
    let registry_path = code_root.join(REGISTRY_REL);
    let registry_text = read_text(&registry_path).map_err(|err| {
        Refusal::refuse(format!(
            "the revision registry {REGISTRY_REL} is unreadable: {}",
            strerror(&err)
        ))
    })?;
    let registry = parse_strict(&registry_text, "the revision registry")?;
    if registry.get("schema").and_then(Json::as_str) != Some(REGISTRY_SCHEMA) {
        return Err(Refusal::refuse(format!(
            "the revision registry does not declare schema {REGISTRY_SCHEMA}"
        )));
    }
    let entries = registry
        .get("revisions")
        .and_then(Json::as_array)
        .ok_or_else(|| Refusal::refuse("the revision registry has no revisions list"))?;
    let current = registry.get("current");
    if require_current && current.and_then(Json::as_str) != Some(revision_id) {
        return Err(Refusal::refuse(format!(
            "pinned_policy_revision '{revision_id}' is not the host-selected current revision '{}'; \
             new launches must pin the current revision",
            current.map_or_else(|| "None".to_string(), py_str)
        )));
    }

    let matches: Vec<&Json> = entries
        .iter()
        .filter(|entry| entry.get("id").and_then(Json::as_str) == Some(revision_id))
        .collect();
    let entry = match matches.as_slice() {
        [] => {
            return Err(Refusal::refuse(format!(
                "pinned_policy_revision '{revision_id}' is not in the approved revision registry; \
                 routing is blocked and no replacement profile is created"
            )));
        }
        [only] => *only,
        _ => {
            return Err(Refusal::refuse(format!(
                "the revision registry lists '{revision_id}' more than once"
            )));
        }
    };
    if !is_contract_version(entry.get("contract_version")) {
        return Err(Refusal::refuse(format!(
            "revision '{revision_id}' is for an unsupported contract version"
        )));
    }
    if entry.get("status").and_then(Json::as_str) != Some("approved") {
        return Err(Refusal::refuse(format!(
            "revision '{revision_id}' is not approved in the registry"
        )));
    }
    let files = entry
        .get("files")
        .and_then(Json::as_object)
        .filter(|files| !files.is_empty())
        .ok_or_else(|| {
            Refusal::refuse(format!("revision '{revision_id}' pins no policy material"))
        })?;
    let mut pinned: Vec<&(String, Json)> = files.iter().collect();
    pinned.sort_by(|a, b| a.0.cmp(&b.0));
    for (rel, digest) in pinned {
        let material = code_root.join(rel);
        let actual = sha256_file(&material)?;
        if digest.as_str() != Some(actual.as_str()) {
            return Err(Refusal::refuse(format!(
                "policy material {rel} differs from the approved content of revision '{revision_id}'; \
                 a changed revision needs its own registry approval"
            )));
        }
    }
    Ok(())
}

/// Python's `value != 1` check: a boolean `true` or a float `1.0` also equals 1.
fn is_contract_version(value: Option<&Json>) -> bool {
    match value {
        Some(Json::Int(digits)) => digits == "1",
        Some(Json::Float(f)) => *f == 1.0,
        Some(Json::Bool(flag)) => *flag,
        _ => false,
    }
}

fn sha256_file(path: &Path) -> Result<String, Refusal> {
    let bytes = fs::read(path).map_err(|err| {
        Refusal::refuse(format!(
            "policy material {} is unreadable: {}",
            path.display(),
            strerror(&err)
        ))
    })?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn read_text(path: &Path) -> io::Result<String> {
    fs::read_to_string(path)
}

fn parse_strict(text: &str, what: &str) -> Result<Json, Refusal> {
    json::parse(text).map_err(|err| match err {
        ParseError::Syntax(msg) => Refusal::refuse(format!("{what} is not valid JSON: {msg}")),
        ParseError::Refuse(reason) => Refusal::refuse(reason),
    })
}

/// The C strerror text Python reports, which Rust's io::Error adds an `(os error N)` suffix to.
fn strerror(err: &io::Error) -> String {
    let text = err.to_string();
    match text.find(" (os error") {
        Some(index) => text[..index].to_string(),
        None => text,
    }
}

/// Python's `str()` of a registry value, for the message that names the current revision.
fn py_str(value: &Json) -> String {
    match value {
        Json::Null => "None".to_string(),
        Json::Bool(true) => "True".to_string(),
        Json::Bool(false) => "False".to_string(),
        Json::Int(digits) => digits.clone(),
        Json::Float(f) => json::py_float(*f),
        Json::Str(s) => s.clone(),
        container => dump(container, json::COMPACT),
    }
}
