//! Task identifiers, the shared stem of a task's `state/<id>.meta` and `state/<id>.status`.
//!
//! The contract is stated once in `docs/architecture.md` ("Task identifiers"). This module and
//! `fm_task_id_creation_valid` in `bin/fm-pr-lib.sh` implement it; `tests/fixtures/task-id/`
//! is the matrix both implementations are checked against.

use std::fmt;

/// A task identifier that the shell contract accepts as a file-name stem.
///
/// [`TaskId::parse`] is the reader rule: non-empty, not starting with `.`, and made only of ASCII
/// letters, digits, `.`, `_`, and `-`, with no length cap. Records written before the creation
/// rule existed stay readable under it. [`TaskId::parse_for_creation`] is the rule for a new id.
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
    /// A new identifier is lowercase ASCII letters, digits, `_`, and `-`, at most
    /// [`MAX_CREATION_LEN`] bytes, and never a Windows reserved device name in any case. A dot is
    /// refused because tmux rejects it in session and window names and reads it as a pane
    /// separator in a target. Uppercase is refused because on a case-insensitive file system two
    /// ids that differ only by case name one file; records written before this rule keep their
    /// uppercase form and still read through [`TaskId::parse`].
    ///
    /// # Errors
    ///
    /// Returns [`TaskIdError::Empty`] for an empty `raw`, [`TaskIdError::OutsideCreationSet`]
    /// for the first character outside the set (uppercase and a dot included), [`TaskIdError::TooLong`] for
    /// more than 64 bytes, and [`TaskIdError::ReservedName`] for a device name such as `con`,
    /// `nul`, `com1`, or `lpt9`.
    pub fn parse_for_creation(raw: &str) -> Result<Self, TaskIdError> {
        if raw.is_empty() {
            return Err(TaskIdError::Empty);
        }
        if let Some(ch) = raw.chars().find(|&ch| !is_creation_allowed(ch)) {
            return Err(TaskIdError::OutsideCreationSet {
                id: raw.to_owned(),
                ch,
            });
        }
        if raw.len() > MAX_CREATION_LEN {
            return Err(TaskIdError::TooLong { len: raw.len() });
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

    /// Refuses this id when it case-folds onto an id that already has a record.
    ///
    /// On a case-insensitive file system (the APFS default on macOS, NTFS on Windows) `Task-A` and
    /// `task-a` name one `state/<id>.meta` file, so creating one overwrites the other. The check is
    /// ASCII case-insensitive, which is the whole creation set. It refuses an exact duplicate too,
    /// because the same name is the same file. Every Rust creator that writes a record must call
    /// it with the ids already in the state directory before the first write. The shell creator
    /// (`bin/fm-spawn.sh`) applies the same rule through `fm_task_id_case_fold_collision`, before
    /// the task lock and again once it is held (see `docs/architecture.md`, "Task identifiers").
    ///
    /// This has no production caller yet: nothing in the crate calls it outside tests. It is a
    /// requirement on the first Rust creation caller, before that caller's first record write.
    ///
    /// # Errors
    ///
    /// Returns [`TaskIdError::CaseFoldCollision`] naming the first existing id that matches.
    pub fn check_case_fold_free<'a, I>(&self, existing: I) -> Result<(), TaskIdError>
    where
        I: IntoIterator<Item = &'a str>,
    {
        match existing
            .into_iter()
            .find(|other| other.eq_ignore_ascii_case(&self.0))
        {
            None => Ok(()),
            Some(other) => Err(TaskIdError::CaseFoldCollision {
                candidate: self.0.clone(),
                existing: other.to_owned(),
            }),
        }
    }
}

/// The longest id a new task may have, matching the shell creator's cap (`fm_task_id_creation_valid`).
pub const MAX_CREATION_LEN: usize = 64;

fn is_allowed(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-')
}

fn is_creation_allowed(ch: char) -> bool {
    ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '_' | '-')
}

/// The digits that complete a numbered device name, `com1` through `com9` and `lpt1` through `lpt9`.
const DEVICE_DIGITS: [&str; 9] = ["1", "2", "3", "4", "5", "6", "7", "8", "9"];

/// Reports whether `name` is a device name Windows reserves in every directory: `con`, `prn`,
/// `aux`, `nul`, `com1` through `com9`, and `lpt1` through `lpt9`, in any case. The name is a
/// state file's stem, so an extension cannot make it safe.
fn is_windows_device_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(lower.as_str(), "con" | "prn" | "aux" | "nul")
        || ["com", "lpt"].iter().any(|prefix| {
            lower
                .strip_prefix(prefix)
                .is_some_and(|digit| DEVICE_DIGITS.contains(&digit))
        })
}

/// Why a string is not a valid [`TaskId`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
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
    /// `_`, and `-`, which includes a dot and uppercase.
    OutsideCreationSet {
        /// The identifier as given.
        id: String,
        /// The first offending character.
        ch: char,
    },
    /// The identifier of a new task is a Windows device name, which no file may be called.
    ReservedName {
        /// The identifier as given.
        name: String,
    },
    /// The identifier of a new task case-folds onto an existing record's identifier, so both would
    /// name one file on a case-insensitive file system.
    CaseFoldCollision {
        /// The identifier being created.
        candidate: String,
        /// The existing identifier it collides with.
        existing: String,
    },
    /// The identifier of a new task is longer than [`MAX_CREATION_LEN`] bytes.
    TooLong {
        /// The identifier length in bytes.
        len: usize,
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
            Self::OutsideCreationSet { id, ch } => write!(
                f,
                "new task id {id:?} contains {ch:?}, which is outside lowercase ASCII letters, digits, '_', '-'"
            ),
            Self::ReservedName { name } => {
                write!(f, "new task id {name:?} is a reserved Windows device name")
            }
            Self::CaseFoldCollision {
                candidate,
                existing,
            } => write!(
                f,
                "new task id {candidate:?} case-folds onto the existing task id {existing:?}, which is one file on a case-insensitive file system"
            ),
            Self::TooLong { len } => write!(
                f,
                "new task id is {len} bytes, longer than the {MAX_CREATION_LEN}-byte limit"
            ),
        }
    }
}

impl std::error::Error for TaskIdError {}
