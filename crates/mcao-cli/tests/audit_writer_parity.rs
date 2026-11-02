//! `mcao audit append`, `verify`, and `project-ledger` replayed against the chain steps of the
//! pre-port reference baseline (DPES-957, DPES-884 AC4).
//!
//! The baseline is `tests/fixtures/pre-port-baseline/outputs/fm-audit-writer/chain`, generated only
//! from `bin/fm-audit-writer.py` at main `d80bcd15` (see that directory's README). The steps run in
//! the order `manifest.json` records, each through the real `mcao` binary against one store in a
//! scratch directory. Each step's stdout, stderr, and exit status are compared byte for byte with
//! the recorded `.stdout`, `.stderr`, and `.exit` files. After the last step the committed segment
//! and checkpoint must equal the recorded `store/` files. No expected value comes from this crate.
//!
//! `KNOWN_GAPS` names steps where the Rust command is known to differ from the reference. The test
//! pins the current Rust result for each one, so a fix fails the test until its entry is removed.
//! A mismatch on any other step fails the test.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

/// A recorded step the Rust command answers differently today.
struct KnownGap {
    /// The step id: `fm-audit-writer/chain/<step>`.
    id: &'static str,
    /// Why Rust differs, with the PR that documents it.
    reason: &'static str,
    /// The exit status Rust returns today.
    rust_exit: i32,
    /// The stdout Rust prints today.
    rust_stdout: &'static str,
}

/// No chain step is a known gap. The blocker and flag cases the reference handles differently
/// (`recovered` reporting, canonicalization refusal) are not in the baseline and are covered, or
/// stated as residual, in `audit_writer_cli`.
const KNOWN_GAPS: &[KnownGap] = &[];

/// The chain steps the manifest records: three appends, one refused append, `project-ledger`, and
/// `verify`. A regeneration that changes the count must update this value.
const EXPECTED_CASES: usize = 6;

/// One recorded reference step and the Rust invocation that reproduces it.
struct Step {
    id: String,
    verb: String,
    args: Vec<String>,
    expected_stdout: Vec<u8>,
    expected_stderr: Vec<u8>,
    expected_exit: i32,
}

/// What the Rust command did for one step.
#[derive(PartialEq, Eq)]
struct Observed {
    exit: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// A scratch root holding `store/` and `state/`, removed when the test ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("the clock is after the epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "mcao-audit-writer-parity-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(path.join("state")).expect("a scratch state directory");
        Self(path)
    }

    fn store(&self) -> PathBuf {
        self.0.join("store")
    }

    fn state(&self) -> PathBuf {
        self.0.join("state")
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

fn chain_dir() -> PathBuf {
    baseline_dir().join("outputs/fm-audit-writer/chain")
}

/// The chain steps in the manifest's order, read from the manifest's `steps` list.
fn manifest_chain_steps() -> Vec<String> {
    let text = fs::read_to_string(baseline_dir().join("manifest.json")).expect("the manifest");
    let manifest: Value = serde_json::from_str(&text).expect("the manifest parses");
    manifest["steps"]
        .as_array()
        .expect("a steps list")
        .iter()
        .filter_map(|step| step["path"].as_str())
        .filter(|path| path.starts_with("outputs/fm-audit-writer/chain/"))
        .map(str::to_owned)
        .collect()
}

/// Maps a recorded reference command line to the `mcao audit` verb and its arguments. Placeholders
/// resolve to the scratch root, this checkout, and the baseline directory.
fn parse_command(line: &str, scratch: &Path) -> (String, Vec<String>) {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let script_at = tokens
        .iter()
        .position(|token| token.starts_with("<reference>/bin/fm-audit-writer.py"))
        .expect("the recorded writer script");
    let verb = tokens[script_at + 1].to_owned();
    let repo = repo_root();
    let baseline = baseline_dir();
    let args = tokens[script_at + 2..]
        .iter()
        .map(|token| {
            token
                .replace("<tmp>", &scratch.display().to_string())
                .replace("<inputs>", &repo.display().to_string())
                .replace("<out>", &baseline.display().to_string())
        })
        .collect();
    (verb, args)
}

fn load_steps(scratch: &Path) -> Vec<Step> {
    manifest_chain_steps()
        .into_iter()
        .map(|path| {
            let name = path.rsplit('/').next().expect("a step name");
            let stem = chain_dir().join(name);
            let read = |ext: &str| -> Vec<u8> {
                let file = stem.with_extension(ext);
                fs::read(&file).unwrap_or_else(|err| panic!("{}: {err}", file.display()))
            };
            let command = String::from_utf8(read("command")).expect("a UTF-8 recorded command");
            let (verb, args) = parse_command(command.trim_end(), scratch);
            let expected_exit = String::from_utf8(read("exit"))
                .expect("a UTF-8 exit status")
                .trim()
                .parse::<i32>()
                .expect("an integer exit status");
            Step {
                id: format!("fm-audit-writer/chain/{name}"),
                verb,
                args,
                expected_stdout: read("stdout"),
                expected_stderr: read("stderr"),
                expected_exit,
            }
        })
        .collect()
}

/// Runs `mcao audit <verb> <args>` with the rejection log under the scratch state directory.
fn run_rust(step: &Step, state: &Path) -> Observed {
    let output = Command::new(env!("CARGO_BIN_EXE_mcao"))
        .args(["audit", step.verb.as_str()])
        .args(&step.args)
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
fn difference(step: &Step, observed: &Observed) -> Option<String> {
    if observed.exit != step.expected_exit {
        return Some(format!(
            "exit {}, reference exit {}",
            observed.exit, step.expected_exit
        ));
    }
    if observed.stdout != step.expected_stdout {
        return Some(format!(
            "stdout {:?}, reference stdout {:?}",
            String::from_utf8_lossy(&observed.stdout),
            String::from_utf8_lossy(&step.expected_stdout)
        ));
    }
    if observed.stderr != step.expected_stderr {
        return Some(format!(
            "stderr {:?}, reference stderr {:?}",
            String::from_utf8_lossy(&observed.stderr),
            String::from_utf8_lossy(&step.expected_stderr)
        ));
    }
    None
}

#[test]
fn chain_steps_match_the_reference() {
    let scratch = Scratch::new();
    let steps = load_steps(&scratch.0);
    assert_eq!(
        steps.len(),
        EXPECTED_CASES,
        "the manifest holds {EXPECTED_CASES} chain steps"
    );
    for gap in KNOWN_GAPS {
        assert!(
            steps.iter().any(|step| step.id == gap.id),
            "KNOWN_GAPS entry {} names no chain step ({})",
            gap.id,
            gap.reason
        );
    }

    let mut matched = 0;
    let mut gaps = 0;
    let mut failures = Vec::new();
    for step in &steps {
        let observed = run_rust(step, &scratch.state());
        let gap = KNOWN_GAPS.iter().find(|gap| gap.id == step.id);
        match (gap, difference(step, &observed)) {
            (None, None) => matched += 1,
            (None, Some(diff)) => failures.push(format!("{}: {diff}", step.id)),
            (Some(gap), Some(_)) => {
                let pinned =
                    observed.exit == gap.rust_exit && observed.stdout == gap.rust_stdout.as_bytes();
                if pinned {
                    gaps += 1;
                } else {
                    failures.push(format!(
                        "{}: known gap ({}) now answers exit {} stdout {:?}, pinned exit {} stdout {:?}",
                        step.id,
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
                step.id, gap.reason
            )),
        }
    }

    let recorded = chain_dir().join("store");
    for name in ["segment-00000000000000000000.jsonl", "checkpoint.json"] {
        let expected = fs::read(recorded.join(name)).expect("the recorded store file");
        let actual = fs::read(scratch.store().join(name)).expect("the replayed store file");
        if actual != expected {
            failures.push(format!(
                "store/{name}: differs from the recorded store file"
            ));
        }
    }

    eprintln!(
        "writer chain parity: {} steps, {matched} matched, {gaps} known gaps, {} failing",
        steps.len(),
        failures.len()
    );
    assert!(
        failures.is_empty(),
        "writer chain parity failures:\n{}",
        failures.join("\n")
    );
}
