//! `mcao audit validate` and `mcao audit activity-map` checked against the pre-port reference
//! baseline (DPES-957, DPES-884).
//!
//! The baseline is `tests/fixtures/pre-port-baseline/outputs`, generated only from the Python
//! reference at main `d80bcd15` (see that directory's README). Every case is read at test time:
//! the recorded `.command` gives the arguments, `.stdout`, `.stderr`, and `.exit` give the expected
//! result, and the `<inputs>` and `<out>` placeholders resolve to the repository root and the
//! baseline directory. Nothing is regenerated, and no expected value comes from this crate.
//!
//! Scope: the envelope `validate` cases and the activity-map `projection` cases. The writer,
//! sanitize, assignment, reassess, and birds-eye utilities have no `mcao audit` command and are
//! not compared here.
//!
//! Stderr is compared only where the reference wrote none. The reference's input-error stderr
//! (`fm-audit-envelope:` line and usage text) differs from the Rust command's wording, a documented
//! gap (PR 415), so for exit 2 cases the compared surface is stdout and the exit status.
//!
//! `KNOWN_GAPS` names the cases where the Rust command is known to differ from the reference. The
//! test pins the current Rust result for each one, so a fix fails the test until its entry is
//! removed. A mismatch on any other case fails the test.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

/// A baseline case the Rust command answers differently today.
struct KnownGap {
    /// The case id: `<utility>/<case>/<step>`.
    id: &'static str,
    /// Why Rust differs, with the PR that documents it.
    reason: &'static str,
    /// The exit status Rust returns today.
    rust_exit: i32,
    /// The stdout Rust prints today.
    rust_stdout: &'static str,
}

/// No baseline case is a known gap. The committed baseline cases do not reach the numeric
/// boundaries, `1e400`, lone surrogates, or deep nesting that PR 415 and PR 420 document, so those
/// gaps stay covered by their own ignored tests and are not claimed by this table.
const KNOWN_GAPS: &[KnownGap] = &[];

/// The cases the baseline holds for these two commands: 19 envelope validate cases and 2
/// activity-map projection cases. A regeneration that changes the count must update this value.
const EXPECTED_CASES: usize = 21;

/// One recorded reference step, with the Rust invocation that reproduces it.
struct Case {
    id: String,
    rust_subcommand: &'static str,
    args: Vec<String>,
    expected_stdout: Vec<u8>,
    expected_stderr: Vec<u8>,
    expected_exit: i32,
}

/// What the Rust command did for one case.
#[derive(PartialEq, Eq)]
struct Observed {
    exit: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// A scratch state directory, removed when the test ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("the clock is after the epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "mcao-audit-baseline-parity-{}-{nanos}",
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

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repository root")
}

fn baseline_dir() -> PathBuf {
    repo_root().join("tests/fixtures/pre-port-baseline")
}

/// The step names recorded in `dir`, from its `<step>.command` files, sorted.
fn steps_in(dir: &Path) -> Vec<String> {
    let mut steps: Vec<String> = fs::read_dir(dir)
        .unwrap_or_else(|err| panic!("{}: {err}", dir.display()))
        .map(|entry| entry.expect("a directory entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "command"))
        .map(|path| {
            path.file_stem()
                .expect("a file stem")
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    steps.sort();
    steps
}

/// The sorted subdirectory names of `dir`.
fn dirs_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap_or_else(|err| panic!("{}: {err}", dir.display()))
        .map(|entry| entry.expect("a directory entry").path())
        .filter(|path| path.is_dir())
        .map(|path| {
            path.file_name()
                .expect("a directory name")
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

/// Maps a recorded reference command line to the `mcao audit` subcommand and its arguments.
/// Placeholders resolve to this checkout, so the input is the committed file itself.
fn parse_command(line: &str) -> (&'static str, Vec<String>) {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let script_at = tokens
        .iter()
        .position(|token| token.starts_with("<reference>/bin/"))
        .expect("a reference script in the recorded command");
    let script = tokens[script_at].trim_start_matches("<reference>/bin/");
    let (rust_subcommand, reference_verb) = match script {
        "fm-audit-envelope.py" => ("validate", "validate"),
        "fm-audit-activity-map.py" => ("activity-map", "map"),
        other => panic!("no mcao audit command for {other}"),
    };
    assert_eq!(
        tokens[script_at + 1],
        reference_verb,
        "the recorded verb for {script}"
    );
    let repo = repo_root();
    let baseline = baseline_dir();
    let args = tokens[script_at + 2..]
        .iter()
        .map(|token| {
            token
                .replace("<inputs>", &repo.display().to_string())
                .replace("<out>", &baseline.display().to_string())
        })
        .collect();
    (rust_subcommand, args)
}

/// Loads every recorded case for the two commands from the committed baseline files.
fn load_cases() -> Vec<Case> {
    let outputs = baseline_dir().join("outputs");
    let mut cases = Vec::new();
    for case in dirs_in(&outputs.join("fm-audit-envelope")) {
        let dir = outputs.join("fm-audit-envelope").join(&case);
        for step in steps_in(&dir) {
            cases.push(load_case(
                &dir,
                &step,
                format!("fm-audit-envelope/{case}/{step}"),
            ));
        }
    }
    let projection = outputs.join("fm-audit-activity-map/projection");
    for step in steps_in(&projection) {
        cases.push(load_case(
            &projection,
            &step,
            format!("fm-audit-activity-map/projection/{step}"),
        ));
    }
    cases
}

fn load_case(dir: &Path, step: &str, id: String) -> Case {
    let read = |ext: &str| -> Vec<u8> {
        let path = dir.join(format!("{step}.{ext}"));
        fs::read(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
    };
    let command = String::from_utf8(read("command")).expect("a UTF-8 recorded command");
    let (rust_subcommand, args) = parse_command(command.trim_end());
    let expected_exit = String::from_utf8(read("exit"))
        .expect("a UTF-8 exit status")
        .trim()
        .parse::<i32>()
        .expect("an integer exit status");
    Case {
        id,
        rust_subcommand,
        args,
        expected_stdout: read("stdout"),
        expected_stderr: read("stderr"),
        expected_exit,
    }
}

/// Runs `mcao audit <subcommand> <args>` with the rejection log under `state`.
fn run_rust(case: &Case, state: &Path) -> Observed {
    let output = Command::new(env!("CARGO_BIN_EXE_mcao"))
        .args(["audit", case.rust_subcommand])
        .args(&case.args)
        .env_remove("FM_HOME")
        .env("FM_STATE_OVERRIDE", state)
        .stdin(Stdio::null())
        .output()
        .expect("mcao runs");
    Observed {
        exit: output.status.code().expect("an exit status"),
        stdout: output.stdout,
        stderr: output.stderr,
    }
}

/// The first way `observed` differs from the recorded reference result, if any.
fn difference(case: &Case, observed: &Observed) -> Option<String> {
    if observed.exit != case.expected_exit {
        return Some(format!(
            "exit {}, reference exit {}",
            observed.exit, case.expected_exit
        ));
    }
    if observed.stdout != case.expected_stdout {
        return Some(format!(
            "stdout {:?}, reference stdout {:?}",
            String::from_utf8_lossy(&observed.stdout),
            String::from_utf8_lossy(&case.expected_stdout)
        ));
    }
    if case.expected_stderr.is_empty() && !observed.stderr.is_empty() {
        return Some(format!(
            "stderr {:?}, reference wrote no stderr",
            String::from_utf8_lossy(&observed.stderr)
        ));
    }
    None
}

#[test]
fn baseline_cases_match_the_reference() {
    let cases = load_cases();
    assert_eq!(
        cases.len(),
        EXPECTED_CASES,
        "the baseline holds {EXPECTED_CASES} cases for these commands"
    );
    for gap in KNOWN_GAPS {
        assert!(
            cases.iter().any(|case| case.id == gap.id),
            "KNOWN_GAPS entry {} names no baseline case ({})",
            gap.id,
            gap.reason
        );
    }

    let state = Scratch::new();
    let mut matched = 0;
    let mut gaps = 0;
    let mut failures = Vec::new();
    for case in &cases {
        let observed = run_rust(case, &state.0);
        let gap = KNOWN_GAPS.iter().find(|gap| gap.id == case.id);
        match (gap, difference(case, &observed)) {
            (None, None) => matched += 1,
            (None, Some(diff)) => failures.push(format!("{}: {diff}", case.id)),
            (Some(gap), Some(_)) => {
                let pinned =
                    observed.exit == gap.rust_exit && observed.stdout == gap.rust_stdout.as_bytes();
                if pinned {
                    gaps += 1;
                } else {
                    failures.push(format!(
                        "{}: known gap ({}) now answers exit {} stdout {:?}, pinned exit {} stdout {:?}",
                        case.id,
                        gap.reason,
                        observed.exit,
                        String::from_utf8_lossy(&observed.stdout),
                        gap.rust_exit,
                        gap.rust_stdout
                    ));
                }
            }
            (Some(gap), None) => failures.push(format!(
                "{}: matches the reference now; remove its KNOWN_GAPS entry ({})",
                case.id, gap.reason
            )),
        }
    }

    eprintln!(
        "baseline parity: {} fixtures, {matched} matched, {gaps} known gaps, {} failing",
        cases.len(),
        failures.len()
    );
    assert!(
        failures.is_empty(),
        "baseline parity failures:\n{}",
        failures.join("\n")
    );
}
