//! Command-line contract for `mcao notify` (DPES-887): usage and refusal exit codes, a missing
//! handler, a failing handler, a handler past its deadline, and the argv each handler receives.
//!
//! Every test puts stub handlers, and nothing else, on `PATH`, so no real `open`, `xdg-open`,
//! `osascript` or `notify-send` runs and no banner is posted. The suite is Unix-only because the
//! stubs are shell scripts.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
const OPEN_HANDLER: &str = "open";
#[cfg(not(target_os = "macos"))]
const OPEN_HANDLER: &str = "xdg-open";

#[cfg(target_os = "macos")]
const WEDGE_HANDLER: &str = "osascript";
#[cfg(not(target_os = "macos"))]
const WEDGE_HANDLER: &str = "notify-send";

/// A scratch directory under cargo's per-target temporary directory. It holds the stub `bin/`
/// directory and the `argv` record, and is removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let path = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("notify-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create the scratch directory");
        Self(path)
    }

    fn bin(&self) -> PathBuf {
        let bin = self.0.join("bin");
        fs::create_dir_all(&bin).expect("create the stub directory");
        bin
    }

    fn record(&self) -> PathBuf {
        self.0.join("argv")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Writes an executable stub named `program` into the scratch `bin/`. The stub first records its
/// argv, NUL-separated, into the file named by `RECORD`, then runs `body`.
fn write_stub(dir: &Scratch, program: &str, body: &str) {
    let path = dir.bin().join(program);
    let script = format!("#!/bin/sh\nprintf '%s\\0' \"$@\" > \"$RECORD\"\n{body}\n");
    fs::write(&path, script).expect("write the stub");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
        .expect("make the stub executable");
}

/// The argv a stub recorded, one entry per argument.
fn recorded_argv(dir: &Scratch) -> Vec<String> {
    let bytes = fs::read(dir.record()).expect("the stub recorded its argv");
    let trimmed = bytes.strip_suffix(&[0]).unwrap_or(&bytes);
    trimmed
        .split(|byte| *byte == 0)
        .map(|item| String::from_utf8_lossy(item).into_owned())
        .collect()
}

/// Runs `mcao notify <args>` with `PATH` set to the scratch `bin/` alone and `RECORD` pointing at
/// the argv record, plus any `extra` environment.
fn notify(dir: &Scratch, args: &[&str], extra: &[(&str, &str)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mcao"))
        .arg("notify")
        .args(args)
        .env("PATH", dir.bin())
        .env("RECORD", dir.record())
        .envs(extra.iter().copied())
        .stdin(Stdio::null())
        .output()
        .expect("mcao runs")
}

fn exit_code(output: &Output) -> Option<i32> {
    output.status.code()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn open_without_a_target_is_a_usage_error() {
    let dir = Scratch::new("no-target");
    let output = notify(&dir, &["open"], &[]);
    assert_eq!(exit_code(&output), Some(2), "stderr: {}", stderr(&output));
}

#[test]
fn open_with_both_url_and_path_is_a_usage_error() {
    let dir = Scratch::new("both-targets");
    let output = notify(
        &dir,
        &["open", "--url", "https://example.test/", "--path", "/tmp/x"],
        &[],
    );
    assert_eq!(exit_code(&output), Some(2), "stderr: {}", stderr(&output));
}

#[test]
fn refused_uris_exit_4_and_never_reach_the_handler() {
    let dir = Scratch::new("refused");
    write_stub(&dir, OPEN_HANDLER, "");
    for uri in ["https://example.test/$HOME", "javascript:alert(1)"] {
        let output = notify(&dir, &["open", "--url", uri], &[]);
        assert_eq!(exit_code(&output), Some(4), "{uri} must be refused");
        assert!(!dir.record().exists(), "{uri} reached the handler");
    }
}

#[test]
fn open_url_hands_the_uri_to_the_handler_as_one_argument() {
    let dir = Scratch::new("open-url");
    write_stub(&dir, OPEN_HANDLER, "");
    let uri = "https://example.test/a?b=c";
    let output = notify(&dir, &["open", "--url", uri], &[]);
    assert_eq!(exit_code(&output), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(recorded_argv(&dir), [uri]);
}

#[test]
fn open_path_hands_an_encoded_file_uri_to_the_handler() {
    let dir = Scratch::new("open-path");
    write_stub(&dir, OPEN_HANDLER, "");
    let output = notify(&dir, &["open", "--path", "/tmp/a b(c)$.md"], &[]);
    assert_eq!(exit_code(&output), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(recorded_argv(&dir), ["file:///tmp/a%20b%28c%29%24.md"]);
}

#[test]
fn a_missing_open_handler_is_a_preflight_failure() {
    let dir = Scratch::new("missing-handler");
    // No stub is written, so the empty `bin/` leaves the handler off PATH.
    let output = notify(&dir, &["open", "--url", "https://example.test/"], &[]);
    assert_eq!(exit_code(&output), Some(3), "stderr: {}", stderr(&output));
}

#[test]
fn a_failing_open_handler_is_a_backend_failure() {
    let dir = Scratch::new("failing-handler");
    write_stub(&dir, OPEN_HANDLER, "exit 1");
    let output = notify(&dir, &["open", "--url", "https://example.test/"], &[]);
    assert_eq!(exit_code(&output), Some(1), "stderr: {}", stderr(&output));
}

#[test]
fn a_handler_past_its_deadline_is_killed_and_exits_1() {
    let dir = Scratch::new("deadline");
    // `exec` replaces the stub shell, so the deadline kills the sleep itself.
    write_stub(&dir, OPEN_HANDLER, "exec /bin/sleep 30");
    let started = Instant::now();
    let output = notify(
        &dir,
        &["open", "--url", "https://example.test/"],
        &[("FM_WEDGE_ALARM_TIMEOUT_SECS", "1")],
    );
    assert_eq!(exit_code(&output), Some(1), "stderr: {}", stderr(&output));
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "the handler was not killed at its deadline"
    );
}

#[test]
fn wedge_alarm_passes_a_dash_prefixed_summary_as_the_last_argument() {
    let dir = Scratch::new("wedge-dash");
    write_stub(&dir, WEDGE_HANDLER, "");
    let summary = "-ereturn \"injected\"";
    let output = notify(&dir, &["wedge-alarm", summary], &[]);
    assert_eq!(exit_code(&output), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(
        recorded_argv(&dir).last().map(String::as_str),
        Some(summary)
    );
}

#[test]
fn wedge_alarm_summary_is_data_and_never_runs_as_shell() {
    let dir = Scratch::new("wedge-shell");
    write_stub(&dir, WEDGE_HANDLER, "");
    let pwned = dir.0.join("pwned");
    let summary = format!("$(touch {}); `id`", pwned.display());
    let output = notify(&dir, &["wedge-alarm", summary.as_str()], &[]);
    assert_eq!(exit_code(&output), Some(0), "stderr: {}", stderr(&output));
    assert!(!pwned.exists(), "the summary ran as shell");
    assert_eq!(recorded_argv(&dir).last(), Some(&summary));
}
