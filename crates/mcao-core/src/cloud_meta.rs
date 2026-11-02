//! Cloud fields of a task metadata record (DPES-882).
//!
//! A `state/<id>.meta` record is read through [`TaskMeta`] and its execution target through
//! [`ExecutionTarget`]; this module adds only what those types lack: the four `cloud_*` keys of
//! a `gcp` record, checked to the shape the shell's `fm_exec_record_cloud_ref_valid` accepts.
//!
//! Writing edits the raw record text rather than rebuilding it from pairs, as the [`TaskMeta`]
//! documentation requires. Lines the module does not own, including comments, a leading byte
//! order mark and line endings, pass through unchanged, so a record the shell reads is not
//! altered beyond the five keys set here.
//!
//! None of these functions refuse an invalid or repeated `execution_target` by turning it into
//! `local`: a record that does not name one target is an error, never a guess.

use std::error::Error;
use std::fmt;

use crate::state::encoding::{EXECUTION_TARGET_KEY, strip_bom};
use crate::state::{ExecutionTarget, TaskMeta};

/// Key of the cloud run identifier.
const RUN_ID_KEY: &str = "cloud_run_id";
/// Key of the cloud request identifier.
const REQUEST_ID_KEY: &str = "cloud_request_id";
/// Key of the cloud API base URL.
const API_BASE_KEY: &str = "cloud_api_base";
/// Key of the cloud task owner.
const OWNER_KEY: &str = "cloud_owner";

/// Maximum number of characters of untrusted input an error message echoes.
const MAX_ECHO_CHARS: usize = 40;

/// A cloud field that is missing, repeated or malformed, or an ambiguous execution target.
///
/// Variants that hold untrusted text keep it whole. [`fmt::Display`] truncates and escapes it, so
/// a message is bounded and cannot forge a log line or carry a terminal escape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CloudMetaError {
    /// The value of `field` is empty.
    Empty {
        /// The key whose value is empty.
        field: &'static str,
    },
    /// The value of `field` is not a non-empty run of `[A-Za-z0-9._-]`, or is `.` or `..`.
    BadId {
        /// The key whose value is malformed.
        field: &'static str,
        /// The offending value.
        value: String,
    },
    /// The value of `field` contains `\n`, `\r` or NUL, which a one-line record cannot carry.
    BadValue {
        /// The key whose value is malformed.
        field: &'static str,
        /// The offending value.
        value: String,
    },
    /// `key` appears on more than one line, so the record does not say which value is true.
    Ambiguous {
        /// The repeated key.
        key: &'static str,
    },
    /// A `gcp` record has no `key` line.
    Missing {
        /// The absent key.
        key: &'static str,
    },
    /// The `execution_target` value is neither `local` nor `gcp`, including an empty one.
    BadTarget(String),
}

impl fmt::Display for CloudMetaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty { field } => write!(f, "{field} is empty"),
            Self::BadId { field, value } => write!(
                f,
                "{field} must be a non-empty [A-Za-z0-9._-] identifier, got {}",
                echo(value)
            ),
            Self::BadValue { field, value } => write!(
                f,
                "{field} contains a line break or NUL, got {}",
                echo(value)
            ),
            Self::Ambiguous { key } => write!(f, "{key} appears more than once"),
            Self::Missing { key } => write!(f, "{key} is missing"),
            Self::BadTarget(value) => write!(f, "unknown execution target {}", echo(value)),
        }
    }
}

impl Error for CloudMetaError {}

/// Quotes `value` for an error message: at most [`MAX_ECHO_CHARS`] characters, each escaped with
/// [`char::escape_debug`], and `...` after a truncated value.
fn echo(value: &str) -> String {
    let mut chars = value.chars();
    let mut out = String::from("\"");
    for c in chars.by_ref().take(MAX_ECHO_CHARS) {
        out.extend(c.escape_debug());
    }
    out.push('"');
    if chars.next().is_some() {
        out.push_str("...");
    }
    out
}

/// Checks a cloud run or request identifier: non-empty `[A-Za-z0-9._-]+`, as the shell checks,
/// and not `.` or `..`, because the identifier becomes a path component later.
fn check_id(field: &'static str, value: &str) -> Result<(), CloudMetaError> {
    if value.is_empty() {
        return Err(CloudMetaError::Empty { field });
    }
    let plain = value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if !plain || value == "." || value == ".." {
        return Err(CloudMetaError::BadId {
            field,
            value: value.to_owned(),
        });
    }
    Ok(())
}

/// Checks a free-text cloud value: non-empty and free of `\n`, `\r` and NUL.
fn check_text(field: &'static str, value: &str) -> Result<(), CloudMetaError> {
    if value.is_empty() {
        return Err(CloudMetaError::Empty { field });
    }
    if value.contains(['\n', '\r', '\0']) {
        return Err(CloudMetaError::BadValue {
            field,
            value: value.to_owned(),
        });
    }
    Ok(())
}

/// The four cloud fields of a `gcp` record, each checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloudFields {
    run_id: String,
    request_id: String,
    api_base: String,
    owner: String,
}

impl CloudFields {
    /// Checks and keeps the four cloud fields.
    ///
    /// `run_id` and `request_id` must be non-empty `[A-Za-z0-9._-]+` and not `.` or `..`.
    /// `api_base` and `owner` must be non-empty and free of `\n`, `\r` and NUL.
    ///
    /// # Errors
    ///
    /// Returns the first failing field's [`CloudMetaError`], checked in the order of the
    /// parameters.
    pub fn new(
        run_id: &str,
        request_id: &str,
        api_base: &str,
        owner: &str,
    ) -> Result<Self, CloudMetaError> {
        check_id(RUN_ID_KEY, run_id)?;
        check_id(REQUEST_ID_KEY, request_id)?;
        check_text(API_BASE_KEY, api_base)?;
        check_text(OWNER_KEY, owner)?;
        Ok(Self {
            run_id: run_id.to_owned(),
            request_id: request_id.to_owned(),
            api_base: api_base.to_owned(),
            owner: owner.to_owned(),
        })
    }

    /// Reads the cloud fields of `meta`.
    ///
    /// Returns `Ok(None)` for a `local` record, whatever other keys it carries. A `gcp` record
    /// must carry each of the four keys exactly once with a valid value.
    ///
    /// # Errors
    ///
    /// Returns [`CloudMetaError::Ambiguous`] when `execution_target` or a cloud key repeats,
    /// [`CloudMetaError::BadTarget`] for a target other than `local` or `gcp`, including an empty
    /// one, [`CloudMetaError::Missing`] for an absent cloud key, and the error of
    /// [`CloudFields::new`] for an invalid value. An invalid target is never read as `local`.
    pub fn read(meta: &TaskMeta) -> Result<Option<Self>, CloudMetaError> {
        let target =
            exact(meta, EXECUTION_TARGET_KEY)?.map_or(Ok(ExecutionTarget::Local), |value| {
                ExecutionTarget::parse(value)
                    .map_err(|_| CloudMetaError::BadTarget(value.to_owned()))
            })?;
        if target == ExecutionTarget::Local {
            return Ok(None);
        }
        let run_id = required(meta, RUN_ID_KEY)?;
        let request_id = required(meta, REQUEST_ID_KEY)?;
        let api_base = required(meta, API_BASE_KEY)?;
        let owner = required(meta, OWNER_KEY)?;
        Self::new(run_id, request_id, api_base, owner).map(Some)
    }

    /// Returns the cloud run identifier.
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Returns the cloud request identifier.
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    /// Returns the cloud API base URL.
    pub fn api_base(&self) -> &str {
        &self.api_base
    }

    /// Returns the cloud task owner.
    pub fn owner(&self) -> &str {
        &self.owner
    }
}

/// Returns the single value of `key` in `meta`, or `None` when no line has it.
fn exact<'a>(meta: &'a TaskMeta, key: &'static str) -> Result<Option<&'a str>, CloudMetaError> {
    meta.exact(key)
        .map_err(|_| CloudMetaError::Ambiguous { key })
}

/// Returns the single value of `key` in `meta`, which a `gcp` record must carry.
fn required<'a>(meta: &'a TaskMeta, key: &'static str) -> Result<&'a str, CloudMetaError> {
    exact(meta, key)?.ok_or(CloudMetaError::Missing { key })
}

/// Returns `text` with `execution_target=gcp` and the four cloud keys set to the given values.
///
/// All four values are checked before `text` is touched, so an error leaves nothing half
/// written. A key already present has its first line's value replaced in place; an absent key is
/// appended as `key=value\n`. Every other line, a leading byte order mark and each line's own
/// ending are kept.
///
/// # Errors
///
/// Returns the error of [`CloudFields::new`] for an invalid value, and
/// [`CloudMetaError::Ambiguous`] when `text` already repeats one of the five keys, because the
/// record then does not say which line is true. The existing `execution_target` value is
/// replaced, not validated.
pub fn set_cloud_fields(
    text: &str,
    run_id: &str,
    request_id: &str,
    api_base: &str,
    owner: &str,
) -> Result<String, CloudMetaError> {
    let fields = CloudFields::new(run_id, request_id, api_base, owner)?;
    let pairs = [
        (EXECUTION_TARGET_KEY, ExecutionTarget::Gcp.as_str()),
        (RUN_ID_KEY, fields.run_id()),
        (REQUEST_ID_KEY, fields.request_id()),
        (API_BASE_KEY, fields.api_base()),
        (OWNER_KEY, fields.owner()),
    ];
    let body = strip_bom(text);
    let bom = &text[..text.len() - body.len()];

    for (key, _) in pairs {
        if body
            .lines()
            .filter(|line| line_key(line) == Some(key))
            .count()
            > 1
        {
            return Err(CloudMetaError::Ambiguous { key });
        }
    }

    let mut out = String::from(bom);
    let mut written = [false; 5];
    for line in body.split_inclusive('\n') {
        let slot = pairs
            .iter()
            .position(|(key, _)| line_key(line) == Some(*key));
        match slot {
            Some(slot) => {
                let (key, value) = pairs[slot];
                out.push_str(key);
                out.push('=');
                out.push_str(value);
                out.push_str(line_ending(line));
                written[slot] = true;
            }
            None => out.push_str(line),
        }
    }
    for (slot, (key, value)) in pairs.iter().enumerate() {
        if !written[slot] {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(key);
            out.push('=');
            out.push_str(value);
            out.push('\n');
        }
    }
    Ok(out)
}

/// Returns the key of a `key=value` line, or `None` for a line that carries no pair, which
/// [`TaskMeta::parse`] ignores too.
fn line_key(line: &str) -> Option<&str> {
    let (key, _) = line.split_once('=')?;
    (!key.is_empty()).then_some(key)
}

/// Returns the line ending of `line` as [`TaskMeta::parse`] reads it: `\r\n`, `\n`, or none.
fn line_ending(line: &str) -> &str {
    if line.ends_with("\r\n") {
        "\r\n"
    } else if line.ends_with('\n') {
        "\n"
    } else {
        ""
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::TaskId;

    fn meta(text: &str) -> TaskMeta {
        TaskMeta::parse(TaskId::parse("t1").expect("task id"), text)
    }

    const GOOD: &str = "execution_target=gcp\ncloud_run_id=run-1\ncloud_request_id=req.2_x\n\
                        cloud_api_base=https://api.example/v1\ncloud_owner=dev user\n";

    fn read(text: &str) -> Result<Option<CloudFields>, CloudMetaError> {
        CloudFields::read(&meta(text))
    }

    #[test]
    fn read_gcp_record_returns_every_field() {
        let fields = read(GOOD).expect("read").expect("cloud");
        assert_eq!(fields.run_id(), "run-1");
        assert_eq!(fields.request_id(), "req.2_x");
        assert_eq!(fields.api_base(), "https://api.example/v1");
        assert_eq!(fields.owner(), "dev user");
    }

    #[test]
    fn read_keeps_leading_and_trailing_whitespace_in_values() {
        let text = GOOD.replace("dev user", "  dev user  ");
        let fields = read(&text).expect("read").expect("cloud");
        assert_eq!(fields.owner(), "  dev user  ");
    }

    #[test]
    fn read_local_record_has_no_cloud_fields_even_with_cloud_keys() {
        assert_eq!(read(""), Ok(None));
        assert_eq!(read("execution_target=local\n"), Ok(None));
        assert_eq!(read("cloud_run_id=\ncloud_run_id=\n"), Ok(None));
    }

    #[test]
    fn read_refuses_invalid_target_instead_of_reading_local() {
        for target in ["GCP", "aws", "", " gcp"] {
            let text = format!("execution_target={target}\n");
            assert_eq!(
                read(&text),
                Err(CloudMetaError::BadTarget(target.to_owned())),
                "target {target:?}"
            );
        }
    }

    #[test]
    fn read_refuses_repeated_execution_target() {
        assert_eq!(
            read("execution_target=gcp\nexecution_target=local\n"),
            Err(CloudMetaError::Ambiguous {
                key: "execution_target"
            })
        );
        assert_eq!(
            read("execution_target=local\nexecution_target=local\n"),
            Err(CloudMetaError::Ambiguous {
                key: "execution_target"
            })
        );
    }

    #[test]
    fn read_refuses_repeated_cloud_keys() {
        for key in [RUN_ID_KEY, REQUEST_ID_KEY, API_BASE_KEY, OWNER_KEY] {
            let text = format!("{GOOD}{key}=other\n");
            assert_eq!(read(&text), Err(CloudMetaError::Ambiguous { key }));
        }
    }

    #[test]
    fn read_refuses_missing_cloud_key() {
        for key in [RUN_ID_KEY, REQUEST_ID_KEY, API_BASE_KEY, OWNER_KEY] {
            let text: String = GOOD
                .lines()
                .filter(|line| !line.starts_with(key))
                .map(|line| format!("{line}\n"))
                .collect();
            assert_eq!(read(&text), Err(CloudMetaError::Missing { key }));
        }
    }

    #[test]
    fn read_refuses_empty_cloud_values() {
        let text = GOOD.replace("run-1", "");
        assert_eq!(
            read(&text),
            Err(CloudMetaError::Empty { field: RUN_ID_KEY })
        );
        let text = GOOD.replace("dev user", "");
        assert_eq!(read(&text), Err(CloudMetaError::Empty { field: OWNER_KEY }));
    }

    #[test]
    fn read_parses_like_state_meta_with_bom_and_non_pair_lines() {
        let text = format!("\u{feff}{GOOD}no pair here\n=empty key\n# comment\n");
        let fields = read(&text).expect("read").expect("cloud");
        assert_eq!(fields.run_id(), "run-1");

        let crlf = GOOD.replace('\n', "\r\n");
        let fields = read(&crlf).expect("read").expect("cloud");
        assert_eq!(fields.owner(), "dev user");
    }

    #[test]
    fn read_refuses_interior_and_trailing_carriage_return() {
        let interior = GOOD.replace("dev user", "dev\ruser");
        assert!(matches!(
            read(&interior),
            Err(CloudMetaError::BadValue {
                field: OWNER_KEY,
                ..
            })
        ));
        // A final line without `\n` keeps its `\r`, which is part of the value.
        let trailing = format!("{}\r", GOOD.trim_end_matches('\n'));
        assert!(matches!(
            read(&trailing),
            Err(CloudMetaError::BadValue {
                field: OWNER_KEY,
                ..
            })
        ));
        let unterminated = "execution_target=gcp\r";
        assert_eq!(
            read(unterminated),
            Err(CloudMetaError::BadTarget("gcp\r".to_owned()))
        );
    }

    #[test]
    fn new_checks_each_field() {
        assert!(CloudFields::new("r", "q", "https://a", "o").is_ok());
        assert_eq!(
            CloudFields::new("", "q", "a", "o"),
            Err(CloudMetaError::Empty { field: RUN_ID_KEY })
        );
        assert_eq!(
            CloudFields::new("r", "", "a", "o"),
            Err(CloudMetaError::Empty {
                field: REQUEST_ID_KEY
            })
        );
        assert_eq!(
            CloudFields::new("r", "q", "", "o"),
            Err(CloudMetaError::Empty {
                field: API_BASE_KEY
            })
        );
        assert_eq!(
            CloudFields::new("r", "q", "a", ""),
            Err(CloudMetaError::Empty { field: OWNER_KEY })
        );
    }

    #[test]
    fn new_refuses_identifiers_outside_the_shell_shape() {
        for bad in [
            "a b",
            "a/b",
            "a\\b",
            "..",
            ".",
            "a\n",
            "\u{1b}[31m",
            "é",
            "a=b",
        ] {
            for (run, request) in [(bad, "q"), ("r", bad)] {
                assert!(
                    matches!(
                        CloudFields::new(run, request, "a", "o"),
                        Err(CloudMetaError::BadId { .. })
                    ),
                    "id {bad:?}"
                );
            }
        }
        assert!(CloudFields::new("A-z_0.9", "..a", "a", "o").is_ok());
    }

    #[test]
    fn new_refuses_line_breaks_and_nul_in_text_fields() {
        for bad in ["a\nb", "a\rb", "a\0b"] {
            assert!(matches!(
                CloudFields::new("r", "q", bad, "o"),
                Err(CloudMetaError::BadValue {
                    field: API_BASE_KEY,
                    ..
                })
            ));
            assert!(matches!(
                CloudFields::new("r", "q", "a", bad),
                Err(CloudMetaError::BadValue {
                    field: OWNER_KEY,
                    ..
                })
            ));
        }
    }

    #[test]
    fn set_cloud_fields_appends_to_empty_and_reads_back() {
        let text = set_cloud_fields("", "run-1", "req.2_x", "https://api.example/v1", "dev user")
            .expect("set");
        assert_eq!(text, GOOD);
        assert!(read(&text).expect("read").is_some());
    }

    #[test]
    fn set_cloud_fields_replaces_in_place_and_keeps_other_lines() {
        let before = "\u{feff}# note\r\nbackend=tmux\r\nexecution_target=local\r\nkind=ship\r\n\
                      cloud_owner=old\r\nno pair\r\nworktree=/w";
        let after = set_cloud_fields(before, "run-1", "req.2_x", "https://api.example/v1", "new")
            .expect("set");
        assert_eq!(
            after,
            "\u{feff}# note\r\nbackend=tmux\r\nexecution_target=gcp\r\nkind=ship\r\n\
             cloud_owner=new\r\nno pair\r\nworktree=/w\n\
             cloud_run_id=run-1\ncloud_request_id=req.2_x\n\
             cloud_api_base=https://api.example/v1\n"
        );
        let fields = read(&after).expect("read").expect("cloud");
        assert_eq!(fields.owner(), "new");
    }

    #[test]
    fn set_cloud_fields_refuses_invalid_input_before_any_change() {
        // The last argument is the invalid one: nothing from the earlier ones is applied.
        assert_eq!(
            set_cloud_fields("kind=ship\n", "r", "q", "a", "o\n"),
            Err(CloudMetaError::BadValue {
                field: OWNER_KEY,
                value: "o\n".to_owned()
            })
        );
        assert_eq!(
            set_cloud_fields("", "", "", "", ""),
            Err(CloudMetaError::Empty { field: RUN_ID_KEY })
        );
    }

    #[test]
    fn set_cloud_fields_checks_owner_when_earlier_arguments_are_valid() {
        assert_eq!(
            set_cloud_fields("", "r", "q", "a", ""),
            Err(CloudMetaError::Empty { field: OWNER_KEY })
        );
    }

    #[test]
    fn set_cloud_fields_refuses_repeated_keys() {
        for key in [
            EXECUTION_TARGET_KEY,
            RUN_ID_KEY,
            REQUEST_ID_KEY,
            API_BASE_KEY,
            OWNER_KEY,
        ] {
            let before = format!("{key}=a\nkind=ship\n{key}=b\n");
            assert_eq!(
                set_cloud_fields(&before, "r", "q", "a", "o"),
                Err(CloudMetaError::Ambiguous { key })
            );
        }
    }

    #[test]
    fn display_truncates_and_escapes_every_untrusted_variant() {
        let long = format!("\u{1b}[31m\r\n{}", "x".repeat(5000));
        let errors = [
            CloudMetaError::BadId {
                field: RUN_ID_KEY,
                value: long.clone(),
            },
            CloudMetaError::BadValue {
                field: OWNER_KEY,
                value: long.clone(),
            },
            CloudMetaError::BadTarget(long),
        ];
        for err in errors {
            let msg = err.to_string();
            assert!(msg.len() < 200, "{msg}");
            assert!(!msg.contains(|c: char| c.is_control()), "{msg:?}");
            assert!(msg.contains("\\u{1b}[31m\\r\\n"), "{msg}");
            assert!(msg.ends_with("\"..."), "{msg}");
        }
    }

    #[test]
    fn display_does_not_mark_a_value_at_the_limit_as_truncated() {
        let exact = "y".repeat(MAX_ECHO_CHARS);
        let msg = CloudMetaError::BadTarget(exact.clone()).to_string();
        assert!(msg.ends_with(&format!("\"{exact}\"")), "{msg}");
    }

    #[test]
    fn display_of_fixed_variants() {
        assert_eq!(
            CloudMetaError::Empty { field: RUN_ID_KEY }.to_string(),
            "cloud_run_id is empty"
        );
        assert_eq!(
            CloudMetaError::Ambiguous { key: OWNER_KEY }.to_string(),
            "cloud_owner appears more than once"
        );
        assert_eq!(
            CloudMetaError::Missing { key: API_BASE_KEY }.to_string(),
            "cloud_api_base is missing"
        );
        assert_eq!(
            CloudMetaError::BadTarget("azure".to_owned()).to_string(),
            "unknown execution target \"azure\""
        );
        let err: &dyn Error = &CloudMetaError::BadTarget(String::new());
        assert!(err.source().is_none());
    }
}
