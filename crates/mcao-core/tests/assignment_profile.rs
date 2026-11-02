//! Parity tests for the assignment-profile core. Each case under tests/fixtures/assignment_profile/cases
//! records what bin/fm-assignment-profile.py printed (stdout, stderr and exit code) for one record.
//! The fixtures are written by tests/fixtures/assignment_profile/generate.py; these tests read them and
//! never run Python.

use mcao_core::assignment_profile::{self, CanonicalJson, Pins, Refusal};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

const MIN_CASES: usize = 40;

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/assignment_profile")
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|err| panic!("reading {}: {err}", path.display()))
}

struct Outcome {
    stdout: String,
    stderr: String,
    exit: i32,
}

fn pins_from(case: &Value) -> Pins {
    let pins = &case["pins"];
    Pins {
        task: pins["task"].as_str().map(str::to_string),
        target_branch: pins["target_branch"].as_str().map(str::to_string),
        base_commit: pins["base_commit"].as_str().map(str::to_string),
        require_current: pins["require_current"].as_bool().unwrap_or(false),
    }
}

fn accepted(stdout: String) -> Outcome {
    Outcome {
        stdout,
        stderr: String::new(),
        exit: 0,
    }
}

fn refused(refusal: &Refusal) -> Outcome {
    Outcome {
        stdout: String::new(),
        stderr: format!("{}\n", refusal.stderr_line()),
        exit: refusal.exit_code(),
    }
}

fn run(case: &Value, root: &Path, record: &[u8]) -> Outcome {
    let checked: Result<CanonicalJson, Refusal> =
        assignment_profile::validate(root, record, &pins_from(case));
    match (case["command"].as_str(), checked) {
        (_, Err(refusal)) => refused(&refusal),
        (Some("validate"), Ok(decision)) => accepted(decision.as_str().to_string()),
        (Some("render"), Ok(decision)) => accepted(assignment_profile::render(&decision)),
        (Some("meta"), Ok(decision)) => accepted(assignment_profile::meta(&decision)),
        (command, Ok(_)) => panic!("unknown command {command:?}"),
    }
}

#[test]
fn every_case_matches_the_recorded_python_output() {
    let mut dirs: Vec<PathBuf> = fs::read_dir(fixture_dir().join("cases"))
        .expect("reading the parity cases")
        .map(|entry| entry.expect("reading a parity case").path())
        .collect();
    dirs.sort();
    assert!(
        dirs.len() >= MIN_CASES,
        "expected at least {MIN_CASES} parity cases, found {}",
        dirs.len()
    );

    let mut failures = Vec::new();
    for dir in &dirs {
        let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        let case: Value = serde_json::from_str(&read(&dir.join("case.json"))).expect("case.json");
        let root = fixture_dir().join(case["code_root"].as_str().expect("code_root"));
        let record = fs::read(dir.join("record.json")).expect("record.json");
        let got = run(&case, &root, &record);

        let want_stdout = read(&dir.join("expected.stdout"));
        let want_stderr = read(&dir.join("expected.stderr"));
        let want_exit: i32 = read(&dir.join("expected.exit"))
            .trim()
            .parse()
            .expect("expected.exit");
        if got.stdout != want_stdout || got.stderr != want_stderr || got.exit != want_exit {
            failures.push(format!(
                "{name}: exit {} (want {want_exit})\n  stdout: {:?}\n  want:   {:?}\n  stderr: {:?}\n  want:   {:?}",
                got.exit, got.stdout, want_stdout, got.stderr, want_stderr
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} parity cases differ:\n{}",
        failures.len(),
        dirs.len(),
        failures.join("\n")
    );
}
