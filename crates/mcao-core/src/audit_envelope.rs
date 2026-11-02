//! Audit envelope v1 and fleet ledger v1 validator.
//!
//! Validates audit envelope v1 and fleet ledger v1 records over parsed
//! JSON values without external regex or JSON Schema engines.

use std::collections::BTreeSet;

/// The document definition to validate against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Definition {
    /// Audit envelope v1 document.
    Envelope,
    /// Fleet ledger v1 record.
    FleetLedgerV1,
}

/// The reason a document was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// Identity fields are unresolved or missing.
    IdentityUnresolved,
    /// Schema constraints were violated.
    SchemaInvalid,
    /// The computed day key does not match the record's day key.
    DayKeyMismatch,
}

impl Reason {
    /// Returns the wire string representation of the rejection reason.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::IdentityUnresolved => "identity_unresolved",
            Self::SchemaInvalid => "schema_invalid",
            Self::DayKeyMismatch => "day_key_mismatch",
        }
    }
}

impl std::fmt::Display for Reason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A validation rejection containing the reason and affected JSON Pointer paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejection {
    /// The rejection reason.
    pub reason: Reason,
    /// Sorted, de-duplicated JSON Pointer paths where validation failed.
    pub paths: Vec<String>,
}

impl std::fmt::Display for Rejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {:?}", self.reason.as_str(), self.paths)
    }
}

impl std::error::Error for Rejection {}

/// Validates a parsed JSON document against the requested definition.
///
/// `utc_offset_secs(zone_name, utc_epoch_secs)` returns the zone's UTC offset
/// in seconds at that instant, or `None` for an unknown zone.
pub fn validate(
    doc: &serde_json::Value,
    definition: Definition,
    utc_offset_secs: &dyn Fn(&str, i64) -> Option<i32>,
) -> Result<(), Rejection> {
    match definition {
        Definition::Envelope => validate_envelope(doc, utc_offset_secs),
        Definition::FleetLedgerV1 => validate_fleet_ledger_v1(doc),
    }
}

fn validate_envelope(
    doc: &serde_json::Value,
    utc_offset_secs: &dyn Fn(&str, i64) -> Option<i32>,
) -> Result<(), Rejection> {
    // 1. Identity gaps (only if doc is an object).
    if let Some(doc_obj) = doc.as_object() {
        let user = doc_obj.get("user");
        match user {
            None => {
                return Err(Rejection {
                    reason: Reason::IdentityUnresolved,
                    paths: vec!["/user".to_string()],
                });
            }
            Some(u) if !u.is_object() => {
                return Err(Rejection {
                    reason: Reason::IdentityUnresolved,
                    paths: vec!["/user".to_string()],
                });
            }
            Some(u) => {
                let user_obj = u.as_object();
                let mut gaps = Vec::new();

                let id_val = user_obj.and_then(|o| o.get("id"));
                let is_id_gap = match id_val {
                    None => true,
                    Some(v) if v.is_null() => true,
                    Some(v) if v.as_str() == Some("PENDING") => true,
                    _ => false,
                };
                if is_id_gap {
                    gaps.push("/user/id".to_string());
                }

                let source_val = user_obj.and_then(|o| o.get("identity_source"));
                let is_source_gap = match source_val {
                    None => true,
                    Some(v) if v.is_null() => true,
                    _ => false,
                };
                if is_source_gap {
                    gaps.push("/user/identity_source".to_string());
                }

                if !gaps.is_empty() {
                    return Err(Rejection {
                        reason: Reason::IdentityUnresolved,
                        paths: gaps,
                    });
                }
            }
        }
    }

    // 2. Schema check.
    let mut errors = BTreeSet::new();
    check_envelope_schema(doc, &mut errors);
    if !errors.is_empty() {
        return Err(Rejection {
            reason: Reason::SchemaInvalid,
            paths: errors.into_iter().collect(),
        });
    }

    // 3. Cross checks.
    // (a) (seq == 0) != prev_hash.is_null() -> SchemaInvalid ["/prev_hash"]
    let is_seq_zero = match doc.get("seq") {
        Some(v) => v.as_i64() == Some(0) || v.as_u64() == Some(0),
        None => false,
    };
    let prev_hash_is_null = doc.get("prev_hash").is_some_and(|v| v.is_null());
    if is_seq_zero != prev_hash_is_null {
        return Err(Rejection {
            reason: Reason::SchemaInvalid,
            paths: vec!["/prev_hash".to_string()],
        });
    }

    // (b) actor.type == "user" and actor.id != user.id -> SchemaInvalid ["/actor/id"]
    let actor_type = doc
        .get("actor")
        .and_then(|a| a.get("type"))
        .and_then(|v| v.as_str());
    let actor_id = doc
        .get("actor")
        .and_then(|a| a.get("id"))
        .and_then(|v| v.as_str());
    let user_id = doc
        .get("user")
        .and_then(|u| u.get("id"))
        .and_then(|v| v.as_str());
    if actor_type == Some("user") && actor_id != user_id {
        return Err(Rejection {
            reason: Reason::SchemaInvalid,
            paths: vec!["/actor/id".to_string()],
        });
    }

    // (c) Zone known: utc_offset_secs(timezone, 0) is None -> SchemaInvalid ["/user/timezone"]
    let timezone = doc
        .get("user")
        .and_then(|u| u.get("timezone"))
        .and_then(|v| v.as_str());
    let Some(timezone) = timezone else {
        return Err(Rejection {
            reason: Reason::SchemaInvalid,
            paths: vec!["/user/timezone".to_string()],
        });
    };
    if utc_offset_secs(timezone, 0).is_none() {
        return Err(Rejection {
            reason: Reason::SchemaInvalid,
            paths: vec!["/user/timezone".to_string()],
        });
    }

    // (d) ts calendar check: year 1..=9999, month 1..=12, day valid for month (Gregorian leap),
    //     hour 0..=23, minute 0..=59, second 0..=59 -> SchemaInvalid ["/ts"]
    let Some(ts_str) = doc.get("ts").and_then(|v| v.as_str()) else {
        return Err(Rejection {
            reason: Reason::SchemaInvalid,
            paths: vec!["/ts".to_string()],
        });
    };
    let Some((year, month, day, hour, minute, second)) = parse_ts_calendar(ts_str) else {
        return Err(Rejection {
            reason: Reason::SchemaInvalid,
            paths: vec!["/ts".to_string()],
        });
    };

    // (e) ts_epoch = UTC epoch seconds; offset = utc_offset_secs(timezone, ts_epoch);
    //     local seconds = ts_epoch + offset; local year in 1..=9999 -> SchemaInvalid ["/ts"]
    let days = days_from_civil(year, month, day);
    let ts_epoch = days * 86400 + (hour as i64) * 3600 + (minute as i64) * 60 + (second as i64);
    let offset = match utc_offset_secs(timezone, ts_epoch) {
        Some(off) => off as i64,
        None => {
            return Err(Rejection {
                reason: Reason::SchemaInvalid,
                paths: vec!["/user/timezone".to_string()],
            });
        }
    };
    let local_seconds = ts_epoch + offset;
    let local_days = local_seconds.div_euclid(86400);
    let (local_year, local_month, local_day) = civil_from_days(local_days);
    if !(1..=9999).contains(&local_year) {
        return Err(Rejection {
            reason: Reason::SchemaInvalid,
            paths: vec!["/ts".to_string()],
        });
    }

    // (f) Local date as zero-padded YYYY-MM-DD != day_key -> DayKeyMismatch ["/day_key"]
    let computed_day_key = format!("{local_year:04}-{local_month:02}-{local_day:02}");
    let Some(actual_day_key) = doc.get("day_key").and_then(|v| v.as_str()) else {
        return Err(Rejection {
            reason: Reason::DayKeyMismatch,
            paths: vec!["/day_key".to_string()],
        });
    };
    if computed_day_key != actual_day_key {
        return Err(Rejection {
            reason: Reason::DayKeyMismatch,
            paths: vec!["/day_key".to_string()],
        });
    }

    Ok(())
}

fn check_envelope_schema(doc: &serde_json::Value, errors: &mut BTreeSet<String>) {
    let Some(doc_obj) = doc.as_object() else {
        errors.insert(String::new());
        return;
    };

    const ENVELOPE_REQUIRED: &[&str] = &[
        "schema",
        "event_id",
        "seq",
        "ts",
        "prev_hash",
        "event",
        "category",
        "outcome",
        "platform",
        "home_id",
        "day_key",
        "session_id",
        "agent",
        "actor",
        "repo",
        "pr",
        "commit",
        "user",
    ];

    const ENVELOPE_PROPERTIES: &[&str] = &[
        "schema",
        "event_id",
        "seq",
        "ts",
        "prev_hash",
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

    for key in doc_obj.keys() {
        if !ENVELOPE_PROPERTIES.contains(&key.as_str()) {
            errors.insert(String::new());
            break;
        }
    }

    for &req in ENVELOPE_REQUIRED {
        if !doc_obj.contains_key(req) {
            errors.insert(format!("/{req}"));
        }
    }

    if doc_obj
        .get("schema")
        .is_some_and(|val| val.as_str() != Some("mcao.audit-envelope.v1"))
    {
        errors.insert("/schema".to_string());
    }

    if let Some(val) = doc_obj.get("event_id") {
        match val.as_str() {
            Some(s) if is_uuid_v7(s) => {}
            _ => {
                errors.insert("/event_id".to_string());
            }
        }
    }

    if let Some(val) = doc_obj.get("seq") {
        let is_valid_seq = match (val.as_i64(), val.as_u64()) {
            (Some(n), _) => n >= 0,
            (_, Some(_)) => true,
            _ => false,
        };
        if !is_valid_seq {
            errors.insert("/seq".to_string());
        }
    }

    if let Some(val) = doc_obj.get("ts") {
        match val.as_str() {
            Some(s) if is_ts_format(s) => {}
            _ => {
                errors.insert("/ts".to_string());
            }
        }
    }

    if let Some(val) = doc_obj.get("prev_hash") {
        if val.is_null() {
            // null is permitted
        } else if let Some(s) = val.as_str() {
            if !is_sha256_hash(s) {
                errors.insert("/prev_hash".to_string());
            }
        } else {
            errors.insert("/prev_hash".to_string());
        }
    }

    if let Some(val) = doc_obj.get("event") {
        match val.as_str() {
            Some(s) if is_valid_event(s) => {}
            _ => {
                errors.insert("/event".to_string());
            }
        }
    }

    if let Some(val) = doc_obj.get("category") {
        match val.as_str() {
            Some("lifecycle" | "delivery" | "status" | "supervision" | "identity") => {}
            _ => {
                errors.insert("/category".to_string());
            }
        }
    }

    if let Some(val) = doc_obj.get("outcome") {
        match val.as_str() {
            Some("success" | "failure" | "rejected" | "not_applicable") => {}
            _ => {
                errors.insert("/outcome".to_string());
            }
        }
    }

    if doc_obj
        .get("platform")
        .is_some_and(|val| val.as_str() != Some("mcao"))
    {
        errors.insert("/platform".to_string());
    }

    if let Some(val) = doc_obj.get("home_id") {
        match val.as_str() {
            Some(s) if is_uuid_v4(s) => {}
            _ => {
                errors.insert("/home_id".to_string());
            }
        }
    }

    if let Some(val) = doc_obj.get("day_key") {
        match val.as_str() {
            Some(s) if is_day_key_format(s) => {}
            _ => {
                errors.insert("/day_key".to_string());
            }
        }
    }

    if let Some(val) = doc_obj.get("session_id") {
        match val.as_str() {
            Some(s) if !s.is_empty() => {}
            _ => {
                errors.insert("/session_id".to_string());
            }
        }
    }

    if let Some(val) = doc_obj.get("task_id") {
        match val.as_str() {
            Some(s) if !s.is_empty() => {}
            _ => {
                errors.insert("/task_id".to_string());
            }
        }
    }

    if let Some(val) = doc_obj.get("agent") {
        check_agent(val, errors);
    }

    if let Some(val) = doc_obj.get("actor") {
        check_actor(val, errors);
    }

    if let Some(val) = doc_obj.get("repo") {
        if val.is_null() {
            // null is permitted
        } else if let Some(s) = val.as_str() {
            if !is_valid_repo(s) {
                errors.insert("/repo".to_string());
            }
        } else {
            errors.insert("/repo".to_string());
        }
    }

    if let Some(val) = doc_obj.get("pr") {
        if val.is_null() {
            // null is permitted
        } else if let Some(s) = val.as_str() {
            if !is_valid_pr(s) {
                errors.insert("/pr".to_string());
            }
        } else {
            errors.insert("/pr".to_string());
        }
    }

    if let Some(val) = doc_obj.get("commit") {
        if val.is_null() {
            // null is permitted
        } else if let Some(s) = val.as_str() {
            if !is_sha1_commit(s) {
                errors.insert("/commit".to_string());
            }
        } else {
            errors.insert("/commit".to_string());
        }
    }

    if let Some(val) = doc_obj.get("user") {
        check_user(val, errors);
    }
}

fn check_agent(val: &serde_json::Value, errors: &mut BTreeSet<String>) {
    let Some(agent_obj) = val.as_object() else {
        errors.insert("/agent".to_string());
        return;
    };

    const AGENT_PROPERTIES: &[&str] = &["role", "harness", "model"];

    for key in agent_obj.keys() {
        if !AGENT_PROPERTIES.contains(&key.as_str()) {
            errors.insert("/agent".to_string());
            break;
        }
    }

    for &req in AGENT_PROPERTIES {
        if !agent_obj.contains_key(req) {
            errors.insert(format!("/agent/{req}"));
        }
    }

    if let Some(role) = agent_obj.get("role") {
        match role.as_str() {
            Some("firstmate" | "secondmate" | "ship" | "scout") => {}
            _ => {
                errors.insert("/agent/role".to_string());
            }
        }
    }

    if let Some(harness) = agent_obj.get("harness") {
        match harness.as_str() {
            Some(s) if !s.is_empty() => {}
            _ => {
                errors.insert("/agent/harness".to_string());
            }
        }
    }

    if let Some(model) = agent_obj.get("model") {
        if model.is_null() {
            // null is permitted
        } else if let Some(s) = model.as_str() {
            if s.is_empty() {
                errors.insert("/agent/model".to_string());
            }
        } else {
            errors.insert("/agent/model".to_string());
        }
    }
}

fn check_actor(val: &serde_json::Value, errors: &mut BTreeSet<String>) {
    let Some(actor_obj) = val.as_object() else {
        errors.insert("/actor".to_string());
        return;
    };

    const ACTOR_PROPERTIES: &[&str] = &["type", "id"];

    for key in actor_obj.keys() {
        if !ACTOR_PROPERTIES.contains(&key.as_str()) {
            errors.insert("/actor".to_string());
            break;
        }
    }

    for &req in ACTOR_PROPERTIES {
        if !actor_obj.contains_key(req) {
            errors.insert(format!("/actor/{req}"));
        }
    }

    if let Some(act_type) = actor_obj.get("type") {
        match act_type.as_str() {
            Some("user" | "agent" | "system") => {}
            _ => {
                errors.insert("/actor/type".to_string());
            }
        }
    }

    if let Some(id) = actor_obj.get("id") {
        match id.as_str() {
            Some(s) if !s.is_empty() => {}
            _ => {
                errors.insert("/actor/id".to_string());
            }
        }
    }
}

fn check_user(val: &serde_json::Value, errors: &mut BTreeSet<String>) {
    let Some(user_obj) = val.as_object() else {
        errors.insert("/user".to_string());
        return;
    };

    const USER_PROPERTIES: &[&str] = &["id", "identity_source", "timezone"];

    for key in user_obj.keys() {
        if !USER_PROPERTIES.contains(&key.as_str()) {
            errors.insert("/user".to_string());
            break;
        }
    }

    for &req in USER_PROPERTIES {
        if !user_obj.contains_key(req) {
            errors.insert(format!("/user/{req}"));
        }
    }

    if let Some(id) = user_obj.get("id") {
        match id.as_str() {
            Some(s) if is_hmac_sha256(s) => {}
            _ => {
                errors.insert("/user/id".to_string());
            }
        }
    }

    if let Some(source) = user_obj.get("identity_source") {
        match source.as_str() {
            Some("github_login" | "git_email" | "local_account") => {}
            _ => {
                errors.insert("/user/identity_source".to_string());
            }
        }
    }

    if let Some(tz) = user_obj.get("timezone") {
        match tz.as_str() {
            Some(s) if !s.is_empty() => {}
            _ => {
                errors.insert("/user/timezone".to_string());
            }
        }
    }
}

fn validate_fleet_ledger_v1(doc: &serde_json::Value) -> Result<(), Rejection> {
    let Some(doc_obj) = doc.as_object() else {
        return Err(Rejection {
            reason: Reason::SchemaInvalid,
            paths: vec![String::new()],
        });
    };

    let mut errors = BTreeSet::new();

    // v: required, const 1 (an integer or float 1.0 equals 1; true does not)
    if let Some(v) = doc_obj.get("v") {
        if !is_const_1(v) {
            errors.insert("/v".to_string());
        }
    } else {
        errors.insert("/v".to_string());
    }

    // ts: required, integer min 0
    if let Some(ts) = doc_obj.get("ts") {
        let is_valid_ts = match (ts.as_i64(), ts.as_u64()) {
            (Some(n), _) => n >= 0,
            (_, Some(_)) => true,
            _ => false,
        };
        if !is_valid_ts {
            errors.insert("/ts".to_string());
        }
    } else {
        errors.insert("/ts".to_string());
    }

    // event: required, string min 1
    if let Some(event) = doc_obj.get("event") {
        match event.as_str() {
            Some(s) if !s.is_empty() => {}
            _ => {
                errors.insert("/event".to_string());
            }
        }
    } else {
        errors.insert("/event".to_string());
    }

    // task: required, string min 1
    if let Some(task) = doc_obj.get("task") {
        match task.as_str() {
            Some(s) if !s.is_empty() => {}
            _ => {
                errors.insert("/task".to_string());
            }
        }
    } else {
        errors.insert("/task".to_string());
    }

    if !errors.is_empty() {
        Err(Rejection {
            reason: Reason::SchemaInvalid,
            paths: errors.into_iter().collect(),
        })
    } else {
        Ok(())
    }
}

fn is_const_1(val: &serde_json::Value) -> bool {
    if let Some(n) = val.as_i64() {
        n == 1
    } else if let Some(n) = val.as_u64() {
        n == 1
    } else if let Some(f) = val.as_f64() {
        f == 1.0
    } else {
        false
    }
}

fn is_lower_hex(b: &u8) -> bool {
    matches!(b, b'0'..=b'9' | b'a'..=b'f')
}

fn is_uuid_v7(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 36 {
        return false;
    }
    b[8] == b'-'
        && b[13] == b'-'
        && b[18] == b'-'
        && b[23] == b'-'
        && b[14] == b'7'
        && matches!(b[19], b'8' | b'9' | b'a' | b'b')
        && b[0..8].iter().all(is_lower_hex)
        && b[9..13].iter().all(is_lower_hex)
        && b[15..18].iter().all(is_lower_hex)
        && b[20..23].iter().all(is_lower_hex)
        && b[24..36].iter().all(is_lower_hex)
}

fn is_uuid_v4(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 36 {
        return false;
    }
    b[8] == b'-'
        && b[13] == b'-'
        && b[18] == b'-'
        && b[23] == b'-'
        && b[14] == b'4'
        && matches!(b[19], b'8' | b'9' | b'a' | b'b')
        && b[0..8].iter().all(is_lower_hex)
        && b[9..13].iter().all(is_lower_hex)
        && b[15..18].iter().all(is_lower_hex)
        && b[20..23].iter().all(is_lower_hex)
        && b[24..36].iter().all(is_lower_hex)
}

fn is_ts_format(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 24 {
        return false;
    }
    b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
        && b[13] == b':'
        && b[16] == b':'
        && b[19] == b'.'
        && b[23] == b'Z'
        && b[0..4].iter().all(u8::is_ascii_digit)
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[8..10].iter().all(u8::is_ascii_digit)
        && b[11..13].iter().all(u8::is_ascii_digit)
        && b[14..16].iter().all(u8::is_ascii_digit)
        && b[17..19].iter().all(u8::is_ascii_digit)
        && b[20..23].iter().all(u8::is_ascii_digit)
}

fn is_day_key_format(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 10 {
        return false;
    }
    b[4] == b'-'
        && b[7] == b'-'
        && b[0..4].iter().all(u8::is_ascii_digit)
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[8..10].iter().all(u8::is_ascii_digit)
}

fn is_sha256_hash(s: &str) -> bool {
    if !s.starts_with("sha256:") {
        return false;
    }
    let rest = &s[7..];
    rest.len() == 64 && rest.as_bytes().iter().all(is_lower_hex)
}

fn is_hmac_sha256(s: &str) -> bool {
    if !s.starts_with("hmac-sha256:") {
        return false;
    }
    let rest = &s[12..];
    rest.len() == 64 && rest.as_bytes().iter().all(is_lower_hex)
}

fn is_sha1_commit(s: &str) -> bool {
    s.len() == 40 && s.as_bytes().iter().all(is_lower_hex)
}

fn is_valid_repo(s: &str) -> bool {
    let parts: Vec<&str> = s.split('/').collect();
    if parts.len() != 2 {
        return false;
    }
    let is_repo_char = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'-';
    let p0 = parts[0].as_bytes();
    let p1 = parts[1].as_bytes();
    !p0.is_empty()
        && !p1.is_empty()
        && p0.iter().all(|&c| is_repo_char(c))
        && p1.iter().all(|&c| is_repo_char(c))
}

fn is_valid_pr(s: &str) -> bool {
    if !s.starts_with("https://") {
        return false;
    }
    let rest = &s[8..];
    if rest.is_empty() {
        return false;
    }
    !rest.chars().any(|c| c == ' ' || c == '\n')
}

fn is_valid_event(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() < 2 {
        return false;
    }
    for part in parts {
        let b = part.as_bytes();
        if b.is_empty() || !b[0].is_ascii_lowercase() {
            return false;
        }
        if !b[1..]
            .iter()
            .all(|&c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
        {
            return false;
        }
    }
    true
}

fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

pub(crate) fn parse_ts_calendar(ts: &str) -> Option<(i64, u32, u32, u32, u32, u32)> {
    if ts.len() != 24 {
        return None;
    }
    let year = ts[0..4].parse::<i64>().ok()?;
    let month = ts[5..7].parse::<u32>().ok()?;
    let day = ts[8..10].parse::<u32>().ok()?;
    let hour = ts[11..13].parse::<u32>().ok()?;
    let minute = ts[14..16].parse::<u32>().ok()?;
    let second = ts[17..19].parse::<u32>().ok()?;

    if !(1..=9999).contains(&year) {
        return None;
    }
    if !(1..=12).contains(&month) {
        return None;
    }
    let max_days = days_in_month(year, month);
    if day < 1 || day > max_days {
        return None;
    }
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    Some((year, month, day, hour, minute, second))
}

/// Computes days since 1970-01-01 from a civil date (Howard Hinnant's algorithm).
pub(crate) fn days_from_civil(mut y: i64, m: u32, d: u32) -> i64 {
    if m <= 2 {
        y -= 1;
    }
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u32;
    let m_adj = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * m_adj + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + (doe as i64) - 719468
}

/// Computes civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_zone_table(zone: &str, _instant: i64) -> Option<i32> {
        match zone {
            "America/Chicago" => Some(-18000),
            "UTC" => Some(0),
            "Asia/Kolkata" => Some(19800),
            _ => None,
        }
    }

    fn user_id_u() -> String {
        format!("hmac-sha256:{}", "a".repeat(64))
    }

    fn base_doc() -> serde_json::Value {
        let u = user_id_u();
        serde_json::json!({
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
                "id": u
            },
            "repo": "mc-innersource/motif-hve-multi-agent-orchestrator",
            "pr": null,
            "commit": null,
            "user": {
                "id": u,
                "identity_source": "github_login",
                "timezone": "America/Chicago"
            }
        })
    }

    // 1. Base accepted. day_key 2026-10-16 -> DayKeyMismatch ["/day_key"].
    //    Kolkata with ts 2026-10-15T20:00:00.000Z: day_key 2026-10-16 accepted, 2026-10-15 DayKeyMismatch.
    //    task_id removed accepted.
    #[test]
    fn test_base_and_day_key_and_optional_task_id() {
        let base = base_doc();
        assert_eq!(
            validate(&base, Definition::Envelope, &test_zone_table),
            Ok(())
        );

        let mut doc = base.clone();
        doc["day_key"] = serde_json::json!("2026-10-16");
        assert_eq!(
            validate(&doc, Definition::Envelope, &test_zone_table),
            Err(Rejection {
                reason: Reason::DayKeyMismatch,
                paths: vec!["/day_key".to_string()],
            })
        );

        let mut doc_kolkata = base.clone();
        doc_kolkata["user"]["timezone"] = serde_json::json!("Asia/Kolkata");
        doc_kolkata["ts"] = serde_json::json!("2026-10-15T20:00:00.000Z");
        doc_kolkata["day_key"] = serde_json::json!("2026-10-16");
        assert_eq!(
            validate(&doc_kolkata, Definition::Envelope, &test_zone_table),
            Ok(())
        );

        doc_kolkata["day_key"] = serde_json::json!("2026-10-15");
        assert_eq!(
            validate(&doc_kolkata, Definition::Envelope, &test_zone_table),
            Err(Rejection {
                reason: Reason::DayKeyMismatch,
                paths: vec!["/day_key".to_string()],
            })
        );

        let mut doc_no_task = base;
        doc_no_task.as_object_mut().unwrap().remove("task_id");
        assert_eq!(
            validate(&doc_no_task, Definition::Envelope, &test_zone_table),
            Ok(())
        );
    }

    // 2. Identity: user.id null -> IdentityUnresolved ["/user/id"]; "PENDING" same;
    //    identity_source null -> ["/user/identity_source"]; both -> ["/user/id","/user/identity_source"];
    //    user removed or "x" -> ["/user"]; error values are never echoed (paths only).
    #[test]
    fn test_identity_gaps() {
        let base = base_doc();

        let mut doc = base.clone();
        doc["user"]["id"] = serde_json::Value::Null;
        assert_eq!(
            validate(&doc, Definition::Envelope, &test_zone_table),
            Err(Rejection {
                reason: Reason::IdentityUnresolved,
                paths: vec!["/user/id".to_string()],
            })
        );

        let mut doc_pending = base.clone();
        doc_pending["user"]["id"] = serde_json::json!("PENDING");
        assert_eq!(
            validate(&doc_pending, Definition::Envelope, &test_zone_table),
            Err(Rejection {
                reason: Reason::IdentityUnresolved,
                paths: vec!["/user/id".to_string()],
            })
        );

        let mut doc_source_null = base.clone();
        doc_source_null["user"]["identity_source"] = serde_json::Value::Null;
        assert_eq!(
            validate(&doc_source_null, Definition::Envelope, &test_zone_table),
            Err(Rejection {
                reason: Reason::IdentityUnresolved,
                paths: vec!["/user/identity_source".to_string()],
            })
        );

        let mut doc_both_null = base.clone();
        doc_both_null["user"]["id"] = serde_json::Value::Null;
        doc_both_null["user"]["identity_source"] = serde_json::Value::Null;
        assert_eq!(
            validate(&doc_both_null, Definition::Envelope, &test_zone_table),
            Err(Rejection {
                reason: Reason::IdentityUnresolved,
                paths: vec!["/user/id".to_string(), "/user/identity_source".to_string()],
            })
        );

        let mut doc_user_removed = base.clone();
        doc_user_removed.as_object_mut().unwrap().remove("user");
        assert_eq!(
            validate(&doc_user_removed, Definition::Envelope, &test_zone_table),
            Err(Rejection {
                reason: Reason::IdentityUnresolved,
                paths: vec!["/user".to_string()],
            })
        );

        let mut doc_user_x = base;
        doc_user_x["user"] = serde_json::json!("x");
        assert_eq!(
            validate(&doc_user_x, Definition::Envelope, &test_zone_table),
            Err(Rejection {
                reason: Reason::IdentityUnresolved,
                paths: vec!["/user".to_string()],
            })
        );
    }

    // 3. Cross: seq 1 with prev_hash null -> ["/prev_hash"];
    //    seq 0 with prev_hash sha256: + 64 x "f" -> ["/prev_hash"];
    //    seq 1 with that prev_hash accepted.
    //    actor {user, id hmac-sha256: + 64 x "b"} -> ["/actor/id"];
    //    actor {agent, id "a"} accepted.
    //    Timezone Mars/Base -> ["/user/timezone"] (also when ts is invalid).
    //    ts 2026-02-30T00:00:00.000Z -> ["/ts"]; 2026-10-16T24:00:00.000Z -> ["/ts"];
    //    2026-10-16T02:00:60.000Z -> ["/ts"]; 2023-02-29T12:00:00.000Z -> ["/ts"];
    //    2024-02-29T12:00:00.000Z with day_key 2024-02-29 and timezone UTC accepted.
    //    0001-01-01T00:00:00.000Z with UTC and day_key 0001-01-01 accepted; the same with Chicago -> ["/ts"];
    //    9999-12-31T23:59:59.000Z with Kolkata -> ["/ts"].
    //    ts 2026-03-01T00:30:00.000Z Chicago day_key 2026-02-28 accepted.
    #[test]
    fn test_cross_checks() {
        let base = base_doc();
        let prev_hash_f = format!("sha256:{}", "f".repeat(64));

        let mut doc_seq1_null = base.clone();
        doc_seq1_null["seq"] = serde_json::json!(1);
        assert_eq!(
            validate(&doc_seq1_null, Definition::Envelope, &test_zone_table),
            Err(Rejection {
                reason: Reason::SchemaInvalid,
                paths: vec!["/prev_hash".to_string()],
            })
        );

        let mut doc_seq0_hash = base.clone();
        doc_seq0_hash["prev_hash"] = serde_json::json!(prev_hash_f);
        assert_eq!(
            validate(&doc_seq0_hash, Definition::Envelope, &test_zone_table),
            Err(Rejection {
                reason: Reason::SchemaInvalid,
                paths: vec!["/prev_hash".to_string()],
            })
        );

        let mut doc_seq1_hash = base.clone();
        doc_seq1_hash["seq"] = serde_json::json!(1);
        doc_seq1_hash["prev_hash"] = serde_json::json!(prev_hash_f);
        assert_eq!(
            validate(&doc_seq1_hash, Definition::Envelope, &test_zone_table),
            Ok(())
        );

        let other_u = format!("hmac-sha256:{}", "b".repeat(64));
        let mut doc_actor_mismatch = base.clone();
        doc_actor_mismatch["actor"]["id"] = serde_json::json!(other_u);
        assert_eq!(
            validate(&doc_actor_mismatch, Definition::Envelope, &test_zone_table),
            Err(Rejection {
                reason: Reason::SchemaInvalid,
                paths: vec!["/actor/id".to_string()],
            })
        );

        let mut doc_actor_agent = base.clone();
        doc_actor_agent["actor"] = serde_json::json!({
            "type": "agent",
            "id": "a"
        });
        assert_eq!(
            validate(&doc_actor_agent, Definition::Envelope, &test_zone_table),
            Ok(())
        );

        let mut doc_mars = base.clone();
        doc_mars["user"]["timezone"] = serde_json::json!("Mars/Base");
        assert_eq!(
            validate(&doc_mars, Definition::Envelope, &test_zone_table),
            Err(Rejection {
                reason: Reason::SchemaInvalid,
                paths: vec!["/user/timezone".to_string()],
            })
        );

        let mut doc_mars_bad_ts = doc_mars.clone();
        doc_mars_bad_ts["ts"] = serde_json::json!("2026-02-30T00:00:00.000Z");
        assert_eq!(
            validate(&doc_mars_bad_ts, Definition::Envelope, &test_zone_table),
            Err(Rejection {
                reason: Reason::SchemaInvalid,
                paths: vec!["/user/timezone".to_string()],
            })
        );

        for bad_ts in [
            "2026-02-30T00:00:00.000Z",
            "2026-10-16T24:00:00.000Z",
            "2026-10-16T02:00:60.000Z",
            "2023-02-29T12:00:00.000Z",
        ] {
            let mut d = base.clone();
            d["ts"] = serde_json::json!(bad_ts);
            assert_eq!(
                validate(&d, Definition::Envelope, &test_zone_table),
                Err(Rejection {
                    reason: Reason::SchemaInvalid,
                    paths: vec!["/ts".to_string()],
                }),
                "expected failure for ts={bad_ts}"
            );
        }

        let mut doc_leap_utc = base.clone();
        doc_leap_utc["user"]["timezone"] = serde_json::json!("UTC");
        doc_leap_utc["ts"] = serde_json::json!("2024-02-29T12:00:00.000Z");
        doc_leap_utc["day_key"] = serde_json::json!("2024-02-29");
        assert_eq!(
            validate(&doc_leap_utc, Definition::Envelope, &test_zone_table),
            Ok(())
        );

        let mut doc_year1_utc = base.clone();
        doc_year1_utc["user"]["timezone"] = serde_json::json!("UTC");
        doc_year1_utc["ts"] = serde_json::json!("0001-01-01T00:00:00.000Z");
        doc_year1_utc["day_key"] = serde_json::json!("0001-01-01");
        assert_eq!(
            validate(&doc_year1_utc, Definition::Envelope, &test_zone_table),
            Ok(())
        );

        let mut doc_year1_chicago = base.clone();
        doc_year1_chicago["user"]["timezone"] = serde_json::json!("America/Chicago");
        doc_year1_chicago["ts"] = serde_json::json!("0001-01-01T00:00:00.000Z");
        doc_year1_chicago["day_key"] = serde_json::json!("0001-01-01");
        assert_eq!(
            validate(&doc_year1_chicago, Definition::Envelope, &test_zone_table),
            Err(Rejection {
                reason: Reason::SchemaInvalid,
                paths: vec!["/ts".to_string()],
            })
        );

        let mut doc_year9999_kolkata = base.clone();
        doc_year9999_kolkata["user"]["timezone"] = serde_json::json!("Asia/Kolkata");
        doc_year9999_kolkata["ts"] = serde_json::json!("9999-12-31T23:59:59.000Z");
        doc_year9999_kolkata["day_key"] = serde_json::json!("9999-12-31");
        assert_eq!(
            validate(
                &doc_year9999_kolkata,
                Definition::Envelope,
                &test_zone_table
            ),
            Err(Rejection {
                reason: Reason::SchemaInvalid,
                paths: vec!["/ts".to_string()],
            })
        );

        let mut doc_feb28 = base;
        doc_feb28["ts"] = serde_json::json!("2026-03-01T00:30:00.000Z");
        doc_feb28["day_key"] = serde_json::json!("2026-02-28");
        assert_eq!(
            validate(&doc_feb28, Definition::Envelope, &test_zone_table),
            Ok(())
        );
    }

    // 4. Schema (paths sorted):
    //    extra root key x -> [""]; commit removed -> ["/commit"]; category other -> ["/category"];
    //    both -> ["/category","/commit"]; seq -1, seq 1.0 and seq true -> ["/seq"];
    //    event task -> ["/event"]; event_id with 4 version nibble -> ["/event_id"];
    //    home_id with 7 nibble -> ["/home_id"]; pr http://x and https://a b -> ["/pr"];
    //    repo a/b/c -> ["/repo"]; commit of 39 hex chars -> ["/commit"]; agent extra key -> ["/agent"];
    //    agent role pilot -> ["/agent/role"]; agent without model -> ["/agent/model"];
    //    model "" -> ["/agent/model"]; session_id "" -> ["/session_id"]; platform x -> ["/platform"];
    //    schema v2 -> ["/schema"]; user.id hmac-sha256:abc -> ["/user/id"]; user.timezone "" -> ["/user/timezone"];
    //    agent set to 5 -> ["/agent"]; whole doc [] -> SchemaInvalid [""]; whole doc null -> [""];
    //    ts 2026-10-16T02:00:00Z (no millis) -> ["/ts"].
    #[test]
    fn test_schema_checks() {
        let base = base_doc();

        let mut doc_extra = base.clone();
        doc_extra["x"] = serde_json::json!(1);
        assert_eq!(
            validate(&doc_extra, Definition::Envelope, &test_zone_table),
            Err(Rejection {
                reason: Reason::SchemaInvalid,
                paths: vec![String::new()],
            })
        );

        let mut doc_no_commit = base.clone();
        doc_no_commit.as_object_mut().unwrap().remove("commit");
        assert_eq!(
            validate(&doc_no_commit, Definition::Envelope, &test_zone_table),
            Err(Rejection {
                reason: Reason::SchemaInvalid,
                paths: vec!["/commit".to_string()],
            })
        );

        let mut doc_other_cat = base.clone();
        doc_other_cat["category"] = serde_json::json!("other");
        assert_eq!(
            validate(&doc_other_cat, Definition::Envelope, &test_zone_table),
            Err(Rejection {
                reason: Reason::SchemaInvalid,
                paths: vec!["/category".to_string()],
            })
        );

        let mut doc_both = base.clone();
        doc_both["category"] = serde_json::json!("other");
        doc_both.as_object_mut().unwrap().remove("commit");
        assert_eq!(
            validate(&doc_both, Definition::Envelope, &test_zone_table),
            Err(Rejection {
                reason: Reason::SchemaInvalid,
                paths: vec!["/category".to_string(), "/commit".to_string()],
            })
        );

        for bad_seq in [
            serde_json::json!(-1),
            serde_json::json!(1.0),
            serde_json::json!(true),
        ] {
            let mut d = base.clone();
            d["seq"] = bad_seq;
            assert_eq!(
                validate(&d, Definition::Envelope, &test_zone_table),
                Err(Rejection {
                    reason: Reason::SchemaInvalid,
                    paths: vec!["/seq".to_string()],
                })
            );
        }

        let mut doc_bad_event = base.clone();
        doc_bad_event["event"] = serde_json::json!("task");
        assert_eq!(
            validate(&doc_bad_event, Definition::Envelope, &test_zone_table),
            Err(Rejection {
                reason: Reason::SchemaInvalid,
                paths: vec!["/event".to_string()],
            })
        );

        let mut doc_event_v4 = base.clone();
        doc_event_v4["event_id"] = serde_json::json!("018f3b7e-9c1a-4d2e-8f3a-4b5c6d7e8f90");
        assert_eq!(
            validate(&doc_event_v4, Definition::Envelope, &test_zone_table),
            Err(Rejection {
                reason: Reason::SchemaInvalid,
                paths: vec!["/event_id".to_string()],
            })
        );

        let mut doc_home_v7 = base.clone();
        doc_home_v7["home_id"] = serde_json::json!("018f3b7e-9c1a-7d2e-8f3a-4b5c6d7e8f90");
        assert_eq!(
            validate(&doc_home_v7, Definition::Envelope, &test_zone_table),
            Err(Rejection {
                reason: Reason::SchemaInvalid,
                paths: vec!["/home_id".to_string()],
            })
        );

        for bad_pr in ["http://x", "https://a b"] {
            let mut d = base.clone();
            d["pr"] = serde_json::json!(bad_pr);
            assert_eq!(
                validate(&d, Definition::Envelope, &test_zone_table),
                Err(Rejection {
                    reason: Reason::SchemaInvalid,
                    paths: vec!["/pr".to_string()],
                })
            );
        }

        let mut doc_repo = base.clone();
        doc_repo["repo"] = serde_json::json!("a/b/c");
        assert_eq!(
            validate(&doc_repo, Definition::Envelope, &test_zone_table),
            Err(Rejection {
                reason: Reason::SchemaInvalid,
                paths: vec!["/repo".to_string()],
            })
        );

        let mut doc_commit_39 = base.clone();
        doc_commit_39["commit"] = serde_json::json!("a".repeat(39));
        assert_eq!(
            validate(&doc_commit_39, Definition::Envelope, &test_zone_table),
            Err(Rejection {
                reason: Reason::SchemaInvalid,
                paths: vec!["/commit".to_string()],
            })
        );

        let mut doc_agent_extra = base.clone();
        doc_agent_extra["agent"]["extra"] = serde_json::json!(1);
        assert_eq!(
            validate(&doc_agent_extra, Definition::Envelope, &test_zone_table),
            Err(Rejection {
                reason: Reason::SchemaInvalid,
                paths: vec!["/agent".to_string()],
            })
        );

        let mut doc_agent_role = base.clone();
        doc_agent_role["agent"]["role"] = serde_json::json!("pilot");
        assert_eq!(
            validate(&doc_agent_role, Definition::Envelope, &test_zone_table),
            Err(Rejection {
                reason: Reason::SchemaInvalid,
                paths: vec!["/agent/role".to_string()],
            })
        );

        let mut doc_agent_no_model = base.clone();
        doc_agent_no_model
            .get_mut("agent")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove("model");
        assert_eq!(
            validate(&doc_agent_no_model, Definition::Envelope, &test_zone_table),
            Err(Rejection {
                reason: Reason::SchemaInvalid,
                paths: vec!["/agent/model".to_string()],
            })
        );

        let mut doc_model_empty = base.clone();
        doc_model_empty["agent"]["model"] = serde_json::json!("");
        assert_eq!(
            validate(&doc_model_empty, Definition::Envelope, &test_zone_table),
            Err(Rejection {
                reason: Reason::SchemaInvalid,
                paths: vec!["/agent/model".to_string()],
            })
        );

        let mut doc_sess_empty = base.clone();
        doc_sess_empty["session_id"] = serde_json::json!("");
        assert_eq!(
            validate(&doc_sess_empty, Definition::Envelope, &test_zone_table),
            Err(Rejection {
                reason: Reason::SchemaInvalid,
                paths: vec!["/session_id".to_string()],
            })
        );

        let mut doc_plat_x = base.clone();
        doc_plat_x["platform"] = serde_json::json!("x");
        assert_eq!(
            validate(&doc_plat_x, Definition::Envelope, &test_zone_table),
            Err(Rejection {
                reason: Reason::SchemaInvalid,
                paths: vec!["/platform".to_string()],
            })
        );

        let mut doc_schema_v2 = base.clone();
        doc_schema_v2["schema"] = serde_json::json!("v2");
        assert_eq!(
            validate(&doc_schema_v2, Definition::Envelope, &test_zone_table),
            Err(Rejection {
                reason: Reason::SchemaInvalid,
                paths: vec!["/schema".to_string()],
            })
        );

        let mut doc_user_id_short = base.clone();
        doc_user_id_short["user"]["id"] = serde_json::json!("hmac-sha256:abc");
        assert_eq!(
            validate(&doc_user_id_short, Definition::Envelope, &test_zone_table),
            Err(Rejection {
                reason: Reason::SchemaInvalid,
                paths: vec!["/user/id".to_string()],
            })
        );

        let mut doc_tz_empty = base.clone();
        doc_tz_empty["user"]["timezone"] = serde_json::json!("");
        assert_eq!(
            validate(&doc_tz_empty, Definition::Envelope, &test_zone_table),
            Err(Rejection {
                reason: Reason::SchemaInvalid,
                paths: vec!["/user/timezone".to_string()],
            })
        );

        let mut doc_agent_5 = base.clone();
        doc_agent_5["agent"] = serde_json::json!(5);
        assert_eq!(
            validate(&doc_agent_5, Definition::Envelope, &test_zone_table),
            Err(Rejection {
                reason: Reason::SchemaInvalid,
                paths: vec!["/agent".to_string()],
            })
        );

        let doc_arr = serde_json::json!([]);
        assert_eq!(
            validate(&doc_arr, Definition::Envelope, &test_zone_table),
            Err(Rejection {
                reason: Reason::SchemaInvalid,
                paths: vec![String::new()],
            })
        );

        let doc_null = serde_json::Value::Null;
        assert_eq!(
            validate(&doc_null, Definition::Envelope, &test_zone_table),
            Err(Rejection {
                reason: Reason::SchemaInvalid,
                paths: vec![String::new()],
            })
        );

        let mut doc_ts_no_millis = base;
        doc_ts_no_millis["ts"] = serde_json::json!("2026-10-16T02:00:00Z");
        assert_eq!(
            validate(&doc_ts_no_millis, Definition::Envelope, &test_zone_table),
            Err(Rejection {
                reason: Reason::SchemaInvalid,
                paths: vec!["/ts".to_string()],
            })
        );
    }

    // 5. FleetLedgerV1: {"v":1,"ts":0,"event":"e","task":"t"} accepted; extra key accepted;
    //    v 1.0 accepted; v 2 and v true -> ["/v"]; task removed -> ["/task"];
    //    ts -1 -> ["/ts"]; event "" -> ["/event"]; [] -> [""];
    //    an envelope with user.id null is NOT identity-checked here (fails only on the required members).
    #[test]
    fn test_fleet_ledger_v1() {
        let base_ledger = serde_json::json!({
            "v": 1,
            "ts": 0,
            "event": "e",
            "task": "t"
        });
        assert_eq!(
            validate(&base_ledger, Definition::FleetLedgerV1, &test_zone_table),
            Ok(())
        );

        let mut ledger_extra = base_ledger.clone();
        ledger_extra["extra"] = serde_json::json!("hello");
        assert_eq!(
            validate(&ledger_extra, Definition::FleetLedgerV1, &test_zone_table),
            Ok(())
        );

        let mut ledger_f = base_ledger.clone();
        ledger_f["v"] = serde_json::json!(1.0);
        assert_eq!(
            validate(&ledger_f, Definition::FleetLedgerV1, &test_zone_table),
            Ok(())
        );

        for bad_v in [serde_json::json!(2), serde_json::json!(true)] {
            let mut d = base_ledger.clone();
            d["v"] = bad_v;
            assert_eq!(
                validate(&d, Definition::FleetLedgerV1, &test_zone_table),
                Err(Rejection {
                    reason: Reason::SchemaInvalid,
                    paths: vec!["/v".to_string()],
                })
            );
        }

        let mut ledger_no_task = base_ledger.clone();
        ledger_no_task.as_object_mut().unwrap().remove("task");
        assert_eq!(
            validate(&ledger_no_task, Definition::FleetLedgerV1, &test_zone_table),
            Err(Rejection {
                reason: Reason::SchemaInvalid,
                paths: vec!["/task".to_string()],
            })
        );

        let mut ledger_neg_ts = base_ledger.clone();
        ledger_neg_ts["ts"] = serde_json::json!(-1);
        assert_eq!(
            validate(&ledger_neg_ts, Definition::FleetLedgerV1, &test_zone_table),
            Err(Rejection {
                reason: Reason::SchemaInvalid,
                paths: vec!["/ts".to_string()],
            })
        );

        let mut ledger_empty_event = base_ledger;
        ledger_empty_event["event"] = serde_json::json!("");
        assert_eq!(
            validate(
                &ledger_empty_event,
                Definition::FleetLedgerV1,
                &test_zone_table
            ),
            Err(Rejection {
                reason: Reason::SchemaInvalid,
                paths: vec!["/event".to_string()],
            })
        );

        let ledger_arr = serde_json::json!([]);
        assert_eq!(
            validate(&ledger_arr, Definition::FleetLedgerV1, &test_zone_table),
            Err(Rejection {
                reason: Reason::SchemaInvalid,
                paths: vec![String::new()],
            })
        );

        let mut envelope_null_id = base_doc();
        envelope_null_id["user"]["id"] = serde_json::Value::Null;
        let res = validate(
            &envelope_null_id,
            Definition::FleetLedgerV1,
            &test_zone_table,
        );
        match res {
            Err(Rejection { reason, paths }) => {
                assert_eq!(reason, Reason::SchemaInvalid);
                assert!(!paths.contains(&"/user/id".to_string()));
            }
            Ok(()) => panic!("expected rejection for envelope under fleet ledger"),
        }
    }

    // 6. Reason::as_str strings exact.
    #[test]
    fn test_reason_as_str() {
        assert_eq!(Reason::IdentityUnresolved.as_str(), "identity_unresolved");
        assert_eq!(Reason::SchemaInvalid.as_str(), "schema_invalid");
        assert_eq!(Reason::DayKeyMismatch.as_str(), "day_key_mismatch");
    }
}
