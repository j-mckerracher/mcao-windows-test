//! End-to-end checks of `mcao toolchain` (DPES-890). Each test uses its own scratch directory under
//! the system temp dir. No test writes `docs/windows-toolchain.json` or installs a tool.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

const QUALIFY_KEYS: [&str; 7] = [
    "tools",
    "pilot_harness_launcher",
    "gh_auth_status",
    "git_credential_manager",
    "timestamp",
    "runner",
    "ci_job",
];

fn mcao(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mcao"))
        .args(args)
        .env_remove("CLICOLOR_FORCE")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .output()
        .expect("mcao runs")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// A scratch directory under the system temp dir, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "mcao-toolchain-{label}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("scratch directory is created");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Writes a real qualification record from this host and returns its path.
fn qualify_to(scratch: &Scratch) -> PathBuf {
    let record = scratch.path().join("record.json");
    let record_arg = record.to_str().expect("UTF-8 temp path");
    let output = mcao(&[
        "toolchain",
        "qualify",
        "--output-json",
        "--output-file",
        record_arg,
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    record
}

fn write_record(scratch: &Scratch, body: &str) -> PathBuf {
    let record = scratch.path().join("synthetic.json");
    std::fs::write(&record, body).expect("synthetic record is written");
    record
}

#[test]
fn qualify_writes_the_ac2_root_keys_and_known_verdicts() {
    let scratch = Scratch::new("keys");
    let record = text(&std::fs::read(qualify_to(&scratch)).expect("record is readable"));

    for key in QUALIFY_KEYS {
        assert!(
            record.contains(&format!("\"{key}\":")),
            "missing root key {key}"
        );
    }
    let verdicts: Vec<&str> = record
        .lines()
        .filter(|line| line.contains("\"verdict\":"))
        .collect();
    assert!(!verdicts.is_empty(), "the record carries verdicts");
    for line in verdicts {
        let known = ["supported\"", "unsupported\"", "blocked\""]
            .iter()
            .any(|suffix| line.trim_end_matches(',').ends_with(suffix));
        assert!(known, "unexpected verdict line: {line}");
    }
}

#[test]
fn qualify_prints_to_stdout_without_an_output_file() {
    let output = mcao(&["toolchain", "qualify", "--output-json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert!(text(&output.stdout).trim_start().starts_with('{'));
}

#[test]
fn qualify_without_output_json_is_a_usage_error() {
    let output = mcao(&["toolchain", "qualify"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
}

#[test]
fn no_mistakes_is_refused_on_every_host() {
    // AC10: no-mistakes is never supported on any host, so its delivery path exits 3 and prints no
    // success line. The refusal names the tool, the verdict, the re-run command, and the unapproved
    // install method.
    let scratch = Scratch::new("refuse");
    let record = qualify_to(&scratch);
    let record_arg = record.to_str().expect("UTF-8 temp path");
    let output = mcao(&[
        "toolchain",
        "check",
        "--evidence",
        record_arg,
        "--delivery-path",
        "no-mistakes",
    ]);
    assert_eq!(output.status.code(), Some(3), "{}", text(&output.stderr));
    assert!(output.stdout.is_empty(), "a refusal prints no success line");
    let message = text(&output.stderr);
    assert!(message.contains("no-mistakes"), "{message}");
    assert!(message.contains("qualify --output-json"), "{message}");
    assert!(message.contains("unapproved"), "{message}");
}

#[test]
fn all_required_supported_prints_the_ac11_line() {
    // AC11: every required tool is supported, so the check exits 0 with the success line.
    let scratch = Scratch::new("pass");
    let record = write_record(
        &scratch,
        r#"{"tools":[{"name":"git","verdict":"supported"},{"name":"gh","verdict":"supported"}]}"#,
    );
    let record_arg = record.to_str().expect("UTF-8 temp path");
    let output = mcao(&[
        "toolchain",
        "check",
        "--evidence",
        record_arg,
        "--delivery-path",
        "direct-PR",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(
        text(&output.stdout).trim(),
        "all required tools are supported"
    );
}

#[test]
fn unsupported_required_tool_exits_three_with_no_success_line() {
    // AC12: a required tool that is not supported never passes, and never falls back or skips.
    let scratch = Scratch::new("unsupported");
    let record = write_record(
        &scratch,
        r#"{"tools":[{"name":"git","verdict":"supported"},{"name":"gh","verdict":"unsupported"}]}"#,
    );
    let record_arg = record.to_str().expect("UTF-8 temp path");
    let output = mcao(&[
        "toolchain",
        "check",
        "--evidence",
        record_arg,
        "--delivery-path",
        "direct-PR",
    ]);
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert!(text(&output.stderr).contains("gh"));
}

#[test]
fn unknown_delivery_path_is_a_usage_error() {
    let scratch = Scratch::new("path");
    let record = write_record(&scratch, r#"{"tools":[]}"#);
    let record_arg = record.to_str().expect("UTF-8 temp path");
    let output = mcao(&[
        "toolchain",
        "check",
        "--evidence",
        record_arg,
        "--delivery-path",
        "merge-everything",
    ]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn unreadable_evidence_is_refused() {
    let scratch = Scratch::new("missing");
    let record = scratch.path().join("absent.json");
    let record_arg = record.to_str().expect("UTF-8 temp path");
    let output = mcao(&[
        "toolchain",
        "check",
        "--evidence",
        record_arg,
        "--delivery-path",
        "local-only",
    ]);
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
}
