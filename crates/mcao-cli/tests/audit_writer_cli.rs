//! `mcao audit append`, `verify`, and `project-ledger` behavior that the baseline chain does not
//! reach: failure results, the store lock, the flags, and input errors (DPES-753, DPES-884 AC4).
//!
//! Every expected stdout line, stderr presence, and exit status was recorded by running
//! `bin/fm-audit-writer.py` with `python3 -I -B -X utf8` on the same inputs, at the same
//! `FM_STATE_OVERRIDE` layout. That file is identical on `origin/DPES-751` and at main `d80bcd15`
//! (only `bin/fm-audit-activity-map.py` differs, per the baseline README). No expected value comes
//! from this crate.
//!
//! Stderr wording of a usage or input error differs from the reference (the Rust line names
//! `mcao audit <verb>`), so those tests compare stdout, the exit status, and only whether stderr is
//! empty or names the same cause.

#![cfg(unix)]

use std::fs::{self, File};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const FIRST_SEGMENT: &str = "segment-00000000000000000000.jsonl";

/// The three committed append inputs of the baseline chain, which name `America/Chicago`.
fn chain_input(index: usize) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../tests/fixtures/pre-port-baseline/outputs/fm-audit-writer/chain/inputs/append-{index}.json"
    ))
}

/// The envelope the reference rejects for carrying a caller `seq`.
fn refused_envelope() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/audit-validate/envelope-seq-one-null-prev.json")
}

/// A scratch directory under the system temp dir, removed when the test ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("the clock is after the epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "mcao-audit-writer-cli-{label}-{}-{nanos}",
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

/// Runs `mcao audit <args>` with the rejection log under `state`, or with no state location.
fn mcao(args: &[&str], state: Option<&Path>, stdin: Option<&[u8]>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mcao"));
    command
        .args(["audit"])
        .args(args)
        .env_remove("FM_HOME")
        .env_remove("FM_STATE_OVERRIDE")
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    if let Some(state) = state {
        command.env("FM_STATE_OVERRIDE", state);
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("mcao runs");
    if let Some(bytes) = stdin {
        child
            .stdin
            .take()
            .expect("piped stdin")
            .write_all(bytes)
            .expect("writing stdin");
    }
    child.wait_with_output().expect("mcao finishes")
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("UTF-8 stdout")
}

fn status(output: &Output) -> i32 {
    output.status.code().expect("an exit status")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Appends the first `count` committed inputs to `store`, asserting each commits.
fn seed(store: &Path, state: &Path, count: usize) {
    for index in 0..count {
        let input = chain_input(index);
        let output = mcao(
            &[
                "append",
                "--store",
                store.to_str().unwrap(),
                input.to_str().unwrap(),
            ],
            Some(state),
            None,
        );
        assert_eq!(
            status(&output),
            0,
            "seeding append {index}: {}",
            stderr(&output)
        );
    }
}

/// The store's lock file as the store path names it.
fn lock_path(store: &Path) -> PathBuf {
    store.join(".lock")
}

/// The `failed` line for an OSError during an append is `{"error": ..., "may_be_durable": false,
/// "result": "failed"}`. A lock path that is a directory makes the lock open fail with EISDIR, which
/// the reference reports as `IsADirectoryError`, and nothing reaches the commit point.
#[test]
fn an_os_error_during_append_reports_a_failed_result_that_is_not_durable() {
    let scratch = Scratch::new("eisdir");
    fs::create_dir_all(scratch.store().join(".lock")).expect("a directory at the lock path");
    let input = chain_input(0);
    let output = mcao(
        &[
            "append",
            "--store",
            scratch.store().to_str().unwrap(),
            input.to_str().unwrap(),
        ],
        Some(&scratch.state()),
        None,
    );
    assert_eq!(
        stdout(&output),
        "{\"error\": \"IsADirectoryError\", \"may_be_durable\": false, \"result\": \"failed\"}\n"
    );
    assert_eq!(status(&output), 3, "stderr: {}", stderr(&output));
    assert!(stderr(&output).is_empty());
    assert!(!scratch.store().join(FIRST_SEGMENT).exists());
}

/// A store path that is a regular file makes the directory create fail with EEXIST, which the
/// reference reports as `FileExistsError`.
#[test]
fn a_store_path_that_is_a_file_reports_file_exists_error() {
    let scratch = Scratch::new("eexist");
    fs::write(scratch.store(), b"not a directory").expect("a file at the store path");
    let input = chain_input(0);
    let output = mcao(
        &[
            "append",
            "--store",
            scratch.store().to_str().unwrap(),
            input.to_str().unwrap(),
        ],
        Some(&scratch.state()),
        None,
    );
    assert_eq!(
        stdout(&output),
        "{\"error\": \"FileExistsError\", \"may_be_durable\": false, \"result\": \"failed\"}\n"
    );
    assert_eq!(status(&output), 3, "stderr: {}", stderr(&output));
}

/// A directory at the checkpoint's temporary path makes the publish fail after the record is
/// written. The write is rolled back, so the reference reports `may_be_durable` false, and the store
/// verifies and takes the next append as if the failed one never happened.
#[test]
fn a_failed_checkpoint_publish_rolls_back_and_reports_not_durable() {
    let scratch = Scratch::new("publish");
    let state = scratch.state();
    seed(&scratch.store(), &state, 3);
    let committed = fs::read(scratch.store().join(FIRST_SEGMENT)).expect("the committed segment");
    fs::create_dir(scratch.store().join("checkpoint.json.tmp")).expect("a blocking directory");

    let input = chain_input(0);
    let output = mcao(
        &[
            "append",
            "--store",
            scratch.store().to_str().unwrap(),
            input.to_str().unwrap(),
        ],
        Some(&state),
        None,
    );
    assert_eq!(
        stdout(&output),
        "{\"may_be_durable\": false, \"result\": \"failed\"}\n"
    );
    assert_eq!(status(&output), 3, "stderr: {}", stderr(&output));
    assert_eq!(
        fs::read(scratch.store().join(FIRST_SEGMENT)).expect("the segment"),
        committed,
        "the failed record must be rolled back to the committed length"
    );

    fs::remove_dir(scratch.store().join("checkpoint.json.tmp")).expect("removing the blocker");
    let verified = mcao(
        &["verify", "--store", scratch.store().to_str().unwrap()],
        Some(&state),
        None,
    );
    assert_eq!(status(&verified), 0, "stderr: {}", stderr(&verified));
    assert!(
        stdout(&verified).contains("\"last_seq\": 2"),
        "{}",
        stdout(&verified)
    );
}

/// The reference's lock is `flock` on the path `.lock`, not on an inode it keeps. Once the path is
/// unlinked, the next append opens a new file and takes its lock, so it commits while the earlier
/// holder still holds the old one. The Rust command does the same (the reference, run on this host,
/// committed seq 0 in that state). This pins the behavior; it is not a fix.
#[test]
fn an_unlinked_lock_path_lets_an_append_proceed_while_the_old_holder_keeps_its_lock() {
    let scratch = Scratch::new("unlinked-lock");
    fs::create_dir_all(scratch.store()).expect("creating store");
    let holder = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path(&scratch.store()))
        .expect("the lock file");
    // SAFETY: `holder` is an open descriptor for the duration of this call; flock takes no pointers.
    let locked = unsafe { libc::flock(holder.as_raw_fd(), libc::LOCK_EX) };
    assert_eq!(locked, 0, "the test holds the store lock");
    fs::remove_file(lock_path(&scratch.store())).expect("unlinking the held lock path");

    let store = scratch.store();
    let state = scratch.state();
    let input = chain_input(0);
    let (done, outcome) = mpsc::channel();
    std::thread::spawn(move || {
        let output = mcao(
            &[
                "append",
                "--store",
                store.to_str().unwrap(),
                input.to_str().unwrap(),
            ],
            Some(&state),
            None,
        );
        let _ = done.send(output);
    });
    let output = outcome
        .recv_timeout(Duration::from_secs(20))
        .expect("the append commits while the old holder keeps its lock");
    assert_eq!(status(&output), 0, "stderr: {}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "{\"event_id\": \"018f3b7e-9c1a-7d2e-8f3a-4b5c6d7e8f90\", \"result\": \"committed\", \"seq\": 0}\n"
    );
    drop(holder);
}

/// A tampered committed record is reported as the first damage with `corrupt` and exit 1, as the
/// reference reports it. The reference's result for the same edit is `hash_mismatch` at seq 1.
#[test]
fn verify_reports_an_edited_record_as_corrupt() {
    let scratch = Scratch::new("tamper");
    let state = scratch.state();
    seed(&scratch.store(), &state, 2);
    let segment = scratch.store().join(FIRST_SEGMENT);
    let edited = fs::read_to_string(&segment)
        .expect("the segment")
        .replace("fix-login", "fix-logix");
    fs::write(&segment, edited).expect("writing the edit");

    let output = mcao(
        &["verify", "--store", scratch.store().to_str().unwrap()],
        Some(&state),
        None,
    );
    assert_eq!(
        stdout(&output),
        "{\"intact_segments\": [], \"paths\": [\"/prev_hash\"], \"reason\": \"hash_mismatch\", \
         \"result\": \"corrupt\", \"segment\": \"segment-00000000000000000000.jsonl\", \"seq\": 1}\n"
    );
    assert_eq!(status(&output), 1, "stderr: {}", stderr(&output));
}

/// An unterminated tail is reported under `torn_tail` and verify still exits 0. Verify never repairs,
/// so the tail stays on disk.
#[test]
fn verify_reports_an_unterminated_tail_and_exits_zero() {
    let scratch = Scratch::new("torn");
    let state = scratch.state();
    seed(&scratch.store(), &state, 2);
    let segment = scratch.store().join(FIRST_SEGMENT);
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&segment)
        .expect("the segment");
    file.write_all(b"{\"partial").expect("a torn tail");
    drop(file);

    let output = mcao(
        &["verify", "--store", scratch.store().to_str().unwrap()],
        Some(&state),
        None,
    );
    assert_eq!(
        stdout(&output),
        "{\"committed_seq\": 1, \"last_seq\": 1, \"records\": 2, \"result\": \"verified\", \
         \"segments\": [{\"name\": \"segment-00000000000000000000.jsonl\", \"records\": 2, \
         \"status\": \"intact\"}], \"torn_tail\": {\"bytes\": 9, \"segment\": \
         \"segment-00000000000000000000.jsonl\"}, \"uncommitted_seq\": null}\n"
    );
    assert_eq!(status(&output), 0, "stderr: {}", stderr(&output));
    assert!(
        fs::read(&segment)
            .expect("the segment")
            .ends_with(b"{\"partial")
    );
}

#[test]
fn append_reads_the_envelope_from_stdin() {
    let scratch = Scratch::new("stdin");
    let bytes = fs::read(chain_input(0)).expect("the committed input");
    let output = mcao(
        &["append", "--store", scratch.store().to_str().unwrap(), "-"],
        Some(&scratch.state()),
        Some(&bytes),
    );
    assert_eq!(
        stdout(&output),
        "{\"event_id\": \"018f3b7e-9c1a-7d2e-8f3a-4b5c6d7e8f90\", \"result\": \"committed\", \"seq\": 0}\n"
    );
    assert_eq!(status(&output), 0, "stderr: {}", stderr(&output));
}

/// A rejection prints `rejected` with exit 1 and appends one line to the rejection log, as
/// `audit validate` does for the same definition.
#[test]
fn a_rejected_envelope_is_recorded_and_exits_one() {
    let scratch = Scratch::new("rejected");
    let refused = refused_envelope();
    let output = mcao(
        &[
            "append",
            "--store",
            scratch.store().to_str().unwrap(),
            refused.to_str().unwrap(),
        ],
        Some(&scratch.state()),
        None,
    );
    assert_eq!(
        stdout(&output),
        "{\"definition\": \"envelope\", \"paths\": [\"/seq\"], \"reason\": \"schema_invalid\", \
         \"result\": \"rejected\"}\n"
    );
    assert_eq!(status(&output), 1, "stderr: {}", stderr(&output));
    let log = fs::read_to_string(scratch.state().join("audit-rejections.jsonl"))
        .expect("the rejection log");
    assert_eq!(log.lines().count(), 1);
    assert!(
        log.starts_with(
            "{\"definition\": \"envelope\", \"paths\": [\"/seq\"], \"reason\": \"schema_invalid\", \
             \"rejected_at\": "
        ),
        "{log}"
    );
    assert!(
        !scratch.store().exists(),
        "a rejected envelope must not create the store"
    );
}

/// A rejection that cannot be recorded is an input error (exit 2) and prints no result line, as the
/// reference does when neither `FM_STATE_OVERRIDE` nor `FM_HOME` is set.
#[test]
fn a_rejection_with_no_state_location_is_not_recorded_and_exits_two() {
    let scratch = Scratch::new("no-state");
    let refused = refused_envelope();
    let output = mcao(
        &[
            "append",
            "--store",
            scratch.store().to_str().unwrap(),
            refused.to_str().unwrap(),
        ],
        None,
        None,
    );
    assert!(stdout(&output).is_empty(), "stdout: {}", stdout(&output));
    assert_eq!(status(&output), 2, "stderr: {}", stderr(&output));
    assert!(
        stderr(&output).contains("rejection not recorded"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn append_without_a_store_is_a_usage_error() {
    let scratch = Scratch::new("no-store");
    let input = chain_input(0);
    let output = mcao(
        &["append", input.to_str().unwrap()],
        Some(&scratch.state()),
        None,
    );
    assert!(stdout(&output).is_empty(), "stdout: {}", stdout(&output));
    assert_eq!(status(&output), 2, "stderr: {}", stderr(&output));
}

#[test]
fn segment_records_below_one_is_an_input_error() {
    let scratch = Scratch::new("segment-records");
    let input = chain_input(0);
    let output = mcao(
        &[
            "append",
            "--store",
            scratch.store().to_str().unwrap(),
            "--segment-records",
            "0",
            input.to_str().unwrap(),
        ],
        Some(&scratch.state()),
        None,
    );
    assert!(stdout(&output).is_empty(), "stdout: {}", stdout(&output));
    assert_eq!(status(&output), 2, "stderr: {}", stderr(&output));
    assert!(stderr(&output).contains("--segment-records must be at least 1"));
    assert!(!scratch.store().exists());
}

#[test]
fn verify_and_project_ledger_refuse_a_missing_store_with_exit_two() {
    let scratch = Scratch::new("missing");
    let store = scratch.store();
    for verb in ["verify", "project-ledger"] {
        let output = mcao(
            &[verb, "--store", store.to_str().unwrap()],
            Some(&scratch.state()),
            None,
        );
        assert!(
            stdout(&output).is_empty(),
            "{verb} stdout: {}",
            stdout(&output)
        );
        assert_eq!(status(&output), 2, "{verb} stderr: {}", stderr(&output));
        assert!(
            stderr(&output).contains("store directory does not exist"),
            "{verb}"
        );
    }
}

#[test]
fn a_duplicate_key_on_stdin_is_an_input_error() {
    let scratch = Scratch::new("duplicate");
    let output = mcao(
        &["append", "--store", scratch.store().to_str().unwrap(), "-"],
        Some(&scratch.state()),
        Some(br#"{"a": 1, "a": 2}"#),
    );
    assert!(stdout(&output).is_empty(), "stdout: {}", stdout(&output));
    assert_eq!(status(&output), 2, "stderr: {}", stderr(&output));
    assert!(stderr(&output).contains("duplicate object key"));
    assert!(
        !scratch.store().exists(),
        "an input error must not create the store"
    );
}

#[test]
fn an_unreadable_input_file_is_an_input_error() {
    let scratch = Scratch::new("no-input");
    let missing = scratch.0.join("absent.json");
    let output = mcao(
        &[
            "append",
            "--store",
            scratch.store().to_str().unwrap(),
            missing.to_str().unwrap(),
        ],
        Some(&scratch.state()),
        None,
    );
    assert!(stdout(&output).is_empty(), "stdout: {}", stdout(&output));
    assert_eq!(status(&output), 2, "stderr: {}", stderr(&output));
}
