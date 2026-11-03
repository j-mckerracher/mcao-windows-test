//! The OneDrive guard on the two hooks that write state (DPES-892 AC4): `hook session-start` and
//! `hook host-mirror`. A positively detected OneDrive-synchronized operational home makes each hook
//! write nothing, print the relocation on standard error, and return its own non-failure exit code.
//! `hook subagent-pretool` reads state only and is not guarded. The homes and roots are spelled in
//! the host's own form, as `tests/onedrive_guard.rs` does, so the unix cases run on unix hosts.

use std::fs;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use mcao_core::preflight::ONEDRIVE_RELOCATION;
use serde_json::json;

/// Variables cleared before each hook runs, so only the test's own home and roots are in effect.
const CLEARED: [&str; 10] = [
    "FM_HOME",
    "FM_STATE_OVERRIDE",
    "FM_ROOT_OVERRIDE",
    "USERPROFILE",
    "LOCALAPPDATA",
    "OneDrive",
    "OneDriveCommercial",
    "OneDriveConsumer",
    "NO_MISTAKES_GATE",
    "CLAUDE_PROJECT_DIR",
];
/// Written by the session-start child stub when the hook runs it, so a run is observable.
#[cfg(unix)]
const CHILD_MARKER: &str = "child-ran";
#[cfg(unix)]
const MIRROR: &str = ".host-mirror.jsonl";
const KEY: &str = "main-session-key";

/// A directory under cargo's test temporary directory, emptied before use.
fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("hook-onedrive-guard-{name}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

/// A home under a OneDrive root that `OneDrive` names, with an empty `state` directory. Returns the
/// root and the home.
fn onedrive_home(dir: &Path) -> (PathBuf, PathBuf) {
    let root = dir.join("OneDrive");
    let home = root.join("fm");
    fs::create_dir_all(home.join("state")).expect("home state");
    (root, home)
}

/// A firstmate root that passes the session-start primary-root check through a secondmate marker.
/// Its `bin/fm-session-start.sh` child writes `CHILD_MARKER` into the root, so a run is observable.
#[cfg(unix)]
fn firstmate_root(dir: &Path) -> PathBuf {
    let root = dir.join("firstmate");
    fs::create_dir_all(root.join("bin")).expect("bin directory");
    fs::write(root.join("AGENTS.md"), "# scratch\n").expect("AGENTS.md");
    fs::write(root.join(".fm-secondmate-home"), "dpes892-guard\n").expect("secondmate marker");
    let child = root.join("bin").join("fm-session-start.sh");
    let script = format!(
        "#!/bin/sh\necho ran > '{}'\n",
        root.join(CHILD_MARKER).display()
    );
    fs::write(&child, script).expect("child stub");
    fs::set_permissions(&child, fs::Permissions::from_mode(0o755)).expect("child executable");
    root
}

/// The names of the entries directly under `dir`.
fn entries(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .expect("list directory")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

/// Runs `mcao hook <hook...>` with `env` applied over the cleared variables, feeding `stdin`.
fn mcao_hook(hook: &[&str], env: &[(&str, &Path)], stdin: &[u8]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mcao"));
    command
        .arg("hook")
        .args(hook)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for name in CLEARED {
        command.env_remove(name);
    }
    for (name, value) in env {
        command.env(name, value);
    }
    let mut child = command.spawn().expect("mcao spawns");
    child
        .stdin
        .take()
        .expect("stdin pipe")
        .write_all(stdin)
        .expect("stdin written");
    child.wait_with_output().expect("mcao exits")
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn prompt(prompt_id: &str, text: &str) -> String {
    json!({"hook_event_name": "UserPromptSubmit", "prompt_id": prompt_id, "prompt": text})
        .to_string()
}

#[cfg(unix)]
#[test]
fn a_onedrive_home_stands_down_session_start_without_writing() {
    let dir = scratch("session-start-refused");
    let (onedrive, home) = onedrive_home(&dir);
    let root = firstmate_root(&dir);

    let output = mcao_hook(
        &["session-start", "--source", "startup"],
        &[
            ("FM_ROOT_OVERRIDE", root.as_path()),
            ("FM_HOME", home.as_path()),
            ("OneDrive", onedrive.as_path()),
        ],
        b"",
    );

    assert_eq!(output.status.code(), Some(0), "{}", stderr_of(&output));
    let stderr = stderr_of(&output);
    assert!(stderr.contains("named by OneDrive"), "{stderr}");
    assert!(stderr.contains(ONEDRIVE_RELOCATION), "{stderr}");
    assert!(output.stdout.is_empty(), "{:?}", output.stdout);
    assert!(!root.join(CHILD_MARKER).exists(), "the child must not run");
    assert!(
        entries(&home.join("state")).is_empty(),
        "nothing may be written"
    );
}

#[cfg(unix)]
#[test]
fn a_refused_session_start_keeps_the_pi_prerequisite_stand_down_code() {
    let dir = scratch("session-start-refused-pi");
    let (onedrive, home) = onedrive_home(&dir);
    let root = firstmate_root(&dir);

    let output = mcao_hook(
        &["session-start", "--source", "startup", "--pi-prerequisite"],
        &[
            ("FM_ROOT_OVERRIDE", root.as_path()),
            ("FM_HOME", home.as_path()),
            ("OneDrive", onedrive.as_path()),
        ],
        b"",
    );

    assert_eq!(output.status.code(), Some(3), "{}", stderr_of(&output));
    assert!(stderr_of(&output).contains(ONEDRIVE_RELOCATION));
    assert!(!root.join(CHILD_MARKER).exists(), "the child must not run");
    assert!(
        entries(&home.join("state")).is_empty(),
        "nothing may be written"
    );
}

#[cfg(unix)]
#[test]
fn a_name_only_sync_folder_does_not_stop_session_start() {
    let dir = scratch("session-start-name-only");
    let profile = dir.join("profile");
    let home = profile.join("OneDrive - Contoso").join("fm");
    fs::create_dir_all(home.join("state")).expect("home state");
    let root = firstmate_root(&dir);

    let output = mcao_hook(
        &["session-start", "--source", "startup"],
        &[
            ("FM_ROOT_OVERRIDE", root.as_path()),
            ("FM_HOME", home.as_path()),
            ("USERPROFILE", profile.as_path()),
        ],
        b"",
    );

    assert_eq!(output.status.code(), Some(0), "{}", stderr_of(&output));
    assert!(
        !stderr_of(&output).contains("Refused"),
        "{}",
        stderr_of(&output)
    );
    assert!(
        root.join(CHILD_MARKER).exists(),
        "a name alone only warns, so the child runs"
    );
}

#[test]
fn a_onedrive_home_stands_down_host_mirror_without_recording() {
    let dir = scratch("host-mirror-refused");
    let (onedrive, home) = onedrive_home(&dir);
    let root = dir.join("firstmate");
    fs::create_dir_all(&root).expect("root");

    let output = mcao_hook(
        &[
            "host-mirror",
            "--harness",
            "claude",
            "--main-key",
            KEY,
            "--root",
            root.to_str().expect("utf-8 root"),
        ],
        &[
            ("FM_HOME", home.as_path()),
            ("OneDrive", onedrive.as_path()),
        ],
        prompt("p1", "refused dialog").as_bytes(),
    );

    assert_eq!(output.status.code(), Some(0), "{}", stderr_of(&output));
    assert!(output.stdout.is_empty(), "{:?}", output.stdout);
    let stderr = stderr_of(&output);
    assert!(stderr.contains("named by OneDrive"), "{stderr}");
    assert!(stderr.contains(ONEDRIVE_RELOCATION), "{stderr}");
    assert!(
        entries(&home.join("state")).is_empty(),
        "nothing may be written"
    );
}

#[cfg(unix)]
#[test]
fn a_name_only_sync_folder_still_records_host_mirror() {
    let dir = scratch("host-mirror-name-only");
    let profile = dir.join("profile");
    let home = profile.join("OneDrive - Contoso").join("fm");
    fs::create_dir_all(home.join("state")).expect("home state");
    let root = dir.join("firstmate");
    fs::create_dir_all(&root).expect("root");

    let output = mcao_hook(
        &[
            "host-mirror",
            "--harness",
            "claude",
            "--main-key",
            KEY,
            "--root",
            root.to_str().expect("utf-8 root"),
        ],
        &[
            ("FM_HOME", home.as_path()),
            ("USERPROFILE", profile.as_path()),
        ],
        prompt("p2", "named folder dialog").as_bytes(),
    );

    assert_eq!(output.status.code(), Some(0), "{}", stderr_of(&output));
    assert!(
        !stderr_of(&output).contains("Refused"),
        "{}",
        stderr_of(&output)
    );
    let mirror = fs::read_to_string(home.join("state").join(MIRROR)).expect("mirror written");
    assert!(mirror.contains("named folder dialog"), "{mirror}");
}
