//! Host-runnable tests for the write-window floor and the escalation schedule.
//!
//! The Windows crate compiles out on other platforms, so this file includes `src/window.rs` by
//! path. That module uses only `std`, so these tests run on every host and in Windows CI. The
//! escalation schedule runs on a virtual clock, so its timings are exact and do not depend on
//! how long the host takes to sleep.

#[path = "../src/window.rs"]
mod window;

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::io::{self, Write};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use window::{
    BoundedWrite, Clock, Finish, MIN_INPUT_WAIT, bounded_write, bounded_write_on, ctrl_c_within,
    escalate_schedule, window_meets_floor,
};

/// The poll interval of `interrupt::escalate`. This mirrors `interrupt::POLL_INTERVAL`, which this
/// host test cannot import because the crate is compiled only for Windows. A change to that
/// constant must change this one too.
const POLL: Duration = Duration::from_millis(250);
const SECOND: Duration = Duration::from_secs(1);
const ZERO: Duration = Duration::ZERO;
const CTRL_C: &[u8] = b"\x03";

/// A clock that moves only when the schedule sleeps or a scripted write takes time. The schedule
/// and the scripted write share it, so a slow write advances the time the schedule reads.
///
/// `stalls` holds the extra time each successive sleep takes beyond its request, the way a thread
/// that is not scheduled wakes late. A sleep past the end of `stalls` takes exactly its request.
#[derive(Clone, Default)]
struct Virtual {
    now: Rc<Cell<Duration>>,
    stalls: Rc<RefCell<VecDeque<Duration>>>,
}

impl Virtual {
    fn advance(&self, by: Duration) {
        self.now.set(self.now.get() + by);
    }
}

impl Clock for Virtual {
    fn elapsed(&self) -> Duration {
        self.now.get()
    }

    fn sleep(&self, duration: Duration) {
        let stall = self.stalls.borrow_mut().pop_front().unwrap_or(ZERO);
        self.advance(duration + stall);
    }
}

/// One scripted Ctrl+C write: its result, and how long it takes on the virtual clock.
type Scripted = (io::Result<bool>, Duration);

/// What one run of the schedule against a busy job observed.
struct Run {
    outcome: io::Result<Finish>,
    /// The window of every Ctrl+C write, as it was issued.
    windows: Vec<Duration>,
    /// How many times the job was terminated.
    terminations: u32,
    /// Virtual time when the schedule returned.
    elapsed: Duration,
}

/// Runs the schedule against a job that never empties. The Ctrl+C writes follow `script`, and a
/// write past the end of the script succeeds at once and is not pending.
fn run_busy(wait: Duration, script: Vec<Scripted>) -> Run {
    run_busy_stalled(wait, script, Vec::new())
}

/// [`run_busy`] with `stalls` added to the successive sleeps of the poll loop.
fn run_busy_stalled(wait: Duration, script: Vec<Scripted>, stalls: Vec<Duration>) -> Run {
    let clock = Virtual::default();
    *clock.stalls.borrow_mut() = VecDeque::from(stalls);
    let script = RefCell::new(VecDeque::from(script));
    let windows = RefCell::new(Vec::new());
    let terminations = Cell::new(0);
    let outcome = escalate_schedule(
        wait,
        POLL,
        &clock,
        || Ok(1),
        || {
            terminations.set(terminations.get() + 1);
            Ok(())
        },
        |window| {
            windows.borrow_mut().push(window);
            let (result, took) = script.borrow_mut().pop_front().unwrap_or((Ok(false), ZERO));
            clock.advance(took);
            result
        },
    );
    Run {
        outcome,
        windows: windows.into_inner(),
        terminations: terminations.get(),
        elapsed: clock.elapsed(),
    }
}

/// A sink that accepts nothing until the test drops its sender, the way a full ConPTY input
/// pipe blocks a write.
struct BlockingSink(mpsc::Receiver<()>);

impl Write for BlockingSink {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        // Returns once the test drops its sender; the write was never accepted.
        let _ = self.0.recv();
        Err(io::Error::other("released by the test"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A sink that forwards every byte it accepts to the test.
struct ChannelSink(mpsc::Sender<Vec<u8>>);

impl Write for ChannelSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0
            .send(buf.to_vec())
            .map_err(|_| io::Error::other("the test stopped listening"))?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn the_floor_is_inclusive_and_a_nanosecond_below_it_is_not_writable() {
    assert!(window_meets_floor(MIN_INPUT_WAIT));
    assert!(!window_meets_floor(
        MIN_INPUT_WAIT - Duration::from_nanos(1)
    ));
}

#[test]
fn a_write_window_below_the_floor_is_refused_and_nothing_is_written() {
    let (sent, received) = mpsc::channel::<Vec<u8>>();
    let window = MIN_INPUT_WAIT - Duration::from_nanos(1);

    let error = bounded_write(ChannelSink(sent), CTRL_C.to_vec(), window)
        .expect_err("a window under the floor is refused");

    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert!(received.try_recv().is_err(), "nothing is written");
}

#[test]
fn a_window_of_exactly_the_floor_is_not_refused() {
    let (sent, _received) = mpsc::channel::<Vec<u8>>();

    let outcome = bounded_write(ChannelSink(sent), CTRL_C.to_vec(), MIN_INPUT_WAIT);

    assert!(
        outcome.is_ok(),
        "a window at the floor is not refused: {outcome:?}"
    );
}

#[test]
fn a_blocked_write_is_abandoned_when_its_window_ends() {
    let (release, blocked) = mpsc::channel::<()>();
    let started = Instant::now();
    let window = Duration::from_millis(200);

    let outcome = bounded_write(BlockingSink(blocked), CTRL_C.to_vec(), window)
        .expect("a blocked write is not an I/O error");

    let waited = started.elapsed();
    assert_eq!(outcome, BoundedWrite::Pending);
    assert!(waited >= Duration::from_millis(200), "waited {waited:?}");
    assert!(
        waited < SECOND * 5,
        "the wait is bounded by the window: {waited:?}"
    );
    // Releasing the sender lets the abandoned thread finish after the test.
    drop(release);
}

#[test]
fn a_write_that_finishes_in_time_sends_exactly_the_ctrl_c_byte() {
    let (sent, received) = mpsc::channel::<Vec<u8>>();

    let outcome =
        bounded_write(ChannelSink(sent), CTRL_C.to_vec(), SECOND * 5).expect("the write succeeds");

    assert_eq!(outcome, BoundedWrite::Finished);
    let bytes: Vec<u8> = received.try_iter().flatten().collect();
    assert_eq!(bytes, [0x03], "the write is exactly the Ctrl+C byte");
}

#[test]
fn a_write_thread_that_cannot_start_is_an_error_and_nothing_is_written() {
    let (sent, received) = mpsc::channel::<Vec<u8>>();
    let window = SECOND * 5;
    let spawn = |_task: Box<dyn FnOnce() + Send>| -> io::Result<()> {
        Err(io::Error::other("no thread is available"))
    };

    let outcome = bounded_write_on(spawn, ChannelSink(sent), CTRL_C.to_vec(), window);

    let error = outcome.expect_err("a thread that cannot start is reported, not a panic");
    assert_eq!(error.to_string(), "no thread is available");
    assert!(
        received.try_recv().is_err(),
        "the task was dropped, so no byte is written"
    );
}

#[test]
fn an_empty_job_exits_without_writing_a_ctrl_c() {
    let clock = Virtual::default();
    let writes = Cell::new(0);

    let outcome = escalate_schedule(
        SECOND,
        POLL,
        &clock,
        || Ok(0),
        || Ok(()),
        |_window| {
            writes.set(writes.get() + 1);
            Ok(false)
        },
    );

    assert_eq!(
        outcome.expect("an empty job is not an error"),
        Finish::Exited
    );
    assert_eq!(writes.get(), 0, "nothing is written to an empty job");
}

#[test]
fn a_first_ctrl_c_still_pending_at_the_deadline_terminates_the_job() {
    let run = run_busy(SECOND, vec![(Ok(true), ZERO)]);

    assert_eq!(run.outcome.expect("the schedule runs"), Finish::Terminated);
    assert_eq!(run.windows, vec![SECOND], "one write, with the whole wait");
    assert_eq!(run.terminations, 1);
}

#[test]
fn a_second_ctrl_c_still_pending_at_the_deadline_terminates_the_job() {
    let run = run_busy(SECOND, vec![(Ok(false), ZERO), (Ok(true), ZERO)]);

    assert_eq!(run.outcome.expect("the schedule runs"), Finish::Terminated);
    assert_eq!(
        run.windows,
        vec![SECOND, Duration::from_millis(500)],
        "the second write has the time left after half the wait"
    );
    assert_eq!(run.terminations, 1);
}

#[test]
fn a_failed_second_ctrl_c_does_not_stop_the_escalation() {
    let run = run_busy(
        SECOND,
        vec![
            (Ok(false), ZERO),
            (Err(io::Error::other("the input pipe closed")), ZERO),
        ],
    );

    assert_eq!(run.outcome.expect("the schedule runs"), Finish::Terminated);
    assert_eq!(run.windows, vec![SECOND, Duration::from_millis(500)]);
    assert_eq!(run.terminations, 1, "the job is terminated once");
    assert_eq!(run.elapsed, SECOND, "the job runs to the wait");
}

#[test]
fn a_slow_second_ctrl_c_does_not_carry_the_wait_past_its_end() {
    // The second write takes 450 ms. The sleep after it must be sized from the time read after
    // the write, so the schedule still ends at the wait.
    let run = run_busy(
        SECOND,
        vec![(Ok(false), ZERO), (Ok(false), Duration::from_millis(450))],
    );

    assert_eq!(run.outcome.expect("the schedule runs"), Finish::Terminated);
    assert_eq!(
        run.elapsed, SECOND,
        "the wait ends at the deadline, not after the slow write"
    );
}

#[test]
fn a_poll_stalled_past_half_the_wait_is_a_known_limitation_that_skips_the_second_ctrl_c() {
    // Characterizes an accepted limitation and pins current behavior: a fix must update this test.
    // The decision is recorded for DPES-969; the follow-up is DPES-993.
    // The Windows failure recorded for DPES-969 had write count (1, 1), not (2, 1). The first poll
    // wakes at 250 ms. The second sleep stalls 500 ms, so the wake that would have sent the second
    // Ctrl+C is at the wait itself, and no window is left for it. A timer-driven second write would
    // deliver it; that is a design change, out of scope for this slice.
    // The 500 ms row of the table test covers the same schedule. This test stays as the named
    // regression for the DPES-969 failure, and it also pins the single termination.
    let run = run_busy_stalled(SECOND, Vec::new(), vec![ZERO, Duration::from_millis(500)]);

    assert_eq!(run.outcome.expect("the schedule runs"), Finish::Terminated);
    assert_eq!(run.windows, vec![SECOND], "one write, with the whole wait");
    assert_eq!(run.terminations, 1, "the job is terminated once");
    assert_eq!(run.elapsed, SECOND, "the job ends at the wait");
}

#[test]
fn a_stall_before_the_second_ctrl_c_leaves_it_the_time_that_remains() {
    // Each case: the stall added to the second poll sleep, in ms, and the window of every Ctrl+C
    // write in ms. The second Ctrl+C goes out at the first wake at or after 500 ms, with the time
    // left to 1000 ms as its window. A wake at 980 ms leaves 20 ms, the floor, so the write is made.
    // A wake at 981 ms leaves 19 ms, under the floor, so it is skipped. A wake at the wait or later
    // has nothing left, so no second write is attempted.
    let cases: [(u64, &[u64]); 5] = [
        (0, &[1000, 500]),
        (200, &[1000, 300]),
        (480, &[1000, 20]),
        (481, &[1000]),
        (500, &[1000]),
    ];
    for (stall_ms, expected_ms) in cases {
        let stalls = vec![ZERO, Duration::from_millis(stall_ms)];

        let run = run_busy_stalled(SECOND, Vec::new(), stalls);

        assert_eq!(
            run.outcome.as_ref().ok(),
            Some(&Finish::Terminated),
            "the schedule ends by terminating with a {stall_ms} ms stall"
        );
        let expected: Vec<Duration> = expected_ms
            .iter()
            .map(|ms| Duration::from_millis(*ms))
            .collect();
        assert_eq!(
            run.windows, expected,
            "write windows with a {stall_ms} ms stall"
        );
        assert!(
            run.windows.iter().all(|window| window_meets_floor(*window)),
            "every write window meets the floor with a {stall_ms} ms stall: {:?}",
            run.windows
        );
        assert_eq!(
            run.elapsed, SECOND,
            "the schedule ends at the wait with a {stall_ms} ms stall"
        );
    }
}

#[test]
fn the_second_ctrl_c_is_written_only_when_its_window_meets_the_floor() {
    // Each case: the wait in ms, and the window of every Ctrl+C write in ms. The poll wakes every
    // 250 ms. The first Ctrl+C returns at once in every case. Waits of 250 ms and under have no
    // wake before the deadline, so they get no second Ctrl+C. At 260 ms the second Ctrl+C is due at
    // 250 ms with 10 ms left, under the floor, so it is skipped. At 510 ms it is due at 500 ms with
    // 10 ms left, so it is skipped too.
    let cases: [(u64, &[u64]); 10] = [
        (20, &[20]),
        (100, &[100]),
        (200, &[200]),
        (250, &[250]),
        (260, &[260]),
        (300, &[300, 50]),
        (500, &[500, 250]),
        (510, &[510]),
        (1000, &[1000, 500]),
        (2000, &[2000, 1000]),
    ];
    for (wait_ms, expected_ms) in cases {
        let wait = Duration::from_millis(wait_ms);

        let run = run_busy(wait, Vec::new());

        assert_eq!(
            run.outcome.as_ref().ok(),
            Some(&Finish::Terminated),
            "the schedule ends by terminating at {wait_ms} ms"
        );
        let expected: Vec<Duration> = expected_ms
            .iter()
            .map(|ms| Duration::from_millis(*ms))
            .collect();
        assert_eq!(run.windows, expected, "write windows at {wait_ms} ms");
        assert!(
            run.windows.iter().all(|window| window_meets_floor(*window)),
            "every write window meets the floor at {wait_ms} ms: {:?}",
            run.windows
        );
        assert_eq!(
            run.elapsed, wait,
            "the schedule ends at the wait, {wait_ms} ms"
        );
    }
}

/// The stall flag that an abandoned write sets on the session's input, as `Session` keeps it.
#[derive(Default)]
struct Input {
    stalled: Cell<bool>,
}

/// Runs `escalate_schedule` on a job that never empties, as `interrupt::escalate` does: each
/// Ctrl+C goes through `ctrl_c_within` on `input`. The writes follow `results`, and a write past
/// the end of `results` succeeds at once. A pending write takes its whole window on the virtual
/// clock, as `bounded_write` waits for it.
fn escalate_on(input: &Input, wait: Duration, results: Vec<io::Result<BoundedWrite>>) -> Run {
    let clock = Virtual::default();
    let results = RefCell::new(VecDeque::from(results));
    let windows = RefCell::new(Vec::new());
    let terminations = Cell::new(0);
    let outcome = escalate_schedule(
        wait,
        POLL,
        &clock,
        || Ok(1),
        || {
            terminations.set(terminations.get() + 1);
            Ok(())
        },
        |window| {
            ctrl_c_within(
                input.stalled.get(),
                window,
                |window| {
                    windows.borrow_mut().push(window);
                    let result = results
                        .borrow_mut()
                        .pop_front()
                        .unwrap_or(Ok(BoundedWrite::Finished));
                    if let Ok(BoundedWrite::Pending) = result {
                        clock.advance(window);
                    }
                    result
                },
                || input.stalled.set(true),
            )
        },
    );
    Run {
        outcome,
        windows: windows.into_inner(),
        terminations: terminations.get(),
        elapsed: clock.elapsed(),
    }
}

#[test]
fn a_stalled_input_writes_nothing_and_ends_the_job_at_once() {
    let input = Input::default();
    input.stalled.set(true);

    let run = escalate_on(&input, SECOND, Vec::new());

    assert_eq!(run.outcome.expect("the schedule runs"), Finish::Terminated);
    assert!(
        run.windows.is_empty(),
        "no Ctrl+C is written to a stalled input: {:?}",
        run.windows
    );
    assert_eq!(run.terminations, 1);
    assert_eq!(run.elapsed, ZERO, "a stalled input skips the wait");
    assert!(input.stalled.get(), "the stall is kept for later input");
}

#[test]
fn a_first_ctrl_c_abandoned_at_its_deadline_stalls_the_input_and_ends_the_job() {
    let input = Input::default();

    let run = escalate_on(&input, SECOND, vec![Ok(BoundedWrite::Pending)]);

    assert_eq!(run.outcome.expect("the schedule runs"), Finish::Terminated);
    assert_eq!(run.windows, vec![SECOND], "one write, with the whole wait");
    assert_eq!(
        run.elapsed, SECOND,
        "the abandoned write holds the schedule to its deadline"
    );
    assert_eq!(run.terminations, 1);
    assert!(input.stalled.get(), "the abandoned bytes stall the input");
}

#[test]
fn a_second_ctrl_c_abandoned_at_its_deadline_stalls_the_input_and_ends_the_job() {
    let input = Input::default();

    let run = escalate_on(
        &input,
        SECOND,
        vec![Ok(BoundedWrite::Finished), Ok(BoundedWrite::Pending)],
    );

    assert_eq!(run.outcome.expect("the schedule runs"), Finish::Terminated);
    assert_eq!(run.windows, vec![SECOND, Duration::from_millis(500)]);
    assert_eq!(run.elapsed, SECOND, "the wait ends at the deadline");
    assert_eq!(run.terminations, 1);
    assert!(
        input.stalled.get(),
        "the abandoned second Ctrl+C stalls the input"
    );
}

#[test]
fn an_escalation_after_an_abandoned_write_writes_nothing() {
    let input = Input::default();
    let first = escalate_on(&input, SECOND, vec![Ok(BoundedWrite::Pending)]);
    assert_eq!(
        first.windows.len(),
        1,
        "the first escalation abandons its write"
    );

    let second = escalate_on(&input, SECOND, Vec::new());

    assert_eq!(
        second.outcome.expect("the schedule runs"),
        Finish::Terminated
    );
    assert!(
        second.windows.is_empty(),
        "no Ctrl+C is written past the stall: {:?}",
        second.windows
    );
    assert_eq!(
        second.elapsed, ZERO,
        "the stalled input ends the job at once"
    );
    assert_eq!(second.terminations, 1);
}

#[test]
fn a_failed_ctrl_c_write_is_an_error_that_leaves_the_job_running_and_stalls_nothing() {
    let input = Input::default();

    let run = escalate_on(
        &input,
        SECOND,
        vec![Err(io::Error::other("no thread is available"))],
    );

    let error = run.outcome.expect_err("the write error is returned");
    assert_eq!(error.to_string(), "no thread is available");
    assert_eq!(
        run.terminations, 0,
        "the job is left for the caller to decide"
    );
    assert!(
        !input.stalled.get(),
        "a write that failed did not stall the input"
    );
}

#[test]
fn a_failed_second_ctrl_c_stalls_nothing() {
    let input = Input::default();

    let run = escalate_on(
        &input,
        SECOND,
        vec![
            Ok(BoundedWrite::Finished),
            Err(io::Error::other("the input pipe closed")),
        ],
    );

    assert_eq!(run.outcome.expect("the schedule runs"), Finish::Terminated);
    assert_eq!(run.terminations, 1);
    assert!(
        !input.stalled.get(),
        "a failed write is not an abandoned one"
    );
}
