//! Integration tests for the ConPTY session: output capture and text input.
//!
//! The child is this test binary re-run with `--exact conpty_stub_echo --ignored`,
//! so the tests need no shell and no extra binary. The stub prints a ready marker,
//! then answers each input line with `got:<line>`. That answer exists only when
//! the child ran and read the line, which a terminal echo of the typed text alone
//! cannot fake. The stub test is ignored and runs only through that re-exec.

#![cfg(windows)]

use std::env;
use std::io::{self, BufRead, BufReader};
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::process::{ChildStdout, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use mcao_backend_windows::conpty::{ConsoleSize, console_window_size, init_console_encoding};
use mcao_backend_windows::job::{JobObject, probe_named};
use mcao_backend_windows::process::{agent_evidence, probe_pid};
use mcao_backend_windows::session::{MIN_INPUT_WAIT, Session};
use mcao_core::process_identity::{JobProbe, PidProbe, ProcessEvidence, ProcessIdentity};
use mcao_core::terminal::resolve_window_size;
use windows_sys::Win32::System::Console::{
    GetConsoleCP, GetConsoleOutputCP, SetConsoleCP, SetConsoleOutputCP,
};

/// Long enough for a loaded CI runner to start the child and answer a line.
const WAIT: Duration = Duration::from_secs(20);
/// A pseudoconsole wide enough that the short lines below never wrap.
const SIZE: ConsoleSize = ConsoleSize {
    columns: 120,
    rows: 30,
};
/// The stub's exit code when it is told to quit.
const QUIT_EXIT: u32 = 0;
/// The environment variable that names the job the orchestrator stand-in creates.
const JOB_NAME_ENV: &str = "MCAO_TEST_JOB_NAME";
/// How long a worker has to end after its orchestrator exits.
const SETTLE: Duration = Duration::from_secs(15);

fn this_exe() -> PathBuf {
    env::current_exe().expect("test binary path is available")
}

const STUB_ARGS: [&str; 5] = [
    "--exact",
    "conpty_stub_echo",
    "--ignored",
    "--nocapture",
    "--test-threads=1",
];

/// A session whose job is terminated when it drops, so a failed assertion cannot
/// leak the stub.
struct Running(Session);

impl Deref for Running {
    type Target = Session;

    fn deref(&self) -> &Session {
        &self.0
    }
}

impl DerefMut for Running {
    fn deref_mut(&mut self) -> &mut Session {
        &mut self.0
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        // Best effort: the job may already be empty.
        let _ = self.0.job().terminate(1);
    }
}

/// Ends the named job when it drops, if the job still exists. Used by a test whose
/// worker outlives its session, so the worker cannot leak.
struct EndJob(String);

impl Drop for EndJob {
    fn drop(&mut self) {
        if let Ok(job) = JobObject::open(&self.0) {
            let _ = job.terminate(1);
        }
    }
}

/// Reports whether the process named by `identity` still runs. The creation time must
/// match, so a PID reused by another process does not count.
fn worker_is_running(identity: ProcessIdentity) -> bool {
    matches!(
        probe_pid(identity.pid),
        PidProbe::Found { start, running: true } if start == identity.start
    )
}

/// Starts the stub and waits for its ready marker.
fn start_stub() -> Running {
    let session = Session::spawn(&this_exe(), &STUB_ARGS, SIZE).expect("the stub starts");
    let running = Running(session);
    expect_output(&running, "conpty-stub-ready");
    running
}

/// Waits for `needle` in the output, and fails with the capture if it never comes.
fn expect_output(session: &Session, needle: &str) {
    let found = session
        .wait_for_output(needle.as_bytes(), WAIT)
        .expect("the output buffer is readable");
    let capture = session.capture().expect("the output buffer is readable");
    assert!(
        found,
        "output never contained {needle:?}; closed={}, error={:?}, captured {:?}",
        capture.closed,
        capture.read_error,
        capture.text()
    );
}

#[test]
#[ignore = "runs only as the child of the ConPTY tests"]
fn conpty_stub_echo() {
    println!("conpty-stub-ready");
    let mut line = String::new();
    loop {
        line.clear();
        if io::stdin().read_line(&mut line).expect("stdin reads") == 0 {
            return;
        }
        let text = line.trim();
        if text == "quit" {
            return;
        }
        println!("got:{text}");
    }
}

/// The line the UTF-8 stub prints, wrapped in a colour sequence.
const UTF8_LINE: &str = "héllo ✓";

#[test]
#[ignore = "runs only as the child of the ConPTY tests"]
fn conpty_stub_utf8() {
    // libtest writes `test <name> ... ` without a line end before the test runs, so
    // the first line of output would otherwise carry that prefix. The blank line ends it.
    println!();
    println!("\u{1b}[32m{UTF8_LINE}\u{1b}[0m");
    println!("conpty-utf8-ready");
    std::thread::sleep(Duration::from_secs(120));
}

#[test]
#[ignore = "runs only as the child of the ConPTY tests"]
fn conpty_stub_size() {
    // The blank line ends the libtest prefix, as in the UTF-8 stub.
    println!();
    match console_window_size() {
        Some((columns, rows)) => println!("conpty-size:{columns}x{rows}"),
        None => println!("conpty-size:none"),
    }
    thread::sleep(Duration::from_secs(120));
}

/// The UTF-8 code page that `init_console_encoding` must set for input and output.
const CODE_PAGE_UTF8: u32 = 65001;
/// A code page that is not UTF-8. The codepage stub sets it before calling the init, so
/// the test cannot pass on a console that already starts in UTF-8.
const NON_UTF8_CODE_PAGE: u32 = 437;

/// Reads the console's input and output code pages as `(input, output)`.
fn console_code_pages() -> (u32, u32) {
    // SAFETY: GetConsoleCP takes no arguments and reads no memory.
    let input = unsafe { GetConsoleCP() };
    // SAFETY: GetConsoleOutputCP takes no arguments and reads no memory.
    let output = unsafe { GetConsoleOutputCP() };
    (input, output)
}

#[test]
#[ignore = "runs only as the child of the ConPTY tests"]
fn conpty_stub_codepage() {
    // The blank line ends the libtest prefix, as in the UTF-8 stub.
    println!();
    // SAFETY: SetConsoleCP takes a code page value and no pointers. It changes the input
    // code page of the console this child is attached to, which is the test's own
    // pseudoconsole.
    let input_set = unsafe { SetConsoleCP(NON_UTF8_CODE_PAGE) };
    // SAFETY: SetConsoleOutputCP takes a code page value and no pointers. It changes the
    // output code page of the same console.
    let output_set = unsafe { SetConsoleOutputCP(NON_UTF8_CODE_PAGE) };
    assert_ne!(
        input_set, 0,
        "the child's console accepts an input code page"
    );
    assert_ne!(
        output_set, 0,
        "the child's console accepts an output code page"
    );
    let (input_before, output_before) = console_code_pages();
    assert_eq!(
        (input_before, output_before),
        (NON_UTF8_CODE_PAGE, NON_UTF8_CODE_PAGE),
        "the starting code page took effect before the init runs"
    );
    init_console_encoding().expect("the child's console accepts UTF-8");
    let (input_after, output_after) = console_code_pages();
    println!(
        "conpty-codepage:before={input_before}/{output_before} after={input_after}/{output_after}"
    );
    println!("conpty-codepage-ready");
    thread::sleep(Duration::from_secs(120));
}

#[test]
#[ignore = "runs only as the orchestrator stand-in of the orchestrator-exit test"]
fn conpty_stub_orchestrator() {
    let name = env::var(JOB_NAME_ENV).expect("the test names the job");
    let session = Session::spawn_named(&this_exe(), &STUB_ARGS, SIZE, &name, None)
        .expect("the worker starts");
    assert!(
        session
            .wait_for_output(b"conpty-stub-ready", WAIT)
            .expect("the output buffer is readable"),
        "the worker never became ready"
    );
    let identity = session
        .child()
        .identity()
        .expect("the worker identity is readable");
    // The blank line ends the libtest prefix, as in the UTF-8 stub.
    println!();
    println!(
        "worker-identity {} {} {}",
        identity.pid, identity.start, name
    );
    // Exit without dropping the session. The process ends and the OS closes its handles,
    // which is what an orchestrator that exits or crashes does.
    std::process::exit(0);
}

#[test]
fn capture_is_plain_utf8_with_lf_line_endings_and_no_escape_sequences() {
    let session = Session::spawn(
        &this_exe(),
        &[
            "--exact",
            "conpty_stub_utf8",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ],
        SIZE,
    )
    .expect("the stub starts");
    let running = Running(session);
    expect_output(&running, "conpty-utf8-ready");

    let text = running.capture().expect("the buffer is readable").text();
    assert!(
        text.lines().any(|line| line.trim_end() == UTF8_LINE),
        "the multibyte line survives as plain text: {text:?}"
    );
    assert!(!text.contains('\r'), "no CR survives: {text:?}");
    assert!(!text.contains('\u{1b}'), "no escape survives: {text:?}");
}

#[test]
fn output_is_captured_and_text_reaches_the_child() {
    let mut session = start_stub();

    session.send_text("hello\r").expect("input is accepted");
    expect_output(&session, "got:hello");

    session.send_text("quit\r").expect("input is accepted");
    let exit = session
        .child()
        .wait_timeout(WAIT)
        .expect("the wait succeeds");
    assert_eq!(exit, Some(QUIT_EXIT), "the stub exits after quit");
}

#[test]
fn text_is_sent_as_utf8() {
    let mut session = start_stub();

    session.send_text("héllo ✓\r").expect("input is accepted");
    expect_output(&session, "got:héllo ✓");
}

#[test]
fn text_sent_in_pieces_is_one_line() {
    let mut session = start_stub();

    session.send_text("par").expect("input is accepted");
    session.send_text("tial\r").expect("input is accepted");
    expect_output(&session, "got:partial");
}

#[test]
fn empty_text_is_accepted_and_changes_nothing() {
    let mut session = start_stub();

    session.send_text("").expect("empty input is accepted");
    session.send_text("after\r").expect("input is accepted");
    expect_output(&session, "got:after");
}

#[test]
fn capture_is_a_snapshot_that_grows() {
    let mut session = start_stub();
    let before = session.capture().expect("the buffer is readable");

    session.send_text("grow\r").expect("input is accepted");
    expect_output(&session, "got:grow");
    let after = session.capture().expect("the buffer is readable");

    assert!(after.bytes.len() > before.bytes.len());
    assert!(
        !before.text().contains("got:grow"),
        "an old snapshot is unchanged"
    );
    assert_eq!(after.discarded, 0, "this small run drops nothing");
}

#[test]
fn the_child_runs_inside_the_session_job() {
    let session = start_stub();

    let live = session
        .job()
        .active_processes()
        .expect("the job reports its members");
    assert!(live >= 1, "the stub counts as a member of the job");
}

#[test]
fn waiting_for_missing_output_times_out_with_false() {
    let session = start_stub();

    let started = Instant::now();
    let found = session
        .wait_for_output(b"never-printed-marker", Duration::from_millis(300))
        .expect("the buffer is readable");

    assert!(!found);
    assert!(started.elapsed() >= Duration::from_millis(300));
}

#[test]
fn dropping_a_session_with_a_terminated_job_returns_promptly() {
    let session = start_stub();
    session.job().terminate(1).expect("the job terminates");

    let started = Instant::now();
    drop(session);

    // The reader is given 5 seconds to finish. Returning well inside that shows the
    // output stream ended when the console closed, not that the wait ran out.
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "drop took {:?}",
        started.elapsed()
    );
}

#[test]
fn a_named_session_job_is_reachable_by_its_name() {
    let name = format!(r"Local\mcao-test-{}-named-session", std::process::id());
    let session = Session::spawn_named(&this_exe(), &STUB_ARGS, SIZE, &name, None)
        .expect("the named session starts");
    let running = Running(session);
    expect_output(&running, "conpty-stub-ready");

    assert!(
        matches!(probe_named(&name), JobProbe::Members(1..)),
        "the job holds the stub under its name"
    );
    let duplicate = Session::spawn_named(&this_exe(), &STUB_ARGS, SIZE, &name, None)
        .expect_err("a second job with the same name is refused");
    assert_eq!(duplicate.kind(), io::ErrorKind::AlreadyExists);
}

/// Reads the line the orchestrator stand-in prints to name its worker.
fn worker_identity_from(stdout: ChildStdout) -> ProcessIdentity {
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader
            .read_line(&mut line)
            .expect("the stand-in's output is readable");
        assert_ne!(read, 0, "the stand-in exited without naming its worker");
        if let Some((_, fields)) = line.split_once("worker-identity ") {
            let mut parts = fields.split_whitespace();
            let pid = parts
                .next()
                .and_then(|text| text.parse().ok())
                .expect("the worker pid is numeric");
            let start = parts
                .next()
                .and_then(|text| text.parse().ok())
                .expect("the worker creation time is numeric");
            return ProcessIdentity { pid, start };
        }
    }
}

#[test]
fn a_worker_reports_its_true_state_after_its_orchestrator_exits() {
    // The orchestrator stand-in exits without dropping its session, so its pseudoconsole
    // closes with the process. On Windows ConPTY the attached worker ends with it, and
    // this test asserts that outcome: the worker is not running, and the probes report it
    // as Dead or Missing rather than Alive. The observed state goes to stderr through
    // eprintln!, which libtest shows only for a failing test, so a passing run keeps the
    // assertions as its only record.
    let name = format!(r"Local\mcao-test-{}-orchestrator-exit", std::process::id());
    let _end = EndJob(name.clone());
    let mut orchestrator = Command::new(this_exe())
        .args([
            "--exact",
            "conpty_stub_orchestrator",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(JOB_NAME_ENV, &name)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("the orchestrator stand-in starts");
    let stdout = orchestrator
        .stdout
        .take()
        .expect("the stand-in's output is piped");
    let identity = worker_identity_from(stdout);
    let status = orchestrator
        .wait()
        .expect("the stand-in's exit is observed");
    assert!(status.success(), "the stand-in exits cleanly: {status:?}");

    // Give a worker that is ending with its console time to finish exiting, so the
    // probes below see its settled state rather than a racing one.
    let deadline = Instant::now() + SETTLE;
    while worker_is_running(identity) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(250));
    }

    let running = worker_is_running(identity);
    let pid_state = probe_pid(identity.pid);
    let job_state = probe_named(&name);
    let evidence = agent_evidence(identity, &name);
    eprintln!(
        "observed after the orchestrator exited: the worker {}; pid {pid_state:?}, job {job_state:?}, evidence {evidence:?}",
        if running {
            "is still running"
        } else {
            "has ended"
        }
    );
    assert!(
        !running,
        "the worker must end with its orchestrator on ConPTY: pid {pid_state:?}, job {job_state:?}, evidence {evidence:?}"
    );
    assert!(
        matches!(evidence, ProcessEvidence::Dead | ProcessEvidence::Missing),
        "the probes must report the ended worker as Dead or Missing: pid {pid_state:?}, job {job_state:?}, evidence {evidence:?}"
    );
}

#[test]
fn a_named_workers_identity_probes_alive_then_dead_and_a_reused_pid_is_ambiguous() {
    let name = format!(r"Local\mcao-test-{}-identity", std::process::id());
    let session = Session::spawn_named(&this_exe(), &STUB_ARGS, SIZE, &name, None)
        .expect("the named session starts");
    let running = Running(session);
    expect_output(&running, "conpty-stub-ready");

    let identity = running
        .child()
        .identity()
        .expect("the child's identity is readable");
    assert_eq!(
        identity.pid,
        running.child().id(),
        "the identity names the child"
    );

    // The creation time comes from the child's own handle. It must differ from this test
    // process's creation time, which a read of the current process would return.
    match probe_pid(std::process::id()) {
        PidProbe::Found { start, .. } => assert_ne!(
            start, identity.start,
            "the child's creation time is not the test process's"
        ),
        other => panic!("this test process is probed as {other:?}"),
    }

    assert_eq!(
        probe_pid(identity.pid),
        PidProbe::Found {
            start: identity.start,
            running: true,
        },
        "the probe finds the child with its recorded creation time"
    );
    assert_eq!(agent_evidence(identity, &name), ProcessEvidence::Alive);

    // A recorded creation time that no longer matches a live PID is a reused PID. The
    // job still holds the process, so the evidence is Ambiguous, never Dead.
    let reused = ProcessIdentity {
        pid: identity.pid,
        start: identity.start + 1,
    };
    assert_eq!(
        agent_evidence(reused, &name),
        ProcessEvidence::Ambiguous,
        "a reused PID with live members is ambiguous"
    );

    running.job().terminate(1).expect("the job terminates");
    let exit = running
        .child()
        .wait_timeout(WAIT)
        .expect("the wait succeeds");
    assert_eq!(exit, Some(1), "the child ends with the job");
    assert_eq!(
        agent_evidence(identity, &name),
        ProcessEvidence::Dead,
        "an ended child with an empty job is Dead"
    );
}

#[test]
fn a_session_sized_by_the_size_rule_opens_at_that_size() {
    // resolve_window_size takes the SIZE value as an argument, so this test does not
    // change the process environment that other tests read.
    let (columns, rows) = resolve_window_size(Some("100x33"), None);
    assert_eq!((columns, rows), (100, 33), "a valid SIZE is used as given");

    let session = Session::spawn(
        &this_exe(),
        &[
            "--exact",
            "conpty_stub_size",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ],
        ConsoleSize { columns, rows },
    )
    .expect("the size stub starts");
    let running = Running(session);
    expect_output(&running, "conpty-size:100x33");
}

/// Runs the code page init inside the child, on the child's own ConPTY console. Production runs the
/// init in the orchestrator instead, so this test does not show that `ConPty::open` calls it.
#[test]
fn the_init_sets_utf8_code_pages_on_a_conpty_console() {
    let session = Session::spawn(
        &this_exe(),
        &[
            "--exact",
            "conpty_stub_codepage",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ],
        SIZE,
    )
    .expect("the code page stub starts");
    let running = Running(session);
    expect_output(&running, "conpty-codepage-ready");

    let text = running.capture().expect("the buffer is readable").text();
    let report = text
        .lines()
        .find(|line| line.contains("conpty-codepage:"))
        .expect("the stub reports its code pages")
        .trim_end();
    assert!(
        report.ends_with(&format!("after={CODE_PAGE_UTF8}/{CODE_PAGE_UTF8}")),
        "init_console_encoding leaves the worker console on UTF-8 for input and output: {report:?}"
    );
}

#[test]
fn a_relative_program_is_refused_and_leaves_nothing_running() {
    let error = Session::spawn(Path::new("relative.exe"), &[], SIZE)
        .expect_err("a relative program is refused");

    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn a_missing_program_is_reported_and_the_session_cleans_up() {
    let missing = env::temp_dir().join("mcao-conpty-no-such-program.exe");
    let started = Instant::now();

    let error = Session::spawn(&missing, &[], SIZE).expect_err("a missing program fails");

    assert_eq!(error.kind(), io::ErrorKind::NotFound);
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "cleanup took {:?}",
        started.elapsed()
    );
}

#[test]
fn a_bad_console_size_is_refused() {
    let error = Session::spawn(
        &this_exe(),
        &STUB_ARGS,
        ConsoleSize {
            columns: 0,
            rows: 30,
        },
    )
    .expect_err("a zero-width console is refused");

    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
}

/// The arguments that start the stub which prints its ready line and never reads input.
const SILENT_ARGS: [&str; 5] = [
    "--exact",
    "conpty_stub_utf8",
    "--ignored",
    "--nocapture",
    "--test-threads=1",
];

#[test]
fn send_text_within_delivers_input_the_child_reads() {
    let mut session = start_stub();

    session
        .send_text_within("bounded\r", WAIT)
        .expect("a child that reads its input accepts the text in time");
    expect_output(&session, "got:bounded");
}

#[test]
fn a_short_input_wait_is_refused_and_leaves_later_input_open() {
    let mut session = start_stub();

    // A wait below MIN_INPUT_WAIT cannot cover thread start and one write, so it is refused
    // before any write. Refusing it must not stall the session: a stalled session refuses later
    // input. Zero and sub-scheduling waits are all refused the same way.
    for wait in [
        Duration::ZERO,
        Duration::from_nanos(1),
        Duration::from_micros(1),
        MIN_INPUT_WAIT - Duration::from_nanos(1),
    ] {
        let refused = session
            .send_text_within("refused\r", wait)
            .expect_err("a short wait is refused");
        assert_eq!(refused.kind(), io::ErrorKind::InvalidInput, "wait {wait:?}");
    }
    session
        .send_text("after\r")
        .expect("refused short waits do not stall the input");
    expect_output(&session, "got:after");
    assert!(
        !session
            .capture()
            .expect("readable")
            .text()
            .contains("got:refused"),
        "the refused short-wait input was written"
    );
}

#[test]
fn a_timed_out_input_write_stalls_all_later_input() {
    let session = Session::spawn(&this_exe(), &SILENT_ARGS, SIZE).expect("the silent stub starts");
    let mut session = Running(session);
    expect_output(&session, "conpty-utf8-ready");

    // The child never reads, so a flood larger than the terminal input buffer blocks
    // until the short wait expires.
    let flood = "x".repeat(8 * 1024 * 1024);
    let timed_out = session
        .send_text_within(&flood, Duration::from_millis(500))
        .expect_err("a child that never reads cannot take the flood in time");
    assert_eq!(timed_out.kind(), io::ErrorKind::TimedOut);
    assert!(
        timed_out.to_string().contains("delivery is unknown"),
        "the error does not claim non-delivery: {timed_out}"
    );

    // The abandoned write may still complete, so every later write is refused rather
    // than allowed to interleave with it.
    let plain = session
        .send_text("after\r")
        .expect_err("plain input after an abandoned write is refused");
    assert_eq!(plain.kind(), io::ErrorKind::Other);
    let bounded = session
        .send_text_within("after\r", WAIT)
        .expect_err("bounded input after an abandoned write is refused");
    assert_eq!(bounded.kind(), io::ErrorKind::Other);
    let ctrl_c = session
        .send_ctrl_c()
        .expect_err("Ctrl+C after an abandoned write is refused");
    assert_eq!(ctrl_c.kind(), io::ErrorKind::Other);
}
