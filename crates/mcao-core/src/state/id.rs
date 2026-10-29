//! Task identifiers, the shared stem of a task's `state/<id>.meta` and `state/<id>.status`.

use std::fmt;

/// A task identifier that the shell contract accepts as a file-name stem.
///
/// The accepted form matches `fm_task_id_path_safe` in `bin/fm-pr-lib.sh`: non-empty, not
/// starting with `.`, and made only of ASCII letters, digits, `.`, `_`, and `-`. The shell's
/// additional 64-byte creation cap is not applied here, because this type reads existing
/// records rather than creating them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskId(String);

impl TaskId {
    /// Parses `raw` as a task identifier.
    ///
    /// Returns the first rule `raw` breaks, so the caller can report the exact cause.
    pub fn parse(raw: &str) -> Result<Self, TaskIdError> {
        if raw.is_empty() {
            return Err(TaskIdError::Empty);
        }
        if raw.starts_with('.') {
            return Err(TaskIdError::LeadingDot);
        }
        if let Some(ch) = raw.chars().find(|&ch| !is_allowed(ch)) {
            return Err(TaskIdError::InvalidChar { ch });
        }
        Ok(Self(raw.to_owned()))
    }

    /// Parses `raw` as the identifier of a task about to be created.
    ///
    /// A new identifier is lowercase ASCII letters, digits, and `-`, so two ids never differ only
    /// by case, which a case-insensitive file system folds into one name. ASCII text is already
    /// in Unicode normalization form C, so no further normalization check applies. [`TaskId::parse`]
    /// stays the reader's rule, because records that exist already use other forms.
    ///
    /// # Errors
    ///
    /// Returns [`TaskIdError::Empty`] for an empty `raw`, [`TaskIdError::OutsideCreationSet`]
    /// for the first character outside `[a-z0-9-]`, and [`TaskIdError::ReservedName`] for a Windows
    /// device name such as `con`, `nul`, or `com1`. Control bytes, separators, `.`, and trailing
    /// dots or spaces all fall outside the set, so a device name with an extension such as
    /// `con.txt` is refused as a bad character.
    pub fn parse_for_creation(raw: &str) -> Result<Self, TaskIdError> {
        if raw.is_empty() {
            return Err(TaskIdError::Empty);
        }
        if let Some(ch) = raw
            .chars()
            .find(|ch| !(ch.is_ascii_lowercase() || ch.is_ascii_digit() || *ch == '-'))
        {
            return Err(TaskIdError::OutsideCreationSet { ch });
        }
        if is_windows_device_name(raw) {
            return Err(TaskIdError::ReservedName {
                name: raw.to_owned(),
            });
        }
        Ok(Self(raw.to_owned()))
    }

    /// Returns the identifier text exactly as parsed.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn is_allowed(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-')
}

/// The digits that complete a numbered device name, `com1` through `com9` and `lpt1` through `lpt9`.
const DEVICE_DIGITS: [&str; 9] = ["1", "2", "3", "4", "5", "6", "7", "8", "9"];

/// Reports whether `name` is a device name Windows reserves in every directory: `con`, `prn`,
/// `aux`, `nul`, `com1` through `com9`, and `lpt1` through `lpt9`. Callers pass lowercase text.
fn is_windows_device_name(name: &str) -> bool {
    matches!(name, "con" | "prn" | "aux" | "nul")
        || ["com", "lpt"].iter().any(|prefix| {
            name.strip_prefix(prefix)
                .is_some_and(|digit| DEVICE_DIGITS.contains(&digit))
        })
}

/// Why a string is not a valid [`TaskId`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskIdError {
    /// The identifier is empty.
    Empty,
    /// The identifier starts with `.`, which the shell contract refuses.
    LeadingDot,
    /// The identifier contains a character outside ASCII letters, digits, `.`, `_`, and `-`.
    InvalidChar {
        /// The first offending character.
        ch: char,
    },
    /// The identifier of a new task contains a character outside lowercase ASCII letters, digits,
    /// and `-`.
    OutsideCreationSet {
        /// The first offending character.
        ch: char,
    },
    /// The identifier of a new task is a Windows device name, which no file may be called.
    ReservedName {
        /// The identifier as given.
        name: String,
    },
}

impl fmt::Display for TaskIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "task id is empty"),
            Self::LeadingDot => write!(f, "task id starts with '.'"),
            Self::InvalidChar { ch } => write!(
                f,
                "task id contains {ch:?}, which is outside ASCII letters, digits, '.', '_', '-'"
            ),
            Self::OutsideCreationSet { ch } => write!(
                f,
                "new task id contains {ch:?}, which is outside lowercase ASCII letters, digits, '-'"
            ),
            Self::ReservedName { name } => {
                write!(f, "new task id {name:?} is a reserved Windows device name")
            }
        }
    }
}

impl std::error::Error for TaskIdError {}
