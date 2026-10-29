//! Session-start source extraction and routing logic (DPES-893).
//!
//! Provides decision logic ported from `bin/fm-sessionstart-run.sh`: parsing
//! script arguments, extracting the `source` field from hook payloads using
//! the shell awk contract, foreign-host detection, session start completion
//! checks, and source routing.

/// Arguments parsed from the session-start command line.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SessionStartArgs {
    /// Session-open source name, or empty if unspecified.
    pub source: String,
    /// Whether internal Pi prerequisite mode is enabled.
    pub pi_prerequisite: bool,
}

/// Parses session-start command line arguments from left to right.
///
/// `--source` takes the next argument as the source (or empty if it is the
/// last argument); `--source=VALUE` sets the source to `VALUE`;
/// `--pi-prerequisite` sets the prerequisite flag; any other argument is
/// skipped. A later `--source` overrides an earlier one.
pub fn parse_args(args: &[String]) -> SessionStartArgs {
    let mut source = String::new();
    let mut pi_prerequisite = false;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--source" {
            if i + 1 < args.len() {
                source = args[i + 1].clone();
                i += 2;
            } else {
                source = String::new();
                i += 1;
            }
        } else if let Some(val) = arg.strip_prefix("--source=") {
            source = val.to_string();
            i += 1;
        } else if arg == "--pi-prerequisite" {
            pi_prerequisite = true;
            i += 1;
        } else {
            i += 1;
        }
    }
    SessionStartArgs {
        source,
        pi_prerequisite,
    }
}

fn is_ascii_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r')
}

fn matches_colon_record(record: &str) -> bool {
    let mut colon_count = 0;
    for &b in record.as_bytes() {
        if b == b':' {
            colon_count += 1;
        } else if !is_ascii_ws(b) {
            return false;
        }
    }
    colon_count == 1
}

/// Extracts the session-start source from a hook payload using the shell awk record splitter.
///
/// Splits the payload at every `"`. Drops the final empty record if the payload
/// ends with `"`. Scans records using state `seen` (starts at 0):
/// 1. If `seen == 2`, this record is the result.
/// 2. If `seen == 1` and the record is only ASCII whitespace around exactly one `:`,
///    transitions to `seen = 2` and advances to the next record.
/// 3. If `seen == 1` otherwise, resets `seen = 0` and falls through.
/// 4. If the record equals `"source"`, transitions to `seen = 1`.
///
/// All trailing newlines are removed from the captured result. Returns an empty
/// string if not found.
pub fn extract_source(payload: &str) -> String {
    if payload.is_empty() {
        return String::new();
    }
    let mut records: Vec<&str> = payload.split('"').collect();
    if payload.ends_with('"') {
        records.pop();
    }

    let mut seen = 0;
    let mut result = None;

    for record in records {
        if seen == 2 {
            result = Some(record);
            break;
        }
        if seen == 1 {
            if matches_colon_record(record) {
                seen = 2;
                continue;
            }
            seen = 0;
        }
        if record == "source" {
            seen = 1;
        }
    }

    result.map_or_else(String::new, |r| r.trim_end_matches('\n').to_string())
}

/// Detects whether the hook payload was delivered by a foreign host (e.g. Cursor).
///
/// Returns `true` only when the payload parses as a JSON object containing
/// a `"cursor_version"` key whose value is a JSON string.
pub fn payload_is_foreign_host(payload: &str) -> bool {
    if payload.is_empty() {
        return false;
    }
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(payload)
    else {
        return false;
    };
    matches!(
        map.get("cursor_version"),
        Some(serde_json::Value::String(_))
    )
}

/// Facts about lock and completion files provided by the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CompletionFacts<'a> {
    /// Whether the lock file is a regular file (symlinks excluded).
    pub lock_is_regular_file: bool,
    /// Whether the completion file is a regular file (symlinks excluded).
    pub completion_is_regular_file: bool,
    /// Whether the lock is owned by the current process.
    pub lock_owned_by_self: bool,
    /// The contents of the lock file, if readable.
    pub lock_text: Option<&'a str>,
    /// The contents of the completion file, if readable.
    pub completion_text: Option<&'a str>,
}

/// Checks whether session start has completed successfully.
///
/// Requires regular files for lock and completion, lock ownership by self,
/// both texts present, lock text non-empty and all ASCII digits after stripping
/// trailing newlines, and completion text equal to lock text after stripping
/// trailing newlines.
pub fn session_start_completed(facts: &CompletionFacts<'_>) -> bool {
    if !facts.lock_is_regular_file || !facts.completion_is_regular_file || !facts.lock_owned_by_self
    {
        return false;
    }
    let (Some(raw_lock), Some(raw_completion)) = (facts.lock_text, facts.completion_text) else {
        return false;
    };
    let lock_pid = raw_lock.trim_end_matches('\n');
    let completion_pid = raw_completion.trim_end_matches('\n');

    if lock_pid.is_empty() || !lock_pid.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    completion_pid == lock_pid
}

/// Execution route decided from the session-start source and completion status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceRoute {
    /// Delegate to the nudge wrapper.
    Nudge,
    /// Re-emit digest only, carrying the session source.
    Reemit {
        /// Session-open source name.
        source: String,
    },
    /// Run a full session-start digest, carrying the session source.
    Full {
        /// Session-open source name.
        source: String,
    },
}

/// Routes session start based on the source name and completion status.
///
/// - `"resume"`, `"reload"`, `"fork"` -> [`SourceRoute::Nudge`]
/// - `"clear"`, `"compact"` -> [`SourceRoute::Reemit`] if completed, else [`SourceRoute::Full`]
/// - Anything else -> [`SourceRoute::Full`] with the source passed through unchanged
pub fn route(source: &str, completed: bool) -> SourceRoute {
    match source {
        "resume" | "reload" | "fork" => SourceRoute::Nudge,
        "clear" | "compact" => {
            if completed {
                SourceRoute::Reemit {
                    source: source.to_string(),
                }
            } else {
                SourceRoute::Full {
                    source: source.to_string(),
                }
            }
        }
        _ => SourceRoute::Full {
            source: source.to_string(),
        },
    }
}

/// Returns the exit code for standing down.
///
/// Exits with 3 when `pi_prerequisite` is true, otherwise 0.
pub fn stand_down_exit_code(pi_prerequisite: bool) -> i32 {
    if pi_prerequisite { 3 } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_args() {
        // none
        assert_eq!(
            parse_args(&[]),
            SessionStartArgs {
                source: String::new(),
                pi_prerequisite: false,
            }
        );

        // --source clear
        assert_eq!(
            parse_args(&["--source".to_string(), "clear".to_string()]),
            SessionStartArgs {
                source: "clear".to_string(),
                pi_prerequisite: false,
            }
        );

        // --source=compact
        assert_eq!(
            parse_args(&["--source=compact".to_string()]),
            SessionStartArgs {
                source: "compact".to_string(),
                pi_prerequisite: false,
            }
        );

        // trailing --source -> empty
        assert_eq!(
            parse_args(&["--source".to_string()]),
            SessionStartArgs {
                source: String::new(),
                pi_prerequisite: false,
            }
        );

        // --pi-prerequisite mixed with unknown args
        assert_eq!(
            parse_args(&[
                "--unknown".to_string(),
                "--pi-prerequisite".to_string(),
                "junk".to_string(),
                "--source".to_string(),
                "reload".to_string(),
                "--other".to_string(),
            ]),
            SessionStartArgs {
                source: "reload".to_string(),
                pi_prerequisite: true,
            }
        );

        // later --source wins
        assert_eq!(
            parse_args(&[
                "--source".to_string(),
                "first".to_string(),
                "--source=second".to_string(),
            ]),
            SessionStartArgs {
                source: "second".to_string(),
                pi_prerequisite: false,
            }
        );
        assert_eq!(
            parse_args(&[
                "--source=first".to_string(),
                "--source".to_string(),
                "second".to_string(),
            ]),
            SessionStartArgs {
                source: "second".to_string(),
                pi_prerequisite: false,
            }
        );
        assert_eq!(
            parse_args(&[
                "--source".to_string(),
                "first".to_string(),
                "--source".to_string(),
            ]),
            SessionStartArgs {
                source: String::new(),
                pi_prerequisite: false,
            }
        );
    }

    #[test]
    fn test_extract_source() {
        // extract_source table
        assert_eq!(extract_source(r#"{"source":"clear"}"#), "clear");
        assert_eq!(extract_source(r#"{"source" : "resume" ,"x":1}"#), "resume");
        assert_eq!(extract_source(r#"{"a":"source","b":"x"}"#), "");
        assert_eq!(
            extract_source(r#"{"hook":"s","source":"startup"}"#),
            "startup"
        );
        assert_eq!(extract_source(r#"{"source":""}"#), "");
        assert_eq!(extract_source(r#"{"source":""#), "");
        assert_eq!(extract_source("{\"source\":\"a\n\"}"), "a");
        assert_eq!(
            extract_source(r#"{"source":"first","source":"second"}"#),
            "first"
        );
        assert_eq!(extract_source(r#"{"x":{"source":"fork"}}"#), "fork");
        assert_eq!(extract_source(""), "");
        assert_eq!(extract_source(r#"junk "source":"new" junk"#), "new");
    }

    #[test]
    fn test_payload_is_foreign_host() {
        // string version true
        assert!(payload_is_foreign_host(r#"{"cursor_version":"0.1.0"}"#));
        // number version false
        assert!(!payload_is_foreign_host(r#"{"cursor_version":1}"#));
        // no key false
        assert!(!payload_is_foreign_host(r#"{"other":"val"}"#));
        // array false
        assert!(!payload_is_foreign_host(r#"["cursor_version"]"#));
        // empty false
        assert!(!payload_is_foreign_host(""));
        // invalid JSON false
        assert!(!payload_is_foreign_host(r#"{"cursor_version": broken"#));
    }

    #[test]
    fn test_session_start_completed() {
        let base = CompletionFacts {
            lock_is_regular_file: true,
            completion_is_regular_file: true,
            lock_owned_by_self: true,
            lock_text: Some("123"),
            completion_text: Some("123"),
        };

        // matching pids true
        assert!(session_start_completed(&base));

        // each single failing fact
        assert!(!session_start_completed(&CompletionFacts {
            lock_is_regular_file: false,
            ..base
        }));
        assert!(!session_start_completed(&CompletionFacts {
            completion_is_regular_file: false,
            ..base
        }));
        assert!(!session_start_completed(&CompletionFacts {
            lock_owned_by_self: false,
            ..base
        }));
        assert!(!session_start_completed(&CompletionFacts {
            lock_text: None,
            ..base
        }));
        assert!(!session_start_completed(&CompletionFacts {
            completion_text: None,
            ..base
        }));

        // mismatch false
        assert!(!session_start_completed(&CompletionFacts {
            completion_text: Some("456"),
            ..base
        }));

        // 12\n equals 12
        assert!(session_start_completed(&CompletionFacts {
            lock_text: Some("12\n"),
            completion_text: Some("12"),
            ..base
        }));
        assert!(session_start_completed(&CompletionFacts {
            lock_text: Some("12"),
            completion_text: Some("12\n"),
            ..base
        }));
        assert!(session_start_completed(&CompletionFacts {
            lock_text: Some("12\n\n"),
            completion_text: Some("12\n"),
            ..base
        }));

        // 1a false
        assert!(!session_start_completed(&CompletionFacts {
            lock_text: Some("1a"),
            completion_text: Some("1a"),
            ..base
        }));

        // empty false
        assert!(!session_start_completed(&CompletionFacts {
            lock_text: Some(""),
            completion_text: Some(""),
            ..base
        }));
        assert!(!session_start_completed(&CompletionFacts {
            lock_text: Some("\n"),
            completion_text: Some("\n"),
            ..base
        }));

        // None text false
        assert!(!session_start_completed(&CompletionFacts {
            lock_text: None,
            completion_text: None,
            ..base
        }));
    }

    #[test]
    fn test_route() {
        // all seven named sources
        assert_eq!(route("resume", true), SourceRoute::Nudge);
        assert_eq!(route("resume", false), SourceRoute::Nudge);

        assert_eq!(route("reload", true), SourceRoute::Nudge);
        assert_eq!(route("reload", false), SourceRoute::Nudge);

        assert_eq!(route("fork", true), SourceRoute::Nudge);
        assert_eq!(route("fork", false), SourceRoute::Nudge);

        assert_eq!(
            route("clear", true),
            SourceRoute::Reemit {
                source: "clear".to_string(),
            }
        );
        assert_eq!(
            route("clear", false),
            SourceRoute::Full {
                source: "clear".to_string(),
            }
        );

        assert_eq!(
            route("compact", true),
            SourceRoute::Reemit {
                source: "compact".to_string(),
            }
        );
        assert_eq!(
            route("compact", false),
            SourceRoute::Full {
                source: "compact".to_string(),
            }
        );

        assert_eq!(
            route("startup", true),
            SourceRoute::Full {
                source: "startup".to_string(),
            }
        );
        assert_eq!(
            route("startup", false),
            SourceRoute::Full {
                source: "startup".to_string(),
            }
        );

        assert_eq!(
            route("new", true),
            SourceRoute::Full {
                source: "new".to_string(),
            }
        );
        assert_eq!(
            route("new", false),
            SourceRoute::Full {
                source: "new".to_string(),
            }
        );

        // ""
        assert_eq!(
            route("", true),
            SourceRoute::Full {
                source: String::new(),
            }
        );
        assert_eq!(
            route("", false),
            SourceRoute::Full {
                source: String::new(),
            }
        );

        // Startup (capitalised, goes to Full)
        assert_eq!(
            route("Startup", true),
            SourceRoute::Full {
                source: "Startup".to_string(),
            }
        );
        assert_eq!(
            route("Startup", false),
            SourceRoute::Full {
                source: "Startup".to_string(),
            }
        );
    }

    #[test]
    fn test_stand_down_exit_code() {
        assert_eq!(stand_down_exit_code(true), 3);
        assert_eq!(stand_down_exit_code(false), 0);
    }

    #[test]
    fn extract_source_requires_exactly_one_colon() {
        // awk's separator record holds one colon; `::` does not match.
        assert_eq!(extract_source(r#"{"source"::"x"}"#), "");
    }

    #[test]
    fn extract_source_accepts_each_shell_space_class_around_colon() {
        // awk `[[:space:]]` covers space, tab, LF, VT, FF and CR; each one alone must work.
        for ws in [" ", "\t", "\n", "\u{0b}", "\u{0c}", "\r"] {
            let payload = format!("{{\"source\"{ws}:{ws}\"x\"}}");
            assert_eq!(extract_source(&payload), "x", "separator {ws:?}");
        }
    }

    #[test]
    fn extract_source_keeps_trailing_whitespace_and_strips_only_newlines() {
        assert_eq!(extract_source("{\"source\":\"a \"}"), "a ");
        assert_eq!(extract_source("{\"source\":\"a\t\"}"), "a\t");
        assert_eq!(extract_source("{\"source\":\"a\r\"}"), "a\r");
        assert_eq!(extract_source("{\"source\":\"a\n\n\"}"), "a");
    }

    #[test]
    fn extract_source_restarts_scan_after_non_colon_record() {
        // A `source` key with no colon after it is dropped; the next `source` key still applies.
        assert_eq!(extract_source(r#"{"source"source":"x"}"#), "x");
    }

    #[test]
    fn extract_source_matches_key_exactly() {
        assert_eq!(extract_source(r#"{"sourcex":"a","source":"b"}"#), "b");
    }

    #[test]
    fn parse_args_matches_pi_prerequisite_exactly() {
        let args = ["--pi", "--pi-foo", "--pi-prerequisites"].map(String::from);
        assert!(!parse_args(&args).pi_prerequisite);
    }

    #[test]
    fn session_start_completed_requires_whole_pid_match() {
        let facts = |lock_text: &'static str, completion_text: &'static str| CompletionFacts {
            lock_is_regular_file: true,
            completion_is_regular_file: true,
            lock_owned_by_self: true,
            lock_text: Some(lock_text),
            completion_text: Some(completion_text),
        };
        assert!(!session_start_completed(&facts("123", "12")));
        assert!(!session_start_completed(&facts("12", "123")));
    }

    #[test]
    fn extract_source_rejects_separator_without_colon() {
        // Empty and whitespace-only records are not the separator; the key is dropped.
        assert_eq!(extract_source(r#"{"source""x"}"#), "");
        assert_eq!(extract_source("{\"source\"  \"x\"}"), "");
    }

    #[test]
    fn extract_source_rejects_non_whitespace_around_colon() {
        // Any other character in the separator record means the key is not followed by a bare colon.
        assert_eq!(extract_source(r#"{"source"x:"y"}"#), "");
    }
}
