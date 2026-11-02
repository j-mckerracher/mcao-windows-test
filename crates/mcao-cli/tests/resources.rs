//! `mcao resources` CLI contract (DPES-1019, DPES-866): `show` and `admit` run through the real
//! binary against temporary config and state directories.
//!
//! The host's own memory, CPU, and disk readings are measured for real, so every admission case
//! switches those limits off and decides on the running-agent count alone. Nothing here starts a
//! worker or reads a live Firstmate home.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value;

const RESOURCE_KEYS: [&str; 5] = [
    "memory_used_pct",
    "swap_used_pct",
    "cpu_used_pct",
    "disk_free_pct",
    "running_agents",
];

/// Turns off the three host-measured limits and the disk limit, so admission depends only on the
/// running-agent count.
const HOST_LIMITS_OFF: &str =
    "memory_used_pct off\nswap_used_pct off\ncpu_used_pct off\ndisk_free_pct off\n";

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mcao-resources-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

fn mcao(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mcao"))
        .args(args)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .output()
        .expect("mcao runs")
}

fn stdout_text(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout is UTF-8")
}

fn stderr_text(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("stderr is UTF-8")
}

fn stdout_json(output: &Output) -> Value {
    serde_json::from_str(&stdout_text(output)).expect("stdout is one JSON object")
}

/// A config directory whose `resource-thresholds` holds `text`.
fn config_dir(name: &str, text: &str) -> PathBuf {
    let dir = scratch(name);
    fs::write(dir.join("resource-thresholds"), text).expect("thresholds file");
    dir
}

/// A state directory holding one task record per `(id, body)` pair.
fn state_dir(root: &Path, name: &str, records: &[(&str, &str)]) -> PathBuf {
    let dir = root.join(name);
    fs::create_dir_all(&dir).expect("state directory");
    for (id, body) in records {
        fs::write(dir.join(format!("{id}.meta")), body).expect("task record");
    }
    dir
}

fn path_arg(path: &Path) -> &str {
    path.to_str().expect("test paths are UTF-8")
}

fn reading<'a>(snapshot: &'a Value, key: &str) -> &'a Value {
    snapshot["readings"]
        .as_array()
        .expect("readings is an array")
        .iter()
        .find(|reading| reading["kind"] == key)
        .unwrap_or_else(|| panic!("no {key} reading in {snapshot}"))
}

#[test]
fn show_json_prints_the_v1_snapshot_with_every_resource() {
    let config = scratch("show-json");
    let output = mcao(&[
        "resources",
        "show",
        "--json",
        "--config-dir",
        path_arg(&config),
    ]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_text(&output)
    );
    let value = stdout_json(&output);

    assert_eq!(value["schema"], "fm-resource-snapshot.v1");
    assert_eq!(
        value["platform"],
        format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
    );
    assert!(
        value["sampled_at"]
            .as_str()
            .is_some_and(|at| at.ends_with('Z'))
    );
    assert_eq!(value["config"]["source"], "defaults");
    assert_eq!(value["config"]["warning"], Value::Null);

    let keys: Vec<&str> = value["readings"]
        .as_array()
        .expect("readings is an array")
        .iter()
        .map(|reading| reading["kind"].as_str().expect("kind is a string"))
        .collect();
    assert_eq!(keys, RESOURCE_KEYS);

    for key in [
        "memory_used_pct",
        "swap_used_pct",
        "cpu_used_pct",
        "disk_free_pct",
    ] {
        let entry = reading(&value, key);
        match entry["value"].as_f64() {
            Some(percent) => assert!((0.0..=100.0).contains(&percent), "{key}: {entry}"),
            None => assert!(
                entry["reason"].is_string(),
                "{key} has no value or reason: {entry}"
            ),
        }
    }
    // No state directory was given, so the running-agent count is unavailable with its reason.
    let agents = reading(&value, "running_agents");
    assert_eq!(agents["value"], Value::Null);
    assert!(
        agents["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("no state directory")),
        "{agents}"
    );
    assert_eq!(agents["level"], "disabled");
}

#[test]
fn show_json_measures_the_disk_for_a_bare_relative_config_dir() {
    let root = scratch("show-relative-config");
    fs::create_dir(root.join("cfg")).expect("config directory");
    // The disk default is the parent of `cfg`, the working directory itself. It must be measured,
    // not handed to the filesystem as an empty path.
    let output = Command::new(env!("CARGO_BIN_EXE_mcao"))
        .args(["resources", "show", "--json", "--config-dir", "cfg"])
        .current_dir(&root)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .output()
        .expect("mcao runs");
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_text(&output)
    );
    let value = stdout_json(&output);
    let disk = reading(&value, "disk_free_pct");
    assert!(
        disk["value"]
            .as_f64()
            .is_some_and(|percent| (0.0..=100.0).contains(&percent)),
        "a bare relative config dir must measure the disk: {disk}"
    );
}

#[test]
fn show_text_prints_one_line_per_resource() {
    let config = scratch("show-text");
    let output = mcao(&["resources", "show", "--config-dir", path_arg(&config)]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_text(&output)
    );
    let text = stdout_text(&output);
    assert!(text.starts_with("resources "), "{text}");
    for key in RESOURCE_KEYS {
        assert!(
            text.lines()
                .any(|line| line.starts_with(&format!("{key}: "))),
            "no line for {key} in {text}"
        );
    }
}

#[test]
fn show_reports_a_malformed_thresholds_file_as_defaults_with_its_line() {
    let config = config_dir(
        "show-malformed",
        "memory_used_pct 90\nrunning_agents zero\n",
    );
    let output = mcao(&[
        "resources",
        "show",
        "--json",
        "--config-dir",
        path_arg(&config),
    ]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_text(&output)
    );
    let value = stdout_json(&output);
    assert_eq!(value["config"]["source"], "defaults");
    let warning = value["config"]["warning"]
        .as_str()
        .expect("a warning is reported");
    assert!(warning.contains("resource-thresholds:2:"), "{warning}");
    assert!(warning.contains("built-in defaults apply"), "{warning}");
}

#[test]
fn show_counts_running_agents_across_state_dirs_once_each() {
    let root = scratch("show-agents");
    let home_a = state_dir(
        &root,
        "home-a",
        &[
            ("ship-1", "kind=ship\nproject=p\n"),
            ("ship-2", "kind=ship\nproject=p\n"),
            ("sm", "kind=secondmate\nproject=p\n"),
        ],
    );
    let home_b = state_dir(
        &root,
        "home-b",
        &[
            (
                "ship-3",
                "kind=ship\nproject=p\npr=https://example.test/pull/1\n",
            ),
            ("ship-4", "kind=ship\nproject=p\n"),
        ],
    );
    let config = config_dir("show-agents-config", "running_agents 5\n");
    let output = mcao(&[
        "resources",
        "show",
        "--json",
        "--config-dir",
        path_arg(&config),
        "--state-dir",
        path_arg(&home_a),
        "--state-dir",
        path_arg(&home_b),
        "--state-dir",
        path_arg(&home_a),
    ]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_text(&output)
    );
    let agents = reading(&stdout_json(&output), "running_agents").clone();
    // Two ships in home A and one in home B count. The secondmate, the record with a PR, and the
    // repeated home A do not add to the count.
    assert_eq!(agents["value"].as_f64(), Some(3.0), "{agents}");
    assert_eq!(agents["limit"].as_f64(), Some(5.0), "{agents}");
    assert_eq!(agents["level"], "ok");
}

#[test]
fn show_accepts_an_explicit_disk_path() {
    let config = scratch("show-disk");
    let output = mcao(&[
        "resources",
        "show",
        "--json",
        "--config-dir",
        path_arg(&config),
        "--disk-path",
        path_arg(&config),
    ]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_text(&output)
    );
}

#[test]
fn admit_admits_with_exit_zero_and_prints_the_admission_json() {
    let root = scratch("admit-admit");
    let home = state_dir(
        &root,
        "home",
        &[
            ("ship-1", "kind=ship\nproject=p\n"),
            ("ship-2", "kind=ship\nproject=p\n"),
            ("ship-3", "kind=ship\nproject=p\n"),
        ],
    );
    let config = config_dir(
        "admit-admit-config",
        &format!("{HOST_LIMITS_OFF}running_agents 4\n"),
    );
    for (kind, name) in [
        ("ship", "ship"),
        ("scout", "scout"),
        ("secondmate", "secondmate"),
        ("relaunch", "relaunch"),
    ] {
        let output = mcao(&[
            "resources",
            "admit",
            "--kind",
            kind,
            "--task",
            "new-1",
            "--config-dir",
            path_arg(&config),
            "--state-dir",
            path_arg(&home),
        ]);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{kind}: {}",
            stderr_text(&output)
        );
        assert_eq!(stderr_text(&output), "", "{kind}");
        let value = stdout_json(&output);
        assert_eq!(value["schema"], "fm-resource-admission.v1");
        assert_eq!(value["disposition"], "admit");
        assert_eq!(value["kind"], name);
        assert_eq!(value["task"], "new-1");
        assert_eq!(value["message"], Value::Null);
        assert_eq!(value["snapshot"]["schema"], "fm-resource-snapshot.v1");
        assert_eq!(value["snapshot"]["state"], "ok");
    }
}

#[test]
fn admit_defers_at_the_running_agent_limit_with_exit_75_and_the_reason_on_stderr() {
    let root = scratch("admit-defer");
    let home = state_dir(
        &root,
        "home",
        &[
            ("ship-1", "kind=ship\nproject=p\n"),
            ("ship-2", "kind=ship\nproject=p\n"),
            ("ship-3", "kind=ship\nproject=p\n"),
        ],
    );
    let config = config_dir(
        "admit-defer-config",
        &format!("{HOST_LIMITS_OFF}running_agents 3\n"),
    );
    let output = mcao(&[
        "resources",
        "admit",
        "--kind",
        "ship",
        "--task",
        "new-1",
        "--config-dir",
        path_arg(&config),
        "--state-dir",
        path_arg(&home),
    ]);
    assert_eq!(
        output.status.code(),
        Some(75),
        "stderr: {}",
        stderr_text(&output)
    );
    let value = stdout_json(&output);
    assert_eq!(value["disposition"], "defer");
    assert_eq!(value["snapshot"]["state"], "breached");
    let message = value["message"].as_str().expect("a deferral has a message");
    assert!(message.starts_with("deferred: "), "{message}");
    assert!(message.contains("running_agents 3 >= 3"), "{message}");
    assert!(message.contains("task new-1 was not launched"), "{message}");
    assert_eq!(stderr_text(&output).trim_end(), message);
}

#[test]
fn admit_does_not_count_the_task_its_own_record() {
    let root = scratch("admit-own");
    let home = state_dir(
        &root,
        "home",
        &[
            ("ship-1", "kind=ship\nproject=p\n"),
            ("ship-2", "kind=ship\nproject=p\n"),
            ("ship-3", "kind=ship\nproject=p\n"),
        ],
    );
    let config = config_dir(
        "admit-own-config",
        &format!("{HOST_LIMITS_OFF}running_agents 3\n"),
    );
    // ship-1 is already recorded, so the relaunch counts ship-2 and ship-3 only: 2 < 3, admitted.
    let output = mcao(&[
        "resources",
        "admit",
        "--kind",
        "relaunch",
        "--task",
        "ship-1",
        "--config-dir",
        path_arg(&config),
        "--state-dir",
        path_arg(&home),
    ]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_text(&output)
    );
    assert_eq!(stdout_json(&output)["disposition"], "admit");
}

#[test]
fn admit_reads_an_unavailable_running_agent_count_as_unknown_and_admits() {
    let config = config_dir(
        "admit-unknown",
        &format!("{HOST_LIMITS_OFF}running_agents 1\n"),
    );
    let output = mcao(&[
        "resources",
        "admit",
        "--kind",
        "ship",
        "--task",
        "new-1",
        "--config-dir",
        path_arg(&config),
    ]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_text(&output)
    );
    let value = stdout_json(&output);
    assert_eq!(value["disposition"], "admit");
    assert_eq!(value["snapshot"]["state"], "unknown");
    let agents = reading(&value["snapshot"], "running_agents");
    assert_eq!(agents["value"], Value::Null);
    assert!(
        agents["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("no state directory")),
        "{agents}"
    );
}

#[test]
fn admit_warns_about_a_malformed_thresholds_file_and_still_decides() {
    let config = config_dir("admit-malformed", "running_agents 0\n");
    let output = mcao(&[
        "resources",
        "admit",
        "--kind",
        "ship",
        "--task",
        "new-1",
        "--config-dir",
        path_arg(&config),
    ]);
    // The built-in defaults apply, so the host's own readings decide: admit (0) or defer (75).
    assert!(
        matches!(output.status.code(), Some(0) | Some(75)),
        "stderr: {}",
        stderr_text(&output)
    );
    let stderr = stderr_text(&output);
    assert!(stderr.starts_with("warning: "), "{stderr}");
    assert!(stderr.contains("resource-thresholds:1:"), "{stderr}");
    let value = stdout_json(&output);
    assert_eq!(value["snapshot"]["config"]["source"], "defaults");
    assert!(value["snapshot"]["config"]["warning"].is_string());
}

#[test]
fn invalid_resource_arguments_are_usage_errors_with_exit_two() {
    let config = scratch("usage");
    let config_arg = path_arg(&config);
    let cases: [&[&str]; 6] = [
        &["resources", "show"],
        &[
            "resources",
            "admit",
            "--kind",
            "bogus",
            "--task",
            "t",
            "--config-dir",
            config_arg,
        ],
        &[
            "resources",
            "admit",
            "--kind",
            "ship",
            "--task",
            ".hidden",
            "--config-dir",
            config_arg,
        ],
        &[
            "resources",
            "admit",
            "--kind",
            "ship",
            "--config-dir",
            config_arg,
        ],
        &[
            "resources",
            "admit",
            "--task",
            "t",
            "--config-dir",
            config_arg,
        ],
        &["resources", "admit", "--kind", "ship", "--task", "t"],
    ];
    for args in cases {
        let output = mcao(args);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?}: {}",
            stderr_text(&output)
        );
        assert!(
            stderr_text(&output).starts_with("Usage: "),
            "{args:?}: {}",
            stderr_text(&output)
        );
    }
}
