//! `mcao audit activity-map`, checked against `bin/fm-audit-activity-map.py map`.
//!
//! The `.mapped.json` goldens in `tests/fixtures/audit-activity-map` are the reference's stdout for
//! the named record, recorded by running `python3 bin/fm-audit-activity-map.py map` on it. The
//! refusal lines below were recorded the same way. Zone lookups read the system zoneinfo
//! database, so the record-bearing cases run only on Unix, as `audit_validate` does.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

/// The synthetic home key the reference's test config uses. It is not a credential.
// gitleaks:allow - synthetic test key, not a credential
const HOME_KEY: &str = "5e8f1c2a7b9d4e6f0a3b5c7d9e1f2a4b6c8d0e2f4a6b8c0d1e3f5a7b9c1d2e3f";

/// A scratch directory under the system temp dir, removed when the test ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("the clock is after the epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "mcao-audit-activity-map-{label}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("a scratch directory");
        Self(path)
    }

    fn write(&self, name: &str, text: &str) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, text).expect("a scratch file");
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn fixture(name: &str) -> String {
    fs::read_to_string(fixture_dir().join("audit-validate").join(name))
        .expect("an envelope fixture")
}

fn config(scratch: &Scratch) -> PathBuf {
    scratch.write(
        "config.json",
        &format!("{{\"home_key\": \"{HOME_KEY}\"}}\n"),
    )
}

/// Runs `mcao audit activity-map --config <config> <input>` with the state directory set to
/// `state`, or unset. The record is read from stdin when `input` is `-`.
fn map(config: &Path, input: &str, state: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mcao"))
        .args(["audit", "activity-map", "--config"])
        .arg(config)
        .arg(input)
        .env_remove("FM_HOME")
        .env("FM_STATE_OVERRIDE", state)
        .output()
        .expect("mcao runs")
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
    use std::process::Stdio;

    fn golden(name: &str) -> String {
        fs::read_to_string(fixture_dir().join("audit-activity-map").join(name)).expect("a golden")
    }

    #[test]
    fn an_accepted_record_maps_to_the_reference_output() {
        let scratch = Scratch::new("accepted");
        let record = scratch.write("record.json", &fixture("envelope-accepted.json"));
        let output = map(&config(&scratch), record.to_str().unwrap(), &scratch.0);
        assert_eq!(stdout(&output), golden("envelope-accepted.mapped.json"));
        assert_eq!(status(&output), 0);
    }

    #[test]
    fn a_token_in_task_id_is_redacted_and_listed_as_changed() {
        let scratch = Scratch::new("token");
        let text = fixture("envelope-accepted.json")
            .replace("\"fix-login\"", "\"ghp_SyntheticTokenAbcdefghij0123\"");
        let record = scratch.write("record.json", &text);
        let output = map(&config(&scratch), record.to_str().unwrap(), &scratch.0);
        assert_eq!(stdout(&output), golden("envelope-task-token.mapped.json"));
        assert_eq!(status(&output), 0);
    }

    #[test]
    fn the_record_can_come_from_stdin() {
        let scratch = Scratch::new("stdin");
        let output = Command::new(env!("CARGO_BIN_EXE_mcao"))
            .args(["audit", "activity-map", "--config"])
            .arg(config(&scratch))
            .arg("-")
            .env_remove("FM_HOME")
            .env("FM_STATE_OVERRIDE", &scratch.0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                child
                    .stdin
                    .take()
                    .expect("stdin")
                    .write_all(fixture("envelope-accepted.json").as_bytes())?;
                child.wait_with_output()
            })
            .expect("mcao runs");
        assert_eq!(stdout(&output), golden("envelope-accepted.mapped.json"));
        assert_eq!(status(&output), 0);
    }

    #[test]
    fn a_refused_record_exits_1_with_pointers_and_writes_no_rejection_log() {
        let scratch = Scratch::new("refused");
        let text = fixture("envelope-accepted.json")
            .replace("\"mcao.audit-envelope.v1\"", "\"mcao.audit-envelope.v2\"");
        let record = scratch.write("record.json", &text);
        let output = map(&config(&scratch), record.to_str().unwrap(), &scratch.0);
        assert_eq!(
            stdout(&output),
            "{\"paths\": [\"/schema\"], \"reason\": \"schema_invalid\", \"result\": \"refused\"}\n"
        );
        assert_eq!(status(&output), 1);
        assert!(!scratch.0.join("audit-rejections.jsonl").exists());
    }
}

#[test]
fn a_missing_config_is_refused_as_config_invalid_with_the_document_pointer() {
    let scratch = Scratch::new("missing-config");
    let record = scratch.write("record.json", &fixture("envelope-accepted.json"));
    let output = map(
        &scratch.0.join("absent.json"),
        record.to_str().unwrap(),
        &scratch.0,
    );
    assert_eq!(
        stdout(&output),
        "{\"paths\": [\"\"], \"reason\": \"config_invalid\", \"result\": \"refused\"}\n"
    );
    assert_eq!(status(&output), 1);
}

#[test]
fn a_malformed_home_key_is_refused_at_its_pointer() {
    let scratch = Scratch::new("bad-key");
    let bad = scratch.write("config.json", "{\"home_key\": \"ABC\"}\n");
    let record = scratch.write("record.json", &fixture("envelope-accepted.json"));
    let output = map(&bad, record.to_str().unwrap(), &scratch.0);
    assert_eq!(
        stdout(&output),
        "{\"paths\": [\"/home_key\"], \"reason\": \"config_invalid\", \"result\": \"refused\"}\n"
    );
    assert_eq!(status(&output), 1);
}

#[test]
fn a_config_given_as_stdin_is_a_usage_error() {
    let scratch = Scratch::new("config-stdin");
    let output = map(Path::new("-"), "-", &scratch.0);
    assert_eq!(status(&output), 2);
    assert_eq!(stdout(&output), "");
}

#[test]
fn a_missing_config_flag_is_a_usage_error() {
    let scratch = Scratch::new("no-config");
    let output = Command::new(env!("CARGO_BIN_EXE_mcao"))
        .args(["audit", "activity-map"])
        .arg(scratch.write("record.json", "{}"))
        .env("FM_STATE_OVERRIDE", &scratch.0)
        .output()
        .expect("mcao runs");
    assert_eq!(status(&output), 2);
    assert_eq!(stdout(&output), "");
}

#[test]
fn an_input_that_is_not_json_is_an_input_error() {
    let scratch = Scratch::new("not-json");
    let config = config(&scratch);
    let record = scratch.write("record.json", "{");
    let output = map(&config, record.to_str().unwrap(), &scratch.0);
    assert_eq!(status(&output), 2);
    assert_eq!(stdout(&output), "");
}

// The three tests below pin known divergences from the Python reference; see the module
// documentation of `mcao_core::audit_activity`.

#[test]
fn a_record_nested_past_the_json_depth_limit_is_an_input_error() {
    let scratch = Scratch::new("deep-nest");
    let config = config(&scratch);
    let text = format!("{}{}", "[".repeat(400), "]".repeat(400));
    let record = scratch.write("record.json", &text);
    let output = map(&config, record.to_str().unwrap(), &scratch.0);
    assert_eq!(status(&output), 2);
    assert_eq!(stdout(&output), "");
}

#[test]
fn a_lone_surrogate_escape_is_an_input_error() {
    let scratch = Scratch::new("lone-surrogate");
    let config = config(&scratch);
    let text = fixture("envelope-accepted.json").replace("\"sess-1\"", "\"\\ud800\"");
    let record = scratch.write("record.json", &text);
    let output = map(&config, record.to_str().unwrap(), &scratch.0);
    assert_eq!(status(&output), 2);
    assert_eq!(stdout(&output), "");
}

#[test]
fn an_out_of_range_seq_is_refused_at_seq_where_the_reference_names_prev_hash() {
    let scratch = Scratch::new("big-seq");
    let config = config(&scratch);
    let text = fixture("envelope-accepted.json")
        .replace("\"seq\": 0,", "\"seq\": 99999999999999999999999999,");
    let record = scratch.write("record.json", &text);
    let output = map(&config, record.to_str().unwrap(), &scratch.0);
    assert_eq!(
        stdout(&output),
        "{\"paths\": [\"/seq\"], \"reason\": \"schema_invalid\", \"result\": \"refused\"}\n"
    );
    assert_eq!(status(&output), 1);
}
