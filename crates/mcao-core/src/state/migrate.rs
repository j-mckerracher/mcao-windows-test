//! Migration of legacy `state/<id>.meta` records to the current encoding, and the way back.
//!
//! A legacy record is a version 1 record: it carries no `format` key, which is every record the
//! shell writes. Migration stamps it with the current encoding, `format=2` and a `path_style`
//! line, as [`stamp_encoding`] builds it. Every other pair is kept, in its order, with its value,
//! so approval and proof keys such as `decision_keys` and the `*_gen` markers survive. Unknown
//! keys are ordinary pairs and survive the same way. A repeated key stays repeated.
//!
//! # Preconditions
//!
//! A home belongs to one operating system, so `style` is the path style of the system that wrote
//! the records. Mixed-OS homes are out of scope. Migration is an offline operation: run it while
//! no other writer touches the state directory. A writer that appends to a record between this
//! module's last read and its rename is detected and reported as [`McaoError::StateConflict`],
//! but the window between that check and the rename is not closed.
//!
//! # Proof before write
//!
//! Re-encoding rewrites line endings, so a record is replaced only after its stamped form is
//! shown to hold the same pairs as the original. A record that would change a value, such as a
//! final value with no line break that ends in `\r`, is refused and left as it is.
//!
//! # Rollback
//!
//! [`rollback_meta`] removes every `format` and `path_style` line and keeps every other line as
//! it is. It edits the current record instead of restoring a saved copy, so a pair a writer added
//! after the migration is kept. Rollback does not restore a line ending or byte order mark that
//! migration removed, and no reader of the shell treats those as part of a value. A record that
//! only a version 2 reader can use is refused: one that names the `windows` backend, or whose
//! `path_style` is `windows`.
//!
//! Both operations are idempotent. Migrating a current record and rolling back a record with
//! neither key change nothing.
//!
//! # Crash points
//!
//! | The process stops | What a reader finds | The next run |
//! | --- | --- | --- |
//! | before the temporary file exists | the original record | migrates |
//! | after the temporary file, before the rename | the original record, and an inert temporary file that no reader treats as a record | migrates; the temporary file stays for later cleanup |
//! | after the rename, before the directory sync | the new record, or the original if the power fails | converges either way |
//! | after the rename | the new record | reports the record already current |

use std::cell::Cell;
use std::fs;
use std::io;
use std::path::Path;

use crate::error::McaoError;

use super::encoding::{
    BackendToken, FORMAT_KEY, FormatVersion, PATH_STYLE_KEY, PathStyle, stamp_encoding, strip_bom,
};
use super::id::TaskId;
use super::lock::{SidecarLock, lock_meta};
use super::meta::TaskMeta;
use super::store::publish_with;

/// What [`migrate_meta`] did to one record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MigrateOutcome {
    /// The record was legacy and is now stamped with the current encoding.
    Migrated,
    /// The record already carried the current encoding, so nothing was written.
    AlreadyCurrent,
}

/// What [`rollback_meta`] did to one record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RollbackOutcome {
    /// The `format` and `path_style` lines were removed.
    RolledBack,
    /// The record carried neither key, so nothing was written.
    AlreadyLegacy,
}

/// The result for each record of a directory, as `(file stem, result)`, sorted by stem.
pub type RecordResults<T> = Vec<(String, Result<T, McaoError>)>;

/// Migrates `state_dir/<id>.meta` from a legacy record to the current encoding.
///
/// See the module documentation for the preconditions, the proof, and the crash points. On any
/// failure the original record stays in place and readable.
///
/// # Errors
///
/// Returns [`McaoError::Refused`] when the record is not a regular file, is not UTF-8, names a
/// `format` this reader does not know, names a `path_style` other than `style`, or would change a
/// value when re-encoded. Returns [`McaoError::StateConflict`] when the record changed while it
/// was being rewritten, and [`McaoError::Io`] when it cannot be read or published. The final
/// directory sync can also fail after the record was replaced, and the next run then reports
/// [`MigrateOutcome::AlreadyCurrent`].
pub fn migrate_meta(
    state_dir: &Path,
    id: &TaskId,
    style: PathStyle,
) -> Result<MigrateOutcome, McaoError> {
    // The sidecar lock covers the read, the checks, and the rename, so no other Rust writer of the
    // record can land between them.
    let _lock = lock_existing_record(state_dir, id)?;
    migrate_with(state_dir, id, style, || Ok(()))
}

/// Migrates like [`migrate_meta`], calling `before_rename` after the stamped record is written to
/// its temporary file and the record is checked unchanged, and before the rename. A test uses it
/// to stop or to interfere at that point.
fn migrate_with(
    state_dir: &Path,
    id: &TaskId,
    style: PathStyle,
    before_rename: impl FnOnce() -> io::Result<()>,
) -> Result<MigrateOutcome, McaoError> {
    let original = read_record(state_dir, id)?;
    let meta = TaskMeta::parse(id.clone(), &original);
    let refuse = |reason: String| refused(id, reason);
    match meta
        .format_version()
        .map_err(|error| refuse(error.to_string()))?
    {
        FormatVersion::V2 => return Ok(MigrateOutcome::AlreadyCurrent),
        FormatVersion::V1 => {}
    }
    let stamped = stamp_encoding(&original, style).map_err(|error| refuse(error.to_string()))?;
    if content_pairs(&meta) != content_pairs(&TaskMeta::parse(id.clone(), &stamped)) {
        return Err(refuse(
            "re-encoding would change a value, so the record is left as it is".to_owned(),
        ));
    }
    replace_record(state_dir, id, &original, &stamped, before_rename)?;
    Ok(MigrateOutcome::Migrated)
}

/// Rolls `state_dir/<id>.meta` back to the legacy encoding by removing its `format` and
/// `path_style` lines.
///
/// See the module documentation for what rollback keeps and refuses.
///
/// # Errors
///
/// Returns [`McaoError::Refused`] when the record is not a regular file, is not UTF-8, names a
/// `format` this reader does not know, or can only be read as version 2: it names the `windows`
/// backend, or its `path_style` is `windows` or unknown. Returns [`McaoError::StateConflict`] when
/// the record changed while it was being rewritten, and [`McaoError::Io`] when it cannot be read
/// or published.
pub fn rollback_meta(state_dir: &Path, id: &TaskId) -> Result<RollbackOutcome, McaoError> {
    // Held from the read to the rename, as in `migrate_meta`.
    let _lock = lock_existing_record(state_dir, id)?;
    let original = read_record(state_dir, id)?;
    let meta = TaskMeta::parse(id.clone(), &original);
    let refuse = |reason: String| refused(id, reason);
    meta.format_version()
        .map_err(|error| refuse(error.to_string()))?;
    match meta.path_style() {
        Ok(Some(PathStyle::Windows)) => {
            return Err(refuse(
                "its paths are in Windows form, which version 1 readers cannot use".to_owned(),
            ));
        }
        Ok(Some(PathStyle::Posix) | None) => {}
        Err(error) => return Err(refuse(error.to_string())),
    }
    if meta.backend() == Some(BackendToken::Windows.as_str()) {
        return Err(refuse(
            "its backend is windows, which version 1 readers refuse".to_owned(),
        ));
    }
    let Some(legacy) = remove_encoding_lines(&original) else {
        return Ok(RollbackOutcome::AlreadyLegacy);
    };
    replace_record(state_dir, id, &original, &legacy, || Ok(()))?;
    Ok(RollbackOutcome::RolledBack)
}

/// Migrates every record in `state_dir`, each on its own.
///
/// A failure on one record does not stop the others. Hidden entries, which include temporary
/// files, and entries that do not end in `.meta` are not records. A `.meta` entry whose stem is not
/// a task id is reported as refused.
///
/// # Errors
///
/// Returns [`McaoError::Io`] only when the directory cannot be listed.
pub fn migrate_dir(
    state_dir: &Path,
    style: PathStyle,
) -> Result<RecordResults<MigrateOutcome>, McaoError> {
    each_record(state_dir, |id| migrate_meta(state_dir, id, style))
}

/// Rolls back every record in `state_dir`, each on its own, as [`migrate_dir`] visits them.
///
/// # Errors
///
/// Returns [`McaoError::Io`] only when the directory cannot be listed.
pub fn rollback_dir(state_dir: &Path) -> Result<RecordResults<RollbackOutcome>, McaoError> {
    each_record(state_dir, |id| rollback_meta(state_dir, id))
}

fn each_record<T>(
    state_dir: &Path,
    operate: impl Fn(&TaskId) -> Result<T, McaoError>,
) -> Result<RecordResults<T>, McaoError> {
    let mut stems = Vec::new();
    for entry in fs::read_dir(state_dir)? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if let Some(stem) = name.strip_suffix(".meta")
            && !name.starts_with('.')
        {
            stems.push(stem.to_owned());
        }
    }
    stems.sort();
    Ok(stems
        .into_iter()
        .map(|stem| {
            let result = match TaskId::parse(&stem) {
                Ok(id) => operate(&id),
                Err(error) => Err(McaoError::Refused(format!("record {stem}.meta: {error}"))),
            };
            (stem, result)
        })
        .collect())
}

fn refused(id: &TaskId, reason: String) -> McaoError {
    McaoError::Refused(format!("task {}: {reason}", id.as_str()))
}

/// Takes the record's sidecar lock once the record is known to exist, so a call on a missing record
/// is refused before any lock file is created.
fn lock_existing_record(state_dir: &Path, id: &TaskId) -> Result<SidecarLock, McaoError> {
    fs::symlink_metadata(state_dir.join(format!("{}.meta", id.as_str())))?;
    lock_meta(state_dir, id)
}

/// Reads the record as UTF-8 text. A symlink or a non-regular file is refused, because a rename
/// over it would replace the link or fail, not edit the record it points to.
fn read_record(state_dir: &Path, id: &TaskId) -> Result<String, McaoError> {
    let path = state_dir.join(format!("{}.meta", id.as_str()));
    if !fs::symlink_metadata(&path)?.is_file() {
        return Err(refused(id, "the record is not a regular file".to_owned()));
    }
    String::from_utf8(fs::read(&path)?)
        .map_err(|_| refused(id, "the record is not valid UTF-8".to_owned()))
}

/// Replaces the record with `new`, provided it still holds `original` when the replacement is
/// ready. `before_rename` runs after that check.
fn replace_record(
    state_dir: &Path,
    id: &TaskId,
    original: &str,
    new: &str,
    before_rename: impl FnOnce() -> io::Result<()>,
) -> Result<(), McaoError> {
    let path = state_dir.join(format!("{}.meta", id.as_str()));
    let changed = Cell::new(false);
    let result = publish_with(state_dir, id, new, || {
        if fs::read(&path)? != original.as_bytes() {
            changed.set(true);
            return Err(io::Error::other("record changed before the rename"));
        }
        before_rename()
    });
    match result {
        Err(McaoError::Io(_)) if changed.get() => Err(McaoError::StateConflict(format!(
            "task {}: the record changed while it was being rewritten, and nothing was replaced",
            id.as_str()
        ))),
        other => other,
    }
}

/// Returns the pairs of `meta` that migration must keep: everything but the encoding keys.
fn content_pairs(meta: &TaskMeta) -> Vec<(&str, &str)> {
    meta.pairs()
        .filter(|(key, _)| *key != FORMAT_KEY && *key != PATH_STYLE_KEY)
        .collect()
}

/// Returns `text` without its `format` and `path_style` lines, or `None` when it has none. Every
/// other line is kept as it is. A leading byte order mark goes with the lines, so the first key
/// still matches.
fn remove_encoding_lines(text: &str) -> Option<String> {
    let mut removed = false;
    let kept: String = strip_bom(text)
        .split_inclusive('\n')
        .filter(|line| {
            let encoding_line = line
                .split_once('=')
                .is_some_and(|(key, _)| key == FORMAT_KEY || key == PATH_STYLE_KEY);
            removed |= encoding_line;
            !encoding_line
        })
        .collect();
    removed.then_some(kept)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Removes its directory on drop, so a failed assertion leaves nothing behind.
    struct ScratchDir(std::path::PathBuf);

    impl ScratchDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("mcao-migrate-unit-{label}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("scratch dir is created");
            Self(path)
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .expect("scratch dir is listable")
            .map(|entry| {
                entry
                    .expect("directory entry is readable")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }

    #[test]
    fn failure_before_the_rename_keeps_the_original_and_leaves_no_temporary_file() {
        let scratch = ScratchDir::new("fail");
        let id = TaskId::parse("task-h").expect("test task id is valid");
        let original = "harness=claude\nkind=ship\n";
        fs::write(scratch.0.join("task-h.meta"), original).expect("seed record");

        let result = migrate_with(&scratch.0, &id, PathStyle::Posix, || {
            Err(io::Error::other("injected failure before the rename"))
        });

        assert!(
            matches!(result, Err(McaoError::Io(_))),
            "injected failure is reported: {result:?}"
        );
        assert_eq!(
            fs::read_to_string(scratch.0.join("task-h.meta")).expect("record is readable"),
            original,
            "the original record is kept whole"
        );
        assert_eq!(names(&scratch.0), ["task-h.meta"]);
    }

    #[test]
    fn a_record_that_changed_since_it_was_read_is_a_state_conflict() {
        let scratch = ScratchDir::new("conflict-detected");
        let id = TaskId::parse("task-h").expect("test task id is valid");
        let record = scratch.0.join("task-h.meta");
        fs::write(&record, "harness=claude\n").expect("seed record");
        let appended = "harness=claude\ndecision_keys=k1\n";

        // The writer appends after the record was read and before the replacement is ready.
        let original = read_record(&scratch.0, &id).expect("record is readable");
        fs::write(&record, appended).expect("writer appends");
        let result = replace_record(
            &scratch.0,
            &id,
            &original,
            "format=2\npath_style=posix\nharness=claude\n",
            || Ok(()),
        );

        assert!(
            matches!(result, Err(McaoError::StateConflict(_))),
            "a changed record is a state conflict: {result:?}"
        );
        assert_eq!(
            fs::read_to_string(&record).expect("record is readable"),
            appended,
            "the writer's record is untouched"
        );
        assert_eq!(names(&scratch.0), ["task-h.meta"]);
    }

    fn is_not_found(error: &Option<McaoError>) -> bool {
        matches!(error, Some(McaoError::Io(error)) if error.kind() == io::ErrorKind::NotFound)
    }

    #[test]
    fn a_missing_record_is_refused_and_leaves_no_sidecar_behind() {
        let scratch = ScratchDir::new("missing");
        let id = TaskId::parse("task-m").expect("test task id is valid");

        let migrated = migrate_meta(&scratch.0, &id, PathStyle::Posix).err();
        let rolled_back = rollback_meta(&scratch.0, &id).err();

        assert!(
            is_not_found(&migrated) && is_not_found(&rolled_back),
            "{migrated:?} {rolled_back:?}"
        );
        assert_eq!(
            names(&scratch.0),
            Vec::<String>::new(),
            "a refused call must not create the sidecar lock"
        );
    }

    #[test]
    fn encoding_lines_are_removed_and_nothing_else() {
        assert_eq!(
            remove_encoding_lines("format=2\npath_style=posix\na=1\n\nb=2"),
            Some("a=1\n\nb=2".to_owned())
        );
        assert_eq!(remove_encoding_lines("a=1\nformat_x=2\n"), None);
    }
}
