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
    // Windows resolves the default home under LOCALAPPDATA when FM_HOME is unset, so that points at
    // the scratch directory, which has no state beneath it.
    let output = Command::new(env!("CARGO_BIN_EXE_mcao"))
        .args(["audit", "append", "--store"])
        .arg(scratch.store())
        .arg(&refused)
        .env_remove("FM_HOME")
        .env_remove("FM_STATE_OVERRIDE")
        .env("LOCALAPPDATA", &scratch.0)
        .output()
        .expect("mcao runs");
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

// The reference expected values below come from `bin/fm-audit-writer.py` run with
// `python3 -I -B -X utf8` on the committed baseline chain store
// (tests/fixtures/pre-port-baseline/outputs/fm-audit-writer/chain/store) and its inputs
// (chain/inputs/append-N.json). Each test's comment names the exact reference command. No value
// here is produced by this crate.

/// The record for seq 3 that the reference appends to the baseline store with `append-0.json`.
const RECORD_SEQ3: &str = r#"{"actor":{"id":"hmac-sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","type":"user"},"agent":{"harness":"claude","model":null,"role":"firstmate"},"category":"lifecycle","commit":null,"day_key":"2026-10-15","event":"task.dispatched","event_id":"018f3b7e-9c1a-7d2e-8f3a-4b5c6d7e8f90","home_id":"5f1c2b3a-4d5e-4f60-8a71-92b3c4d5e6f7","outcome":"success","platform":"mcao","pr":null,"prev_hash":"sha256:cb74a51e88132d54e517ec8630d67c53446f2091d5f113b85abde183d0c77bb4","repo":"mc-innersource/motif-hve-multi-agent-orchestrator","schema":"mcao.audit-envelope.v1","seq":3,"session_id":"sess-1","task_id":"fix-login","ts":"2026-10-16T02:00:00.000Z","user":{"id":"hmac-sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","identity_source":"github_login","timezone":"America/Chicago"}}"#;
/// The record for seq 4 that the reference appends after an uncommitted seq 3 is adopted.
const RECORD_SEQ4: &str = r#"{"actor":{"id":"hmac-sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","type":"user"},"agent":{"harness":"claude","model":null,"role":"firstmate"},"category":"lifecycle","commit":null,"day_key":"2026-10-15","event":"task.dispatched","event_id":"018f3b7e-9c1a-7d2e-8f3a-4b5c6d7e8f90","home_id":"5f1c2b3a-4d5e-4f60-8a71-92b3c4d5e6f7","outcome":"success","platform":"mcao","pr":null,"prev_hash":"sha256:204ad84928c9e0b0db25778cb4586097f90866fe4911f5ba4c9d3b14532d4bfe","repo":"mc-innersource/motif-hve-multi-agent-orchestrator","schema":"mcao.audit-envelope.v1","seq":4,"session_id":"sess-1","task_id":"fix-login","ts":"2026-10-16T02:00:00.000Z","user":{"id":"hmac-sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","identity_source":"github_login","timezone":"America/Chicago"}}"#;
/// The checkpoint the reference writes after seq 3 is committed in `segment-...0000.jsonl`.
const CHECKPOINT_SEQ3: &str = concat!(
    r#"{"last_hash":"sha256:204ad84928c9e0b0db25778cb4586097f90866fe4911f5ba4c9d3b14532d4bfe","last_seq":3,"segment":"segment-00000000000000000000.jsonl","v":1}"#,
    "\n"
);
/// The checkpoint the reference writes after seq 4 is committed in `segment-...0000.jsonl`.
const CHECKPOINT_SEQ4: &str = concat!(
    r#"{"last_hash":"sha256:d68ee79a025fd71ac13385d52bd0c24d19f68f8bf2087efe64d1d9bc13e5094a","last_seq":4,"segment":"segment-00000000000000000000.jsonl","v":1}"#,
    "\n"
);
/// The unterminated tail the torn-tail cases append to the segment: nine bytes.
const TORN_TAIL: &[u8] = b"{\"partial";

const STDOUT_CLEAN: &str = concat!(
    r#"{"event_id": "018f3b7e-9c1a-7d2e-8f3a-4b5c6d7e8f90", "result": "committed", "seq": 3}"#,
    "\n"
);
const STDOUT_TORN: &str = concat!(
    r#"{"event_id": "018f3b7e-9c1a-7d2e-8f3a-4b5c6d7e8f90", "recovered": {"torn_tail": {"bytes": 9, "segment": "segment-00000000000000000000.jsonl"}}, "result": "committed", "seq": 3}"#,
    "\n"
);
const STDOUT_ADOPTED: &str = concat!(
    r#"{"event_id": "018f3b7e-9c1a-7d2e-8f3a-4b5c6d7e8f90", "recovered": {"adopted": {"segment": "segment-00000000000000000000.jsonl", "seq": 3}}, "result": "committed", "seq": 4}"#,
    "\n"
);
const STDOUT_BOTH: &str = concat!(
    r#"{"event_id": "018f3b7e-9c1a-7d2e-8f3a-4b5c6d7e8f90", "recovered": {"adopted": {"segment": "segment-00000000000000000000.jsonl", "seq": 3}, "torn_tail": {"bytes": 9, "segment": "segment-00000000000000000000.jsonl"}}, "result": "committed", "seq": 4}"#,
    "\n"
);

const ROT1_STDOUT_0: &str = concat!(
    r#"{"event_id": "018f3b7e-9c1a-7d2e-8f3a-4b5c6d7e8f90", "result": "committed", "seq": 0}"#,
    "\n"
);
const ROT1_STDOUT_1: &str = concat!(
    r#"{"event_id": "018f3b7e-9c1a-7d2e-8f3a-4b5c6d7e8f91", "result": "committed", "seq": 1}"#,
    "\n"
);
const ROT1_STDOUT_2: &str = concat!(
    r#"{"event_id": "018f3b7e-9c1a-7d2e-8f3a-4b5c6d7e8f92", "result": "committed", "seq": 2}"#,
    "\n"
);
const ROT2_STDOUT_0: &str = concat!(
    r#"{"event_id": "018f3b7e-9c1a-7d2e-8f3a-4b5c6d7e8f90", "result": "committed", "seq": 0}"#,
    "\n"
);
const ROT2_STDOUT_1: &str = concat!(
    r#"{"event_id": "018f3b7e-9c1a-7d2e-8f3a-4b5c6d7e8f91", "result": "committed", "seq": 1}"#,
    "\n"
);
const ROT2_STDOUT_2: &str = concat!(
    r#"{"event_id": "018f3b7e-9c1a-7d2e-8f3a-4b5c6d7e8f92", "result": "committed", "seq": 2}"#,
    "\n"
);
const ROT2_STDOUT_3: &str = concat!(
    r#"{"event_id": "018f3b7e-9c1a-7d2e-8f3a-4b5c6d7e8f90", "result": "committed", "seq": 3}"#,
    "\n"
);
const ROT2_STDOUT_4: &str = concat!(
    r#"{"event_id": "018f3b7e-9c1a-7d2e-8f3a-4b5c6d7e8f91", "result": "committed", "seq": 4}"#,
    "\n"
);
/// The checkpoint after three records at `--segment-records 1`: seq 2 in segment 2.
const CHECKPOINT_ROTATE1: &str = concat!(
    r#"{"last_hash":"sha256:cb74a51e88132d54e517ec8630d67c53446f2091d5f113b85abde183d0c77bb4","last_seq":2,"segment":"segment-00000000000000000002.jsonl","v":1}"#,
    "\n"
);
/// The checkpoint after five records at `--segment-records 2`: seq 4 in segment 4.
const CHECKPOINT_ROTATE2: &str = concat!(
    r#"{"last_hash":"sha256:9bd372ad92781635c1949f85f9d17eb664bb0a4a1b73d79b652111409bdc3dac","last_seq":4,"segment":"segment-00000000000000000004.jsonl","v":1}"#,
    "\n"
);

/// The baseline chain segment's bytes, which hold the records for seq 0 to 2.
fn baseline_segment() -> Vec<u8> {
    fs::read(baseline_store_file(FIRST_SEGMENT)).expect("the baseline segment")
}

fn baseline_store_file(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../tests/fixtures/pre-port-baseline/outputs/fm-audit-writer/chain/store/{name}"
    ))
}

/// Copies the baseline chain's segment and checkpoint into `store`.
fn write_baseline_store(store: &Path) {
    fs::create_dir_all(store).expect("creating store");
    fs::copy(
        baseline_store_file(FIRST_SEGMENT),
        store.join(FIRST_SEGMENT),
    )
    .expect("copying segment");
    fs::copy(
        baseline_store_file("checkpoint.json"),
        store.join("checkpoint.json"),
    )
    .expect("copying checkpoint");
}

/// Adds bytes to the end of a segment file, as a crash mid-append would leave them.
fn append_raw(path: &Path, bytes: &[u8]) {
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(path)
        .expect("opening the segment");
    file.write_all(bytes).expect("writing the segment");
}

/// A stored line: the reference's canonical record followed by its newline.
fn stored_line(record: &str) -> Vec<u8> {
    format!("{record}\n").into_bytes()
}

/// The store with the record for seq 3 written to its segment but not committed: the checkpoint
/// still names seq 2, which is the state a crash between the record write and the checkpoint
/// publish leaves.
fn write_uncommitted_record_store(store: &Path) {
    write_baseline_store(store);
    append_raw(&store.join(FIRST_SEGMENT), &stored_line(RECORD_SEQ3));
}

fn append_first_input(store: &Path, state: &Path) -> Output {
    let input = chain_input(0);
    mcao(
        &[
            "append",
            "--store",
            store.to_str().unwrap(),
            input.to_str().unwrap(),
        ],
        Some(state),
        None,
    )
}

fn append_input_with_segment_records(
    store: &Path,
    state: &Path,
    records: &str,
    index: usize,
) -> Output {
    let input = chain_input(index);
    mcao(
        &[
            "append",
            "--store",
            store.to_str().unwrap(),
            "--segment-records",
            records,
            input.to_str().unwrap(),
        ],
        Some(state),
        None,
    )
}

/// The segment file names in `store`, sorted.
fn segment_names(store: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(store)
        .expect("reading store")
        .map(|entry| entry.expect("store entry").file_name())
        .filter_map(|name| name.into_string().ok())
        .filter(|name| name.starts_with("segment-"))
        .collect();
    names.sort();
    names
}

/// Reference: `bin/fm-audit-writer.py append --store <copy of the baseline store> chain/inputs/append-0.json`
/// on the baseline store as committed, with nothing added. The `committed` line has no `recovered`.
#[test]
fn append_to_a_clean_store_prints_no_recovered_member() {
    let scratch = Scratch::new("clean-append");
    write_baseline_store(&scratch.store());
    let output = append_first_input(&scratch.store(), &scratch.state());
    assert_eq!(stdout(&output), STDOUT_CLEAN, "stderr: {}", stderr(&output));
    assert_eq!(status(&output), 0, "stderr: {}", stderr(&output));
    let mut segment = baseline_segment();
    segment.extend(stored_line(RECORD_SEQ3));
    assert_eq!(
        fs::read(scratch.store().join(FIRST_SEGMENT)).expect("the segment"),
        segment
    );
    assert_eq!(
        fs::read(scratch.store().join("checkpoint.json")).expect("the checkpoint"),
        CHECKPOINT_SEQ3.as_bytes()
    );
}

/// Reference: the baseline store with `b'{"partial'` (9 bytes, no newline) appended to its segment,
/// then `bin/fm-audit-writer.py append --store <that store> chain/inputs/append-0.json`. The
/// `recovered` member names the truncated tail, and the truncation leaves the committed segment.
#[test]
fn append_after_a_torn_tail_prints_the_truncated_bytes_as_recovered() {
    let scratch = Scratch::new("torn-append");
    write_baseline_store(&scratch.store());
    append_raw(&scratch.store().join(FIRST_SEGMENT), TORN_TAIL);
    let output = append_first_input(&scratch.store(), &scratch.state());
    assert_eq!(stdout(&output), STDOUT_TORN, "stderr: {}", stderr(&output));
    assert_eq!(status(&output), 0, "stderr: {}", stderr(&output));
    let mut segment = baseline_segment();
    segment.extend(stored_line(RECORD_SEQ3));
    assert_eq!(
        fs::read(scratch.store().join(FIRST_SEGMENT)).expect("the segment"),
        segment
    );
    assert_eq!(
        fs::read(scratch.store().join("checkpoint.json")).expect("the checkpoint"),
        CHECKPOINT_SEQ3.as_bytes()
    );
}

/// Reference: the baseline store with the seq 3 record of the clean case written to the segment and
/// the checkpoint still at seq 2, then `bin/fm-audit-writer.py append --store <that store>
/// chain/inputs/append-0.json`. The `adopted` member names seq 3, and the new record is seq 4.
#[test]
fn append_after_an_uncommitted_record_adopts_it_and_prints_it_as_recovered() {
    let scratch = Scratch::new("adopted-append");
    write_uncommitted_record_store(&scratch.store());
    let output = append_first_input(&scratch.store(), &scratch.state());
    assert_eq!(
        stdout(&output),
        STDOUT_ADOPTED,
        "stderr: {}",
        stderr(&output)
    );
    assert_eq!(status(&output), 0, "stderr: {}", stderr(&output));
    let mut segment = baseline_segment();
    segment.extend(stored_line(RECORD_SEQ3));
    segment.extend(stored_line(RECORD_SEQ4));
    assert_eq!(
        fs::read(scratch.store().join(FIRST_SEGMENT)).expect("the segment"),
        segment
    );
    assert_eq!(
        fs::read(scratch.store().join("checkpoint.json")).expect("the checkpoint"),
        CHECKPOINT_SEQ4.as_bytes()
    );
}

/// Reference: the uncommitted seq 3 store of the adopted case with `b'{"partial'` appended after it,
/// then `bin/fm-audit-writer.py append --store <that store> chain/inputs/append-0.json`. Both
/// members appear, in the reference's sorted key order.
#[test]
fn append_after_a_torn_tail_and_an_uncommitted_record_prints_both_repairs() {
    let scratch = Scratch::new("both-append");
    write_uncommitted_record_store(&scratch.store());
    append_raw(&scratch.store().join(FIRST_SEGMENT), TORN_TAIL);
    let output = append_first_input(&scratch.store(), &scratch.state());
    assert_eq!(stdout(&output), STDOUT_BOTH, "stderr: {}", stderr(&output));
    assert_eq!(status(&output), 0, "stderr: {}", stderr(&output));
    let mut segment = baseline_segment();
    segment.extend(stored_line(RECORD_SEQ3));
    segment.extend(stored_line(RECORD_SEQ4));
    assert_eq!(
        fs::read(scratch.store().join(FIRST_SEGMENT)).expect("the segment"),
        segment
    );
    assert_eq!(
        fs::read(scratch.store().join("checkpoint.json")).expect("the checkpoint"),
        CHECKPOINT_SEQ4.as_bytes()
    );
}

/// Reference: three runs of `bin/fm-audit-writer.py append --store <empty store> --segment-records 1
/// chain/inputs/append-{0,1,2}.json`. Each record starts its own segment, so the checkpoint names
/// seq 2 in segment 2. An off-by-one at the threshold (`>=` to `>`) keeps seq 1 in segment 0.
#[test]
fn segment_records_of_one_starts_a_segment_for_every_record() {
    let scratch = Scratch::new("rotate-one");
    let expected = [ROT1_STDOUT_0, ROT1_STDOUT_1, ROT1_STDOUT_2];
    for (index, stdout_line) in expected.iter().enumerate() {
        let output =
            append_input_with_segment_records(&scratch.store(), &scratch.state(), "1", index);
        assert_eq!(
            stdout(&output),
            *stdout_line,
            "append {index}: {}",
            stderr(&output)
        );
        assert_eq!(status(&output), 0, "append {index}: {}", stderr(&output));
    }
    assert_eq!(
        segment_names(&scratch.store()),
        [
            "segment-00000000000000000000.jsonl",
            "segment-00000000000000000001.jsonl",
            "segment-00000000000000000002.jsonl",
        ]
    );
    assert_eq!(
        fs::read(scratch.store().join("checkpoint.json")).expect("the checkpoint"),
        CHECKPOINT_ROTATE1.as_bytes()
    );
}

/// Reference: five runs of `bin/fm-audit-writer.py append --store <empty store> --segment-records 2
/// chain/inputs/append-{0,1,2,0,1}.json`. Segments start at seq 0, 2, and 4, so the threshold is
/// reached after every second record, and the checkpoint names seq 4 in segment 4.
#[test]
fn segment_records_of_two_rotates_after_every_second_record() {
    let scratch = Scratch::new("rotate-two");
    let expected = [
        ROT2_STDOUT_0,
        ROT2_STDOUT_1,
        ROT2_STDOUT_2,
        ROT2_STDOUT_3,
        ROT2_STDOUT_4,
    ];
    for (index, stdout_line) in expected.iter().enumerate() {
        let output =
            append_input_with_segment_records(&scratch.store(), &scratch.state(), "2", index % 3);
        assert_eq!(
            stdout(&output),
            *stdout_line,
            "append {index}: {}",
            stderr(&output)
        );
        assert_eq!(status(&output), 0, "append {index}: {}", stderr(&output));
    }
    assert_eq!(
        segment_names(&scratch.store()),
        [
            "segment-00000000000000000000.jsonl",
            "segment-00000000000000000002.jsonl",
            "segment-00000000000000000004.jsonl",
        ]
    );
    assert_eq!(
        fs::read(scratch.store().join("checkpoint.json")).expect("the checkpoint"),
        CHECKPOINT_ROTATE2.as_bytes()
    );
}
