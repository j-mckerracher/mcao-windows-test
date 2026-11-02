//! DPES-737 sanitization boundary for audit events.
//!
//! Ports `Sanitizer` and the config loader of `bin/fm-audit-sanitize.py`. The boundary runs over
//! one event before it is serialized, spooled, logged, or shipped. It redacts secret and PII
//! shapes in every string, cuts each string to [`TRUNCATE_CHARS`] characters, and drops every
//! member it cannot carry. A dropped member is reported by JSON Pointer in the dropped list and
//! its value is never kept. The boundary writes nothing.
//!
//! The order is the reference's: token shapes (`audit_redact_tokens`), then PII and PHI
//! (`audit_redact_pii`), then truncation. Typed `$kind` objects keep only digests, the
//! `path` and `destination` summaries, and their reviewed member sets.
//!
//! # Known divergences from the reference
//!
//! - `shell_command` argv is digested without NFC normalization. The workspace has no Unicode
//!   normalization table, so an argv that is not already NFC digests differently. No projected
//!   `audit.activity` event carries a typed object, so `activity-map` never reaches this path.
//! - The reference's `EXECUTABLE_ALLOWLIST` and `TOOL_SCALAR_ALLOWLIST` are not ported. Both
//!   are empty and the config cannot extend them, so no output depends on them.
//! - Rust strings and object keys cannot hold lone surrogates, and the JSON parser refuses lone
//!   surrogate escapes before they reach this module. The reference's encoding drop and its
//!   `[unparseable-key]` pointer are therefore unreachable here.

use serde_json::{Map, Value, json};

use crate::audit_hmac;
use crate::audit_path::{
    TRUNCATE_CHARS, pointer_token, posix_basename, repo_relative, truncate_chars, url_scheme_host,
};
use crate::audit_redact_pii::redact_pii;
use crate::audit_redact_tokens::redact_tokens;

/// The member that marks a typed object.
const KIND_MEMBER: &str = "$kind";

/// The only member of the closed sanitizer config.
const CONFIG_KEY: &str = "home_key";

/// Member names whose values are never kept, whatever their type or content.
const DROPPED_MEMBERS: [&str; 7] = [
    "prompt",
    "model_output",
    "retry_body",
    "exception_body",
    "env",
    "environment",
    "file_contents",
];

/// The members each typed object kind may carry besides its `$kind` marker, or `None` for an
/// unknown kind.
fn typed_members(kind: &str) -> Option<&'static [&'static str]> {
    match kind {
        "shell_command" => Some(&["argv"]),
        "tool_call" => Some(&["tool", "args"]),
        "path" => Some(&["path", "root"]),
        "destination" => Some(&["url"]),
        _ => None,
    }
}

/// Why the sanitizer config was refused. `paths` names members by JSON Pointer, never by value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigRefusal {
    /// The offending JSON Pointers; the empty pointer names the whole config document.
    pub paths: Vec<String>,
}

/// The event handed to the boundary is not a JSON object, or it carries the `$kind` marker that
/// only typed objects may hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventRefused;

/// Marks a member or typed object the boundary drops; the caller records its pointer.
struct FieldDrop;

/// Returns the operational home key from a parsed closed config.
///
/// The config is an object whose only member is `home_key`, a 64-character lowercase hex string.
/// Any other member refuses the whole document at the empty pointer; a missing or malformed key
/// refuses `/home_key`.
///
/// # Errors
///
/// Returns [`ConfigRefusal`] with the pointers described above.
pub fn home_key_from_config(doc: &Value) -> Result<[u8; 32], ConfigRefusal> {
    let Some(object) = doc.as_object() else {
        return Err(ConfigRefusal {
            paths: vec![String::new()],
        });
    };
    if object.keys().any(|key| key != CONFIG_KEY) {
        return Err(ConfigRefusal {
            paths: vec![String::new()],
        });
    }
    object
        .get(CONFIG_KEY)
        .and_then(Value::as_str)
        .and_then(audit_hmac::parse_home_key)
        .ok_or_else(|| ConfigRefusal {
            paths: vec![format!("/{CONFIG_KEY}")],
        })
}

/// Sanitizes one event under `home_key`.
///
/// Returns the sanitized event and the JSON Pointers of every member the boundary dropped, in
/// traversal order. The caller decides whether a non-empty dropped list refuses the event.
///
/// # Errors
///
/// Returns [`EventRefused`] when `event` is not an object or carries a `$kind` member.
pub fn sanitize_event(
    event: &Value,
    home_key: &[u8],
) -> Result<(Value, Vec<String>), EventRefused> {
    let mut sanitizer = Sanitizer {
        home_key,
        dropped: Vec::new(),
    };
    let sanitized = sanitizer.event(event)?;
    Ok((sanitized, sanitizer.dropped))
}

/// Sanitizes one event and collects the pointers of the members it drops.
struct Sanitizer<'a> {
    home_key: &'a [u8],
    dropped: Vec<String>,
}

impl Sanitizer<'_> {
    fn event(&mut self, event: &Value) -> Result<Value, EventRefused> {
        match event {
            Value::Object(node) if !node.contains_key(KIND_MEMBER) => {
                Ok(Value::Object(self.mapping(node, "")))
            }
            _ => Err(EventRefused),
        }
    }

    /// Returns the sanitized value, or `None` after recording `pointer` as dropped.
    fn value(&mut self, value: &Value, pointer: &str) -> Option<Value> {
        let kept = match value {
            Value::Object(node) if node.contains_key(KIND_MEMBER) => self.typed(node),
            Value::Object(node) => Ok(Value::Object(self.mapping(node, pointer))),
            Value::Array(items) => Ok(Value::Array(self.sequence(items, pointer))),
            Value::String(text) => Ok(Value::String(sanitize_text(text))),
            other => Ok(other.clone()),
        };
        match kept {
            Ok(kept) => Some(kept),
            Err(FieldDrop) => {
                self.dropped.push(pointer.to_string());
                None
            }
        }
    }

    fn mapping(&mut self, node: &Map<String, Value>, pointer: &str) -> Map<String, Value> {
        let mut out = Map::new();
        for (key, member) in node {
            let name = sanitize_text(key);
            let member_pointer = format!("{pointer}/{}", pointer_token(&name));
            // A key that collides after redaction drops like a value, as the reference does.
            if out.contains_key(&name) || DROPPED_MEMBERS.contains(&key.as_str()) {
                self.dropped.push(member_pointer);
                continue;
            }
            if let Some(kept) = self.value(member, &member_pointer) {
                out.insert(name, kept);
            }
        }
        out
    }

    fn sequence(&mut self, items: &[Value], pointer: &str) -> Vec<Value> {
        let mut out = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            if let Some(kept) = self.value(item, &format!("{pointer}/{index}")) {
                out.push(kept);
            }
        }
        out
    }

    fn typed(&self, node: &Map<String, Value>) -> Result<Value, FieldDrop> {
        let kind = node
            .get(KIND_MEMBER)
            .and_then(Value::as_str)
            .ok_or(FieldDrop)?;
        let members = typed_members(kind).ok_or(FieldDrop)?;
        if node
            .keys()
            .any(|key| key != KIND_MEMBER && !members.contains(&key.as_str()))
        {
            return Err(FieldDrop);
        }
        match kind {
            "shell_command" => self.shell_command(node),
            "tool_call" => self.tool_call(node),
            "path" => self.path(node),
            _ => self.destination(node),
        }
    }

    fn shell_command(&self, node: &Map<String, Value>) -> Result<Value, FieldDrop> {
        let argv = node
            .get("argv")
            .and_then(Value::as_array)
            .filter(|argv| !argv.is_empty())
            .ok_or(FieldDrop)?;
        if !argv.iter().all(Value::is_string) {
            return Err(FieldDrop);
        }
        Ok(json!({"argv_digest": self.digest(&Value::Array(argv.clone()))?}))
    }

    fn tool_call(&self, node: &Map<String, Value>) -> Result<Value, FieldDrop> {
        let tool = node.get("tool").and_then(Value::as_str).ok_or(FieldDrop)?;
        let args = node
            .get("args")
            .filter(|args| args.is_object())
            .ok_or(FieldDrop)?;
        Ok(json!({
            "tool": sanitize_text(tool),
            "args_digest": self.digest(args)?,
        }))
    }

    fn path(&self, node: &Map<String, Value>) -> Result<Value, FieldDrop> {
        let raw = node.get("path").and_then(Value::as_str).ok_or(FieldDrop)?;
        let root = match node.get("root") {
            None | Some(Value::Null) => None,
            Some(Value::String(root)) if root.starts_with('/') => Some(root.as_str()),
            Some(_) => return Err(FieldDrop),
        };
        match repo_relative(raw, root) {
            Some(relative) => Ok(json!({"repo_path": sanitize_text(&relative)})),
            None => {
                let basename = sanitize_text(posix_basename(raw.trim_end_matches('/')));
                Ok(json!({
                    "basename": format!("~/{basename}"),
                    "path_digest": self.digest(&Value::String(raw.to_string()))?,
                }))
            }
        }
    }

    fn destination(&self, node: &Map<String, Value>) -> Result<Value, FieldDrop> {
        let url = node.get("url").and_then(Value::as_str).ok_or(FieldDrop)?;
        let (scheme, host) = url_scheme_host(url).ok_or(FieldDrop)?;
        Ok(json!({
            "scheme": sanitize_text(&scheme),
            "host": sanitize_text(&host),
        }))
    }

    fn digest(&self, value: &Value) -> Result<String, FieldDrop> {
        audit_hmac::digest_value(self.home_key, value).map_err(|_| FieldDrop)
    }
}

/// Redacts secret and PII shapes in `text`, then cuts it to [`TRUNCATE_CHARS`] characters.
fn sanitize_text(text: &str) -> String {
    truncate_chars(&redact_pii(&redact_tokens(text)), TRUNCATE_CHARS)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = [0x5e; 32];

    fn sanitize(event: &Value) -> (Value, Vec<String>) {
        sanitize_event(event, &KEY).expect("test event must be an object without $kind")
    }

    #[test]
    fn token_shapes_are_redacted_with_their_labels() {
        let text = "tok ghp_SyntheticTokenAbcdefghij0123 and AKIAABCDEFGHIJKLMNOP";
        assert_eq!(
            sanitize_text(text),
            "tok [REDACTED:github_token] and [REDACTED:aws_access_key]"
        );
    }

    #[test]
    fn credential_assignment_is_redacted_before_truncation() {
        assert_eq!(
            sanitize_text("password=hunter2 trailing"),
            "[REDACTED:credential_assignment] trailing"
        );
    }

    #[test]
    fn pii_shapes_are_redacted_with_their_labels() {
        assert_eq!(
            sanitize_text("mail synthetic.patient@example.test now"),
            "mail [REDACTED:email] now"
        );
        assert_eq!(sanitize_text("ssn 123-45-6789"), "ssn [REDACTED:ssn]");
    }

    #[test]
    fn truncation_counts_unicode_scalar_values() {
        let exact: String = "é".repeat(TRUNCATE_CHARS);
        assert_eq!(sanitize_text(&exact), exact);

        let over: String = "é".repeat(TRUNCATE_CHARS + 1);
        assert_eq!(sanitize_text(&over), "é".repeat(TRUNCATE_CHARS));

        let emoji: String = "😀".repeat(TRUNCATE_CHARS + 2);
        assert_eq!(sanitize_text(&emoji).chars().count(), TRUNCATE_CHARS);
    }

    #[test]
    fn a_secret_straddling_the_cut_is_redacted_before_it_is_cut() {
        let prefix = "a".repeat(TRUNCATE_CHARS - 8);
        let text = format!("{prefix} ghp_SyntheticTokenAbcdefghij0123");
        let out = sanitize_text(&text);
        assert!(!out.contains("ghp_"), "{out}");
        assert_eq!(out.chars().count(), TRUNCATE_CHARS);
    }

    #[test]
    fn dropped_members_are_removed_and_listed_by_pointer() {
        let (out, dropped) = sanitize(&json!({"prompt": "secret", "kept": 1}));
        assert_eq!(out, json!({"kept": 1}));
        assert_eq!(dropped, vec!["/prompt"]);
    }

    #[test]
    fn nested_members_are_sanitized_and_arrays_keep_index_pointers() {
        let event = json!({"items": ["ok", "ghp_SyntheticTokenAbcdefghij0123"], "n": null});
        let (out, dropped) = sanitize(&event);
        assert_eq!(
            out,
            json!({"items": ["ok", "[REDACTED:github_token]"], "n": null})
        );
        assert!(dropped.is_empty());
    }

    #[test]
    fn a_typed_member_outside_its_set_is_dropped_at_its_pointer() {
        let event = json!({"x": {"$kind": "destination", "url": "https://h.example/", "extra": 1}});
        let (out, dropped) = sanitize(&event);
        assert_eq!(out, json!({}));
        assert_eq!(dropped, vec!["/x"]);
    }

    #[test]
    fn shell_command_keeps_only_a_digest_of_its_argv() {
        let argv = json!(["git", "status"]);
        let event = json!({"cmd": {"$kind": "shell_command", "argv": ["git", "status"]}});
        let (out, dropped) = sanitize(&event);
        let expected = audit_hmac::digest_value(&KEY, &argv).expect("digest");
        assert_eq!(out, json!({"cmd": {"argv_digest": expected}}));
        assert!(dropped.is_empty());
    }

    #[test]
    fn destination_keeps_scheme_and_host_only() {
        let event = json!({"u": {"$kind": "destination", "url": "https://user:pw@Example.test:8443/a?q=1"}});
        let (out, _) = sanitize(&event);
        assert_eq!(
            out,
            json!({"u": {"scheme": "https", "host": "example.test"}})
        );
    }

    #[test]
    fn path_is_repo_relative_inside_its_root_and_a_basename_outside_it() {
        let event = json!({
            "inside": {"$kind": "path", "path": "/w/repo/src/a.rs", "root": "/w/repo"},
            "outside": {"$kind": "path", "path": "/etc/secret/", "root": "/w/repo"}
        });
        let (out, dropped) = sanitize(&event);
        assert_eq!(out["inside"], json!({"repo_path": "src/a.rs"}));
        assert_eq!(out["outside"]["basename"], json!("~/secret"));
        assert!(
            out["outside"]["path_digest"]
                .as_str()
                .unwrap()
                .starts_with("hmac-sha256:")
        );
        assert!(dropped.is_empty());
    }

    #[test]
    fn a_non_object_event_or_one_with_kind_is_refused() {
        assert_eq!(sanitize_event(&json!([1]), &KEY), Err(EventRefused));
        assert_eq!(
            sanitize_event(&json!({"$kind": "path"}), &KEY),
            Err(EventRefused)
        );
    }

    #[test]
    fn config_accepts_one_valid_home_key_only() {
        let key = "5e".repeat(32);
        let ok = home_key_from_config(&json!({"home_key": key})).expect("valid config");
        assert_eq!(ok, [0x5e; 32]);

        assert_eq!(
            home_key_from_config(&json!({"home_key": key, "extra": 1})),
            Err(ConfigRefusal {
                paths: vec![String::new()]
            })
        );
        assert_eq!(
            home_key_from_config(&json!({"home_key": "AB".repeat(32)})),
            Err(ConfigRefusal {
                paths: vec!["/home_key".to_string()]
            })
        );
        assert_eq!(
            home_key_from_config(&json!({})),
            Err(ConfigRefusal {
                paths: vec!["/home_key".to_string()]
            })
        );
        assert_eq!(
            home_key_from_config(&json!([])),
            Err(ConfigRefusal {
                paths: vec![String::new()]
            })
        );
    }
}
