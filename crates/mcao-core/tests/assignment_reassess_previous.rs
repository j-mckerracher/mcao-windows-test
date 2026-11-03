//! Replays every previous-record case in `tests/fixtures/assignment-reassess-previous` against the
//! Python reference at main d80bcd15 (`bin/fm-assignment-reassess.py reassess`). `generate.py` in
//! that directory writes `cases.json`, each case's reference stdout (`<case>.json`) and exit code
//! (`<case>.exit`), run offline as `python3 -I -B -X utf8 generate.py <ref-root>`. This test
//! rebuilds each repository with the generator's fixed identity and dates, parses the recorded
//! request in a child process started with only the generator's environment, and compares the
//! rendered stdout and the exit code with the recorded ones. A case whose reference status is
//! `unchanged` must come back `Unported`, which is the one divergence KNOWN_GAPS names for this
//! path. No expected output is generated here.

use mcao_core::assignment_reassess::{
    RequestError, collect, parse_request, reassess, render_blocked, render_reassess,
};
use serde_json::Value;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Set on the re-executed test binary that runs the cases. Without it the child test does nothing.
const CASE_CHILD: &str = "MCAO_ASSIGNMENT_REASSESS_PREVIOUS_CHILD";

/// The environment the generator gave every reference run and every repository command.
const GENERATOR_ENV: [(&str, &str); 8] = [
    ("GIT_CONFIG_NOSYSTEM", "1"),
    ("GIT_CONFIG_GLOBAL", "/dev/null"),
    ("GIT_AUTHOR_NAME", "fixture"),
    ("GIT_AUTHOR_EMAIL", "fixture@example.invalid"),
    ("GIT_COMMITTER_NAME", "fixture"),
    ("GIT_COMMITTER_EMAIL", "fixture@example.invalid"),
    ("GIT_AUTHOR_DATE", "2026-10-16T02:00:00Z"),
    ("GIT_COMMITTER_DATE", "2026-10-16T02:00:00Z"),
];

/// The Windows variables that `env::temp_dir()` reads. Every `env_clear()` child that may use a
/// temp directory gets them, because without them the child's temp directory is the Windows
/// directory, which is not writable. Unix reads `TMPDIR`, which the children already receive.
fn windows_temp_env() -> Vec<(&'static str, OsString)> {
    if !cfg!(windows) {
        return Vec::new();
    }
    ["TEMP", "TMP", "USERPROFILE"]
        .into_iter()
        .filter_map(|name| env::var_os(name).map(|value| (name, value)))
        .collect()
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/assignment-reassess-previous")
}

/// Runs the cases in a child process whose environment is the generator's, so the caller's git
/// configuration cannot reach `collect`.
#[test]
fn every_previous_case_matches_the_python_reference() {
    let exe = env::current_exe().expect("locating the test binary");
    let output = Command::new(exe)
        .args(["--exact", "previous_child_runs_every_case", "--nocapture"])
        .env_clear()
        .env("PATH", env::var_os("PATH").unwrap_or_default())
        .env("TMPDIR", env::temp_dir())
        .envs(windows_temp_env())
        .envs(GENERATOR_ENV)
        .env(CASE_CHILD, "1")
        .output()
        .expect("running the previous-record case child process");
    assert!(
        output.status.success(),
        "previous-record cases failed:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Re-executed by `every_previous_case_matches_the_python_reference` with [`CASE_CHILD`] set.
#[test]
fn previous_child_runs_every_case() {
    if env::var_os(CASE_CHILD).is_none() {
        return;
    }
    let cases: Vec<Value> = serde_json::from_str(
        &fs::read_to_string(fixture_root().join("cases.json")).expect("reading cases.json"),
    )
    .expect("parsing cases.json");
    let recorded = fs::read_dir(fixture_root())
        .expect("listing the fixture directory")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .filter(|path| path.file_name().is_some_and(|name| name != "cases.json"))
        .count();
    assert!(!cases.is_empty(), "cases.json holds no cases");
    assert_eq!(
        cases.len(),
        recorded,
        "every case needs one recorded reference output"
    );

    let scratch = env::temp_dir().join(format!("mcao-previous-cases-{}", std::process::id()));
    let _ = fs::remove_dir_all(&scratch);
    let mut failures = Vec::new();
    let mut replayed = 0;
    for case in &cases {
        let name = case["name"].as_str().expect("case name");
        match replay_case(&scratch, case) {
            Ok(Replay::Rendered) => replayed += 1,
            Ok(Replay::Unported) => {}
            Err(message) => failures.push(format!("{name}: {message}")),
        }
    }
    let _ = fs::remove_dir_all(&scratch);
    assert!(
        failures.is_empty(),
        "{} of {} previous-record cases differ from the reference:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
    assert!(replayed > 0, "no case was replayed to a rendered outcome");
}

/// How one case came out: rendered and byte-compared, or an unchanged record that is unported.
enum Replay {
    Rendered,
    Unported,
}

fn replay_case(scratch: &Path, case: &Value) -> Result<Replay, String> {
    let name = case["name"].as_str().ok_or("case has no name")?;
    let repo = scratch.join(name);
    let base = build_repo(&repo, case)?;
    let text = case["request"].to_string().replace("$BASE", &base);
    let (stdout, exit) = match parse_request(&text) {
        Ok(request) => {
            let collection = collect(&repo, &request.comparison_base_commit);
            match reassess(&collection, &request) {
                Ok(decision) => (render_reassess(&decision), 0),
                Err(RequestError::Unported(_)) => return expect_unchanged(name),
                Err(err) => return Err(format!("unexpected refusal: {err:?}")),
            }
        }
        Err(RequestError::Blocked(reasons)) => (render_blocked(&reasons), 1),
        Err(err) => return Err(format!("request refused: {err:?}")),
    };
    let recorded_exit = fs::read_to_string(fixture_root().join(format!("{name}.exit")))
        .map_err(|err| format!("reading the recorded exit code: {err}"))?;
    let recorded_exit: i32 = recorded_exit
        .trim()
        .parse()
        .map_err(|err| format!("parsing the recorded exit code: {err}"))?;
    if exit != recorded_exit {
        return Err(format!("exit {exit}, recorded {recorded_exit}"));
    }
    let expected = fs::read(fixture_root().join(format!("{name}.json")))
        .map_err(|err| format!("reading the recorded output: {err}"))?;
    if stdout.as_bytes() == expected.as_slice() {
        Ok(Replay::Rendered)
    } else {
        Err(format!(
            "stdout differs from the recorded output ({} bytes replayed, {} recorded)",
            stdout.len(),
            expected.len()
        ))
    }
}

/// A case the port refuses as unported must be one the reference reports as `unchanged`, exit 0.
fn expect_unchanged(name: &str) -> Result<Replay, String> {
    let recorded: Value = serde_json::from_slice(
        &fs::read(fixture_root().join(format!("{name}.json")))
            .map_err(|err| format!("reading the recorded output: {err}"))?,
    )
    .map_err(|err| format!("parsing the recorded output: {err}"))?;
    let exit = fs::read_to_string(fixture_root().join(format!("{name}.exit")))
        .map_err(|err| format!("reading the recorded exit code: {err}"))?;
    if recorded["status"] == "unchanged" && exit.trim() == "0" {
        Ok(Replay::Unported)
    } else {
        Err(format!(
            "unported, but the reference status is {} with exit {}",
            recorded["status"],
            exit.trim()
        ))
    }
}

/// Builds the base commit and then the worktree edits of one case, as the generator does.
fn build_repo(repo: &Path, case: &Value) -> Result<String, String> {
    fs::create_dir_all(repo).map_err(|err| format!("creating the repository: {err}"))?;
    git(repo, &["-c", "init.defaultBranch=main", "init", "-q"])?;
    for entry in entries(case, "base")? {
        write_entry(repo, entry)?;
    }
    git(repo, &["add", "-A"])?;
    git(repo, &["commit", "-q", "-m", "base"])?;
    let base = git(repo, &["rev-parse", "HEAD"])?;
    for entry in entries(case, "worktree")? {
        write_entry(repo, entry)?;
    }
    Ok(base.trim().to_string())
}

fn entries<'a>(case: &'a Value, key: &str) -> Result<&'a Vec<Value>, String> {
    case[key]
        .as_array()
        .ok_or_else(|| format!("case has no {key} list"))
}

fn write_entry(repo: &Path, entry: &Value) -> Result<(), String> {
    let path = repo.join(entry["path"].as_str().ok_or("entry has no path")?);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| format!("creating a directory: {err}"))?;
    }
    let data = entry["text"]
        .as_str()
        .ok_or("entry has no text")?
        .as_bytes()
        .to_vec();
    fs::write(&path, data).map_err(|err| format!("writing an entry: {err}"))
}

/// Runs git with the generator's environment and returns its stdout.
fn git(repo: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env_clear()
        .env("PATH", env::var_os("PATH").unwrap_or_default())
        .envs(windows_temp_env())
        .envs(GENERATOR_ENV)
        .output()
        .map_err(|err| format!("running git: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    String::from_utf8(output.stdout).map_err(|err| format!("git output: {err}"))
}
