use std::process::{Command, Output};

fn probe(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mcao-probe"))
        .args(args)
        .output()
        .expect("run mcao-probe")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn missing_backend_is_refused_with_exit_two() {
    let output = probe(&[]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("missing required flag --backend"));
}

#[test]
fn herdr_backend_is_refused_with_exit_two() {
    let output = probe(&[
        "--backend",
        "herdr",
        "--harness",
        "mock",
        "--scenario",
        "cooperative",
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("backend `herdr` is not implemented"));
}

#[test]
fn home_flag_is_refused_as_unknown() {
    let output = probe(&[
        "--backend",
        "mock",
        "--harness",
        "mock",
        "--scenario",
        "cooperative",
        "--home",
        "/tmp/not-used",
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("unknown flag `--home`"));
}

#[test]
fn cooperative_run_writes_seven_mock_only_records_as_json() {
    let output = probe(&[
        "--backend",
        "mock",
        "--harness",
        "mock",
        "--scenario",
        "cooperative",
    ]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));

    let records: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout is JSON");
    let records = records.as_array().expect("top-level array");
    assert_eq!(records.len(), 7);
    assert_eq!(records[0]["operation"], "spawn");
    assert_eq!(records[6]["operation"], "cleanup");
    for record in records {
        assert_eq!(record["backend"], "mock");
        assert_eq!(record["harness"], "mock");
        assert_eq!(record["execution"], "mock");
        assert_eq!(record["support"], "mock_only");
        assert!(record["platform"].is_string());
        assert!(record["duration_ms"].is_u64());
        assert!(record["timestamp_unix_ms"].is_u64());
    }
}

#[test]
fn uncooperative_run_exits_zero_and_records_unresolved_interrupt() {
    let output = probe(&[
        "--backend",
        "mock",
        "--harness",
        "mock",
        "--scenario",
        "uncooperative",
    ]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));

    let records: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout is JSON");
    let interrupt = records
        .as_array()
        .expect("top-level array")
        .iter()
        .find(|record| record["operation"] == "interrupt")
        .expect("interrupt recorded");
    assert_eq!(interrupt["support"], "unresolved");
    assert_eq!(interrupt["outcome"]["status"], "failed");
}

#[test]
fn native_backend_refuses_the_mock_harness_with_exit_two() {
    let output = probe(&[
        "--backend",
        "native",
        "--harness",
        "mock",
        "--scenario",
        "cooperative",
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("it runs only `stub`"));
}

#[test]
fn stub_harness_refuses_an_unknown_flag_with_exit_two() {
    let output = probe(&["stub-harness", "--bogus"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("unknown stub-harness flag `--bogus`"));
}
