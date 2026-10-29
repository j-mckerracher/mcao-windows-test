//! The sidecar lock that serializes Rust writers of one `state/` file.
//!
//! A sidecar is the guarded path with `.lock` appended, in the same directory. It is created when
//! missing and never truncated, read, or removed, so only the lock on it matters. Every Rust
//! writer of `state/<id>.meta` takes the record's sidecar, and so does every appender of a status
//! log, so a read-merge-publish cannot interleave with another writer of the same file.
//!
//! The shell's `<state>/.meta-<id>.lock` directory lock is a different protocol and a different
//! file. This module does not exclude a shell writer.

use std::fs;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use crate::error::McaoError;

use super::id::TaskId;

/// A held sidecar lock. Dropping it releases the lock, so bind it for the whole read-merge-publish
/// it guards; a bare `lock_meta(...)?;` statement releases the lock at once.
#[must_use = "the lock is released as soon as the guard is dropped"]
pub(crate) struct SidecarLock {
    _file: File,
}

/// Takes the exclusive lock on the sidecar of `state_dir/<id>.meta`, blocking until it is granted.
///
/// # Errors
///
/// Returns [`McaoError::Uncertain`] when the sidecar is a symlink or another non-regular file, and
/// [`McaoError::Io`] when it cannot be opened or locked.
pub(crate) fn lock_meta(state_dir: &Path, id: &TaskId) -> Result<SidecarLock, McaoError> {
    lock_sidecar(&state_dir.join(format!("{}.meta", id.as_str())))
}

/// Takes the exclusive lock on the sidecar of `guarded`, blocking until it is granted.
///
/// # Errors
///
/// Returns [`McaoError::Uncertain`] when the sidecar is a symlink or another non-regular file, and
/// [`McaoError::Io`] when it cannot be opened or locked.
pub(crate) fn lock_sidecar(guarded: &Path) -> Result<SidecarLock, McaoError> {
    let sidecar = sidecar_path(guarded);
    refuse_non_regular(&sidecar)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&sidecar)?;
    file.lock()?;
    Ok(SidecarLock { _file: file })
}

/// The sidecar path: `guarded` with `.lock` appended.
fn sidecar_path(guarded: &Path) -> PathBuf {
    let mut name = guarded.as_os_str().to_owned();
    name.push(".lock");
    PathBuf::from(name)
}

/// Refuses a symlink, or any other non-regular file, at `path`. A missing path is allowed, because
/// it holds no data and an append or publish creates it. The check uses `symlink_metadata`, so a
/// symlink is refused even when its target is a regular file or does not exist.
pub(crate) fn refuse_non_regular(path: &Path) -> Result<(), McaoError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => Err(
            McaoError::Uncertain(format!("{} is not a regular file", path.display())),
        ),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}
