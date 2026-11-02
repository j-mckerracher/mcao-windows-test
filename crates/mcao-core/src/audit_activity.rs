//! Audit envelope to SIEM activity event projection.
//!
//! Ports the pure projection logic from `bin/fm-audit-activity-map.py`: it builds the SIEM
//! `audit.activity` event from one audit envelope v1 record, wraps that event in a Cloud Logging
//! LogEntry, and lists the JSON Pointers of leaves that differ.
//!
//! # Scope and authoritative sources
//!
//! - Input record: `mcao.audit-envelope.v1`, owned by `docs/audit-envelope.md` and
//!   `docs/audit-envelope.v1.schema.json` in this repository.
//! - Event and field mapping: section 3.1 of `docs/siem-event-format.md` in
//!   `mc-innersource/agentic-workflow-data-api` (DPES-788). Only the members listed there are
//!   copied; `schema` and `user.timezone` are not.
//! - Cloud Logging target: section 4 of the same document. The log is
//!   `projects/dpe-platform-d/logs/mcao-audit` ([`LOG_NAME`]), the monitored resource is
//!   `cloud_run_revision` ([`RESOURCE_TYPE`]), `insertId` is the event id, and `severity` is
//!   `DEFAULT`. The target is planned: nothing is deployed and no writer exists yet.
//! - CLI contract: `bin/fm-audit-activity-map.py` is the reference. `mcao audit activity-map`
//!   prints `mapped` or `refused` and exits 0, 1, or 2 as it does.
//!
//! [`map_record`] is the whole pipeline: it validates the record, projects it, runs the DPES-737
//! boundary ([`crate::audit_sanitize`]) over the projected event, and wraps the result. Nothing
//! else in this module serializes, spools, logs, or ships an event, so only sanitized output
//! leaves it. [`project`] and [`log_entry`] are its steps and do not sanitize on their own.
//!
//! [`project`] enforces only the `schema` gate, the presence of each copied member, the object
//! shape of `agent`, `actor`, and `user`, and the string type of `event_id`. It does not check
//! value patterns, enums, `day_key`, identity, or the hash chain; [`map_record`] runs
//! [`crate::audit_envelope::validate`] first for those.
//!
//! # Known divergences from the reference
//!
//! The parity run covers 535 adversarial cases and compares exit status and stdout bytes. Every
//! case matches except these, and each is pinned by a test in `tests/audit_activity_map.rs`:
//!
//! - Nesting past serde_json's 128-level limit is invalid JSON here (exit 2). The reference parses
//!   it and refuses the record at schema (exit 1). Pinned by
//!   `a_record_nested_past_the_json_depth_limit_is_an_input_error`.
//! - A lone surrogate escape is invalid JSON here (exit 2). The reference accepts it and refuses at
//!   the member that carries it (exit 1). Pinned by `a_lone_surrogate_escape_is_an_input_error`.
//! - When `seq` and `prev_hash` are both invalid, the shared envelope validator reports `/seq`, and
//!   the reference reports `/prev_hash`. Both refuse with `schema_invalid` and exit 1. The validator
//!   is the one `audit validate` uses, so its order is not changed here. Pinned by
//!   `an_out_of_range_seq_is_refused_at_seq_where_the_reference_names_prev_hash`.
//! - `-h` and `--help` print clap's help text, not the reference's one-line usage. Both exit 0.

use serde_json::{Map, Value, json};

use crate::audit_envelope::{self, Definition};
use crate::audit_path::pointer_token;
use crate::audit_sanitize::{EventRefused, sanitize_event};

/// Errors that can occur during projection of an audit envelope to a SIEM activity event.
///
/// Each variant holds the JSON Pointer of the member at fault, never the member's value.
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProjectError {
    /// A required member is absent from the record.
    Missing(String),
    /// A member is present but has a JSON type the projection cannot use: an object for the
    /// record, `agent`, `actor`, and `user`, or a string for `event_id`.
    WrongType(String),
    /// The record's `schema` member is absent or is not `mcao.audit-envelope.v1`.
    SchemaInvalid(String),
}

impl std::fmt::Display for ProjectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing(pointer) => write!(f, "missing member {pointer}"),
            Self::WrongType(pointer) => write!(f, "wrong type at {pointer:?}"),
            Self::SchemaInvalid(pointer) => write!(f, "schema_invalid at {pointer:?}"),
        }
    }
}

impl std::error::Error for ProjectError {}

/// The schema identifier for SIEM events.
pub const EVENT_SCHEMA: &str = "mcao.siem.event/v1";

/// The event type identifier for audit activity events.
pub const EVENT_TYPE: &str = "audit.activity";

/// The source route identifier for audit envelope v1 records.
pub const SOURCE_ROUTE: &str = "mcao.audit-envelope.v1";

/// The Cloud Logging log name for mcao audit events.
pub const LOG_NAME: &str = "projects/dpe-platform-d/logs/mcao-audit";

/// The monitored resource type for Cloud Run revisions.
pub const RESOURCE_TYPE: &str = "cloud_run_revision";

/// Returns the JSON Pointer of `key` under `base`, escaped per RFC 6901.
fn child_pointer(base: &str, key: &str) -> String {
    format!("{base}/{}", pointer_token(key))
}

/// A JSON object together with its JSON Pointer, so that each error names the member at fault.
struct Fields<'a> {
    object: &'a Map<String, Value>,
    pointer: String,
}

impl<'a> Fields<'a> {
    /// Wraps `value`, which lives at `pointer`, or reports `pointer` when it is not an object.
    fn new(value: &'a Value, pointer: &str) -> Result<Self, ProjectError> {
        match value.as_object() {
            Some(object) => Ok(Self {
                object,
                pointer: pointer.to_string(),
            }),
            None => Err(ProjectError::WrongType(pointer.to_string())),
        }
    }

    /// Returns member `key`, or reports its pointer when it is absent.
    fn get(&self, key: &str) -> Result<&'a Value, ProjectError> {
        self.object
            .get(key)
            .ok_or_else(|| ProjectError::Missing(child_pointer(&self.pointer, key)))
    }

    /// Returns member `key` as a string, or reports its pointer when it is absent or not a string.
    fn text(&self, key: &str) -> Result<&'a str, ProjectError> {
        self.get(key)?
            .as_str()
            .ok_or_else(|| ProjectError::WrongType(child_pointer(&self.pointer, key)))
    }

    /// Returns member `key` as an object, or reports its pointer when it is absent or not one.
    fn nested(&self, key: &str) -> Result<Self, ProjectError> {
        Self::new(self.get(key)?, &child_pointer(&self.pointer, key))
    }
}

/// Projects a validated audit envelope record into a SIEM `audit.activity` event.
///
/// The record must already pass [`crate::audit_envelope::validate`]; see the module preconditions.
/// The `schema` gate refuses a record whose `schema` is absent or is not `mcao.audit-envelope.v1`.
/// Each copied member must be present, `agent`, `actor`, and `user` must be objects, and
/// `event_id` must be a string. An absent `task_id` projects as `null`, as the SIEM contract
/// requires for an envelope that omits it. The returned event is not sanitized.
///
/// # Errors
///
/// - [`ProjectError::SchemaInvalid`] at `/schema` when the schema gate fails.
/// - [`ProjectError::Missing`] with the pointer of the first absent copied member.
/// - [`ProjectError::WrongType`] with the pointer of the first member of the wrong type.
pub fn project(record: &Value) -> Result<Value, ProjectError> {
    let root = Fields::new(record, "")?;
    if root.object.get("schema").and_then(Value::as_str) != Some(SOURCE_ROUTE) {
        return Err(ProjectError::SchemaInvalid("/schema".to_string()));
    }

    let event_id = root.text("event_id")?;
    let agent = root.nested("agent")?;
    let actor = root.nested("actor")?;
    let user = root.nested("user")?;

    Ok(json!({
        "schema": EVENT_SCHEMA,
        "event_type": EVENT_TYPE,
        "event_id": format!("{EVENT_TYPE}:{event_id}"),
        "occurred_at_utc": root.get("ts")?,
        "source_route": SOURCE_ROUTE,
        "lineage": {
            "event_id": event_id,
            "seq": root.get("seq")?,
            "prev_hash": root.get("prev_hash")?,
        },
        "data": {
            "event": root.get("event")?,
            "category": root.get("category")?,
            "outcome": root.get("outcome")?,
            "platform": root.get("platform")?,
            "home_id": root.get("home_id")?,
            "day_key": root.get("day_key")?,
            "session_id": root.get("session_id")?,
            "task_id": root.object.get("task_id").cloned().unwrap_or(Value::Null),
            "agent": {
                "role": agent.get("role")?,
                "harness": agent.get("harness")?,
                "model": agent.get("model")?,
            },
            "actor": {
                "type": actor.get("type")?,
                "id": actor.get("id")?,
            },
            "repo": root.get("repo")?,
            "pr": root.get("pr")?,
            "commit": root.get("commit")?,
            "user": {
                "id": user.get("id")?,
                "identity_source": user.get("identity_source")?,
            },
        },
    }))
}

/// Wraps a SIEM `audit.activity` event in a Cloud Logging LogEntry object.
///
/// `event` must be the output of [`project`] and must already be sanitized (see the module
/// preconditions). The entry carries the event unchanged as `jsonPayload`.
///
/// # Errors
///
/// - [`ProjectError::WrongType`] at the empty pointer when `event` is not an object.
/// - [`ProjectError::Missing`] for `/occurred_at_utc` or `/event_id` when either is absent.
pub fn log_entry(event: &Value) -> Result<Value, ProjectError> {
    let fields = Fields::new(event, "")?;
    let timestamp = fields.get("occurred_at_utc")?;
    let insert_id = fields.get("event_id")?;

    Ok(json!({
        "logName": LOG_NAME,
        "resource": {
            "type": RESOURCE_TYPE,
        },
        "jsonPayload": event.clone(),
        "timestamp": timestamp.clone(),
        "insertId": insert_id.clone(),
        "severity": "DEFAULT",
    }))
}

/// A projected and sanitized record, ready to print.
#[derive(Debug, Clone, PartialEq)]
pub struct Mapped {
    /// The JSON Pointers of the members the boundary redacted or cut, in projection order.
    pub changed: Vec<String>,
    /// The sanitized `audit.activity` event.
    pub event: Value,
    /// The sanitized event wrapped in its Cloud Logging LogEntry.
    pub log_entry: Value,
}

/// A record that cannot be mapped. `reason` is a fixed word and `paths` holds JSON Pointers only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// The fixed reason: an envelope rejection reason, `sanitization_dropped`, or `schema_invalid`.
    pub reason: &'static str,
    /// The JSON Pointers of the members at fault; never a value.
    pub paths: Vec<String>,
}

/// Validates, projects, and sanitizes one envelope record.
///
/// `utc_offset_secs` supplies zone offsets as [`audit_envelope::validate`] takes them. A record
/// the validator rejects, or a projected member the boundary drops, refuses the whole record, and
/// no partial event is returned.
///
/// # Errors
///
/// Returns [`Refusal`] for an invalid envelope, an unprojectable record, or a dropped member.
pub fn map_record(
    record: &Value,
    home_key: &[u8],
    utc_offset_secs: &dyn Fn(&str, i64) -> Option<i32>,
) -> Result<Mapped, Refusal> {
    if let Err(rejection) = audit_envelope::validate(record, Definition::Envelope, utc_offset_secs)
    {
        return Err(Refusal {
            reason: rejection.reason.as_str(),
            paths: rejection.paths,
        });
    }
    let event = project(record).map_err(projection_refusal)?;
    let (sanitized, dropped) =
        sanitize_event(&event, home_key).map_err(|EventRefused| Refusal {
            reason: "event_invalid",
            paths: vec![String::new()],
        })?;
    if !dropped.is_empty() {
        return Err(Refusal {
            reason: "sanitization_dropped",
            paths: dropped,
        });
    }
    let changed = differing_pointers(&event, &sanitized);
    let log_entry = log_entry(&sanitized).map_err(projection_refusal)?;
    Ok(Mapped {
        changed,
        event: sanitized,
        log_entry,
    })
}

/// Reports a projection failure as a `schema_invalid` refusal at the pointer it names.
fn projection_refusal(error: ProjectError) -> Refusal {
    let pointer = match error {
        ProjectError::Missing(pointer)
        | ProjectError::WrongType(pointer)
        | ProjectError::SchemaInvalid(pointer) => pointer,
    };
    Refusal {
        reason: "schema_invalid",
        paths: vec![pointer],
    }
}

/// The member order of the projected event, as the reference's dict literals insert them.
const EVENT_MEMBERS: [&str; 7] = [
    "schema",
    "event_type",
    "event_id",
    "occurred_at_utc",
    "source_route",
    "lineage",
    "data",
];
const LINEAGE_MEMBERS: [&str; 3] = ["event_id", "seq", "prev_hash"];
const DATA_MEMBERS: [&str; 14] = [
    "event",
    "category",
    "outcome",
    "platform",
    "home_id",
    "day_key",
    "session_id",
    "task_id",
    "agent",
    "actor",
    "repo",
    "pr",
    "commit",
    "user",
];
const AGENT_MEMBERS: [&str; 3] = ["role", "harness", "model"];
const ACTOR_MEMBERS: [&str; 2] = ["type", "id"];
const USER_MEMBERS: [&str; 2] = ["id", "identity_source"];

/// Returns the projection's member order for the object at `parent`, or nothing for other objects.
fn member_order(parent: &[String]) -> &'static [&'static str] {
    let segments: Vec<&str> = parent.iter().map(String::as_str).collect();
    match segments[..] {
        [] => &EVENT_MEMBERS,
        ["lineage"] => &LINEAGE_MEMBERS,
        ["data"] => &DATA_MEMBERS,
        ["data", "agent"] => &AGENT_MEMBERS,
        ["data", "actor"] => &ACTOR_MEMBERS,
        ["data", "user"] => &USER_MEMBERS,
        _ => &[],
    }
}

/// Sort key that orders pointers as the reference's insertion-order walk visits them.
///
/// Each segment ranks by its position in the projection's member order; a member the projection
/// does not name ranks last and ties break by key.
fn projection_order_key(pointer: &str) -> Vec<(usize, String)> {
    let segments: Vec<String> = pointer
        .split('/')
        .skip(1)
        .map(|segment| segment.replace("~1", "/").replace("~0", "~"))
        .collect();
    (0..segments.len())
        .map(|depth| {
            let rank = member_order(&segments[..depth])
                .iter()
                .position(|name| *name == segments[depth])
                .unwrap_or(usize::MAX);
            (rank, segments[depth].clone())
        })
        .collect()
}

/// Compares two JSON values and returns the JSON Pointers of differing leaves.
///
/// If both `before` and `after` are objects, keys from `before` are visited. For each key present
/// in `after`, this recurses with the key appended to the pointer. For each key absent from
/// `after`, that pointer is pushed. Keys present only in `after` are ignored.
///
/// Otherwise, if `before == after`, an empty list is returned. If they differ, a list holding the
/// current base pointer is returned. Numbers and booleans compare by `serde_json::Value` equality,
/// so `1` and `1.0` differ and `true` and `1` differ.
///
/// The top-level base is the empty string, so differing top-level scalars yield `[""]`.
///
/// The result is ordered as the reference reports it. The reference walks `before` in insertion
/// order, which for a projected event is the projection's member order. This crate does not
/// enable `serde_json`'s `preserve_order` feature, so the walk cannot follow insertion order;
/// [`projection_order_key`] reproduces it from the projection's member lists instead.
#[must_use]
pub fn differing_pointers(before: &Value, after: &Value) -> Vec<String> {
    let mut changed = diff_pointers(before, after, "");
    changed.sort_by_key(|pointer| projection_order_key(pointer));
    changed
}

fn diff_pointers(before: &Value, after: &Value, base: &str) -> Vec<String> {
    if let (Value::Object(before_map), Value::Object(after_map)) = (before, after) {
        let mut changed = Vec::new();
        for (key, value) in before_map {
            let pointer = child_pointer(base, key);
            if let Some(after_value) = after_map.get(key) {
                changed.extend(diff_pointers(value, after_value, &pointer));
            } else {
                changed.push(pointer);
            }
        }
        changed
    } else if before == after {
        Vec::new()
    } else {
        vec![base.to_string()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_differing_pointers_escapes_keys() {
        // Keys are escaped with the canonical pointer_token: ~ becomes ~0 before / becomes ~1.
        let before = json!({"x~y": 1, "~/": 1});
        let after = json!({"x~y": 2, "~/": 2});
        assert_eq!(differing_pointers(&before, &after), vec!["/x~0y", "/~0~1"]);
    }

    fn sample_envelope() -> Value {
        json!({
            "schema": "mcao.audit-envelope.v1",
            "event_id": "018f3b7e-9c1a-7d2e-8f3a-4b5c6d7e8f90",
            "seq": 0,
            "ts": "2026-10-16T02:00:00.000Z",
            "prev_hash": null,
            "event": "task.dispatched",
            "category": "lifecycle",
            "outcome": "success",
            "platform": "mcao",
            "home_id": "5f1c2b3a-4d5e-4f60-8a71-92b3c4d5e6f7",
            "day_key": "2026-10-15",
            "session_id": "sess-1",
            "task_id": "fix-login",
            "agent": {
                "role": "firstmate",
                "harness": "claude",
                "model": null
            },
            "actor": {
                "type": "user",
                "id": "hmac-sha256:fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210"
            },
            "repo": "mc-innersource/motif-hve-multi-agent-orchestrator",
            "pr": "https://github.com/mc-innersource/motif-hve-multi-agent-orchestrator/pull/316",
            "commit": "36347693b43ae0acaa11e22f6c71a01f63663847",
            "user": {
                "id": "hmac-sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "identity_source": "github_login",
                "timezone": "America/Chicago"
            }
        })
    }

    #[test]
    fn test_project_full_sample_envelope() {
        let envelope = sample_envelope();
        let projected = project(&envelope).expect("projection should succeed");

        let expected = json!({
            "schema": "mcao.siem.event/v1",
            "event_type": "audit.activity",
            "event_id": "audit.activity:018f3b7e-9c1a-7d2e-8f3a-4b5c6d7e8f90",
            "occurred_at_utc": "2026-10-16T02:00:00.000Z",
            "source_route": "mcao.audit-envelope.v1",
            "lineage": {
                "event_id": "018f3b7e-9c1a-7d2e-8f3a-4b5c6d7e8f90",
                "seq": 0,
                "prev_hash": null,
            },
            "data": {
                "event": "task.dispatched",
                "category": "lifecycle",
                "outcome": "success",
                "platform": "mcao",
                "home_id": "5f1c2b3a-4d5e-4f60-8a71-92b3c4d5e6f7",
                "day_key": "2026-10-15",
                "session_id": "sess-1",
                "task_id": "fix-login",
                "agent": {
                    "role": "firstmate",
                    "harness": "claude",
                    "model": null,
                },
                "actor": {
                    "type": "user",
                    "id": "hmac-sha256:fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210",
                },
                "repo": "mc-innersource/motif-hve-multi-agent-orchestrator",
                "pr": "https://github.com/mc-innersource/motif-hve-multi-agent-orchestrator/pull/316",
                "commit": "36347693b43ae0acaa11e22f6c71a01f63663847",
                "user": {
                    "id": "hmac-sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                    "identity_source": "github_login",
                },
            },
        });

        assert_eq!(projected, expected);
        // Verify event_id prefix
        assert_eq!(
            projected["event_id"],
            "audit.activity:018f3b7e-9c1a-7d2e-8f3a-4b5c6d7e8f90"
        );
        assert_eq!(
            projected["lineage"]["event_id"],
            "018f3b7e-9c1a-7d2e-8f3a-4b5c6d7e8f90"
        );
        // Verify null agent.model copied
        assert_eq!(projected["data"]["agent"]["model"], Value::Null);
        // Verify timezone from user is not copied
        assert!(projected["data"]["user"].get("timezone").is_none());
    }

    /// Second sample: non-null `agent.model`, `actor.type` "agent", `identity_source`
    /// "git_email", non-zero `seq`, non-null `prev_hash`, and different `home_id`, `repo`, and
    /// `pr` values. `platform` is not varied because the envelope schema fixes it to `mcao`, so
    /// a hard-coded `platform` is equivalent for every valid record.
    ///
    /// The record passes `bin/fm-audit-envelope.py` verification. The expected event and
    /// LogEntry are golden output of `python3 -I bin/fm-audit-activity-map.py map`; regenerate
    /// them with that command rather than editing values by hand.
    fn second_sample_envelope() -> Value {
        json!({
            "schema": "mcao.audit-envelope.v1",
            "event_id": "018f3b7f-1d2e-7a3b-9c4d-5e6f7a8b9c0d",
            "seq": 7,
            "ts": "2026-10-16T03:15:30.123Z",
            "prev_hash": "sha256:abababababababababababababababababababababababababababababababab",
            "event": "task.completed",
            "category": "delivery",
            "outcome": "failure",
            "platform": "mcao",
            "home_id": "a0b1c2d3-e4f5-4a6b-8c7d-9e0f1a2b3c4d",
            "day_key": "2026-10-16",
            "session_id": "sess-2",
            "task_id": "ship-pr-316",
            "agent": {
                "role": "ship",
                "harness": "codex",
                "model": "gpt-5-codex"
            },
            "actor": {
                "type": "agent",
                "id": "hmac-sha256:1212121212121212121212121212121212121212121212121212121212121212"
            },
            "repo": "example-org/second-repo",
            "pr": "https://github.com/example-org/second-repo/pull/7",
            "commit": "5f7bd4a5bfcaf78edfb0aa84a6ed04815ef5b420",
            "user": {
                "id": "hmac-sha256:cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd",
                "identity_source": "git_email",
                "timezone": "Europe/Berlin"
            }
        })
    }

    #[test]
    fn test_project_second_sample_matches_python_golden() {
        let envelope = second_sample_envelope();
        let projected = project(&envelope).expect("projection should succeed");

        let golden_event = json!({
            "data": {
                "actor": {
                    "id": "hmac-sha256:1212121212121212121212121212121212121212121212121212121212121212",
                    "type": "agent"
                },
                "agent": {
                    "harness": "codex",
                    "model": "gpt-5-codex",
                    "role": "ship"
                },
                "category": "delivery",
                "commit": "5f7bd4a5bfcaf78edfb0aa84a6ed04815ef5b420",
                "day_key": "2026-10-16",
                "event": "task.completed",
                "home_id": "a0b1c2d3-e4f5-4a6b-8c7d-9e0f1a2b3c4d",
                "outcome": "failure",
                "platform": "mcao",
                "pr": "https://github.com/example-org/second-repo/pull/7",
                "repo": "example-org/second-repo",
                "session_id": "sess-2",
                "task_id": "ship-pr-316",
                "user": {
                    "id": "hmac-sha256:cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd",
                    "identity_source": "git_email"
                }
            },
            "event_id": "audit.activity:018f3b7f-1d2e-7a3b-9c4d-5e6f7a8b9c0d",
            "event_type": "audit.activity",
            "lineage": {
                "event_id": "018f3b7f-1d2e-7a3b-9c4d-5e6f7a8b9c0d",
                "prev_hash": "sha256:abababababababababababababababababababababababababababababababab",
                "seq": 7
            },
            "occurred_at_utc": "2026-10-16T03:15:30.123Z",
            "schema": "mcao.siem.event/v1",
            "source_route": "mcao.audit-envelope.v1"
        });
        assert_eq!(projected, golden_event);

        let entry = log_entry(&projected).expect("log entry should succeed");
        let golden_entry = json!({
            "insertId": "audit.activity:018f3b7f-1d2e-7a3b-9c4d-5e6f7a8b9c0d",
            "jsonPayload": {
                "data": {
                    "actor": {
                        "id": "hmac-sha256:1212121212121212121212121212121212121212121212121212121212121212",
                        "type": "agent"
                    },
                    "agent": {
                        "harness": "codex",
                        "model": "gpt-5-codex",
                        "role": "ship"
                    },
                    "category": "delivery",
                    "commit": "5f7bd4a5bfcaf78edfb0aa84a6ed04815ef5b420",
                    "day_key": "2026-10-16",
                    "event": "task.completed",
                    "home_id": "a0b1c2d3-e4f5-4a6b-8c7d-9e0f1a2b3c4d",
                    "outcome": "failure",
                    "platform": "mcao",
                    "pr": "https://github.com/example-org/second-repo/pull/7",
                    "repo": "example-org/second-repo",
                    "session_id": "sess-2",
                    "task_id": "ship-pr-316",
                    "user": {
                        "id": "hmac-sha256:cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd",
                        "identity_source": "git_email"
                    }
                },
                "event_id": "audit.activity:018f3b7f-1d2e-7a3b-9c4d-5e6f7a8b9c0d",
                "event_type": "audit.activity",
                "lineage": {
                    "event_id": "018f3b7f-1d2e-7a3b-9c4d-5e6f7a8b9c0d",
                    "prev_hash": "sha256:abababababababababababababababababababababababababababababababab",
                    "seq": 7
                },
                "occurred_at_utc": "2026-10-16T03:15:30.123Z",
                "schema": "mcao.siem.event/v1",
                "source_route": "mcao.audit-envelope.v1"
            },
            "logName": "projects/dpe-platform-d/logs/mcao-audit",
            "resource": {
                "type": "cloud_run_revision"
            },
            "severity": "DEFAULT",
            "timestamp": "2026-10-16T03:15:30.123Z"
        });
        assert_eq!(entry, golden_entry);
    }

    #[test]
    fn test_project_non_object_members_report_their_own_pointer() {
        // A member that must be an object but has another JSON type is reported at its own
        // pointer as a wrong type, never as a missing child.
        let cases = ["agent", "actor", "user"];
        let wrong_types = [json!("text"), json!(42), json!(["array"]), Value::Null];

        for parent in cases {
            for wrong in &wrong_types {
                let mut record = sample_envelope();
                record[parent] = wrong.clone();
                assert_eq!(
                    project(&record),
                    Err(ProjectError::WrongType(format!("/{parent}"))),
                    "failed when {parent} is {wrong}"
                );
            }
        }
    }

    #[test]
    fn test_project_non_object_record_reports_the_document_pointer() {
        assert_eq!(
            project(&json!("text")),
            Err(ProjectError::WrongType(String::new()))
        );
    }

    #[test]
    fn test_project_absent_task_id_projects_null() {
        // The SIEM contract (section 3.1) says task_id is null when the envelope omits it. The
        // envelope schema marks task_id optional, so the record is valid.
        let mut record = sample_envelope();
        record.as_object_mut().unwrap().remove("task_id");
        let projected = project(&record).expect("an absent task_id must project");

        let mut expected = project(&sample_envelope()).expect("the sample must project");
        expected["data"]["task_id"] = Value::Null;
        assert_eq!(projected, expected);
        assert_eq!(projected["data"]["task_id"], Value::Null);
    }

    #[test]
    fn test_project_refuses_missing_or_non_v1_schema() {
        let mut absent = sample_envelope();
        absent.as_object_mut().unwrap().remove("schema");
        assert_eq!(
            project(&absent),
            Err(ProjectError::SchemaInvalid("/schema".to_string()))
        );

        for wrong in [json!("mcao.audit-envelope.v2"), json!(42), Value::Null] {
            let mut record = sample_envelope();
            record["schema"] = wrong.clone();
            assert_eq!(
                project(&record),
                Err(ProjectError::SchemaInvalid("/schema".to_string())),
                "failed when schema is {wrong}"
            );
        }
    }

    #[test]
    fn test_project_missing_members() {
        let envelope = sample_envelope();

        let cases: &[(&str, &str)] = &[
            ("event_id", "/event_id"),
            ("ts", "/ts"),
            ("seq", "/seq"),
            ("prev_hash", "/prev_hash"),
            ("event", "/event"),
            ("category", "/category"),
            ("outcome", "/outcome"),
            ("platform", "/platform"),
            ("home_id", "/home_id"),
            ("day_key", "/day_key"),
            ("session_id", "/session_id"),
            ("agent", "/agent"),
            ("agent/role", "/agent/role"),
            ("agent/harness", "/agent/harness"),
            ("agent/model", "/agent/model"),
            ("actor", "/actor"),
            ("actor/type", "/actor/type"),
            ("actor/id", "/actor/id"),
            ("repo", "/repo"),
            ("pr", "/pr"),
            ("commit", "/commit"),
            ("user", "/user"),
            ("user/id", "/user/id"),
            ("user/identity_source", "/user/identity_source"),
        ];

        for &(remove_target, expected_pointer) in cases {
            let mut record = envelope.clone();
            let parts: Vec<&str> = remove_target.split('/').collect();
            if parts.len() == 1 {
                record.as_object_mut().unwrap().remove(parts[0]);
            } else if parts.len() == 2 {
                record
                    .get_mut(parts[0])
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .remove(parts[1]);
            }

            assert_eq!(
                project(&record),
                Err(ProjectError::Missing(expected_pointer.to_string())),
                "failed when removing {remove_target}"
            );
        }

        // A non-string event_id is a wrong type at its own pointer.
        for wrong in [json!(12345), json!(true), Value::Null] {
            let mut record = envelope.clone();
            record["event_id"] = wrong.clone();
            assert_eq!(
                project(&record),
                Err(ProjectError::WrongType("/event_id".to_string())),
                "failed when event_id is {wrong}"
            );
        }
    }

    #[test]
    fn test_log_entry_success_and_missing() {
        let envelope = sample_envelope();
        let projected = project(&envelope).expect("projection should succeed");

        let entry = log_entry(&projected).expect("log entry should succeed");
        let expected_entry = json!({
            "logName": "projects/dpe-platform-d/logs/mcao-audit",
            "resource": {
                "type": "cloud_run_revision",
            },
            "jsonPayload": projected,
            "timestamp": "2026-10-16T02:00:00.000Z",
            "insertId": "audit.activity:018f3b7e-9c1a-7d2e-8f3a-4b5c6d7e8f90",
            "severity": "DEFAULT",
        });
        assert_eq!(entry, expected_entry);

        // Missing occurred_at_utc
        let mut missing_ts = projected.clone();
        missing_ts
            .as_object_mut()
            .unwrap()
            .remove("occurred_at_utc");
        assert_eq!(
            log_entry(&missing_ts),
            Err(ProjectError::Missing("/occurred_at_utc".to_string()))
        );

        // Missing event_id
        let mut missing_id = projected.clone();
        missing_id.as_object_mut().unwrap().remove("event_id");
        assert_eq!(
            log_entry(&missing_id),
            Err(ProjectError::Missing("/event_id".to_string()))
        );
    }

    #[test]
    fn test_differing_pointers() {
        // identical -> empty
        let v1 = json!({"a": 1, "b": "hello", "c": [1, 2]});
        let v2 = json!({"a": 1, "b": "hello", "c": [1, 2]});
        assert_eq!(differing_pointers(&v1, &v2), Vec::<String>::new());

        // one nested string changed -> ["/data/event"]
        let before = json!({"data": {"event": "task.dispatched"}});
        let after = json!({"data": {"event": "task.completed"}});
        assert_eq!(differing_pointers(&before, &after), vec!["/data/event"]);

        // two changed in sorted key order
        let before = json!({"z": "old", "a": "old"});
        let after = json!({"z": "new", "a": "new"});
        assert_eq!(differing_pointers(&before, &after), vec!["/a", "/z"]);

        // key removed in after -> its pointer
        let before = json!({"keep": 1, "drop": 2});
        let after = json!({"keep": 1});
        assert_eq!(differing_pointers(&before, &after), vec!["/drop"]);

        // key only in after ignored
        let before = json!({"keep": 1});
        let after = json!({"keep": 1, "added": 2});
        assert_eq!(differing_pointers(&before, &after), Vec::<String>::new());

        // scalar vs object -> base pointer
        let before = json!({"nested": 1});
        let after = json!({"nested": {"inner": 2}});
        assert_eq!(differing_pointers(&before, &after), vec!["/nested"]);

        // array differs -> pointer of the array (not its elements)
        let before = json!({"items": [1, 2, 3]});
        let after = json!({"items": [1, 2, 4]});
        assert_eq!(differing_pointers(&before, &after), vec!["/items"]);

        // top-level scalars differ -> [""]
        let before = json!("alpha");
        let after = json!("beta");
        assert_eq!(differing_pointers(&before, &after), vec![""]);

        // key with / escapes in the output pointer
        let before = json!({"a/b": 1});
        let after = json!({"a/b": 2});
        assert_eq!(differing_pointers(&before, &after), vec!["/a~1b"]);
    }

    #[test]
    fn test_differing_pointers_sorted_key_order() {
        // Members the projection does not name sort by key. The workspace does not enable
        // preserve_order, so the result ignores how the input was written.
        let before = json!({"zeta": 1, "alpha": 1, "mid": 1});
        let after = json!({"zeta": 2, "alpha": 2, "mid": 2});
        assert_eq!(
            differing_pointers(&before, &after),
            vec!["/alpha", "/mid", "/zeta"]
        );

        let before_reordered = json!({"alpha": 1, "mid": 1, "zeta": 1});
        assert_eq!(
            differing_pointers(&before_reordered, &after),
            vec!["/alpha", "/mid", "/zeta"]
        );
    }

    #[test]
    fn test_differing_pointers_follows_projection_order() {
        // The reference walks the projected event in insertion order: top level, then lineage,
        // then data. Keys the projection names keep that order whatever the input key order.
        let before = project(&sample_envelope()).expect("the sample must project");
        let mut after = before.clone();
        after["data"]["task_id"] = json!("x");
        after["data"]["session_id"] = json!("y");
        after["lineage"]["event_id"] = json!("z");
        after["event_id"] = json!("w");
        assert_eq!(
            differing_pointers(&before, &after),
            vec![
                "/event_id",
                "/lineage/event_id",
                "/data/session_id",
                "/data/task_id",
            ]
        );
    }

    fn valid_sample_envelope() -> Value {
        // The validator also needs actor.id to equal user.id for a user actor.
        let mut record = sample_envelope();
        record["actor"]["id"] = record["user"]["id"].clone();
        record
    }

    /// The sample's zone is America/Chicago, which is UTC-5 at its timestamp.
    fn chicago_offset(zone: &str, _utc_epoch_secs: i64) -> Option<i32> {
        (zone == "America/Chicago").then_some(-18_000)
    }

    #[test]
    fn test_map_record_maps_a_clean_envelope_with_no_changes() {
        let mapped = map_record(&valid_sample_envelope(), &[0x5e; 32], &chicago_offset)
            .expect("a valid envelope must map");
        assert!(mapped.changed.is_empty());
        assert_eq!(
            mapped.event,
            project(&valid_sample_envelope()).expect("the sample must project")
        );
        assert_eq!(mapped.log_entry["insertId"], mapped.event["event_id"]);
    }

    #[test]
    fn test_map_record_redacts_and_lists_the_changed_member() {
        let mut record = valid_sample_envelope();
        record["task_id"] = json!("ghp_SyntheticTokenAbcdefghij0123");
        let mapped = map_record(&record, &[0x5e; 32], &chicago_offset).expect("must map");
        assert_eq!(mapped.changed, vec!["/data/task_id"]);
        assert_eq!(mapped.event["data"]["task_id"], "[REDACTED:github_token]");
        assert_eq!(
            mapped.log_entry["jsonPayload"]["data"]["task_id"],
            "[REDACTED:github_token]"
        );
    }

    #[test]
    fn test_map_record_refuses_an_invalid_envelope_with_pointers_only() {
        let mut record = valid_sample_envelope();
        record["schema"] = json!("mcao.audit-envelope.v2");
        assert_eq!(
            map_record(&record, &[0x5e; 32], &chicago_offset),
            Err(Refusal {
                reason: "schema_invalid",
                paths: vec!["/schema".to_string()],
            })
        );
    }

    #[test]
    fn test_project_error_display() {
        let err = ProjectError::Missing("/task_id".to_string());
        assert_eq!(format!("{err}"), "missing member /task_id");
    }
}
