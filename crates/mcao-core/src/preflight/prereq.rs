//! Prerequisite checks for `mcao preflight` (DPES-754): Git and the GitHub CLI on `PATH`, and the
//! path-length budget of an operational home.
//!
//! A lookup reads directory entries and file modes only. It never runs the file it finds, so the
//! check makes no `execve` or `CreateProcess` call. `PATH` is passed in, so a caller or a test
//! chooses it.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use super::{PathCheck, PathLimit, check_path_len, projected_path_len};
use crate::error::McaoError;
use crate::state::TaskId;
use crate::state::id::MAX_CREATION_LEN;

/// The remedy the Git check names when Git is not on `PATH`.
pub const GIT_REMEDY: &str = "install Git from https://git-scm.com and open a new terminal";

/// The remedy the GitHub CLI check names when `gh` is not on `PATH`.
pub const GH_REMEDY: &str =
    "install the GitHub CLI from https://cli.github.com and open a new terminal";

/// Windows extensions that may be started directly, in the order they are tried. This matches
/// `toolchain::discovery`, so a `.cmd`, `.bat`, or `.ps1` shim is never runnable.
#[cfg(windows)]
const WINDOWS_EXE_EXTENSIONS: [&str; 2] = ["exe", "com"];

/// Checks that Git can be found on `PATH`, returning the file that would run.
///
/// The message names the failure and its remedy. The caller adds the check's name in front. A
/// file that is on `PATH` but cannot be run is reported as `NOT EXECUTABLE`, which is not the same
/// as `MISSING`.
///
/// # Errors
///
/// Returns [`McaoError::Preflight`] when no runnable `git` is found in `path`.
pub fn check_git(path: Option<&OsStr>) -> Result<PathBuf, McaoError> {
    check_tool("git", GIT_REMEDY, path)
}

/// Checks that the GitHub CLI (`gh`) can be found on `PATH`, returning the file that would run.
///
/// Like [`check_git`], it never runs `gh`, reads no credentials, and makes no network call, so it
/// holds offline and before any login.
///
/// # Errors
///
/// Returns [`McaoError::Preflight`] when no runnable `gh` is found in `path`.
pub fn check_gh(path: Option<&OsStr>) -> Result<PathBuf, McaoError> {
    check_tool("gh", GH_REMEDY, path)
}

/// Checks that the tool `name` can be found on `PATH`, returning the file that would run. The
/// message names the failure and carries `remedy`.
fn check_tool(name: &str, remedy: &str, path: Option<&OsStr>) -> Result<PathBuf, McaoError> {
    match find_executable(name, path) {
        Found::Runnable(tool) => Ok(tool),
        Found::NotExecutable(file) => Err(McaoError::Preflight(format!(
            "NOT EXECUTABLE ({} is on PATH but the OS will not run it); {remedy}",
            file.display()
        ))),
        Found::Missing => Err(McaoError::Preflight(format!(
            "MISSING (not found on PATH); {remedy}"
        ))),
    }
}

/// Checks that `home` leaves room under `limit` for the longest task directory that creation
/// admits. The projection counts the home, a separator, and that task id, as
/// [`projected_path_len`] does. Files below the task directory add more, so this is a floor.
///
/// # Errors
///
/// Returns [`McaoError::Preflight`] when the projected path reaches `limit`.
pub fn check_home_budget(home: &Path, limit: PathLimit) -> Result<PathCheck, McaoError> {
    let longest = TaskId::parse_for_creation(&"a".repeat(MAX_CREATION_LEN))?;
    check_path_len(projected_path_len(home, &longest, 0), limit)
}

/// The result of looking up a tool name on `PATH`.
#[derive(Debug, PartialEq, Eq)]
enum Found {
    /// The first runnable file of that name, in `PATH` order.
    Runnable(PathBuf),
    /// A file of that name is on `PATH`, but the OS would not run it.
    NotExecutable(PathBuf),
    /// No file of that name is on `PATH`.
    Missing,
}

/// Looks up `name` in the `PATH` entries, in order.
///
/// Only absolute entries are searched. An empty entry, `.`, or any other relative entry names the
/// working directory, so each one is skipped. On Unix a file must pass `access(X_OK)`. On Windows
/// only a `.exe` or `.com` file counts, and a script shim never does.
fn find_executable(name: &str, path: Option<&OsStr>) -> Found {
    let Some(path) = path else {
        return Found::Missing;
    };
    let mut blocked = None;
    for dir in std::env::split_paths(path).filter(|dir| dir.is_absolute()) {
        for candidate in candidates(&dir, name) {
            if !candidate.is_file() {
                continue;
            }
            if is_executable(&candidate) {
                return Found::Runnable(candidate);
            }
            blocked.get_or_insert(candidate);
        }
    }
    blocked.map_or(Found::Missing, Found::NotExecutable)
}

#[cfg(unix)]
fn candidates(dir: &Path, name: &str) -> impl Iterator<Item = PathBuf> {
    std::iter::once(dir.join(name))
}

#[cfg(windows)]
fn candidates(dir: &Path, name: &str) -> impl Iterator<Item = PathBuf> {
    WINDOWS_EXE_EXTENSIONS
        .into_iter()
        .map(move |ext| dir.join(format!("{name}.{ext}")))
}

/// True when the OS would run `path`. On Windows the candidates are already limited to `.exe` and
/// `.com` files, so every file that reaches this check counts.
#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::ffi::{CString, c_char, c_int};
    use std::os::unix::ffi::OsStrExt;

    /// `X_OK` from `<unistd.h>`, which has the same value on macOS and Linux.
    const X_OK: c_int = 1;

    unsafe extern "C" {
        fn access(path: *const c_char, mode: c_int) -> c_int;
    }

    let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `path` is a NUL-terminated string that outlives the call, and `access` only reads it.
    unsafe { access(path.as_ptr(), X_OK) == 0 }
}

#[cfg(not(unix))]
fn is_executable(_path: &Path) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::Path;

    use super::*;

    #[test]
    fn preflight_git_is_missing_without_a_path_and_names_its_remedy() {
        for path in [None, Some(OsStr::new(""))] {
            let error = check_git(path).expect_err("no git is on PATH");
            let message = error.to_string();
            assert!(message.contains("MISSING"), "{message}");
            assert!(message.contains("https://git-scm.com"), "{message}");
            assert_eq!(error.exit_code(), 3);
        }
    }

    #[test]
    fn preflight_gh_is_missing_without_a_path_and_names_its_remedy() {
        for path in [None, Some(OsStr::new(""))] {
            let error = check_gh(path).expect_err("no gh is on PATH");
            let message = error.to_string();
            assert!(message.contains("MISSING"), "{message}");
            assert!(message.contains("https://cli.github.com"), "{message}");
            assert_eq!(error.exit_code(), 3);
        }
    }

    #[test]
    fn preflight_home_budget_counts_the_longest_task_directory() {
        // A home of 193 units plus a separator, a 64-unit task id, and a separator is 259 units,
        // which is under the classic limit of 260 and past the tool warning length.
        let fits = format!("/{}", "a".repeat(192));
        assert_eq!(
            check_home_budget(Path::new(&fits), PathLimit::Classic).expect("259 is under 260"),
            PathCheck::ToolsMayFail
        );
        // One more unit reaches 260, which the limit refuses because it counts the NUL.
        let too_long = format!("/{}", "a".repeat(193));
        assert!(check_home_budget(Path::new(&too_long), PathLimit::Classic).is_err());
    }
}
