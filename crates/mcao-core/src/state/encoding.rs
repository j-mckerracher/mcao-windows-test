//! Cross-platform encoding of `state/<id>.meta`: the format version, the path style, and the
//! backend token.
//!
//! The record stays `key=value` text, one pair per line. These keys are additive. A reader that
//! does not know them ignores them, as the shell's `grep '^key='` readers do, and a record that
//! carries none of them is a version 1 record and reads as before. A home belongs to one
//! operating system, so a path value is an opaque string written in the verbatim form of the
//! system that wrote it and is never converted when read on another system.
//!
//! Line endings are not part of this encoding. A writer emits `\n` only, and the reader in
//! [`super::meta`] removes a `\r` before a `\n`. A file is rewritten only when a writer publishes
//! it again; nothing normalizes existing files in place.

use std::fmt;

/// The key that carries the [`FormatVersion`]. A record without it is version 1.
pub const FORMAT_KEY: &str = "format";

/// The key that carries the [`PathStyle`] of the system that wrote the record.
pub const PATH_STYLE_KEY: &str = "path_style";

/// The key that names where a task's worker runs. A record without it runs locally.
pub const EXECUTION_TARGET_KEY: &str = "execution_target";

/// The version of the `.meta` encoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormatVersion {
    /// No `format` key. Every record the shell writes today.
    V1,
    /// `format=2`: the record may carry [`PATH_STYLE_KEY`] and the `windows` backend token.
    V2,
}

impl FormatVersion {
    /// The version a record stamped by [`stamp_encoding`] carries.
    pub const CURRENT: Self = Self::V2;

    /// Returns the value written after `format=`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::V1 => "1",
            Self::V2 => "2",
        }
    }

    /// Parses the value of a `format` key.
    ///
    /// # Errors
    ///
    /// Returns [`EncodingError::UnsupportedFormat`] for any value other than `1` or `2`, so a
    /// reader never guesses at a layout written by a newer writer.
    pub fn parse(value: &str) -> Result<Self, EncodingError> {
        match value {
            "1" => Ok(Self::V1),
            "2" => Ok(Self::V2),
            other => Err(EncodingError::UnsupportedFormat(other.to_owned())),
        }
    }
}

/// The path syntax of the system that wrote a record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathStyle {
    /// Drive letters, backslashes, and UNC roots.
    Windows,
    /// Forward slashes from `/`.
    Posix,
}

impl PathStyle {
    /// Returns the style of the system this program runs on.
    pub fn host() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Posix
        }
    }

    /// Returns the value written after `path_style=`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::Posix => "posix",
        }
    }

    /// Parses the value of a `path_style` key.
    ///
    /// # Errors
    ///
    /// Returns [`EncodingError::UnknownPathStyle`] for any value other than `windows` or
    /// `posix`.
    pub fn parse(value: &str) -> Result<Self, EncodingError> {
        match value {
            "windows" => Ok(Self::Windows),
            "posix" => Ok(Self::Posix),
            other => Err(EncodingError::UnknownPathStyle(other.to_owned())),
        }
    }
}

/// The terminal backend recorded in a task's `backend` key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendToken {
    /// `tmux`, which the shell also reads from an absent or empty key.
    Tmux,
    /// `herdr`.
    Herdr,
    /// `windows`, written only by a version 2 writer.
    Windows,
}

impl BackendToken {
    /// Returns the value written after `backend=`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tmux => "tmux",
            Self::Herdr => "herdr",
            Self::Windows => "windows",
        }
    }

    /// Parses the value of a `backend` key.
    ///
    /// # Errors
    ///
    /// Returns [`EncodingError::UnknownBackend`] for any other value. The shell refuses an
    /// unknown backend the same way, and a caller must not fall back to `tmux`.
    pub fn parse(value: &str) -> Result<Self, EncodingError> {
        match value {
            "tmux" => Ok(Self::Tmux),
            "herdr" => Ok(Self::Herdr),
            "windows" => Ok(Self::Windows),
            other => Err(EncodingError::UnknownBackend(other.to_owned())),
        }
    }
}

/// The place a task's worker runs, from its `execution_target` key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionTarget {
    /// `local`, which a record without the key also means.
    Local,
    /// `gcp`, a cloud run. It has no terminal backend.
    Gcp,
}

impl ExecutionTarget {
    /// Returns the value written after `execution_target=`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Gcp => "gcp",
        }
    }

    /// Parses the value of an `execution_target` key.
    ///
    /// # Errors
    ///
    /// Returns [`EncodingError::UnknownExecutionTarget`] for any other value, including an empty
    /// one. The shell refuses both rather than guess a target.
    pub fn parse(value: &str) -> Result<Self, EncodingError> {
        match value {
            "local" => Ok(Self::Local),
            "gcp" => Ok(Self::Gcp),
            other => Err(EncodingError::UnknownExecutionTarget(other.to_owned())),
        }
    }
}

/// A value of an encoding key that this reader refuses rather than guesses at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EncodingError {
    /// The `format` value is neither `1` nor `2`.
    UnsupportedFormat(String),
    /// The `path_style` value is neither `windows` nor `posix`.
    UnknownPathStyle(String),
    /// The `backend` value is not `tmux`, `herdr`, or `windows`.
    UnknownBackend(String),
    /// The `execution_target` value is neither `local` nor `gcp`.
    UnknownExecutionTarget(String),
    /// The `execution_target` key appears more than once, so the record does not say which
    /// target is true.
    AmbiguousExecutionTarget,
    /// The record runs on `target`, which has no terminal backend, so the `tmux` default does not
    /// apply.
    NoTerminalBackend { target: ExecutionTarget },
    /// The record names `existing` as its path style, but the stamp is for `requested`. Its paths
    /// came from another system, so they are not relabelled.
    PathStyleConflict {
        existing: PathStyle,
        requested: PathStyle,
    },
}

impl fmt::Display for EncodingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedFormat(value) => write!(f, "unsupported meta format {value:?}"),
            Self::UnknownPathStyle(value) => write!(f, "unknown path style {value:?}"),
            Self::UnknownBackend(value) => write!(f, "unknown backend {value:?}"),
            Self::UnknownExecutionTarget(value) => write!(f, "unknown execution target {value:?}"),
            Self::AmbiguousExecutionTarget => write!(f, "execution target appears more than once"),
            Self::NoTerminalBackend { target } => write!(
                f,
                "execution target {} has no terminal backend",
                target.as_str()
            ),
            Self::PathStyleConflict {
                existing,
                requested,
            } => write!(
                f,
                "record path style {} conflicts with {}",
                existing.as_str(),
                requested.as_str()
            ),
        }
    }
}

impl std::error::Error for EncodingError {}

/// Returns `text` with exactly one `format=2` line and one `path_style=` line at the top.
///
/// A line whose key is `format` is dropped, as is a `path_style` line that names `style`. Every
/// other line is kept byte for byte except its line ending, which is written as `\n`: every `\r`
/// at the end of a line, before the `\n` or at the end of the text, is removed. A leading byte
/// order mark is removed so that the first key matches, and none is added, whatever system runs
/// this. Paths in `text` are not touched.
///
/// # Errors
///
/// Returns [`EncodingError::PathStyleConflict`] when `text` already names another path style, and
/// [`EncodingError::UnknownPathStyle`] when its `path_style` value is unknown. The paths of such a
/// record came from another system or from a writer this reader does not know, so the stamp
/// refuses rather than relabel them.
pub fn stamp_encoding(text: &str, style: PathStyle) -> Result<String, EncodingError> {
    let mut stamped = format!(
        "{FORMAT_KEY}={}\n{PATH_STYLE_KEY}={}\n",
        FormatVersion::CURRENT.as_str(),
        style.as_str()
    );
    for line in strip_bom(text).split_inclusive('\n') {
        if let Some((key, value)) = line.split_once('=') {
            if key == FORMAT_KEY {
                continue;
            }
            if key == PATH_STYLE_KEY {
                let existing = PathStyle::parse(value.trim_end_matches(['\r', '\n']))?;
                if existing != style {
                    return Err(EncodingError::PathStyleConflict {
                        existing,
                        requested: style,
                    });
                }
                continue;
            }
        }
        match line.strip_suffix('\n') {
            Some(body) => {
                stamped.push_str(body.trim_end_matches('\r'));
                stamped.push('\n');
            }
            None => stamped.push_str(line.trim_end_matches('\r')),
        }
    }
    Ok(stamped)
}

/// Returns `text` without a leading byte order mark. Windows editors add one by default.
pub(crate) fn strip_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}
