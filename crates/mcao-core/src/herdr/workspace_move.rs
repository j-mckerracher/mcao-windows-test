//! `mcao herdr workspace-move`: one `workspace.move` request over herdr's control socket.
//!
//! The client sends one request line and reads one reply line. It prints that reply and exits 0
//! only when the reply answers this request with a `workspace_list` result. It is the wire half of
//! `bin/backends/herdr-workspace-move.py`, and its exit codes are that script's: 0 for a matching
//! reply, 2 when the arguments or the connection are invalid, 3 when the request cannot be sent or
//! no reply line arrives, and 4 when the reply is malformed, mismatched, or an error.
//!
//! Only Unix has the socket transport. Elsewhere [`run`] returns [`WorkspaceMoveError::Unsupported`].
//! Nothing here starts, stops, or drives herdr. The only method it sends is `workspace.move`.

use std::fmt;
use std::io::{self, Write};
use std::path::PathBuf;

use serde_json::Value;

/// The request id the reference sends and expects back.
const REQUEST_ID: &str = "fm-workspace-move";

/// A validated `workspace-move` request. The reference takes the same three values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceMoveRequest {
    /// Path of the herdr control socket. It must be absolute, as the reference requires.
    pub socket_path: PathBuf,
    /// The workspace to move. It must be non-empty and hold no tab or line break.
    pub workspace_id: String,
    /// The position to move the workspace to.
    pub insert_index: u64,
}

/// Why `workspace-move` failed. Each variant maps to a reference exit code.
#[derive(Debug)]
#[non_exhaustive]
pub enum WorkspaceMoveError {
    /// An argument was malformed. Exit code 2.
    Usage(String),
    /// The socket could not be reached. Exit code 2.
    Unreachable(String),
    /// The request could not be sent, or no complete reply line arrived in time. Exit code 3.
    Transport(String),
    /// The reply was malformed, mismatched, or reported an error. Exit code 4.
    Refused(String),
    /// Writing the reply failed. Exit code 3 for a closed pipe, as the reference reports it, and
    /// 1 for any other failure.
    Output(io::Error),
    /// The platform has no herdr control-socket transport. Exit code 7.
    Unsupported,
}

impl WorkspaceMoveError {
    /// The process exit code for this failure, as `bin/backends/herdr-workspace-move.py` reports it.
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Usage(_) | Self::Unreachable(_) => 2,
            Self::Transport(_) => 3,
            Self::Refused(_) => 4,
            Self::Output(error) if error.kind() == io::ErrorKind::BrokenPipe => 3,
            Self::Output(_) => 1,
            Self::Unsupported => 7,
        }
    }
}

impl fmt::Display for WorkspaceMoveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(message) => write!(f, "Usage: {message}"),
            Self::Unreachable(message) => write!(f, "Unreachable: {message}"),
            Self::Transport(message) => write!(f, "Transport: {message}"),
            Self::Refused(message) => write!(f, "Refused: {message}"),
            Self::Output(error) => write!(f, "I/O error: {error}"),
            Self::Unsupported => {
                write!(
                    f,
                    "Unsupported: herdr does not support workspace-move on this platform"
                )
            }
        }
    }
}

impl std::error::Error for WorkspaceMoveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Output(error) => Some(error),
            Self::Usage(_)
            | Self::Unreachable(_)
            | Self::Transport(_)
            | Self::Refused(_)
            | Self::Unsupported => None,
        }
    }
}

/// Parses the insert index the way the reference accepts it: plain non-negative digits, with no
/// sign, leading zero, or surrounding space. Any other spelling is a usage error.
pub fn parse_insert_index(text: &str) -> Result<u64, WorkspaceMoveError> {
    match text.parse::<u64>() {
        Ok(index) if index.to_string() == text => Ok(index),
        _ => Err(WorkspaceMoveError::Usage(format!(
            "insert index must be a plain non-negative integer, not {text:?}"
        ))),
    }
}

/// Checks the request the way the reference does, before any connection is made. The socket path
/// must be absolute, and the workspace id must be non-empty with no tab or line break.
pub fn validate(request: &WorkspaceMoveRequest) -> Result<(), WorkspaceMoveError> {
    let absolute = request
        .socket_path
        .as_os_str()
        .as_encoded_bytes()
        .starts_with(b"/");
    if !absolute {
        return Err(WorkspaceMoveError::Usage(format!(
            "socket path must be absolute, not {:?}",
            request.socket_path
        )));
    }
    if request.workspace_id.is_empty() || request.workspace_id.contains(['\t', '\r', '\n']) {
        return Err(WorkspaceMoveError::Usage(format!(
            "workspace id must be non-empty with no tab or line break, not {:?}",
            request.workspace_id
        )));
    }
    Ok(())
}

/// Checks one reply line the way the reference does, and returns the text to print. The reply
/// must answer this request, carry no error, and hold a `workspace_list` result with a
/// `workspaces` array. Any other reply, including one that is not a JSON object, is refused.
pub fn judge_reply(line: &[u8]) -> Result<String, WorkspaceMoveError> {
    let text = String::from_utf8_lossy(line).into_owned();
    let response: Value = serde_json::from_str(&text)
        .map_err(|error| WorkspaceMoveError::Refused(format!("reply is not JSON: {error}")))?;
    let result = response.get("result");
    let answers_request = response.get("id").and_then(Value::as_str) == Some(REQUEST_ID);
    let carries_no_error = response.get("error").is_none_or(Value::is_null);
    let is_workspace_list = result
        .and_then(|result| result.get("type"))
        .and_then(Value::as_str)
        == Some("workspace_list")
        && result
            .and_then(|result| result.get("workspaces"))
            .is_some_and(Value::is_array);
    if answers_request && carries_no_error && is_workspace_list {
        Ok(text)
    } else {
        Err(WorkspaceMoveError::Refused(
            "reply does not answer the workspace.move request".to_owned(),
        ))
    }
}

/// Sends the request and writes the matching reply to `out`. Returns `Ok` only for a matching
/// reply. The module docs list the failure exit codes.
#[cfg(unix)]
pub fn run(request: &WorkspaceMoveRequest, out: &mut impl Write) -> Result<(), WorkspaceMoveError> {
    socket::run(request, out)
}

/// Windows has no herdr control-socket transport in this slice, so the result is explicit.
#[cfg(not(unix))]
pub fn run(
    _request: &WorkspaceMoveRequest,
    _out: &mut impl Write,
) -> Result<(), WorkspaceMoveError> {
    Err(WorkspaceMoveError::Unsupported)
}

#[cfg(unix)]
mod socket {
    use std::io::{self, Write};
    use std::os::unix::net::UnixStream;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use serde::Serialize;

    use super::{REQUEST_ID, WorkspaceMoveError, WorkspaceMoveRequest, judge_reply, validate};
    use crate::herdr::control_socket::{LineReader, Next, connect_within, set_timeouts};

    /// Budget for opening the control socket. The standard library `UnixStream::connect` takes no
    /// timeout, so [`connect_within`] bounds it from outside, as the reference's connect does.
    const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
    /// Budget for sending the request, as the reference's connect timeout applies to it too.
    const SEND_TIMEOUT: Duration = Duration::from_secs(5);
    /// Budget for the reply line, counted from the end of the send, as in the reference.
    const RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);
    /// The largest reply the reference accepts before it gives up.
    const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

    #[derive(Serialize)]
    struct Envelope<'a> {
        id: &'static str,
        method: &'static str,
        params: Params<'a>,
    }

    /// Field order matches the reference, so the request bytes match it for ASCII input.
    #[derive(Serialize)]
    struct Params<'a> {
        workspace_id: &'a str,
        insert_index: u64,
    }

    pub fn run(
        request: &WorkspaceMoveRequest,
        out: &mut impl Write,
    ) -> Result<(), WorkspaceMoveError> {
        validate(request)?;
        let mut stream = connect(&request.socket_path)?;
        stream
            .write_all(request_line(request).as_bytes())
            .map_err(|error| {
                WorkspaceMoveError::Transport(format!("cannot send the request: {error}"))
            })?;
        let mut reader = LineReader::new(stream, Some(MAX_RESPONSE_BYTES));
        let reply = match reader.next_line(Instant::now() + RESPONSE_TIMEOUT) {
            Next::Line(line) => line,
            Next::Timeout => {
                return Err(WorkspaceMoveError::Transport(
                    "no complete reply line before the deadline".to_owned(),
                ));
            }
            Next::Closed => {
                return Err(WorkspaceMoveError::Transport(
                    "the connection closed before a reply line".to_owned(),
                ));
            }
            Next::Failed(error) => {
                return Err(WorkspaceMoveError::Transport(format!(
                    "cannot read the reply: {error}"
                )));
            }
        };
        let line = judge_reply(&reply)?;
        writeln!(out, "{line}")
            .and_then(|()| out.flush())
            .map_err(WorkspaceMoveError::Output)
    }

    fn connect(path: &Path) -> Result<UnixStream, WorkspaceMoveError> {
        connect_with(path, CONNECT_TIMEOUT, UnixStream::connect)
    }

    /// Opens the control socket within `budget` and sets its timeouts. `connector` is
    /// `UnixStream::connect` in production; the tests pass a connector that never finishes.
    fn connect_with<F>(
        path: &Path,
        budget: Duration,
        connector: F,
    ) -> Result<UnixStream, WorkspaceMoveError>
    where
        F: FnOnce(PathBuf) -> io::Result<UnixStream> + Send + 'static,
    {
        let stream = connect_within(path, budget, connector).map_err(|error| {
            WorkspaceMoveError::Unreachable(format!(
                "cannot connect to {}: {error}",
                path.display()
            ))
        })?;
        set_timeouts(&stream, SEND_TIMEOUT).map_err(|error| {
            WorkspaceMoveError::Unreachable(format!("cannot set socket timeouts: {error}"))
        })?;
        Ok(stream)
    }

    fn request_line(request: &WorkspaceMoveRequest) -> String {
        let envelope = Envelope {
            id: REQUEST_ID,
            method: "workspace.move",
            params: Params {
                workspace_id: &request.workspace_id,
                insert_index: request.insert_index,
            },
        };
        let mut line = serde_json::to_string(&envelope)
            .expect("a struct of strings and integers always serializes");
        line.push('\n');
        line
    }

    #[cfg(test)]
    mod tests {
        use std::sync::mpsc;

        use super::*;

        #[test]
        fn a_connect_that_never_finishes_is_unreachable_within_its_budget() {
            // Stands in for a kernel connect blocked on a full listen backlog. It stays blocked
            // until the check below has run, then lets the abandoned helper thread finish.
            let (release, released) = mpsc::channel::<()>();
            let budget = Duration::from_millis(100);
            let started = Instant::now();
            let result = connect_with(Path::new("herdr.sock"), budget, move |_| {
                let _ = released.recv();
                Err(io::Error::other("released"))
            });
            let waited = started.elapsed();
            drop(release);
            let err = result.expect_err("a connect that never finishes must fail");
            assert!(
                matches!(err, WorkspaceMoveError::Unreachable(_)),
                "got: {err}"
            );
            assert_eq!(err.exit_code(), 2);
            assert!(waited < Duration::from_secs(2), "waited {waited:?}");
        }
    }
}
