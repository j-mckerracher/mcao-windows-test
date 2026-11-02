//! Command-line contract for `mcao herdr workspace-move` (DPES-864, slice E): a workspace id that
//! starts with a dash is taken as written, an index with a sign is refused by the same parser as
//! any other bad index, and a request reaches a fake control socket in the form the reference
//! sends. The socket test is Unix-only, because Windows has no socket transport here.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::{fs, thread};

/// The reply a fake herdr sends: a `workspace_list` answer to the request.
const LIST_REPLY: &str =
    r#"{"id":"fm-workspace-move","result":{"type":"workspace_list","workspaces":[{"id":"ws-1"}]}}"#;

/// A scratch directory under `/tmp`, short enough for a Unix socket path, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let path = PathBuf::from("/tmp").join(format!("mcao-wm-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create the scratch directory");
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Serves one connection: records the request line, then writes [`LIST_REPLY`].
fn serve_once(socket: &Path) -> thread::JoinHandle<String> {
    let listener = UnixListener::bind(socket).expect("bind the fake control socket");
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept the client");
        let mut line = String::new();
        BufReader::new(stream.try_clone().expect("clone the stream"))
            .read_line(&mut line)
            .expect("read the request");
        stream
            .write_all(format!("{LIST_REPLY}\n").as_bytes())
            .expect("write the reply");
        line
    })
}

/// Runs `mcao herdr workspace-move <socket> <args...>` with colour output pinned off.
fn workspace_move(socket: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mcao"))
        .args(["herdr", "workspace-move"])
        .arg(socket)
        .args(args)
        .env_remove("CLICOLOR_FORCE")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .output()
        .expect("mcao runs")
}

#[test]
fn a_leading_dash_workspace_id_is_taken_as_written() {
    let dir = Scratch::new("dash");
    let socket = dir.0.join("herdr.sock");
    let fake = serve_once(&socket);
    let output = workspace_move(&socket, &["-ws-1", "0"]);
    // Unblocks the fake if the client never connected, so a failure reports instead of hanging.
    let _ = UnixStream::connect(&socket);
    let sent = fake.join().expect("fake control socket thread");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).expect("stdout is UTF-8"),
        format!("{LIST_REPLY}\n")
    );
    assert_eq!(
        sent,
        "{\"id\":\"fm-workspace-move\",\"method\":\"workspace.move\",\"params\":{\"workspace_id\":\"-ws-1\",\"insert_index\":0}}\n"
    );
}

#[test]
fn a_signed_index_is_refused_before_any_connection() {
    let dir = Scratch::new("sign");
    let socket = dir.0.join("herdr.sock");
    let output = workspace_move(&socket, &["ws-1", "-1"]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("insert index must be a plain non-negative integer"),
        "stderr: {stderr}"
    );
    assert!(!socket.exists(), "no socket was created or contacted");
}
