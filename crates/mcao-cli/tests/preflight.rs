//! `mcao preflight` end to end: each check passes or fails with its name, the first failure exits 3,
//! and no tool found on `PATH` is ever run.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const ONEDRIVE_VARS: [&str; 3] = ["OneDrive", "OneDriveCommercial", "OneDriveConsumer"];

/// A directory under the test target directory, emptied before use.
fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("preflight-{name}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

/// An absolute path for the host. On Windows it gains a drive, as the core rules require.
fn absolute(unix_tail: &str) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(format!("C:\\{}", unix_tail.replace('/', "\\")))
    } else {
        PathBuf::from(format!("/{unix_tail}"))
    }
}

/// Places a tool named `name` in `dir` that would create a marker file and exit 1 if it were ever
/// run, as a failing `gh auth status` would. Returns the marker path, which must not exist after
/// preflight.
fn fake_tool(dir: &Path, name: &str) -> PathBuf {
    let marker = dir.join(format!("{name}-was-run"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let tool = dir.join(name);
        fs::write(
            &tool,
            format!("#!/bin/sh\n: > '{}'\nexit 1\n", marker.display()),
        )
        .expect("fake tool");
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).expect("fake tool mode");
    }
    #[cfg(windows)]
    {
        fs::write(dir.join(format!("{name}.EXE")), b"not a program").expect("fake tool");
    }
    marker
}

/// Runs `mcao preflight` with `PATH` set to `path` and the home and OneDrive variables cleared, then
/// applies `extra`.
fn preflight(path: &OsStr, extra: &[(&str, &OsStr)]) -> Output {
    run_preflight(None, path, extra)
}

/// Like [`preflight`], run from the working directory `cwd` when one is given.
fn run_preflight(cwd: Option<&Path>, path: &OsStr, extra: &[(&str, &OsStr)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mcao"));
    command
        .arg("preflight")
        .env("PATH", path)
        .env_remove("FM_HOME")
        .stdin(Stdio::null());
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    for name in ONEDRIVE_VARS {
        command.env_remove(name);
    }
    command.env_remove("USERPROFILE");
    for (name, value) in extra {
        command.env(name, value);
    }
    command.output().expect("mcao runs")
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn preflight_passes_with_git_and_gh_on_path_and_never_runs_them() {
    let dir = scratch("git-gh-present");
    let git_marker = fake_tool(&dir, "git");
    let gh_marker = fake_tool(&dir, "gh");

    let output = preflight(dir.as_os_str(), &[]);

    assert_eq!(output.status.code(), Some(0), "{}", stderr_of(&output));
    let stdout = stdout_of(&output);
    assert!(stdout.contains("Git: ok ("), "{stdout}");
    assert!(stdout.contains("Gh: ok ("), "{stdout}");
    assert!(stdout.contains("Preflight: all checks passed"), "{stdout}");
    assert!(output.stderr.is_empty(), "{}", stderr_of(&output));
    assert!(!git_marker.exists(), "preflight ran the git it found");
    assert!(!gh_marker.exists(), "preflight ran the gh it found");
}

#[test]
fn preflight_exits_three_and_names_git_when_it_is_missing_from_path() {
    let dir = scratch("git-missing");

    let output = preflight(dir.as_os_str(), &[]);

    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty(), "{}", stdout_of(&output));
    let stderr = stderr_of(&output);
    assert!(stderr.contains("Git: MISSING"), "{stderr}");
    assert!(stderr.contains("https://git-scm.com"), "{stderr}");
    assert!(!stderr.contains("Gh:"), "Gh is checked after Git: {stderr}");
}

#[test]
fn preflight_exits_three_and_names_gh_when_it_is_missing_from_path() {
    let dir = scratch("gh-missing");
    fake_tool(&dir, "git");

    let output = preflight(dir.as_os_str(), &[]);

    assert_eq!(output.status.code(), Some(3));
    let stdout = stdout_of(&output);
    assert!(stdout.contains("Git: ok ("), "{stdout}");
    assert!(!stdout.contains("all checks passed"), "{stdout}");
    let stderr = stderr_of(&output);
    assert!(stderr.contains("Gh: MISSING"), "{stderr}");
    assert!(stderr.contains("https://cli.github.com"), "{stderr}");
}

#[test]
fn preflight_checks_gh_offline_and_never_runs_it_even_when_it_would_fail() {
    // Each stub fails when run, and the proxies point at a closed port. A check that ran gh or
    // reached GitHub would fail, so a pass shows the lookup is PATH-only.
    let dir = scratch("gh-offline");
    fake_tool(&dir, "git");
    let gh_marker = fake_tool(&dir, "gh");

    let output = preflight(
        dir.as_os_str(),
        &[
            ("HTTPS_PROXY", OsStr::new("http://127.0.0.1:9")),
            ("HTTP_PROXY", OsStr::new("http://127.0.0.1:9")),
            ("GH_HOST", OsStr::new("127.0.0.1:9")),
        ],
    );

    assert_eq!(output.status.code(), Some(0), "{}", stderr_of(&output));
    assert!(
        stdout_of(&output).contains("Gh: ok ("),
        "{}",
        stdout_of(&output)
    );
    assert!(!gh_marker.exists(), "preflight ran the gh it found");
}

#[test]
fn preflight_prints_an_fm_home_with_spaces_and_non_ascii_exactly() {
    let dir = scratch("unusual-home");
    fake_tool(&dir, "git");
    fake_tool(&dir, "gh");
    let home = absolute("opt/fm home/Test User/工作流程/$HOME ~/workspace");
    let home_text = home.to_str().expect("fixture path is UTF-8");

    let output = preflight(dir.as_os_str(), &[("FM_HOME", home.as_os_str())]);

    assert_eq!(output.status.code(), Some(0), "{}", stderr_of(&output));
    let stdout = stdout_of(&output);
    let budget_line = if cfg!(windows) {
        format!("FM_HOME path length: ok ({home_text})")
    } else {
        "FM_HOME path length: not checked".to_owned()
    };
    assert!(stdout.contains(&budget_line), "{stdout}");
    assert!(
        stdout.contains(&format!("FM_HOME OneDrive: ok ({home_text})")),
        "{stdout}"
    );
}

#[cfg(windows)]
#[test]
fn preflight_exits_three_naming_the_path_length_check_for_an_overlong_home() {
    let dir = scratch("overlong-home");
    fake_tool(&dir, "git");
    fake_tool(&dir, "gh");
    let home = absolute(&"a".repeat(300));

    let output = preflight(dir.as_os_str(), &[("FM_HOME", home.as_os_str())]);

    assert_eq!(output.status.code(), Some(3));
    let stdout = stdout_of(&output);
    assert!(stdout.contains("Git: ok ("), "{stdout}");
    assert!(stdout.contains("Gh: ok ("), "{stdout}");
    assert!(!stdout.contains("all checks passed"), "{stdout}");
    let stderr = stderr_of(&output);
    assert!(stderr.contains("FM_HOME path length"), "{stderr}");
}

#[cfg(unix)]
#[test]
fn preflight_passes_a_long_fm_home_on_unix_because_the_windows_budget_does_not_apply() {
    let dir = scratch("long-home-unix");
    fake_tool(&dir, "git");
    fake_tool(&dir, "gh");
    let home = absolute(&"a".repeat(300));

    let output = preflight(dir.as_os_str(), &[("FM_HOME", home.as_os_str())]);

    assert_eq!(output.status.code(), Some(0), "{}", stderr_of(&output));
    let stdout = stdout_of(&output);
    assert!(
        stdout.contains("FM_HOME path length: not checked"),
        "{stdout}"
    );
    assert!(stdout.contains("all checks passed"), "{stdout}");
}

#[test]
fn preflight_refuses_a_relative_fm_home() {
    let dir = scratch("relative-home");
    fake_tool(&dir, "git");
    fake_tool(&dir, "gh");

    let output = preflight(dir.as_os_str(), &[("FM_HOME", OsStr::new("relative/fm"))]);

    assert_eq!(output.status.code(), Some(3));
    let stderr = stderr_of(&output);
    assert!(stderr.contains("FM_HOME"), "{stderr}");
    assert!(stderr.contains("not an absolute path"), "{stderr}");
}

#[test]
fn preflight_never_resolves_git_from_the_working_directory() {
    // An empty PATH entry and `.` both name the working directory, so a git kept there must not count.
    let dir = scratch("git-in-cwd");
    let marker = fake_tool(&dir, "git");
    let path = std::env::join_paths(["", "."]).expect("PATH entries hold no separator");

    let output = run_preflight(Some(&dir), &path, &[]);

    assert_eq!(output.status.code(), Some(3), "{}", stderr_of(&output));
    assert!(
        stderr_of(&output).contains("Git: MISSING"),
        "{}",
        stderr_of(&output)
    );
    assert!(
        !marker.exists(),
        "preflight ran the git in the working directory"
    );
}

#[cfg(unix)]
#[test]
fn preflight_reports_git_present_but_not_executable_apart_from_missing() {
    use std::os::unix::fs::PermissionsExt;

    let dir = scratch("git-not-executable");
    let git = dir.join("git");
    fs::write(&git, "#!/bin/sh\n").expect("git file");
    fs::set_permissions(&git, fs::Permissions::from_mode(0o644)).expect("git mode");

    let output = preflight(dir.as_os_str(), &[]);

    assert_eq!(output.status.code(), Some(3));
    let stderr = stderr_of(&output);
    assert!(stderr.contains("Git: NOT EXECUTABLE"), "{stderr}");
    assert!(!stderr.contains("MISSING"), "{stderr}");
}

#[cfg(unix)]
#[test]
fn preflight_reports_gh_present_but_not_executable_apart_from_missing() {
    use std::os::unix::fs::PermissionsExt;

    let dir = scratch("gh-not-executable");
    fake_tool(&dir, "git");
    let gh = dir.join("gh");
    fs::write(&gh, "#!/bin/sh\n").expect("gh file");
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o644)).expect("gh mode");

    let output = preflight(dir.as_os_str(), &[]);

    assert_eq!(output.status.code(), Some(3));
    let stderr = stderr_of(&output);
    assert!(stderr.contains("Gh: NOT EXECUTABLE"), "{stderr}");
    assert!(!stderr.contains("Gh: MISSING"), "{stderr}");
}

#[cfg(windows)]
#[test]
fn preflight_does_not_accept_a_script_shim_as_git() {
    let dir = scratch("git-shim-only");
    fs::write(dir.join("git.cmd"), "@echo off\r\n").expect("shim");

    let output = preflight(dir.as_os_str(), &[]);

    assert_eq!(output.status.code(), Some(3));
    assert!(
        stderr_of(&output).contains("Git: MISSING"),
        "{}",
        stderr_of(&output)
    );
}

#[cfg(windows)]
#[test]
fn preflight_does_not_accept_a_script_shim_as_gh() {
    let dir = scratch("gh-shim-only");
    fake_tool(&dir, "git");
    fs::write(dir.join("gh.cmd"), "@echo off\r\n").expect("shim");

    let output = preflight(dir.as_os_str(), &[]);

    assert_eq!(output.status.code(), Some(3));
    assert!(
        stderr_of(&output).contains("Gh: MISSING"),
        "{}",
        stderr_of(&output)
    );
}

#[test]
fn preflight_warns_and_passes_for_a_home_beneath_the_profile_sync_folder() {
    let dir = scratch("onedrive-profile-home");
    fake_tool(&dir, "git");
    fake_tool(&dir, "gh");
    let profile = absolute("opt/profile");
    let home = profile.join("OneDrive").join("fm");

    let output = preflight(
        dir.as_os_str(),
        &[
            ("FM_HOME", home.as_os_str()),
            ("USERPROFILE", profile.as_os_str()),
        ],
    );

    assert_eq!(output.status.code(), Some(0), "{}", stderr_of(&output));
    let stdout = stdout_of(&output);
    assert!(stdout.contains("FM_HOME OneDrive: warning:"), "{stdout}");
    assert!(stdout.contains("beneath USERPROFILE"), "{stdout}");
    assert!(stdout.contains("all checks passed"), "{stdout}");
    assert!(output.stderr.is_empty(), "{}", stderr_of(&output));
}

#[test]
fn preflight_exits_three_for_a_profile_sync_folder_that_a_root_also_names() {
    let dir = scratch("onedrive-profile-root");
    fake_tool(&dir, "git");
    fake_tool(&dir, "gh");
    let profile = absolute("opt/profile");
    let root = profile.join("OneDrive");
    let home = root.join("fm");

    let output = preflight(
        dir.as_os_str(),
        &[
            ("FM_HOME", home.as_os_str()),
            ("USERPROFILE", profile.as_os_str()),
            ("OneDrive", root.as_os_str()),
        ],
    );

    assert_eq!(output.status.code(), Some(3), "{}", stderr_of(&output));
    let stderr = stderr_of(&output);
    assert!(stderr.contains("FM_HOME OneDrive"), "{stderr}");
    assert!(stderr.contains("named by OneDrive"), "{stderr}");
}

#[cfg(unix)]
#[test]
fn preflight_passes_a_windows_form_profile_for_a_unix_home() {
    let dir = scratch("windows-profile-unix-home");
    fake_tool(&dir, "git");
    fake_tool(&dir, "gh");
    let home = absolute("opt/fm");

    let output = preflight(
        dir.as_os_str(),
        &[
            ("FM_HOME", home.as_os_str()),
            ("USERPROFILE", OsStr::new(r"C:\Users\u")),
        ],
    );

    assert_eq!(output.status.code(), Some(0), "{}", stderr_of(&output));
    assert!(
        stdout_of(&output).contains("FM_HOME OneDrive: ok ("),
        "{}",
        stdout_of(&output)
    );
}

#[test]
fn preflight_exits_three_naming_onedrive_for_a_home_under_a_onedrive_root() {
    let dir = scratch("onedrive-home");
    fake_tool(&dir, "git");
    fake_tool(&dir, "gh");
    let root = absolute("opt/OneDrive");
    let home = root.join("fm");

    let output = preflight(
        dir.as_os_str(),
        &[
            ("FM_HOME", home.as_os_str()),
            ("OneDrive", root.as_os_str()),
        ],
    );

    assert_eq!(output.status.code(), Some(3));
    let stderr = stderr_of(&output);
    assert!(stderr.contains("FM_HOME OneDrive"), "{stderr}");
    assert!(stderr.contains("OneDrive"), "{stderr}");
}
