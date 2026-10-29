//! Tests for `mcao herdr workspace-move` (DPES-864, slice E).
//!
//! Every expected socket value comes from `tests/fixtures/herdr-workspace-move/`, which records the
//! pinned Python reference `bin/backends/herdr-workspace-move.py` against scripted fake sockets.
//! Its provenance.txt says how the fixtures were made. Nothing in this file generates an expected
//! value. The argument and reply checks are pure and run on every platform. The socket test runs a
//! fake herdr control socket that replays each case's replies, then checks stdout bytes, the exit
//! status, and the request line byte for byte. Windows has no socket transport here, so its
//! unsupported result is checked only on Windows.

mod support;

use std::io;

use mcao_core::herdr::workspace_move::{self, WorkspaceMoveError};

/// A reply that answers the request with a workspace list.
const LIST_REPLY: &str =
    r#"{"id":"fm-workspace-move","result":{"type":"workspace_list","workspaces":[{"id":"ws-1"}]}}"#;

#[test]
fn insert_index_accepts_only_plain_non_negative_digits() {
    assert_eq!(workspace_move::parse_insert_index("0").expect("zero"), 0);
    assert_eq!(
        workspace_move::parse_insert_index("12").expect("twelve"),
        12
    );
    for rejected in ["-1", "+1", "01", " 1", "1 ", "1_0", "1.0", "", "x"] {
        let err = workspace_move::parse_insert_index(rejected).expect_err(rejected);
        assert!(
            matches!(err, WorkspaceMoveError::Usage(_)),
            "index {rejected:?}"
        );
        assert_eq!(err.exit_code(), 2, "index {rejected:?}");
    }
}

#[test]
fn request_checks_socket_path_and_workspace_id_as_the_reference_does() {
    use mcao_core::herdr::workspace_move::WorkspaceMoveRequest;
    use std::path::PathBuf;

    let request = |socket: &str, workspace_id: &str| WorkspaceMoveRequest {
        socket_path: PathBuf::from(socket),
        workspace_id: workspace_id.to_owned(),
        insert_index: 0,
    };
    workspace_move::validate(&request("/tmp/herdr.sock", "ws-1")).expect("a valid request");
    for (socket, workspace_id) in [
        ("herdr.sock", "ws-1"),
        ("/tmp/herdr.sock", ""),
        ("/tmp/herdr.sock", "ws\t1"),
        ("/tmp/herdr.sock", "ws\r1"),
        ("/tmp/herdr.sock", "ws\n1"),
    ] {
        let err = workspace_move::validate(&request(socket, workspace_id))
            .expect_err("a malformed argument");
        assert_eq!(
            err.exit_code(),
            2,
            "socket {socket:?}, workspace {workspace_id:?}"
        );
    }
}

#[test]
fn only_a_workspace_list_reply_for_this_request_is_accepted() {
    let accepted_with_null_error = r#"{"id":"fm-workspace-move","error":null,"result":{"type":"workspace_list","workspaces":[]}}"#;
    for accepted in [LIST_REPLY, accepted_with_null_error] {
        assert_eq!(
            workspace_move::judge_reply(accepted.as_bytes()).expect("workspace list"),
            accepted
        );
    }
    for refused in [
        r#"{"id":"fm-workspace-move","error":{"code":"unknown_workspace"}}"#,
        r#"{"id":"other","result":{"type":"workspace_list","workspaces":[]}}"#,
        r#"{"id":"fm-workspace-move","result":{"type":"ok"}}"#,
        r#"{"id":"fm-workspace-move","result":{"type":"workspace_list","workspaces":{}}}"#,
        r#"["fm-workspace-move"]"#,
        "garbage",
    ] {
        let err = workspace_move::judge_reply(refused.as_bytes()).expect_err(refused);
        assert!(matches!(err, WorkspaceMoveError::Refused(_)), "{refused}");
        assert_eq!(err.exit_code(), 4, "{refused}");
    }
}

#[test]
fn failures_map_to_the_reference_exit_codes() {
    assert_eq!(WorkspaceMoveError::Usage("x".to_owned()).exit_code(), 2);
    assert_eq!(
        WorkspaceMoveError::Unreachable("x".to_owned()).exit_code(),
        2
    );
    assert_eq!(WorkspaceMoveError::Transport("x".to_owned()).exit_code(), 3);
    assert_eq!(WorkspaceMoveError::Refused("x".to_owned()).exit_code(), 4);
    assert_eq!(
        WorkspaceMoveError::Output(io::Error::from(io::ErrorKind::BrokenPipe)).exit_code(),
        3
    );
    assert_eq!(
        WorkspaceMoveError::Output(io::Error::other("x")).exit_code(),
        1
    );
    assert_eq!(WorkspaceMoveError::Unsupported.exit_code(), 7);
}

#[cfg(unix)]
mod socket {
    use std::fs;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::path::{Path, PathBuf};
    use std::sync::mpsc::{self, Sender};
    use std::thread::{self, JoinHandle};
    use std::time::Duration;

    use mcao_core::herdr::workspace_move::{self, WorkspaceMoveError, WorkspaceMoveRequest};
    use serde_json::Value;

    use crate::support::TempDir;

    /// Directory of recorded cases, relative to the crate root.
    const FIXTURES: &str = "tests/fixtures/herdr-workspace-move";

    /// One recorded case: the scripted input and what the reference did with it.
    struct Case {
        name: String,
        /// Arguments after the script name. `{socket}` stands for the control-socket path.
        args: Vec<String>,
        /// Scripted herdr output, sent byte for byte in order.
        replies: Vec<String>,
        /// Whether the fake server keeps the connection open until the client closes it.
        hold: bool,
        stdout: Vec<u8>,
        exit: u8,
        /// The request line the reference sent, newline included. Present only when it connected.
        request: Option<Vec<u8>>,
    }

    fn recorded_cases() -> Vec<Case> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURES);
        let mut dirs: Vec<PathBuf> = fs::read_dir(&root)
            .unwrap_or_else(|error| panic!("read {}: {error}", root.display()))
            .map(|entry| entry.expect("read a fixture entry").path())
            .filter(|path| path.is_dir())
            .collect();
        dirs.sort();
        dirs.iter().map(|dir| load_case(dir)).collect()
    }

    fn load_case(dir: &Path) -> Case {
        let read = |file: &str| {
            let path = dir.join(file);
            fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
        };
        let input: Value = serde_json::from_slice(&read("case.json")).expect("case.json is JSON");
        let args = input["args"]
            .as_array()
            .expect("case.json has an args array")
            .iter()
            .map(|arg| arg.as_str().expect("each argument is a string").to_owned())
            .collect();
        let replies = input["replies"]
            .as_array()
            .expect("case.json has a replies array")
            .iter()
            .map(|reply| reply.as_str().expect("each reply is a string").to_owned())
            .collect();
        let hold = match input["end"].as_str() {
            Some("hold") => true,
            Some("close" | "absent") => false,
            other => panic!("unknown end mode {other:?} in {}", dir.display()),
        };
        let exit = String::from_utf8(read("exit"))
            .expect("exit is UTF-8")
            .trim()
            .parse()
            .expect("exit is a status code");
        let request = dir.join("request").exists().then(|| read("request"));
        Case {
            name: dir
                .file_name()
                .expect("case directory has a name")
                .to_string_lossy()
                .into_owned(),
            args,
            replies,
            hold,
            stdout: read("stdout"),
            exit,
            request,
        }
    }

    /// A fake control socket that serves one connection. It records the request line, writes
    /// `replies` byte for byte in order, then closes the connection. With `hold` set it keeps the
    /// connection open until [`FakeHerdr::finish`] releases it.
    struct FakeHerdr {
        request: JoinHandle<String>,
        release: Sender<()>,
    }

    impl FakeHerdr {
        fn start(socket: &Path, replies: &[String], hold: bool) -> Self {
            let listener = UnixListener::bind(socket).expect("bind the fake control socket");
            let replies = replies.to_vec();
            let (release, released) = mpsc::channel::<()>();
            let request = thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept the client");
                let mut line = String::new();
                BufReader::new(stream.try_clone().expect("clone the stream"))
                    .read_line(&mut line)
                    .expect("read the request");
                for reply in &replies {
                    stream.write_all(reply.as_bytes()).expect("write a reply");
                }
                if hold {
                    let _ = released.recv_timeout(Duration::from_secs(10));
                }
                line
            });
            Self { request, release }
        }

        /// Releases a held connection and returns the request line the server received.
        fn finish(self) -> String {
            drop(self.release);
            self.request.join().expect("fake control socket thread")
        }
    }

    /// Runs one recorded case through the socket client and returns every mismatch with the
    /// recording, or `None` when the client matches it.
    fn check_case(case: &Case) -> Option<String> {
        let dir = TempDir::new("wm");
        let socket: PathBuf = dir.path().join("herdr.sock");
        let socket_arg = socket.display().to_string();
        let argv: Vec<String> = case
            .args
            .iter()
            .map(|arg| {
                if arg == "{socket}" {
                    socket_arg.clone()
                } else {
                    arg.clone()
                }
            })
            .collect();

        let fake = case
            .request
            .as_ref()
            .map(|_| FakeHerdr::start(&socket, &case.replies, case.hold));

        let mut out = Vec::new();
        let outcome = run_argv(&argv, &mut out);
        let sent = fake.map(FakeHerdr::finish);

        let exit = match &outcome {
            Ok(()) => 0,
            Err(err) => err.exit_code(),
        };
        let mut problems = Vec::new();
        if out != case.stdout {
            problems.push(format!(
                "stdout {:?} differs from the reference {:?}",
                String::from_utf8_lossy(&out),
                String::from_utf8_lossy(&case.stdout)
            ));
        }
        if exit != case.exit {
            problems.push(format!(
                "exit {exit} differs from the reference {}",
                case.exit
            ));
        }
        match (&case.request, &sent) {
            (Some(recorded), Some(sent)) if sent.as_bytes() != recorded.as_slice() => {
                problems.push(format!(
                    "request {sent:?} differs from the reference {:?}",
                    String::from_utf8_lossy(recorded)
                ));
            }
            (Some(_), None) | (None, Some(_)) => {
                problems.push("the client and the reference disagree on connecting".to_owned());
            }
            _ => {}
        }
        (!problems.is_empty()).then(|| problems.join("; "))
    }

    /// Mirrors `mcao herdr workspace-move` argument handling: the socket path, the workspace id,
    /// and the insert index, in that order.
    fn run_argv(argv: &[String], out: &mut Vec<u8>) -> Result<(), WorkspaceMoveError> {
        let insert_index = workspace_move::parse_insert_index(&argv[2])?;
        let request = WorkspaceMoveRequest {
            socket_path: PathBuf::from(&argv[0]),
            workspace_id: argv[1].clone(),
            insert_index,
        };
        workspace_move::run(&request, out)
    }

    #[test]
    fn recorded_cases_are_present() {
        let cases = recorded_cases();
        assert!(
            cases.len() >= 20,
            "expected the recorded cases, found {}",
            cases.len()
        );
        for case in &cases {
            // Exit 0, 2, 3, and 4 are the reference's documented codes; anything else is a crash.
            assert!(
                matches!(case.exit, 0 | 2 | 3 | 4),
                "{} records exit {}, which the reference never returns",
                case.name,
                case.exit
            );
            assert_eq!(
                case.exit == 0,
                !case.stdout.is_empty(),
                "{} stdout must hold the reply exactly when it exits 0",
                case.name
            );
        }
    }

    #[test]
    fn socket_client_matches_every_recorded_reference_case() {
        let mut failures = Vec::new();
        let cases = recorded_cases();
        for case in &cases {
            if let Some(problem) = check_case(case) {
                failures.push(format!("{}: {problem}", case.name));
            }
        }
        assert!(!cases.is_empty(), "no recorded cases to check");
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
}

#[cfg(not(unix))]
mod windows {
    use std::path::PathBuf;

    use mcao_core::herdr::workspace_move::{self, WorkspaceMoveError, WorkspaceMoveRequest};

    #[test]
    fn workspace_move_reports_unsupported_without_touching_a_socket() {
        let request = WorkspaceMoveRequest {
            socket_path: PathBuf::from("herdr.sock"),
            workspace_id: "ws-1".to_owned(),
            insert_index: 0,
        };
        let mut out = Vec::new();
        let err =
            workspace_move::run(&request, &mut out).expect_err("no socket transport on Windows");
        assert!(matches!(err, WorkspaceMoveError::Unsupported));
        assert_eq!(err.exit_code(), 7);
        assert!(out.is_empty());
    }
}
