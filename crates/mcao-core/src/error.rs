//! The shared failure type for the `mcao` command and its exit-code contract (DPES-754).

use std::error::Error;
use std::fmt;
use std::io;

use crate::resources::Deferral;
use crate::state::TaskIdError;

/// A failure the `mcao` command reports. Each variant maps to one process exit code.
///
/// Most payloads are plain text. `Deferred` carries its structured [`Deferral`], because the
/// caller prints its message and the breaches behind it. Later slices add variants, so the enum
/// is `#[non_exhaustive]`.
#[derive(Debug)]
#[non_exhaustive]
pub enum McaoError {
    /// The command line was malformed. Exit code 2.
    Usage(String),
    /// A preflight check failed before any state changed. Exit code 3.
    Preflight(String),
    /// The request was refused. Exit code 4.
    Refused(String),
    /// The outcome could not be determined. Exit code 5.
    Uncertain(String),
    /// Recorded state conflicts with the request. Exit code 6.
    StateConflict(String),
    /// The backend does not support the operation. Exit code 7.
    Unsupported { backend: String, op: &'static str },
    /// A host resource limit deferred the start before anything was created. Exit code 75.
    Deferred(Deferral),
    /// The backend failed. Exit code 1.
    Backend(String),
    /// An I/O operation failed. Exit code 1.
    Io(io::Error),
}

impl McaoError {
    /// The process exit code for this failure.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Usage(_) => 2,
            Self::Preflight(_) => 3,
            Self::Refused(_) => 4,
            Self::Uncertain(_) => 5,
            Self::StateConflict(_) => 6,
            Self::Unsupported { .. } => 7,
            Self::Deferred(_) => 75,
            Self::Backend(_) | Self::Io(_) => 1,
        }
    }
}

impl fmt::Display for McaoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(message) => write!(f, "Usage: {message}"),
            Self::Preflight(message) => write!(f, "Preflight check failed: {message}"),
            Self::Refused(message) => write!(f, "Refused: {message}"),
            Self::Uncertain(message) => write!(f, "Uncertain: {message}"),
            Self::StateConflict(message) => write!(f, "State conflict: {message}"),
            Self::Unsupported { backend, op } => {
                write!(f, "Unsupported: {backend} does not support {op}")
            }
            // The deferral message already starts with "deferred:", so it is shown as written.
            Self::Deferred(deferral) => f.write_str(&deferral.message),
            Self::Backend(message) => write!(f, "Backend error: {message}"),
            Self::Io(error) => write!(f, "I/O error: {error}"),
        }
    }
}

impl Error for McaoError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Usage(_)
            | Self::Preflight(_)
            | Self::Refused(_)
            | Self::Uncertain(_)
            | Self::StateConflict(_)
            | Self::Unsupported { .. }
            | Self::Deferred(_)
            | Self::Backend(_) => None,
        }
    }
}

impl From<io::Error> for McaoError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<TaskIdError> for McaoError {
    /// A task id that breaks the creation rule is a malformed request, so it exits with usage code 2.
    fn from(error: TaskIdError) -> Self {
        Self::Usage(error.to_string())
    }
}
