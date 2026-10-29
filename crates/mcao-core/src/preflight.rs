//! Windows file-system preflight rules (DPES-888): the path-length budget, the OneDrive home rule,
//! and symbolic-link support.
//!
//! The path-length and OneDrive rules are pure functions over values their caller has already
//! read from the host, so they run on every platform. Reading the host values, such as the
//! `LongPathsEnabled` registry value and an entry's cloud reparse tag, belongs to the caller.
//! [`check_symlink_support`] reads the host and git itself.

mod symlink;

use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf, Prefix};

use crate::error::McaoError;
use crate::state::TaskId;

pub use symlink::{SymlinkSupport, check_symlink_support};

/// The classic `MAX_PATH` limit, in UTF-16 code units.
pub const CLASSIC_PATH_LIMIT: usize = 260;

/// The extended-length limit, in UTF-16 code units. Only mcao's own file operations honor it.
pub const EXTENDED_PATH_LIMIT: usize = 32_767;

/// Path length above which tools that do not opt in to long paths, such as node and npm, may fail.
pub const TOOL_WARNING_LENGTH: usize = 240;

/// Environment variables that name a OneDrive root on Windows.
pub const ONEDRIVE_ENV_VARS: [&str; 3] = ["OneDrive", "OneDriveCommercial", "OneDriveConsumer"];

/// The path-length ceiling in force on a Windows host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathLimit {
    /// `MAX_PATH`. Used unless long paths are enabled for both the machine and git.
    Classic,
    /// The extended limit, which only mcao's own file operations honor.
    Extended,
}

impl PathLimit {
    /// Selects the limit from the two settings that gate long paths: the machine's
    /// `LongPathsEnabled` value and git's `core.longpaths`. Either one off keeps the classic limit.
    pub fn select(long_paths_enabled: bool, git_long_paths: bool) -> Self {
        if long_paths_enabled && git_long_paths {
            Self::Extended
        } else {
            Self::Classic
        }
    }

    /// The limit in UTF-16 code units, counting the terminating NUL. A path must be shorter.
    pub const fn max_len(self) -> usize {
        match self {
            Self::Classic => CLASSIC_PATH_LIMIT,
            Self::Extended => EXTENDED_PATH_LIMIT,
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Classic => "classic",
            Self::Extended => "extended",
        }
    }

    const fn remedy(self) -> &'static str {
        match self {
            Self::Classic => {
                "enable LongPathsEnabled (requires an administrator) and git core.longpaths, or \
                 move the operational home to a shorter path"
            }
            Self::Extended => "move the operational home or shorten the task id",
        }
    }
}

/// Returns the length, in UTF-16 code units, of a file operation under `home` for task
/// `task_id`: the home, a separator, the task id, a separator, and `repo_suffix_units` more units
/// below the task directory.
///
/// Windows counts path length in UTF-16 code units, so a character outside the Basic Multilingual
/// Plane counts as two. On Windows the home is measured with its exact wide-unit count.
///
/// The count excludes the terminating NUL. [`check_path_len`] accounts for it.
pub fn projected_path_len(home: &Path, task_id: &TaskId, repo_suffix_units: usize) -> usize {
    // Task ids are ASCII by construction, so their byte length is their unit count.
    utf16_len(home.as_os_str()) + 1 + task_id.as_str().len() + 1 + repo_suffix_units
}

fn utf16_len(text: &OsStr) -> usize {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        text.encode_wide().count()
    }
    #[cfg(not(windows))]
    {
        text.to_string_lossy().encode_utf16().count()
    }
}

/// A path length that passed [`check_path_len`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathCheck {
    /// The path is short enough for tools that do not opt in to long paths.
    Within,
    /// The path fits the limit but is longer than [`TOOL_WARNING_LENGTH`], so tools that do not
    /// opt in to long paths may fail on it.
    ToolsMayFail,
}

/// Checks a projected path length, which excludes the terminating NUL, against `limit`. The limit
/// counts that NUL, so a path of exactly `limit.max_len()` units is refused.
///
/// Microsoft's classic `MAX_PATH` text counts the NUL explicitly. Its extended-length text gives
/// 32,767 characters without naming the NUL, so the same stricter reading applies to both.
///
/// # Errors
///
/// Returns [`McaoError::Preflight`] with the remedy for `limit` when `len` reaches it. Nothing may
/// be created under such a path.
pub fn check_path_len(len: usize, limit: PathLimit) -> Result<PathCheck, McaoError> {
    if len >= limit.max_len() {
        return Err(McaoError::Preflight(format!(
            "path of {len} UTF-16 units must be under the {} limit of {}, which counts the \
             terminating NUL; {}",
            limit.name(),
            limit.max_len(),
            limit.remedy()
        )));
    }
    Ok(if len > TOOL_WARNING_LENGTH {
        PathCheck::ToolsMayFail
    } else {
        PathCheck::Within
    })
}

/// Refuses an operational home that lies under a OneDrive root.
///
/// `env` reads an environment variable, so a test can supply its own. Each name in
/// [`ONEDRIVE_ENV_VARS`] that is set and non-empty names a root. A root matches when it is the home
/// or one of the home's ancestors, compared by whole components, so `OneDriveBackup` is not under
/// `OneDrive`. Components compare ASCII-case-insensitively, as Windows names do.
///
/// # Input contract
///
/// The caller resolves the home before the call: an absolute path with `..` removed, with symbolic
/// links, junctions, and 8.3 short names resolved, and with any verbatim (`\\?\`) prefix stripped.
/// The comparison is textual and reads no file system state, so it cannot follow a link itself. A
/// home that is relative, contains `..`, or carries a verbatim or device prefix is refused rather
/// than compared, because a textual match on such a form can pass a home that lies under a root.
/// A non-ASCII folder name that differs from its root only by case is not matched.
///
/// The cloud reparse-tag check that DPES-888 also requires is not implemented. It is a remaining
/// gap: a home in a cloud-synced folder that no environment root names is not refused.
///
/// # Errors
///
/// Returns [`McaoError::Preflight`] with a remedy when `home` is under one of the roots, or when a
/// root is set and `home` is not in a form that can be compared.
pub fn check_home_outside_onedrive(
    home: &Path,
    env: impl Fn(&str) -> Option<OsString>,
) -> Result<(), McaoError> {
    for name in ONEDRIVE_ENV_VARS {
        // An empty root has no components, and every path starts with it, so it must not count.
        let Some(value) = env(name).filter(|value| !value.is_empty()) else {
            continue;
        };
        if !is_comparable(home) {
            return Err(McaoError::Preflight(format!(
                "operational home {} cannot be checked against the OneDrive folder named by \
                 {name}; pass an absolute path with no `..` components and no verbatim \
                 (extended-length) prefix",
                home.display()
            )));
        }
        let root = PathBuf::from(value);
        if is_under(home, &root) {
            return Err(McaoError::Preflight(format!(
                "operational home {} is under the OneDrive folder {} named by {name}; set FM_HOME \
                 outside that folder or stop syncing it",
                home.display(),
                root.display()
            )));
        }
    }
    Ok(())
}

/// Reports whether `home` is in a form [`is_under`] compares soundly: absolute, free of `..`, and
/// without a verbatim or device prefix. A disk or UNC prefix is compared as written.
fn is_comparable(home: &Path) -> bool {
    home.is_absolute()
        && home.components().all(|part| match part {
            Component::ParentDir => false,
            Component::Prefix(prefix) => {
                matches!(prefix.kind(), Prefix::Disk(_) | Prefix::UNC(..))
            }
            Component::RootDir | Component::CurDir | Component::Normal(_) => true,
        })
}

fn is_under(path: &Path, root: &Path) -> bool {
    let mut path_parts = path.components();
    root.components().all(|root_part| {
        path_parts
            .next()
            .is_some_and(|path_part| component_eq(path_part, root_part))
    })
}

fn component_eq(a: Component<'_>, b: Component<'_>) -> bool {
    let (a, b) = (
        a.as_os_str().to_string_lossy(),
        b.as_os_str().to_string_lossy(),
    );
    a.eq_ignore_ascii_case(&b)
}
