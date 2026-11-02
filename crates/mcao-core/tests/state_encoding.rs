//! Tests for the cross-platform `.meta` encoding: format version, path style, backend token, and
//! stamped publication. Real files and real fixtures, no mocks.
//!
//! The fixtures under `tests/fixtures/encoding/` are described in their `provenance.txt`. The
//! expected values below are literals from the DPES-889 rules, not computed by the code under
//! test.

mod support;

use std::fs;
use std::path::{Path, PathBuf};

use mcao_core::McaoError;
use mcao_core::state::{
    BackendToken, EncodingError, ExecutionTarget, FormatVersion, PathStyle, StatusKind, StatusLog,
    TaskId, TaskIdError, TaskMeta, publish_meta_encoded, stamp_encoding,
};
use support::TempDir;

/// The version 1 records saved from a live home, which carry none of the new keys.
const V1_FIXTURES: &[&str] = &[
    "DPES-901.meta",
    "dpes-753-a-config-home-followups.meta",
    "dpes-754-f2-cli-skeleton.meta",
    "pr-linkback-gaps.meta",
];

fn fixtures(sub: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(sub)
}

fn task_id(raw: &str) -> TaskId {
    TaskId::parse(raw).expect("test task id is valid")
}

/// The endpoint keys a record needs before the shell will clean up its task, naming task `t1`.
const ENDPOINT: &str = "window=session:fm-t1\nworktree=/w/t1\nproject=/p\n";

/// The endpoint keys of a tmux record for task `name`. Its window names the task's own `fm-` pane,
/// as the shell requires.
fn tmux_endpoint(name: &str) -> String {
    format!("window=session:fm-{name}\nworktree=/w/{name}\nproject=/p\n")
}

/// A complete Herdr record for task `name`: the backend, the binding to the task, the four
/// `herdr_*` keys, and the endpoint keys. Its tab and pane ids hold colons, as the shell's own
/// Herdr records do.
fn herdr_endpoint(name: &str) -> String {
    format!(
        "backend=herdr\nendpoint_task_id={name}\nherdr_session=lab\n\
         herdr_workspace_id=w1\nherdr_tab_id=w1:t2\nherdr_pane_id=w1:p2\n\
         window=lab:w1:p2\nworktree=/w/{name}\nproject=/p\n"
    )
}

fn parse(text: &str) -> TaskMeta {
    TaskMeta::parse(task_id("t1"), text)
}

fn load(sub: &str, name: &str) -> (String, TaskMeta) {
    let text = fs::read_to_string(fixtures(sub).join(name)).expect("fixture is readable");
    let meta = parse(&text);
    (text, meta)
}

#[test]
fn a_record_without_a_format_key_is_version_one() {
    for name in V1_FIXTURES {
        let (_, meta) = load("meta", name);
        assert_eq!(meta.format_version(), Ok(FormatVersion::V1), "{name}");
        assert_eq!(meta.path_style(), Ok(None), "{name}");
        assert_eq!(meta.backend_token(), Ok(BackendToken::Tmux), "{name}");
    }
}

#[test]
fn a_version_one_record_still_parses_to_the_same_pairs() {
    for name in V1_FIXTURES {
        let (text, meta) = load("meta", name);
        let expected: Vec<(&str, &str)> = text
            .lines()
            .map(|line| line.split_once('=').expect("fixture line is key=value"))
            .collect();
        assert_eq!(meta.pairs().collect::<Vec<_>>(), expected, "{name}");
        assert_eq!(
            meta.extra().count(),
            expected.len() - typed_count(&meta),
            "{name}"
        );
    }
}

fn typed_count(meta: &TaskMeta) -> usize {
    [
        "format",
        "path_style",
        "execution_target",
        "backend",
        "endpoint_task_id",
        "worktree",
        "harness",
        "kind",
        "base_branch",
    ]
    .iter()
    .filter(|key| meta.get(key).is_some())
    .count()
}

#[test]
fn a_windows_record_reads_on_any_host_with_every_path_unchanged() {
    let (_, meta) = load("encoding", "windows-v2.meta");
    assert_eq!(meta.format_version(), Ok(FormatVersion::V2));
    assert_eq!(meta.path_style(), Ok(Some(PathStyle::Windows)));
    assert_eq!(meta.backend_token(), Ok(BackendToken::Windows));
    assert_eq!(meta.get("home"), Some(r"C:\Users\REDACTED\mcao\home"));
    assert_eq!(meta.worktree(), Some(r"\\server\share\work\t1"));
    assert_eq!(meta.get("managed_worktree"), Some(r"\\?\C:\work\t1"));
    assert_eq!(meta.get("project"), Some(r"C:\work\proj"));
}

#[test]
fn a_posix_record_reads_on_any_host_with_every_path_unchanged() {
    let (_, meta) = load("encoding", "posix-v2.meta");
    assert_eq!(meta.format_version(), Ok(FormatVersion::V2));
    assert_eq!(meta.path_style(), Ok(Some(PathStyle::Posix)));
    assert_eq!(meta.backend_token(), Ok(BackendToken::Tmux));
    assert_eq!(meta.get("home"), Some("/home/REDACTED/mcao/home"));
    assert_eq!(meta.worktree(), Some("/home/REDACTED/wt/t1"));
}

#[test]
fn an_unknown_key_is_kept_and_does_not_stop_the_read() {
    let (_, meta) = load("encoding", "windows-v2.meta");
    assert_eq!(
        meta.get("future_key"),
        Some("read and ignored by this reader")
    );
    assert!(meta.extra().any(|(key, _)| key == "future_key"));
}

#[test]
fn an_unknown_backend_is_rejected_and_never_read_as_tmux() {
    for value in ["xyz", "gcp", "Tmux", "tmux "] {
        let meta = parse(&format!("backend={value}\n"));
        assert_eq!(
            meta.backend_token(),
            Err(EncodingError::UnknownBackend(value.to_owned())),
            "{value:?}"
        );
        assert_eq!(meta.backend(), Some(value), "the record stays readable");
    }
}

#[test]
fn an_empty_backend_reads_as_tmux_as_in_the_shell() {
    assert_eq!(parse("backend=\n").backend_token(), Ok(BackendToken::Tmux));
}

#[test]
fn an_unknown_format_or_path_style_is_an_error_not_a_guess() {
    assert_eq!(
        parse("format=3\n").format_version(),
        Err(EncodingError::UnsupportedFormat("3".to_owned()))
    );
    assert_eq!(
        parse("format=\n").format_version(),
        Err(EncodingError::UnsupportedFormat(String::new()))
    );
    assert_eq!(
        parse("path_style=amiga\n").path_style(),
        Err(EncodingError::UnknownPathStyle("amiga".to_owned()))
    );
    assert_eq!(parse("format=1\n").format_version(), Ok(FormatVersion::V1));
}

#[test]
fn a_crlf_record_reads_the_same_as_an_lf_record() {
    let (lf_text, lf) = load("encoding", "windows-v2.meta");
    let crlf = parse(&lf_text.replace('\n', "\r\n"));
    assert_eq!(
        crlf.pairs().collect::<Vec<_>>(),
        lf.pairs().collect::<Vec<_>>()
    );
    assert_eq!(crlf.backend_token(), Ok(BackendToken::Windows));
}

#[test]
fn stamping_adds_exactly_one_format_line_and_one_path_style_line() {
    let stamped = stamp_encoding(
        "kind=ship\nhome=C:\\Users\\u\\mcao\\home\nworktree=C:\\work\\t1\n",
        PathStyle::Windows,
    )
    .expect("stamp");
    assert_eq!(
        stamped,
        "format=2\npath_style=windows\nkind=ship\nhome=C:\\Users\\u\\mcao\\home\nworktree=C:\\work\\t1\n"
    );
}

#[test]
fn stamping_replaces_old_encoding_lines_and_keeps_every_other_byte_of_an_lf_record() {
    let old = "format=2\nkind=ship\npath_style=posix\nnote line without pair\nbackend=herdr";
    let stamped = stamp_encoding(old, PathStyle::Posix).expect("stamp");
    assert_eq!(
        stamped,
        "format=2\npath_style=posix\nkind=ship\nnote line without pair\nbackend=herdr"
    );
    assert_eq!(stamped.matches("format=").count(), 1);
    assert_eq!(stamped.matches("path_style=").count(), 1);
}

#[test]
fn stamping_rewrites_crlf_line_endings_as_lf() {
    let old = "format=2\r\nkind=ship\r\npath_style=posix\r\nhome=C:\\h\r\nbackend=herdr\r";
    let stamped = stamp_encoding(old, PathStyle::Posix).expect("stamp");
    assert_eq!(
        stamped,
        "format=2\npath_style=posix\nkind=ship\nhome=C:\\h\nbackend=herdr"
    );
    assert!(!stamped.contains('\r'));
}

#[test]
fn stamping_leaves_a_value_that_merely_contains_the_key_alone() {
    let stamped = stamp_encoding("note=format=1\nxformat=9\n", PathStyle::Posix).expect("stamp");
    assert_eq!(
        stamped,
        "format=2\npath_style=posix\nnote=format=1\nxformat=9\n"
    );
}

#[test]
fn publishing_stamped_writes_verbatim_paths_lf_only_and_no_bom() {
    let dir = TempDir::new("encoding-publish");
    let id = task_id("t1");
    let body = "kind=ship\nhome=C:\\Users\\u\\mcao\\home\nworktree=\\\\server\\share\\t1\n\
                project=C:\\work\\proj\nwindow=session:fm-t1\n";

    publish_meta_encoded(dir.path(), &id, body, PathStyle::Windows).expect("publish");

    let bytes = fs::read(dir.path().join("t1.meta")).expect("record is readable");
    assert!(
        !bytes.starts_with(&[0xEF, 0xBB, 0xBF]),
        "no byte order mark"
    );
    assert!(!bytes.contains(&b'\r'), "no carriage return");
    let text = String::from_utf8(bytes).expect("record is UTF-8");
    assert_eq!(
        text.lines().filter(|l| *l == "path_style=windows").count(),
        1
    );
    let meta = TaskMeta::parse(id, &text);
    assert_eq!(meta.get("home"), Some(r"C:\Users\u\mcao\home"));
    assert_eq!(meta.worktree(), Some(r"\\server\share\t1"));
    assert_eq!(meta.get("project"), Some(r"C:\work\proj"));
    assert_eq!(meta.format_version(), Ok(FormatVersion::V2));
}

#[test]
fn the_host_path_style_matches_the_target() {
    let expected = if cfg!(windows) {
        PathStyle::Windows
    } else {
        PathStyle::Posix
    };
    assert_eq!(PathStyle::host(), expected);
}

/// The shell reads a record with `grep '^key='` and with `fm_meta_get`'s `read -r` loop. Both must
/// return a version 2 value, backslashes included, untouched.
#[cfg(unix)]
#[test]
fn a_version_two_record_is_readable_by_the_shell_readers() {
    use std::process::Command;

    let path = fixtures("encoding").join("windows-v2.meta");
    let grep = Command::new("sh")
        .arg("-c")
        .arg(r#"grep '^home=' "$1" | cut -d= -f2-"#)
        .arg("sh")
        .arg(&path)
        .output()
        .expect("run grep");
    assert_eq!(
        String::from_utf8_lossy(&grep.stdout),
        "C:\\Users\\REDACTED\\mcao\\home\n"
    );

    let loop_script = r#"value=; while IFS= read -r line || [ -n "$line" ]; do
        case "$line" in "$2="*) value=${line#*=} ;; esac
      done < "$1"; printf '%s' "$value""#;
    for (key, expected) in [
        ("home", r"C:\Users\REDACTED\mcao\home"),
        ("worktree", r"\\server\share\work\t1"),
        ("backend", "windows"),
        ("format", "2"),
        ("path_style", "windows"),
        ("kind", "ship"),
    ] {
        let out = Command::new("sh")
            .arg("-c")
            .arg(loop_script)
            .arg("sh")
            .arg(&path)
            .arg(key)
            .output()
            .expect("run read loop");
        assert_eq!(String::from_utf8_lossy(&out.stdout), expected, "{key}");
    }
}

#[test]
fn state_new_task_id_accepts_the_creation_set() {
    for raw in [
        "abc-123",
        "a",
        "0",
        "dpes-889-state-encoding",
        "a_b",
        "-task",
    ] {
        assert_eq!(
            TaskId::parse_for_creation(raw).expect("accepted").as_str(),
            raw
        );
    }
}

#[test]
fn a_new_task_id_outside_that_set_is_refused_with_the_first_bad_character() {
    let cases = [
        ("", TaskIdError::Empty),
        (
            "f\u{f6}\u{f6}-123",
            outside_creation_set("f\u{f6}\u{f6}-123", '\u{f6}'),
        ),
        (
            "foo\u{2013}123",
            outside_creation_set("foo\u{2013}123", '\u{2013}'),
        ),
        ("a.b", outside_creation_set("a.b", '.')),
        (".a", outside_creation_set(".a", '.')),
        ("a b", outside_creation_set("a b", ' ')),
        ("Task-A", outside_creation_set("Task-A", 'T')),
    ];
    for (raw, expected) in cases {
        assert_eq!(TaskId::parse_for_creation(raw), Err(expected), "{raw:?}");
    }
}

#[test]
fn a_new_task_id_that_is_a_windows_device_name_is_refused() {
    for raw in ["con", "prn", "aux", "nul", "com1", "com9", "lpt1", "lpt9"] {
        assert_eq!(
            TaskId::parse_for_creation(raw),
            Err(TaskIdError::ReservedName {
                name: raw.to_owned()
            }),
            "{raw}"
        );
    }
}

#[test]
fn a_name_that_only_resembles_a_device_name_is_accepted() {
    for raw in [
        "com0",
        "com10",
        "lpt0",
        "lpt10",
        "lpt",
        "console",
        "nul-1",
        "conx",
        "auxiliary",
    ] {
        assert!(TaskId::parse_for_creation(raw).is_ok(), "{raw}");
    }
}

#[test]
fn a_device_name_with_an_extension_or_trailing_mark_is_refused_as_a_bad_character() {
    let cases = [("con.txt", '.'), ("con.", '.'), ("nul ", ' ')];
    for (raw, ch) in cases {
        assert_eq!(
            TaskId::parse_for_creation(raw),
            Err(outside_creation_set(raw, ch)),
            "{raw:?}"
        );
    }
    assert!(TaskId::parse_for_creation("task-abc").is_ok());
}

fn outside_creation_set(id: &str, ch: char) -> TaskIdError {
    TaskIdError::OutsideCreationSet {
        id: id.to_owned(),
        ch,
    }
}

/// Uppercase is refused at creation, and the same id still reads: records written before the
/// rule keep their uppercase form.
#[test]
fn a_new_task_id_with_uppercase_is_refused_and_a_legacy_one_still_reads() {
    for raw in ["Task-A", "task-A", "DPES-901", "AAAA", "COM1"] {
        let ch = raw
            .chars()
            .find(|ch| ch.is_ascii_uppercase())
            .expect("the id has an uppercase letter");
        assert_eq!(
            TaskId::parse_for_creation(raw),
            Err(outside_creation_set(raw, ch)),
            "{raw}"
        );
        assert_eq!(TaskId::parse(raw).expect("reads").as_str(), raw);
    }
}

const TASK_ID_MATRIX: &str = include_str!("../../../tests/fixtures/task-id/creation-matrix.txt");

fn fixture_verdict(word: &str) -> bool {
    match word {
        "accept" => true,
        "reject" => false,
        other => panic!("unknown fixture verdict {other:?}"),
    }
}

#[test]
fn state_task_id_matrix_matches_the_shell_fixture() {
    let mut rows = 0;
    for row in TASK_ID_MATRIX
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        let mut fields = row.splitn(3, ' ');
        let creation = fixture_verdict(fields.next().expect("creation verdict"));
        let reader = fixture_verdict(fields.next().expect("reader verdict"));
        let raw = match fields.next().expect("id field") {
            "<empty>" => "",
            id => id,
        };
        assert_eq!(
            TaskId::parse_for_creation(raw).is_ok(),
            creation,
            "creation of {raw:?}"
        );
        assert_eq!(TaskId::parse(raw).is_ok(), reader, "reader of {raw:?}");
        rows += 1;
    }
    assert!(rows > 0, "the shared fixture has no rows");
}

#[test]
fn state_task_id_length_boundary_is_64_bytes_for_creation() {
    let at_cap = "a".repeat(64);
    let over_cap = "a".repeat(65);
    assert!(TaskId::parse_for_creation(&at_cap).is_ok());
    assert_eq!(
        TaskId::parse_for_creation(&over_cap),
        Err(TaskIdError::TooLong { len: 65 })
    );
    assert!(TaskId::parse(&over_cap).is_ok(), "the reader has no cap");
}

#[test]
fn state_case_fold_collision_refuses_an_existing_id_in_any_case() {
    let existing = ["Task-A", "dpes-970"];
    let lower = TaskId::parse_for_creation("task-a").expect("creation accepts the lowercase id");
    assert_eq!(
        lower.check_case_fold_free(existing),
        Err(TaskIdError::CaseFoldCollision {
            candidate: "task-a".to_owned(),
            existing: "Task-A".to_owned(),
        })
    );
    let exact = TaskId::parse_for_creation("dpes-970").expect("creation accepts it");
    assert!(
        exact.check_case_fold_free(existing).is_err(),
        "an exact duplicate is the same file"
    );
    let fresh = TaskId::parse_for_creation("task-b").expect("creation accepts it");
    assert!(fresh.check_case_fold_free(existing).is_ok());
    assert!(fresh.check_case_fold_free(Vec::<&str>::new()).is_ok());
}

#[test]
fn the_reader_still_accepts_ids_the_creation_rule_refuses() {
    for raw in ["DPES-901", "a_b", "x.y"] {
        assert!(TaskId::parse(raw).is_ok(), "{raw}");
    }
}

/// The status log is read and appended by the same bytes on every platform: a CRLF log folds as
/// its LF twin, and an append writes `\n` and never `\r\n`.
#[test]
fn a_crlf_status_log_folds_like_its_lf_twin() {
    let dir = TempDir::new("encoding-status");
    let lf = StatusLog::new(dir.path().join("lf.status"));
    let crlf_path = dir.path().join("crlf.status");
    let lines = [
        "needs-decision [at=1] [key=a]: first",
        "needs-decision [at=2] [key=b]: second",
        "resolved [at=3] [key=a]: answered",
    ];
    for line in lines {
        lf.append(line).expect("append LF line");
    }
    support::write(&crlf_path, lines.join("\r\n") + "\r\n");
    let crlf = StatusLog::new(crlf_path);

    let from_lf = lf.open_decisions(StatusKind::Ship).expect("fold LF");
    let from_crlf = crlf.open_decisions(StatusKind::Ship).expect("fold CRLF");

    assert_eq!(from_lf.len(), 1);
    assert_eq!(from_crlf, from_lf);
    let appended = fs::read(dir.path().join("lf.status")).expect("read appended log");
    assert!(!appended.contains(&b'\r'), "an append writes LF only");
}

#[test]
fn publishing_a_crlf_record_writes_lf_only() {
    let dir = TempDir::new("encoding-publish-crlf");
    let id = task_id("t1");

    publish_meta_encoded(
        dir.path(),
        &id,
        "kind=ship\r\nworktree=C:\\work\\t1\r\nproject=/p\r\nwindow=session:fm-t1\r\n",
        PathStyle::Windows,
    )
    .expect("publish");

    let bytes = fs::read(dir.path().join("t1.meta")).expect("record is readable");
    assert!(!bytes.contains(&b'\r'), "no carriage return is written");
    let meta = TaskMeta::parse(id, &String::from_utf8(bytes).expect("record is UTF-8"));
    assert_eq!(meta.worktree(), Some(r"C:\work\t1"));
}

#[test]
fn publishing_an_unknown_backend_is_refused_and_writes_nothing() {
    let dir = TempDir::new("encoding-publish-backend");
    let id = task_id("t1");

    let text = format!("kind=ship\nbackend=gcp\n{ENDPOINT}");
    let result = publish_meta_encoded(dir.path(), &id, &text, PathStyle::Posix);

    let Err(McaoError::Refused(reason)) = &result else {
        panic!("unknown backend is refused: {result:?}");
    };
    assert!(reason.contains("unknown backend \"gcp\""), "{reason}");
    let names: Vec<_> = fs::read_dir(dir.path())
        .expect("dir is listable")
        .map(|entry| entry.expect("entry is readable").file_name())
        .collect();
    assert!(names.is_empty(), "nothing is written: {names:?}");
}

#[test]
fn publishing_keeps_a_known_or_absent_backend() {
    let dir = TempDir::new("encoding-publish-known-backend");
    for (name, record) in [
        ("a", format!("kind=ship\n{}", herdr_endpoint("a"))),
        (
            "b",
            format!("kind=ship\nbackend=windows\n{}", tmux_endpoint("b")),
        ),
        (
            "c",
            format!("kind=ship\nbackend=tmux\n{}", tmux_endpoint("c")),
        ),
        ("d", format!("kind=ship\n{}", tmux_endpoint("d"))),
        ("e", format!("kind=ship\nformat=1\n{}", tmux_endpoint("e"))),
        ("f", format!("kind=ship\nformat=2\n{}", tmux_endpoint("f"))),
        (
            "g",
            format!("kind=ship\nendpoint_task_id=g\n{}", tmux_endpoint("g")),
        ),
        (
            "h",
            format!("execution_target=local\nkind=ship\n{}", tmux_endpoint("h")),
        ),
    ] {
        publish_meta_encoded(dir.path(), &task_id(name), &record, PathStyle::Posix)
            .unwrap_or_else(|error| panic!("{name}: {error}"));
    }
}

fn assert_refused_and_nothing_written(text: &str) {
    let dir = TempDir::new("encoding-publish-refused");
    let result = publish_meta_encoded(dir.path(), &task_id("t1"), text, PathStyle::Posix);
    assert!(
        matches!(result, Err(McaoError::Refused(_))),
        "{text:?} is refused: {result:?}"
    );
    let names: Vec<_> = fs::read_dir(dir.path())
        .expect("dir is listable")
        .map(|entry| entry.expect("entry is readable").file_name())
        .collect();
    assert!(names.is_empty(), "{text:?} writes nothing: {names:?}");
}

/// The shell refuses to clean up a task whose `backend` is empty or repeated, so the writer must
/// not publish one.
#[test]
fn publishing_an_empty_or_repeated_backend_is_refused() {
    for text in [
        "kind=ship\nbackend=\n",
        "backend=gcp\nbackend=tmux\n",
        "backend=herdr\nbackend=tmux\n",
        "backend=tmux\nbackend=tmux\n",
        "backend=tmux\nbackend=\n",
    ] {
        assert_refused_and_nothing_written(text);
    }
}

#[test]
fn publishing_a_format_other_than_one_or_two_is_refused() {
    for text in [
        "format=3\nkind=ship\n",
        "format=\nkind=ship\n",
        "format=02\n",
    ] {
        assert_refused_and_nothing_written(text);
    }
}

#[test]
fn stamping_removes_every_trailing_cr_and_a_leading_bom() {
    assert_eq!(
        stamp_encoding("kind=ship\r\r\nx=1\r\r", PathStyle::Posix).expect("stamp"),
        "format=2\npath_style=posix\nkind=ship\nx=1"
    );
    assert_eq!(
        stamp_encoding("\u{feff}format=1\nkind=ship\n", PathStyle::Posix).expect("stamp"),
        "format=2\npath_style=posix\nkind=ship\n"
    );
}

#[test]
fn publishing_a_bom_record_checks_its_first_key() {
    assert_refused_and_nothing_written("\u{feff}backend=gcp\nkind=ship\n");
    assert_refused_and_nothing_written("\u{feff}format=3\nkind=ship\n");
}

/// A cloud record has no terminal backend, so its backend token is refused whatever its `backend`
/// key holds. It never reads as the `tmux` default.
#[test]
fn a_record_with_a_cloud_target_has_no_terminal_backend() {
    let refused: Result<BackendToken, EncodingError> = Err(EncodingError::NoTerminalBackend {
        target: ExecutionTarget::Gcp,
    });
    assert_eq!(
        parse("format=2\nexecution_target=gcp\ncloud_run_id=r1\n").backend_token(),
        refused
    );
    assert_eq!(
        parse("execution_target=gcp\nbackend=tmux\n").backend_token(),
        refused
    );
}

#[test]
fn execution_target_is_local_when_absent_and_refuses_any_other_value() {
    assert_eq!(
        parse("kind=ship\n").execution_target(),
        Ok(ExecutionTarget::Local)
    );
    assert_eq!(
        parse("execution_target=local\n").execution_target(),
        Ok(ExecutionTarget::Local)
    );
    assert_eq!(
        parse("execution_target=gcp\n").execution_target(),
        Ok(ExecutionTarget::Gcp)
    );
    assert_eq!(
        parse("execution_target=\n").execution_target(),
        Err(EncodingError::UnknownExecutionTarget(String::new()))
    );
    assert_eq!(
        parse("execution_target=cloud\n").execution_target(),
        Err(EncodingError::UnknownExecutionTarget("cloud".to_owned()))
    );
    assert_eq!(
        parse("execution_target=local\nexecution_target=local\n").execution_target(),
        Err(EncodingError::AmbiguousExecutionTarget)
    );
}

#[test]
fn a_local_record_reads_its_backend_token() {
    assert_eq!(
        parse("execution_target=local\nbackend=herdr\n").backend_token(),
        Ok(BackendToken::Herdr)
    );
    assert_eq!(
        parse("execution_target=local\n").backend_token(),
        Ok(BackendToken::Tmux)
    );
}

#[test]
fn stamping_refuses_a_path_style_that_names_another_system() {
    assert_eq!(
        stamp_encoding("kind=ship\npath_style=windows\n", PathStyle::Posix),
        Err(EncodingError::PathStyleConflict {
            existing: PathStyle::Windows,
            requested: PathStyle::Posix,
        })
    );
}

#[test]
fn stamping_refuses_an_unknown_path_style_rather_than_relabelling_it() {
    assert_eq!(
        stamp_encoding("path_style=amiga\n", PathStyle::Posix),
        Err(EncodingError::UnknownPathStyle("amiga".to_owned()))
    );
}

#[test]
fn publishing_keeps_a_matching_path_style_and_refuses_a_conflicting_one() {
    let dir = TempDir::new("encoding-publish-path-style");
    let id = task_id("t1");
    let record = dir.path().join("t1.meta");
    publish_meta_encoded(
        dir.path(),
        &id,
        &format!("path_style=posix\nkind=ship\n{ENDPOINT}"),
        PathStyle::Posix,
    )
    .expect("a matching path style publishes");
    let kept = fs::read_to_string(&record).expect("record is readable");
    assert_eq!(
        kept.lines()
            .filter(|line| line.starts_with("path_style="))
            .collect::<Vec<_>>(),
        ["path_style=posix"]
    );

    let result = publish_meta_encoded(
        dir.path(),
        &id,
        &format!("path_style=windows\nkind=ship\n{ENDPOINT}"),
        PathStyle::Posix,
    );
    let Err(McaoError::Refused(reason)) = &result else {
        panic!("a conflicting path style is refused: {result:?}");
    };
    assert!(reason.contains("conflicts with posix"), "{reason}");
    assert_eq!(
        fs::read_to_string(&record).expect("record is readable"),
        kept,
        "the previous record is kept"
    );
}

#[test]
fn publishing_a_bom_record_checks_and_writes_its_first_key_without_a_bom() {
    let dir = TempDir::new("encoding-publish-bom");
    let id = task_id("t1");
    publish_meta_encoded(
        dir.path(),
        &id,
        &format!("\u{feff}{}", herdr_endpoint("t1")),
        PathStyle::Posix,
    )
    .expect("publish");

    let bytes = fs::read(dir.path().join("t1.meta")).expect("record is readable");
    assert!(bytes.starts_with(b"format=2\npath_style=posix\nbackend=herdr\n"));
    let meta = TaskMeta::parse(id, &String::from_utf8(bytes).expect("record is UTF-8"));
    assert_eq!(meta.backend_token(), Ok(BackendToken::Herdr));
}

/// The shell refuses to clean up a task whose endpoint keys are missing, repeated, empty, or
/// malformed, or whose endpoint task binding is empty or names another task, so the writer must
/// not publish one.
#[test]
fn publishing_an_incomplete_or_foreign_endpoint_is_refused() {
    for text in [
        "kind=ship\nworktree=/w\nproject=/p\n",
        "kind=ship\nwindow=session:fm-t1\nproject=/p\n",
        "kind=ship\nwindow=session:fm-t1\nworktree=/w\n",
        "kind=ship\nwindow=\nworktree=/w\nproject=/p\n",
        "kind=ship\nwindow=session:fm-t1\nworktree=\nproject=/p\n",
        "kind=ship\nwindow=session:fm-t1\nworktree=/w\nproject=\n",
        "kind=ship\nwindow=session:fm-t1\nwindow=session:fm-t2\nworktree=/w\nproject=/p\n",
        "kind=ship\nwindow=session:fm\tt1\nworktree=/w\nproject=/p\n",
        "kind=ship\nwindow=session:fm-t1\nworktree=/w\nproject=/p\nendpoint_task_id=\n",
        "kind=ship\nwindow=session:fm-t1\nworktree=/w\nproject=/p\nendpoint_task_id=t2\n",
        "kind=ship\nwindow=session:fm-t1\nworktree=/w\nproject=/p\nendpoint_task_id=t1\nendpoint_task_id=t1\n",
    ] {
        assert_refused_and_nothing_written(text);
    }
}

#[test]
fn publishing_a_cloud_record_is_refused_and_writes_nothing() {
    let dir = TempDir::new("encoding-publish-cloud");
    let result = publish_meta_encoded(
        dir.path(),
        &task_id("t1"),
        &format!("execution_target=gcp\ncloud_run_id=r1\n{ENDPOINT}"),
        PathStyle::Posix,
    );
    let Err(McaoError::Refused(reason)) = &result else {
        panic!("a cloud record is refused: {result:?}");
    };
    assert!(reason.contains("has no terminal backend"), "{reason}");
    assert_eq!(
        fs::read_dir(dir.path()).expect("dir is listable").count(),
        0,
        "nothing is written"
    );
}

/// The shell refuses a tmux window that does not split into a session and an `fm-<task id>` pane,
/// so the writer must not publish one.
#[test]
fn publishing_a_tmux_window_not_naming_its_task_is_refused() {
    for window in [
        "ambient-current-window",
        ":fm-t1",
        "session:t1",
        "session:fm-t2",
        "session:fm-t1:extra",
    ] {
        assert_refused_and_nothing_written(&format!(
            "kind=ship\nwindow={window}\nworktree=/w/t1\nproject=/p\n"
        ));
    }
}

/// A tmux window is kept under any session name the shell accepts, spaces included.
#[test]
fn publishing_a_tmux_window_naming_its_task_is_kept() {
    let dir = TempDir::new("encoding-publish-tmux");
    for (name, session) in [("t1", "firstmate"), ("t2", "team work")] {
        let record =
            format!("kind=ship\nwindow={session}:fm-{name}\nworktree=/w/{name}\nproject=/p\n");
        publish_meta_encoded(dir.path(), &task_id(name), &record, PathStyle::Posix)
            .unwrap_or_else(|error| panic!("{name}: {error}"));
    }
}

/// The shell refuses a Herdr record that does not bind to its task, lacks or repeats a `herdr_*`
/// key, names a window other than its session and pane, or holds a session or workspace that is
/// not a shell atom, so the writer must not publish one.
#[test]
fn publishing_an_inconsistent_herdr_endpoint_is_refused() {
    let complete = herdr_endpoint("t1");
    for text in [
        complete.replace("endpoint_task_id=t1\n", ""),
        complete.replace("endpoint_task_id=t1", "endpoint_task_id=t2"),
        complete.replace("endpoint_task_id=t1", "endpoint_task_id="),
        complete.replace("herdr_session=lab\n", ""),
        complete.replace("herdr_tab_id=w1:t2", "herdr_tab_id="),
        complete.replace(
            "herdr_pane_id=w1:p2\n",
            "herdr_pane_id=w1:p2\nherdr_pane_id=w1:p2\n",
        ),
        complete.replace("window=lab:w1:p2", "window=lab:w1:p9"),
        complete.replace("window=lab:w1:p2", "window=other:w1:p2"),
        complete
            .replace("herdr_session=lab", "herdr_session=la:b")
            .replace("window=lab:w1:p2", "window=la:b:w1:p2"),
        complete.replace("herdr_workspace_id=w1", "herdr_workspace_id=w 1"),
    ] {
        assert_refused_and_nothing_written(&text);
    }
}

/// One record from the shell's endpoint tests in `tests/fm-teardown-endpoint-safety.test.sh`, with
/// the verdict that suite expects of it.
#[cfg(unix)]
struct EndpointCase {
    name: &'static str,
    task: &'static str,
    text: &'static str,
    accepted: bool,
}

/// Runs the shell's own endpoint validation on the record at `meta` for task `id`, and reports
/// whether the shell accepts the record.
#[cfg(unix)]
fn shell_accepts(meta: &Path, id: &str) -> bool {
    use std::process::{Command, Stdio};

    let backend = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bin/fm-backend.sh");
    Command::new("bash")
        .arg("-c")
        .arg(r#". "$1" && fm_backend_validate_task_endpoint "$2" "$3""#)
        .arg("bash")
        .arg(&backend)
        .arg(meta)
        .arg(id)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("bash runs")
        .success()
}

/// The publisher and the shell must agree on each endpoint case from the shell's suite. The shell
/// is run on the raw record, and the publisher on the same text. A record the publisher accepts
/// must also pass the shell once stamped, because that stamped copy is what the shell reads.
#[cfg(unix)]
#[test]
fn the_publisher_agrees_with_the_shell_on_the_endpoint_cases() {
    let cases = [
        EndpointCase {
            name: "tmux endpoint",
            task: "tmux-task",
            text: "window=firstmate:fm-tmux-task\nworktree=/w\nproject=/p\n",
            accepted: true,
        },
        EndpointCase {
            name: "tmux endpoint with a spaced session",
            task: "tmux-spaced-session",
            text: "window=team work:fm-tmux-spaced-session\nworktree=/w\nproject=/p\n",
            accepted: true,
        },
        EndpointCase {
            name: "herdr endpoint",
            task: "herdr-task",
            text: concat!(
                "backend=herdr\n",
                "window=lab:w1:p2\n",
                "endpoint_task_id=herdr-task\n",
                "worktree=/w\n",
                "project=/p\n",
                "herdr_session=lab\n",
                "herdr_workspace_id=w1\n",
                "herdr_tab_id=w1:t2\n",
                "herdr_pane_id=w1:p2\n",
            ),
            accepted: true,
        },
        EndpointCase {
            name: "missing endpoint",
            task: "endpoint-a",
            text: "worktree=/w\nproject=/p\nkind=scout\n",
            accepted: false,
        },
        EndpointCase {
            name: "empty endpoint",
            task: "endpoint-a",
            text: "window=\nworktree=/w\nproject=/p\n",
            accepted: false,
        },
        EndpointCase {
            name: "malformed endpoint",
            task: "endpoint-a",
            text: "window=ambient-current-window\nworktree=/w\nproject=/p\n",
            accepted: false,
        },
        EndpointCase {
            name: "task-mismatched endpoint",
            task: "endpoint-a",
            text: concat!(
                "window=isolated:fm-other-task\n",
                "endpoint_task_id=other-task\n",
                "worktree=/w\n",
                "project=/p\n",
            ),
            accepted: false,
        },
        EndpointCase {
            name: "empty task binding",
            task: "endpoint-a",
            text: concat!(
                "window=isolated:fm-endpoint-a\n",
                "endpoint_task_id=\n",
                "worktree=/w\n",
                "project=/p\n",
            ),
            accepted: false,
        },
        EndpointCase {
            name: "duplicate task binding",
            task: "endpoint-a",
            text: concat!(
                "window=isolated:fm-endpoint-a\n",
                "endpoint_task_id=endpoint-a\n",
                "endpoint_task_id=endpoint-a\n",
                "worktree=/w\n",
                "project=/p\n",
            ),
            accepted: false,
        },
        EndpointCase {
            name: "removed zellij backend",
            task: "zellij-task",
            text: concat!(
                "window=lab:7\n",
                "endpoint_task_id=zellij-task\n",
                "worktree=/w\n",
                "project=/p\n",
                "backend=zellij\n",
                "zellij_session=lab\n",
                "zellij_tab_id=3\n",
                "zellij_pane_id=7\n",
            ),
            accepted: false,
        },
        EndpointCase {
            name: "removed orca backend",
            task: "orca-task",
            text: concat!(
                "window=fm-orca-task\n",
                "endpoint_task_id=orca-task\n",
                "terminal=term-7\n",
                "worktree=/w\n",
                "project=/p\n",
                "backend=orca\n",
                "orca_worktree_id=worktree-9::/orca/worktree-9\n",
            ),
            accepted: false,
        },
        EndpointCase {
            name: "removed cmux backend",
            task: "cmux-task",
            text: concat!(
                "window=workspace-1:surface-2\n",
                "endpoint_task_id=cmux-task\n",
                "worktree=/w\n",
                "project=/p\n",
                "backend=cmux\n",
                "cmux_workspace_id=workspace-1\n",
                "cmux_surface_id=surface-2\n",
            ),
            accepted: false,
        },
    ];

    for case in cases {
        let shell_dir = TempDir::new(&format!("encoding-shell-{}", case.task));
        let raw = shell_dir.path().join(format!("{}.meta", case.task));
        fs::write(&raw, case.text).expect("record is written");
        assert_eq!(
            shell_accepts(&raw, case.task),
            case.accepted,
            "the shell verdict on {}",
            case.name
        );

        let publish_dir = TempDir::new(&format!("encoding-publish-{}", case.task));
        let result = publish_meta_encoded(
            publish_dir.path(),
            &task_id(case.task),
            case.text,
            PathStyle::Posix,
        );
        assert_eq!(
            result.is_ok(),
            case.accepted,
            "the publisher verdict on {}: {result:?}",
            case.name
        );
        if case.accepted {
            let published = publish_dir.path().join(format!("{}.meta", case.task));
            assert!(
                shell_accepts(&published, case.task),
                "the shell accepts the stamped record for {}",
                case.name
            );
        }
    }
}

#[test]
fn a_status_line_ending_in_cr_is_appended_without_it() {
    let dir = TempDir::new("encoding-status-cr");
    let log = StatusLog::new(dir.path().join("t.status"));

    log.append("working [at=1]: one\r").expect("append");
    log.append("working [at=2]: two\r\r").expect("append");

    let bytes = fs::read(dir.path().join("t.status")).expect("read log");
    assert_eq!(bytes, b"working [at=1]: one\nworking [at=2]: two\n");
}
