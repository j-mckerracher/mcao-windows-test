//! Reads an agent's state from the Herdr control socket.
//!
//! [`HerdrBackend::get_agent_state`] sends one `agent.get` request as a JSON line and reads one
//! reply line. The request and the status mapping follow the `agent get` reads in
//! `bin/backends/herdr.sh`: `working` is running, `idle`, `done`, and `blocked` are idle, and
//! `agent_not_found` means nothing is registered in the pane.

use std::env;
use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use mcao_core::McaoError;
use mcao_core::backend::{AgentState, OpError, SessionBackend};
use mcao_core::evidence::Execution;
use serde::Deserialize;
use serde_json::json;

/// The request `id`, which names the request in the reply.
const REQUEST_ID: &str = "fm-agent-get";
/// The socket method that reads one agent's registration and status.
const AGENT_GET_METHOD: &str = "agent.get";
/// The error code herdr returns when no agent is registered in the target.
const AGENT_NOT_FOUND: &str = "agent_not_found";
/// The longest reply line accepted. Replies are small, so this only bounds a broken peer.
const REPLY_LIMIT: u64 = 1 << 20;
/// Budget for each socket write and read.
const IO_TIMEOUT: Duration = Duration::from_secs(5);
/// Budget for connecting to the control socket.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// The Herdr session backend. Only the agent-state read is implemented in this slice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HerdrBackend {
    socket: Option<PathBuf>,
}

impl HerdrBackend {
    /// A backend that talks to the control socket at `socket`.
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: Some(socket.into()),
        }
    }

    /// A backend whose socket is the path herdr injects as `HERDR_SOCKET_PATH` into its panes.
    /// With the variable unset or empty, every read fails with a message that names the socket.
    pub fn from_env() -> Self {
        Self::from_socket_var(env::var_os("HERDR_SOCKET_PATH"))
    }

    /// A backend for the `HERDR_SOCKET_PATH` value `value`. An unset or empty value leaves no
    /// socket.
    fn from_socket_var(value: Option<OsString>) -> Self {
        Self {
            socket: value.filter(|path| !path.is_empty()).map(PathBuf::from),
        }
    }

    /// Reports the state of the agent `agent_id`. An agent with no registration reads as
    /// [`AgentState::Missing`]. A socket that cannot be reached, or a reply that is not a valid
    /// status, is an error.
    pub fn get_agent_state(&self, agent_id: &str) -> Result<AgentState, McaoError> {
        let socket = self.socket.clone().ok_or_else(|| {
            McaoError::Backend("herdr socket path is not set (HERDR_SOCKET_PATH)".to_owned())
        })?;
        let connect_path = socket.clone();
        let stream = connect_within(move || UnixStream::connect(connect_path), CONNECT_TIMEOUT)
            .map_err(|error| {
                McaoError::Backend(format!(
                    "herdr socket {} is unavailable: {error}",
                    socket.display()
                ))
            })?;
        let request = agent_get_request(agent_id);
        let reply = round_trip(stream, &request).map_err(|error| {
            McaoError::Backend(format!(
                "herdr socket {} request failed: {error}",
                socket.display()
            ))
        })?;
        agent_state_from_reply(&reply)
    }
}

impl SessionBackend for HerdrBackend {
    fn name(&self) -> &'static str {
        "herdr"
    }

    fn execution(&self) -> Execution {
        Execution::Native
    }

    fn spawn(&mut self, _harness: &str) -> Result<String, OpError> {
        Err(not_implemented("spawn"))
    }

    fn capture(&self, _handle: &str) -> Result<String, OpError> {
        Err(not_implemented("capture"))
    }

    fn send_text(&mut self, _handle: &str, _text: &str) -> Result<(), OpError> {
        Err(not_implemented("send_text"))
    }

    fn interrupt(&mut self, _handle: &str) -> Result<(), OpError> {
        Err(not_implemented("interrupt"))
    }

    fn agent_state(&self, handle: &str) -> Result<AgentState, OpError> {
        self.get_agent_state(handle)
            .map_err(|error| OpError::Failed(error.to_string()))
    }

    fn reconnect(&self, _handle: &str) -> Result<(), OpError> {
        Err(not_implemented("reconnect"))
    }

    fn cleanup(&mut self, _handle: &str) -> Result<(), OpError> {
        Err(not_implemented("cleanup"))
    }
}

fn not_implemented(operation: &str) -> OpError {
    OpError::Unsupported(format!(
        "the Rust herdr backend does not implement {operation} yet (DPES-756)"
    ))
}

/// The `agent.get` request line for `agent_id`, without its newline.
fn agent_get_request(agent_id: &str) -> String {
    json!({
        "id": REQUEST_ID,
        "method": AGENT_GET_METHOD,
        "params": {"target": agent_id},
    })
    .to_string()
}

/// Runs `connect` on a helper thread and waits at most `timeout` for its stream. std has no
/// connect timeout. After a timeout the helper is not joined: it finishes or stays blocked on its
/// own, and any stream it later makes is dropped there.
fn connect_within<F>(connect: F, timeout: Duration) -> io::Result<UnixStream>
where
    F: FnOnce() -> io::Result<UnixStream> + Send + 'static,
{
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(connect());
    });
    match receiver.recv_timeout(timeout) {
        Ok(stream) => stream,
        Err(RecvTimeoutError::Timeout) => {
            Err(io::Error::new(io::ErrorKind::TimedOut, "connect timed out"))
        }
        Err(RecvTimeoutError::Disconnected) => {
            Err(io::Error::other("connect thread ended without a result"))
        }
    }
}

/// Sends `request` and returns the first reply line, without its newline.
fn round_trip(stream: UnixStream, request: &str) -> io::Result<Vec<u8>> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let mut writer = &stream;
    writer.write_all(format!("{request}\n").as_bytes())?;
    let mut reply = Vec::new();
    BufReader::new((&stream).take(REPLY_LIMIT)).read_until(b'\n', &mut reply)?;
    if reply.last() != Some(&b'\n') {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "no complete reply line before the connection ended or the size limit",
        ));
    }
    reply.pop();
    Ok(reply)
}

#[derive(Deserialize)]
struct AgentGetReply {
    result: Option<AgentGetResult>,
    error: Option<ReplyError>,
}

#[derive(Deserialize)]
struct AgentGetResult {
    agent: AgentRecord,
}

#[derive(Deserialize)]
struct AgentRecord {
    agent_status: String,
}

#[derive(Deserialize)]
struct ReplyError {
    code: String,
}

/// Maps one `agent.get` reply line onto an [`AgentState`].
fn agent_state_from_reply(line: &[u8]) -> Result<AgentState, McaoError> {
    let reply: AgentGetReply = serde_json::from_slice(line).map_err(|error| {
        McaoError::Backend(format!(
            "herdr socket reply is not an agent.get reply: {error}"
        ))
    })?;
    if let Some(error) = reply.error {
        return if error.code == AGENT_NOT_FOUND {
            Ok(AgentState::Missing)
        } else {
            Err(McaoError::Backend(format!(
                "herdr agent.get failed: {}",
                error.code
            )))
        };
    }
    let result = reply.result.ok_or_else(|| {
        McaoError::Backend("herdr socket reply has neither a result nor an error".to_owned())
    })?;
    match result.agent.agent_status.as_str() {
        "working" => Ok(AgentState::Running),
        "idle" | "done" | "blocked" => Ok(AgentState::Idle),
        other => Err(McaoError::Backend(format!(
            "herdr reported an unrecognized agent_status {other:?}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    #[test]
    fn an_unset_or_empty_socket_path_leaves_no_socket() {
        assert_eq!(HerdrBackend::from_socket_var(None).socket, None);
        assert_eq!(
            HerdrBackend::from_socket_var(Some(OsString::new())).socket,
            None
        );
    }

    #[test]
    fn a_read_without_a_socket_path_names_the_variable() {
        match HerdrBackend::from_socket_var(None).get_agent_state("w1:p1") {
            Err(McaoError::Backend(message)) => {
                assert!(message.contains("HERDR_SOCKET_PATH"), "{message}");
            }
            other => panic!("expected a backend error naming HERDR_SOCKET_PATH, got {other:?}"),
        }
    }

    #[test]
    fn a_connect_that_does_not_finish_in_time_is_a_timeout() {
        let started = Instant::now();

        let error = connect_within(
            || {
                thread::sleep(Duration::from_secs(5));
                Err(io::Error::other("finished late"))
            },
            Duration::from_millis(50),
        )
        .expect_err("the connect is not waited for past the timeout");

        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(4));
    }
}
