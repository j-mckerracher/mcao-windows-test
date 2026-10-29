//! The refusal a delivery path applies to a qualification record (AC10-AC12).
//!
//! A required tool that is not `supported` stops the check with [`McaoError::Preflight`], which
//! exits 3. The check never reports success for it, never falls back to another platform, and never
//! skips it. A tool the record does not mention is also a refusal, since skipping it would hide a gap.

use serde_json::Value;

use crate::error::McaoError;

/// The success line printed when every required tool is supported (AC11).
pub const ALL_SUPPORTED_MESSAGE: &str = "all required tools are supported";

/// The tools a delivery path needs before its work may start.
pub fn required_tools_for(delivery_path: &str) -> Result<&'static [&'static str], McaoError> {
    match delivery_path {
        "no-mistakes" => Ok(&["git", "gh", "no-mistakes"]),
        "direct-PR" => Ok(&["git", "gh"]),
        "local-only" => Ok(&["git"]),
        other => Err(McaoError::Usage(format!(
            "unknown delivery path '{other}'; expected no-mistakes, direct-PR, or local-only"
        ))),
    }
}

/// Checks that every `required` tool is `supported` in the qualification record `evidence_json`.
///
/// Returns `Ok` only when all of them are. Any other outcome is a [`McaoError::Preflight`]: a
/// record that does not parse, a missing tool list, an absent tool, or a tool whose verdict is not
/// `supported`.
pub fn check_required_tools(evidence_json: &str, required: &[&str]) -> Result<(), McaoError> {
    let evidence: Value = serde_json::from_str(evidence_json).map_err(|error| {
        McaoError::Preflight(format!("the qualification record does not parse: {error}"))
    })?;
    let Some(tools) = evidence.get("tools").and_then(Value::as_array) else {
        return Err(McaoError::Preflight(
            "the qualification record has no tools list".to_owned(),
        ));
    };
    for &name in required {
        let record = tools
            .iter()
            .find(|record| record.get("name").and_then(Value::as_str) == Some(name));
        let Some(record) = record else {
            return Err(McaoError::Preflight(format!(
                "required tool {name} is absent from the qualification record"
            )));
        };
        let verdict = record
            .get("verdict")
            .and_then(Value::as_str)
            .unwrap_or("missing");
        if verdict != "supported" {
            return Err(McaoError::Preflight(refusal(
                name,
                verdict,
                &recorded_blockers(record),
            )));
        }
    }
    Ok(())
}

/// The blockers the record gives for one tool. A probe that never started is recorded as unsupported
/// with its blocker here, so the refusal can say why.
fn recorded_blockers(record: &Value) -> Vec<&str> {
    record
        .get("blockers")
        .and_then(Value::as_array)
        .map(|blockers| blockers.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

/// The refusal text for one required tool that is not supported.
fn refusal(name: &str, verdict: &str, blockers: &[&str]) -> String {
    let reason = if blockers.is_empty() {
        String::new()
    } else {
        format!(" ({})", blockers.join("; "))
    };
    let install = match name {
        "no-mistakes" => {
            " The install method for no-mistakes on managed Windows is unapproved, so this check does not install it."
        }
        _ => "",
    };
    format!(
        "required tool {name} is {verdict}{reason}; correct it on this machine, then re-run `mcao toolchain qualify --output-json`. The Windows evidence record is docs/windows-toolchain.json.{install}"
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn record(tools: &Value) -> String {
        json!({ "tools": tools }).to_string()
    }

    fn all_supported() -> Value {
        json!([
            {"name": "git", "verdict": "supported"},
            {"name": "gh", "verdict": "supported"},
            {"name": "no-mistakes", "verdict": "unsupported"},
        ])
    }

    fn refusal_of(error: McaoError) -> String {
        assert_eq!(error.exit_code(), 3, "a refusal exits 3");
        error.to_string()
    }

    #[test]
    fn delivery_paths_map_to_their_required_tools() {
        assert_eq!(
            required_tools_for("no-mistakes").expect("known path"),
            ["git", "gh", "no-mistakes"]
        );
        assert_eq!(
            required_tools_for("direct-PR").expect("known path"),
            ["git", "gh"]
        );
        assert_eq!(
            required_tools_for("local-only").expect("known path"),
            ["git"]
        );
    }

    #[test]
    fn unknown_delivery_path_is_a_usage_error() {
        let error = required_tools_for("merge-everything").expect_err("unknown path");
        assert_eq!(error.exit_code(), 2);
    }

    #[test]
    fn all_required_supported_passes() {
        // AC11: the required tools are supported, so the check passes.
        let text = record(&all_supported());
        assert!(check_required_tools(&text, &["git", "gh"]).is_ok());
    }

    #[test]
    fn unsupported_required_tool_is_refused_with_exit_three() {
        // AC10: no-mistakes unsupported with the no-mistakes delivery path refuses and names the tool,
        // the verdict, the re-run command, the Windows evidence record, and the unapproved install
        // method.
        let text = record(&all_supported());
        let message = refusal_of(
            check_required_tools(&text, &["git", "gh", "no-mistakes"]).expect_err("refused"),
        );
        assert!(message.contains("no-mistakes"), "{message}");
        assert!(message.contains("unsupported"), "{message}");
        assert!(message.contains("qualify --output-json"), "{message}");
        assert!(message.contains("unapproved"), "{message}");
        assert!(message.contains("docs/windows-toolchain.json"), "{message}");
    }

    #[test]
    fn blocked_required_tool_is_refused() {
        // AC12: a required tool that is not supported is never a pass.
        let tools = json!([{"name": "git", "verdict": "blocked"}]);
        let message =
            refusal_of(check_required_tools(&record(&tools), &["git"]).expect_err("refused"));
        assert!(message.contains("blocked"), "{message}");
    }

    #[test]
    fn absent_required_tool_is_refused_not_skipped() {
        // AC12: a required tool the record omits is a refusal, never a skip.
        let tools = json!([{"name": "git", "verdict": "supported"}]);
        let message =
            refusal_of(check_required_tools(&record(&tools), &["git", "gh"]).expect_err("refused"));
        assert!(message.contains("gh"), "{message}");
        assert!(message.contains("absent"), "{message}");
    }

    #[test]
    fn refusal_quotes_the_recorded_blocker() {
        // A probe skipped for lack of budget is recorded as unsupported, and the refusal
        // names the blocker that says it never started.
        let tools = json!([
            {"name": "git", "verdict": "supported", "blockers": []},
            {
                "name": "gh",
                "verdict": "unsupported",
                "blockers": ["gh --version did not run: the qualification time budget was spent, so this probe did not start"],
            },
        ]);
        let message =
            refusal_of(check_required_tools(&record(&tools), &["git", "gh"]).expect_err("refused"));
        assert!(
            message.contains("gh is unsupported (gh --version did not run"),
            "{message}"
        );
        assert!(message.contains("time budget was spent"), "{message}");
    }

    #[test]
    fn unrequired_unsupported_tool_does_not_block() {
        let text = record(&all_supported());
        assert!(check_required_tools(&text, &["git", "gh"]).is_ok());
    }

    #[test]
    fn malformed_record_is_refused() {
        let message = refusal_of(check_required_tools("not json", &["git"]).expect_err("refused"));
        assert!(message.contains("does not parse"), "{message}");

        let no_list = refusal_of(
            check_required_tools(r#"{"timestamp":"x"}"#, &["git"]).expect_err("refused"),
        );
        assert!(no_list.contains("no tools list"), "{no_list}");
    }

    #[test]
    fn success_message_is_the_ac11_text() {
        assert_eq!(ALL_SUPPORTED_MESSAGE, "all required tools are supported");
    }
}
