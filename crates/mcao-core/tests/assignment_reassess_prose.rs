//! Replays every prose case in `tests/fixtures/assignment-reassess-prose` against the Python
//! reference at main d80bcd15 (`bin/fm-assignment-reassess.py reassess`). `generate.py` in that
//! directory writes `cases.json` and each case's reference stdout, run offline as
//! `python3 -I -B crates/mcao-core/tests/fixtures/assignment-reassess-prose/generate.py <ref-root>`
//! with `<ref-root>` extracted from `git archive d80bcd15`. This test rebuilds each repository with
//! the generator's fixed identity and dates, runs `collect`, `parse_request` and `reassess` in a
//! child process started with only the generator's environment, and compares the rendered decision
//! with the recorded stdout byte for byte. No expected output is generated here.

use mcao_core::assignment_reassess::{collect, parse_request, reassess, render_reassess};
use serde_json::Value;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Set on the re-executed test binary that runs the cases. Without it the child test does nothing.
const CASE_CHILD: &str = "MCAO_ASSIGNMENT_REASSESS_PROSE_CHILD";

/// Cases whose file name ends in a dot. Win32 path parsing strips a trailing dot from a file name
/// unless the `\\?\` prefix is used (Microsoft naming conventions), so on Windows the replay
/// creates `a` for the case's `a.`. The harness uses ordinary paths, so these are skipped on
/// Windows only. Their classification is covered without the disk by
/// `path_class_follows_python_splitext`.
const WINDOWS_UNREPRESENTABLE: [&str; 2] = ["path-trailing-dot", "path-trailing-dot-in-dir"];

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
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/assignment-reassess-prose")
}

/// Runs the cases in a child process whose environment is the generator's, so the caller's git
/// configuration cannot reach `collect`.
#[test]
fn every_prose_case_matches_the_python_reference() {
    let exe = env::current_exe().expect("locating the test binary");
    let output = Command::new(exe)
        .args(["--exact", "prose_child_runs_every_case", "--nocapture"])
        .env_clear()
        .env("PATH", env::var_os("PATH").unwrap_or_default())
        .env("TMPDIR", env::temp_dir())
        .envs(windows_temp_env())
        .envs(GENERATOR_ENV)
        .env(CASE_CHILD, "1")
        .output()
        .expect("running the prose case child process");
    assert!(
        output.status.success(),
        "prose cases failed:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    print!("{}", String::from_utf8_lossy(&output.stdout));
}

/// Re-executed by `every_prose_case_matches_the_python_reference` with [`CASE_CHILD`] set.
/// Without the variable it does nothing.
#[test]
fn prose_child_runs_every_case() {
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

    let scratch = env::temp_dir().join(format!("mcao-prose-cases-{}", std::process::id()));
    let _ = fs::remove_dir_all(&scratch);
    let mut failures = Vec::new();
    let mut replayed = 0;
    for case in &cases {
        let name = case["name"].as_str().expect("case name");
        if cfg!(windows) && WINDOWS_UNREPRESENTABLE.contains(&name) {
            println!("known gap on Windows, not replayed: {name}");
            continue;
        }
        replayed += 1;
        if let Err(message) = replay_case(&scratch, case) {
            failures.push(format!("{name}: {message}"));
        }
    }
    println!("replayed {replayed} of {} prose cases", cases.len());
    let _ = fs::remove_dir_all(&scratch);
    let expected_replayed = if cfg!(windows) {
        cases.len() - WINDOWS_UNREPRESENTABLE.len()
    } else {
        cases.len()
    };
    assert_eq!(
        replayed, expected_replayed,
        "replayed {replayed} prose cases, expected {expected_replayed} on this platform"
    );
    assert!(
        failures.is_empty(),
        "{} of {} prose cases differ from the reference:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}

fn replay_case(scratch: &Path, case: &Value) -> Result<(), String> {
    let name = case["name"].as_str().ok_or("case has no name")?;
    let repo = scratch.join(name);
    let base = build_repo(&repo, case)?;
    let text = case["request"].to_string().replace("$BASE", &base);
    let request = parse_request(&text).map_err(|err| format!("request refused: {err:?}"))?;
    let collection = collect(&repo, &request.comparison_base_commit);
    let decision = reassess(&collection, &request).map_err(|err| format!("unported: {err:?}"))?;
    let rendered = render_reassess(&decision);
    let expected = fs::read(fixture_root().join(format!("{name}.json")))
        .map_err(|err| format!("reading the recorded output: {err}"))?;
    if rendered.as_bytes() == expected.as_slice() {
        Ok(())
    } else {
        Err(format!(
            "stdout differs from the recorded output ({} bytes replayed, {} recorded)",
            rendered.len(),
            expected.len()
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
    if entry["delete"].as_bool() == Some(true) {
        return fs::remove_file(&path).map_err(|err| format!("deleting an entry: {err}"));
    }
    let data = match entry["hex"].as_str() {
        Some(hex) => decode_hex(hex)?,
        None => entry["text"]
            .as_str()
            .ok_or("entry has neither text nor hex")?
            .as_bytes()
            .to_vec(),
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| format!("creating a directory: {err}"))?;
    }
    fs::write(&path, data).map_err(|err| format!("writing an entry: {err}"))
}

fn decode_hex(hex: &str) -> Result<Vec<u8>, String> {
    (0..hex.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&hex[index..index + 2], 16)
                .map_err(|err| format!("decoding hex: {err}"))
        })
        .collect()
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
