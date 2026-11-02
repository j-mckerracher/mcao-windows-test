//! Windows file-system preflight rules (DPES-888): the path-length budget, the OneDrive home rule,
//! and symbolic-link support.
//!
//! The path-length and OneDrive rules are pure functions over values their caller has already
//! read from the host, so they run on every platform. Reading the host values, such as the
//! `LongPathsEnabled` registry value and an entry's cloud reparse tag, belongs to the caller.
//! [`check_symlink_support`] reads the host and git itself.

mod prereq;
mod symlink;

use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf, Prefix};

use crate::error::McaoError;
use crate::state::TaskId;

pub use prereq::{check_gh, check_git, check_home_budget};
pub use symlink::{SymlinkSupport, check_symlink_support};

/// The classic `MAX_PATH` limit, in UTF-16 code units.
pub const CLASSIC_PATH_LIMIT: usize = 260;

/// The extended-length limit, in UTF-16 code units. Only mcao's own file operations honor it.
pub const EXTENDED_PATH_LIMIT: usize = 32_767;

/// Path length above which tools that do not opt in to long paths, such as node and npm, may fail.
pub const TOOL_WARNING_LENGTH: usize = 240;

/// Environment variables that name a OneDrive root on Windows.
pub const ONEDRIVE_ENV_VARS: [&str; 3] = ["OneDrive", "OneDriveCommercial", "OneDriveConsumer"];

/// The relocation that a refused OneDrive home names: the Windows default home, which lies under
/// `%LOCALAPPDATA%` and is not synced by OneDrive.
pub const ONEDRIVE_RELOCATION: &str = r"%LOCALAPPDATA%\mcao\home";

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

/// Returns the default operational home on Windows, `%LOCALAPPDATA%\mcao\home`, built from the
/// injected `LOCALAPPDATA` value. Callers use it only when no `FM_HOME` is selected.
///
/// # Errors
///
/// Returns [`McaoError::Preflight`] when `LOCALAPPDATA` is unset, empty, or not an absolute path,
/// because no default home can be placed then. The remedy is to select a home explicitly with
/// `FM_HOME`.
pub fn default_home(env: impl Fn(&str) -> Option<OsString>) -> Result<PathBuf, McaoError> {
    let local = env("LOCALAPPDATA")
        .map(PathBuf::from)
        .filter(|local| local.is_absolute());
    let Some(local) = local else {
        return Err(McaoError::Preflight(
            "LOCALAPPDATA is not set to an absolute path, so the default home cannot be placed; \
             set FM_HOME to an absolute folder that is not synced"
                .to_owned(),
        ));
    };
    Ok(local.join("mcao").join("home"))
}

/// The name prefix that OneDrive gives a business sync folder, such as `OneDrive - Contoso`.
const ONEDRIVE_BUSINESS_FOLDER_PREFIX: &str = "OneDrive - ";

/// Why an operational home counts as OneDrive-synchronized.
#[derive(Debug, PartialEq, Eq)]
enum OneDriveMatch {
    /// The home lies under the root that the environment variable `name` names. This is positive
    /// evidence of synchronization, so it refuses.
    Root { name: &'static str, root: PathBuf },
    /// The home lies under a sync folder that sits directly beneath `%USERPROFILE%` and carries
    /// OneDrive's name. The name alone is not proof of synchronization, so it only warns.
    ProfileFolder { folder: PathBuf },
}

/// The outcome of the OneDrive rule for a home that it does not refuse.
#[derive(Debug, PartialEq, Eq)]
pub enum OneDriveCheck {
    /// No OneDrive signal names the home.
    Clear,
    /// The home lies under a sync folder beneath `%USERPROFILE%` that carries OneDrive's name, and
    /// no environment root names it. The home stays usable; the message gives the relocation.
    Warning(String),
}

/// Checks an operational home against OneDrive.
///
/// `env` reads an environment variable, so a test can supply its own. A home under a root that a
/// name in [`ONEDRIVE_ENV_VARS`] names is refused. A home under a sync folder directly beneath
/// `%USERPROFILE%` that OneDrive names, `OneDrive` or `OneDrive - <organization>`, is not refused by
/// that name alone: the result is [`OneDriveCheck::Warning`]. A root match on the same home refuses.
/// Matching is by whole components, so `OneDriveBackup` is not `OneDrive`. Windows-form names
/// compare ASCII-case-insensitively; unix names compare exactly. No reparse point is classified as
/// OneDrive by itself.
///
/// # Input contract
///
/// The home and `USERPROFILE` must be absolute. Before comparison, a verbatim (`\\?\`) prefix is
/// replaced by the plain form it names, and `..` components are removed lexically, so `C:\a\..\b`
/// compares as `C:\b`. Symbolic links, junctions, and 8.3 short names are not resolved here, and the
/// comparison reads no file system state. A home that is relative, uses a device-namespace prefix,
/// or has a `..` with nothing to remove cannot be compared, and is refused when any signal is set.
/// A `USERPROFILE` in Windows form cannot contain a home in another form, so it is ignored when the
/// home is not in Windows form. A non-ASCII folder name that differs from its root only by case is
/// not matched.
///
/// A home inside a cloud-synced folder that neither an environment root nor a profile folder name
/// identifies is not refused. Detecting such a folder by its cloud reparse tag is not implemented.
///
/// # Errors
///
/// Returns [`McaoError::Preflight`] with the relocation in [`ONEDRIVE_RELOCATION`] when `home` is
/// under an environment root, or when a signal is set and `home` or `USERPROFILE` cannot be
/// compared.
pub fn check_home_outside_onedrive(
    home: &Path,
    env: impl Fn(&str) -> Option<OsString>,
) -> Result<OneDriveCheck, McaoError> {
    match find_onedrive_match(home, &env)? {
        None => Ok(OneDriveCheck::Clear),
        Some(OneDriveMatch::Root { name, root }) => Err(McaoError::Preflight(onedrive_refusal(
            home,
            &root,
            &format!("named by {name}"),
        ))),
        Some(OneDriveMatch::ProfileFolder { folder }) => {
            Ok(OneDriveCheck::Warning(onedrive_warning(home, &folder)))
        }
    }
}

/// Refuses a stateful operation whose operational home is OneDrive-synchronized. The detection and
/// the relocation are those of [`check_home_outside_onedrive`]. A warning does not refuse. The
/// refusal exits 4 rather than 3, because preflight owns exit 3.
///
/// # Errors
///
/// Returns [`McaoError::Refused`] with the message that preflight would print for a refused home,
/// or the error from [`check_home_outside_onedrive`] when the home cannot be compared.
pub fn refuse_onedrive_home(
    home: &Path,
    env: impl Fn(&str) -> Option<OsString>,
) -> Result<(), McaoError> {
    match check_home_outside_onedrive(home, env) {
        Ok(_) => Ok(()),
        Err(McaoError::Preflight(message)) => Err(McaoError::Refused(message)),
        Err(other) => Err(other),
    }
}

fn find_onedrive_match(
    home: &Path,
    env: &impl Fn(&str) -> Option<OsString>,
) -> Result<Option<OneDriveMatch>, McaoError> {
    let roots: Vec<(&'static str, PathBuf)> = ONEDRIVE_ENV_VARS
        .iter()
        .filter_map(|&name| {
            env(name)
                .filter(|value| !value.is_empty())
                .map(|value| (name, PathBuf::from(value)))
        })
        .collect();
    let profile = env("USERPROFILE")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    if roots.is_empty() && profile.is_none() {
        return Ok(None);
    }
    let Some(home) = resolved(home) else {
        let source = match roots.first() {
            Some((name, _)) => format!("the OneDrive folder named by {name}"),
            None => "the OneDrive folder beneath USERPROFILE".to_owned(),
        };
        return Err(McaoError::Preflight(format!(
            "operational home {} cannot be checked against {source}; pass an absolute path",
            home.display()
        )));
    };
    let windows = is_windows_form(&home.to_string_lossy());
    for (name, root) in roots {
        let Some(root) = resolved(&root) else {
            continue;
        };
        if is_under(&home, &root, windows) {
            return Ok(Some(OneDriveMatch::Root { name, root }));
        }
    }
    if let Some(profile) = profile {
        match resolved(&profile) {
            Some(resolved_profile) => {
                if let Some(folder) = profile_onedrive_folder(&home, &resolved_profile, windows) {
                    return Ok(Some(OneDriveMatch::ProfileFolder { folder }));
                }
            }
            None if is_windows_form(&profile.to_string_lossy()) && !windows => {}
            None => {
                return Err(McaoError::Preflight(format!(
                    "USERPROFILE {} cannot be checked against the operational home; pass an \
                     absolute path",
                    profile.display()
                )));
            }
        }
    }
    Ok(None)
}

/// Returns the sync folder that `home` lies under, when that folder sits directly beneath `profile`
/// and carries OneDrive's name. Other folders beneath the profile never match.
fn profile_onedrive_folder(home: &Path, profile: &Path, windows: bool) -> Option<PathBuf> {
    let mut home_parts = home.components();
    for part in profile.components() {
        if !home_parts
            .next()
            .is_some_and(|home_part| component_eq(home_part, part, windows))
        {
            return None;
        }
    }
    let folder = home_parts.next()?;
    is_onedrive_folder_name(&folder.as_os_str().to_string_lossy(), windows)
        .then(|| profile.join(folder.as_os_str()))
}

fn is_onedrive_folder_name(name: &str, windows: bool) -> bool {
    let prefix = ONEDRIVE_BUSINESS_FOLDER_PREFIX;
    name_eq(name, "OneDrive", windows)
        || (name.len() > prefix.len()
            && name
                .get(..prefix.len())
                .is_some_and(|head| name_eq(head, prefix, windows)))
}

fn onedrive_refusal(home: &Path, folder: &Path, source: &str) -> String {
    format!(
        "operational home {} is under the OneDrive folder {} {source}; set FM_HOME to \
         {ONEDRIVE_RELOCATION} or another folder that OneDrive does not sync",
        home.display(),
        folder.display()
    )
}

fn onedrive_warning(home: &Path, folder: &Path) -> String {
    format!(
        "operational home {} is under {}, a sync folder beneath USERPROFILE that OneDrive may \
         sync; set FM_HOME to {ONEDRIVE_RELOCATION} or another folder that OneDrive does not sync",
        home.display(),
        folder.display()
    )
}

/// Whether `text` is written in Windows form: a drive letter followed by a separator, or a UNC or
/// verbatim prefix. The Windows rules apply only to such text, so a backslash in a unix path is an
/// ordinary character.
fn is_windows_form(text: &str) -> bool {
    let bytes = text.as_bytes();
    text.starts_with(r"\\")
        || matches!(bytes, [drive, b':', b'\\' | b'/', ..] if drive.is_ascii_alphabetic())
}

/// Replaces a verbatim (`\\?\`) prefix with the plain path it names: `\\?\C:\x` becomes `C:\x`, and
/// `\\?\UNC\server\share` becomes `\\server\share`. Other paths are returned unchanged.
fn without_verbatim(path: &Path) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path.to_path_buf();
    };
    let Some(rest) = text.strip_prefix(r"\\?\") else {
        return path.to_path_buf();
    };
    match rest.get(..4) {
        Some(head) if head.eq_ignore_ascii_case(r"UNC\") => {
            PathBuf::from(format!(r"\\{}", &rest[4..]))
        }
        _ => PathBuf::from(rest),
    }
}

/// Resolves a path for comparison: the verbatim prefix is removed, and `..` components are removed
/// lexically, so `C:\a\..\b` compares as `C:\b`. Returns `None` when the result is not absolute,
/// when a `..` has nothing to remove from a relative path, or when a device-namespace prefix
/// remains. Symbolic links, junctions, and 8.3 short names are not resolved.
fn resolved(path: &Path) -> Option<PathBuf> {
    let plain = without_verbatim(path);
    let mut resolved = PathBuf::new();
    for part in plain.components() {
        match part {
            Component::Prefix(prefix)
                if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::UNC(..)) =>
            {
                resolved.push(part.as_os_str());
            }
            Component::Prefix(_) => return None,
            Component::RootDir | Component::Normal(_) => resolved.push(part.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                let last = resolved.components().next_back();
                match last {
                    Some(Component::Normal(_)) => {
                        resolved.pop();
                    }
                    // Climbing above a root stays at the root, as the file system resolves it.
                    Some(Component::RootDir | Component::Prefix(_)) => {}
                    _ => return None,
                }
            }
        }
    }
    resolved.is_absolute().then_some(resolved)
}

fn is_under(path: &Path, root: &Path, windows: bool) -> bool {
    let mut path_parts = path.components();
    root.components().all(|root_part| {
        path_parts
            .next()
            .is_some_and(|path_part| component_eq(path_part, root_part, windows))
    })
}

fn component_eq(a: Component<'_>, b: Component<'_>, windows: bool) -> bool {
    name_eq(
        &a.as_os_str().to_string_lossy(),
        &b.as_os_str().to_string_lossy(),
        windows,
    )
}

/// Compares two path names. Windows names compare ASCII-case-insensitively; unix names compare
/// exactly.
fn name_eq(a: &str, b: &str, windows: bool) -> bool {
    if windows {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}
