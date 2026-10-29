//! Tests for atomic `state/<id>.meta` publication. Each test works in its own temporary state
//! directory, using real files and real threads.

mod support;

use std::fmt::Write;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::thread;

use mcao_core::McaoError;
use mcao_core::state::TaskId;
use mcao_core::state::TaskMeta;
use mcao_core::state::publish_meta;
use support::TempDir;

fn task_id(raw: &str) -> TaskId {
    TaskId::parse(raw).expect("test task id is valid")
}

fn record_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.meta"))
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).expect("record is readable")
}

/// Lists the file names in `dir`, sorted.
fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .expect("state dir is listable")
        .map(|entry| {
            entry
                .expect("directory entry is readable")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        // A record's sidecar lock (`<id>.meta.lock`) stays behind by design, so it is not a leftover.
        .filter(|name| !name.ends_with(".meta.lock"))
        .collect();
    names.sort();
    names
}

/// A record of about `lines` pairs tagged with `tag`. It is large enough that a torn write shows
/// up as a mismatch.
fn record(tag: &str, lines: usize) -> String {
    let mut text = format!("backend=tmux\nkind=ship\nharness={tag}\n");
    for n in 0..lines {
        writeln!(text, "pad_{n}={tag}-{n:08}").expect("writing to a String cannot fail");
    }
    text
}

#[test]
fn publish_writes_the_record_verbatim_and_leaves_no_temporary_file() {
    let dir = TempDir::new("publish-verbatim");
    let text = "backend=tmux\nkind=ship\nworktree=/w/one\n";

    publish_meta(dir.path(), &task_id("task-a"), text).expect("publish succeeds");

    let path = record_path(dir.path(), "task-a");
    assert_eq!(read(&path), text, "record holds the text verbatim");
    assert_eq!(entries(dir.path()), ["task-a.meta"]);
    let meta = TaskMeta::parse(task_id("task-a"), &read(&path));
    assert_eq!(meta.kind(), Some("ship"));
    assert_eq!(meta.worktree(), Some("/w/one"));
}

#[test]
fn publish_replaces_the_previous_record() {
    let dir = TempDir::new("publish-replace");
    let id = task_id("task-b");
    publish_meta(dir.path(), &id, "harness=claude\nkind=ship\n").expect("first publish succeeds");

    publish_meta(dir.path(), &id, "harness=codex\n").expect("second publish succeeds");

    let path = record_path(dir.path(), "task-b");
    assert_eq!(read(&path), "harness=codex\n");
    let meta = TaskMeta::parse(id, &read(&path));
    assert_eq!(meta.harness(), Some("codex"));
    assert_eq!(meta.kind(), None, "a key missing from the new text is gone");
    assert_eq!(entries(dir.path()), ["task-b.meta"]);
}

#[test]
fn orphan_temporary_file_does_not_touch_the_record() {
    let dir = TempDir::new("publish-orphan");
    let id = task_id("task-c");
    let old = "backend=tmux\nkind=ship\nharness=claude\n";
    publish_meta(dir.path(), &id, old).expect("seed publish succeeds");

    // A writer killed after creating its temporary file and before the rename leaves a partial
    // file behind. The record must still read as the previous complete record.
    fs::write(dir.path().join(".task-c.meta.4242.0.tmp"), "backend=tm")
        .expect("orphan temporary file is written");
    let path = record_path(dir.path(), "task-c");
    assert_eq!(
        read(&path),
        old,
        "partial temporary file does not touch the record"
    );
    assert_eq!(
        TaskMeta::parse(id.clone(), &read(&path)).harness(),
        Some("claude")
    );

    let new = "backend=tmux\nkind=ship\nharness=codex\n";
    publish_meta(dir.path(), &id, new).expect("publish succeeds beside the orphan");

    assert_eq!(read(&path), new);
    assert_eq!(
        entries(dir.path()),
        [".task-c.meta.4242.0.tmp", "task-c.meta"],
        "the orphan is left alone and no new temporary file remains"
    );
}

#[test]
fn failed_publish_removes_its_temporary_file() {
    let dir = TempDir::new("publish-failed");
    let id = task_id("task-d");
    // A directory where the record belongs makes the rename fail, which runs the cleanup path.
    fs::create_dir(record_path(dir.path(), "task-d")).expect("blocking directory is created");

    let result = publish_meta(dir.path(), &id, "kind=ship\n");

    assert!(
        matches!(result, Err(McaoError::Io(_))),
        "rename over a directory is refused: {result:?}"
    );
    assert_eq!(
        entries(dir.path()),
        ["task-d.meta"],
        "no temporary file remains after the failure"
    );
    assert!(record_path(dir.path(), "task-d").is_dir());
}

#[test]
fn concurrent_writers_leave_one_complete_record() {
    const WRITERS: usize = 10;
    let dir = TempDir::new("publish-concurrent");
    let id = task_id("task-e");
    let candidates: Vec<String> = (0..WRITERS)
        .map(|n| record(&format!("writer-{n}"), 400))
        .collect();

    thread::scope(|scope| {
        for text in &candidates {
            let state_dir = dir.path();
            let id = &id;
            scope.spawn(move || {
                publish_meta(state_dir, id, text).expect("publish succeeds under contention");
            });
        }
    });

    let final_text = read(&record_path(dir.path(), "task-e"));
    assert!(
        candidates.contains(&final_text),
        "final record is one writer's complete text, {} bytes",
        final_text.len()
    );
    assert_eq!(entries(dir.path()), ["task-e.meta"]);
}

#[test]
fn readers_never_observe_a_partial_record() {
    const ROUNDS: usize = 1000;
    let dir = TempDir::new("publish-reader");
    let id = task_id("task-f");
    let first = record("first", 2000);
    let second = record("second", 2000);
    publish_meta(dir.path(), &id, &first).expect("seed publish succeeds");
    let path = record_path(dir.path(), "task-f");
    let done = AtomicBool::new(false);

    let complete_reads = thread::scope(|scope| {
        let reader = scope.spawn(|| {
            let mut complete_reads = 0_usize;
            while !done.load(Ordering::Acquire) {
                let text = match fs::read_to_string(&path) {
                    Ok(text) => text,
                    // On Windows a read can lose a race with the rename and fail with an access
                    // or sharing error. A missing record is never expected on any platform.
                    Err(error)
                        if cfg!(windows) && matches!(error.raw_os_error(), Some(5 | 32 | 33)) =>
                    {
                        continue;
                    }
                    Err(error) => panic!("record is present during every replace: {error}"),
                };
                assert!(
                    text == first || text == second,
                    "reader saw a partial record of {} bytes",
                    text.len()
                );
                complete_reads += 1;
            }
            complete_reads
        });

        let outcome = (0..ROUNDS).try_for_each(|round| {
            let text = if round % 2 == 0 { &second } else { &first };
            publish_meta(dir.path(), &id, text)
        });
        // Stop the reader before reporting a failed publish, so the scope can finish.
        done.store(true, Ordering::Release);
        let reads = reader
            .join()
            .unwrap_or_else(|payload| std::panic::resume_unwind(payload));
        outcome.expect("publish succeeds while a reader runs");
        reads
    });

    assert!(
        complete_reads > 0,
        "reader saw at least one complete record"
    );
    assert_eq!(entries(dir.path()), ["task-f.meta"]);
}

/// `FILE_SHARE_READ`: other openers may read but not delete or replace the file.
#[cfg(windows)]
const FILE_SHARE_READ: u32 = 0x0000_0001;

#[cfg(windows)]
#[test]
fn publish_gives_up_after_bounded_retries_while_a_handle_stays_open() {
    use std::os::windows::fs::OpenOptionsExt;

    let dir = TempDir::new("publish-held");
    let id = task_id("task-g");
    publish_meta(dir.path(), &id, "kind=ship\n").expect("seed publish succeeds");
    // The share mode withholds delete sharing explicitly, so replacing the record is refused
    // while this handle is open.
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(record_path(dir.path(), "task-g"))
        .expect("record is opened");

    let result = publish_meta(dir.path(), &id, "kind=ship\nharness=codex\n");
    drop(held);

    assert!(
        matches!(result, Err(McaoError::Io(_))),
        "publish reports the conflict after its bounded retries: {result:?}"
    );
    assert_eq!(read(&record_path(dir.path(), "task-g")), "kind=ship\n");
    assert_eq!(entries(dir.path()), ["task-g.meta"]);
}
