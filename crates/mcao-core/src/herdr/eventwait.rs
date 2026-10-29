//! `mcao herdr eventwait`: one bounded subscription to herdr's `pane.agent_status_changed` stream.
//!
//! The reader connects to the control socket, subscribes to the given panes, and prints one
//! projected line per event until the timeout elapses. It is the wire half of the escalation path
//! that `bin/backends/herdr-eventwait.py` implements, and its exit codes are that script's: 0 for
//! a clean timeout, 2 when herdr cannot be reached within five seconds or does not acknowledge in
//! time, 3 when the subscription is not started, and 4 when the stream ends early. Any nonzero
//! status tells the caller to fall back to polling.
//!
//! Only Unix has the socket transport. Elsewhere [`run`] returns [`EventwaitError::Unsupported`].
//! Nothing here starts, stops, or drives herdr. It only subscribes.

use std::fmt;
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

/// A validated `eventwait` request. The reference reader takes the same three values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventwaitRequest {
    /// Path of the herdr control socket.
    pub socket_path: PathBuf,
    /// How long to stay subscribed. Running to this deadline is the normal end.
    pub timeout: Duration,
    /// Pane ids to subscribe to, one subscription each.
    pub panes: Vec<String>,
}

/// Why `eventwait` ended without a clean timeout. Each variant maps to a reference exit code.
#[derive(Debug)]
#[non_exhaustive]
pub enum EventwaitError {
    /// An argument was malformed. Exit code 2.
    Usage(String),
    /// The socket could not be reached, the request was not sent, or no acknowledgement came in
    /// time. Exit code 2.
    Unreachable(String),
    /// herdr answered without a started subscription. Exit code 3.
    NotSubscribed,
    /// The stream ended, or a read failed, before the deadline. Exit code 4.
    StreamLost(String),
    /// Writing an event to the output failed. Exit code 1.
    Output(std::io::Error),
    /// The platform has no herdr control-socket transport. Exit code 7.
    Unsupported,
}

impl EventwaitError {
    /// The process exit code for this failure, as `bin/backends/herdr-eventwait.py` reports it.
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Usage(_) | Self::Unreachable(_) => 2,
            Self::NotSubscribed => 3,
            Self::StreamLost(_) => 4,
            Self::Output(_) => 1,
            Self::Unsupported => 7,
        }
    }
}

impl fmt::Display for EventwaitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(message) => write!(f, "Usage: {message}"),
            Self::Unreachable(message) => write!(f, "Unreachable: {message}"),
            Self::NotSubscribed => {
                write!(
                    f,
                    "Not subscribed: herdr did not acknowledge the subscription"
                )
            }
            Self::StreamLost(message) => write!(f, "Stream lost: {message}"),
            Self::Output(error) => write!(f, "I/O error: {error}"),
            Self::Unsupported => {
                write!(
                    f,
                    "Unsupported: herdr does not support eventwait on this platform"
                )
            }
        }
    }
}

impl std::error::Error for EventwaitError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Output(error) => Some(error),
            Self::Usage(_)
            | Self::Unreachable(_)
            | Self::NotSubscribed
            | Self::StreamLost(_)
            | Self::Unsupported => None,
        }
    }
}

/// Parses the timeout in seconds. It accepts a finite number after trimming whitespace, which is
/// what Python's `float()` accepts for these inputs, and rejects zero, negatives, NaN, and
/// infinity, as the reference does.
pub fn parse_timeout(text: &str) -> Result<Duration, EventwaitError> {
    let usage = || {
        EventwaitError::Usage(format!(
            "timeout must be a positive number of seconds, not {text:?}"
        ))
    };
    let seconds: f64 = text.trim().parse().map_err(|_| usage())?;
    if !seconds.is_finite() || seconds <= 0.0 {
        return Err(usage());
    }
    Duration::try_from_secs_f64(seconds).map_err(|_| usage())
}

/// Connects, subscribes, and streams projected events to `out` until the deadline. Returns `Ok`
/// at the deadline. The module docs list the failure exit codes.
#[cfg(unix)]
pub fn run(request: &EventwaitRequest, out: &mut impl Write) -> Result<(), EventwaitError> {
    socket::run(request, out)
}

/// Windows has no herdr control-socket transport in this slice, so the result is explicit.
#[cfg(not(unix))]
pub fn run(_request: &EventwaitRequest, _out: &mut impl Write) -> Result<(), EventwaitError> {
    Err(EventwaitError::Unsupported)
}

#[cfg(unix)]
mod socket {
    use std::io::{self, Write};
    use std::os::unix::net::UnixStream;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use super::{EventwaitError, EventwaitRequest};
    use crate::herdr::control_socket::{LineReader, Next, connect_within, set_timeouts};
    use crate::herdr::wire;

    /// Budget for opening the control socket, as in the reference reader. The standard library
    /// `UnixStream::connect` takes no timeout, so [`connect_within`] bounds it from outside.
    const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
    /// Budget for sending the subscribe request, as in the reference reader.
    const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
    /// Budget for the acknowledgement, which the overall deadline also caps.
    const ACK_TIMEOUT: Duration = Duration::from_secs(5);

    pub fn run(request: &EventwaitRequest, out: &mut impl Write) -> Result<(), EventwaitError> {
        check(request)?;
        let start = Instant::now();
        let deadline = start
            .checked_add(request.timeout)
            .ok_or_else(|| EventwaitError::Usage("timeout is too large".to_owned()))?;
        let mut stream = connect(&request.socket_path)?;
        subscribe(&mut stream, &request.panes)?;
        let mut reader = LineReader::new(stream, None);
        acknowledge(&mut reader, deadline.min(start + ACK_TIMEOUT))?;
        match stream_events(&mut reader, deadline, out) {
            // A consumer that stopped reading ends the wait normally, as the reference does.
            Err(EventwaitError::Output(error)) if error.kind() == io::ErrorKind::BrokenPipe => {
                Ok(())
            }
            other => other,
        }
    }

    fn check(request: &EventwaitRequest) -> Result<(), EventwaitError> {
        if request.panes.is_empty() {
            return Err(EventwaitError::Usage(
                "at least one pane id is required".to_owned(),
            ));
        }
        if request.timeout.is_zero() {
            return Err(EventwaitError::Usage("timeout must be positive".to_owned()));
        }
        Ok(())
    }

    fn connect(path: &Path) -> Result<UnixStream, EventwaitError> {
        connect_with(path, CONNECT_TIMEOUT, UnixStream::connect)
    }

    /// Opens the control socket within `budget` and sets its timeouts. `connector` is
    /// `UnixStream::connect` in production; tests pass a connector that never finishes.
    fn connect_with<F>(
        path: &Path,
        budget: Duration,
        connector: F,
    ) -> Result<UnixStream, EventwaitError>
    where
        F: FnOnce(PathBuf) -> io::Result<UnixStream> + Send + 'static,
    {
        let stream = connect_within(path, budget, connector).map_err(|error| {
            EventwaitError::Unreachable(format!("cannot connect to {}: {error}", path.display()))
        })?;
        set_timeouts(&stream, WRITE_TIMEOUT).map_err(|error| {
            EventwaitError::Unreachable(format!("cannot set socket timeouts: {error}"))
        })?;
        Ok(stream)
    }

    fn subscribe(stream: &mut UnixStream, panes: &[String]) -> Result<(), EventwaitError> {
        let mut request = wire::subscribe_request(panes);
        request.push('\n');
        stream.write_all(request.as_bytes()).map_err(send_error)
    }

    fn send_error(error: io::Error) -> EventwaitError {
        EventwaitError::Unreachable(format!("cannot send the subscribe request: {error}"))
    }

    fn acknowledge(reader: &mut LineReader, deadline: Instant) -> Result<(), EventwaitError> {
        match reader.next_line(deadline) {
            Next::Line(line) if wire::is_subscription_started(&line) => Ok(()),
            Next::Line(_) => Err(EventwaitError::NotSubscribed),
            Next::Timeout => Err(EventwaitError::Unreachable(
                "no subscription acknowledgement before the deadline".to_owned(),
            )),
            Next::Closed => Err(EventwaitError::Unreachable(
                "herdr closed the connection before acknowledging".to_owned(),
            )),
            Next::Failed(error) => Err(EventwaitError::Unreachable(format!(
                "cannot read the acknowledgement: {error}"
            ))),
        }
    }

    fn stream_events(
        reader: &mut LineReader,
        deadline: Instant,
        out: &mut impl Write,
    ) -> Result<(), EventwaitError> {
        emit(out, "@subscribed")?;
        loop {
            match reader.next_line(deadline) {
                Next::Line(line) => {
                    if let Some(event) = wire::project_event(&line) {
                        emit(out, &event.to_line())?;
                    }
                }
                Next::Timeout => return Ok(()),
                Next::Closed => {
                    return Err(EventwaitError::StreamLost(
                        "herdr closed the event stream before the deadline".to_owned(),
                    ));
                }
                Next::Failed(error) => {
                    return Err(EventwaitError::StreamLost(format!(
                        "cannot read the event stream: {error}"
                    )));
                }
            }
        }
    }

    /// Writes one line and flushes it, so a caller reacts to each event as it arrives.
    fn emit(out: &mut impl Write, line: &str) -> Result<(), EventwaitError> {
        writeln!(out, "{line}")
            .and_then(|()| out.flush())
            .map_err(EventwaitError::Output)
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
            assert!(matches!(err, EventwaitError::Unreachable(_)), "got: {err}");
            assert_eq!(err.exit_code(), 2);
            assert!(waited < Duration::from_secs(2), "waited {waited:?}");
        }
    }
}
