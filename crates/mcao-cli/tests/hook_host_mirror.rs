//! Golden cases for `mcao hook host-mirror` (DPES-894 slice 3), the writer half of
//! `bin/fm-host-mirror.sh`. Each case runs the built binary against a scratch state directory and
//! checks the record it appends, its exit code, and its effect on the cursor and trim rules. The
//! expected record shape is the shell's `jq -c` output for the same fields.

use std::fs;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::Child;
use std::process::{Command, Output, Stdio};

use serde_json::json;

const KEY: &str = "main-session-key";
/// The writer's test-only lock ceiling override, in milliseconds. Production never sets it.
const LOCK_WAIT_ENV: &str = "MCAO_HOST_MIRROR_LOCK_WAIT_MS";

/// A scratch firstmate root with an empty `state/` directory, under cargo's per-target
/// temporary directory and removed before reuse.
fn scratch(name: &str) -> (PathBuf, PathBuf) {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("hook-host-mirror-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let state = root.join("state");
    fs::create_dir_all(&state).expect("create scratch state directory");
    (root, state)
}

fn mirror_path(state: &Path) -> PathBuf {
    state.join(".host-mirror.jsonl")
}

fn read_mirror(state: &Path) -> Vec<String> {
    fs::read_to_string(mirror_path(state))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The names of temp files the writer leaves in the state directory.
#[cfg(unix)]
fn temp_files(state: &Path) -> Vec<String> {
    fs::read_dir(state)
        .expect("list state directory")
        .map(|entry| {
            entry
                .expect("read entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name.contains(".tmp."))
        .collect()
}

/// Makes a state directory refuse new entries until dropped, so the writer cannot create its temp
/// file. Drop restores write permission, so a failing assertion still lets the next run remove the
/// scratch directory.
#[cfg(unix)]
struct ReadOnlyDir<'a>(&'a Path);

#[cfg(unix)]
impl ReadOnlyDir<'_> {
    fn lock(state: &Path) -> ReadOnlyDir<'_> {
        fs::set_permissions(state, fs::Permissions::from_mode(0o500))
            .expect("make state read-only");
        assert!(
            fs::write(state.join("probe"), b"").is_err(),
            "a read-only state directory must refuse new files"
        );
        ReadOnlyDir(state)
    }
}

#[cfg(unix)]
impl Drop for ReadOnlyDir<'_> {
    fn drop(&mut self) {
        let _ = fs::set_permissions(self.0, fs::Permissions::from_mode(0o700));
    }
}

/// Runs `mcao hook host-mirror` with the given harness and key, feeding `payload` on stdin.
fn run_writer(
    root: &Path,
    state: &Path,
    harness: &str,
    key: Option<&str>,
    payload: &str,
) -> Output {
    run_writer_with_lock_wait(root, state, harness, key, payload, None)
}

/// Like `run_writer`, with the writer's lock ceiling set to `lock_wait_ms` milliseconds when given.
fn run_writer_with_lock_wait(
    root: &Path,
    state: &Path,
    harness: &str,
    key: Option<&str>,
    payload: &str,
    lock_wait_ms: Option<&str>,
) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mcao"));
    command
        .args(["hook", "host-mirror", "--harness", harness, "--root"])
        .arg(root)
        .env("FM_STATE_OVERRIDE", state)
        .env_remove("FM_ROOT_OVERRIDE")
        .env_remove("FM_HOME")
        .env_remove("CLAUDE_PROJECT_DIR")
        .env_remove(LOCK_WAIT_ENV)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(lock_wait_ms) = lock_wait_ms {
        command.env(LOCK_WAIT_ENV, lock_wait_ms);
    }
    if let Some(key) = key {
        command.args(["--main-key", key]);
    }
    let mut child = command.spawn().expect("mcao spawns");
    child
        .stdin
        .take()
        .expect("stdin pipe")
        .write_all(payload.as_bytes())
        .expect("payload written");
    child.wait_with_output().expect("mcao exits")
}

fn prompt(prompt_id: &str, text: &str) -> String {
    json!({"hook_event_name": "UserPromptSubmit", "prompt_id": prompt_id, "prompt": text})
        .to_string()
}

/// Replaces the epoch, which varies per run, so a record compares exactly.
fn without_epoch(line: &str) -> String {
    let start = line.find("\"epoch\":").expect("record has an epoch") + "\"epoch\":".len();
    let end = start
        + line[start..]
            .find(|character: char| !character.is_ascii_digit())
            .expect("epoch ends");
    format!("{}0{}", &line[..start], &line[end..])
}

#[test]
fn prompt_submit_appends_one_user_record_and_exits_0() {
    let (root, state) = scratch("append");
    let output = run_writer(&root, &state, "claude", Some(KEY), &prompt("p1", "ship it"));
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
    let lines = read_mirror(&state);
    assert_eq!(lines.len(), 1);
    assert_eq!(
        without_epoch(&lines[0]),
        format!(
            r#"{{"seq":1,"epoch":0,"key":"{KEY}","id":"p1","tag":"captain","text":"ship it"}}"#
        )
    );
}

#[test]
fn turn_end_records_main_text_with_trailing_whitespace_trimmed() {
    let (root, state) = scratch("stop");
    let payload = json!({
        "hook_event_name": "Stop",
        "generation_id": 7,
        "last_assistant_message": "done \n\t",
    })
    .to_string();
    let output = run_writer(&root, &state, "claude", Some(KEY), &payload);
    assert_eq!(output.status.code(), Some(0));
    let lines = read_mirror(&state);
    assert_eq!(
        without_epoch(&lines[0]),
        format!(r#"{{"seq":1,"epoch":0,"key":"{KEY}","id":"7","tag":"main","text":"done"}}"#)
    );
}

#[test]
fn cursor_beside_the_mirror_raises_the_next_sequence_number() {
    let (root, state) = scratch("cursor");
    fs::write(state.join(".host-mirror-cursor.next"), "5\tsession\n").expect("write cursor");
    let output = run_writer(&root, &state, "claude", Some(KEY), &prompt("p1", "after"));
    assert_eq!(output.status.code(), Some(0));
    assert!(without_epoch(&read_mirror(&state)[0]).starts_with(r#"{"seq":6,"#));
}

/// Makes `state/.host-mirror.lock` a link to an owner directory whose `pid` is `holder_pid`, as a
/// shell or Rust writer leaves it while it holds the lock.
#[cfg(unix)]
fn hold_mirror_lock(state: &Path, holder_pid: u32) -> PathBuf {
    let owner = state.join(".host-mirror.lock.owner.test");
    fs::create_dir_all(&owner).expect("create owner directory");
    fs::write(owner.join("pid"), format!("{holder_pid}\n")).expect("write owner pid");
    std::os::unix::fs::symlink(&owner, state.join(".host-mirror.lock")).expect("link lock");
    owner
}

/// Lock names and owner directories a writer leaves in the state directory.
#[cfg(unix)]
fn lock_leftovers(state: &Path) -> Vec<String> {
    fs::read_dir(state)
        .expect("list state directory")
        .map(|entry| {
            entry
                .expect("read entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name.starts_with(".host-mirror.lock"))
        .collect()
}

/// Makes `state/.host-mirror.lock.steal` a link to an owner directory whose `pid` is `holder_pid`,
/// as a reclaimer holds the steal mutex while it removes a stale lock.
#[cfg(unix)]
fn hold_steal_mutex(state: &Path, holder_pid: u32) -> PathBuf {
    let owner = state.join(".host-mirror.lock.steal.owner.test");
    fs::create_dir_all(&owner).expect("create steal owner directory");
    fs::write(owner.join("pid"), format!("{holder_pid}\n")).expect("write steal owner pid");
    std::os::unix::fs::symlink(&owner, state.join(".host-mirror.lock.steal")).expect("link steal");
    owner
}

/// A pid that names no live process: a child that has exited and been reaped.
#[cfg(unix)]
fn dead_pid() -> u32 {
    let mut child = Command::new("true").spawn().expect("spawn");
    let pid = child.id();
    child.wait().expect("reap");
    pid
}

/// A scratch firstmate root for the shell writer: `bin` links the tracked scripts, and a fake
/// `claude` is the session that runs the writer, so the writer records as that session.
#[cfg(unix)]
fn shell_root(name: &str) -> (PathBuf, PathBuf, PathBuf) {
    let (root, state) = scratch(name);
    let bin = fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bin"))
        .expect("repo bin directory");
    assert!(
        Command::new("git")
            .arg("init")
            .arg("-q")
            .arg(&root)
            .status()
            .expect("git runs")
            .success()
    );
    fs::write(root.join("AGENTS.md"), "").expect("write AGENTS.md");
    std::os::unix::fs::symlink(&bin, root.join("bin")).expect("link bin");
    let fakebin = root.join("fakebin");
    fs::create_dir(&fakebin).expect("create fakebin");
    std::os::unix::fs::symlink("/bin/bash", fakebin.join("claude")).expect("link claude");
    (root, state, bin)
}

/// Starts the shell writer session running `hooks`, after recording the session lock.
#[cfg(unix)]
fn spawn_shell_session(root: &Path, state: &Path, bin: &Path, hooks: &str) -> Child {
    let script = format!("printf '%s\\n' \"$$\" > \"$FM_STATE_OVERRIDE/.lock\"\n{hooks}");
    Command::new(root.join("fakebin/claude"))
        .arg("-c")
        .arg(script)
        .env("MIRROR", bin.join("fm-host-mirror.sh"))
        .env("FM_ROOT_OVERRIDE", root)
        .env("FM_STATE_OVERRIDE", state)
        .env("FM_CONFIG_OVERRIDE", root.join("config"))
        .env_remove("FM_HOME")
        .env_remove("CLAUDE_PROJECT_DIR")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("shell writer spawns")
}

/// A live holder past the ceiling: the writer gives up, records nothing, and exits 0. The test
/// process is alive and holds the lock, and the ceiling is shortened to 200 ms so the path is
/// reached quickly. The production ceiling is `LOCK_WAIT`.
#[cfg(unix)]
#[test]
fn held_lock_past_the_ceiling_records_nothing_and_exits_0() {
    let (root, state) = scratch("locked");
    let existing = format!(
        "{{\"seq\":1,\"epoch\":1,\"key\":\"{KEY}\",\"id\":\"p0\",\"tag\":\"main\",\"text\":\"kept\"}}\n"
    );
    fs::write(mirror_path(&state), &existing).expect("seed mirror");
    let owner = hold_mirror_lock(&state, std::process::id());
    let output = run_writer_with_lock_wait(
        &root,
        &state,
        "claude",
        Some(KEY),
        &prompt("p1", "blocked"),
        Some("200"),
    );
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty() && output.stderr.is_empty());
    assert_eq!(
        fs::read_to_string(mirror_path(&state)).expect("mirror still readable"),
        existing
    );
    assert!(temp_files(&state).is_empty());
    // A live holder's lock is left exactly as it was: the link, the owner directory, and its pid.
    assert_eq!(
        fs::read_link(state.join(".host-mirror.lock")).expect("lock still linked"),
        owner
    );
    assert_eq!(
        fs::read_to_string(owner.join("pid")).expect("holder pid kept"),
        format!("{}\n", std::process::id())
    );
}

#[cfg(unix)]
#[test]
fn dead_holder_lock_is_reclaimed_and_released() {
    let (root, state) = scratch("dead-holder");
    let mut child = Command::new("true").spawn().expect("spawn");
    let dead_pid = child.id();
    child.wait().expect("reap");
    hold_mirror_lock(&state, dead_pid);
    let output = run_writer(
        &root,
        &state,
        "claude",
        Some(KEY),
        &prompt("p1", "reclaimed"),
    );
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        without_epoch(&read_mirror(&state)[0]),
        format!(
            r#"{{"seq":1,"epoch":0,"key":"{KEY}","id":"p1","tag":"captain","text":"reclaimed"}}"#
        )
    );
    assert!(
        lock_leftovers(&state).is_empty(),
        "the released lock left {:?}",
        lock_leftovers(&state)
    );
}

#[cfg(unix)]
#[test]
fn dead_holder_lock_is_reclaimed_by_the_shell_writer() {
    if Command::new("jq").arg("--version").output().is_err() {
        eprintln!("skip: jq absent");
        return;
    }
    let (root, state, bin) = shell_root("shell-dead-holder");
    hold_mirror_lock(&state, dead_pid());
    let mut shell = spawn_shell_session(
        &root,
        &state,
        &bin,
        r#"printf '%s' '{"hook_event_name":"UserPromptSubmit","prompt_id":"shell-reclaim","prompt":"shell reclaimed"}' | "$MIRROR" hook claude"#,
    );
    assert!(shell.wait().expect("shell writer exits").success());
    let lines = read_mirror(&state);
    assert_eq!(
        lines.len(),
        1,
        "the shell writer must reclaim the dead holder"
    );
    assert!(lines[0].contains(r#""text":"shell reclaimed""#));
    assert!(
        lock_leftovers(&state).is_empty(),
        "the released lock left {:?}",
        lock_leftovers(&state)
    );
}

#[cfg(unix)]
#[test]
fn held_steal_mutex_blocks_reclaim_of_a_stale_lock() {
    let (root, state) = scratch("held-steal");
    let owner = hold_mirror_lock(&state, dead_pid());
    // This test process is alive, so it holds the steal mutex as a live reclaimer would.
    let steal_owner = hold_steal_mutex(&state, std::process::id());
    let output = run_writer(&root, &state, "claude", Some(KEY), &prompt("p1", "blocked"));
    assert_eq!(output.status.code(), Some(0));
    assert!(read_mirror(&state).is_empty());
    // The stale holder is left for the steal mutex's owner to reclaim.
    assert_eq!(
        fs::read_link(state.join(".host-mirror.lock")).expect("lock still linked"),
        owner
    );
    assert_eq!(
        fs::read_link(state.join(".host-mirror.lock.steal")).expect("steal still linked"),
        steal_owner
    );
}

#[cfg(unix)]
#[test]
fn dangling_steal_link_is_held_and_records_nothing() {
    let (root, state) = scratch("dangling-steal");
    let owner = hold_mirror_lock(&state, dead_pid());
    let gone = state.join(".host-mirror.lock.steal.owner.gone");
    std::os::unix::fs::symlink(&gone, state.join(".host-mirror.lock.steal")).expect("link steal");
    let output = run_writer(&root, &state, "claude", Some(KEY), &prompt("p1", "blocked"));
    assert_eq!(output.status.code(), Some(0));
    assert!(read_mirror(&state).is_empty());
    assert_eq!(
        fs::read_link(state.join(".host-mirror.lock")).expect("lock still linked"),
        owner
    );
    assert_eq!(
        fs::read_link(state.join(".host-mirror.lock.steal")).expect("dangling steal kept"),
        gone
    );
}

#[test]
fn unreadable_mirror_exits_0_and_records_nothing() {
    let (root, state) = scratch("unreadable");
    fs::create_dir(mirror_path(&state)).expect("make mirror a directory");
    let output = run_writer(&root, &state, "claude", Some(KEY), &prompt("p1", "lost"));
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty() && output.stderr.is_empty());
    assert!(mirror_path(&state).is_dir());
}

#[cfg(unix)]
#[test]
fn read_only_state_exits_0_and_leaves_the_mirror_intact() {
    let (root, state) = scratch("write-failure-intact");
    let existing = format!(
        "{{\"seq\":1,\"epoch\":1,\"key\":\"{KEY}\",\"id\":\"p0\",\"tag\":\"main\",\"text\":\"kept\"}}\n"
    );
    fs::write(mirror_path(&state), &existing).expect("seed mirror");
    // The lock link cannot be created in a read-only directory, so nothing is written and the
    // writer exits 0.
    let read_only = ReadOnlyDir::lock(&state);
    let output = run_writer(&root, &state, "claude", Some(KEY), &prompt("p1", "lost"));
    drop(read_only);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        fs::read_to_string(mirror_path(&state)).expect("mirror still readable"),
        existing
    );
    assert!(temp_files(&state).is_empty());
}

#[cfg(unix)]
#[test]
fn existing_bytes_are_kept_and_an_unparseable_line_is_skipped_for_the_sequence() {
    let (root, state) = scratch("corrupt");
    let valid = format!(
        "{{\"seq\":1,\"epoch\":1,\"key\":\"{KEY}\",\"id\":\"p0\",\"tag\":\"main\",\"text\":\"kept\"}}\n"
    );
    let mut existing = valid.into_bytes();
    existing.extend_from_slice(b"\xff\xfe not json\n");
    fs::write(mirror_path(&state), &existing).expect("seed mirror");
    let output = run_writer(&root, &state, "claude", Some(KEY), &prompt("p1", "after"));
    assert_eq!(output.status.code(), Some(0));
    // The shell's append keeps existing bytes as they are, so the Rust writer must not rewrite them
    // through a lossy UTF-8 copy.
    let written = fs::read(mirror_path(&state)).expect("mirror readable");
    assert!(
        written.starts_with(&existing),
        "existing bytes were rewritten"
    );
    let last = String::from_utf8_lossy(&written)
        .lines()
        .last()
        .expect("a record")
        .to_owned();
    assert!(
        last.starts_with(r#"{"seq":2,"#),
        "the unparseable line must not set the sequence: {last}"
    );
}

#[test]
fn stands_down_without_key_harness_state_or_payload() {
    let (root, state) = scratch("stand-down");
    for output in [
        run_writer(&root, &state, "claude", None, &prompt("p1", "no key")),
        run_writer(
            &root,
            &state,
            "claude",
            Some(""),
            &prompt("p1", "empty key"),
        ),
        run_writer(&root, &state, "claude", Some(KEY), ""),
    ] {
        assert_eq!(output.status.code(), Some(0));
    }
    assert!(read_mirror(&state).is_empty());

    let missing = root.join("no-state-here");
    let output = run_writer(&root, &missing, "claude", Some(KEY), &prompt("p1", "x"));
    assert_eq!(output.status.code(), Some(0));
    assert!(!missing.exists());
}

#[test]
fn claude_foreign_host_payload_and_user_task_notification_are_dropped() {
    let (root, state) = scratch("dropped");
    let foreign = json!({"cursor_version": "0.1.0", "hook_event_name": "Stop",
        "generation_id": 1, "last_assistant_message": "not ours"})
    .to_string();
    assert_eq!(
        run_writer(&root, &state, "claude", Some(KEY), &foreign)
            .status
            .code(),
        Some(0)
    );
    let notification = prompt(
        "p2",
        "<task-notification>background done</task-notification>",
    );
    assert_eq!(
        run_writer(&root, &state, "claude", Some(KEY), &notification)
            .status
            .code(),
        Some(0)
    );
    assert!(read_mirror(&state).is_empty());
}

#[test]
fn same_id_and_text_is_recorded_once_and_changed_text_is_recorded() {
    let (root, state) = scratch("dedupe");
    run_writer(&root, &state, "claude", Some(KEY), &prompt("p1", "first"));
    run_writer(&root, &state, "claude", Some(KEY), &prompt("p1", "first"));
    assert_eq!(read_mirror(&state).len(), 1);
    run_writer(&root, &state, "claude", Some(KEY), &prompt("p1", "second"));
    let lines = read_mirror(&state);
    assert_eq!(lines.len(), 2);
    assert!(lines[1].starts_with(r#"{"seq":2,"#));
}

#[test]
fn file_at_the_trim_threshold_keeps_its_newest_199_entries_plus_the_new_one() {
    let (root, state) = scratch("trim");
    let mut seeded = String::new();
    for seq in 1..=300 {
        seeded.push_str(&format!(
            "{{\"seq\":{seq},\"epoch\":1,\"key\":\"{KEY}\",\"id\":\"\",\"tag\":\"main\",\"text\":\"t{seq}\"}}\n"
        ));
    }
    fs::write(mirror_path(&state), seeded).expect("seed mirror");
    let output = run_writer(&root, &state, "claude", Some(KEY), &prompt("p1", "new"));
    assert_eq!(output.status.code(), Some(0));
    let lines = read_mirror(&state);
    assert_eq!(lines.len(), 200);
    assert!(lines[0].starts_with(r#"{"seq":102,"#));
    assert!(lines[199].starts_with(r#"{"seq":301,"#));
}

#[test]
fn text_over_the_cap_keeps_head_and_tail_around_the_omitted_count() {
    let (root, state) = scratch("cap");
    let text = "a".repeat(4500);
    run_writer(&root, &state, "claude", Some(KEY), &prompt("p1", &text));
    let line = &read_mirror(&state)[0];
    let record: serde_json::Value = serde_json::from_str(line).expect("record parses");
    let recorded = record["text"].as_str().expect("text is a string");
    assert_eq!(recorded.chars().count(), 4000);
    assert!(recorded.contains("\n[mirror truncated: 544 characters omitted]\n"));
}

/// Records one prompt and returns the text field of the record it appended.
fn recorded_text(root: &Path, state: &Path, text: &str) -> String {
    run_writer(root, state, "claude", Some(KEY), &prompt("p1", text));
    let record: serde_json::Value =
        serde_json::from_str(&read_mirror(state)[0]).expect("record parses");
    record["text"]
        .as_str()
        .expect("text is a string")
        .to_owned()
}

#[test]
fn multibyte_text_over_the_cap_counts_characters_not_bytes() {
    let (root, state) = scratch("cap-multibyte");
    // 2-, 4-, 3-, and 2-byte characters: 13500 characters in 37125 bytes (11 per repeat).
    let text = "é🎉世ß".repeat(3375);
    let characters: Vec<char> = text.chars().collect();
    // The note takes 45 characters of the cap, so 3955 characters are kept: 1978 head and 1977 tail.
    // Omitted is 13500 - 3955 = 9545.
    let expected = format!(
        "{}\n[mirror truncated: 9545 characters omitted]\n{}",
        characters[..1978].iter().collect::<String>(),
        characters[characters.len() - 1977..]
            .iter()
            .collect::<String>(),
    );
    assert_eq!(recorded_text(&root, &state, &text), expected);
}

#[test]
fn multibyte_text_at_the_cap_is_recorded_whole() {
    let (root, state) = scratch("cap-multibyte-exact");
    // 4000 characters in 12000 bytes: a byte count would truncate it.
    let text = format!("{}{}", "é".repeat(2000), "🎉".repeat(2000));
    assert_eq!(recorded_text(&root, &state, &text), text);
}

#[test]
fn multibyte_text_one_character_over_the_cap_is_truncated_by_characters() {
    let (root, state) = scratch("cap-multibyte-plus-one");
    // 4001 characters. Omitting 44 needs a 43-character note, which leaves 3957 kept: 1979 head
    // and 1978 tail.
    let text = format!("{}世", "é🎉".repeat(2000));
    let characters: Vec<char> = text.chars().collect();
    let expected = format!(
        "{}\n[mirror truncated: 44 characters omitted]\n{}",
        characters[..1979].iter().collect::<String>(),
        characters[characters.len() - 1978..]
            .iter()
            .collect::<String>(),
    );
    assert_eq!(recorded_text(&root, &state, &text), expected);
}

#[test]
fn control_and_del_characters_are_escaped_as_jq_escapes_them() {
    let (root, state) = scratch("escape");
    run_writer(
        &root,
        &state,
        "claude",
        Some(KEY),
        &prompt("p1", "a\u{7f}\u{1}\t\"\\"),
    );
    assert_eq!(
        without_epoch(&read_mirror(&state)[0]),
        format!(
            r#"{{"seq":1,"epoch":0,"key":"{KEY}","id":"p1","tag":"captain","text":"a\u007f\u0001\t\"\\"}}"#
        )
    );
}

/// A shell writer and a Rust writer append to one mirror at the same time. Both take the shell's
/// directory lock, so the 80 records carry sequence numbers 1 through 80 and every id once.
#[cfg(unix)]
#[test]
fn concurrent_shell_and_rust_writers_lose_no_records() {
    if Command::new("jq").arg("--version").output().is_err() {
        eprintln!("skip: jq absent");
        return;
    }
    const COUNT: usize = 40;
    let (root, state, bin) = shell_root("concurrent");
    let hooks = r#"i=1
while [ "$i" -le 40 ]; do
  printf '%s' "{\"hook_event_name\":\"UserPromptSubmit\",\"prompt_id\":\"shell-$i\",\"prompt\":\"shell $i\"}" | "$MIRROR" hook claude
  i=$((i + 1))
done"#;
    let mut shell = spawn_shell_session(&root, &state, &bin, hooks);
    for n in 1..=COUNT {
        let output = run_writer(
            &root,
            &state,
            "claude",
            Some(KEY),
            &prompt(&format!("rust-{n}"), &format!("rust {n}")),
        );
        assert_eq!(output.status.code(), Some(0));
    }
    assert!(shell.wait().expect("shell writer exits").success());

    let records: Vec<serde_json::Value> = read_mirror(&state)
        .iter()
        .map(|line| serde_json::from_str(line).expect("record parses"))
        .collect();
    assert_eq!(records.len(), 2 * COUNT, "a writer lost records");
    let mut seqs: Vec<u64> = records
        .iter()
        .map(|record| record["seq"].as_u64().expect("seq is a number"))
        .collect();
    seqs.sort_unstable();
    assert_eq!(seqs, (1..=2 * COUNT as u64).collect::<Vec<_>>());
    let mut ids: Vec<String> = records
        .iter()
        .map(|record| record["id"].as_str().expect("id is a string").to_owned())
        .collect();
    ids.sort_unstable();
    let mut expected: Vec<String> = (1..=COUNT)
        .flat_map(|n| [format!("rust-{n}"), format!("shell-{n}")])
        .collect();
    expected.sort_unstable();
    assert_eq!(ids, expected);
    assert!(lock_leftovers(&state).is_empty());
}
