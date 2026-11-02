//! The OneDrive guard on stateful `mcao` commands, end to end (DPES-892 AC4 and AC5).
//!
//! A positively detected OneDrive-synchronized home refuses with exit 4 before any state is read
//! or written. Help, version, and the stateless commands are unaffected, a name-only sync folder
//! warns without refusing, and an explicitly selected safe home is honored. The home and roots are
//! spelled in the host's own form, as `tests/preflight.rs` does, so these run on unix hosts too.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use mcao_core::preflight::ONEDRIVE_RELOCATION;

const HOME_VARS: [&str; 4] = [
    "FM_HOME",
    "FM_STATE_OVERRIDE",
    "USERPROFILE",
    "LOCALAPPDATA",
];
const ONEDRIVE_VARS: [&str; 3] = ["OneDrive", "OneDriveCommercial", "OneDriveConsumer"];
const REJECTIONS: &str = "audit-rejections.jsonl";

/// A directory under the test target directory, emptied before use.
fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("onedrive-guard-{name}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

/// A rejected envelope, so `audit validate` would record a rejection if it were allowed to run.
fn rejected_document(dir: &Path) -> PathBuf {
    let document = dir.join("document.json");
    fs::write(&document, "{}").expect("document");
    document
}

/// Runs `mcao` with the home, OneDrive, and profile variables cleared, then applies `extra`.
fn mcao(args: &[&OsStr], extra: &[(&str, &OsStr)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mcao"));
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for name in HOME_VARS.iter().chain(ONEDRIVE_VARS.iter()) {
        command.env_remove(name);
    }
    for (name, value) in extra {
        command.env(name, value);
    }
    command.output().expect("mcao runs")
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn code(output: &Output) -> Option<i32> {
    output.status.code()
}

/// Lays out a home with a `state` directory under a OneDrive root that `OneDrive` names.
fn onedrive_home(name: &str) -> (PathBuf, PathBuf, PathBuf) {
    let dir = scratch(name);
    let root = dir.join("OneDrive");
    let home = root.join("fm");
    fs::create_dir_all(home.join("state")).expect("home state");
    (dir, root, home)
}

#[test]
fn a_onedrive_home_refuses_audit_validate_with_exit_four_and_records_nothing() {
    let (dir, root, home) = onedrive_home("refuse-validate");
    let document = rejected_document(&dir);

    let output = mcao(
        &[
            OsStr::new("audit"),
            OsStr::new("validate"),
            document.as_os_str(),
        ],
        &[
            ("FM_HOME", home.as_os_str()),
            ("OneDrive", root.as_os_str()),
        ],
    );

    assert_eq!(code(&output), Some(4), "{}", stderr_of(&output));
    let stderr = stderr_of(&output);
    assert!(stderr.contains("Refused"), "{stderr}");
    assert!(stderr.contains("named by OneDrive"), "{stderr}");
    assert!(stderr.contains(ONEDRIVE_RELOCATION), "{stderr}");
    assert!(output.stdout.is_empty(), "{:?}", output.stdout);
    assert!(!home.join("state").join(REJECTIONS).exists());
}

#[test]
fn every_stateful_command_refuses_a_onedrive_home() {
    let (dir, root, home) = onedrive_home("refuse-every");
    let document = rejected_document(&dir);
    let store = dir.join("store");
    let config_dir = dir.join("config");
    let state_dir = home.join("state");
    let env = [
        ("FM_HOME", home.as_os_str()),
        ("OneDrive", root.as_os_str()),
    ];
    let cases: Vec<Vec<&OsStr>> = vec![
        vec![
            OsStr::new("audit"),
            OsStr::new("validate"),
            document.as_os_str(),
        ],
        vec![
            OsStr::new("audit"),
            OsStr::new("append"),
            OsStr::new("--store"),
            store.as_os_str(),
            document.as_os_str(),
        ],
        vec![
            OsStr::new("audit"),
            OsStr::new("verify"),
            OsStr::new("--store"),
            store.as_os_str(),
        ],
        vec![
            OsStr::new("audit"),
            OsStr::new("project-ledger"),
            OsStr::new("--store"),
            store.as_os_str(),
        ],
        vec![
            OsStr::new("resources"),
            OsStr::new("show"),
            OsStr::new("--config-dir"),
            config_dir.as_os_str(),
            OsStr::new("--state-dir"),
            state_dir.as_os_str(),
        ],
        vec![
            OsStr::new("resources"),
            OsStr::new("admit"),
            OsStr::new("--config-dir"),
            config_dir.as_os_str(),
            OsStr::new("--kind"),
            OsStr::new("ship"),
            OsStr::new("--task"),
            OsStr::new("task-1"),
        ],
    ];

    for args in &cases {
        let output = mcao(args, &env);
        assert_eq!(code(&output), Some(4), "{args:?}: {}", stderr_of(&output));
        assert!(output.stdout.is_empty(), "{args:?}: {:?}", output.stdout);
    }
    assert!(
        !store.exists(),
        "a refused append must not create the store"
    );
    assert!(!state_dir.join(REJECTIONS).exists());
}

#[test]
fn help_and_version_stay_available_under_a_onedrive_home() {
    let (_dir, root, home) = onedrive_home("help-version");
    let env = [
        ("FM_HOME", home.as_os_str()),
        ("OneDrive", root.as_os_str()),
    ];

    for args in [["--help"], ["--version"]] {
        let args: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
        let output = mcao(&args, &env);
        assert_eq!(code(&output), Some(0), "{args:?}: {}", stderr_of(&output));
    }
}

#[test]
fn a_name_only_sync_folder_warns_and_the_stateful_command_still_runs() {
    let dir = scratch("name-only");
    let profile = dir.join("profile");
    let home = profile.join("OneDrive - Contoso").join("fm");
    fs::create_dir_all(home.join("state")).expect("home state");
    let document = rejected_document(&dir);

    let output = mcao(
        &[
            OsStr::new("audit"),
            OsStr::new("validate"),
            document.as_os_str(),
        ],
        &[
            ("FM_HOME", home.as_os_str()),
            ("USERPROFILE", profile.as_os_str()),
        ],
    );

    // The rejected document exits 1 after its rejection is recorded. A refusal would exit 4.
    assert_eq!(code(&output), Some(1), "{}", stderr_of(&output));
    assert!(home.join("state").join(REJECTIONS).is_file());
}

#[test]
fn an_explicitly_selected_safe_home_is_honored_under_a_onedrive_root() {
    let (dir, root, _home) = onedrive_home("safe-home");
    let safe = dir.join("safe").join("fm");
    fs::create_dir_all(safe.join("state")).expect("safe state");
    let document = rejected_document(&dir);

    let output = mcao(
        &[
            OsStr::new("audit"),
            OsStr::new("validate"),
            document.as_os_str(),
        ],
        &[
            ("FM_HOME", safe.as_os_str()),
            ("OneDrive", root.as_os_str()),
        ],
    );

    assert_eq!(code(&output), Some(1), "{}", stderr_of(&output));
    assert!(safe.join("state").join(REJECTIONS).is_file());
}

#[test]
fn an_unset_home_keeps_the_unix_rejection_message() {
    let dir = scratch("unset-home");
    let document = rejected_document(&dir);

    let output = mcao(
        &[
            OsStr::new("audit"),
            OsStr::new("validate"),
            document.as_os_str(),
        ],
        &[],
    );

    if cfg!(unix) {
        // Without FM_HOME there is no Windows default home, so the rejection cannot be recorded.
        assert_eq!(code(&output), Some(2), "{}", stderr_of(&output));
        assert!(
            stderr_of(&output).contains("set FM_HOME or FM_STATE_OVERRIDE"),
            "{}",
            stderr_of(&output)
        );
    }
}
