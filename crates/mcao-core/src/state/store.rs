//! Atomic publication of `state/<id>.meta` records.
//!
//! The record is written to a new temporary file in the state directory, flushed to disk, and
//! renamed over the record. A reader sees the previous record or the new one, never a partial
//! write. Each writer uses its own temporary file, so concurrent writers never share a buffer;
//! the last rename wins.

use std::fs;
use std::io;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::error::McaoError;

use super::encoding::{BackendToken, PathStyle, stamp_encoding};
use super::id::TaskId;
use super::lock::lock_meta;
use super::meta::TaskMeta;

/// Names tried for a new temporary file before giving up. A name is taken only by a file left
/// behind from an earlier process that reused this process id.
const TEMP_NAME_ATTEMPTS: u32 = 16;

/// Renames tried before a sharing conflict is reported. The pauses between them double from
/// [`RENAME_BASE_DELAY`] up to [`RENAME_MAX_DELAY`], so the nine pauses total 9.15 s.
const RENAME_ATTEMPTS: u32 = 10;

/// First pause after a sharing conflict.
const RENAME_BASE_DELAY: Duration = Duration::from_millis(50);

/// Longest pause between rename attempts.
const RENAME_MAX_DELAY: Duration = Duration::from_millis(2000);

/// Distinguishes temporary file names within one process.
static NEXT_TEMP_SERIAL: AtomicU64 = AtomicU64::new(0);

/// Atomically replaces `state_dir/<id>.meta` with `text`.
///
/// `text` is the complete record and is written verbatim. A writer that edits an existing record
/// must pass the raw text with its edit applied: [`crate::state::TaskMeta`] drops lines it does
/// not read, so rebuilding a record from it would lose them. The publish takes the record's
/// sidecar lock, so it cannot interleave with another Rust writer of the same record.
///
/// On failure the temporary file is removed and the previous record is left in place.
///
/// # Errors
///
/// Returns [`McaoError::Io`] when the temporary file cannot be created, written, flushed, or
/// renamed over the record, and when the sidecar lock cannot be taken.
pub fn publish_meta(state_dir: &Path, id: &TaskId, text: &str) -> Result<(), McaoError> {
    let _lock = lock_meta(state_dir, id)?;
    publish_with(state_dir, id, text, || Ok(()))
}

/// Publishes `text` stamped with the version 2 encoding for `style`, as [`stamp_encoding`] builds
/// it, so the record carries exactly one `format=2` line and one `path_style=` line and ends every
/// line in `\n`.
///
/// The record is checked first, and a record that fails is never published, because the shell
/// refuses to dispatch or clean up such a task. The checks mirror the endpoint validation in
/// `fm_backend_validate_task_endpoint` (`bin/fm-backend.sh`), covering these rules:
///
/// - `format`, if present, must be `1` or `2`. The stamp replaces it with `2`, so a value written
///   by a newer writer is refused rather than relabelled.
/// - `execution_target`, if present, must appear once and name `local`. A record for another
///   target is refused: this writer does not validate cloud records, and DPES-882 owns their
///   publication.
/// - `backend`, if present, must appear once and hold a token this writer knows. An absent key
///   reads as `tmux`. An empty value and a repeated key are refused. The token `windows` is
///   accepted as a version 2 token and gets no endpoint rule here. The shell does not know that
///   token and refuses such a record as an unknown backend.
/// - `window`, `worktree`, and `project` must each appear once, with a non-empty value that holds
///   no line break, tab, or NUL.
/// - `endpoint_task_id`, if present, must appear once, be non-empty, and name this task.
/// - A `tmux` record's `window` must split at its first `:` into a non-empty session and a pane
///   named `fm-<task id>`. The session may contain spaces.
/// - A `herdr` record must carry an `endpoint_task_id` that names this task. Each of
///   `herdr_session`, `herdr_workspace_id`, `herdr_tab_id`, and `herdr_pane_id` must appear once
///   with a non-empty value. The `window` must be `<herdr_session>:<herdr_pane_id>`. The session
///   and workspace must be shell atoms, meaning ASCII letters, digits, and `._@%+-`. The tab and
///   pane must be atoms once each `:` is read as `_`, as the shell reads them.
///
/// Passing these checks means the shell's endpoint validation accepts the record. Other checks the
/// shell makes at cleanup are not repeated here.
///
/// The `path_style` line is the writer's own. A value in `text` that names `style` is dropped, and
/// one that names another style is refused by [`stamp_encoding`].
///
/// The publish takes the record's sidecar lock, as [`publish_meta`] does.
///
/// # Errors
///
/// Returns [`McaoError::Refused`] when a check fails or the stamp refuses the `path_style` line,
/// and nothing is written. Returns [`McaoError::Io`] under the same conditions as [`publish_meta`].
pub fn publish_meta_encoded(
    state_dir: &Path,
    id: &TaskId,
    text: &str,
    style: PathStyle,
) -> Result<(), McaoError> {
    // The record is checked before its sidecar is taken, so a refused record touches no file.
    let stamped = checked_stamp(id, text, style)?;
    let _lock = lock_meta(state_dir, id)?;
    publish_with(state_dir, id, &stamped, || Ok(()))
}

/// Checks, stamps, and publishes like [`publish_meta_encoded`], without taking the sidecar lock.
///
/// The caller must hold the record's sidecar lock for the whole read-merge-publish it belongs to,
/// or another writer can land between the read and the publish and be lost.
///
/// # Errors
///
/// As [`publish_meta_encoded`], without the lock error.
pub(crate) fn publish_meta_encoded_locked(
    state_dir: &Path,
    id: &TaskId,
    text: &str,
    style: PathStyle,
) -> Result<(), McaoError> {
    let stamped = checked_stamp(id, text, style)?;
    publish_with(state_dir, id, &stamped, || Ok(()))
}

/// Checks `text` as the record writer does, then stamps it for `style`. Nothing is read or written,
/// so a refusal touches no file.
fn checked_stamp(id: &TaskId, text: &str, style: PathStyle) -> Result<String, McaoError> {
    let refuse = |reason: String| McaoError::Refused(format!("task {}: {reason}", id.as_str()));
    check_record(&TaskMeta::parse(id.clone(), text), id).map_err(refuse)?;
    stamp_encoding(text, style).map_err(|error| refuse(error.to_string()))
}

/// Returns the first reason `meta` may not be published for task `id`, or `Ok(())`.
fn check_record(meta: &TaskMeta, id: &TaskId) -> Result<(), String> {
    meta.format_version().map_err(|error| error.to_string())?;
    let backend = check_backend(meta)?;
    let window = endpoint_value(meta, "window")?;
    endpoint_value(meta, "worktree")?;
    endpoint_value(meta, "project")?;
    check_endpoint_binding(meta, id)?;
    match backend {
        BackendToken::Tmux => check_tmux_endpoint(window, id),
        BackendToken::Herdr => check_herdr_endpoint(meta, window, id),
        BackendToken::Windows => Ok(()),
    }
}

/// Refuses a `backend` key the shell would refuse, and returns the token of one it accepts.
/// [`TaskMeta::backend_token`] also refuses a record whose execution target is not local, so that
/// check runs here as well.
fn check_backend(meta: &TaskMeta) -> Result<BackendToken, String> {
    match meta.exact("backend") {
        Err(error) => Err(error.to_string()),
        Ok(Some("")) => Err("empty backend".to_owned()),
        Ok(_) => meta.backend_token().map_err(|error| error.to_string()),
    }
}

/// Returns the value of an endpoint key the shell reads: exactly one line, non-empty, and free of
/// line breaks, tabs, and NULs.
fn endpoint_value<'a>(meta: &'a TaskMeta, key: &str) -> Result<&'a str, String> {
    match meta.exact(key) {
        Ok(Some(value)) if value.contains(['\n', '\r', '\t', '\0']) => {
            Err(format!("malformed {key} value"))
        }
        Ok(Some(value)) if !value.is_empty() => Ok(value),
        Ok(_) => Err(format!("missing or empty {key}")),
        Err(error) => Err(error.to_string()),
    }
}

/// Refuses a tmux `window` the shell refuses. It must split at its first `:` into a non-empty
/// session and a pane named `fm-<task id>`. The session may contain spaces.
fn check_tmux_endpoint(window: &str, id: &TaskId) -> Result<(), String> {
    match window.split_once(':') {
        Some((session, pane))
            if !session.is_empty() && pane.strip_prefix("fm-") == Some(id.as_str()) =>
        {
            Ok(())
        }
        _ => Err(format!(
            "tmux endpoint '{window}' is malformed or does not belong to task {}",
            id.as_str()
        )),
    }
}

/// Refuses a `herdr` record the shell refuses. The record must bind to this task, carry each
/// `herdr_*` key once with a non-empty value, and name its window
/// `<herdr_session>:<herdr_pane_id>`. The session and workspace must be shell atoms. The tab and
/// pane must be atoms once each `:` is read as `_`, as the shell reads them.
fn check_herdr_endpoint(meta: &TaskMeta, window: &str, id: &TaskId) -> Result<(), String> {
    let refused = || {
        format!(
            "Herdr endpoint metadata for task {} is malformed or inconsistent",
            id.as_str()
        )
    };
    let bound = matches!(
        meta.exact("endpoint_task_id"),
        Ok(Some(binding)) if binding == id.as_str()
    );
    if !bound {
        return Err(refused());
    }
    let (Some(session), Some(workspace), Some(tab), Some(pane)) = (
        herdr_value(meta, "herdr_session"),
        herdr_value(meta, "herdr_workspace_id"),
        herdr_value(meta, "herdr_tab_id"),
        herdr_value(meta, "herdr_pane_id"),
    ) else {
        return Err(refused());
    };
    let atoms = is_shell_atom(session)
        && is_shell_atom(workspace)
        && is_shell_atom(&tab.replace(':', "_"))
        && is_shell_atom(&pane.replace(':', "_"));
    if atoms && window == format!("{session}:{pane}") {
        Ok(())
    } else {
        Err(refused())
    }
}

/// Returns the value of a `herdr_*` key that appears once and is non-empty.
fn herdr_value<'a>(meta: &'a TaskMeta, key: &str) -> Option<&'a str> {
    match meta.exact(key) {
        Ok(Some(value)) if !value.is_empty() => Some(value),
        _ => None,
    }
}

/// Reports whether `value` is a non-empty atom of ASCII letters, digits, and `._@%+-`, the set
/// `fm_backend_endpoint_atom_valid` accepts.
fn is_shell_atom(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'@' | b'%' | b'+' | b'-')
        })
}

/// Refuses an `endpoint_task_id` that is repeated, empty, or names another task. The key may be
/// absent.
fn check_endpoint_binding(meta: &TaskMeta, id: &TaskId) -> Result<(), String> {
    match meta.exact("endpoint_task_id") {
        Ok(None) => Ok(()),
        Ok(Some("")) => Err("empty endpoint task binding".to_owned()),
        Ok(Some(binding)) if binding == id.as_str() => Ok(()),
        Ok(Some(_)) => Err("endpoint metadata belongs to another task".to_owned()),
        Err(error) => Err(error.to_string()),
    }
}

/// Publishes like [`publish_meta`], calling `before_rename` once the temporary file is written and
/// flushed and before it is renamed over the record. An error from `before_rename` takes the same
/// cleanup path as a failed rename, so a test can inject a failure at that point.
///
/// After the rename the state directory is synced where the platform allows it, so a power loss
/// cannot revert the rename. A failure of that sync is returned although the new record is already
/// in place: the caller then knows the record is visible but not yet proven durable.
pub(super) fn publish_with(
    state_dir: &Path,
    id: &TaskId,
    text: &str,
    before_rename: impl FnOnce() -> io::Result<()>,
) -> Result<(), McaoError> {
    let target = state_dir.join(format!("{}.meta", id.as_str()));
    let (temp_path, temp) = create_temp(state_dir, id)?;
    if let Err(error) = write_and_sync(temp, text)
        .and_then(|()| before_rename())
        .and_then(|()| rename_replacing(&temp_path, &target))
    {
        let _ = fs::remove_file(&temp_path);
        return Err(McaoError::Io(error));
    }
    sync_dir(state_dir).map_err(McaoError::Io)
}

/// Flushes the directory entry that a rename changed.
///
/// A file system that cannot sync a directory reports `EINVAL`, which Rust maps to
/// `InvalidInput`. It has no stronger guarantee to offer, so that case counts as done.
#[cfg(unix)]
fn sync_dir(dir: &Path) -> io::Result<()> {
    match fs::File::open(dir).and_then(|handle| handle.sync_all()) {
        Err(error) if error.kind() == io::ErrorKind::InvalidInput => Ok(()),
        other => other,
    }
}

/// `std` offers no directory sync on Windows, so the rename is as durable as the platform makes it.
#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> io::Result<()> {
    Ok(())
}

/// Creates a new temporary file in `state_dir`.
///
/// The name carries the task id, the process id, and a serial number. `create_new` refuses an
/// existing file, so a writer never opens another writer's file or one left by an interrupted
/// write. The temporary name starts with `.` and ends in `.tmp`, so it is never read as a record.
fn create_temp(state_dir: &Path, id: &TaskId) -> io::Result<(PathBuf, fs::File)> {
    for _ in 0..TEMP_NAME_ATTEMPTS {
        let serial = NEXT_TEMP_SERIAL.fetch_add(1, Ordering::Relaxed);
        let path = state_dir.join(format!(
            ".{}.meta.{}.{serial}.tmp",
            id.as_str(),
            std::process::id()
        ));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        match options.open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no unused temporary record name",
    ))
}

/// Writes `text` and flushes it to disk. Consuming `temp` closes the file before the rename.
fn write_and_sync(mut temp: fs::File, text: &str) -> io::Result<()> {
    temp.write_all(text.as_bytes())?;
    temp.sync_all()
}

/// Renames `from` over `to`, replacing `to`.
///
/// `fs::rename` replaces an existing destination on Unix and on Windows. On Windows a rename can
/// also fail while another process, such as a virus scanner, holds the destination open. Those
/// refusals are retried by [`retry_rename`], and the last error is returned.
fn rename_replacing(from: &Path, to: &Path) -> io::Result<()> {
    retry_rename(
        || fs::rename(from, to),
        is_sharing_conflict,
        std::thread::sleep,
    )
}

/// Runs `rename` until it succeeds, fails with an error `is_conflict` does not accept, or has
/// failed [`RENAME_ATTEMPTS`] times. Before each retry it calls `pause` with that retry's delay,
/// which starts at [`RENAME_BASE_DELAY`], doubles, and stops at [`RENAME_MAX_DELAY`].
fn retry_rename(
    mut rename: impl FnMut() -> io::Result<()>,
    is_conflict: impl Fn(&io::Error) -> bool,
    mut pause: impl FnMut(Duration),
) -> io::Result<()> {
    let mut delay = RENAME_BASE_DELAY;
    let mut attempt = 1;
    loop {
        match rename() {
            Err(error) if attempt < RENAME_ATTEMPTS && is_conflict(&error) => {
                pause(delay);
                delay = (delay * 2).min(RENAME_MAX_DELAY);
                attempt += 1;
            }
            outcome => return outcome,
        }
    }
}

/// Reports whether a rename failed because another process holds the destination open.
///
/// Windows reports that case as access denied (5) or sharing violation (32). Lock violation (33)
/// is also retried. The ticket names only 5 and 32, so 33 is a documented superset:
/// `tests/state_publish.rs` already treats it as a race a Windows reader meets while a record is
/// replaced. Other platforms do not retry.
#[cfg(windows)]
fn is_sharing_conflict(error: &io::Error) -> bool {
    matches!(error.raw_os_error(), Some(5 | 32 | 33))
}

#[cfg(not(windows))]
fn is_sharing_conflict(_error: &io::Error) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Removes its directory on drop, so a failed assertion leaves nothing behind.
    struct ScratchDir(PathBuf);

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn failure_before_the_rename_keeps_the_previous_record() {
        let scratch = ScratchDir(
            std::env::temp_dir().join(format!("mcao-publish-interrupted-{}", std::process::id())),
        );
        let _ = fs::remove_dir_all(&scratch.0);
        fs::create_dir_all(&scratch.0).expect("scratch dir is created");
        let id = TaskId::parse("task-c").expect("test task id is valid");
        let old = "harness=claude\nkind=ship\n";
        publish_meta(&scratch.0, &id, old).expect("seed publish succeeds");

        let result = publish_with(&scratch.0, &id, "harness=codex\n", || {
            Err(io::Error::other("injected failure before the rename"))
        });

        assert!(
            matches!(result, Err(McaoError::Io(_))),
            "injected failure is reported: {result:?}"
        );
        assert_eq!(
            fs::read_to_string(scratch.0.join("task-c.meta")).expect("record is readable"),
            old,
            "the previous record is kept whole"
        );
        let mut names: Vec<String> = fs::read_dir(&scratch.0)
            .expect("scratch dir is listable")
            .map(|entry| {
                entry
                    .expect("directory entry is readable")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        // The publish takes the record's sidecar lock, which stays behind by design.
        names.retain(|name| name != "task-c.meta.lock");
        assert_eq!(names, ["task-c.meta"], "the temporary file is removed");
    }

    /// The Windows sharing-violation code, which the retry treats as a conflict on every platform
    /// in these tests.
    const SHARING_VIOLATION: i32 = 32;

    fn is_sharing_violation(error: &io::Error) -> bool {
        error.raw_os_error() == Some(SHARING_VIOLATION)
    }

    #[test]
    fn persistent_sharing_conflict_uses_doubling_pauses_then_reports_the_error() {
        let mut attempts = 0;
        let mut pauses = Vec::new();
        let result = retry_rename(
            || {
                attempts += 1;
                Err(io::Error::from_raw_os_error(SHARING_VIOLATION))
            },
            is_sharing_violation,
            |delay| pauses.push(delay),
        );

        assert_eq!(attempts, RENAME_ATTEMPTS, "every attempt is made");
        let millis: Vec<u128> = pauses.iter().map(Duration::as_millis).collect();
        assert_eq!(
            millis,
            [50, 100, 200, 400, 800, 1600, 2000, 2000, 2000],
            "pauses double from 50 ms and stop at 2000 ms"
        );
        assert_eq!(
            result
                .expect_err("the conflict never clears")
                .raw_os_error(),
            Some(SHARING_VIOLATION),
            "the last error is returned"
        );
    }

    #[test]
    fn sharing_conflict_that_clears_stops_retrying() {
        let mut attempts = 0;
        let mut pauses = Vec::new();
        let result = retry_rename(
            || {
                attempts += 1;
                if attempts < 4 {
                    Err(io::Error::from_raw_os_error(SHARING_VIOLATION))
                } else {
                    Ok(())
                }
            },
            is_sharing_violation,
            |delay| pauses.push(delay),
        );

        assert!(result.is_ok(), "the rename succeeds on the fourth attempt");
        assert_eq!(attempts, 4);
        assert_eq!(
            pauses,
            [
                Duration::from_millis(50),
                Duration::from_millis(100),
                Duration::from_millis(200),
            ]
        );
    }

    #[test]
    fn other_rename_errors_fail_at_once() {
        const NO_SUCH_FILE: i32 = 2;
        let mut attempts = 0;
        let mut pauses = Vec::new();
        let result = retry_rename(
            || {
                attempts += 1;
                Err(io::Error::from_raw_os_error(NO_SUCH_FILE))
            },
            is_sharing_violation,
            |delay| pauses.push(delay),
        );

        assert_eq!(attempts, 1, "a non-conflict error is not retried");
        assert!(pauses.is_empty(), "no pause precedes the error");
        assert_eq!(
            result.expect_err("the error is returned").raw_os_error(),
            Some(NO_SUCH_FILE)
        );
    }
}
