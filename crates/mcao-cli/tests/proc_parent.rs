//! Command-line contract for `mcao proc parent` (DPES-894): JSON and text output for a live child,
//! the PID-reuse guard on either side of its one-second tolerance, and the refusal for a process
//! that has already exited.
//!
//! Each test starts a `sleep` child, so the child's parent is this test process. The suite is
//! Unix-only because it relies on `sleep`.

#![cfg(unix)]

use std::process::{Child, Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

/// A `sleep` child that is killed and reaped when the test ends.
struct Sleeper(Child);

impl Sleeper {
    fn start() -> Self {
        let child = Command::new("sleep")
            .arg("60")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("sleep starts");
        Self(child)
    }

    fn pid(&self) -> String {
        self.0.id().to_string()
    }
}

impl Drop for Sleeper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn proc_parent(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mcao"))
        .arg("proc")
        .arg("parent")
        .args(args)
        .output()
        .expect("mcao runs")
}

fn exit_code(output: &Output) -> Option<i32> {
    output.status.code()
}

fn stdout_text(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout is UTF-8")
}

fn stderr_text(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("stderr is UTF-8")
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock is after the Unix epoch")
        .as_millis() as u64
}

/// Reads the two fields of the one-line JSON object `mcao proc parent` prints.
fn parse_json(text: &str) -> (u32, u64) {
    let body = text
        .trim()
        .strip_prefix('{')
        .and_then(|rest| rest.strip_suffix('}'))
        .expect("stdout is a JSON object");
    let mut parent = None;
    let mut created = None;
    for field in body.split(',') {
        let (key, value) = field.split_once(':').expect("field is key: value");
        let value = value.trim();
        match key.trim().trim_matches('"') {
            "parent_pid" => parent = Some(value.parse().expect("parent_pid is an integer")),
            "creation_time_ms" => {
                created = Some(value.parse().expect("creation_time_ms is an integer"))
            }
            other => panic!("unexpected JSON field {other}"),
        }
    }
    (
        parent.expect("parent_pid is present"),
        created.expect("creation_time_ms is present"),
    )
}

#[test]
fn json_reports_this_process_as_parent_and_a_recent_start() {
    let child = Sleeper::start();
    let output = proc_parent(&["--pid", &child.pid(), "--output", "json"]);
    assert_eq!(
        exit_code(&output),
        Some(0),
        "stderr: {}",
        stderr_text(&output)
    );
    let (parent, created) = parse_json(&stdout_text(&output));
    assert_eq!(parent, std::process::id());
    assert!(created <= now_ms() + 2_000, "start is not in the future");
    assert!(
        created + 3_600_000 >= now_ms(),
        "child started within the hour"
    );
}

#[test]
fn text_prints_the_parent_pid_as_an_integer() {
    let child = Sleeper::start();
    let output = proc_parent(&["--pid", &child.pid(), "--output", "text"]);
    assert_eq!(
        exit_code(&output),
        Some(0),
        "stderr: {}",
        stderr_text(&output)
    );
    assert_eq!(stdout_text(&output).trim(), std::process::id().to_string());
}

#[test]
fn expect_start_passes_within_one_second_and_exits_5_beyond_it() {
    let child = Sleeper::start();
    let pid = child.pid();
    let (_, created) = parse_json(&stdout_text(&proc_parent(&[
        "--pid", &pid, "--output", "json",
    ])));

    let near = proc_parent(&[
        "--pid",
        &pid,
        "--output",
        "json",
        "--expect-start",
        &(created + 500).to_string(),
    ]);
    assert_eq!(exit_code(&near), Some(0), "stderr: {}", stderr_text(&near));

    let far = proc_parent(&[
        "--pid",
        &pid,
        "--output",
        "json",
        "--expect-start",
        &created.saturating_sub(1_500).to_string(),
    ]);
    assert_eq!(exit_code(&far), Some(5));
    assert!(far.stdout.is_empty(), "no parent is reported on a mismatch");
    assert!(
        stderr_text(&far).starts_with("Uncertain: "),
        "stderr: {}",
        stderr_text(&far)
    );
}

#[test]
fn a_process_that_already_exited_exits_2() {
    let mut child = Command::new("sleep")
        .arg("0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("sleep starts");
    let pid = child.id().to_string();
    child.wait().expect("sleep is reaped");

    let output = proc_parent(&["--pid", &pid, "--output", "text"]);
    assert_eq!(exit_code(&output), Some(2));
    assert!(output.stdout.is_empty());
    assert!(
        stderr_text(&output).starts_with("error: cannot query process"),
        "stderr: {}",
        stderr_text(&output)
    );
}

#[test]
fn an_unknown_output_format_is_a_usage_error() {
    let child = Sleeper::start();
    let output = proc_parent(&["--pid", &child.pid(), "--output", "xml"]);
    assert_eq!(exit_code(&output), Some(2));
    assert!(output.stdout.is_empty());
}
