//! Session-start source extraction and routing logic (DPES-893).
//!
//! Ports the decision logic of `bin/fm-sessionstart-run.sh`: parsing script arguments,
//! extracting the `source` field from a hook payload with the shell awk contract,
//! foreign-host detection with the shell `jq` contract, session-start completion checks,
//! and source routing.
//!
//! The shell is the behavioral authority. Where the DPES-893 pseudocode disagrees with
//! the shell, this module follows the shell. `docs/hook-source.md` owns the reconciliation
//! table, the scope limits (ASCII separators, single-value routing), and the wiring
//! requirements every caller must meet before these functions can stand in for the shell.

/// Arguments parsed from the session-start command line.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SessionStartArgs {
    /// Session-open source name, or empty if unspecified.
    pub source: String,
    /// Whether the `--pi-prerequisite` argument was given. The shell reads this argument only,
    /// while DPES-893 AC4 names an environment variable; that choice is decided for the
    /// argument (see `docs/hook-source.md`).
    pub pi_prerequisite: bool,
}

/// Parses session-start command line arguments from left to right.
///
/// `--source` takes the next argument as the source, even when that argument looks like a
/// flag, and yields an empty source when it is the last argument; `--source=VALUE` sets the
/// source to `VALUE`; `--pi-prerequisite` sets the prerequisite flag; any other argument is
/// skipped. A later `--source` overrides an earlier one.
#[must_use]
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

/// ASCII bytes matched by awk's `[[:space:]]` in the C locale.
///
/// `\x0b` (vertical tab) is included, which `u8::is_ascii_whitespace` omits.
const fn is_shell_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r')
}

/// Matches the awk separator record `^[[:space:]]*:[[:space:]]*$`.
///
/// The record must hold exactly one colon and otherwise only shell space.
fn is_separator_record(record: &[u8]) -> bool {
    let mut significant = record.iter().copied().filter(|&b| !is_shell_space(b));
    significant.next() == Some(b':') && significant.next().is_none()
}

/// Removes trailing `\n` bytes, as command substitution does to captured output.
fn trim_trailing_newlines(bytes: &[u8]) -> &[u8] {
    let end = bytes
        .iter()
        .rposition(|&b| b != b'\n')
        .map_or(0, |last| last + 1);
    &bytes[..end]
}

/// Models a shell command substitution such as `PAYLOAD=$(cat)` on raw bytes.
///
/// Bash discards every NUL byte from captured output and strips trailing newlines. Any
/// wiring that reads a hook payload, a lock file, or a completion file must apply this
/// before the other functions here, or a NUL byte can change a match (see
/// `docs/hook-source.md`, "Wiring requirements").
#[must_use]
pub fn normalize_command_substitution(raw: &[u8]) -> Vec<u8> {
    let without_nul: Vec<u8> = raw.iter().copied().filter(|&b| b != 0).collect();
    trim_trailing_newlines(&without_nul).to_vec()
}

/// Scan state of the awk source extractor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scan {
    /// Looking for a record equal to `source`.
    Seeking,
    /// The `source` key was the previous record; the next record must be the bare colon.
    AfterKey,
    /// The colon was the previous record; the next record is the value.
    AfterColon,
}

/// Extracts the session-start source from raw payload bytes with the shell awk contract.
///
/// The payload is split at every `"` byte, matching awk with `RS = "\""`. A `source` record
/// starts a scan; it succeeds only when the next record is a separator record (exactly one
/// colon, with shell space around it), and the record after that is the result. A record
/// that breaks the scan restarts it, so a later `source` key still applies. The result loses
/// its trailing newlines, as the shell's command substitution does.
///
/// Returns an empty vector when no value is found. The shell treats an absent value and an
/// empty value identically at routing, so this sentinel matches there. The shell's baseline
/// step differs for an empty source, and the port does not perform that step; see
/// `docs/hook-source.md`.
///
/// The split at `"` never cuts a multi-byte UTF-8 sequence, and invalid UTF-8 in the value
/// passes through unchanged. A trailing `"` yields a final empty record that only ever
/// produces an empty result, so it needs no special handling.
#[must_use]
pub fn extract_source_bytes(payload: &[u8]) -> Vec<u8> {
    let mut state = Scan::Seeking;
    for record in payload.split(|&b| b == b'"') {
        if state == Scan::AfterColon {
            return trim_trailing_newlines(record).to_vec();
        }
        if state == Scan::AfterKey {
            if is_separator_record(record) {
                state = Scan::AfterColon;
                continue;
            }
            state = Scan::Seeking;
        }
        if record == b"source" {
            state = Scan::AfterKey;
        }
    }
    Vec::new()
}

/// Detects whether the hook payload was delivered by a foreign host (e.g. Cursor).
///
/// Matches the shell predicate `jq -e 'type == "object" and has("cursor_version") and
/// (.cursor_version | type) == "string"'` over the whole payload:
///
/// - Every JSON value in the stream must parse; any trailing garbage makes the result false.
/// - The last value in the stream decides the result, as `jq -e` judges its last output.
/// - Invalid UTF-8 is replaced before parsing, as jq replaces it, so a string value still
///   counts.
/// - An empty or whitespace-only payload is not foreign.
/// - Nesting deeper than serde_json's recursion limit (128 levels) fails parsing and so reads
///   as not foreign, where `jq` would judge the value. Lifting the limit would let a payload
///   exhaust the stack.
/// - Several classes that jq 1.8.1 accepts fail parsing here: a BOM, `NaN`, `Infinity`,
///   leading zeros, a leading `+` or `.`, out-of-range numbers, and a lone low surrogate
///   escape.
///
/// Every such divergence reads as not foreign, so the port runs redundantly rather than
/// skipping a run. `docs/hook-source.md` lists them with measured verdicts. The shell also
/// treats a missing `jq` as not foreign; that is a wiring concern, not part of this predicate.
#[must_use]
pub fn payload_is_foreign_host(payload: &[u8]) -> bool {
    let text = String::from_utf8_lossy(payload);
    let mut last = None;
    for value in serde_json::Deserializer::from_str(&text).into_iter::<serde_json::Value>() {
        match value {
            Ok(value) => last = Some(value),
            Err(_) => return false,
        }
    }
    last.is_some_and(|value| is_cursor_host_value(&value))
}

fn is_cursor_host_value(value: &serde_json::Value) -> bool {
    matches!(
        value,
        serde_json::Value::Object(map)
            if matches!(map.get("cursor_version"), Some(serde_json::Value::String(_)))
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
/// Requires regular files for lock and completion, lock ownership by self, both texts
/// present, lock text non-empty and all ASCII digits after stripping trailing newlines, and
/// completion text equal to lock text after stripping trailing newlines. The comparison is
/// on the whole PID, so `123` never matches `12`.
///
/// Texts must already be normalized with [`normalize_command_substitution`], as the shell
/// reads them through command substitution.
#[must_use]
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

/// Whether a recorded, PID-matched session-start completion exists for this state directory.
///
/// Derived by the caller from [`session_start_completed`]; it selects between a re-emit and a
/// full digest for `clear` and `compact`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Completion {
    /// The completion record matches the lock owner.
    Recorded,
    /// No matching completion record exists.
    Absent,
}

/// Execution route decided from the session-start source and completion status.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
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
/// - `"clear"`, `"compact"` -> [`SourceRoute::Reemit`] when completion is recorded, else
///   [`SourceRoute::Full`]
/// - Anything else, including the empty sentinel -> [`SourceRoute::Full`] with the source
///   passed through unchanged
///
/// The argument is `&str`. A caller that holds invalid UTF-8 must convert it lossily before
/// routing; the conversion changes no routing decision. The bytes handed to the shell stay the
/// caller's to keep (see `docs/hook-source.md`).
#[must_use]
pub fn route(source: &str, completion: Completion) -> SourceRoute {
    match source {
        "resume" | "reload" | "fork" => SourceRoute::Nudge,
        "clear" | "compact" => match completion {
            Completion::Recorded => SourceRoute::Reemit {
                source: source.to_string(),
            },
            Completion::Absent => SourceRoute::Full {
                source: source.to_string(),
            },
        },
        _ => SourceRoute::Full {
            source: source.to_string(),
        },
    }
}

/// Returns the exit code for standing down.
///
/// Exits with 3 when the internal Pi prerequisite flag is set, otherwise 0. The shell also
/// uses this code when the state directory cannot be created under that flag; see
/// `docs/hook-source.md`.
#[must_use]
pub fn stand_down_exit_code(args: &SessionStartArgs) -> i32 {
    if args.pi_prerequisite { 3 } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    /// Text view of [`extract_source_bytes`]. Production callers pass bytes, so only tests use
    /// this `&str` form.
    fn extract_source(payload: &str) -> String {
        String::from_utf8_lossy(&extract_source_bytes(payload.as_bytes())).into_owned()
    }

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
            parse_args(&args(&["--source", "clear"])),
            SessionStartArgs {
                source: "clear".to_string(),
                pi_prerequisite: false,
            }
        );

        // --source=compact
        assert_eq!(
            parse_args(&args(&["--source=compact"])),
            SessionStartArgs {
                source: "compact".to_string(),
                pi_prerequisite: false,
            }
        );

        // trailing --source -> empty
        assert_eq!(
            parse_args(&args(&["--source"])),
            SessionStartArgs {
                source: String::new(),
                pi_prerequisite: false,
            }
        );

        // --pi-prerequisite mixed with unknown args
        assert_eq!(
            parse_args(&args(&[
                "--unknown",
                "--pi-prerequisite",
                "junk",
                "--source",
                "reload",
                "--other",
            ])),
            SessionStartArgs {
                source: "reload".to_string(),
                pi_prerequisite: true,
            }
        );

        // later --source wins
        assert_eq!(
            parse_args(&args(&["--source", "first", "--source=second"])),
            SessionStartArgs {
                source: "second".to_string(),
                pi_prerequisite: false,
            }
        );
        assert_eq!(
            parse_args(&args(&["--source=first", "--source", "second"])),
            SessionStartArgs {
                source: "second".to_string(),
                pi_prerequisite: false,
            }
        );
        assert_eq!(
            parse_args(&args(&["--source", "first", "--source"])),
            SessionStartArgs {
                source: String::new(),
                pi_prerequisite: false,
            }
        );
    }

    #[test]
    fn parse_args_source_consumes_a_following_flag() {
        // The shell reads `${2:-}` unconditionally, so `--pi-prerequisite` after `--source` is
        // the source value and does not set the flag.
        assert_eq!(
            parse_args(&args(&["--source", "--pi-prerequisite"])),
            SessionStartArgs {
                source: "--pi-prerequisite".to_string(),
                pi_prerequisite: false,
            }
        );
    }

    #[test]
    fn parse_args_explicit_empty_source_is_empty() {
        // `--source ""` and `--source=` both yield the empty source, as the shell's `${2:-}` and
        // `${1#--source=}` do. The shell then reads the payload, because the source is empty.
        let empty = SessionStartArgs {
            source: String::new(),
            pi_prerequisite: false,
        };
        assert_eq!(parse_args(&args(&["--source", ""])), empty);
        assert_eq!(parse_args(&args(&["--source="])), empty);
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
    fn extract_source_trailing_quote_matches_shell() {
        // The final record after a trailing quote is empty and cannot change the result.
        assert_eq!(extract_source(r#"{"source":"x""#), "x");
        assert_eq!(extract_source(r#"{"source":"x"}""#), "x");
        assert_eq!(extract_source(r#"{"source":"#), "");
    }

    #[test]
    fn extract_source_does_not_decode_json_escapes() {
        // The shell splits on every quote, so an escaped quote ends the value.
        assert_eq!(extract_source(r#"{"source":"a\"b"}"#), "a\\");
        assert_eq!(extract_source(r#"{"source":"\\"}"#), "\\\\");
    }

    #[test]
    fn test_payload_is_foreign_host() {
        // string version true
        assert!(payload_is_foreign_host(br#"{"cursor_version":"0.1.0"}"#));
        // number version false
        assert!(!payload_is_foreign_host(br#"{"cursor_version":1}"#));
        // no key false
        assert!(!payload_is_foreign_host(br#"{"other":"val"}"#));
        // array false
        assert!(!payload_is_foreign_host(br#"["cursor_version"]"#));
        // empty false
        assert!(!payload_is_foreign_host(b""));
        // invalid JSON false
        assert!(!payload_is_foreign_host(br#"{"cursor_version": broken"#));
    }

    #[test]
    fn payload_is_foreign_host_judges_the_last_value_of_a_stream() {
        // jq -e evaluates the last output, so a trailing non-foreign object is not foreign.
        assert!(!payload_is_foreign_host(br#"{"cursor_version":"x"} {}"#));
        // A foreign object followed by a foreign object is foreign.
        assert!(payload_is_foreign_host(
            br#"{"cursor_version":"x"}{"cursor_version":"y"}"#
        ));
        // A leading non-foreign object does not stop a foreign last object.
        assert!(payload_is_foreign_host(br#"{} {"cursor_version":"x"}"#));
        // Trailing garbage makes jq fail, so the payload is not foreign.
        assert!(!payload_is_foreign_host(
            br#"{"cursor_version":"x"} garbage"#
        ));
    }

    #[test]
    fn payload_is_foreign_host_replaces_invalid_utf8_as_jq_does() {
        // jq replaces invalid UTF-8 with U+FFFD, so the string still has a string type.
        assert!(payload_is_foreign_host(b"{\"cursor_version\":\"\xff\"}"));
        assert!(payload_is_foreign_host(
            b"{\"\xff\":1,\"cursor_version\":\"x\"}"
        ));
    }

    #[test]
    fn payload_is_foreign_host_fails_toward_a_run_on_deep_nesting() {
        // Beyond serde_json's 128-level limit the predicate reads not foreign; jq accepts the
        // same payload as foreign. See the limit note on `payload_is_foreign_host`.
        let deep = format!(
            "{{\"cursor_version\":\"x\",\"a\":{}{}}}",
            "[".repeat(200),
            "]".repeat(200)
        );
        assert!(!payload_is_foreign_host(deep.as_bytes()));
        let shallow = format!(
            "{{\"cursor_version\":\"x\",\"a\":{}{}}}",
            "[".repeat(20),
            "]".repeat(20)
        );
        assert!(payload_is_foreign_host(shallow.as_bytes()));
    }

    #[test]
    fn payload_is_foreign_host_ignores_whitespace_only_payloads() {
        assert!(!payload_is_foreign_host(b"   "));
        assert!(!payload_is_foreign_host(b"\n"));
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
    fn session_start_completed_after_nul_normalization_matches_shell() {
        // Bash drops NUL from `$(cat)`, so "1\x002" reads as "12" and matches "12".
        let lock = normalize_command_substitution(b"1\x002\n");
        let lock = std::str::from_utf8(&lock).expect("digits are ASCII");
        let completion = normalize_command_substitution(b"12");
        let completion = std::str::from_utf8(&completion).expect("digits are ASCII");
        assert!(session_start_completed(&CompletionFacts {
            lock_is_regular_file: true,
            completion_is_regular_file: true,
            lock_owned_by_self: true,
            lock_text: Some(lock),
            completion_text: Some(completion),
        }));
    }

    #[test]
    fn session_start_completed_rejects_symlinked_lock_or_completion() {
        // Symlinks are not regular files, so a link to a valid PID never completes.
        let base = CompletionFacts {
            lock_is_regular_file: true,
            completion_is_regular_file: true,
            lock_owned_by_self: true,
            lock_text: Some("7"),
            completion_text: Some("7"),
        };
        assert!(!session_start_completed(&CompletionFacts {
            lock_is_regular_file: false,
            ..base
        }));
        assert!(!session_start_completed(&CompletionFacts {
            completion_is_regular_file: false,
            ..base
        }));
    }

    #[test]
    fn test_route() {
        // resume, reload, fork always delegate to the nudge wrapper
        for source in ["resume", "reload", "fork"] {
            assert_eq!(route(source, Completion::Recorded), SourceRoute::Nudge);
            assert_eq!(route(source, Completion::Absent), SourceRoute::Nudge);
        }

        // clear and compact re-emit only with a recorded completion
        for source in ["clear", "compact"] {
            assert_eq!(
                route(source, Completion::Recorded),
                SourceRoute::Reemit {
                    source: source.to_string(),
                }
            );
            assert_eq!(
                route(source, Completion::Absent),
                SourceRoute::Full {
                    source: source.to_string(),
                }
            );
        }

        // every other value, including the empty sentinel and capitalised names, runs full
        for source in ["startup", "new", "", "Startup"] {
            for completion in [Completion::Recorded, Completion::Absent] {
                assert_eq!(
                    route(source, completion),
                    SourceRoute::Full {
                        source: source.to_string(),
                    }
                );
            }
        }
    }

    #[test]
    fn route_treats_empty_source_as_unrecognized_full_run() {
        // The shell passes an empty --source through; it runs the full digest, and the
        // caller must not read that as `startup`.
        assert_eq!(
            route("", Completion::Recorded),
            SourceRoute::Full {
                source: String::new(),
            }
        );
    }

    #[test]
    fn parsed_empty_source_forms_route_to_the_full_run() {
        // Wiring requirements 5 and 10: every empty-source spelling parses to the empty sentinel,
        // and the sentinel routes to a full run that carries no invented source name.
        let forms: [&[&str]; 4] = [&[], &["--source"], &["--source", ""], &["--source="]];
        for form in forms {
            let parsed = parse_args(&args(form));
            assert_eq!(
                route(&parsed.source, Completion::Recorded),
                SourceRoute::Full {
                    source: String::new(),
                },
                "form {form:?}"
            );
        }
    }

    #[test]
    fn route_lossy_invalid_utf8_source_never_matches_a_named_route() {
        // Wiring requirement 11: the caller converts invalid bytes lossily for routing only.
        // U+FFFD never equals a route name, so the lossy form cannot become a nudge or re-emit.
        let invalid: [&[u8]; 4] = [b"resume\xff", b"\xffclear", b"compact\xfe", b"fork\x80"];
        for bytes in invalid {
            let lossy = String::from_utf8_lossy(bytes);
            assert!(
                matches!(
                    route(&lossy, Completion::Recorded),
                    SourceRoute::Full { .. }
                ),
                "bytes {bytes:?} routed as {:?}",
                route(&lossy, Completion::Recorded)
            );
        }
        // Valid UTF-8 is unchanged by the lossy form, so its routing is the same as the bytes.
        assert_eq!(
            route(&String::from_utf8_lossy(b"clear"), Completion::Recorded),
            SourceRoute::Reemit {
                source: "clear".to_string(),
            }
        );
    }

    #[test]
    fn test_stand_down_exit_code() {
        assert_eq!(
            stand_down_exit_code(&parse_args(&args(&["--pi-prerequisite"]))),
            3
        );
        assert_eq!(stand_down_exit_code(&parse_args(&[])), 0);
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
    fn extract_source_ignores_non_ascii_space_around_colon() {
        // The contract is ASCII. In the C locale the shell also rejects U+00A0 and U+2003;
        // under en_US.UTF-8 the shell accepts U+00A0, a difference recorded in
        // docs/hook-source.md rather than copied here.
        assert_eq!(extract_source("{\"source\"\u{a0}:\u{a0}\"x\"}"), "");
        assert_eq!(extract_source("{\"source\"\u{2003}:\u{2003}\"x\"}"), "");
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

    #[test]
    fn extract_source_preserves_invalid_utf8_value_bytes() {
        // The shell carries value bytes through unchanged, so the byte form keeps them.
        assert_eq!(
            extract_source_bytes(b"{\"source\":\"\xff\xfe\"}"),
            vec![0xff, 0xfe]
        );
    }

    #[test]
    fn extract_source_does_not_strip_nul_without_normalization() {
        // NUL is data to the extractor. A raw NUL inside the key breaks the match; the caller
        // must normalize first, as the shell's command substitution does.
        assert_eq!(extract_source("{\"sou\0rce\":\"x\"}"), "");
    }

    #[test]
    fn normalized_payload_with_nul_extracts_like_the_shell() {
        // Bash reads `{"sou\0rce":"cl\0ear"}` as `{"source":"clear"}`.
        let payload = normalize_command_substitution(b"{\"sou\0rce\":\"cl\0ear\"}\n\n");
        assert_eq!(payload, b"{\"source\":\"clear\"}".to_vec());
        assert_eq!(
            String::from_utf8_lossy(&extract_source_bytes(&payload)),
            "clear"
        );
    }

    #[test]
    fn normalize_command_substitution_strips_only_trailing_newlines() {
        assert_eq!(normalize_command_substitution(b"a\r\n"), b"a\r".to_vec());
        assert_eq!(
            normalize_command_substitution(b"a\n\nb\n"),
            b"a\n\nb".to_vec()
        );
        assert_eq!(normalize_command_substitution(b"\n\n"), Vec::<u8>::new());
        assert_eq!(normalize_command_substitution(b""), Vec::<u8>::new());
    }
}
