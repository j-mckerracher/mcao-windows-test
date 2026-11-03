//! Replays every assignment-reassess case recorded by the Python reference at main d80bcd15
//! (tests/fixtures/pre-port-baseline). The fixture steps and their committed inputs are read at
//! run time, the synthetic git repository is rebuilt with the generator's fixed identity and dates,
//! and each step's stdout, stderr and exit status are compared byte for byte. No expected output
//! is generated here. The replay runs in a child process whose environment is the generator's, so
//! the caller's git configuration cannot reach `collect`.

use mcao_core::assignment_reassess::{
    collect, parse_request, reassess, render_collect, render_reassess,
};
use mcao_core::sha256::hex_digest;
use serde_json::Value;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use std::process::Command;

const TOOL: &str = "fm-assignment-reassess.py";

struct Outcome {
    stdout: String,
    stderr: String,
    exit: i32,
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let dir = env::temp_dir().join(format!("mcao-{label}-{}", process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("creating the scratch directory");
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/pre-port-baseline")
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|err| panic!("reading {}: {err}", path.display()))
}

/// The environment the generator gave every reassess step: `subprocess.run(env=...)` replaces the
/// caller's environment, so the only git settings that survive are the ones listed here.
const GENERATOR_GIT_ENV: [(&str, &str); 2] = [
    ("GIT_CONFIG_NOSYSTEM", "1"),
    ("GIT_CONFIG_GLOBAL", "/dev/null"),
];

/// Set on the re-executed test binary that runs the replay. Without it the child test does nothing.
const REPLAY_CHILD: &str = "MCAO_ASSIGNMENT_REASSESS_REPLAY_CHILD";

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

/// Runs git with the generator's environment: no system or global config and a fixed identity.
fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env_clear()
        .env("PATH", env::var_os("PATH").unwrap_or_default())
        .envs(windows_temp_env())
        .envs(git_env())
        .output()
        .expect("running git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git output is UTF-8")
        .trim()
        .to_string()
}

fn git_env() -> [(&'static str, &'static str); 8] {
    [
        GENERATOR_GIT_ENV[0],
        GENERATOR_GIT_ENV[1],
        ("GIT_AUTHOR_NAME", "fixture"),
        ("GIT_AUTHOR_EMAIL", "fixture@example.invalid"),
        ("GIT_COMMITTER_NAME", "fixture"),
        ("GIT_COMMITTER_EMAIL", "fixture@example.invalid"),
        ("GIT_AUTHOR_DATE", "2026-10-16T02:00:00Z"),
        ("GIT_COMMITTER_DATE", "2026-10-16T02:00:00Z"),
    ]
}

/// Rebuilds the base repository that `run_reassess` in the generator builds, and returns its HEAD.
fn build_base_repo(repo: &Path) -> String {
    let status = Command::new("git")
        .args(["-c", "init.defaultBranch=main", "init", "-q"])
        .arg(repo)
        .env_clear()
        .env("PATH", env::var_os("PATH").unwrap_or_default())
        .envs(windows_temp_env())
        .envs(git_env())
        .status()
        .expect("running git init");
    assert!(status.success(), "git init failed");
    for (rel, text) in [
        ("README.md", "# fixture\n"),
        ("docs/guide.md", "guide\n"),
        ("bin/tool.sh", "#!/bin/sh\necho tool\n"),
    ] {
        let full = repo.join(rel);
        fs::create_dir_all(full.parent().expect("file has a parent"))
            .expect("creating a directory");
        fs::write(&full, text).expect("writing a fixture file");
    }
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "base"]);
    git(repo, &["rev-parse", "HEAD"])
}

/// Produces the outcome of one recorded step against the repository in its recorded order.
fn replay(step: &str, repo: &Path) -> Outcome {
    let case = fixture_root().join("outputs/fm-assignment-reassess/repo");
    match step {
        "collect-clean" => collect_outcome(repo),
        "collect-dirty" => {
            fs::write(repo.join("README.md"), "# fixture changed\n").expect("editing README.md");
            fs::write(repo.join("docs/new.md"), "untracked\n").expect("adding docs/new.md");
            collect_outcome(repo)
        }
        "reassess-high-risk" => reassess_outcome(repo, &case.join("inputs/request-high-risk.json")),
        "reassess-no-trigger" => {
            reassess_outcome(repo, &case.join("inputs/request-no-trigger.json"))
        }
        other => panic!("no replay for recorded step {other}"),
    }
}

fn collect_outcome(repo: &Path) -> Outcome {
    let base = git(repo, &["rev-parse", "HEAD"]);
    let collection = collect(repo, &base);
    Outcome {
        stdout: render_collect(&collection),
        stderr: String::new(),
        exit: 0,
    }
}

fn reassess_outcome(repo: &Path, request_path: &Path) -> Outcome {
    let request = parse_request(&read(request_path)).expect("committed request is accepted");
    let collection = collect(repo, &request.comparison_base_commit);
    let decision = reassess(&collection, &request).expect("the committed request is fresh");
    Outcome {
        stdout: render_reassess(&decision),
        stderr: String::new(),
        exit: 0,
    }
}

fn compare(name: &str, step: &Value, outcome: &Outcome) -> Vec<String> {
    let path = step["path"].as_str().expect("manifest step path");
    let recorded_dir =
        fixture_root().join(Path::new(path).parent().expect("step path has a parent"));
    let leaf = Path::new(path)
        .file_name()
        .and_then(|leaf| leaf.to_str())
        .expect("step path has a leaf");
    let mut failures = Vec::new();
    let expected_stdout =
        fs::read(recorded_dir.join(format!("{leaf}.stdout"))).expect("recorded stdout");
    let expected_stderr =
        fs::read(recorded_dir.join(format!("{leaf}.stderr"))).expect("recorded stderr");
    let expected_exit: i32 = read(&recorded_dir.join(format!("{leaf}.exit")))
        .trim()
        .parse()
        .expect("recorded exit status");
    if hex_digest(&expected_stdout) != step["stdout_sha256"].as_str().unwrap_or_default() {
        failures.push(format!(
            "{name}: recorded stdout does not match the manifest hash"
        ));
    }
    if outcome.stdout.as_bytes() != expected_stdout.as_slice() {
        failures.push(format!(
            "{name}: stdout differs from the recorded output ({} bytes replayed, {} recorded)",
            outcome.stdout.len(),
            expected_stdout.len()
        ));
    }
    if outcome.stderr.as_bytes() != expected_stderr.as_slice() {
        failures.push(format!("{name}: stderr differs from the recorded output"));
    }
    if outcome.exit != expected_exit {
        failures.push(format!(
            "{name}: exit {} replayed, {expected_exit} recorded",
            outcome.exit
        ));
    }
    failures
}

/// Runs the replay in a child process started with only the generator's environment, as the
/// generator's `subprocess.run(env=...)` did. The caller's git configuration does not reach it.
#[test]
fn every_reassess_fixture_replays_to_the_recorded_output() {
    let exe = env::current_exe().expect("locating the test binary");
    let output = Command::new(exe)
        .args([
            "--exact",
            "replay_child_runs_the_reassess_replay",
            "--nocapture",
        ])
        .env_clear()
        .env("PATH", env::var_os("PATH").unwrap_or_default())
        .env("TMPDIR", env::temp_dir())
        .envs(windows_temp_env())
        .envs(GENERATOR_GIT_ENV)
        .env(REPLAY_CHILD, "1")
        .output()
        .expect("running the replay child process");
    assert!(
        output.status.success(),
        "replay child failed:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Runs the replay with the outer environment carrying hostile git settings that ignore every `*.md`
/// file: `GIT_CONFIG_GLOBAL`, `HOME/.gitconfig`, `XDG_CONFIG_HOME/git/ignore` and `GIT_CONFIG_COUNT`.
/// The replay child must not inherit any of them, so it must still match the recorded output.
#[test]
fn replay_passes_under_a_hostile_global_git_config() {
    let scratch = Scratch::new("hostile-git-config");
    let ignore = scratch.0.join("ignore");
    let config = scratch.0.join("gitconfig");
    let home = scratch.0.join("home");
    let xdg = scratch.0.join("xdg");
    fs::create_dir_all(xdg.join("git")).expect("creating the hostile XDG directory");
    fs::create_dir_all(&home).expect("creating the hostile home directory");
    fs::write(&ignore, "*.md\n").expect("writing the hostile ignore file");
    fs::write(xdg.join("git").join("ignore"), "*.md\n").expect("writing the XDG ignore file");
    let hostile = format!("[core]\n\texcludesFile = {}\n", ignore.display());
    fs::write(&config, &hostile).expect("writing the hostile git config");
    fs::write(home.join(".gitconfig"), &hostile).expect("writing the hostile home git config");
    let exe = env::current_exe().expect("locating the test binary");
    let output = Command::new(exe)
        .args([
            "--exact",
            "every_reassess_fixture_replays_to_the_recorded_output",
            "--nocapture",
        ])
        .env("GIT_CONFIG_GLOBAL", &config)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &xdg)
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.excludesFile")
        .env("GIT_CONFIG_VALUE_0", &ignore)
        .output()
        .expect("running the replay driver under a hostile git config");
    assert!(
        output.status.success(),
        "replay failed under a hostile global git config:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Re-executed by `every_reassess_fixture_replays_to_the_recorded_output` with [`REPLAY_CHILD`]
/// set. Without the variable it does nothing.
#[test]
fn replay_child_runs_the_reassess_replay() {
    if env::var_os(REPLAY_CHILD).is_none() {
        return;
    }
    let manifest: Value = serde_json::from_str(&read(&fixture_root().join("manifest.json")))
        .expect("parsing the fixture manifest");
    let steps: Vec<&Value> = manifest["steps"]
        .as_array()
        .expect("manifest steps")
        .iter()
        .filter(|step| {
            step["command"]
                .as_str()
                .is_some_and(|command| command.contains(TOOL))
        })
        .collect();
    assert!(!steps.is_empty(), "the manifest records no reassess steps");

    let scratch = Scratch::new("assignment-reassess");
    let repo = scratch.0.join("reassess-repo");
    fs::create_dir_all(&repo).expect("creating the repository directory");
    let head = build_base_repo(&repo);
    let recorded_base = steps[0]["command"]
        .as_str()
        .and_then(|command| command.split_once("--base-commit "))
        .map(|(_, rest)| rest.split_whitespace().next().unwrap_or_default())
        .expect("collect command records the base commit");
    assert_eq!(
        head, recorded_base,
        "rebuilt base repository differs from the recorded base commit"
    );

    let recorded_cases = fs::read_dir(fixture_root().join("outputs/fm-assignment-reassess/repo"))
        .expect("listing the recorded reassess fixtures")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".exit"))
        .count();
    assert_eq!(
        recorded_cases,
        steps.len(),
        "recorded reassess fixtures differ from the manifest count"
    );

    let mut failures = Vec::new();
    let mut replayed = 0;
    for step in &steps {
        let path = step["path"].as_str().expect("manifest step path");
        let name = Path::new(path)
            .file_name()
            .and_then(|leaf| leaf.to_str())
            .expect("step path has a leaf");
        let outcome = replay(name, &repo);
        failures.extend(compare(name, step, &outcome));
        replayed += 1;
    }
    assert_eq!(
        replayed, recorded_cases,
        "replayed case count differs from the recorded fixture count"
    );
    assert!(
        failures.is_empty(),
        "{} of {replayed} reassess fixtures differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
