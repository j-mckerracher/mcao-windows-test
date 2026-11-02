//! Behaviour tests for the migration of legacy `state/<id>.meta` records and its rollback. Each
//! test works in its own temporary state directory with real files and real threads.
//!
//! The legacy inputs are saved records under `tests/fixtures/meta/` and one hand-built record
//! under `tests/fixtures/migrate/`. The expected migrated bytes are frozen files beside them. See
//! `tests/fixtures/migrate/provenance.txt`. No expected value is generated from Rust.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::thread;

use mcao_core::McaoError;
use mcao_core::state::{
    MigrateOutcome, PathStyle, RollbackOutcome, TaskId, TaskMeta, migrate_dir, migrate_meta,
    rollback_dir, rollback_meta,
};
use support::TempDir;

/// Legacy inputs as `(task id, input path under tests/fixtures, expected migrated file)`.
const FIXTURES: &[(&str, &str, &str)] = &[
    ("DPES-901", "meta/DPES-901.meta", "migrate/DPES-901.v2.meta"),
    (
        "dpes-753-a-config-home-followups",
        "meta/dpes-753-a-config-home-followups.meta",
        "migrate/dpes-753-a-config-home-followups.v2.meta",
    ),
    (
        "dpes-754-f2-cli-skeleton",
        "meta/dpes-754-f2-cli-skeleton.meta",
        "migrate/dpes-754-f2-cli-skeleton.v2.meta",
    ),
    (
        "pr-linkback-gaps",
        "meta/pr-linkback-gaps.meta",
        "migrate/pr-linkback-gaps.v2.meta",
    ),
    (
        "crlf-bom-approval",
        "migrate/crlf-bom-approval.v1.meta",
        "migrate/crlf-bom-approval.v2.meta",
    ),
];

const POSIX: PathStyle = PathStyle::Posix;

fn fixture(relative: &str) -> Vec<u8> {
    fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(relative),
    )
    .expect("fixture is readable")
}

fn task_id(raw: &str) -> TaskId {
    TaskId::parse(raw).expect("test task id is valid")
}

fn record_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.meta"))
}

fn seed(dir: &Path, id: &str, bytes: &[u8]) {
    fs::write(record_path(dir, id), bytes).expect("seed record");
}

fn bytes(dir: &Path, id: &str) -> Vec<u8> {
    fs::read(record_path(dir, id)).expect("record is readable")
}

/// Lists the file names in `dir`, sorted. A record's sidecar lock (`<id>.meta.lock`) is left out:
/// every record writer takes it and nothing removes it, so it is not a file a migration wrote.
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
        .filter(|name| !name.ends_with(".meta.lock"))
        .collect();
    names.sort();
    names
}

/// Every pair of `text` except the encoding keys, in order.
fn content_pairs(id: &str, text: &str) -> Vec<(String, String)> {
    TaskMeta::parse(task_id(id), text)
        .pairs()
        .filter(|(key, _)| *key != "format" && *key != "path_style")
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

#[test]
fn legacy_records_migrate_to_the_frozen_bytes() {
    for &(id, input, expected) in FIXTURES {
        let dir = TempDir::new("migrate-frozen");
        seed(dir.path(), id, &fixture(input));

        let outcome = migrate_meta(dir.path(), &task_id(id), POSIX);

        assert!(
            matches!(outcome, Ok(MigrateOutcome::Migrated)),
            "{id}: {outcome:?}"
        );
        assert_eq!(
            bytes(dir.path(), id),
            fixture(expected),
            "{id}: migrated record"
        );
        assert_eq!(
            entries(dir.path()),
            [format!("{id}.meta")],
            "{id}: no leftovers"
        );
    }
}

#[test]
fn fixtures_keep_the_raw_bytes_the_expectations_depend_on() {
    let crlf = fixture("migrate/crlf-bom-approval.v1.meta");
    assert!(
        crlf.starts_with(b"\xef\xbb\xbfwindow="),
        "byte order mark is kept"
    );
    assert!(crlf.windows(2).any(|pair| pair == b"\r\n"), "CRLF is kept");
    let migrated = fixture("migrate/crlf-bom-approval.v2.meta");
    assert!(migrated.starts_with(b"format=2\npath_style=posix\nwindow="));
    assert!(!migrated.contains(&b'\r'), "the expectation is LF only");
}

#[test]
fn migration_keeps_approval_and_unknown_pairs_in_order_with_repeats() {
    let dir = TempDir::new("migrate-pairs");
    seed(
        dir.path(),
        "crlf-bom-approval",
        &fixture("migrate/crlf-bom-approval.v1.meta"),
    );

    migrate_meta(dir.path(), &task_id("crlf-bom-approval"), POSIX).expect("migrates");

    let text = String::from_utf8(bytes(dir.path(), "crlf-bom-approval")).expect("UTF-8");
    let meta = TaskMeta::parse(task_id("crlf-bom-approval"), &text);
    assert_eq!(
        meta.format_version().expect("known format"),
        mcao_core::state::FormatVersion::V2
    );
    assert_eq!(
        meta.path_style().expect("known style"),
        Some(PathStyle::Posix)
    );
    let kept: Vec<(&str, &str)> = meta
        .pairs()
        .filter(|(key, _)| {
            matches!(
                *key,
                "decision_keys" | "approval" | "proof_sha" | "future_key"
            )
        })
        .collect();
    assert_eq!(
        kept,
        [
            ("decision_keys", "d1"),
            ("approval", "granted at=1792101960 by=owner"),
            ("proof_sha", "9f2c1d0e4b7a"),
            ("decision_keys", "d2"),
            ("future_key", "a=b=c"),
        ]
    );
}

#[test]
fn migrating_twice_changes_nothing_the_second_time() {
    let dir = TempDir::new("migrate-twice");
    let id = "pr-linkback-gaps";
    seed(dir.path(), id, &fixture("meta/pr-linkback-gaps.meta"));
    migrate_meta(dir.path(), &task_id(id), POSIX).expect("first run migrates");
    let after_first = bytes(dir.path(), id);

    let second = migrate_meta(dir.path(), &task_id(id), POSIX);

    assert!(
        matches!(second, Ok(MigrateOutcome::AlreadyCurrent)),
        "{second:?}"
    );
    assert_eq!(bytes(dir.path(), id), after_first);
    assert_eq!(entries(dir.path()), [format!("{id}.meta")]);
}

#[test]
fn rollback_restores_the_original_bytes_of_an_lf_record() {
    for &(id, input, _) in FIXTURES
        .iter()
        .filter(|(id, _, _)| *id != "crlf-bom-approval")
    {
        let dir = TempDir::new("migrate-rollback");
        seed(dir.path(), id, &fixture(input));
        migrate_meta(dir.path(), &task_id(id), POSIX).expect("migrates");

        let outcome = rollback_meta(dir.path(), &task_id(id));

        assert!(
            matches!(outcome, Ok(RollbackOutcome::RolledBack)),
            "{id}: {outcome:?}"
        );
        assert_eq!(
            bytes(dir.path(), id),
            fixture(input),
            "{id}: original bytes"
        );
    }
}

#[test]
fn rollback_of_a_crlf_record_keeps_every_pair_and_drops_the_line_endings() {
    let dir = TempDir::new("migrate-rollback-crlf");
    let id = "crlf-bom-approval";
    let original = fixture("migrate/crlf-bom-approval.v1.meta");
    seed(dir.path(), id, &original);
    migrate_meta(dir.path(), &task_id(id), POSIX).expect("migrates");

    rollback_meta(dir.path(), &task_id(id)).expect("rolls back");

    let rolled = String::from_utf8(bytes(dir.path(), id)).expect("UTF-8");
    assert_eq!(
        rolled,
        String::from_utf8(fixture("migrate/crlf-bom-approval.v2.meta"))
            .expect("UTF-8")
            .strip_prefix("format=2\npath_style=posix\n")
            .expect("the expectation starts with the stamp"),
    );
    assert_eq!(
        content_pairs(id, &rolled),
        content_pairs(id, &String::from_utf8(original).expect("UTF-8")),
        "no pair is lost or changed"
    );
}

#[test]
fn migrate_then_rollback_then_migrate_converges_on_the_same_bytes() {
    let dir = TempDir::new("migrate-roundtrip");
    let id = "crlf-bom-approval";
    seed(
        dir.path(),
        id,
        &fixture("migrate/crlf-bom-approval.v1.meta"),
    );
    migrate_meta(dir.path(), &task_id(id), POSIX).expect("migrates");
    let first = bytes(dir.path(), id);

    rollback_meta(dir.path(), &task_id(id)).expect("rolls back");
    migrate_meta(dir.path(), &task_id(id), POSIX).expect("migrates again");

    assert_eq!(bytes(dir.path(), id), first);
}

#[test]
fn rolling_back_a_record_with_neither_key_changes_nothing() {
    let dir = TempDir::new("migrate-already-legacy");
    let id = "crlf-bom-approval";
    let original = fixture("migrate/crlf-bom-approval.v1.meta");
    seed(dir.path(), id, &original);

    let outcome = rollback_meta(dir.path(), &task_id(id));

    assert!(
        matches!(outcome, Ok(RollbackOutcome::AlreadyLegacy)),
        "{outcome:?}"
    );
    assert_eq!(
        bytes(dir.path(), id),
        original,
        "byte order mark and CRLF are untouched"
    );
}

#[test]
fn rollback_keeps_a_pair_written_after_the_migration() {
    let dir = TempDir::new("migrate-rollback-later-write");
    let id = "pr-linkback-gaps";
    let original = fixture("meta/pr-linkback-gaps.meta");
    seed(dir.path(), id, &original);
    migrate_meta(dir.path(), &task_id(id), POSIX).expect("migrates");
    let mut migrated = bytes(dir.path(), id);
    migrated.extend_from_slice(b"decision_keys=later\n");
    seed(dir.path(), id, &migrated);

    rollback_meta(dir.path(), &task_id(id)).expect("rolls back");

    let mut expected = original;
    expected.extend_from_slice(b"decision_keys=later\n");
    assert_eq!(bytes(dir.path(), id), expected);
}

#[test]
fn rollback_refuses_a_record_only_version_2_can_read() {
    for name in ["windows-v2", "windows-backend-posix-style"] {
        let dir = TempDir::new("migrate-rollback-refused");
        let id = "task-w";
        let record = match name {
            "windows-v2" => fixture("encoding/windows-v2.meta"),
            _ => b"format=2\npath_style=posix\nbackend=windows\nkind=ship\n".to_vec(),
        };
        seed(dir.path(), id, &record);

        let outcome = rollback_meta(dir.path(), &task_id(id));

        assert!(
            matches!(outcome, Err(McaoError::Refused(_))),
            "{name}: {outcome:?}"
        );
        assert_eq!(bytes(dir.path(), id), record, "{name}: record is untouched");
    }
}

#[test]
fn rollback_accepts_the_posix_v2_fixture() {
    let dir = TempDir::new("migrate-rollback-posix");
    let id = "task-p";
    let record = String::from_utf8(fixture("encoding/posix-v2.meta")).expect("UTF-8");
    seed(dir.path(), id, record.as_bytes());

    rollback_meta(dir.path(), &task_id(id)).expect("rolls back");

    let rolled = String::from_utf8(bytes(dir.path(), id)).expect("UTF-8");
    assert!(
        !rolled
            .lines()
            .any(|line| line.starts_with("format=") || line.starts_with("path_style="))
    );
    assert_eq!(content_pairs(id, &rolled), content_pairs(id, &record));
}

#[test]
fn a_refused_migration_leaves_the_original_bytes_and_no_other_file() {
    let cases: &[(&str, &[u8])] = &[
        ("newer format", b"format=3\nkind=ship\n"),
        ("other path style", b"path_style=windows\nkind=ship\n"),
        ("unknown path style", b"path_style=plan9\nkind=ship\n"),
        ("not UTF-8", b"kind=ship\nworktree=/tmp/\xff\xfe\n"),
        (
            "last value ends in a bare CR",
            b"kind=ship\nharness=claude\r",
        ),
    ];
    for &(name, record) in cases {
        let dir = TempDir::new("migrate-refused");
        seed(dir.path(), "task-r", record);

        let outcome = migrate_meta(dir.path(), &task_id("task-r"), POSIX);

        assert!(
            matches!(outcome, Err(McaoError::Refused(_))),
            "{name}: {outcome:?}"
        );
        assert_eq!(
            bytes(dir.path(), "task-r"),
            record,
            "{name}: original bytes"
        );
        assert_eq!(
            entries(dir.path()),
            ["task-r.meta"],
            "{name}: no other file"
        );
    }
}

#[test]
fn a_missing_record_and_a_directory_in_its_place_are_reported() {
    let dir = TempDir::new("migrate-not-a-record");
    let missing = migrate_meta(dir.path(), &task_id("task-m"), POSIX);
    assert!(matches!(missing, Err(McaoError::Io(_))), "{missing:?}");

    fs::create_dir(record_path(dir.path(), "task-d")).expect("create directory");
    let directory = migrate_meta(dir.path(), &task_id("task-d"), POSIX);
    assert!(
        matches!(directory, Err(McaoError::Refused(_))),
        "{directory:?}"
    );
    assert!(record_path(dir.path(), "task-d").is_dir());
}

#[cfg(unix)]
#[test]
fn a_symlinked_record_is_refused_and_its_target_is_untouched() {
    let dir = TempDir::new("migrate-symlink");
    let target = dir.path().join("elsewhere.txt");
    fs::write(&target, b"kind=ship\n").expect("write target");
    std::os::unix::fs::symlink(&target, record_path(dir.path(), "task-l")).expect("symlink");

    let outcome = migrate_meta(dir.path(), &task_id("task-l"), POSIX);

    assert!(matches!(outcome, Err(McaoError::Refused(_))), "{outcome:?}");
    assert_eq!(
        fs::read(&target).expect("target is readable"),
        b"kind=ship\n"
    );
    assert!(
        fs::symlink_metadata(record_path(dir.path(), "task-l"))
            .expect("link is still there")
            .file_type()
            .is_symlink()
    );
}

#[cfg(unix)]
#[test]
fn a_publish_that_cannot_create_its_file_leaves_the_original_readable() {
    use std::os::unix::fs::PermissionsExt;

    /// Restores write permission so the temporary directory can be removed.
    struct Restore(PathBuf);
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
        }
    }

    let dir = TempDir::new("migrate-read-only");
    let original = fixture("meta/DPES-901.meta");
    seed(dir.path(), "DPES-901", &original);
    let _restore = Restore(dir.path().to_owned());
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o555)).expect("chmod");
    if fs::write(dir.path().join("probe"), b"").is_ok() {
        return; // A superuser ignores the mode, so the failure cannot be produced.
    }

    let outcome = migrate_meta(dir.path(), &task_id("DPES-901"), POSIX);

    assert!(matches!(outcome, Err(McaoError::Io(_))), "{outcome:?}");
    assert_eq!(bytes(dir.path(), "DPES-901"), original);
}

#[test]
fn a_temporary_file_left_by_a_killed_writer_does_not_disturb_the_record_or_the_next_run() {
    let dir = TempDir::new("migrate-orphan");
    let id = "pr-linkback-gaps";
    let original = fixture("meta/pr-linkback-gaps.meta");
    seed(dir.path(), id, &original);
    let orphan = format!(".{id}.meta.424242.0.tmp");
    fs::write(dir.path().join(&orphan), b"format=2\npath_st").expect("write orphan");

    assert_eq!(
        bytes(dir.path(), id),
        original,
        "the record reads as before"
    );
    let outcome = migrate_meta(dir.path(), &task_id(id), POSIX);

    assert!(
        matches!(outcome, Ok(MigrateOutcome::Migrated)),
        "{outcome:?}"
    );
    assert_eq!(
        bytes(dir.path(), id),
        fixture("migrate/pr-linkback-gaps.v2.meta")
    );
    assert_eq!(entries(dir.path()), [orphan, format!("{id}.meta")]);
}

#[test]
fn migrating_a_directory_reports_each_record_on_its_own() {
    let dir = TempDir::new("migrate-dir");
    seed(dir.path(), "a-legacy", b"kind=ship\n");
    seed(
        dir.path(),
        "b-current",
        b"format=2\npath_style=posix\nkind=ship\n",
    );
    seed(dir.path(), "c-bad-utf8", b"kind=\xff\n");
    seed(dir.path(), "bad name", b"kind=ship\n");
    seed(dir.path(), ".hidden", b"kind=ship\n");
    fs::write(dir.path().join(".meta"), b"kind=ship\n").expect("write bare hidden entry");
    fs::write(dir.path().join("a-legacy.status"), b"working: x\n").expect("write status");
    fs::write(dir.path().join(".a-legacy.meta.1.0.tmp"), b"x").expect("write temp");

    let results = migrate_dir(dir.path(), POSIX).expect("directory is listable");

    let summary: Vec<(String, String)> = results
        .iter()
        .map(|(stem, result)| {
            let text = match result {
                Ok(outcome) => format!("{outcome:?}"),
                Err(McaoError::Refused(_)) => "refused".to_owned(),
                Err(other) => format!("unexpected {other}"),
            };
            (stem.clone(), text)
        })
        .collect();
    assert_eq!(
        summary,
        [
            ("a-legacy".to_owned(), "Migrated".to_owned()),
            ("b-current".to_owned(), "AlreadyCurrent".to_owned()),
            ("bad name".to_owned(), "refused".to_owned()),
            ("c-bad-utf8".to_owned(), "refused".to_owned()),
        ]
    );
    assert_eq!(
        bytes(dir.path(), "a-legacy"),
        b"format=2\npath_style=posix\nkind=ship\n"
    );
    assert_eq!(
        bytes(dir.path(), "c-bad-utf8"),
        b"kind=\xff\n",
        "a refused record is untouched"
    );
    assert_eq!(
        bytes(dir.path(), ".hidden"),
        b"kind=ship\n",
        "a hidden entry is not a record"
    );
}

#[test]
fn rolling_back_a_directory_reports_each_record_on_its_own() {
    let dir = TempDir::new("migrate-rollback-dir");
    seed(
        dir.path(),
        "a-v2",
        b"format=2\npath_style=posix\nkind=ship\n",
    );
    seed(dir.path(), "b-v1", b"kind=ship\n");
    seed(
        dir.path(),
        "c-windows",
        b"format=2\npath_style=windows\nkind=ship\n",
    );

    let results = rollback_dir(dir.path()).expect("directory is listable");

    let summary: Vec<(&str, &str)> = results
        .iter()
        .map(|(stem, result)| {
            let text = match result {
                Ok(RollbackOutcome::RolledBack) => "rolled back",
                Ok(RollbackOutcome::AlreadyLegacy) => "already legacy",
                Err(McaoError::Refused(_)) => "refused",
                Err(_) => "unexpected",
            };
            (stem.as_str(), text)
        })
        .collect();
    assert_eq!(
        summary,
        [
            ("a-v2", "rolled back"),
            ("b-v1", "already legacy"),
            ("c-windows", "refused")
        ]
    );
    assert_eq!(bytes(dir.path(), "a-v2"), b"kind=ship\n");
}

#[test]
fn concurrent_migrations_of_one_record_leave_one_valid_migrated_record() {
    let dir = TempDir::new("migrate-concurrent");
    let id = "dpes-754-f2-cli-skeleton";
    seed(
        dir.path(),
        id,
        &fixture("meta/dpes-754-f2-cli-skeleton.meta"),
    );
    let state_dir = dir.path().to_owned();

    let handles: Vec<_> = (0..8)
        .map(|_| {
            let state_dir = state_dir.clone();
            thread::spawn(move || migrate_meta(&state_dir, &task_id(id), POSIX))
        })
        .collect();
    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().expect("thread does not panic"))
        .collect();

    for result in &results {
        // A loser of the race sees the winner's record, or a record that changed under it. On
        // Windows a read that meets a rename can also report a sharing violation.
        let acceptable = match result {
            Ok(_) | Err(McaoError::StateConflict(_)) => true,
            Err(McaoError::Io(_)) => cfg!(windows),
            Err(_) => false,
        };
        assert!(acceptable, "unexpected result: {result:?}");
    }
    assert!(
        results
            .iter()
            .any(|result| matches!(result, Ok(MigrateOutcome::Migrated))),
        "one thread migrated the record: {results:?}"
    );
    assert_eq!(
        bytes(dir.path(), id),
        fixture("migrate/dpes-754-f2-cli-skeleton.v2.meta")
    );
    assert_eq!(
        entries(dir.path()),
        [format!("{id}.meta")],
        "no temporary file is left"
    );
}
