//! Runs the real probe binary against the native backend and the stub harness.
//!
//! The expected support per operation differs by platform because the backends differ: the Unix
//! backend has pipes, the Windows backend has a Job Object and no terminal yet. Each table states
//! what that backend can honestly demonstrate, so a regression to a false `demonstrated` fails.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::Value;

const PROBE: &str = env!("CARGO_BIN_EXE_mcao-probe");

#[cfg(unix)]
const BACKEND: &str = "unix-pipe";
#[cfg(windows)]
const BACKEND: &str = "windows-job";

/// Expected `support` for each operation, in lifecycle order.
fn expected(scenario: &str) -> [(&'static str, &'static str); 7] {
    #[cfg(unix)]
    let interrupt = if scenario == "uncooperative" {
        "unresolved"
    } else {
        "demonstrated"
    };
    #[cfg(windows)]
    let interrupt = {
        let _ = scenario;
        "unsupported"
    };
    #[cfg(unix)]
    let terminal = "demonstrated";
    #[cfg(windows)]
    let terminal = "unsupported";

    [
        ("spawn", "demonstrated"),
        ("capture", terminal),
        ("send_text", terminal),
        ("interrupt", interrupt),
        ("agent_state", "demonstrated"),
        ("reconnect", "unsupported"),
        ("cleanup", "demonstrated"),
    ]
}

fn run_native(scenario: &str, extra: &[&str]) -> (Vec<Value>, String) {
    let output = Command::new(PROBE)
        .args([
            "--backend",
            "native",
            "--harness",
            "stub",
            "--scenario",
            scenario,
        ])
        .args(extra)
        .output()
        .expect("run mcao-probe");
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    let records: Value = serde_json::from_str(&stdout).expect("stdout is JSON");
    (records.as_array().expect("top-level array").clone(), stdout)
}

fn check_scenario(scenario: &str) {
    let (records, _) = run_native(scenario, &[]);
    let seen: Vec<(&str, &str)> = records
        .iter()
        .map(|record| {
            (
                record["operation"].as_str().unwrap(),
                record["support"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(seen, expected(scenario), "records: {records:#?}");

    let handle = records[0]["handle"].as_str().expect("spawn handle");
    assert!(handle.starts_with(BACKEND), "handle {handle}");
    for record in &records {
        assert_eq!(record["backend"], BACKEND);
        assert_eq!(record["harness"], "stub");
        assert_eq!(record["execution"], "native");
        assert_eq!(record["handle"], handle);
        assert_ne!(record["support"], "mock_only");
    }
    // A capability the backend lacks is recorded with its reason, never as a bare failure.
    for record in records.iter().filter(|r| r["support"] == "unsupported") {
        assert!(
            record["outcome"]["reason"]
                .as_str()
                .is_some_and(|reason| !reason.is_empty())
        );
    }
}

#[test]
fn cooperative_run_records_only_what_the_native_backend_can_show() {
    check_scenario("cooperative");
}

#[test]
fn uncooperative_run_never_records_a_false_stop() {
    check_scenario("uncooperative");
}

fn scratch_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mcao-probe-{name}-{}", std::process::id()));
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

#[test]
fn evidence_out_writes_the_same_json_and_a_manifest_without_values() {
    let dir = scratch_dir("artifact");
    let path = dir.join("evidence.json");
    let (_, stdout) = run_native("cooperative", &["--evidence-out", path.to_str().unwrap()]);

    let written = fs::read_to_string(&path).expect("evidence file");
    assert_eq!(written, stdout.trim_end());

    let manifest_path = dir.join("evidence.json.redaction.json");
    let manifest: Value =
        serde_json::from_str(&fs::read_to_string(&manifest_path).expect("manifest file"))
            .expect("manifest is JSON");
    let rules: Vec<&str> = manifest["rules"]
        .as_array()
        .expect("rules")
        .iter()
        .map(|rule| rule["rule"].as_str().unwrap())
        .collect();
    assert_eq!(rules, ["home", "user", "cwd", "temp", "exe_dir"]);
    let manifest_text = manifest.to_string();
    for (key, value) in std::env::vars() {
        if matches!(key.as_str(), "HOME" | "USERPROFILE") && value.len() >= 4 {
            assert!(!manifest_text.contains(&value), "manifest leaked {key}");
        }
    }
    fs::remove_dir_all(&dir).expect("remove scratch dir");
}

#[test]
fn unwritable_evidence_path_exits_one() {
    let dir = scratch_dir("unwritable");
    let missing = dir.join("no-such-dir").join("evidence.json");
    let output = Command::new(PROBE)
        .args([
            "--backend",
            "native",
            "--harness",
            "stub",
            "--scenario",
            "cooperative",
            "--evidence-out",
            missing.to_str().unwrap(),
        ])
        .output()
        .expect("run mcao-probe");
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("could not write"));
    fs::remove_dir_all(&dir).expect("remove scratch dir");
}

/// Reads stub output lines until one equals `wanted`, and returns the lines read.
fn read_until(lines: &mut impl Iterator<Item = String>, wanted: &str) -> Vec<String> {
    let mut seen = Vec::new();
    for line in lines {
        let done = line == wanted;
        seen.push(line);
        if done {
            return seen;
        }
    }
    panic!("stub closed before printing `{wanted}`; saw {seen:?}");
}

fn drive_stub(extra: &[&str]) -> (Vec<String>, Vec<String>) {
    let mut child = Command::new(PROBE)
        .args(["stub-harness", "--lifetime-secs", "10"])
        .args(extra)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("start stub harness");
    let mut stdin = child.stdin.take().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap())
        .lines()
        .map(|line| line.expect("stub line"));

    let ready = read_until(&mut lines, "state: idle");
    stdin.write_all(b"hello\n").unwrap();
    let started = read_until(&mut lines, "state: running");
    stdin.write_all(&[0x1b]).unwrap();
    stdin.write_all(b"again\n").unwrap();
    let after = read_until(&mut lines, "state: running");

    drop(stdin);
    child.wait().expect("stub exits when stdin closes");
    (ready.into_iter().chain(started).collect(), after)
}

#[test]
fn stub_stops_a_running_turn_on_one_escape() {
    let (before, after) = drive_stub(&[]);
    assert_eq!(
        before,
        [
            "stub harness ready",
            "state: idle",
            "> hello",
            "state: running"
        ]
    );
    assert_eq!(
        after,
        [
            "[turn interrupted]",
            "state: idle",
            "> again",
            "state: running"
        ]
    );
}

#[test]
fn stub_started_with_ignore_escape_keeps_running() {
    let (_, after) = drive_stub(&["--ignore-escape"]);
    assert_eq!(after, ["> again", "state: running"]);
}
