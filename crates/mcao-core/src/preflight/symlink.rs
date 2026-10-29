//! Symbolic-link support for a git worktree (DPES-888 AC3).
//!
//! The check reports three facts and leaves the policy to its caller: whether the host can create
//! a symbolic link in the directory, what git's `core.symlinks` setting says, and which tracked
//! symbolic links are checked out as plain placeholder files. Windows creates symbolic links only
//! with Developer Mode or the `SeCreateSymbolicLinkPrivilege` right, so the host fact comes from a
//! probe rather than from a registry read.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::McaoError;

/// Git's file mode for a symbolic link in the index.
const SYMLINK_MODE: &str = "120000 ";

/// What the host and git report about symbolic links in one worktree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SymlinkSupport {
    /// Whether the host created a symbolic link in the checked directory.
    pub system_creates_symlinks: bool,
    /// Git's `core.symlinks`, or `None` when the setting is unset.
    pub git_core_symlinks: Option<bool>,
    /// Tracked symbolic links that the working tree holds as regular files. Git writes a link
    /// as its target text when `core.symlinks` is false, so these entries are placeholders. Each
    /// path is the checked directory joined with the tracked path.
    pub placeholders: Vec<PathBuf>,
}

/// Reports the host's and git's symbolic-link support for the worktree at `dir`.
///
/// `dir` must be inside a git worktree. The probe creates and removes one link in `dir`, and the
/// git queries read the repository's configuration and index without changing them.
///
/// # Errors
///
/// Returns [`McaoError::Io`] when `git` cannot run or a working-tree entry cannot be read, and
/// [`McaoError::Preflight`] when git refuses the directory, for example because it is not a
/// repository, or prints a value this check does not recognise.
pub fn check_symlink_support(dir: &Path) -> Result<SymlinkSupport, McaoError> {
    Ok(SymlinkSupport {
        system_creates_symlinks: probe_symlink(dir)?,
        git_core_symlinks: git_core_symlinks(dir)?,
        placeholders: placeholders(dir)?,
    })
}

static PROBE_SERIAL: AtomicU64 = AtomicU64::new(0);

/// Creates and then removes one symbolic link in `dir`. Returns `false` when the host refuses to
/// create the link, whether for want of the privilege or because the file system has no links.
fn probe_symlink(dir: &Path) -> io::Result<bool> {
    let serial = PROBE_SERIAL.fetch_add(1, Ordering::Relaxed);
    let link = dir.join(format!(
        ".mcao-symlink-probe-{}-{serial}",
        std::process::id()
    ));
    match create_symlink(Path::new("mcao-symlink-probe-target"), &link) {
        Ok(()) => {
            fs::remove_file(&link)?;
            Ok(true)
        }
        Err(_) => Ok(false),
    }
}

#[cfg(unix)]
fn create_symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn create_symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::windows::fs::symlink_file(target, link)
}

#[cfg(not(any(unix, windows)))]
fn create_symlink(_target: &Path, _link: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "this platform has no symbolic links",
    ))
}

/// Reads `core.symlinks` as a boolean, or `None` when git reports the key unset.
fn git_core_symlinks(dir: &Path) -> Result<Option<bool>, McaoError> {
    let output = git(dir, &["config", "--get", "--type=bool", "core.symlinks"])?;
    // `git config --get` exits with 1 when the key is unset. Any other failure is a real error.
    if output.status.code() == Some(1) {
        return Ok(None);
    }
    require_success(&output, "read core.symlinks")?;
    match stdout_text(&output)?.trim() {
        "true" => Ok(Some(true)),
        "false" => Ok(Some(false)),
        other => Err(McaoError::Preflight(format!(
            "core.symlinks has an unrecognised value {other:?}"
        ))),
    }
}

/// Lists tracked symbolic links whose working-tree entry is a regular file.
///
/// A link deleted from the working tree is not a placeholder, so it is skipped.
fn placeholders(dir: &Path) -> Result<Vec<PathBuf>, McaoError> {
    let output = git(dir, &["ls-files", "-s", "-z"])?;
    require_success(&output, "list tracked files")?;
    let listing = stdout_text(&output)?;

    let mut found = Vec::new();
    // Each NUL-terminated entry reads `<mode> <object> <stage>\t<path>`.
    for entry in listing.split('\0').filter(|entry| !entry.is_empty()) {
        let Some((info, tracked)) = entry.split_once('\t') else {
            return Err(McaoError::Preflight(format!(
                "unexpected index listing entry {entry:?}"
            )));
        };
        if !info.starts_with(SYMLINK_MODE) {
            continue;
        }
        let path = dir.join(tracked);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() => found.push(path),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(McaoError::Io(error)),
        }
    }
    Ok(found)
}

fn git(dir: &Path, args: &[&str]) -> Result<Output, McaoError> {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(McaoError::Io)
}

/// Turns a failed git query into a preflight refusal that quotes git's own message.
fn require_success(output: &Output, action: &str) -> Result<(), McaoError> {
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(McaoError::Preflight(format!(
        "cannot {action}: {}",
        stderr.trim()
    )))
}

fn stdout_text(output: &Output) -> Result<&str, McaoError> {
    std::str::from_utf8(&output.stdout)
        .map_err(|_| McaoError::Preflight("git printed output that is not UTF-8".to_owned()))
}
