//! Write windows for the Windows terminal input path.
//!
//! Every bounded write is given a window, and no window may be shorter than
//! [`MIN_INPUT_WAIT`]. This module holds that floor, the bounded write, and the schedule of
//! the escalation's Ctrl+C writes. It uses only `std` and makes no OS calls, so
//! `tests/window_host.rs` includes it by path and runs on every host. The Windows crate
//! reaches it through `mod window`.

use std::io::{self, Write};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

/// The shortest window any single terminal write is given. It is the shortest input wait that
/// [`crate::session::Session::send_text_within`] and `interrupt::escalate` accept, and the
/// shortest window of each Ctrl+C write that `escalate` makes, its second write included.
///
/// A write thread must start and reach the pipe before its window ends. A shorter window can end
/// before a fast write completes, and that false `TimedOut` stalls input for the rest of the
/// session. The value is a conservative floor, not measured on Windows: it sits above one
/// default Windows timer tick (about 15.6 ms), because timed waits there round to ticks.
pub const MIN_INPUT_WAIT: Duration = Duration::from_millis(20);

/// Reports whether a write may be given `window`: it must be at least [`MIN_INPUT_WAIT`].
pub fn window_meets_floor(window: Duration) -> bool {
    window >= MIN_INPUT_WAIT
}

/// Whether a write finished before its window ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BoundedWrite {
    /// The bytes were written and flushed before the window ended.
    Finished,
    /// The window ended first. The write is still blocked on its own thread.
    Pending,
}

/// The clock the escalation schedule reads and sleeps on.
pub(crate) trait Clock {
    /// Returns the time elapsed since the clock started.
    fn elapsed(&self) -> Duration;
    /// Sleeps for `duration`.
    fn sleep(&self, duration: Duration);
}

/// How an escalation schedule ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Finish {
    /// The job emptied before the wait ran out.
    Exited,
    /// The schedule terminated the job.
    Terminated,
}

/// Runs the schedule of `interrupt::escalate`, with the job, the clock and the Ctrl+C write
/// passed in so that tests can drive each branch.
///
/// `active` reads the job's process count, `terminate` ends the job, and
/// `ctrl_c_pending_within` writes one Ctrl+C with the given window and returns whether it was
/// still pending when that window ended. The first Ctrl+C has a window of `wait`.
///
/// Let `t1` be the time the first Ctrl+C returned. The poll then wakes at `t1 + k * poll` for
/// k = 1, 2, ..., capped at the deadline, so its wakes are phased by `t1`, not by time zero.
/// Those times leave out the overhead of each membership read and sleep. The second Ctrl+C is
/// written once, at the first wake at or after half of `wait`, with the time that remains as its
/// window. If the first Ctrl+C itself returns at or after half of `wait`, the second is written
/// at once instead. It is skipped when that remaining time is below [`MIN_INPUT_WAIT`]. When the
/// first Ctrl+C returns before half of `wait`, a wait of `poll` or less has no wake before its
/// deadline, so it gets no second Ctrl+C.
///
/// For example, with `wait` of 500 ms and `poll` of 250 ms, a first Ctrl+C that returns at 240 ms
/// wakes at 490 ms with 10 ms left, under the floor. The second Ctrl+C is skipped, so the
/// schedule is the first write alone.
///
/// # Errors
///
/// Returns the error of `active`, `terminate`, or of the first Ctrl+C write. A failed second
/// Ctrl+C does not stop the schedule.
pub(crate) fn escalate_schedule(
    wait: Duration,
    poll: Duration,
    clock: &impl Clock,
    mut active: impl FnMut() -> io::Result<u32>,
    mut terminate: impl FnMut() -> io::Result<()>,
    mut ctrl_c_pending_within: impl FnMut(Duration) -> io::Result<bool>,
) -> io::Result<Finish> {
    if active()? == 0 {
        return Ok(Finish::Exited);
    }
    let start = clock.elapsed();
    let since_start = || clock.elapsed().saturating_sub(start);
    if ctrl_c_pending_within(wait)? {
        terminate()?;
        return Ok(Finish::Terminated);
    }

    let half = wait / 2;
    let mut second_sent = false;
    loop {
        if active()? == 0 {
            return Ok(Finish::Exited);
        }
        if since_start() >= wait {
            break;
        }
        if !second_sent && since_start() >= half {
            second_sent = true;
            // The second write gets what remains of `wait`, not all of it. A window under the
            // floor cannot cover the write, so the write is skipped.
            let remaining = wait.saturating_sub(since_start());
            // A failed second write must not stop the escalation, so its error is not
            // reported. A write still pending at the end of its window ends the wait.
            if window_meets_floor(remaining) && ctrl_c_pending_within(remaining).unwrap_or(false) {
                break;
            }
        }
        // The remaining time is read after the write, so a slow write cannot carry the sleep
        // past the deadline.
        clock.sleep(poll.min(wait.saturating_sub(since_start())));
    }
    terminate()?;
    Ok(Finish::Terminated)
}

/// Decides one Ctrl+C write of the escalation, and reports whether it was still pending when its
/// window ended.
///
/// `stalled` says whether an earlier abandoned write already stalled the input. Such an input
/// sends nothing and reports the Ctrl+C as pending, so the escalation ends the job at once.
/// Otherwise `write` makes the write with `window`. A pending write calls `mark_stalled`, because
/// its bytes may still arrive and no later input may interleave with them. A write that fails
/// returns its error and stalls nothing.
///
/// # Errors
///
/// Returns the error of `write`.
pub(crate) fn ctrl_c_within(
    stalled: bool,
    window: Duration,
    write: impl FnOnce(Duration) -> io::Result<BoundedWrite>,
    mark_stalled: impl FnOnce(),
) -> io::Result<bool> {
    if stalled {
        return Ok(true);
    }
    let pending = write(window)? == BoundedWrite::Pending;
    if pending {
        mark_stalled();
    }
    Ok(pending)
}

/// Writes `bytes` to `sink` and flushes it on a new thread, and waits for the result for up to
/// `window`. The window starts when this function is called, before the thread starts, so a slow
/// start uses part of it. A write still pending when its window ends is
/// abandoned on its thread, not cancelled: the thread keeps `sink` until the write returns, and the
/// caller does not wait for it.
///
/// # Errors
///
/// Returns `InvalidInput` when `window` is shorter than [`MIN_INPUT_WAIT`] or too long to reach a
/// deadline. Returns the I/O error of a write that fails before the window ends, and the error of a
/// thread that cannot be started. In both cases nothing is written.
pub(crate) fn bounded_write<W>(
    sink: W,
    bytes: Vec<u8>,
    window: Duration,
) -> io::Result<BoundedWrite>
where
    W: Write + Send + 'static,
{
    bounded_write_on(spawn_write_thread, sink, bytes, window)
}

/// Starts `task` on a new thread. The builder reports a failure to start the thread as an error,
/// where `thread::spawn` would panic.
fn spawn_write_thread(task: Box<dyn FnOnce() + Send>) -> io::Result<()> {
    thread::Builder::new().spawn(task).map(|_handle| ())
}

/// [`bounded_write`] with the thread started by `spawn`, so that a test can make the start fail.
pub(crate) fn bounded_write_on<W, S>(
    spawn: S,
    mut sink: W,
    bytes: Vec<u8>,
    window: Duration,
) -> io::Result<BoundedWrite>
where
    W: Write + Send + 'static,
    S: FnOnce(Box<dyn FnOnce() + Send>) -> io::Result<()>,
{
    // The floor applies to every write, so a window below it is refused before anything is written.
    if !window_meets_floor(window) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("a write window must be at least {MIN_INPUT_WAIT:?}; nothing was written"),
        ));
    }
    let deadline = Instant::now().checked_add(window).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "the write window is too long to reach a deadline",
        )
    })?;
    let (done, outcome) = mpsc::channel();
    spawn(Box::new(move || {
        let result = sink.write_all(&bytes).and_then(|()| sink.flush());
        // The caller may have stopped waiting already; then the result is not needed.
        let _ = done.send(result);
    }))?;
    match outcome.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(result) => result.map(|()| BoundedWrite::Finished),
        Err(RecvTimeoutError::Timeout) => Ok(BoundedWrite::Pending),
        Err(RecvTimeoutError::Disconnected) => Err(io::Error::other(
            "the write thread ended without reporting a result",
        )),
    }
}
