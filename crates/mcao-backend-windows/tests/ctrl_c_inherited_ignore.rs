//! A console child started while this process ignores CTRL+C still receives CTRL+C.
//!
//! `SetConsoleCtrlHandler(NULL, TRUE)` makes a process ignore CTRL+C, and Windows
//! copies that attribute into every child the process creates. This binary sets it
//! in the test process, starts a stub on a pseudoconsole, and interrupts it. The
//! attribute is process-wide, so the test lives in its own binary.
//!
//! The stub is this test binary re-run with `--exact` and `--ignored`, as in
//! `interrupt.rs`. It prints `op-running` and then waits on the default CTRL+C
//! handler, so only a CTRL+C that reaches it ends it.

#![cfg(windows)]

use std::env;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use mcao_backend_windows::conpty::ConsoleSize;
use mcao_backend_windows::interrupt::escalate;
use mcao_backend_windows::process::ProcessGroup;
use mcao_backend_windows::session::Session;
use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;

/// Long enough for a loaded runner to start the stub and print its marker.
const WAIT: Duration = Duration::from_secs(20);
/// A ceiling for the escalation, not a sleep: `escalate` returns as soon as the job is empty.
/// It bounds only a failing run; a loaded runner cannot fail a passing one within it.
const INTERRUPT_WAIT: Duration = Duration::from_secs(60);
/// The exit status of a console process that the default CTRL+C handler ends. A process that the
/// job's terminate call ends exits with 0x1F instead.
const STATUS_CONTROL_C_EXIT: u32 = 0xC000_013A;
/// Long enough that the job, not the stub's own sleep, ends the stub.
const STUB_SLEEP: Duration = Duration::from_secs(120);
/// A pseudoconsole wide enough that the short markers never wrap.
const SIZE: ConsoleSize = ConsoleSize {
    columns: 120,
    rows: 30,
};

/// A session whose job is terminated when it drops, so a failed assertion cannot
/// leak the stub.
struct Running(Session);

impl Drop for Running {
    fn drop(&mut self) {
        // Best effort: the job may already be empty.
        let _ = self.0.job().terminate(1);
    }
}

fn this_exe() -> PathBuf {
    env::current_exe().expect("test binary path is available")
}

#[test]
fn ctrl_c_ends_a_child_when_the_caller_ignores_ctrl_c() {
    // SAFETY: A NULL handler with TRUE sets this process's ignore-CTRL+C attribute. The call
    // takes no pointers and changes only this process.
    let set = unsafe { SetConsoleCtrlHandler(None, 1) };
    // The attribute cannot be read back, so its effect is observed through the stub's exit status.
    assert_ne!(set, 0, "SetConsoleCtrlHandler(NULL, TRUE) must succeed");

    let args = [
        "--exact",
        "inherited_ignore_stub_ends_on_ctrl_c",
        "--ignored",
        "--nocapture",
        "--test-threads=1",
    ];
    let session = Session::spawn_in_group(&this_exe(), &args, SIZE, ProcessGroup::Inherit)
        .expect("the stub starts");
    let mut running = Running(session);
    let ready = running
        .0
        .wait_for_output(b"op-running", WAIT)
        .expect("the output buffer is readable");
    assert!(ready, "the stub prints its ready marker");

    assert!(
        running
            .0
            .child()
            .wait_timeout(Duration::ZERO)
            .expect("the stub's state is readable")
            .is_none(),
        "the stub is running before the CTRL+C"
    );
    let outcome = escalate(&mut running.0, INTERRUPT_WAIT).expect("the escalation runs");
    assert_eq!(
        outcome,
        mcao_backend_windows::interrupt::Escalation::Exited,
        "the CTRL+C ended the stub; TerminatedJob means the stub ignored it"
    );
    let status = running
        .0
        .child()
        .wait_timeout(INTERRUPT_WAIT)
        .expect("the stub's exit status is readable");
    assert_eq!(
        status,
        Some(STATUS_CONTROL_C_EXIT),
        "the stub must end on the default CTRL+C handler; exit 0x1F means the job's terminate call ended it"
    );
}

/// Re-exec stub: prints its ready marker and waits on the default CTRL+C handler.
#[test]
#[ignore = "re-exec stub; run only by the test above"]
fn inherited_ignore_stub_ends_on_ctrl_c() {
    println!("op-running");
    thread::sleep(STUB_SLEEP);
}
