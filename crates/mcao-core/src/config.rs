//! Firstmate configuration: single-value files under `config/`, the `FM_BACKEND`
//! environment override, command-line flags, and the documented static defaults.
//!
//! Precedence per setting is flag > environment > config file > runtime detection > default.
//! Runtime detection applies only to `backend`. Every setting records where its value came from.
//!
//! A known key is parsed with the rule of its live shell reader (see [`Parse`]), so one file
//! resolves to the same value in both, except where the crate deliberately differs. Each such
//! difference is stated beside the rule it qualifies: an interior control byte is refused, and
//! a known-key file whose content is not UTF-8 is refused even past its value line. An unknown
//! file is kept verbatim as bytes, presence included. Only the entries a load consumes are
//! validated: a stray, shadowed, or unrelated entry is skipped with a diagnostic and never
//! aborts the load.
//!
//! Reading never executes anything and never writes.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::home::{Env, FmHome, is_control_byte};

/// Where a setting's value came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Flag,
    /// An environment variable, named by the variable.
    Env(&'static str),
    /// A config file, by its path.
    File(PathBuf),
    /// Derived from a runtime marker in the environment, named by the marker.
    Detected(&'static str),
    Default,
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Flag => write!(f, "flag"),
            Self::Env(var) => write!(f, "{var}"),
            Self::File(path) => write!(f, "{}", path.display()),
            Self::Detected(marker) => write!(f, "runtime marker {marker}"),
            Self::Default => write!(f, "default"),
        }
    }
}

/// A resolved known key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Setting {
    pub value: String,
    pub source: Source,
}

/// An unknown file under `config/`, kept exactly as written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unknown {
    pub bytes: Vec<u8>,
    pub path: PathBuf,
}

/// An entry under `config/` that the load neither read nor failed on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skipped {
    pub path: PathBuf,
    pub reason: SkipReason,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkipReason {
    /// A symlink. Never followed, so no read can reach its target.
    Symlink,
    /// A socket, FIFO, device, or other non-regular file. Never opened, so it cannot block.
    NotRegular,
    /// A file name that is not UTF-8. Such a name cannot be a key.
    NameNotUtf8,
    /// A regular file that could not be read.
    Unreadable(ErrorKind),
}

/// The resolved configuration. Known keys are the documented settings; unknown keys are
/// the file names kept verbatim.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Config {
    settings: BTreeMap<String, Setting>,
    unknown: BTreeMap<String, Unknown>,
    skipped: Vec<Skipped>,
}

impl Config {
    /// The resolved value of a known key.
    pub fn get(&self, key: &str) -> Option<&Setting> {
        self.settings.get(key)
    }

    /// Every known key, sorted.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Setting)> {
        self.settings
            .iter()
            .map(|(key, setting)| (key.as_str(), setting))
    }

    /// An unknown file, kept verbatim.
    pub fn unknown(&self, key: &str) -> Option<&Unknown> {
        self.unknown.get(key)
    }

    /// Every unknown file, sorted.
    pub fn unknown_iter(&self) -> impl Iterator<Item = (&str, &Unknown)> {
        self.unknown
            .iter()
            .map(|(key, entry)| (key.as_str(), entry))
    }

    /// Entries the load skipped with a diagnostic, sorted by path.
    pub fn skipped(&self) -> &[Skipped] {
        &self.skipped
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigError {
    /// A flag named a key this crate does not know.
    UnknownKey { key: String },
    /// A flag was present but empty. Empty environment values count as unset.
    EmptyFlag { key: String },
    /// A value contained a control byte.
    ControlByte {
        key: String,
        source: Source,
        byte: u8,
    },
    /// A consumed known-key file is not valid UTF-8.
    NotUtf8 { path: PathBuf },
    /// A consumed known-key entry is a symlink, which is refused.
    Symlink { path: PathBuf },
    /// A consumed known-key entry is not a regular file. A directory is refused only under a key
    /// whose shell reader tests presence with `lstat`, which is `claude-permission-mode` alone.
    NotRegular { path: PathBuf },
    /// A consumed value outside its key's closed set. The shell reader refuses it too.
    NotAccepted {
        key: String,
        source: Source,
        value: String,
        accepted: &'static [&'static str],
    },
    /// Reading the config directory or a consumed config file failed.
    Io { path: PathBuf, kind: ErrorKind },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownKey { key } => write!(f, "unknown setting key: {key}"),
            Self::EmptyFlag { key } => write!(f, "flag for {key} is set but empty"),
            Self::ControlByte { key, source, byte } => {
                write!(f, "{key} from {source} contains control byte 0x{byte:02x}")
            }
            Self::NotUtf8 { path } => write!(f, "config is not UTF-8: {}", path.display()),
            Self::Symlink { path } => write!(
                f,
                "config entry is a symlink, which is refused: {}",
                path.display()
            ),
            Self::NotRegular { path } => {
                write!(f, "config entry is not a regular file: {}", path.display())
            }
            Self::NotAccepted {
                key,
                source,
                value,
                accepted,
            } => write!(
                f,
                "{key} from {source} is '{value}', not one of: {}",
                accepted.join(", ")
            ),
            Self::Io { path, kind } => write!(f, "cannot read {}: {kind:?}", path.display()),
        }
    }
}

impl std::error::Error for ConfigError {}

/// Environment variables that set a known key, as (variable, key).
const ENV_OVERRIDES: &[(&str, &str)] = &[("FM_BACKEND", "backend")];

/// How a known key's live shell reader turns file content into its value. Every rule uses the
/// shell `[:space:]` class (see [`is_shell_space`]), so a vertical tab counts as space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Parse {
    /// `head -n 1`, trimmed at both ends: `bin/fm-branch-prefix-lib.sh`. A blank first line is
    /// unset, even when later lines hold text.
    FirstLine,
    /// `tr -d '[:space:]'` over the whole file: `bin/fm-harness.sh` `resolve_crew`. A blank file
    /// is unset, so the default applies, as the shell treats it.
    WholeFileSqueezed,
    /// The same squeeze as [`Parse::WholeFileSqueezed`], but a present file always yields a
    /// value, even an empty one: `bin/fm-spawn.sh` reads `claude-permission-mode` this way, tests
    /// presence with `lstat`, and refuses a blank file rather than launching on the default. The
    /// empty value is returned here and then refused by the key's closed set ([`Known::accepted`]).
    WholeFileSqueezedPresent,
    /// The first line whose squeezed text is non-empty: `bin/fm-backend.sh` `fm_backend_name`.
    FirstSqueezedLine,
    /// The first line with text after trimming: `bin/mcao-setup.sh` `cfg_read`, which reads
    /// `developer-name` and `max-workers`. One deliberate difference: an interior CR is refused
    /// here, where `cfg_read` deletes it.
    FirstNonBlankLine,
}

impl Parse {
    /// Whether the shell reader decides presence with `lstat`, so a directory of the key's name
    /// counts as present and is refused. The other readers test `[ -f ]`, which a directory fails,
    /// so the key reads as unset.
    fn presence_is_lstat(self) -> bool {
        matches!(self, Self::WholeFileSqueezedPresent)
    }
}

/// A documented setting: its file name is its key.
struct Known {
    key: &'static str,
    /// Used when no source supplies the key. `"default"` for `crew-harness` means "mirror the
    /// firstmate's own harness", exactly as `resolve_crew` treats an absent file.
    default: &'static str,
    parse: Parse,
    /// The closed value set the shell reader enforces, or `None` when this crate leaves the
    /// value to its consumer. Checked for every source that wins precedence, never for a default.
    accepted: Option<&'static [&'static str]>,
}

/// The documented settings. `backend` and `crew-harness` have value sets that their consumers
/// check, so this crate leaves them open.
const KNOWN: &[Known] = &[
    Known {
        key: "developer-name",
        default: "Developer",
        parse: Parse::FirstNonBlankLine,
        accepted: None,
    },
    Known {
        key: "branch-prefix",
        default: "moa/",
        parse: Parse::FirstLine,
        accepted: None,
    },
    Known {
        key: "crew-harness",
        default: "default",
        parse: Parse::WholeFileSqueezed,
        accepted: None,
    },
    Known {
        key: "max-workers",
        default: "6",
        parse: Parse::FirstNonBlankLine,
        accepted: None,
    },
    Known {
        key: "claude-permission-mode",
        default: "bypass",
        parse: Parse::WholeFileSqueezedPresent,
        accepted: Some(&["bypass", "auto"]),
    },
    Known {
        key: "backend",
        default: "tmux",
        parse: Parse::FirstSqueezedLine,
        accepted: None,
    },
];

/// Loads every setting for `home`. Flags and environment values are validated only when they
/// win precedence. A config file is read only when no higher source supplies its key, and an
/// unknown file is read and kept only when it is a regular file.
pub fn load(
    home: &FmHome,
    flags: &BTreeMap<String, String>,
    env: &Env,
) -> Result<Config, ConfigError> {
    if let Some(key) = flags.keys().find(|key| !is_known(key)) {
        return Err(ConfigError::UnknownKey { key: key.clone() });
    }
    let scan = scan(&home.config)?;
    let mut settings = BTreeMap::new();
    for known in KNOWN {
        let setting = resolve_known(known, flags, env, &scan.entries)?;
        settings.insert(known.key.to_owned(), setting);
    }
    let mut unknown = BTreeMap::new();
    let mut skipped = scan.skipped;
    for (key, (kind, path)) in &scan.entries {
        if is_known(key) {
            continue;
        }
        match kind {
            Kind::Regular => match fs::read(path) {
                Ok(bytes) => {
                    unknown.insert(
                        key.clone(),
                        Unknown {
                            bytes,
                            path: path.clone(),
                        },
                    );
                }
                Err(error) => skipped.push(Skipped {
                    path: path.clone(),
                    reason: SkipReason::Unreadable(error.kind()),
                }),
            },
            Kind::Directory => {}
            Kind::Symlink => skipped.push(Skipped {
                path: path.clone(),
                reason: SkipReason::Symlink,
            }),
            Kind::Other => skipped.push(Skipped {
                path: path.clone(),
                reason: SkipReason::NotRegular,
            }),
        }
    }
    skipped.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(Config {
        settings,
        unknown,
        skipped,
    })
}

fn is_known(key: &str) -> bool {
    KNOWN.iter().any(|known| known.key == key)
}

fn resolve_known(
    known: &Known,
    flags: &BTreeMap<String, String>,
    env: &Env,
    entries: &BTreeMap<String, (Kind, PathBuf)>,
) -> Result<Setting, ConfigError> {
    let key = known.key;
    if let Some(raw) = flags.get(key) {
        if raw.is_empty() {
            return Err(ConfigError::EmptyFlag {
                key: key.to_owned(),
            });
        }
        check_control(key, &Source::Flag, raw)?;
        check_accepted(known, &Source::Flag, raw)?;
        return Ok(Setting {
            value: raw.clone(),
            source: Source::Flag,
        });
    }
    for &(var, env_key) in ENV_OVERRIDES {
        if env_key != key {
            continue;
        }
        if let Some(raw) = env.get(var).filter(|raw| !raw.is_empty()) {
            let source = Source::Env(var);
            check_control(key, &source, raw)?;
            check_accepted(known, &source, raw)?;
            return Ok(Setting {
                value: raw.clone(),
                source,
            });
        }
    }
    // Only reaching this point consumes the file. A shadowed file is never examined.
    if let Some((kind, path)) = entries.get(key) {
        match kind {
            Kind::Regular => {
                if let Some(value) = read_known(key, known.parse, path)? {
                    let source = Source::File(path.clone());
                    check_accepted(known, &source, &value)?;
                    return Ok(Setting { value, source });
                }
            }
            // Only a key whose shell reader tests presence with `lstat` refuses a directory.
            // The others test `[ -f ]`, which a directory fails, so the key reads as unset.
            Kind::Directory if known.parse.presence_is_lstat() => {
                return Err(ConfigError::NotRegular { path: path.clone() });
            }
            Kind::Directory => {}
            Kind::Symlink => return Err(ConfigError::Symlink { path: path.clone() }),
            Kind::Other => return Err(ConfigError::NotRegular { path: path.clone() }),
        }
    }
    if key == "backend"
        && let Some((value, marker)) = detect_backend(env)
    {
        return Ok(Setting {
            value: value.to_owned(),
            source: Source::Detected(marker),
        });
    }
    Ok(Setting {
        value: known.default.to_owned(),
        source: Source::Default,
    })
}

/// Runtime auto-detection for `backend`: `$TMUX` is checked before `HERDR_ENV=1`, so the
/// innermost marker wins when both are present.
fn detect_backend(env: &Env) -> Option<(&'static str, &'static str)> {
    if env.get("TMUX").is_some_and(|raw| !raw.is_empty()) {
        return Some(("tmux", "TMUX"));
    }
    if env.get("HERDR_ENV").is_some_and(|raw| raw == "1") {
        return Some(("herdr", "HERDR_ENV"));
    }
    None
}

/// What a directory entry is, decided from its own metadata without following a link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Regular,
    /// A directory. Not opened or read; only a presence test sees it.
    Directory,
    Symlink,
    Other,
}

#[derive(Default)]
struct Scan {
    /// Every entry whose name is UTF-8, by name. A non-UTF-8 directory is not recorded, since
    /// no key can name it.
    entries: BTreeMap<String, (Kind, PathBuf)>,
    skipped: Vec<Skipped>,
}

/// Classifies the entries directly under `config/`. Nothing is opened here, so no entry can
/// block or be followed; a failure to list the directory is the only error.
fn scan(config: &Path) -> Result<Scan, ConfigError> {
    let entries = match fs::read_dir(config) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Scan::default()),
        Err(error) => return Err(io_error(config, &error)),
    };
    let mut scan = Scan::default();
    for entry in entries {
        let entry = entry.map_err(|error| io_error(config, &error))?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|error| io_error(&path, &error))?;
        let Ok(name) = entry.file_name().into_string() else {
            // A directory of a non-UTF-8 name is ignored like any directory.
            if !file_type.is_dir() {
                scan.skipped.push(Skipped {
                    path,
                    reason: SkipReason::NameNotUtf8,
                });
            }
            continue;
        };
        let kind = if file_type.is_dir() {
            Kind::Directory
        } else if file_type.is_symlink() {
            Kind::Symlink
        } else if file_type.is_file() {
            Kind::Regular
        } else {
            Kind::Other
        };
        scan.entries.insert(name, (kind, path));
    }
    Ok(scan)
}

/// Refuses a value outside the key's closed set, if it has one. Checked only for a value that
/// wins precedence.
fn check_accepted(known: &Known, source: &Source, value: &str) -> Result<(), ConfigError> {
    match known.accepted {
        Some(accepted) if !accepted.contains(&value) => Err(ConfigError::NotAccepted {
            key: known.key.to_owned(),
            source: source.clone(),
            value: value.to_owned(),
            accepted,
        }),
        _ => Ok(()),
    }
}

/// Reads a consumed known-key file and applies its shell reader's rule. `None` means the file
/// has no value, which counts as unset. The whole file must be UTF-8, not only its value line:
/// the shell reads bytes and does not refuse later-line non-UTF-8, a deliberate difference. An
/// unreadable file is refused with an I/O error, also a deliberate difference: `cfg_read` reads
/// it as unset, so the default would apply silently.
fn read_known(key: &str, parse: Parse, path: &Path) -> Result<Option<String>, ConfigError> {
    let bytes = fs::read(path).map_err(|error| io_error(path, &error))?;
    let text = String::from_utf8(bytes).map_err(|_| ConfigError::NotUtf8 {
        path: path.to_path_buf(),
    })?;
    let Some(value) = parse_value(parse, &text) else {
        return Ok(None);
    };
    check_control(key, &Source::File(path.to_path_buf()), &value)?;
    Ok(Some(value))
}

/// The shell `[:space:]` class: space, tab, newline, vertical tab, form feed, and carriage return.
fn is_shell_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\u{0b}' | '\u{0c}' | '\r')
}

fn trimmed(line: &str) -> String {
    line.trim_matches(is_shell_space).to_owned()
}

fn squeezed(text: &str) -> String {
    text.chars().filter(|&c| !is_shell_space(c)).collect()
}

fn parse_value(parse: Parse, text: &str) -> Option<String> {
    let value = match parse {
        Parse::FirstLine => trimmed(text.split('\n').next().unwrap_or("")),
        Parse::WholeFileSqueezed => squeezed(text),
        Parse::WholeFileSqueezedPresent => return Some(squeezed(text)),
        Parse::FirstSqueezedLine => text
            .split('\n')
            .map(squeezed)
            .find(|line| !line.is_empty())
            .unwrap_or_default(),
        Parse::FirstNonBlankLine => text
            .split('\n')
            .map(trimmed)
            .find(|line| !line.is_empty())
            .unwrap_or_default(),
    };
    (!value.is_empty()).then_some(value)
}

fn check_control(key: &str, source: &Source, value: &str) -> Result<(), ConfigError> {
    match value.bytes().find(|&byte| is_control_byte(byte)) {
        Some(byte) => Err(ConfigError::ControlByte {
            key: key.to_owned(),
            source: source.clone(),
            byte,
        }),
        None => Ok(()),
    }
}

fn io_error(path: &Path, error: &std::io::Error) -> ConfigError {
    ConfigError::Io {
        path: path.to_path_buf(),
        kind: error.kind(),
    }
}
