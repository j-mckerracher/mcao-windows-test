mod support;

#[cfg(unix)]
use std::fs;
use std::path::Path;
use std::process::Command;

use mcao_core::preflight::check_symlink_support;
use support::TempDir;

/// Runs `git` in `dir` and fails the test when it does not succeed.
fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .expect("git runs");
    assert!(status.success(), "git {args:?} failed");
}

/// A new, empty git repository under a temporary directory.
fn repo(label: &str) -> TempDir {
    let dir = TempDir::new(label);
    git(dir.path(), &["init", "-q"]);
    dir
}

#[test]
fn a_directory_outside_a_repository_is_refused() {
    let dir = TempDir::new("symlink-no-repo");

    let error = check_symlink_support(dir.path()).expect_err("not a worktree");

    assert_eq!(error.exit_code(), 3);
    assert!(
        error.to_string().contains("cannot list tracked files"),
        "{error}"
    );
}

#[test]
fn core_symlinks_reports_the_repositorys_setting() {
    let dir = repo("symlink-setting");

    git(dir.path(), &["config", "core.symlinks", "true"]);
    let enabled = check_symlink_support(dir.path()).expect("git answers");
    assert_eq!(enabled.git_core_symlinks, Some(true));

    git(dir.path(), &["config", "core.symlinks", "false"]);
    let disabled = check_symlink_support(dir.path()).expect("git answers");
    assert_eq!(disabled.git_core_symlinks, Some(false));
}

#[cfg(unix)]
#[test]
fn a_link_checked_out_as_text_is_reported_as_a_placeholder() {
    let dir = repo("symlink-placeholder");
    let link = dir.path().join("link");
    std::os::unix::fs::symlink("target.txt", &link).expect("link created");
    git(dir.path(), &["add", "link"]);
    git(dir.path(), &["config", "core.symlinks", "false"]);
    // With core.symlinks false, git checks a tracked link out as a file holding the target text.
    fs::remove_file(&link).expect("link removed");
    fs::write(&link, "target.txt").expect("placeholder written");

    let support = check_symlink_support(dir.path()).expect("git answers");

    assert_eq!(support.git_core_symlinks, Some(false));
    assert_eq!(support.placeholders, vec![link]);
}

#[cfg(unix)]
#[test]
fn a_link_still_in_place_is_not_a_placeholder() {
    let dir = repo("symlink-in-place");
    let link = dir.path().join("link");
    std::os::unix::fs::symlink("target.txt", &link).expect("link created");
    git(dir.path(), &["add", "link"]);

    let support = check_symlink_support(dir.path()).expect("git answers");

    assert!(
        support.placeholders.is_empty(),
        "{:?}",
        support.placeholders
    );
}

#[cfg(unix)]
#[test]
fn a_link_deleted_from_the_working_tree_is_not_a_placeholder() {
    let dir = repo("symlink-deleted");
    let link = dir.path().join("link");
    std::os::unix::fs::symlink("target.txt", &link).expect("link created");
    git(dir.path(), &["add", "link"]);
    fs::remove_file(&link).expect("link removed");

    let support = check_symlink_support(dir.path()).expect("git answers");

    assert!(
        support.placeholders.is_empty(),
        "{:?}",
        support.placeholders
    );
}

#[cfg(unix)]
#[test]
fn the_host_probe_creates_a_link_and_leaves_nothing_behind() {
    let dir = repo("symlink-probe");

    let support = check_symlink_support(dir.path()).expect("git answers");

    assert!(support.system_creates_symlinks);
    let leftovers: Vec<_> = fs::read_dir(dir.path())
        .expect("directory readable")
        .map(|entry| entry.expect("entry readable").file_name())
        .filter(|name| name != ".git")
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}
