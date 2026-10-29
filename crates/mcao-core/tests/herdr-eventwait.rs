//! Tests for the herdr wire and `mcao herdr eventwait` (DPES-864, slice D; DPES-912 parity).
//!
//! Every expected value comes from `tests/fixtures/herdr-eventwait/`, which records the output of
//! the pinned Python reference `bin/backends/herdr-eventwait.py` against scripted herdr replies.
//! Its provenance.txt says how the fixtures were made. Nothing in this file generates an expected
//! value.
//!
//! The wire tests run on every platform. The socket test runs a fake herdr control socket that
//! replays each case's replies, then checks stdout bytes, the exit status, and the subscribe
//! request the client sent. Windows has no socket transport here, so its unsupported result is
//! checked only when the suite runs on Windows.

mod support;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use mcao_core::herdr::eventwait::{self, EventwaitError};
use mcao_core::herdr::wire;
use serde_json::Value;

/// Directory of recorded cases, relative to the crate root.
const FIXTURES: &str = "tests/fixtures/herdr-eventwait";

/// One recorded case: the scripted input and what the reference did with it.
struct Case {
    name: String,
    /// Arguments after the script name. `{socket}` stands for the control-socket path.
    args: Vec<String>,
    /// Scripted herdr output, one entry per line: a string is sent raw, any other JSON value
    /// is sent as compact JSON.
    replies: Vec<Value>,
    /// Whether the fake server keeps the connection open until the client closes it.
    #[cfg_attr(not(unix), allow(dead_code))]
    hold: bool,
    stdout: Vec<u8>,
    exit: u8,
    /// The subscribe line the reference sent, present only when it connected.
    request: Option<String>,
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
        .clone();
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
    let request = dir.join("request").exists().then(|| {
        String::from_utf8(read("request"))
            .expect("request is UTF-8")
            .trim_end()
            .to_owned()
    });
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

/// A scripted reply as the line the fake server sends.
fn reply_line(reply: &Value) -> String {
    match reply {
        Value::String(raw) => raw.clone(),
        other => other.to_string(),
    }
}

/// The request as parsed JSON. The reference spaces its JSON differently from the compact form
/// the client sends, so the protocol comparison is on values, not bytes.
fn parse_request(line: &str) -> Value {
    serde_json::from_str(line.trim_end()).expect("the subscribe request is JSON")
}

/// The panes a recorded case subscribed to: every argument after the socket and timeout.
fn panes_of(case: &Case) -> Vec<String> {
    case.args[2..].to_vec()
}

#[test]
fn recorded_cases_are_present() {
    let cases = recorded_cases();
    assert!(
        cases.len() >= 10,
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
    }
}

#[test]
fn subscribe_request_matches_the_reference_request() {
    let mut checked = 0;
    for case in recorded_cases() {
        let Some(recorded) = &case.request else {
            continue;
        };
        let built = wire::subscribe_request(&panes_of(&case));
        assert_eq!(
            parse_request(&built),
            parse_request(recorded),
            "case {}",
            case.name
        );
        checked += 1;
    }
    assert!(checked > 0, "no recorded case shows a subscribe request");
}

#[test]
fn projection_matches_the_reference_output_lines() {
    let mut checked = 0;
    for case in recorded_cases() {
        let Ok(output) = std::str::from_utf8(&case.stdout) else {
            panic!("case {} stdout is not UTF-8", case.name);
        };
        let Some(body) = output.strip_prefix("@subscribed\n") else {
            continue;
        };
        let projected: Vec<String> = case
            .replies
            .iter()
            .filter_map(|reply| wire::project_event(reply_line(reply).as_bytes()))
            .map(|event| event.to_line())
            .collect();
        let expected: Vec<&str> = body.lines().collect();
        assert_eq!(projected, expected, "case {}", case.name);
        checked += 1;
    }
    assert!(checked > 0, "no recorded case shows projected lines");
}

#[test]
fn only_a_started_subscription_is_acknowledged() {
    assert!(wire::is_subscription_started(
        br#"{"id":"fm-eventwait","result":{"type":"subscription_started"}}"#
    ));
    assert!(!wire::is_subscription_started(
        br#"{"id":"fm-eventwait","error":{"code":"unknown_pane"}}"#
    ));
    assert!(!wire::is_subscription_started(
        br#"{"result":{"type":"other"}}"#
    ));
    assert!(!wire::is_subscription_started(b"not json"));
    assert!(!wire::is_subscription_started(b"[]"));
}

#[test]
fn timeout_accepts_positive_seconds_only() {
    assert_eq!(
        eventwait::parse_timeout(" 2 ").expect("two seconds"),
        Duration::from_secs(2)
    );
    assert_eq!(
        eventwait::parse_timeout("0.5").expect("half a second"),
        Duration::from_millis(500)
    );
    for rejected in ["0", "-1", "nan", "inf", "soon", ""] {
        let err = eventwait::parse_timeout(rejected).expect_err(rejected);
        assert_eq!(err.exit_code(), 2, "timeout {rejected:?}");
    }
}

#[test]
fn failures_map_to_the_reference_exit_codes() {
    assert_eq!(EventwaitError::Usage("x".to_owned()).exit_code(), 2);
    assert_eq!(EventwaitError::Unreachable("x".to_owned()).exit_code(), 2);
    assert_eq!(EventwaitError::NotSubscribed.exit_code(), 3);
    assert_eq!(EventwaitError::StreamLost("x".to_owned()).exit_code(), 4);
    assert_eq!(EventwaitError::Output(io::Error::other("x")).exit_code(), 1);
    assert_eq!(EventwaitError::Unsupported.exit_code(), 7);
}

#[cfg(unix)]
mod socket {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::path::{Path, PathBuf};
    use std::sync::mpsc::{self, Sender};
    use std::thread::{self, JoinHandle};
    use std::time::Duration;

    use mcao_core::herdr::eventwait::{self, EventwaitError, EventwaitRequest};

    use super::{Case, parse_request, recorded_cases, reply_line};
    use crate::support::TempDir;

    /// A fake control socket that serves one connection. It records the request line, writes
    /// `replies` in order, then closes the connection. With `hold` set it keeps the connection
    /// open until [`FakeHerdr::finish`] releases it.
    struct FakeHerdr {
        request: JoinHandle<String>,
        release: Sender<()>,
    }

    impl FakeHerdr {
        fn start(socket: &Path, replies: &[String], hold: bool) -> Self {
            let listener = UnixListener::bind(socket).expect("bind the fake control socket");
            let replies: Vec<String> = replies.iter().map(|reply| format!("{reply}\n")).collect();
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
        let dir = TempDir::new("ew");
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

        let replies: Vec<String> = case.replies.iter().map(reply_line).collect();
        let fake = case
            .request
            .as_ref()
            .map(|_| FakeHerdr::start(&socket, &replies, case.hold));

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
        if let (Some(recorded), Some(sent)) = (&case.request, &sent)
            && parse_request(sent) != parse_request(recorded)
        {
            problems.push(format!(
                "request {sent:?} differs from the reference {recorded:?}"
            ));
        }
        (!problems.is_empty()).then(|| problems.join("; "))
    }

    /// Mirrors `mcao herdr eventwait` argument handling: the timeout is parsed first, then the
    /// remaining arguments are the panes.
    fn run_argv(argv: &[String], out: &mut Vec<u8>) -> Result<(), EventwaitError> {
        let timeout = eventwait::parse_timeout(&argv[1])?;
        let request = EventwaitRequest {
            socket_path: PathBuf::from(&argv[0]),
            timeout,
            panes: argv[2..].to_vec(),
        };
        eventwait::run(&request, out)
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
    use std::time::Duration;

    use mcao_core::herdr::eventwait::{self, EventwaitError, EventwaitRequest};

    #[test]
    fn eventwait_reports_unsupported_without_touching_a_socket() {
        let request = EventwaitRequest {
            socket_path: PathBuf::from("herdr.sock"),
            timeout: Duration::from_secs(1),
            panes: vec!["1-1".to_owned()],
        };
        let mut out = Vec::new();
        let err = eventwait::run(&request, &mut out).expect_err("no socket transport on Windows");
        assert!(matches!(err, EventwaitError::Unsupported));
        assert_eq!(err.exit_code(), 7);
        assert!(out.is_empty());
    }
}
