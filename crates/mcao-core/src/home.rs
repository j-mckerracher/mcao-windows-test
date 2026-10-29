//! Firstmate home resolution: the instance root and the `config/`, `state/`, `data/`,
//! and `projects/` directories beneath it.
//!
//! Resolution reads metadata only. It never creates, removes, or follows a final-component
//! symlink, so resolving a home cannot mutate a real one. Intermediate symlinks are followed
//! by the operating system as usual; only the final component is checked.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

/// The captured environment: only the names in [`READ_ENV`], keyed by their canonical spelling.
/// Build it with [`capture_env`], which refuses a non-UTF-8 value of a read name rather than
/// dropping it.
pub type Env = BTreeMap<String, String>;

/// Every environment variable this crate reads. [`capture_env`] keeps only these names.
pub const READ_ENV: &[&str] = &[
    "FM_HOME",
    "FM_ROOT_OVERRIDE",
    "FM_ROOT",
    "FM_CONFIG_OVERRIDE",
    "FM_STATE_OVERRIDE",
    "FM_DATA_OVERRIDE",
    "FM_PROJECTS_OVERRIDE",
    "FM_BACKEND",
    "TMUX",
    "HERDR_ENV",
];

/// Why the process environment could not be captured.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EnvError {
    /// A variable this crate reads has a value that is not UTF-8. Refused rather than dropped:
    /// an unset `FM_HOME` would silently select a different home.
    NotUtf8 { name: &'static str },
    /// A variable this crate reads is set more than once. Refused rather than picking one.
    Duplicate { name: &'static str },
}

impl fmt::Display for EnvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotUtf8 { name } => write!(f, "{name} is set to a value that is not UTF-8"),
            Self::Duplicate { name } => write!(f, "{name} is set more than once"),
        }
    }
}

impl std::error::Error for EnvError {}

/// Captures the variables this crate reads from an OS environment, such as
/// `std::env::vars_os()`. Other names are ignored whatever their bytes. On Windows, names match
/// case-insensitively and are stored under their canonical spelling from [`READ_ENV`]; elsewhere
/// they match exactly.
pub fn capture_env<I>(vars: I) -> Result<Env, EnvError>
where
    I: IntoIterator<Item = (OsString, OsString)>,
{
    let mut env = Env::new();
    for (name, value) in vars {
        let Some(canonical) = read_name(&name) else {
            continue;
        };
        if env.contains_key(canonical) {
            return Err(EnvError::Duplicate { name: canonical });
        }
        let Some(text) = value.to_str() else {
            return Err(EnvError::NotUtf8 { name: canonical });
        };
        env.insert(canonical.to_owned(), text.to_owned());
    }
    Ok(env)
}

fn read_name(name: &OsStr) -> Option<&'static str> {
    let name = name.to_str()?;
    READ_ENV
        .iter()
        .copied()
        .find(|&read| name_matches(read, name))
}

#[cfg(windows)]
fn name_matches(read: &str, name: &str) -> bool {
    read.eq_ignore_ascii_case(name)
}

#[cfg(not(windows))]
fn name_matches(read: &str, name: &str) -> bool {
    read == name
}

/// Where the home root came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootSource {
    /// The explicit `--fm-home` argument.
    Flag,
    /// `FM_HOME`, `FM_ROOT_OVERRIDE`, or `FM_ROOT`, named by the variable.
    Env(&'static str),
    /// The caller-supplied default root, normally the tracked code root.
    Default,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FmHome {
    pub root: PathBuf,
    pub root_source: RootSource,
    pub config: PathBuf,
    pub state: PathBuf,
    pub data: PathBuf,
    pub projects: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HomeError {
    /// The explicit flag was present but empty. Empty environment values count as unset.
    Empty { source: &'static str },
    /// The input contained a control byte. Directory paths never carry one.
    ControlByte { source: &'static str, byte: u8 },
    /// The final component is a symlink. Refused even when its target lies inside the home.
    FinalSymlink { source: &'static str, path: PathBuf },
    /// The path is still relative after joining the working directory. That happens when the
    /// working directory is relative, or when the input carries a prefix that replaces it
    /// (a Windows `C:foo`).
    NotAbsolute { source: &'static str, path: PathBuf },
    /// The input has a `..` component. Refused because a symlink before it would make the final
    /// component's check name a different directory than the path as written, and no documented
    /// input needs one. The path reported is the joined path, so a relative input shows its
    /// working directory.
    ParentDir { source: &'static str, path: PathBuf },
    /// The path exists but is not a directory.
    NotDirectory { source: &'static str, path: PathBuf },
    /// The path's metadata could not be read.
    Inspect {
        source: &'static str,
        path: PathBuf,
        kind: ErrorKind,
    },
}

impl fmt::Display for HomeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty { source } => write!(f, "{source} is set but empty"),
            Self::ControlByte { source, byte } => {
                write!(f, "{source} contains control byte 0x{byte:02x}")
            }
            Self::FinalSymlink { source, path } => write!(
                f,
                "{source} names a symlink, which is refused: {}",
                path.display()
            ),
            Self::NotAbsolute { source, path } => {
                write!(f, "{source} is not an absolute path: {}", path.display())
            }
            Self::ParentDir { source, path } => write!(
                f,
                "{source} contains a '..' component, which is refused: {}",
                path.display()
            ),
            Self::NotDirectory { source, path } => {
                write!(f, "{source} is not a directory: {}", path.display())
            }
            Self::Inspect { source, path, kind } => {
                write!(f, "{source}: cannot inspect {}: {kind:?}", path.display())
            }
        }
    }
}

impl std::error::Error for HomeError {}

/// Resolves the home from the explicit flag, then the environment, then `default_root`.
///
/// Relative inputs resolve against `cwd`, which must be absolute. Only an input that wins
/// precedence is validated, so an unselected override never causes a refusal. Derived
/// directories (`<root>/config` and so on) are checked the same way as overrides. Existence
/// is not required; callers that need an existing home check it themselves.
pub fn resolve(
    flag: Option<&str>,
    env: &Env,
    cwd: &Path,
    default_root: &Path,
) -> Result<FmHome, HomeError> {
    let (root, root_source) = select_root(flag, env, cwd, default_root)?;
    let config = directory(
        env,
        cwd,
        &root,
        "FM_CONFIG_OVERRIDE",
        "config",
        "config directory",
    )?;
    let state = directory(
        env,
        cwd,
        &root,
        "FM_STATE_OVERRIDE",
        "state",
        "state directory",
    )?;
    let data = directory(
        env,
        cwd,
        &root,
        "FM_DATA_OVERRIDE",
        "data",
        "data directory",
    )?;
    let projects = directory(
        env,
        cwd,
        &root,
        "FM_PROJECTS_OVERRIDE",
        "projects",
        "projects directory",
    )?;
    Ok(FmHome {
        root,
        root_source,
        config,
        state,
        data,
        projects,
    })
}

fn select_root(
    flag: Option<&str>,
    env: &Env,
    cwd: &Path,
    default_root: &Path,
) -> Result<(PathBuf, RootSource), HomeError> {
    if let Some(raw) = flag {
        return Ok((checked_path("--fm-home", raw, cwd)?, RootSource::Flag));
    }
    // `FM_ROOT_OVERRIDE` is the whole-root override only when `FM_HOME` is unset,
    // which this ordering encodes.
    for var in ["FM_HOME", "FM_ROOT_OVERRIDE", "FM_ROOT"] {
        if let Some(raw) = non_empty(env, var) {
            return Ok((checked_path(var, raw, cwd)?, RootSource::Env(var)));
        }
    }
    let root = default_root.to_path_buf();
    if !root.is_absolute() {
        return Err(HomeError::NotAbsolute {
            source: "default root",
            path: root,
        });
    }
    Ok((root, RootSource::Default))
}

/// A per-directory override when set, otherwise the derived `<root>/<name>`.
fn directory(
    env: &Env,
    cwd: &Path,
    root: &Path,
    var: &'static str,
    name: &str,
    label: &'static str,
) -> Result<PathBuf, HomeError> {
    match non_empty(env, var) {
        Some(raw) => checked_path(var, raw, cwd),
        None => inspect(label, root.join(name)),
    }
}

fn non_empty<'a>(env: &'a Env, var: &str) -> Option<&'a str> {
    env.get(var)
        .map(String::as_str)
        .filter(|raw| !raw.is_empty())
}

/// Validates one raw input and makes it absolute against `cwd`. The result is normalized with
/// `Path::components`, which drops trailing separators and `.` components, so `link/` and
/// `link/.` name the final component `link` and cannot hide a symlink from `inspect`. A `..`
/// component in the input is refused: `link/..` follows the link, yet `..` is not a link for
/// `inspect` to see, so it would bypass the final-component check. The check reads the input
/// alone, so a working directory that itself carries `..` does not refuse a relative input.
/// Nothing is canonicalized, so intermediate symlinks stay as the caller wrote them. The
/// caller-supplied default root is not checked here.
fn checked_path(source: &'static str, raw: &str, cwd: &Path) -> Result<PathBuf, HomeError> {
    if raw.is_empty() {
        return Err(HomeError::Empty { source });
    }
    if let Some(&byte) = raw.as_bytes().iter().find(|&&byte| is_control_byte(byte)) {
        return Err(HomeError::ControlByte { source, byte });
    }
    let input = Path::new(raw);
    let joined = if input.is_absolute() {
        PathBuf::from(raw)
    } else {
        cwd.join(raw)
    };
    let path: PathBuf = joined.components().collect();
    if !path.is_absolute() {
        return Err(HomeError::NotAbsolute { source, path });
    }
    if input.components().any(|part| part == Component::ParentDir) {
        return Err(HomeError::ParentDir { source, path });
    }
    inspect(source, path)
}

/// Metadata-only check of one directory path. A missing path is allowed and never created.
fn inspect(source: &'static str, path: PathBuf) -> Result<PathBuf, HomeError> {
    match fs::symlink_metadata(&path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(HomeError::FinalSymlink { source, path }),
        Ok(meta) if meta.is_dir() => Ok(path),
        Ok(_) => Err(HomeError::NotDirectory { source, path }),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(path),
        Err(error) => Err(HomeError::Inspect {
            source,
            path,
            kind: error.kind(),
        }),
    }
}

/// Every byte below 0x20 and 0x7f is refused. Shared with the config reader so both
/// layers reject the same bytes.
pub(crate) fn is_control_byte(byte: u8) -> bool {
    byte < 0x20 || byte == 0x7f
}
