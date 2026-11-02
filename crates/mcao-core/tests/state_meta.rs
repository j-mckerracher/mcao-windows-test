//! Read-side tests for `state::TaskId` and `state::TaskMeta`, run against saved real records.
//!
//! The fixtures under `tests/fixtures/meta/` are real `state/<id>.meta` records with the operator's
//! username redacted from absolute paths. See `tests/fixtures/meta/provenance.txt`. No expected
//! value is generated from Rust.

use std::fs;
use std::path::{Path, PathBuf};

use mcao_core::state::{TaskId, TaskIdError, TaskMeta};

/// Real records, named as the state store names them: `<id>.meta`.
const FIXTURES: &[&str] = &[
    "DPES-901.meta",
    "dpes-753-a-config-home-followups.meta",
    "dpes-754-f2-cli-skeleton.meta",
    "pr-linkback-gaps.meta",
];

/// Keys with a typed accessor on [`TaskMeta`].
const TYPED_KEYS: &[&str] = &[
    "format",
    "path_style",
    "execution_target",
    "backend",
    "endpoint_task_id",
    "worktree",
    "harness",
    "kind",
    "base_branch",
];

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/meta")
}

fn task_id(raw: &str) -> TaskId {
    TaskId::parse(raw).expect("test task id is valid")
}

/// Parses a saved record, naming its task from the file stem as the state store does.
fn load(name: &str) -> TaskMeta {
    let text = fs::read_to_string(fixture_dir().join(name)).expect("fixture is readable");
    let stem = Path::new(name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .expect("fixture name has a UTF-8 stem");
    TaskMeta::parse(task_id(stem), &text)
}

#[test]
fn task_id_accepts_the_shell_contract_charset() {
    for raw in ["dpes-753-b-taskid-meta", "DPES-901", "a", "x.y_z-1", "0"] {
        assert_eq!(task_id(raw).as_str(), raw);
    }
}

#[test]
fn task_id_refuses_what_the_shell_refuses() {
    let cases = [
        ("", TaskIdError::Empty),
        (".", TaskIdError::LeadingDot),
        ("..", TaskIdError::LeadingDot),
        (".hidden", TaskIdError::LeadingDot),
        ("a/b", TaskIdError::InvalidChar { ch: '/' }),
        ("a\\b", TaskIdError::InvalidChar { ch: '\\' }),
        ("a b", TaskIdError::InvalidChar { ch: ' ' }),
        ("a=b", TaskIdError::InvalidChar { ch: '=' }),
        ("a\nb", TaskIdError::InvalidChar { ch: '\n' }),
        ("caf\u{e9}", TaskIdError::InvalidChar { ch: '\u{e9}' }),
    ];
    for (raw, expected) in cases {
        assert_eq!(TaskId::parse(raw), Err(expected), "input {raw:?}");
    }
}

#[test]
fn every_saved_record_keeps_every_pair_in_file_order() {
    for name in FIXTURES {
        let text = fs::read_to_string(fixture_dir().join(name)).expect("fixture is readable");
        let meta = load(name);

        let expected: Vec<(&str, &str)> = text
            .lines()
            .map(|line| line.split_once('=').expect("fixture line is key=value"))
            .collect();
        let actual: Vec<(&str, &str)> = meta.pairs().collect();
        assert_eq!(actual, expected, "{name}: pairs match the file in order");

        let typed_present = TYPED_KEYS
            .iter()
            .filter(|key| meta.get(key).is_some())
            .count();
        assert_eq!(
            typed_present + meta.extra().count(),
            expected.len(),
            "{name}: every line is either typed or kept in extra"
        );
    }
}

#[test]
fn saved_records_take_their_identity_from_the_file_name() {
    for name in FIXTURES {
        let stem = Path::new(name).file_stem().and_then(|s| s.to_str());
        assert_eq!(Some(load(name).id().as_str()), stem, "{name}");
    }
}

#[test]
fn absent_backend_reads_as_none_not_a_default() {
    for name in FIXTURES {
        assert_eq!(load(name).backend(), None, "{name} carries no backend key");
    }
}

#[test]
fn saved_record_reads_typed_fields_under_shell_names() {
    let meta = load("dpes-753-a-config-home-followups.meta");
    assert_eq!(
        meta.endpoint_task_id(),
        Some("dpes-753-a-config-home-followups")
    );
    assert_eq!(meta.harness(), Some("claude"));
    assert_eq!(meta.kind(), Some("ship"));
    assert_eq!(meta.base_branch(), Some("DPES-751"));
    assert!(meta.worktree().is_some());
}

#[test]
fn record_missing_optional_keys_reads_them_as_none() {
    let meta = load("DPES-901.meta");
    assert_eq!(meta.endpoint_task_id(), None);
    assert_eq!(meta.base_branch(), None);
    assert_eq!(meta.kind(), Some("ship"));
}

#[test]
fn unknown_keys_survive_verbatim_including_equals_in_values() {
    let text = "harness=codex\nfuture_field=alpha=beta gamma\nempty_future=\n";
    let meta = TaskMeta::parse(task_id("t-1"), text);
    assert_eq!(meta.harness(), Some("codex"));
    assert_eq!(meta.get("future_field"), Some("alpha=beta gamma"));
    assert_eq!(meta.get("empty_future"), Some(""));
    assert_eq!(
        meta.extra().collect::<Vec<_>>(),
        [("future_field", "alpha=beta gamma"), ("empty_future", "")]
    );
}

#[test]
fn key_named_id_is_an_ordinary_key_and_does_not_replace_the_file_identity() {
    let meta = TaskMeta::parse(task_id("t-1"), "id=other\n");
    assert_eq!(meta.id().as_str(), "t-1");
    assert_eq!(meta.get("id"), Some("other"));
}

#[test]
fn empty_typed_value_is_present_and_empty_not_absent() {
    let meta = TaskMeta::parse(task_id("t-1"), "endpoint_task_id=\n");
    assert_eq!(meta.endpoint_task_id(), Some(""));
    assert_eq!(meta.backend(), None);
}

#[test]
fn crlf_line_endings_are_not_part_of_values() {
    let meta = TaskMeta::parse(task_id("t-1"), "backend=tmux\r\nworktree=/w\r\n");
    assert_eq!(meta.backend(), Some("tmux"));
    assert_eq!(meta.worktree(), Some("/w"));
}

#[test]
fn final_line_keeps_a_lone_carriage_return_while_crlf_is_stripped() {
    let meta = TaskMeta::parse(task_id("t-1"), "kind=ship\r");
    assert_eq!(meta.kind(), Some("ship\r"));
    let meta = TaskMeta::parse(task_id("t-1"), "kind=ship\r\n");
    assert_eq!(meta.kind(), Some("ship"));
}

#[test]
fn padded_values_are_kept_verbatim_with_tabs() {
    let meta = TaskMeta::parse(task_id("t-1"), "endpoint_task_id=  padded value\t\n");
    assert_eq!(meta.endpoint_task_id(), Some("  padded value\t"));
}

#[test]
fn final_line_without_newline_is_read() {
    let meta = TaskMeta::parse(task_id("t-1"), "backend=tmux\nworktree=/w");
    assert_eq!(meta.backend(), Some("tmux"));
    assert_eq!(meta.worktree(), Some("/w"));
    assert_eq!(meta.pairs().count(), 2);
}

#[test]
fn blank_lines_are_skipped() {
    let meta = TaskMeta::parse(task_id("t-1"), "kind=ship\n\n   \nharness=x\n\n");
    assert_eq!(meta.kind(), Some("ship"));
    assert_eq!(meta.harness(), Some("x"));
    assert_eq!(meta.pairs().count(), 2);
}

#[test]
fn empty_text_is_a_record_with_no_keys() {
    let meta = TaskMeta::parse(task_id("t-1"), "");
    assert_eq!(meta.backend(), None);
    assert_eq!(meta.pairs().count(), 0);
}

#[test]
fn repeated_decision_keys_reads_the_last_value() {
    let text = "decisions_reviewed=1\ndecision_keys=a\ndecisions_reviewed=1\ndecision_keys=a,b\n";
    let meta = TaskMeta::parse(task_id("t-1"), text);
    assert_eq!(meta.get("decision_keys"), Some("a,b"));
    assert_eq!(
        meta.pairs().collect::<Vec<_>>(),
        [
            ("decisions_reviewed", "1"),
            ("decision_keys", "a"),
            ("decisions_reviewed", "1"),
            ("decision_keys", "a,b"),
        ]
    );
    assert!(matches!(
        meta.exact("decision_keys"),
        Err(error) if error.key() == "decision_keys"
    ));
}

#[test]
fn exact_lookup_of_a_repeated_window_or_worktree_is_ambiguous() {
    let text = "window=w1\nwindow=w2\nworktree=/a\nworktree=/a\nkind=ship\n";
    let meta = TaskMeta::parse(task_id("t-1"), text);
    assert!(matches!(
        meta.exact("window"),
        Err(error) if error.key() == "window"
    ));
    assert!(matches!(
        meta.exact("worktree"),
        Err(error) if error.key() == "worktree"
    ));
    assert_eq!(meta.exact("kind"), Ok(Some("ship")));
    assert_eq!(meta.exact("absent"), Ok(None));
    assert_eq!(meta.get("window"), Some("w2"));
    assert_eq!(meta.worktree(), Some("/a"));
}

#[test]
fn lines_without_a_separator_are_ignored_as_the_shell_ignores_them() {
    let meta = TaskMeta::parse(task_id("t-1"), "backend=tmux\nnot a pair\nkind=ship\n");
    assert_eq!(meta.backend(), Some("tmux"));
    assert_eq!(meta.kind(), Some("ship"));
    assert_eq!(
        meta.pairs().collect::<Vec<_>>(),
        [("backend", "tmux"), ("kind", "ship")]
    );
}

#[test]
fn lines_with_an_empty_key_are_ignored() {
    let meta = TaskMeta::parse(task_id("t-1"), "=value\nkind=ship\n");
    assert_eq!(meta.pairs().collect::<Vec<_>>(), [("kind", "ship")]);
}

#[test]
fn a_leading_byte_order_mark_is_not_part_of_the_first_key() {
    let meta = TaskMeta::parse(task_id("t-1"), "\u{feff}kind=ship\nharness=x\n");
    assert_eq!(meta.kind(), Some("ship"));
    assert_eq!(
        meta.pairs().collect::<Vec<_>>(),
        [("kind", "ship"), ("harness", "x")]
    );
}

#[test]
fn a_saved_record_with_a_byte_order_mark_reads_as_its_bare_twin() {
    for name in FIXTURES {
        let text = fs::read_to_string(fixture_dir().join(name)).expect("fixture is readable");
        let with_bom = TaskMeta::parse(task_id("t-1"), &format!("\u{feff}{text}"));
        let bare = TaskMeta::parse(task_id("t-1"), &text);
        assert_eq!(
            with_bom.pairs().collect::<Vec<_>>(),
            bare.pairs().collect::<Vec<_>>(),
            "{name}"
        );
    }
}
