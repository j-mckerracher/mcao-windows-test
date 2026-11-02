//! Checks [`HerdrBackend::get_agent_state`] against a fake Herdr control socket. No test touches a
//! real herdr server.

#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use mcao_backend_unix::HerdrBackend;
use mcao_core::McaoError;
use mcao_core::backend::{AgentState, OpError, SessionBackend};

/// Distinguishes the directories of tests that run in the same process.
static NEXT_DIRECTORY: AtomicUsize = AtomicUsize::new(0);

/// A fake control socket that answers one connection with a fixed reply line. The thread returns
/// the request line it read, so a test can check the request shape.
struct FakeHerdr {
    directory: PathBuf,
    socket: PathBuf,
}

impl FakeHerdr {
    fn answering(reply: &str) -> (Self, JoinHandle<String>) {
        let index = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let directory =
            std::env::temp_dir().join(format!("mcao-herdr-{}-{index}", std::process::id()));
        fs::create_dir_all(&directory).expect("create the fake herdr directory");
        let socket = directory.join("herdr.sock");
        let listener = UnixListener::bind(&socket).expect("bind the fake herdr socket");
        let reply = reply.to_owned();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept one client");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("bound the fake server's read");
            let mut request = String::new();
            BufReader::new(stream.try_clone().expect("clone the stream"))
                .read_line(&mut request)
                .expect("read the request line");
            let mut writer: UnixStream = stream;
            writer
                .write_all(format!("{reply}\n").as_bytes())
                .expect("write the reply line");
            request
        });
        (Self { directory, socket }, server)
    }

    fn socket(&self) -> &Path {
        &self.socket
    }
}

impl Drop for FakeHerdr {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn agent_reply(status: &str) -> String {
    format!(
        r#"{{"id":"fm-agent-get","result":{{"type":"agent_info","agent":{{"agent":"claude","agent_status":"{status}"}}}}}}"#
    )
}

#[test]
fn an_idle_agent_reads_as_idle_and_the_request_names_the_agent() {
    let (fake, server) = FakeHerdr::answering(&agent_reply("idle"));
    let backend = HerdrBackend::new(fake.socket());

    let state = backend
        .get_agent_state("w1:p1")
        .expect("read the agent state");

    assert_eq!(state, AgentState::Idle);
    let request: serde_json::Value =
        serde_json::from_str(&server.join().expect("fake server ends")).expect("request is JSON");
    assert_eq!(request["method"], "agent.get");
    assert_eq!(request["params"]["target"], "w1:p1");
}

#[test]
fn a_working_agent_reads_as_running() {
    let (fake, server) = FakeHerdr::answering(&agent_reply("working"));

    let state = HerdrBackend::new(fake.socket())
        .get_agent_state("w1:p1")
        .expect("read the agent state");

    assert_eq!(state, AgentState::Running);
    server.join().expect("fake server ends");
}

#[test]
fn a_blocked_agent_reads_as_idle() {
    let (fake, server) = FakeHerdr::answering(&agent_reply("blocked"));

    let state = HerdrBackend::new(fake.socket())
        .get_agent_state("w1:p1")
        .expect("read the agent state");

    assert_eq!(state, AgentState::Idle);
    server.join().expect("fake server ends");
}

#[test]
fn a_done_agent_reads_as_idle() {
    let (fake, server) = FakeHerdr::answering(&agent_reply("done"));

    let state = HerdrBackend::new(fake.socket())
        .get_agent_state("w1:p1")
        .expect("read the agent state");

    assert_eq!(state, AgentState::Idle);
    server.join().expect("fake server ends");
}

#[test]
fn an_unregistered_agent_reads_as_missing() {
    let reply = r#"{"id":"fm-agent-get","error":{"code":"agent_not_found","message":"no agent"}}"#;
    let (fake, server) = FakeHerdr::answering(reply);

    let state = HerdrBackend::new(fake.socket())
        .get_agent_state("w1:p1")
        .expect("an unregistered agent is a state, not a failure");

    assert_eq!(state, AgentState::Missing);
    server.join().expect("fake server ends");
}

#[test]
fn an_unrecognized_status_is_an_error_not_idle() {
    let (fake, server) = FakeHerdr::answering(&agent_reply("stuck"));

    let result = HerdrBackend::new(fake.socket()).get_agent_state("w1:p1");

    assert!(matches!(result, Err(McaoError::Backend(message)) if message.contains("stuck")));
    server.join().expect("fake server ends");
}

#[test]
fn a_missing_socket_is_an_error_that_names_the_socket() {
    let directory = std::env::temp_dir().join(format!("mcao-herdr-absent-{}", std::process::id()));
    let backend = HerdrBackend::new(directory.join("absent.sock"));

    match backend.get_agent_state("w1:p1") {
        Err(McaoError::Backend(message)) => assert!(message.contains("socket"), "{message}"),
        other => panic!("expected a backend error naming the socket, got {other:?}"),
    }
}

#[test]
fn the_session_backend_reports_a_socket_failure_as_failed() {
    let directory = std::env::temp_dir().join(format!("mcao-herdr-trait-{}", std::process::id()));
    let backend = HerdrBackend::new(directory.join("absent.sock"));

    match backend.agent_state("w1:p1") {
        Err(OpError::Failed(message)) => assert!(message.contains("socket"), "{message}"),
        other => panic!("expected OpError::Failed naming the socket, got {other:?}"),
    }
}
