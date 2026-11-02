//! Counts the agents holding a start slot on this host. Reads state records only: no lock, no
//! writes, no process.
//!
//! The rule is the one `fm_project_capacity_occupants` applies in `bin/fm-project-capacity-lib.sh`,
//! without its per-project filter. A record holds a slot when it is a task record, not a secondmate,
//! not a cloud (`gcp`) task, names a project, and has no `pr` handoff recorded.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::state::{TaskId, TaskMeta};

use super::SampleError;

/// Counts the agents holding a start slot across `dirs`, the state directories of local homes.
///
/// `dirs[0]` is the starting home. `own` is the task being admitted: its record in that home is
/// the one the start replaces, so it is not counted, and the same id in another home is counted.
/// A directory reached more than once, through a symlink or a repeated entry, is read once.
///
/// Only regular `*.meta` files are read, as the shell's `*.meta` glob reads them. Symlinks, other
/// non-file entries, and dot-prefixed names are skipped. A record removed after the directory was
/// listed holds no slot, so it is skipped too, as the shell loop skips it.
///
/// # Errors
///
/// Returns an error when `dirs` is empty, when a directory cannot be canonicalized or listed, when
/// a record cannot be inspected or read for any reason other than removal, or when a record's file
/// name is not UTF-8 or its stem is not a valid task id. A partial count would undercount holders,
/// so the count fails as a whole and the caller reports it as unknown rather than as zero.
pub fn count_running_agents(dirs: &[PathBuf], own: Option<&TaskId>) -> Result<u32, SampleError> {
    if dirs.is_empty() {
        return Err(SampleError::new("no state directory was supplied"));
    }
    let mut seen = HashSet::new();
    let mut count: u32 = 0;
    for (index, dir) in dirs.iter().enumerate() {
        let canonical = fs::canonicalize(dir).map_err(|e| io_error(dir, &e))?;
        if !seen.insert(canonical.clone()) {
            continue;
        }
        let entries = fs::read_dir(&canonical).map_err(|e| io_error(&canonical, &e))?;
        for entry in entries {
            let path = entry.map_err(|e| io_error(&canonical, &e))?.path();
            if !is_record_name(&path) {
                continue;
            }
            let Some(info) = present(fs::symlink_metadata(&path), &path)? else {
                continue;
            };
            if !info.is_file() {
                continue;
            }
            let id = task_id_of(&path)?;
            if index == 0 && own == Some(&id) {
                continue;
            }
            let Some(bytes) = present(fs::read(&path), &path)? else {
                continue;
            };
            // Read as the shell reads it: the bytes, with invalid UTF-8 replaced. Only the ASCII
            // keys compared below decide a slot, and a replaced byte never equals one of them.
            let text = String::from_utf8_lossy(&bytes);
            let meta = TaskMeta::parse(id, &text);
            if meta.kind() == Some("secondmate") {
                continue;
            }
            // The last `execution_target` line decides, as the shell's `fm_cloud_execution_target`
            // reads it. Only `gcp` is skipped. Any other value, an empty one, or no line counts.
            if meta.get("execution_target") == Some("gcp") {
                continue;
            }
            if meta.get("project").is_none_or(str::is_empty) {
                continue;
            }
            if meta.get("pr").is_some_and(|pr| !pr.is_empty()) {
                continue;
            }
            count = count
                .checked_add(1)
                .ok_or_else(|| SampleError::new("running-agent count exceeds u32"))?;
        }
    }
    Ok(count)
}

/// Whether a directory entry is a record name the shell's `*.meta` glob matches. The glob does not
/// match a dot-prefixed name, so `.hidden.meta` is not a record.
fn is_record_name(path: &Path) -> bool {
    path.extension() == Some(OsStr::new("meta"))
        && path
            .file_name()
            .is_some_and(|name| !name.to_string_lossy().starts_with('.'))
}

/// Maps the read of one record to `None` when the record was removed after the directory was
/// listed. Only `NotFound` on the record is skipped. Every other error stays fatal.
fn present<T>(result: io::Result<T>, path: &Path) -> Result<Option<T>, SampleError> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_error(path, &e)),
    }
}

/// The task id a `*.meta` file names by its stem. A stem that is not a valid id is an error, not a
/// skipped file, because the record may still hold a slot.
fn task_id_of(path: &Path) -> Result<TaskId, SampleError> {
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| SampleError::new(format!("{}: file name is not UTF-8", path.display())))?;
    TaskId::parse(stem).map_err(|e| SampleError::new(format!("{}: {e}", path.display())))
}

fn io_error(path: &Path, error: &io::Error) -> SampleError {
    SampleError::new(format!("{}: {error}", path.display()))
}
