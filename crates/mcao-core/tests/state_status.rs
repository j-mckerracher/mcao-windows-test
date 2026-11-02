//! Tests for the append-only status log and its open-decision fold.
//!
//! The fixtures under `tests/fixtures/status/` hold the output of `bin/fm-classify-lib.sh`, and
//! the parity test compares this crate's fold against them. No expected value in this file is
//! computed by Rust. The literal expectations below were read from the shell's output for the
//! same input, and each one pins a single rule so that a change to that rule fails by name.

mod support;

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use mcao_core::McaoError;
use mcao_core::state::status::HELD_VERB;
use mcao_core::state::{ByteRange, OpenDecision, StatusKind, StatusLog};
use support::{TempDir, write};

/// The fixture cases, in the order `gen-status-fixtures.sh` runs them.
const CASES: [&str; 12] = [
    "empty",
    "slug-resolve",
    "no-slug-default",
    "done-clears",
    "corr-ids",
    "stamps-with-colons",
    "note-head-slug",
    "bad-slug-and-prose",
    "reserved-pending-reply",
    "held-closes",
    "verbs-and-guards",
    "no-trailing-newline",
];

/// Each kind a case is folded as, paired with the name its expected files use.
const KINDS: [(&str, StatusKind); 4] = [
    ("ship", StatusKind::Ship),
    ("scout", StatusKind::Scout),
    ("secondmate", StatusKind::Secondmate),
    ("unknown", StatusKind::Unknown),
];

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/status")
}

/// Renders decisions as the shell prints them: `key<TAB>verb<TAB>note` per decision, joined by
/// newlines, with no trailing newline.
fn render(decisions: &[OpenDecision]) -> String {
    decisions
        .iter()
        .map(|decision| format!("{}\t{}\t{}", decision.key, decision.verb, decision.note))
        .collect::<Vec<_>>()
        .join("\n")
}

fn open_in(log: &StatusLog, kind: StatusKind) -> String {
    render(&log.open_decisions(kind).expect("fold status log"))
}

#[test]
fn folds_match_the_shell_classifier_on_saved_fixtures() {
    let dir = fixture_dir();
    let status_files = fs::read_dir(&dir)
        .expect("read fixture dir")
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "status"))
        .count();
    assert_eq!(
        status_files,
        CASES.len(),
        "fixture set changed; regenerate it and update CASES"
    );

    for case in CASES {
        let log = StatusLog::new(dir.join(format!("{case}.status")));
        for (name, kind) in KINDS {
            let expected = fs::read_to_string(dir.join(format!("{case}.{name}.open")))
                .expect("read shell expectation");
            assert_eq!(open_in(&log, kind), expected, "case {case}, kind {name}");
        }
    }
}

#[test]
fn append_creates_the_file_and_writes_one_line() {
    let dir = TempDir::new("status-create");
    let path = dir.path().join("task.status");
    let log = StatusLog::new(&path);

    log.append("working: first").expect("append first line");

    assert_eq!(
        fs::read_to_string(&path).expect("read status file"),
        "working: first\n"
    );
}

#[test]
fn append_never_rewrites_existing_bytes() {
    let dir = TempDir::new("status-append");
    let path = dir.path().join("task.status");
    write(&path, "working: first\n");
    let log = StatusLog::new(&path);

    log.append("needs-decision [key=a]: second")
        .expect("append second line");
    log.append("working: third").expect("append third line");

    assert_eq!(
        fs::read_to_string(&path).expect("read status file"),
        "working: first\nneeds-decision [key=a]: second\nworking: third\n"
    );
}

#[test]
fn append_refuses_an_embedded_newline_before_touching_the_file() {
    let dir = TempDir::new("status-newline");
    let path = dir.path().join("task.status");
    let log = StatusLog::new(&path);

    let error = log
        .append("working: one\nneeds-decision: two")
        .expect_err("a line with a newline must be refused");

    assert!(matches!(error, McaoError::Usage(_)), "got {error:?}");
    assert!(!path.exists(), "a refused append must not create the file");
}

fn event(thread: usize, index: usize) -> String {
    format!("working: thread {thread} line {index} {}", "x".repeat(170))
}

#[test]
fn concurrent_appends_keep_every_event_whole() {
    const THREADS: usize = 8;
    const LINES: usize = 500;
    let dir = TempDir::new("status-concurrent");
    let path = dir.path().join("task.status");
    let log = StatusLog::new(&path);

    std::thread::scope(|scope| {
        for thread in 0..THREADS {
            let log = &log;
            scope.spawn(move || {
                for index in 0..LINES {
                    log.append(&event(thread, index))
                        .expect("append from worker thread");
                }
            });
        }
    });

    let content = fs::read_to_string(&path).expect("read status file");
    let mut actual: Vec<&str> = content.lines().collect();
    let mut expected: Vec<String> = (0..THREADS)
        .flat_map(|thread| (0..LINES).map(move |index| event(thread, index)))
        .collect();
    actual.sort_unstable();
    expected.sort_unstable();

    assert!(content.ends_with('\n'), "every event must end in a newline");
    assert!(
        actual.iter().all(|line| !line.is_empty()),
        "with only Rust appenders, no separator line may be blank"
    );
    assert_eq!(
        actual, expected,
        "every event must appear exactly once, whole"
    );
}

/// Readers take no lock, so a fold that runs during concurrent appends must succeed. Each writer
/// opens a key and closes every odd-numbered one, so the keys still open at the end are the even
/// ones. A read can see the final line with no newline while its write is in flight, and the fold
/// must read that line as it reads a torn one. Every read must succeed, and the fold after the
/// writers finish must hold exactly the even keys.
#[test]
fn readers_during_concurrent_appends_get_no_error() {
    const THREADS: usize = 4;
    const LINES: usize = 250;
    let dir = TempDir::new("status-readers");
    let path = dir.path().join("task.status");
    let log = StatusLog::new(&path);
    let done = AtomicBool::new(false);
    let log = &log;
    let done = &done;

    let reads = std::thread::scope(|scope| {
        let reader = scope.spawn(move || {
            let mut reads = 0_usize;
            while !done.load(Ordering::SeqCst) {
                log.open_decisions(StatusKind::Ship)
                    .expect("a read during concurrent appends must not fail");
                reads += 1;
            }
            reads
        });
        let writers: Vec<_> = (0..THREADS)
            .map(|thread| {
                scope.spawn(move || {
                    for index in 0..LINES {
                        let key = format!("t{thread}-{index}");
                        let opening = format!("needs-decision [key={key}]: {}", "x".repeat(170));
                        log.append(&opening).expect("append an opening line");
                        if index % 2 == 1 {
                            log.append(&format!("resolved [key={key}]: closed"))
                                .expect("append a closing line");
                        }
                    }
                })
            })
            .collect();
        // Stop the reader before any writer's failure can propagate, so the scope always ends.
        let writer_results: Vec<_> = writers.into_iter().map(|writer| writer.join()).collect();
        done.store(true, Ordering::SeqCst);
        for result in writer_results {
            result.expect("writer thread");
        }
        reader.join().expect("reader thread")
    });

    assert!(reads > 0, "the reader must overlap the writers");
    let mut expected: Vec<String> = (0..THREADS)
        .flat_map(|thread| {
            (0..LINES)
                .filter(|index| index % 2 == 0)
                .map(move |index| format!("t{thread}-{index}"))
        })
        .collect();
    let mut keys: Vec<String> = log
        .open_decisions(StatusKind::Ship)
        .expect("fold after the appends")
        .into_iter()
        .map(|decision| decision.key)
        .collect();
    expected.sort_unstable();
    keys.sort_unstable();
    assert_eq!(
        keys, expected,
        "the even keys stay open once all appends finish"
    );
}

/// An append takes the sidecar lock before its torn-tail check. While another handle holds that
/// lock, the append must wait, and it must finish once the lock is released.
#[test]
fn append_waits_for_the_sidecar_lock() {
    let dir = TempDir::new("status-sidecar-lock");
    let path = dir.path().join("task.status");
    let holder = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.path().join("task.status.lock"))
        .expect("open the sidecar lock");
    holder.lock().expect("take the sidecar lock");
    let log = StatusLog::new(&path);
    let (sent, received) = mpsc::channel();
    let writer = std::thread::spawn(move || {
        let result = log.append("working: waiting");
        sent.send(()).expect("report completion");
        result
    });

    assert!(
        received.recv_timeout(Duration::from_millis(200)).is_err(),
        "an append must wait while the sidecar lock is held"
    );
    holder.unlock().expect("release the sidecar lock");
    received
        .recv_timeout(Duration::from_secs(30))
        .expect("the append completes once the lock is released");
    writer
        .join()
        .expect("writer thread")
        .expect("append succeeds");
    assert_eq!(
        fs::read_to_string(&path).expect("read status file"),
        "working: waiting\n"
    );
}

/// A keyless decision is `default`. Each closing line below has a `[key=` opener that never
/// reaches `]`, at the head or at the note head, for each closing verb. The fold refuses every one,
/// so the keyless decision survives. The shell closes `default` on the colon-bearing forms; that is
/// the deliberate difference the module documents.
#[test]
fn malformed_close_lines_never_close_a_decision() {
    let dir = TempDir::new("status-malformed");
    let log = StatusLog::new(dir.path().join("task.status"));
    log.append("needs-decision: held")
        .expect("open keyless decision");

    const HELD: &str = "default\tneeds-decision\theld";
    let mut malformed: Vec<String> = [
        "resolved [key=a: unterminated key token",
        "resolved [key=a",
        "resolved: [key=a unterminated note head",
        "resolved: [key=a",
        "resolved [key=bad slug]: space in the slug",
        "resolved [key=]: empty slug",
        "resolved corr=0123456789abcde [key=a]: fifteen hex digits",
        "resolved corr=zzzzzzzzzzzzzzzz [key=a]: non-hex token",
        "resolved x=1 [key=a]: unknown tag before the verb",
        "resolved [key=a/b]: slash in the slug",
    ]
    .iter()
    .map(|line| line.to_string())
    .collect();
    malformed.extend([
        format!("{HELD_VERB} [key=a: unterminated key token"),
        format!("{HELD_VERB} [key=a"),
        format!("{HELD_VERB}: [key=a unterminated note head"),
        format!("{HELD_VERB}: [key=a"),
    ]);
    for line in &malformed {
        log.append(line).expect("append malformed line");
        assert_eq!(open_in(&log, StatusKind::Ship), HELD, "after {line:?}");
    }

    log.append("resolved: real close")
        .expect("append real close");
    assert_eq!(open_in(&log, StatusKind::Ship), "");
}

/// An empty key slug is not a key, so the line opens nothing.
#[test]
fn an_empty_key_slug_opens_no_decision() {
    let dir = TempDir::new("status-empty-slug");
    let log = StatusLog::new(dir.path().join("task.status"));
    log.append("needs-decision: held")
        .expect("open keyless decision");

    log.append("needs-decision [key=]: empty slug opens nothing")
        .expect("append empty-slug opener");

    assert_eq!(
        open_in(&log, StatusKind::Ship),
        "default\tneeds-decision\theld"
    );
}

/// A correlation token has exactly sixteen hex digits. Seventeen is not a token, so the word stays
/// in the verb and the line does not close; sixteen closes.
#[test]
fn a_correlation_token_has_exactly_sixteen_hex_digits() {
    let dir = TempDir::new("status-corr-length");
    let log = StatusLog::new(dir.path().join("task.status"));
    log.append("needs-decision [key=a]: held")
        .expect("open decision a");

    log.append("resolved corr=0123456789abcdef0 [key=a]: seventeen hex digits")
        .expect("append seventeen-digit closer");
    assert_eq!(
        open_in(&log, StatusKind::Ship),
        "a\tneeds-decision\theld",
        "seventeen digits must not close"
    );

    log.append("resolved corr=0123456789abcdef [key=a]: sixteen hex digits")
        .expect("append sixteen-digit closer");
    assert_eq!(open_in(&log, StatusKind::Ship), "");
}

/// A carriage return is shell space, so it may precede a closing verb.
#[test]
fn a_carriage_return_is_space_before_a_closing_verb() {
    let dir = TempDir::new("status-cr");
    let log = StatusLog::new(dir.path().join("task.status"));
    log.append("needs-decision [key=a]: held")
        .expect("open decision a");

    log.append("\rresolved [key=a]: closes after a carriage return")
        .expect("append carriage-return closer");

    assert_eq!(open_in(&log, StatusKind::Ship), "");
}

/// A key token at the note head is removed from the note, so the note keeps only the words after
/// it.
#[test]
fn a_note_head_key_token_is_not_part_of_the_note() {
    let dir = TempDir::new("status-note-head");
    let log = StatusLog::new(dir.path().join("task.status"));

    log.append("needs-decision: [key=a] held here")
        .expect("open note-head decision");

    assert_eq!(
        open_in(&log, StatusKind::Ship),
        "a\tneeds-decision\theld here"
    );
}

/// A time stamp after the colon is ordinary note text, so it stays in the note.
#[test]
fn a_stamp_after_the_colon_stays_in_the_note() {
    let dir = TempDir::new("status-stamp-note");
    let log = StatusLog::new(dir.path().join("task.status"));

    log.append("needs-decision: waiting [at=1792862193] for reply")
        .expect("open decision with a stamp in its note");

    assert_eq!(
        open_in(&log, StatusKind::Ship),
        "default\tneeds-decision\twaiting [at=1792862193] for reply"
    );
}

#[test]
fn reserved_keys_move_only_for_their_namespace() {
    let dir = TempDir::new("status-reserved");
    let log = StatusLog::new(dir.path().join("task.status"));
    log.append("needs-decision [key=pending-reply-r1]: pending-reply-missed: opened in namespace")
        .expect("open reserved key");
    log.append("resolved [key=pending-reply-r1]: plain close")
        .expect("append plain close");
    assert_eq!(
        open_in(&log, StatusKind::Ship),
        "pending-reply-r1\tneeds-decision\tpending-reply-missed: opened in namespace"
    );

    log.append("resolved [key=pending-reply-r1]: pending-reply-ok: namespaced close")
        .expect("append namespaced close");
    assert_eq!(open_in(&log, StatusKind::Ship), "");

    log.append("needs-decision [key=pending-reply-r2]: plain open")
        .expect("append plain open of reserved key");
    assert_eq!(open_in(&log, StatusKind::Ship), "");
}

/// A reserved key opens only when its note starts with the namespace and has a second colon.
#[test]
fn a_reserved_key_opens_only_with_a_namespaced_note_and_a_colon() {
    let dir = TempDir::new("status-reserved-colon");
    let log = StatusLog::new(dir.path().join("task.status"));

    log.append("needs-decision [key=pending-reply-r9]: pending-reply-no-colon")
        .expect("append namespaced note without a colon");
    assert_eq!(open_in(&log, StatusKind::Ship), "");

    log.append("needs-decision [key=pending-reply-r9]: pending-reply-ok: opens")
        .expect("append namespaced note with a colon");
    assert_eq!(
        open_in(&log, StatusKind::Ship),
        "pending-reply-r9\tneeds-decision\tpending-reply-ok: opens"
    );
}

#[test]
fn a_missing_file_holds_no_open_decisions() {
    let dir = TempDir::new("status-missing");
    let log = StatusLog::new(dir.path().join("absent.status"));

    let decisions = log
        .open_decisions(StatusKind::Ship)
        .expect("a missing status file is not an error");

    assert!(decisions.is_empty());
}

#[test]
fn a_directory_in_the_status_path_is_uncertain_not_empty() {
    let dir = TempDir::new("status-directory");
    let log = StatusLog::new(dir.path());

    let error = log
        .open_decisions(StatusKind::Ship)
        .expect_err("a directory is not a readable status file");

    assert!(matches!(error, McaoError::Uncertain(_)), "got {error:?}");
}

#[cfg(unix)]
#[test]
fn a_symlinked_status_file_is_uncertain_not_empty() {
    let dir = TempDir::new("status-symlink");
    let target = dir.path().join("real.status");
    write(&target, "needs-decision [key=a]: held\n");
    let link = dir.path().join("task.status");
    std::os::unix::fs::symlink(&target, &link).expect("create symlink");

    let error = StatusLog::new(&link)
        .open_decisions(StatusKind::Ship)
        .expect_err("a symlink is not trusted as a status file");

    assert!(matches!(error, McaoError::Uncertain(_)), "got {error:?}");
}

#[test]
fn non_utf8_bytes_are_replaced_in_the_note() {
    let dir = TempDir::new("status-utf8");
    let path = dir.path().join("task.status");
    write(&path, b"needs-decision [key=a]: caf\xe9\n");
    let log = StatusLog::new(&path);

    assert_eq!(
        open_in(&log, StatusKind::Ship),
        "a\tneeds-decision\tcaf\u{FFFD}"
    );
}

/// Returns the bytes a range covers in the file at `path`.
fn bytes_in(path: &std::path::Path, range: ByteRange) -> Vec<u8> {
    let bytes = fs::read(path).expect("read status file");
    let start = usize::try_from(range.start).expect("start fits in usize");
    let end = usize::try_from(range.end).expect("end fits in usize");
    bytes[start..end].to_vec()
}

#[test]
fn append_returns_the_byte_range_of_each_record() {
    let dir = TempDir::new("status-range");
    let path = dir.path().join("task.status");
    let log = StatusLog::new(&path);

    let first = log.append("working: first").expect("append first line");
    let second = log
        .append("needs-decision [key=a]: second")
        .expect("append second line");

    assert_eq!(first.start, 0, "the first record starts at the file start");
    assert_eq!(bytes_in(&path, first), b"working: first\n");
    assert_eq!(second.start, first.end, "records are contiguous");
    assert_eq!(bytes_in(&path, second), b"needs-decision [key=a]: second\n");
}

/// A final line with no newline is closed by the append's own separator, so the next record does
/// not join it. The separator is part of the range, and the torn line folds as its own event.
#[test]
fn a_torn_final_line_is_closed_before_the_next_record() {
    let dir = TempDir::new("status-torn");
    let path = dir.path().join("task.status");
    write(&path, "needs-decision [key=a]: held\nworking: torn");
    let log = StatusLog::new(&path);

    let range = log
        .append("resolved [key=a]: closes")
        .expect("append after a torn line");

    assert_eq!(
        bytes_in(&path, range),
        b"\nresolved [key=a]: closes\n",
        "the range starts at the separator"
    );
    assert_eq!(
        fs::read_to_string(&path).expect("read status file"),
        "needs-decision [key=a]: held\nworking: torn\nresolved [key=a]: closes\n"
    );
    assert_eq!(open_in(&log, StatusKind::Ship), "");
}

#[test]
fn append_on_a_directory_is_uncertain() {
    let dir = TempDir::new("status-append-directory");
    let log = StatusLog::new(dir.path());

    let error = log
        .append("working: appended")
        .expect_err("a directory is not a status file");

    assert!(matches!(error, McaoError::Uncertain(_)), "got {error:?}");
}

#[cfg(unix)]
#[test]
fn append_refuses_a_symlink_and_leaves_its_target_unchanged() {
    let dir = TempDir::new("status-append-symlink");
    let target = dir.path().join("real.status");
    write(&target, "working: original\n");
    let link = dir.path().join("task.status");
    std::os::unix::fs::symlink(&target, &link).expect("create symlink");

    let error = StatusLog::new(&link)
        .append("working: appended")
        .expect_err("a symlink is not appended through");

    assert!(matches!(error, McaoError::Uncertain(_)), "got {error:?}");
    assert_eq!(
        fs::read_to_string(&target).expect("read target"),
        "working: original\n"
    );
}

#[cfg(unix)]
#[test]
fn append_refuses_a_dangling_symlink_and_creates_no_target() {
    let dir = TempDir::new("status-append-dangling");
    let target = dir.path().join("absent.status");
    let link = dir.path().join("task.status");
    std::os::unix::fs::symlink(&target, &link).expect("create dangling symlink");

    let error = StatusLog::new(&link)
        .append("working: appended")
        .expect_err("a dangling symlink is not appended through");

    assert!(matches!(error, McaoError::Uncertain(_)), "got {error:?}");
    assert!(
        !target.exists(),
        "a refused append must not create the target"
    );
}
