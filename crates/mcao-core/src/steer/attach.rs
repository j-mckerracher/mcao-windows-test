//! Unix attach argv construction (DPES-885 AC10, AC11; DPES-962 AC1, AC4, AC7).

use crate::error::McaoError;
use crate::state::id::MAX_CREATION_LEN;
use crate::state::{BackendToken, TaskId};

/// Whether a tmux attach is view-only.
///
/// This governs only the argv handed to `tmux attach-session`: [`AttachMode::ReadOnly`] adds
/// `-r`, which tmux enforces for that client. The steering frame layer has no read-only mode, so
/// it never reads this value. Refusing INPUT from a view-only attacher is the wiring's job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachMode {
    /// Attach with keystroke access.
    Writable,
    /// Attach view-only through tmux (`-r`).
    ReadOnly,
}

/// Builds the argv vector to attach to a session on a Unix backend.
///
/// `task_id` is checked with [`TaskId::parse`], the reader rule the base attach used. That rule
/// accepts ASCII letters, digits, `_`, `-`, and `.`. Two further refusals apply here:
///
/// - Ids over 64 bytes ([`MAX_CREATION_LEN`], the shell creator's cap). No session of that name is
///   creatable, so no attach target is built for it.
/// - Dotted ids. tmux reads `.` as a pane separator in a target, so `fm-a.b` could resolve to
///   another session's pane. A dotted session name is refused at creation too.
///
/// Uppercase, `_`, and `-` ids attach as they always did. Creation (`TaskId::parse_for_creation`)
/// and the shell creator share one creation set, so every id either one creates attaches (DPES-970).
///
/// The tmux target is exact (`=fm-<id>`). A bare name prefix-matches a sibling session, so the
/// exact form is the only one emitted. DPES-885 AC10's literal `-t fm-<task-id>` conflicts with
/// this; the owner's wording decision is pending and the Jira text is unchanged.
///
/// Herdr is refused. Its viewer argv is not verified against a pinned CLI, and the repo's own
/// Herdr call uses `herdr --session`, so no Herdr argv is emitted until verified (DPES-962 AC4).
///
/// # Errors
///
/// Returns [`McaoError::Usage`] for an id that is empty, contains a character outside the reader
/// set, is dotted, or is over 64 bytes (exit 2). Returns [`McaoError::Unsupported`] for Herdr and
/// for Windows (exit 7).
pub fn unix_attach_argv(
    backend: BackendToken,
    task_id: &str,
    mode: AttachMode,
) -> Result<Vec<String>, McaoError> {
    let task_id = TaskId::parse(task_id)?;
    if task_id.as_str().contains('.') {
        return Err(McaoError::Usage(format!(
            "task id {:?} is dotted; tmux reads '.' as a pane separator, so no session of that name can be attached",
            task_id.as_str()
        )));
    }
    if task_id.as_str().len() > MAX_CREATION_LEN {
        return Err(McaoError::Usage(format!(
            "task id is {} bytes, longer than the {MAX_CREATION_LEN}-byte limit",
            task_id.as_str().len()
        )));
    }
    let session = format!("fm-{}", task_id.as_str());
    let read_only = mode == AttachMode::ReadOnly;
    match backend {
        BackendToken::Tmux => {
            let mut argv = vec!["tmux".to_string(), "attach-session".to_string()];
            if read_only {
                argv.push("-r".to_string());
            }
            argv.push("-t".to_string());
            argv.push(format!("={session}"));
            Ok(argv)
        }
        BackendToken::Herdr => Err(McaoError::Unsupported {
            backend: backend.as_str().to_owned(),
            op: "herdr attach",
        }),
        BackendToken::Windows => Err(McaoError::Unsupported {
            backend: backend.as_str().to_owned(),
            op: "unix attach",
        }),
    }
}
