//! Read-only parsing of `state/<id>.meta` records: one `key=value` pair per line.
//!
//! Every pair is kept, in file order, including repeated keys. The shell's own writers append
//! repeated keys (the hold script appends `decision_keys` again), so a repeat is ordinary data
//! and not a corrupt record. Ordinary reads take the last value, as the shell does. An exact
//! lookup refuses to choose when the asked key repeats. Parsing takes text and never touches the
//! filesystem.

use std::fmt;

use super::encoding::{BackendToken, EncodingError, ExecutionTarget, FormatVersion, PathStyle};
use super::encoding::{EXECUTION_TARGET_KEY, FORMAT_KEY, PATH_STYLE_KEY, strip_bom};
use super::id::TaskId;

/// Keys with a typed accessor on [`TaskMeta`]. [`TaskMeta::extra`] leaves them out.
const TYPED_KEYS: [&str; 9] = [
    FORMAT_KEY,
    PATH_STYLE_KEY,
    EXECUTION_TARGET_KEY,
    "backend",
    "endpoint_task_id",
    "worktree",
    "harness",
    "kind",
    "base_branch",
];

/// The read-only contents of one `state/<id>.meta` record.
///
/// A typed accessor returns `None` when the record does not carry that key. `None` is not a
/// default: the shell reads an absent `backend` as `tmux`, and this type leaves that choice to
/// its caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskMeta {
    id: TaskId,
    entries: Vec<(String, String)>,
}

impl TaskMeta {
    /// Parses the text of `state/<id>.meta` for the task `id`.
    ///
    /// A leading byte order mark is removed first, as Windows editors add one by default, so the
    /// first key reads the same as it would without it. Each line splits at its first `=`, so a
    /// value may itself contain `=`. Values are kept verbatim, without trimming. The record's
    /// identity comes from `id`, the file name, rather than from any key, so a key named `id` is
    /// an ordinary key.
    ///
    /// A line ends at `\n`, and a `\r` directly before that `\n` is removed with it. A final line
    /// with no `\n` keeps a trailing `\r`, and any other `\r` is part of the value.
    ///
    /// Lines that carry no pair are ignored: blank lines, lines without `=`, and lines whose key
    /// is empty. The shell's `fm_meta_get` matches only `key=` lines and ignores the rest, so a
    /// record the shell reads is read the same way here. A rewrite built from [`TaskMeta::pairs`]
    /// drops ignored lines, so a writer must edit the raw text rather than rebuild it from this
    /// type.
    pub fn parse(id: TaskId, text: &str) -> Self {
        let entries = strip_bom(text)
            .lines()
            .filter_map(|line| {
                let (key, value) = line.split_once('=')?;
                (!key.is_empty()).then(|| (key.to_owned(), value.to_owned()))
            })
            .collect();
        Self { id, entries }
    }

    /// Returns the task identifier this record belongs to.
    pub fn id(&self) -> &TaskId {
        &self.id
    }

    /// Returns the value of the last line for `key`, which is the value the shell's `fm_meta_get`
    /// reads. Returns `None` when no line has `key`.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries
            .iter()
            .rev()
            .find(|(entry_key, _)| entry_key == key)
            .map(|(_, value)| value.as_str())
    }

    /// Returns the value of `key` only when at most one line has it.
    ///
    /// Recovery-grade reads use this form. It returns `Ok(None)` when no line has `key`, and
    /// [`AmbiguousKeyError`] when more than one does, because the record then does not say which
    /// value is true.
    pub fn exact(&self, key: &str) -> Result<Option<&str>, AmbiguousKeyError> {
        let mut matches = self
            .entries
            .iter()
            .filter(|(entry_key, _)| entry_key == key);
        match (matches.next(), matches.next()) {
            (None, _) => Ok(None),
            (Some((_, value)), None) => Ok(Some(value.as_str())),
            (Some(_), Some(_)) => Err(AmbiguousKeyError {
                key: key.to_owned(),
            }),
        }
    }

    /// Returns every key and value in file order, repeated keys included.
    pub fn pairs(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
    }

    /// Returns the `backend` key. The shell reads an absent key as `tmux`.
    pub fn backend(&self) -> Option<&str> {
        self.get("backend")
    }

    /// Returns the encoding version of the record. A record with no `format` key is version 1.
    ///
    /// # Errors
    ///
    /// Returns [`EncodingError::UnsupportedFormat`] when the key carries a version this reader
    /// does not know.
    pub fn format_version(&self) -> Result<FormatVersion, EncodingError> {
        self.get(FORMAT_KEY)
            .map_or(Ok(FormatVersion::V1), FormatVersion::parse)
    }

    /// Returns the path style of the system that wrote the record, or `None` when the record
    /// carries no `path_style` key, as every version 1 record. The caller chooses what an
    /// absent style means. Path values are opaque strings whatever the style.
    ///
    /// # Errors
    ///
    /// Returns [`EncodingError::UnknownPathStyle`] when the key carries an unknown value.
    pub fn path_style(&self) -> Result<Option<PathStyle>, EncodingError> {
        self.get(PATH_STYLE_KEY).map(PathStyle::parse).transpose()
    }

    /// Returns the `execution_target` key. An absent key means [`ExecutionTarget::Local`], as in
    /// the shell.
    ///
    /// # Errors
    ///
    /// Returns [`EncodingError::AmbiguousExecutionTarget`] when the key appears more than once,
    /// and [`EncodingError::UnknownExecutionTarget`] for a value other than `local` or `gcp`,
    /// including an empty one.
    pub fn execution_target(&self) -> Result<ExecutionTarget, EncodingError> {
        match self.exact(EXECUTION_TARGET_KEY) {
            Ok(None) => Ok(ExecutionTarget::Local),
            Ok(Some(value)) => ExecutionTarget::parse(value),
            Err(_) => Err(EncodingError::AmbiguousExecutionTarget),
        }
    }

    /// Returns the backend token of a local record. An absent or empty `backend` key reads as
    /// [`BackendToken::Tmux`], as in the shell.
    ///
    /// The shell resolves the execution target before it applies that default, and refuses a
    /// non-local target. A cloud record has no terminal backend, so this refuses it whatever its
    /// `backend` key holds rather than report `tmux`.
    ///
    /// # Errors
    ///
    /// Returns [`EncodingError::NoTerminalBackend`] for a record whose execution target is not
    /// local. Returns the error from [`TaskMeta::execution_target`] when that key is ambiguous or
    /// unknown, and [`EncodingError::UnknownBackend`] for any other `backend` value that is not a
    /// known token. The record stays readable through [`TaskMeta::backend`].
    pub fn backend_token(&self) -> Result<BackendToken, EncodingError> {
        match self.execution_target()? {
            ExecutionTarget::Local => match self.backend() {
                None | Some("") => Ok(BackendToken::Tmux),
                Some(value) => BackendToken::parse(value),
            },
            target @ ExecutionTarget::Gcp => Err(EncodingError::NoTerminalBackend { target }),
        }
    }

    /// Returns the `endpoint_task_id` key: the opaque runtime id of a non-tmux task, which the
    /// shell writes because that id does not encode the task label.
    ///
    /// The shell's name is used here rather than the `endpoint` name in the DPES-753 text.
    pub fn endpoint_task_id(&self) -> Option<&str> {
        self.get("endpoint_task_id")
    }

    /// Returns the `worktree` key.
    pub fn worktree(&self) -> Option<&str> {
        self.get("worktree")
    }

    /// Returns the `harness` key.
    pub fn harness(&self) -> Option<&str> {
        self.get("harness")
    }

    /// Returns the `kind` key.
    pub fn kind(&self) -> Option<&str> {
        self.get("kind")
    }

    /// Returns the worker's PID and creation time, from `pid=` and `pid_start=`.
    ///
    /// Both keys are absent for a worker that was never started, and `Ok(None)` is returned
    /// then.
    ///
    /// # Errors
    ///
    /// Returns [`crate::process_identity::IdentityRecordError`] when only one of the keys is
    /// present, a key repeats, or a value is not a number.
    pub fn process_identity(
        &self,
    ) -> Result<
        Option<crate::process_identity::ProcessIdentity>,
        crate::process_identity::IdentityRecordError,
    > {
        use crate::process_identity::{
            IdentityRecordError, PID_KEY, PID_START_KEY, ProcessIdentity, parse_identity_value,
        };
        let pid_text = self
            .exact(PID_KEY)
            .map_err(|_| IdentityRecordError::Ambiguous { key: PID_KEY })?;
        let start_text = self
            .exact(PID_START_KEY)
            .map_err(|_| IdentityRecordError::Ambiguous { key: PID_START_KEY })?;
        let pid = parse_identity_value::<u32>(PID_KEY, pid_text)?;
        let start = parse_identity_value::<u64>(PID_START_KEY, start_text)?;
        match (pid, start) {
            (None, None) => Ok(None),
            (Some(pid), Some(start)) => Ok(Some(ProcessIdentity { pid, start })),
            _ => Err(IdentityRecordError::Partial),
        }
    }

    /// Returns the `base_branch` key.
    pub fn base_branch(&self) -> Option<&str> {
        self.get("base_branch")
    }

    /// Returns every pair whose key has no typed accessor, in file order.
    pub fn extra(&self) -> impl Iterator<Item = (&str, &str)> {
        self.pairs().filter(|(key, _)| !TYPED_KEYS.contains(key))
    }
}

/// An exact lookup found the key on more than one line, so the record does not say which value
/// is true.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AmbiguousKeyError {
    key: String,
}

impl AmbiguousKeyError {
    /// Returns the key that appears more than once.
    pub fn key(&self) -> &str {
        &self.key
    }
}

impl fmt::Display for AmbiguousKeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "key {} appears more than once", self.key)
    }
}

impl std::error::Error for AmbiguousKeyError {}
