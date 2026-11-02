//! Integration tests for the interrupt ladder against native child processes.
//!
//! Each child is this test binary re-run with `--exact <stub> --ignored`, on a
//! ConPTY terminal, so the tests need no shell and no extra binary. The stubs print
//! markers that exist only if the child ran the matching code:
//!
//! - `op-running` says the stub is ready, and `op-stopped` says its operation
//!   stopped after an Escape.
//! - `ctrl-event:<n> at=<ms>` says a console control event reached the stub, and
//!   when, in milliseconds after the stub installed its handler. 0 is Ctrl+C and 1 is
//!   Ctrl+Break. The stub ignores the event, so the marker shows delivery without a stop.
//!
//! The stub tests are ignored and run only through that re-exec.

#![cfg(windows)]

use std::env;
use std::io::{self, Read};
use std::ops::{Deref, DerefMut};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

use mcao_backend_windows::conpty::ConsoleSize;
use mcao_backend_windows::interrupt::{
    DEFAULT_ESCALATION_WAIT, Escalation, Evidence, LadderTimeouts, MAX_RUNG_WAIT, POLL_INTERVAL,
    Rung, RungResult, SkipReason, TERMINATE_EXIT, escalate, run_ladder,
};
use mcao_backend_windows::process::ProcessGroup;
use mcao_backend_windows::session::{MIN_INPUT_WAIT, Session};
use mcao_core::Support;
use windows_sys::Win32::System::Console::{
    ENABLE_VIRTUAL_TERMINAL_INPUT, GetStdHandle, STD_INPUT_HANDLE, SetConsoleCtrlHandler,
    SetConsoleMode,
};
use windows_sys::core::BOOL;

/// Long enough for a loaded CI runner to start a child and print a marker.
const WAIT: Duration = Duration::from_secs(20);
/// Long enough that the job, not the stub's own timeout, ends each stub.
const STUB_SLEEP: Duration = Duration::from_secs(120);
/// Extra time a rung may take beyond its wait on a loaded runner.
const SLACK: Duration = Duration::from_secs(5);
/// Delivery and scheduling noise allowed on the gap between two Ctrl+C arrivals.
const DELIVERY_TOLERANCE: Duration = Duration::from_millis(1000);
/// A pseudoconsole wide enough that the short markers never wrap.
const SIZE: ConsoleSize = ConsoleSize {
    columns: 120,
    rows: 30,
};
/// The exit code Windows gives a process that the default Ctrl+C handler ends.
const STATUS_CONTROL_C_EXIT: u32 = 0xC000_013A;
/// The marker the Escape stub prints after its operation stops.
const STOPPED: &[u8] = b"op-stopped";

/// The waits used by every ladder test. Short enough to keep the suite quick and
/// long enough for the console to deliver a key or an event on a loaded runner.
const TIMEOUTS: LadderTimeouts = LadderTimeouts {
    escape: Duration::from_millis(3000),
    extended_wait: Duration::from_millis(1000),
    ctrl_c: Duration::from_millis(3000),
    ctrl_break: Duration::from_millis(3000),
    terminate: Duration::from_secs(10),
};

fn this_exe() -> PathBuf {
    env::current_exe().expect("test binary path is available")
}

/// A session whose job is terminated when it drops, so a failed assertion cannot
/// leak a stub.
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

/// Starts the stub named `stub` in `group` and waits for its `ready` marker.
fn start(stub: &str, group: ProcessGroup, ready: &str) -> Running {
    let args = [
        "--exact",
        stub,
        "--ignored",
        "--nocapture",
        "--test-threads=1",
    ];
    let session =
        Session::spawn_in_group(&this_exe(), &args, SIZE, group).expect("the stub starts");
    let running = Running(session);
    expect_output(&running, ready);
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
        "expected {needle:?} in the output, got {:?}",
        capture.text()
    );
}

/// Reports whether the output holds `needle` right now.
fn output_holds(session: &Session, needle: &str) -> bool {
    session
        .capture()
        .expect("the output buffer is readable")
        .text()
        .contains(needle)
}

/// The time the stub installed its handler. Each `ctrl-event` marker reports its
/// milliseconds after this point, so the test can measure the gap between two events.
static STUB_START: OnceLock<Instant> = OnceLock::new();

/// Ignores a console control event after reporting it. Returning true tells the
/// console the event was handled, so the process is not ended.
unsafe extern "system" fn ignore_ctrl_event(event: u32) -> BOOL {
    let at = STUB_START
        .get()
        .map_or(0, |start| start.elapsed().as_millis());
    println!("ctrl-event:{event} at={at}");
    1
}

fn install_ignoring_handler() {
    STUB_START.get_or_init(Instant::now);
    // SAFETY: `ignore_ctrl_event` is a valid handler routine that lives for the
    // whole process, and `1` adds it to the handler list.
    let added = unsafe { SetConsoleCtrlHandler(Some(ignore_ctrl_event), 1) };
    assert_ne!(added, 0, "the handler is installed");
}

/// The arrival times, in milliseconds since the stub started, of each Ctrl+C the stub
/// reported, in order.
fn ctrl_c_times_ms(text: &str) -> Vec<u128> {
    text.split("ctrl-event:0 at=")
        .skip(1)
        .filter_map(|rest| {
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().ok()
        })
        .collect()
}

/// Switches the console input to raw virtual-terminal mode, so a read returns each
/// key at once and Ctrl+C is a plain character.
fn raw_console_input() {
    // SAFETY: GetStdHandle takes a constant. SetConsoleMode takes that handle and a
    // plain flag value, and fails harmlessly on a handle that is not a console.
    let set = unsafe {
        let input = GetStdHandle(STD_INPUT_HANDLE);
        SetConsoleMode(input, ENABLE_VIRTUAL_TERMINAL_INPUT)
    };
    assert_ne!(set, 0, "the console input mode is set");
}

/// Re-exec stub: an operation that stops on Escape and stays alive afterwards.
#[test]
#[ignore = "re-exec stub; run only by the ladder tests"]
fn interrupt_stub_stops_on_escape() {
    raw_console_input();
    println!("op-running");
    let mut key = [0_u8; 1];
    let mut input = io::stdin().lock();
    loop {
        match input.read(&mut key) {
            Ok(1) if key[0] == 0x1b => break,
            Ok(1) => {}
            _ => return,
        }
    }
    println!("op-stopped");
    thread::sleep(STUB_SLEEP);
}

/// Re-exec stub: ignores Escape, and ends on Ctrl+C through the default handler.
#[test]
#[ignore = "re-exec stub; run only by the ladder tests"]
fn interrupt_stub_ends_on_ctrl_c() {
    println!("op-running");
    thread::sleep(STUB_SLEEP);
}

/// Re-exec stub: ignores every key and every console control event, and keeps a
/// grandchild that does the same.
#[test]
#[ignore = "re-exec stub; run only by the ladder tests"]
fn interrupt_stub_uncooperative() {
    install_ignoring_handler();
    let mut grandchild = Command::new(this_exe())
        .args(["--exact", "interrupt_stub_idle", "--ignored"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("the stub starts its grandchild");
    println!("op-running");
    // The job ends both processes, which ends this wait.
    grandchild.wait().expect("the stub waits on its grandchild");
}

/// Re-exec stub: the uncooperative stub's grandchild.
#[test]
#[ignore = "re-exec stub; run only by the ladder tests"]
fn interrupt_stub_idle() {
    install_ignoring_handler();
    thread::sleep(STUB_SLEEP);
}

/// Re-exec stub: ignores Escape, and ends on Ctrl+Break through the default handler.
#[test]
#[ignore = "re-exec stub; run only by the ladder tests"]
fn interrupt_stub_ends_on_ctrl_break() {
    println!("op-running");
    thread::sleep(STUB_SLEEP);
}

/// Re-exec stub: the whole process ends on Escape, so the operation never prints
/// its stop marker.
#[test]
#[ignore = "re-exec stub; run only by the ladder tests"]
fn interrupt_stub_exits_on_escape() {
    raw_console_input();
    println!("op-running");
    let mut key = [0_u8; 1];
    let mut input = io::stdin().lock();
    loop {
        match input.read(&mut key) {
            Ok(1) if key[0] == 0x1b => return,
            Ok(1) => {}
            _ => return,
        }
    }
}

/// Re-exec stub: ends at once.
#[test]
#[ignore = "re-exec stub; run only by the ladder tests"]
fn interrupt_stub_exit_at_once() {}

/// Returns the evidence of a record that must carry some.
fn evidence_of(result: &RungResult) -> Evidence {
    match result {
        RungResult::Stopped(evidence) | RungResult::NotStopped(evidence) => *evidence,
        other => panic!("expected a result with evidence, got {other:?}"),
    }
}

#[test]
fn escape_stops_the_operation_and_the_child_stays_alive() {
    let mut session = start(
        "interrupt_stub_stops_on_escape",
        ProcessGroup::Inherit,
        "op-running",
    );
    let report = run_ladder(&mut session, STOPPED, &TIMEOUTS).expect("the ladder runs");
    assert_eq!(report.stopped_by(), Some(Rung::Escape));
    let [record] = report.records() else {
        panic!("only the Escape rung runs, got {:?}", report.records());
    };
    assert_eq!(record.support(), Support::Demonstrated);
    let evidence = evidence_of(&record.result);
    assert!(evidence.marker_seen, "the stop marker is the evidence");
    assert_eq!(
        evidence.child_exit_code, None,
        "the operation stopped, not the process"
    );
    assert_eq!(evidence.job_processes, 1);
}

#[test]
fn ctrl_c_stops_a_child_that_ignores_escape() {
    let mut session = start(
        "interrupt_stub_ends_on_ctrl_c",
        ProcessGroup::Inherit,
        "op-running",
    );
    let report = run_ladder(&mut session, STOPPED, &TIMEOUTS).expect("the ladder runs");
    assert_eq!(report.stopped_by(), Some(Rung::CtrlC));
    let rungs: Vec<Rung> = report.records().iter().map(|record| record.rung).collect();
    assert_eq!(rungs, [Rung::Escape, Rung::ExtendedWait, Rung::CtrlC]);
    for record in &report.records()[..2] {
        assert!(
            matches!(record.result, RungResult::NotStopped(_)),
            "{record:?}"
        );
        assert_eq!(record.support(), Support::Unresolved);
    }
    let last = &report.records()[2];
    assert_eq!(last.support(), Support::Demonstrated);
    assert_eq!(
        evidence_of(&last.result).child_exit_code,
        Some(STATUS_CONTROL_C_EXIT)
    );
}

#[test]
fn ctrl_break_stops_a_child_in_a_new_group_that_ignores_the_earlier_rungs() {
    let mut session = start(
        "interrupt_stub_ends_on_ctrl_break",
        ProcessGroup::New,
        "op-running",
    );
    let report = run_ladder(&mut session, STOPPED, &TIMEOUTS).expect("the ladder runs");
    let rungs: Vec<Rung> = report.records().iter().map(|record| record.rung).collect();
    assert_eq!(
        rungs,
        [
            Rung::Escape,
            Rung::ExtendedWait,
            Rung::CtrlC,
            Rung::CtrlBreak
        ]
    );
    assert_eq!(report.stopped_by(), Some(Rung::CtrlBreak));
    let last = &report.records()[3];
    assert_eq!(last.support(), Support::Demonstrated);
    assert_eq!(last.warning, None, "the console was restored");
    assert!(
        evidence_of(&last.result).child_exit_code.is_some(),
        "the default Ctrl+Break handler ended the child: {last:?}"
    );
}

#[test]
fn a_child_that_exits_on_escape_is_not_credited_as_a_stopped_operation() {
    let mut session = start(
        "interrupt_stub_exits_on_escape",
        ProcessGroup::Inherit,
        "op-running",
    );
    let report = run_ladder(&mut session, STOPPED, &TIMEOUTS).expect("the ladder runs");
    let [record] = report.records() else {
        panic!("the ladder ends at the exit, got {:?}", report.records());
    };
    assert_eq!(record.rung, Rung::Escape);
    assert!(
        matches!(record.result, RungResult::ChildExited(_)),
        "{record:?}"
    );
    assert_eq!(record.support(), Support::Unresolved);
    assert_eq!(report.stopped_by(), None);
}

#[test]
fn uncooperative_child_in_a_new_group_escalates_through_every_rung_to_terminate() {
    let mut session = start(
        "interrupt_stub_uncooperative",
        ProcessGroup::New,
        "op-running",
    );
    assert_eq!(
        session
            .job()
            .active_processes()
            .expect("the job is readable"),
        2,
        "the stub and its grandchild are in the job"
    );
    let report = run_ladder(&mut session, STOPPED, &TIMEOUTS).expect("the ladder runs");

    let rungs: Vec<Rung> = report.records().iter().map(|record| record.rung).collect();
    assert_eq!(rungs, Rung::LADDER);
    assert_eq!(report.stopped_by(), Some(Rung::TerminateJob));
    let (last, earlier) = report
        .records()
        .split_last()
        .expect("the ladder has records");
    for record in earlier {
        assert_eq!(record.support(), Support::Unresolved, "{record:?}");
        assert!(
            !matches!(record.result, RungResult::Stopped(_)),
            "{record:?}"
        );
        if matches!(record.result, RungResult::NotStopped(_)) {
            assert!(record.waited >= record.timeout, "{record:?}");
            assert!(record.waited < record.timeout + SLACK, "{record:?}");
        }
    }
    assert_eq!(last.support(), Support::Demonstrated);
    assert_eq!(
        evidence_of(&last.result),
        Evidence {
            marker_seen: false,
            child_exit_code: Some(TERMINATE_EXIT),
            job_processes: 0,
        },
        "the job reports no live process after the forced stop"
    );
    let break_record = &report.records()[3];
    assert!(
        matches!(break_record.result, RungResult::NotStopped(_)),
        "the Ctrl+Break was sent and the stub ignored it: {break_record:?}"
    );
    assert!(
        output_holds(&session, "ctrl-event:1"),
        "a sent Ctrl+Break reaches the stub"
    );
}

#[test]
fn uncooperative_child_in_the_shared_group_skips_ctrl_break() {
    let mut session = start(
        "interrupt_stub_uncooperative",
        ProcessGroup::Inherit,
        "op-running",
    );
    let report = run_ladder(&mut session, STOPPED, &TIMEOUTS).expect("the ladder runs");

    let rungs: Vec<Rung> = report.records().iter().map(|record| record.rung).collect();
    assert_eq!(rungs, Rung::LADDER);
    assert_eq!(report.stopped_by(), Some(Rung::TerminateJob));
    let skipped = &report.records()[3];
    assert_eq!(
        skipped.result,
        RungResult::Skipped(SkipReason::NoProcessGroup)
    );
    assert_eq!(skipped.support(), Support::Unsupported);
    assert_eq!(report.records()[2].support(), Support::Unresolved);
    assert!(
        output_holds(&session, "ctrl-event:0"),
        "Ctrl+C reaches a child in the shared group, which ignores it"
    );
}

#[test]
fn ctrl_c_does_not_reach_a_child_in_a_new_group() {
    let mut session = start(
        "interrupt_stub_uncooperative",
        ProcessGroup::New,
        "op-running",
    );
    session.send_text("\u{3}").expect("the input is writable");
    thread::sleep(Duration::from_secs(3));
    assert!(
        !output_holds(&session, "ctrl-event:0"),
        "a new process group turns Ctrl+C off for its members"
    );
}

#[test]
fn ladder_refuses_a_child_that_already_exited() {
    let mut session = Running(
        Session::spawn(
            &this_exe(),
            &[
                "--exact",
                "interrupt_stub_exit_at_once",
                "--ignored",
                "--test-threads=1",
            ],
            SIZE,
        )
        .expect("the stub starts"),
    );
    let exited = session
        .child()
        .wait_timeout(WAIT)
        .expect("the child is queryable");
    assert!(exited.is_some(), "the stub ends at once");
    let error = run_ladder(&mut session, STOPPED, &TIMEOUTS).expect_err("nothing to interrupt");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn ladder_refuses_bad_timeouts_and_an_empty_marker_before_sending_anything() {
    let mut session = start(
        "interrupt_stub_stops_on_escape",
        ProcessGroup::Inherit,
        "op-running",
    );
    let zero = LadderTimeouts {
        escape: Duration::ZERO,
        ..TIMEOUTS
    };
    let error = run_ladder(&mut session, STOPPED, &zero).expect_err("a zero wait is refused");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    let error = run_ladder(&mut session, b"", &TIMEOUTS).expect_err("an empty marker is refused");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    thread::sleep(Duration::from_secs(1));
    assert!(
        !output_holds(&session, "op-stopped"),
        "no Escape was sent, so the operation never stopped"
    );
}

#[test]
fn escalation_sends_a_second_ctrl_c_then_terminates_the_tree_at_the_wait() {
    let mut session = start(
        "interrupt_stub_uncooperative",
        ProcessGroup::Inherit,
        "op-running",
    );
    let wait = Duration::from_secs(4);
    let began = Instant::now();
    let outcome = escalate(&mut session, wait).expect("the escalation runs");
    let elapsed = began.elapsed();

    assert_eq!(outcome, Escalation::TerminatedJob);
    assert!(
        elapsed >= wait,
        "the job ends only at the wait: {elapsed:?}"
    );
    assert!(
        elapsed < wait + SLACK,
        "the job ends near the wait: {elapsed:?}"
    );
    // Let the stub's last printed event reach the capture before counting.
    thread::sleep(Duration::from_secs(1));
    let ctrl_c = session
        .capture()
        .expect("the output buffer is readable")
        .text()
        .matches("ctrl-event:0")
        .count();
    assert!(
        ctrl_c >= 2,
        "the first and second Ctrl+C both reach the stub, got {ctrl_c}"
    );

    // The second Ctrl+C goes out at half of the wait, not with the first. The gap
    // between the two arrivals must match that, within one poll and the delivery
    // slack on a loaded runner. Both are sent at once would show a gap near zero.
    let text = session
        .capture()
        .expect("the output buffer is readable")
        .text();
    let times = ctrl_c_times_ms(&text);
    assert!(times.len() >= 2, "two Ctrl+C arrivals are timed: {times:?}");
    let gap = times[1].saturating_sub(times[0]);
    let half = wait.as_millis() / 2;
    let tolerance = POLL_INTERVAL.as_millis() + DELIVERY_TOLERANCE.as_millis();
    assert!(
        gap + tolerance >= half && gap <= half + tolerance,
        "the second Ctrl+C lands at half the wait ({half} ms after the first): \
         gap {gap} ms, arrivals {times:?}"
    );
    assert_eq!(
        session
            .job()
            .active_processes()
            .expect("the job is readable"),
        0,
        "the forced stop left no live process"
    );
}

#[test]
fn escalation_skips_a_second_ctrl_c_that_would_get_less_than_the_floor() {
    let mut session = start(
        "interrupt_stub_uncooperative",
        ProcessGroup::Inherit,
        "op-running",
    );
    // At 260 ms the second Ctrl+C would be written at the 250 ms poll tick with about 10 ms left,
    // under MIN_INPUT_WAIT. It must be skipped, so only the first Ctrl+C reaches the stub.
    let outcome = escalate(&mut session, Duration::from_millis(260)).expect("the escalation runs");
    assert_eq!(outcome, Escalation::TerminatedJob);
    thread::sleep(Duration::from_secs(1));
    let ctrl_c = session
        .capture()
        .expect("the output buffer is readable")
        .text()
        .matches("ctrl-event:0")
        .count();
    assert_eq!(
        ctrl_c, 1,
        "only the first Ctrl+C is written at a 260 ms wait"
    );
}

#[test]
fn escalation_reports_exited_when_ctrl_c_ends_the_child_inside_the_wait() {
    let mut session = start(
        "interrupt_stub_ends_on_ctrl_c",
        ProcessGroup::Inherit,
        "op-running",
    );
    let wait = Duration::from_secs(10);
    let began = Instant::now();
    let outcome = escalate(&mut session, wait).expect("the escalation runs");

    assert_eq!(outcome, Escalation::Exited);
    assert!(
        began.elapsed() < wait,
        "the child exited before the wait ran out"
    );
    assert_eq!(
        session
            .child()
            .wait_timeout(WAIT)
            .expect("the child is queryable"),
        Some(STATUS_CONTROL_C_EXIT),
        "Ctrl+C ended the child with the default handler"
    );
}

#[test]
fn escalation_on_an_empty_job_reports_exited_without_writing() {
    let mut session = Running(
        Session::spawn(
            &this_exe(),
            &[
                "--exact",
                "interrupt_stub_exit_at_once",
                "--ignored",
                "--test-threads=1",
            ],
            SIZE,
        )
        .expect("the stub starts"),
    );
    let exited = session
        .child()
        .wait_timeout(WAIT)
        .expect("the child is queryable");
    assert!(exited.is_some(), "the stub ends at once");
    let outcome =
        escalate(&mut session, DEFAULT_ESCALATION_WAIT).expect("an empty job is not an error");
    assert_eq!(outcome, Escalation::Exited);
}

#[test]
fn escalation_refuses_a_short_or_unbounded_wait_before_writing() {
    let mut session = start(
        "interrupt_stub_uncooperative",
        ProcessGroup::Inherit,
        "op-running",
    );
    // Waits below MIN_INPUT_WAIT are refused: their Ctrl+C write could not reach the pipe in time
    // and would stall the input for the rest of the session.
    for wait in [
        Duration::ZERO,
        Duration::from_nanos(1),
        Duration::from_micros(1),
        MIN_INPUT_WAIT - Duration::from_nanos(1),
        MAX_RUNG_WAIT + Duration::from_secs(1),
    ] {
        let error = escalate(&mut session, wait).expect_err("the wait is refused");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "wait {wait:?}");
    }
    thread::sleep(Duration::from_secs(1));
    assert!(
        !output_holds(&session, "ctrl-event:0"),
        "a refused escalation writes nothing"
    );
    assert_eq!(
        session
            .job()
            .active_processes()
            .expect("the job is readable"),
        2,
        "the stub and its grandchild are still running"
    );
    // A refused escalation must not stall the input: plain input still reaches the session.
    session
        .send_text_within("open\r", WAIT)
        .expect("a refused escalation does not stall the input");
}

#[test]
fn escalation_refuses_a_child_that_cannot_receive_ctrl_c_before_writing() {
    // A child in a new process group never receives Ctrl+C, so escalation is refused
    // before the first write. The refusal leaves the job running for the caller to decide.
    let mut session = start(
        "interrupt_stub_uncooperative",
        ProcessGroup::New,
        "op-running",
    );
    let error =
        escalate(&mut session, DEFAULT_ESCALATION_WAIT).expect_err("a new-group child is refused");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    thread::sleep(Duration::from_secs(1));
    assert_eq!(
        session
            .job()
            .active_processes()
            .expect("the job is readable"),
        2,
        "the refused escalation leaves the stub and its grandchild running"
    );
}

#[test]
fn escalation_after_an_abandoned_input_write_still_ends_the_job() {
    let mut session = start(
        "interrupt_stub_uncooperative",
        ProcessGroup::Inherit,
        "op-running",
    );
    // A child that does not take input blocks the flood, so the write is abandoned and the
    // session's input is stalled before escalation starts.
    let flood = "x".repeat(8 * 1024 * 1024);
    session
        .send_text_within(&flood, Duration::from_millis(500))
        .expect_err("a child that never reads cannot take the flood in time");

    // The stub ignores Ctrl+C and never reads input, so a normal escalation runs its full grace
    // period and cannot end before WAIT. Ending earlier is the stall shortcut: no Ctrl+C write
    // and no grace period. The bound is half of WAIT, so a missing shortcut fails it.
    let started = Instant::now();
    let outcome = escalate(&mut session, WAIT).expect("escalation runs");
    assert!(
        matches!(outcome, Escalation::TerminatedJob),
        "a stalled input ends the job rather than writing: {outcome:?}"
    );
    assert!(
        started.elapsed() < WAIT / 2,
        "escalation skipped the grace period on a stalled input: {:?}",
        started.elapsed()
    );
    let refused = session
        .send_ctrl_c()
        .expect_err("the stall outlives the escalation; no Ctrl+C is written");
    assert_eq!(refused.kind(), io::ErrorKind::Other);
}
