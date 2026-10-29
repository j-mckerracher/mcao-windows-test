//! Integration tests for `WindowsBackend` against native child processes.
//!
//! Each child is this test binary re-run with `--exact <stub> --ignored`, on a ConPTY
//! terminal, so the tests need no shell and no extra binary. The same re-exec pattern is
//! used by `tests/interrupt.rs`. The stubs are ignored tests and run only through that
//! re-exec.

#![cfg(windows)]

use std::env;
use std::io;
use std::path::PathBuf;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use mcao_backend_windows::backend::{Launch, WindowsBackend};
use mcao_core::{AgentState, Execution, InterruptSemantics, OpError, SessionBackend};
use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    WaitForSingleObject,
};
use windows_sys::core::BOOL;

/// The marker a stub prints once it is ready to be interrupted or written to.
const READY: &str = "backend-stub-ready";
/// The harness that uses the default Ctrl+C handler, so Ctrl+C ends it.
const HARNESS: &str = "stub";
/// The harness whose stub ignores Ctrl+C, so only job termination ends it.
const HARNESS_IGNORING_CTRL_C: &str = "stub-ignores-ctrl-c";
/// The harness whose stub reads one line from its terminal and echoes it back.
const HARNESS_ECHO: &str = "stub-echo";
/// The harness whose stub exits at once and leaves a descendant running.
const HARNESS_WITH_DESCENDANT: &str = "stub-with-descendant";
/// A harness whose program does not exist, so starting it must fail.
const HARNESS_MISSING: &str = "stub-missing-program";
/// Long enough for a loaded CI runner to start a child and print its marker.
const WAIT: Duration = Duration::from_secs(10);
/// The most an interrupt may take to end a child that ignores Ctrl+C. The backend waits
/// 500 ms, then polls the job every 250 ms, so this bound leaves a wide margin. The 500 ms
/// value itself awaits the measurement owed by DPES-758.
const INTERRUPT_BOUND: Duration = Duration::from_secs(2);
/// The exit code the backend gives a job it terminates.
const TERMINATED_EXIT: u32 = 1;

fn resolve(harness: &str) -> Option<Launch> {
    let stub = match harness {
        HARNESS => "backend_stub",
        HARNESS_IGNORING_CTRL_C => "backend_stub_ignoring_ctrl_c",
        HARNESS_ECHO => "backend_stub_echo",
        HARNESS_WITH_DESCENDANT => "backend_stub_with_descendant",
        HARNESS_MISSING => {
            return Some(Launch {
                program: PathBuf::from(r"C:\mcao-test-no-such-dir\no-such-program.exe"),
                args: Vec::new(),
            });
        }
        _ => return None,
    };
    Some(Launch {
        program: env::current_exe().expect("the test binary path is available"),
        args: [
            "--exact",
            stub,
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ]
        .map(String::from)
        .to_vec(),
    })
}

/// The ready stub: prints its marker, then waits to be ended.
#[test]
#[ignore = "re-run as a child by the tests below"]
fn backend_stub() {
    println!("{READY}");
    thread::sleep(Duration::from_secs(120));
}

/// The ignoring stub: takes Ctrl+C without stopping, so only the job can end it.
#[test]
#[ignore = "re-run as a child by the tests below"]
fn backend_stub_ignoring_ctrl_c() {
    unsafe extern "system" fn ignore(_event: u32) -> BOOL {
        1
    }
    // SAFETY: `ignore` is a valid handler routine that lives for the whole process, and
    // `1` adds it to the handler list.
    let added = unsafe { SetConsoleCtrlHandler(Some(ignore), 1) };
    assert_ne!(added, 0, "the Ctrl+C handler is installed");
    println!("{READY}");
    thread::sleep(Duration::from_secs(120));
}

/// The echo stub: reads one terminal line and prints it back with a marker.
#[test]
#[ignore = "re-run as a child by the tests below"]
fn backend_stub_echo() {
    println!("{READY}");
    let mut line = String::new();
    io::stdin()
        .read_line(&mut line)
        .expect("the terminal line is readable");
    println!("echo-received:{}", line.trim_end());
    thread::sleep(Duration::from_secs(120));
}

/// The descendant stub: starts a second ready stub, then exits while that stub keeps
/// running in the same job.
#[test]
#[ignore = "re-run as a child by the tests below"]
fn backend_stub_with_descendant() {
    let exe = env::current_exe().expect("the test binary path is available");
    let descendant = Command::new(exe)
        .args([
            "--exact",
            "backend_stub",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .spawn()
        .expect("the descendant starts");
    // Dropping a std `Child` does not end it, so the descendant outlives this stub.
    drop(descendant);
    println!("{READY}");
}

/// Polls the capture until `needle` appears, and fails with the capture if it never does.
fn expect_output(backend: &WindowsBackend, handle: &str, needle: &str) {
    let deadline = Instant::now() + WAIT;
    loop {
        let text = backend.capture(handle).expect("the capture is readable");
        if text.contains(needle) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "expected {needle:?} in the output, got {text:?}"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

/// Polls the agent state until it is `expected`, and returns the time it took.
fn expect_state(backend: &WindowsBackend, handle: &str, expected: AgentState) -> Duration {
    let start = Instant::now();
    loop {
        let state = backend.agent_state(handle).expect("the state is readable");
        if state == expected {
            return start.elapsed();
        }
        assert!(
            start.elapsed() < WAIT,
            "expected {expected:?}, last read {state:?}"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

/// Polls until the state can no longer be read. Fails if it reads `Missing` at any point,
/// because the descendant is still a live member of the job.
fn expect_unknown_state(backend: &WindowsBackend, handle: &str) {
    let deadline = Instant::now() + WAIT;
    loop {
        match backend.agent_state(handle) {
            Ok(AgentState::Running) => {}
            Ok(state) => panic!("a child that exited with a live descendant read as {state:?}"),
            Err(OpError::Failed(_)) => return,
            Err(error) => panic!("expected a failed read, got {error:?}"),
        }
        assert!(Instant::now() < deadline, "the state never became unknown");
        thread::sleep(Duration::from_millis(50));
    }
}

/// Returns the process id that a session handle names.
fn pid_of(handle: &str) -> u32 {
    handle
        .strip_prefix("windows-conpty-")
        .and_then(|pid| pid.parse().ok())
        .expect("a session handle ends with the child's process id")
}

#[test]
fn the_backend_names_itself_runs_natively_and_declares_stop_or_kill() {
    let backend = WindowsBackend::new(resolve);
    assert_eq!(backend.name(), "windows-conpty");
    assert!(matches!(backend.execution(), Execution::Native));
    assert_eq!(
        backend.interrupt_semantics(),
        InterruptSemantics::StopOrKill
    );
}

#[test]
fn an_unknown_harness_is_refused_before_any_process_starts() {
    let mut backend = WindowsBackend::new(resolve);
    let error = backend
        .spawn("no-such-harness")
        .expect_err("an unknown harness has no launch");
    assert!(matches!(error, OpError::Unsupported(_)), "got {error:?}");
}

#[test]
fn a_missing_program_fails_to_start() {
    let mut backend = WindowsBackend::new(resolve);
    let error = backend
        .spawn(HARNESS_MISSING)
        .expect_err("the program does not exist");
    assert!(matches!(error, OpError::Failed(_)), "got {error:?}");
}

#[test]
fn a_started_session_is_running_and_its_output_is_captured_until_cleanup() {
    let mut backend = WindowsBackend::new(resolve);
    let handle = backend.spawn(HARNESS).expect("the stub starts");
    assert!(handle.starts_with("windows-conpty-"), "got {handle:?}");

    expect_output(&backend, &handle, READY);
    assert_eq!(
        backend.agent_state(&handle),
        Ok(AgentState::Running),
        "a live child reads as running"
    );

    backend.cleanup(&handle).expect("cleanup ends the job");
    assert!(
        matches!(backend.agent_state(&handle), Err(OpError::Failed(_))),
        "a released session is no longer held"
    );
}

#[test]
fn reconnect_reattaches_only_to_a_held_session() {
    let mut backend = WindowsBackend::new(resolve);
    let handle = backend.spawn(HARNESS).expect("the stub starts");

    backend
        .reconnect(&handle)
        .expect("a held session reattaches");
    let unknown = backend
        .reconnect("windows-conpty-0")
        .expect_err("no session has process id 0");
    assert!(matches!(unknown, OpError::Failed(_)), "got {unknown:?}");

    backend.cleanup(&handle).expect("cleanup ends the job");
}

#[test]
fn send_text_reaches_the_child_terminal() {
    let mut backend = WindowsBackend::new(resolve);
    let handle = backend.spawn(HARNESS_ECHO).expect("the echo stub starts");
    expect_output(&backend, &handle, READY);

    backend
        .send_text(&handle, "hello\r")
        .expect("the text is written");
    expect_output(&backend, &handle, "echo-received:hello");

    backend.cleanup(&handle).expect("cleanup ends the job");
}

#[test]
fn interrupt_stops_a_child_that_uses_the_default_ctrl_c_handler() {
    let mut backend = WindowsBackend::new(resolve);
    let handle = backend.spawn(HARNESS).expect("the stub starts");
    expect_output(&backend, &handle, READY);

    backend.interrupt(&handle).expect("the interrupt is sent");
    expect_state(&backend, &handle, AgentState::Missing);
    backend
        .cleanup(&handle)
        .expect("cleanup accepts the stopped child");
}

#[test]
fn interrupt_terminates_a_child_that_ignores_ctrl_c_within_the_bound() {
    let mut backend = WindowsBackend::new(resolve);
    let handle = backend
        .spawn(HARNESS_IGNORING_CTRL_C)
        .expect("the ignoring stub starts");
    expect_output(&backend, &handle, READY);

    let start = Instant::now();
    backend.interrupt(&handle).expect("the interrupt is sent");
    expect_state(&backend, &handle, AgentState::Missing);
    let elapsed = start.elapsed();
    assert!(
        elapsed < INTERRUPT_BOUND,
        "the child took {elapsed:?} to stop, over the {INTERRUPT_BOUND:?} bound"
    );

    backend
        .cleanup(&handle)
        .expect("cleanup accepts the terminated child");
}

#[test]
fn an_exited_harness_with_a_live_descendant_is_not_reported_missing() {
    let mut backend = WindowsBackend::new(resolve);
    let handle = backend
        .spawn(HARNESS_WITH_DESCENDANT)
        .expect("the descendant stub starts");
    expect_output(&backend, &handle, READY);

    expect_unknown_state(&backend, &handle);

    backend
        .cleanup(&handle)
        .expect("cleanup ends the descendant with the job");
}

#[test]
fn dropping_the_backend_terminates_the_job_of_a_live_session() {
    let mut backend = WindowsBackend::new(resolve);
    let handle = backend.spawn(HARNESS).expect("the stub starts");
    expect_output(&backend, &handle, READY);

    // SAFETY: plain value arguments. The handle opened here is closed below.
    let process = unsafe {
        OpenProcess(
            PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
            0,
            pid_of(&handle),
        )
    };
    assert!(!process.is_null(), "the test can hold the child open");

    drop(backend);

    // SAFETY: `process` is a handle this test opened and has not closed.
    let waited = unsafe { WaitForSingleObject(process, WAIT.as_millis() as u32) };
    assert_eq!(waited, WAIT_OBJECT_0, "the child ended after the drop");
    let mut code = 0u32;
    // SAFETY: `process` is open, and `code` is a writable exit code slot.
    let read = unsafe { GetExitCodeProcess(process, &mut code) };
    // SAFETY: this test opened `process` and closes it exactly once.
    unsafe { CloseHandle(process) };
    assert_ne!(read, 0, "the exit code is readable");
    assert_eq!(
        code, TERMINATED_EXIT,
        "the job ended the child, not a natural exit"
    );
}
