//! `mcao audit validate`, checked against the results `bin/fm-audit-envelope.py` gives for the same
//! fixtures in `tests/fixtures/audit-validate`. Each expected stdout line and exit status was
//! recorded by running the reference on that fixture with `python3`.
//!
//! Zone lookups read the system zoneinfo database, which Windows does not have here, so the
//! command's zone-bearing cases run only on Unix.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

/// A scratch directory under the system temp dir, removed when the test ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("the clock is after the epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "mcao-audit-validate-{label}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("a scratch directory");
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/audit-validate")
        .join(name)
}

/// Runs `mcao audit validate <args>` with the state directory set to `state`, or unset.
fn validate(args: &[&str], state: Option<&Path>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mcao"));
    command
        .args(["audit", "validate"])
        .args(args)
        .env_remove("FM_HOME")
        .env_remove("FM_STATE_OVERRIDE");
    if let Some(state) = state {
        command.env("FM_STATE_OVERRIDE", state);
    }
    command.output().expect("mcao runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("UTF-8 stdout")
}

fn status(output: &Output) -> i32 {
    output.status.code().expect("an exit status")
}

#[cfg(unix)]
mod zone_bearing {
    use super::*;
    use std::io::Write;
    use std::process::Stdio;

    const ACCEPTED: &str = r#"{"result": "accepted", "definition": "envelope"}"#;

    /// Asserts the command printed exactly `expected` on stdout and exited with `code`.
    fn assert_result(output: &Output, expected: &str, code: i32) {
        assert_eq!(stdout(output), format!("{expected}\n"));
        assert_eq!(
            status(output),
            code,
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn a_valid_envelope_is_accepted_and_records_nothing() {
        let scratch = Scratch::new("accepted");
        let path = fixture("envelope-accepted.json");
        let output = validate(&[path.to_str().unwrap()], Some(&scratch.0));
        assert_result(&output, ACCEPTED, 0);
        assert!(!scratch.0.join("audit-rejections.jsonl").exists());
    }

    #[test]
    fn a_record_on_the_spring_forward_day_is_accepted() {
        let path = fixture("envelope-chicago-spring-forward.json");
        let output = validate(&[path.to_str().unwrap()], None);
        assert_result(&output, ACCEPTED, 0);
    }

    /// 2026-03-08T05:30Z is 23:30 on 2026-03-07 in Chicago (CST, UTC-6). An offset one hour off
    /// lands on 2026-03-08, so the day key pins the exact offset rather than a near one.
    #[test]
    fn a_record_whose_local_day_turns_on_the_exact_offset_is_accepted() {
        let path = fixture("envelope-chicago-offset-edge.json");
        let output = validate(&[path.to_str().unwrap()], None);
        assert_result(&output, ACCEPTED, 0);
    }

    #[test]
    fn a_fleet_ledger_record_is_accepted_under_its_definition() {
        let path = fixture("ledger-accepted.json");
        let output = validate(
            &["--definition", "fleet_ledger_v1", path.to_str().unwrap()],
            None,
        );
        assert_result(
            &output,
            r#"{"result": "accepted", "definition": "fleet_ledger_v1"}"#,
            0,
        );
    }

    /// Each row is `(fixture, expected rejection line)`, as the reference prints it.
    #[test]
    fn rejections_match_the_reference_reason_and_paths() {
        let cases = [
            (
                "envelope-null-user-id.json",
                r#"{"result": "rejected", "definition": "envelope", "reason": "identity_unresolved", "paths": ["/user/id"]}"#,
            ),
            (
                "envelope-pending-user-id.json",
                r#"{"result": "rejected", "definition": "envelope", "reason": "identity_unresolved", "paths": ["/user/id"]}"#,
            ),
            (
                "envelope-missing-user.json",
                r#"{"result": "rejected", "definition": "envelope", "reason": "identity_unresolved", "paths": ["/user"]}"#,
            ),
            (
                "envelope-missing-event.json",
                r#"{"result": "rejected", "definition": "envelope", "reason": "schema_invalid", "paths": ["/event"]}"#,
            ),
            (
                "envelope-extra-member.json",
                r#"{"result": "rejected", "definition": "envelope", "reason": "schema_invalid", "paths": [""]}"#,
            ),
            (
                "envelope-seq-one-null-prev.json",
                r#"{"result": "rejected", "definition": "envelope", "reason": "schema_invalid", "paths": ["/prev_hash"]}"#,
            ),
            (
                "envelope-actor-mismatch.json",
                r#"{"result": "rejected", "definition": "envelope", "reason": "schema_invalid", "paths": ["/actor/id"]}"#,
            ),
            (
                "envelope-unknown-zone.json",
                r#"{"result": "rejected", "definition": "envelope", "reason": "schema_invalid", "paths": ["/user/timezone"]}"#,
            ),
            (
                "envelope-zone-traversal.json",
                r#"{"result": "rejected", "definition": "envelope", "reason": "schema_invalid", "paths": ["/user/timezone"]}"#,
            ),
            (
                "envelope-day-key-mismatch.json",
                r#"{"result": "rejected", "definition": "envelope", "reason": "day_key_mismatch", "paths": ["/day_key"]}"#,
            ),
            (
                "envelope-bad-ts.json",
                r#"{"result": "rejected", "definition": "envelope", "reason": "schema_invalid", "paths": ["/ts"]}"#,
            ),
            (
                "envelope-chicago-offset-edge-next-day.json",
                r#"{"result": "rejected", "definition": "envelope", "reason": "day_key_mismatch", "paths": ["/day_key"]}"#,
            ),
        ];
        let scratch = Scratch::new("rejections");
        for (name, expected) in cases {
            let path = fixture(name);
            let output = validate(&[path.to_str().unwrap()], Some(&scratch.0));
            assert_result(&output, expected, 1);
        }
    }

    #[test]
    fn a_fleet_ledger_record_with_the_wrong_version_is_rejected() {
        let scratch = Scratch::new("ledger");
        let path = fixture("ledger-wrong-version.json");
        let output = validate(
            &["--definition", "fleet_ledger_v1", path.to_str().unwrap()],
            Some(&scratch.0),
        );
        assert_result(
            &output,
            r#"{"result": "rejected", "definition": "fleet_ledger_v1", "reason": "schema_invalid", "paths": ["/v"]}"#,
            1,
        );
    }

    #[test]
    fn a_rejection_is_appended_to_the_rejection_log_in_the_reference_format() {
        let scratch = Scratch::new("log");
        let path = fixture("envelope-null-user-id.json");
        validate(&[path.to_str().unwrap()], Some(&scratch.0));
        validate(&[path.to_str().unwrap()], Some(&scratch.0));
        let log = fs::read_to_string(scratch.0.join("audit-rejections.jsonl")).expect("the log");
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 2, "one line per rejection");
        for line in lines {
            let stamp = line
                .strip_prefix(r#"{"definition": "envelope", "paths": ["/user/id"], "reason": "identity_unresolved", "rejected_at": "#)
                .and_then(|rest| rest.strip_suffix(r#", "v": 1}"#))
                .expect("the reference's sorted key layout");
            assert!(
                stamp.parse::<i64>().is_ok(),
                "rejected_at is an integer: {stamp}"
            );
        }
    }

    #[test]
    fn the_rejection_log_falls_back_to_fm_home_state() {
        let scratch = Scratch::new("home");
        let state = scratch.0.join("state");
        fs::create_dir_all(&state).expect("a state directory");
        let path = fixture("envelope-day-key-mismatch.json");
        let mut command = Command::new(env!("CARGO_BIN_EXE_mcao"));
        let output = command
            .args(["audit", "validate", path.to_str().unwrap()])
            .env_remove("FM_STATE_OVERRIDE")
            .env("FM_HOME", &scratch.0)
            .output()
            .expect("mcao runs");
        assert_eq!(status(&output), 1);
        assert!(state.join("audit-rejections.jsonl").is_file());
    }

    #[test]
    fn stdin_is_read_when_the_input_is_a_dash() {
        let scratch = Scratch::new("stdin");
        let document = fs::read(fixture("envelope-accepted.json")).expect("the fixture");
        let mut child = Command::new(env!("CARGO_BIN_EXE_mcao"))
            .args(["audit", "validate", "-"])
            .env_remove("FM_HOME")
            .env("FM_STATE_OVERRIDE", &scratch.0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("mcao spawns");
        child
            .stdin
            .take()
            .expect("piped stdin")
            .write_all(&document)
            .expect("stdin accepts the document");
        let output = child.wait_with_output().expect("mcao finishes");
        assert_result(&output, ACCEPTED, 0);
    }
}

#[test]
fn a_duplicate_key_is_an_input_error_with_no_result() {
    let path = fixture("envelope-duplicate-key.json");
    let output = validate(&[path.to_str().unwrap()], None);
    assert_eq!(stdout(&output), "");
    assert_eq!(status(&output), 2);
    assert!(String::from_utf8_lossy(&output.stderr).contains("duplicate object key"));
}

#[test]
fn a_non_finite_number_is_an_input_error_with_no_result() {
    let path = fixture("envelope-nan.json");
    let output = validate(&[path.to_str().unwrap()], None);
    assert_eq!(stdout(&output), "");
    assert_eq!(status(&output), 2);
}

#[test]
fn an_unreadable_path_is_an_input_error() {
    let output = validate(&["/nonexistent/audit-input.json"], None);
    assert_eq!(stdout(&output), "");
    assert_eq!(status(&output), 2);
}

#[test]
fn an_unknown_definition_is_a_usage_error() {
    let path = fixture("envelope-accepted.json");
    let output = validate(&["--definition", "bogus", path.to_str().unwrap()], None);
    assert_eq!(stdout(&output), "");
    assert_eq!(status(&output), 2);
}

/// With neither `FM_HOME` nor `FM_STATE_OVERRIDE`, the rejection has no state directory. Windows
/// resolves the default home under `LOCALAPPDATA` instead, so that points at a scratch directory
/// with no state beneath it, and the test never touches the machine's own default home.
#[test]
fn a_rejection_without_a_state_directory_is_not_printed() {
    let scratch = Scratch::new("no-state");
    let path = fixture("envelope-day-key-mismatch.json");
    let output = Command::new(env!("CARGO_BIN_EXE_mcao"))
        .args(["audit", "validate", path.to_str().unwrap()])
        .env_remove("FM_HOME")
        .env_remove("FM_STATE_OVERRIDE")
        .env("LOCALAPPDATA", &scratch.0)
        .output()
        .expect("mcao runs");
    assert_eq!(stdout(&output), "");
    assert_eq!(status(&output), 2);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("rejection not recorded"), "{stderr}");
    if cfg!(unix) {
        assert!(stderr.contains("FM_HOME or FM_STATE_OVERRIDE"), "{stderr}");
    }
}

/// On Windows, an unset `FM_HOME` selects the default home under `LOCALAPPDATA`, so the rejection is
/// recorded in that home's state directory. `LOCALAPPDATA` is a scratch directory here.
#[cfg(windows)]
#[test]
fn a_rejection_is_recorded_under_the_windows_default_home() {
    let scratch = Scratch::new("default-home");
    let state = scratch.0.join("mcao").join("home").join("state");
    fs::create_dir_all(&state).expect("the default home's state directory");
    let path = fixture("envelope-day-key-mismatch.json");
    let output = Command::new(env!("CARGO_BIN_EXE_mcao"))
        .args(["audit", "validate", path.to_str().unwrap()])
        .env_remove("FM_HOME")
        .env_remove("FM_STATE_OVERRIDE")
        .env("LOCALAPPDATA", &scratch.0)
        .output()
        .expect("mcao runs");
    assert_eq!(
        stdout(&output),
        "{\"result\": \"rejected\", \"definition\": \"envelope\", \"reason\": \"day_key_mismatch\", \"paths\": [\"/day_key\"]}\n"
    );
    assert_eq!(status(&output), 1);
    assert!(state.join("audit-rejections.jsonl").is_file());
}

#[test]
fn a_rejection_into_a_missing_state_directory_is_not_printed() {
    let scratch = Scratch::new("missing");
    let missing = scratch.0.join("no-such-dir");
    let path = fixture("envelope-day-key-mismatch.json");
    let output = validate(&[path.to_str().unwrap()], Some(&missing));
    assert_eq!(stdout(&output), "");
    assert_eq!(status(&output), 2);
}
