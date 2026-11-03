//! Behavior tests for `mcao util durable-sync`, the flush `bin/fm-local-handoff.sh` runs on every
//! receipt. The flush itself has no observable output, so these cases pin the exit status, the
//! refusal text, and the path handling that decides which directory is synced.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const DURABLE_USAGE: &str = "usage: mcao util durable-sync <path>\n";

fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("durable-{name}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn durable_sync(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mcao"))
        .current_dir(cwd)
        .args(["util", "durable-sync"])
        .args(args)
        .output()
        .expect("run mcao")
}

// Off unix `sync_file` refuses by design (`src/util/durable.rs`), so only the exit-0 cases are
// unix-only.
#[cfg(unix)]
#[test]
fn durable_sync_of_a_file_exits_zero_with_no_output() {
    let dir = scratch("file");
    fs::write(dir.join("receipt.json"), "{}\n").expect("write receipt");
    let output = durable_sync(&dir, &["receipt.json"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[cfg(unix)]
#[test]
fn durable_sync_resolves_dot_dot_before_choosing_the_parent() {
    let dir = scratch("dotdot");
    fs::create_dir_all(dir.join("a/b")).expect("create nested dirs");
    fs::write(dir.join("a/b/receipt.json"), "{}\n").expect("write receipt");
    let output = durable_sync(&dir, &["a/b/../b/receipt.json"]);
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn durable_sync_of_a_missing_path_exits_one_and_names_it() {
    let dir = scratch("missing");
    let output = durable_sync(&dir, &["absent.json"]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.starts_with("mcao util: durable-sync absent.json: "),
        "stderr was {stderr:?}"
    );
}

#[test]
fn durable_sync_without_exactly_one_path_prints_its_usage() {
    let dir = scratch("usage");
    for args in [vec![], vec!["a", "b"]] {
        let output = durable_sync(&dir, &args);
        assert_eq!(output.status.code(), Some(2), "args {args:?}");
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            DURABLE_USAGE,
            "args {args:?}"
        );
    }
}
