//! Parity tests for `mcao util`. None of these run Python. Every expected value is a frozen
//! fixture produced once by the developer-only reference generator; see
//! `tests/fixtures/util-awaiting/provenance.txt`.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use mcao_core::util::clock::ms_from_secs;

const ROWS_CASES: usize = 35;
const SHOW_CASES: usize = 87;
const CLOCK_ROWS: usize = 10;
const TRACEBACK_HEADER: &str = "Traceback (most recent call last):";
#[cfg(unix)]
const UTIL_USAGE: &str = "usage: mcao util <now-ms|awaiting-hold-ids|awaiting-hold-object|nm-run-inventory|nm-spend-record>\n";
#[cfg(not(unix))]
const UTIL_USAGE: &str = "usage: mcao util <now-ms|awaiting-hold-ids|awaiting-hold-object>\n";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn mcao(args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mcao"));
    command.args(args);
    command
}

fn cases(kind: &str, expected: usize) -> Vec<PathBuf> {
    let dir = fixtures().join("util-awaiting").join(kind);
    let mut paths: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|err| panic!("read {}: {err}", dir.display()))
        .map(|entry| entry.expect("directory entry").path())
        .collect();
    paths.sort();
    assert_eq!(paths.len(), expected, "{kind} fixture count");
    paths
}

/// Runs one parser over every saved case of its kind. A Python crash saves its full
/// traceback, so the rule is: when the reference wrote a traceback, our stderr must be exactly
/// one line equal to the traceback's final exception line. Otherwise stderr must match exactly.
fn check_kind(kind: &str, command: &str, expected_cases: usize) {
    let mut failures = Vec::new();
    for case in cases(kind, expected_cases) {
        let name = case
            .file_name()
            .expect("case name")
            .to_string_lossy()
            .into_owned();
        let stdin = File::open(case.join("stdin")).expect("stdin fixture");
        let output = mcao(&["util", command])
            .stdin(stdin)
            .output()
            .expect("mcao runs");

        let want_stdout = fs::read(case.join("stdout")).expect("stdout fixture");
        let want_stderr = fs::read_to_string(case.join("stderr")).expect("stderr fixture");
        let want_exit: i32 = fs::read_to_string(case.join("exit"))
            .expect("exit fixture")
            .trim()
            .parse()
            .expect("exit status");
        let got_stderr = String::from_utf8(output.stderr).expect("ascii stderr");

        let stdout_ok = output.stdout == want_stdout;
        let exit_ok = output.status.code() == Some(want_exit);
        let stderr_ok = if want_stderr.starts_with(TRACEBACK_HEADER) {
            let want_last = want_stderr.lines().last().unwrap_or_default();
            got_stderr.lines().count() == 1 && got_stderr.trim_end_matches('\n') == want_last
        } else {
            got_stderr == want_stderr
        };
        if !(stdout_ok && exit_ok && stderr_ok) {
            failures.push(format!(
                "{kind}/{name}: stdout_ok={stdout_ok} exit_ok={exit_ok} stderr_ok={stderr_ok} \
                 got_exit={:?} want_exit={want_exit} got_stderr={got_stderr:?}",
                output.status.code()
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {expected_cases} {kind} cases differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn rows_parser_matches_frozen_reference_outputs() {
    check_kind("rows", "awaiting-hold-ids", ROWS_CASES);
}

#[test]
fn show_parser_matches_frozen_reference_outputs() {
    check_kind("show", "awaiting-hold-object", SHOW_CASES);
}

#[test]
fn clock_formula_matches_frozen_python_table() {
    let text = fs::read_to_string(fixtures().join("util-now-ms/now-ms.tsv")).expect("clock table");
    let mut rows = 0;
    for line in text.lines() {
        let (repr, want) = line.split_once('\t').expect("repr<TAB>int row");
        let secs: f64 = repr.parse().expect("python float repr");
        let want: i64 = want.parse().expect("int milliseconds");
        assert_eq!(ms_from_secs(secs), want, "int(float({repr}) * 1000)");
        rows += 1;
    }
    assert_eq!(rows, CLOCK_ROWS, "clock table row count");
}

#[test]
fn now_ms_prints_thirteen_digits_inside_the_call_window() {
    let before = wall_ms();
    let output = mcao(&["util", "now-ms"])
        .stdin(Stdio::null())
        .output()
        .expect("mcao runs");
    let after = wall_ms();

    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).expect("ascii output");
    let digits = text.strip_suffix('\n').expect("newline-terminated");
    assert_eq!(digits.len(), 13, "epoch milliseconds in this era");
    assert!(digits.bytes().all(|b| b.is_ascii_digit()), "digits only");
    let value: i64 = digits.parse().expect("integer");
    // The float path can land one millisecond off the integer clock, so allow one either way.
    assert!(
        value >= before - 1 && value <= after + 1,
        "{value} outside [{before}, {after}]"
    );
}

#[test]
fn no_arguments_prints_the_version_line() {
    let output = mcao(&[]).stdin(Stdio::null()).output().expect("mcao runs");
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!(
            "mcao {} (PROPOSED skeleton, no subcommands yet)\n",
            env!("CARGO_PKG_VERSION")
        )
    );
}

#[test]
fn unknown_util_command_prints_usage_and_exits_two() {
    let output = mcao(&["util", "nope"])
        .stdin(Stdio::null())
        .output()
        .expect("mcao runs");
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(String::from_utf8_lossy(&output.stderr), UTIL_USAGE);
}

fn wall_ms() -> i64 {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch");
    ms_from_secs(elapsed.as_secs_f64())
}
