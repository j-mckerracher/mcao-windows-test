//! The interrupt ladder: escalating ways to stop a worker, with stop evidence per rung.
//!
//! [`run_ladder`] tries one rung at a time, waits a bounded time for evidence that
//! the controlled operation stopped, and escalates when none arrives:
//!
//! 1. [`Rung::Escape`]: a single Escape key written to the terminal input. This is
//!    the intended interrupt for the Claude Code adapter.
//! 2. [`Rung::ExtendedWait`]: no second key, only a longer wait. It tells a slow
//!    stop from no stop.
//! 3. [`Rung::CtrlC`]: a Ctrl+C character written to the terminal input. The
//!    console turns it into `CTRL_C_EVENT` for processes that accept it. A child
//!    started in a new process group does not.
//! 4. [`Rung::CtrlBreak`]: `GenerateConsoleCtrlEvent` with `CTRL_BREAK_EVENT`, sent
//!    through the crate's `ctrl_break` module. It needs a child started in a new process
//!    group, so the rung is skipped for any other child.
//! 5. [`Rung::TerminateJob`]: `TerminateJobObject`, which ends the whole tree.
//!
//! # Timeouts
//!
//! No wait has a default. The decision record fixes each value from measurements on
//! the Windows runner, so the caller passes every one in [`LadderTimeouts`], and
//! [`LadderTimeouts::validate`] refuses a zero or unbounded wait.
//!
//! # Stop evidence
//!
//! A rung stops the operation only when [`Evidence`] shows it. The caller's stop
//! marker in terminal output received after the rung started is evidence for every
//! rung. A child exit is evidence for [`Rung::CtrlC`], [`Rung::CtrlBreak`] and
//! [`Rung::TerminateJob`], which end the process by design. It is not evidence for
//! [`Rung::Escape`] and [`Rung::ExtendedWait`], which must stop the operation and
//! leave the worker alive: an exit there is recorded as [`RungResult::ChildExited`]
//! and ends the ladder. A quiet terminal is never evidence. A rung that is sent and
//! shows no evidence within its wait is recorded as not stopped, and its
//! [`RungRecord::support`] is never `Demonstrated`.

use std::io;
use std::thread;
use std::time::{Duration, Instant};

use mcao_core::{Execution, Outcome, Support, classify};

use crate::ctrl_break;
use crate::process::ProcessGroup;
use crate::session::{MIN_INPUT_WAIT, Session};
use crate::window::{Clock, Finish, bounded_write, ctrl_c_within, escalate_schedule};

/// The longest wait any rung accepts. It keeps every wait bounded.
pub const MAX_RUNG_WAIT: Duration = Duration::from_secs(600);

/// Exit code [`Rung::TerminateJob`] gives to every process in the job.
pub const TERMINATE_EXIT: u32 = 1;

/// How often a wait looks for evidence when no output arrives.
const POLL: Duration = Duration::from_millis(25);
/// The Escape key as terminal input.
const ESCAPE: &str = "\u{1b}";
/// The Ctrl+C character as terminal input. The console reads it as Ctrl+C when
/// processed input is on.
const CTRL_C: &str = "\u{3}";

/// One step of the ladder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rung {
    /// A single Escape key.
    Escape,
    /// A longer wait after Escape, with no second key.
    ExtendedWait,
    /// Ctrl+C written to the terminal input.
    CtrlC,
    /// `CTRL_BREAK_EVENT` sent to the child's process group.
    CtrlBreak,
    /// `TerminateJobObject` on the session's job.
    TerminateJob,
}

impl Rung {
    /// Every rung, in the order the ladder tries them.
    pub const LADDER: [Rung; 5] = [
        Rung::Escape,
        Rung::ExtendedWait,
        Rung::CtrlC,
        Rung::CtrlBreak,
        Rung::TerminateJob,
    ];

    /// A short name for evidence text.
    pub fn name(self) -> &'static str {
        match self {
            Rung::Escape => "escape",
            Rung::ExtendedWait => "extended wait",
            Rung::CtrlC => "ctrl+c",
            Rung::CtrlBreak => "ctrl+break",
            Rung::TerminateJob => "terminate job",
        }
    }
}

/// How long each rung waits for stop evidence after it is sent.
///
/// There is no `Default`: each value comes from a measurement, so each is stated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LadderTimeouts {
    /// The wait after a single Escape.
    pub escape: Duration,
    /// The wait after that, with no second key.
    pub extended_wait: Duration,
    /// The wait after Ctrl+C.
    pub ctrl_c: Duration,
    /// The wait after `CTRL_BREAK_EVENT`.
    pub ctrl_break: Duration,
    /// The wait for the job to empty after `TerminateJobObject`.
    pub terminate: Duration,
}

impl LadderTimeouts {
    /// Returns the wait for `rung`.
    pub fn for_rung(&self, rung: Rung) -> Duration {
        match rung {
            Rung::Escape => self.escape,
            Rung::ExtendedWait => self.extended_wait,
            Rung::CtrlC => self.ctrl_c,
            Rung::CtrlBreak => self.ctrl_break,
            Rung::TerminateJob => self.terminate,
        }
    }

    /// Refuses a wait that is zero or longer than [`MAX_RUNG_WAIT`].
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` naming the first rung whose wait is out of range.
    pub fn validate(&self) -> io::Result<()> {
        for rung in Rung::LADDER {
            let wait = self.for_rung(rung);
            if wait.is_zero() || wait > MAX_RUNG_WAIT {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "the {} wait must be above zero and at most {} s",
                        rung.name(),
                        MAX_RUNG_WAIT.as_secs()
                    ),
                ));
            }
        }
        Ok(())
    }
}

/// What was observed about the child, the job and the output at one moment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Evidence {
    /// Whether the stop marker appeared in output received after the rung started.
    pub marker_seen: bool,
    /// The child's exit code, if it has exited.
    pub child_exit_code: Option<u32>,
    /// How many processes the job still holds.
    pub job_processes: u32,
}

impl Evidence {
    /// Reports whether this evidence shows that `rung` stopped the operation.
    ///
    /// The two keyless-exit rungs, Escape and the extended wait, need the stop
    /// marker: a child exit there is not a stop of the operation. Ctrl+C and
    /// Ctrl+Break need the stop marker or a child exit. The last rung ends the
    /// whole tree, so it needs an exit and an empty job.
    fn shows_stop(&self, rung: Rung) -> bool {
        match rung {
            Rung::Escape | Rung::ExtendedWait => self.marker_seen,
            Rung::CtrlC | Rung::CtrlBreak => self.marker_seen || self.child_exit_code.is_some(),
            Rung::TerminateJob => self.child_exit_code.is_some() && self.job_processes == 0,
        }
    }

    /// Reports whether the child exited at a rung that is meant to leave it alive.
    fn exited_early(&self, rung: Rung) -> bool {
        matches!(rung, Rung::Escape | Rung::ExtendedWait) && self.child_exit_code.is_some()
    }
}

/// Why a rung was not tried.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkipReason {
    /// The child was not started in a new process group, so `CTRL_BREAK_EVENT`
    /// has no group to target.
    NoProcessGroup,
}

/// What happened at one rung.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RungResult {
    /// The rung was sent and the evidence shows the operation stopped.
    Stopped(Evidence),
    /// The rung was sent and its wait ended without stop evidence. The value is
    /// the last evidence observed.
    NotStopped(Evidence),
    /// The rung could not be sent. The text is the OS error.
    SendFailed(String),
    /// The rung was sent and the whole child exited at a rung that must leave it
    /// alive (Escape or the extended wait), without the stop marker. The operation
    /// did not stop on its own. The ladder ends here, because there is no process
    /// left to interrupt.
    ChildExited(Evidence),
    /// The rung's evidence could not be read, so no later rung is judged. The text
    /// is the OS error. The ladder ends here with the records collected so far.
    EvidenceUnreadable(String),
    /// The rung was not tried.
    Skipped(SkipReason),
}

impl RungResult {
    /// Reports whether the ladder stops after this result.
    fn ends_ladder(&self) -> bool {
        matches!(
            self,
            RungResult::Stopped(_) | RungResult::ChildExited(_) | RungResult::EvidenceUnreadable(_)
        )
    }
}

/// The record of one rung.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RungRecord {
    /// The rung this record is about.
    pub rung: Rung,
    /// The wait that applied to the rung.
    pub timeout: Duration,
    /// How long the rung waited, from the moment it was sent.
    pub waited: Duration,
    /// What happened.
    pub result: RungResult,
    /// A problem that did not change the result, for example a console that could
    /// not be restored after a delivered Ctrl+Break.
    pub warning: Option<String>,
}

impl RungRecord {
    /// Returns the lifecycle outcome this rung supports.
    ///
    /// Only a rung with stop evidence is `Success`. A rung that was sent and did
    /// not stop, could not be sent, ended the whole child where it must stay alive,
    /// or could not be read, is `Failed`. A skipped rung is `Unsupported`.
    pub fn outcome(&self) -> Outcome {
        match &self.result {
            RungResult::Stopped(_) => Outcome::Success,
            RungResult::NotStopped(evidence) => Outcome::Failed {
                reason: format!(
                    "{} showed no stop evidence within {} ms (marker seen: {}, child exit code: {:?}, job processes: {})",
                    self.rung.name(),
                    self.timeout.as_millis(),
                    evidence.marker_seen,
                    evidence.child_exit_code,
                    evidence.job_processes,
                ),
            },
            RungResult::SendFailed(error) => Outcome::Failed {
                reason: format!("{} could not be sent: {error}", self.rung.name()),
            },
            RungResult::ChildExited(evidence) => Outcome::Failed {
                reason: format!(
                    "the child exited at {} instead of stopping the operation (child exit code: {:?}, job processes: {})",
                    self.rung.name(),
                    evidence.child_exit_code,
                    evidence.job_processes,
                ),
            },
            RungResult::EvidenceUnreadable(error) => Outcome::Failed {
                reason: format!(
                    "{} stop evidence could not be read: {error}",
                    self.rung.name()
                ),
            },
            RungResult::Skipped(SkipReason::NoProcessGroup) => Outcome::Unsupported {
                reason: format!(
                    "{} needs a child started in a new process group",
                    self.rung.name()
                ),
            },
        }
    }

    /// Returns the compatibility classification of this rung. It was executed on
    /// the native backend, so only stop evidence makes it `Demonstrated`.
    pub fn support(&self) -> Support {
        classify(Execution::Native, &self.outcome())
    }
}

/// The records of one ladder run, in the order the rungs were tried.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LadderReport {
    records: Vec<RungRecord>,
}

impl LadderReport {
    /// Returns the records, first rung first.
    pub fn records(&self) -> &[RungRecord] {
        &self.records
    }

    /// Returns the rung whose evidence shows the stop, if any rung did.
    pub fn stopped_by(&self) -> Option<Rung> {
        self.records
            .iter()
            .find(|record| matches!(record.result, RungResult::Stopped(_)))
            .map(|record| record.rung)
    }
}

/// Interrupts the session's child, one rung at a time, until evidence shows a stop.
///
/// `stop_marker` is output the controlled operation prints only after it stops. It
/// counts only when it arrives after the rung that is being judged was sent. The
/// ladder ends at the first rung with stop evidence, at a child exit where the
/// child must stay alive ([`RungResult::ChildExited`]), at a rung whose evidence
/// cannot be read ([`RungResult::EvidenceUnreadable`]), or after
/// [`Rung::TerminateJob`] has been tried.
///
/// The [`Rung::Escape`] and [`Rung::CtrlC`] writes use the blocking [`Session::send_text`],
/// which has no deadline. A write the terminal does not accept blocks before the rung's wait
/// starts, so only the wait after the write is bounded. A session whose input an abandoned
/// write stalled refuses those writes, and the rung is recorded as [`RungResult::SendFailed`].
///
/// # Errors
///
/// Returns `InvalidInput` when `timeouts` fail [`LadderTimeouts::validate`], when
/// `stop_marker` is empty, or when the child has already exited. Returns an OS
/// error when the child cannot be queried before the first rung. Once a rung is
/// under way, a rung that cannot be sent or whose evidence cannot be read is a
/// record, never an error, so the records collected so far are kept.
pub fn run_ladder(
    session: &mut Session,
    stop_marker: &[u8],
    timeouts: &LadderTimeouts,
) -> io::Result<LadderReport> {
    timeouts.validate()?;
    if stop_marker.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the stop marker must not be empty",
        ));
    }
    if session.child().wait_timeout(Duration::ZERO)?.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "there is nothing to interrupt: the child has already exited",
        ));
    }
    let mut records = Vec::with_capacity(Rung::LADDER.len());
    for rung in Rung::LADDER {
        let record = run_rung(session, rung, stop_marker, timeouts.for_rung(rung));
        let ends = record.result.ends_ladder();
        records.push(record);
        if ends {
            break;
        }
    }
    Ok(LadderReport { records })
}

/// Sends one rung and waits up to `timeout` for stop evidence.
fn run_rung(
    session: &mut Session,
    rung: Rung,
    stop_marker: &[u8],
    timeout: Duration,
) -> RungRecord {
    run_rung_with(session, rung, stop_marker, timeout, |session| {
        send(session, rung)
    })
}

/// Runs one rung with `send` as its action. `send` returns a warning to attach to
/// the record when the action worked but left a problem behind.
fn run_rung_with(
    session: &mut Session,
    rung: Rung,
    stop_marker: &[u8],
    timeout: Duration,
    send: impl FnOnce(&mut Session) -> io::Result<Option<String>>,
) -> RungRecord {
    let record = |waited, result, warning| RungRecord {
        rung,
        timeout,
        waited,
        result,
        warning,
    };
    if rung == Rung::CtrlBreak && session.group() == ProcessGroup::Inherit {
        return record(
            Duration::ZERO,
            RungResult::Skipped(SkipReason::NoProcessGroup),
            None,
        );
    }
    let mark = match session.output_mark() {
        Ok(mark) => mark,
        Err(error) => {
            return record(
                Duration::ZERO,
                RungResult::EvidenceUnreadable(error.to_string()),
                None,
            );
        }
    };
    let started = Instant::now();
    let warning = match send(session) {
        Ok(warning) => warning,
        Err(error) => {
            return record(
                started.elapsed(),
                RungResult::SendFailed(error.to_string()),
                None,
            );
        }
    };
    let deadline = started + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let wait = POLL.min(remaining);
        let tick = Instant::now();
        let evidence = match observe(session, mark, stop_marker, wait) {
            Ok(evidence) => evidence,
            Err(error) => {
                return record(
                    started.elapsed(),
                    RungResult::EvidenceUnreadable(error.to_string()),
                    warning,
                );
            }
        };
        if evidence.shows_stop(rung) {
            return record(started.elapsed(), RungResult::Stopped(evidence), warning);
        }
        if evidence.exited_early(rung) {
            return record(
                started.elapsed(),
                RungResult::ChildExited(evidence),
                warning,
            );
        }
        if remaining.is_zero() {
            return record(started.elapsed(), RungResult::NotStopped(evidence), warning);
        }
        // The output wait ends early when the stream has closed. Sleep out the
        // rest of the tick so that a closed stream cannot turn the loop into a spin.
        let spent = tick.elapsed();
        if spent < wait {
            thread::sleep(wait - spent);
        }
    }
}

/// Performs the action of `rung`. Returns a warning when the action was delivered
/// and left a problem behind.
fn send(session: &mut Session, rung: Rung) -> io::Result<Option<String>> {
    match rung {
        Rung::Escape => session.send_text(ESCAPE).map(|()| None),
        Rung::ExtendedWait => Ok(None),
        Rung::CtrlC => session.send_text(CTRL_C).map(|()| None),
        Rung::CtrlBreak => {
            ctrl_break::send_ctrl_break(session.child().id()).map(|sent| sent.console_warning)
        }
        Rung::TerminateJob => session.job().terminate(TERMINATE_EXIT).map(|()| None),
    }
}

/// Reads the evidence now, waiting up to `wait` for the marker to arrive first.
fn observe(
    session: &Session,
    mark: u64,
    stop_marker: &[u8],
    wait: Duration,
) -> io::Result<Evidence> {
    let marker_seen = session.wait_for_output_after(mark, stop_marker, wait)?;
    Ok(Evidence {
        marker_seen,
        child_exit_code: session.child().wait_timeout(Duration::ZERO)?,
        job_processes: session.job().active_processes()?,
    })
}

/// A provisional wait for the tests of [`escalate`]. Production callers pass their own
/// wait to [`escalate`], and this constant is not used there. The measured interrupt
/// timeout is owed by DPES-758 on the Windows runner, so this 2 s value is not a
/// production default.
pub const DEFAULT_ESCALATION_WAIT: Duration = Duration::from_secs(2);

/// The interval at which [`escalate`] checks job membership.
pub const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// The exit code [`escalate`] gives the job when the wait runs out (DPES-886 AC4).
pub const ESCALATION_EXIT: u32 = 0x1F;

/// The result of [`escalate`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Escalation {
    /// The job was empty before the wait ran out, or was already empty. A Ctrl+C may have caused
    /// the exit, but the escalation does not check that.
    Exited,
    /// The wait ran out with processes still in the job, input was already stalled by an
    /// abandoned write, or a Ctrl+C write was still pending at the deadline, and the job was
    /// terminated.
    TerminatedJob,
}

/// The wall clock an escalation reads and sleeps on.
struct RealClock {
    started: Instant,
}

impl Clock for RealClock {
    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    fn sleep(&self, duration: Duration) {
        thread::sleep(duration);
    }
}

/// Interrupts the worker's process tree with Ctrl+C and ends the tree if it does not stop.
///
/// The sequence, from DPES-886 AC4:
///
/// 1. An already-empty job returns [`Escalation::Exited`] without writing anything.
/// 2. If an earlier write was abandoned, the session's input is stalled and nothing is
///    written. The Ctrl+C counts as pending, so the job is terminated at once and
///    [`Escalation::TerminatedJob`] is returned. This skips the grace period and the second
///    Ctrl+C, so a child that would have stopped on the Ctrl+C is ended without that chance.
/// 3. Ctrl+C is written to the ConPTY input. A write still pending at the deadline terminates
///    the job at once, as in step 2, and stalls the session's input.
/// 4. Job membership is polled every [`POLL_INTERVAL`]. An empty job returns
///    [`Escalation::Exited`].
/// 5. A second Ctrl+C is written once, with the time that remains of `wait` as its window. Let
///    `t1` be the time the first Ctrl+C returned. The poll wakes at `t1 + k * POLL_INTERVAL`
///    for k = 1, 2, ..., not at multiples of the interval from time zero, plus the overhead of
///    each membership read and sleep. The second Ctrl+C goes at the first wake at or after half
///    of `wait`, or at `t1` when the first Ctrl+C returned at or after half of `wait`. When less
///    than [`MIN_INPUT_WAIT`] remains, the second Ctrl+C is skipped and nothing is written. When
///    the first Ctrl+C returns before half of `wait`, a wait of [`POLL_INTERVAL`] (250 ms) or
///    less has no wake before its deadline, so it gets no second Ctrl+C.
///
///    With `INTERRUPT_WAIT` (500 ms, in `backend.rs`) and [`POLL_INTERVAL`] (250 ms), the second
///    Ctrl+C is skipped exactly when the first Ctrl+C returns in (230 ms, 250 ms) or
///    (480 ms, 500 ms). For example, a first return at 240 ms wakes at 490 ms with 10 ms left, so
///    the schedule is [500] and the second is skipped. A first return at 230 ms wakes at 480 ms
///    with exactly the 20 ms floor and is not skipped. A first return at 250 ms or 480 ms writes
///    at once, with 250 ms or 20 ms left, and is not skipped. A late poll wake moves the lower
///    edge of the first band earlier by its overhead. Other waits have their own bands: a 260 ms
///    wait skips a first return before 130 ms and one in (240 ms, 260 ms).
///
///    A failed write does not stop the escalation, because the job is still terminated at
///    `wait`. A write still pending at the deadline ends the wait at once and stalls the
///    session's input.
/// 6. When `wait` runs out with processes still in the job, the job is terminated with
///    [`ESCALATION_EXIT`] and [`Escalation::TerminatedJob`] is returned.
///
/// This path never calls `GenerateConsoleCtrlEvent`. A failed Ctrl+C write or membership
/// read returns the error and leaves the job running, so the caller decides the next step.
///
/// `wait` bounds the schedule, Ctrl+C writes included. The first Ctrl+C has a window of `wait`,
/// and the second has what remains of `wait` when it is written. Each write runs on its own
/// thread and is waited for only until its window ends. Each poll sleep is capped at the time
/// left to `wait`, so the schedule never plans a wait past `wait`. The call can still return
/// later than `wait` by what its membership reads, its terminate call, and any overshoot of the
/// OS sleep take; this code does not bound that overhead. A write still pending at the end of its
/// window ends the wait as if it had run out: the job is terminated and the call returns. The
/// blocked write stays on its thread for as long as the input pipe neither accepts nor fails it.
/// Whether closing the pseudoconsole releases it is not verified on Windows, so a write abandoned
/// this way can outlive the call and stalls the session's input for the rest of the session.
///
/// # Errors
///
/// `InvalidInput` when `wait` is shorter than [`MIN_INPUT_WAIT`], because its Ctrl+C write
/// could not reach the pipe before the deadline and would stall input. It is also refused
/// when `wait` is longer than [`MAX_RUNG_WAIT`], or when the session's child is in
/// [`ProcessGroup::New`], which does not receive Ctrl+C. Nothing is written in either case.
pub fn escalate(session: &mut Session, wait: Duration) -> io::Result<Escalation> {
    if wait < MIN_INPUT_WAIT || wait > MAX_RUNG_WAIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "escalation wait must be at least {MIN_INPUT_WAIT:?} and at most {MAX_RUNG_WAIT:?}"
            ),
        ));
    }
    if session.group() == ProcessGroup::New {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "escalation needs a child that receives Ctrl+C; ProcessGroup::New does not",
        ));
    }
    let session: &Session = session;
    let clock = RealClock {
        started: Instant::now(),
    };
    let finish = escalate_schedule(
        wait,
        POLL_INTERVAL,
        &clock,
        || session.job().active_processes(),
        || session.job().terminate(ESCALATION_EXIT),
        |window| ctrl_c_pending_within(session, window),
    )?;
    Ok(match finish {
        Finish::Exited => Escalation::Exited,
        Finish::Terminated => Escalation::TerminatedJob,
    })
}

/// Writes one Ctrl+C to the terminal input, with `window` as its write window. Returns `true`
/// when the write was still pending when that window ended.
///
/// A session whose input is already stalled by an abandoned write sends nothing and reports
/// the Ctrl+C as pending, so the escalation ends the job as it does for a pending write. A
/// write that is pending when its window ends stalls the session's input, so no later input can
/// interleave with it.
///
/// # Errors
///
/// Returns the I/O error of a write that fails before the window ends, of a window below
/// [`MIN_INPUT_WAIT`], or of a handle that cannot be duplicated.
fn ctrl_c_pending_within(session: &Session, window: Duration) -> io::Result<bool> {
    ctrl_c_within(
        session.input_is_stalled(),
        window,
        |window| bounded_write(session.input_handle()?, CTRL_C.as_bytes().to_vec(), window),
        || session.mark_input_stalled(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conpty::ConsoleSize;

    const SECOND: Duration = Duration::from_secs(1);

    fn timeouts() -> LadderTimeouts {
        LadderTimeouts {
            escape: SECOND,
            extended_wait: SECOND,
            ctrl_c: SECOND,
            ctrl_break: SECOND,
            terminate: SECOND,
        }
    }

    fn evidence(marker_seen: bool, child_exit_code: Option<u32>, job_processes: u32) -> Evidence {
        Evidence {
            marker_seen,
            child_exit_code,
            job_processes,
        }
    }

    fn record(result: RungResult) -> RungRecord {
        RungRecord {
            rung: Rung::Escape,
            timeout: SECOND,
            waited: SECOND,
            result,
            warning: None,
        }
    }

    #[test]
    fn timeouts_within_range_are_accepted() {
        timeouts().validate().expect("one second is in range");
        let longest = LadderTimeouts {
            terminate: MAX_RUNG_WAIT,
            ..timeouts()
        };
        longest.validate().expect("the maximum wait is in range");
    }

    #[test]
    fn a_zero_wait_is_refused_and_names_its_rung() {
        for rung in Rung::LADDER {
            let mut bad = timeouts();
            match rung {
                Rung::Escape => bad.escape = Duration::ZERO,
                Rung::ExtendedWait => bad.extended_wait = Duration::ZERO,
                Rung::CtrlC => bad.ctrl_c = Duration::ZERO,
                Rung::CtrlBreak => bad.ctrl_break = Duration::ZERO,
                Rung::TerminateJob => bad.terminate = Duration::ZERO,
            }
            let error = bad.validate().expect_err("a zero wait is refused");
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            assert!(error.to_string().contains(rung.name()), "{error}");
        }
    }

    #[test]
    fn a_wait_past_the_maximum_is_refused() {
        let bad = LadderTimeouts {
            ctrl_c: MAX_RUNG_WAIT + Duration::from_millis(1),
            ..timeouts()
        };
        let error = bad.validate().expect_err("an unbounded wait is refused");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("ctrl+c"), "{error}");
    }

    #[test]
    fn each_rung_reads_its_own_wait() {
        let distinct = LadderTimeouts {
            escape: Duration::from_millis(1),
            extended_wait: Duration::from_millis(2),
            ctrl_c: Duration::from_millis(3),
            ctrl_break: Duration::from_millis(4),
            terminate: Duration::from_millis(5),
        };
        let waits: Vec<u128> = Rung::LADDER
            .iter()
            .map(|&rung| distinct.for_rung(rung).as_millis())
            .collect();
        assert_eq!(waits, [1, 2, 3, 4, 5]);
    }

    #[test]
    fn a_quiet_terminal_is_not_a_stop() {
        let quiet = evidence(false, None, 1);
        for rung in Rung::LADDER {
            assert!(!quiet.shows_stop(rung), "{}", rung.name());
        }
    }

    #[test]
    fn the_marker_stops_every_rung_but_the_last() {
        for rung in [
            Rung::Escape,
            Rung::ExtendedWait,
            Rung::CtrlC,
            Rung::CtrlBreak,
        ] {
            assert!(evidence(true, None, 1).shows_stop(rung), "{}", rung.name());
        }
    }

    #[test]
    fn an_exit_stops_ctrl_c_and_ctrl_break_but_not_the_keyless_rungs() {
        for rung in [Rung::CtrlC, Rung::CtrlBreak] {
            assert!(
                evidence(false, Some(0), 1).shows_stop(rung),
                "{}",
                rung.name()
            );
        }
        for rung in [Rung::Escape, Rung::ExtendedWait] {
            let exited = evidence(false, Some(0), 0);
            assert!(!exited.shows_stop(rung), "{}", rung.name());
            assert!(exited.exited_early(rung), "{}", rung.name());
        }
        for rung in [Rung::CtrlC, Rung::CtrlBreak, Rung::TerminateJob] {
            assert!(!evidence(false, Some(0), 0).exited_early(rung));
        }
        assert!(!evidence(false, None, 1).exited_early(Rung::Escape));
    }

    #[test]
    fn the_marker_beats_an_exit_at_escape() {
        let both = evidence(true, Some(0), 0);
        assert!(both.shows_stop(Rung::Escape));
    }

    #[test]
    fn terminate_needs_an_exit_and_an_empty_job() {
        assert!(evidence(false, Some(TERMINATE_EXIT), 0).shows_stop(Rung::TerminateJob));
        assert!(!evidence(false, Some(TERMINATE_EXIT), 1).shows_stop(Rung::TerminateJob));
        assert!(!evidence(true, None, 0).shows_stop(Rung::TerminateJob));
    }

    #[test]
    fn only_a_stopped_rung_is_demonstrated() {
        let stopped = record(RungResult::Stopped(evidence(true, None, 1)));
        assert_eq!(stopped.outcome(), Outcome::Success);
        assert_eq!(stopped.support(), Support::Demonstrated);

        let not_stopped = record(RungResult::NotStopped(evidence(false, None, 1)));
        assert!(matches!(not_stopped.outcome(), Outcome::Failed { .. }));
        assert_eq!(not_stopped.support(), Support::Unresolved);

        let unsent = record(RungResult::SendFailed("broken pipe".to_owned()));
        assert_eq!(unsent.support(), Support::Unresolved);

        let skipped = record(RungResult::Skipped(SkipReason::NoProcessGroup));
        assert!(matches!(skipped.outcome(), Outcome::Unsupported { .. }));
        assert_eq!(skipped.support(), Support::Unsupported);

        let exited = record(RungResult::ChildExited(evidence(false, Some(0), 0)));
        assert!(matches!(exited.outcome(), Outcome::Failed { .. }));
        assert_eq!(exited.support(), Support::Unresolved);

        let unreadable = record(RungResult::EvidenceUnreadable("access denied".to_owned()));
        assert!(matches!(unreadable.outcome(), Outcome::Failed { .. }));
        assert_eq!(unreadable.support(), Support::Unresolved);
    }

    #[test]
    fn only_a_stop_an_exit_or_unreadable_evidence_ends_the_ladder() {
        let ends = |result: RungResult| result.ends_ladder();
        assert!(ends(RungResult::Stopped(evidence(true, None, 1))));
        assert!(ends(RungResult::ChildExited(evidence(false, Some(0), 0))));
        assert!(ends(RungResult::EvidenceUnreadable("x".to_owned())));
        assert!(!ends(RungResult::NotStopped(evidence(false, None, 1))));
        assert!(!ends(RungResult::SendFailed("x".to_owned())));
        assert!(!ends(RungResult::Skipped(SkipReason::NoProcessGroup)));
    }

    #[test]
    fn a_failed_reason_carries_the_evidence() {
        let reason = match record(RungResult::NotStopped(evidence(false, None, 3))).outcome() {
            Outcome::Failed { reason } => reason,
            other => panic!("expected a failed outcome, got {other:?}"),
        };
        assert!(reason.contains("1000 ms"), "{reason}");
        assert!(reason.contains("job processes: 3"), "{reason}");
    }

    #[test]
    fn the_report_names_the_rung_that_stopped() {
        let report = LadderReport {
            records: vec![
                record(RungResult::NotStopped(evidence(false, None, 1))),
                RungRecord {
                    rung: Rung::CtrlC,
                    ..record(RungResult::Stopped(evidence(false, Some(1), 0)))
                },
            ],
        };
        assert_eq!(report.stopped_by(), Some(Rung::CtrlC));
        let none = LadderReport {
            records: vec![record(RungResult::SendFailed("x".to_owned()))],
        };
        assert_eq!(none.stopped_by(), None);
    }

    /// Re-exec stub: stays alive until the job ends it.
    #[test]
    #[ignore = "re-exec stub; run only by the rung tests"]
    fn stub_idle() {
        thread::sleep(Duration::from_secs(120));
    }

    /// A session whose job ends when it drops, so a failed assertion cannot leak
    /// the stub.
    struct Running(Session);

    impl Drop for Running {
        fn drop(&mut self) {
            // Best effort: the job may already be empty.
            let _ = self.0.job().terminate(TERMINATE_EXIT);
        }
    }

    fn idle_session() -> Running {
        let exe = std::env::current_exe().expect("test binary path is available");
        let args = [
            "--exact",
            "interrupt::tests::stub_idle",
            "--ignored",
            "--test-threads=1",
        ];
        let size = ConsoleSize {
            columns: 120,
            rows: 30,
        };
        Running(
            Session::spawn_in_group(&exe, &args, size, ProcessGroup::New).expect("the stub starts"),
        )
    }

    #[test]
    fn a_delivered_break_with_a_console_warning_is_waited_on_and_not_killed() {
        let mut running = idle_session();
        let wait = Duration::from_millis(300);
        let record = run_rung_with(&mut running.0, Rung::CtrlBreak, b"never", wait, |_| {
            Ok(Some("no parent console".to_owned()))
        });
        assert!(
            matches!(record.result, RungResult::NotStopped(_)),
            "{record:?}"
        );
        assert!(record.waited >= wait, "{record:?}");
        assert_eq!(record.warning.as_deref(), Some("no parent console"));
        assert_eq!(
            running
                .0
                .child()
                .wait_timeout(Duration::ZERO)
                .expect("queryable"),
            None,
            "the ladder did not end the child"
        );
        assert_eq!(running.0.job().active_processes().expect("readable"), 1);
    }

    #[test]
    fn a_break_that_was_not_delivered_is_a_send_failure_and_ends_no_wait() {
        let mut running = idle_session();
        let wait = Duration::from_secs(30);
        let record = run_rung_with(&mut running.0, Rung::CtrlBreak, b"never", wait, |_| {
            Err(io::Error::other("attach failed"))
        });
        assert_eq!(
            record.result,
            RungResult::SendFailed("attach failed".to_owned())
        );
        assert!(record.waited < wait, "{record:?}");
        assert_eq!(record.warning, None);
    }
}
