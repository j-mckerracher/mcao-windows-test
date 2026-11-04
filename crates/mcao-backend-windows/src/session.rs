//! A worker session: a child in a Job Object, on a ConPTY terminal, with its
//! output captured and text input.
//!
//! [`Session::spawn`] puts the child in a new job and attaches it to a new
//! pseudoconsole. A reader thread drains the terminal output into a bounded
//! buffer for the whole life of the session. The thread is required, not
//! optional: when nothing reads the output pipe, the console host stalls once the
//! pipe is full, and the child stalls with it. [`Session::capture`] and
//! [`Session::wait_for_output`] read that buffer and never touch the pipe.
//!
//! The buffer holds the raw terminal stream, which includes VT escape sequences.
//! It is not a rendered screen. Bytes past [`MAX_CAPTURE_BYTES`] are dropped from
//! the front and counted in [`Capture::discarded`].

use std::cell::Cell;
use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use mcao_core::terminal::normalize_capture;

use crate::conpty::{ConPty, ConsoleSize};
use crate::job::{JobLimits, JobObject};
use crate::process::{self, ChildProcess, ProcessGroup};
use crate::window::{BoundedWrite, bounded_write};

/// The most terminal output a session keeps. Older bytes are dropped first.
pub const MAX_CAPTURE_BYTES: usize = 1024 * 1024;

pub use crate::window::MIN_INPUT_WAIT;

/// How long dropping a session waits for the reader thread to finish.
const READER_SHUTDOWN_WAIT: Duration = Duration::from_secs(5);
/// How much terminal output the reader thread takes per read.
const READ_CHUNK_BYTES: usize = 4096;

/// The Ctrl+C character. Written to ConPTY input, the console turns it into `CTRL_C_EVENT`.
const CTRL_C: &str = "\u{3}";

/// A snapshot of the captured terminal output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capture {
    /// The retained output, oldest byte first.
    pub bytes: Vec<u8>,
    /// How many older bytes were dropped to stay within the buffer limit.
    pub discarded: u64,
    /// Whether the output stream has ended. No more bytes will arrive.
    pub closed: bool,
    /// The error that ended the stream, if it did not end cleanly.
    pub read_error: Option<String>,
}

impl Capture {
    /// Returns the retained output as normalized text: UTF-8 with LF line ends and no
    /// terminal control sequences. See [`normalize_capture`].
    pub fn text(&self) -> String {
        normalize_capture(&self.bytes)
    }
}

/// A child process on a ConPTY terminal, inside a Job Object.
///
/// Dropping the value closes the pseudoconsole and stops the reader thread. It
/// does not terminate the job, so a caller that wants the tree gone calls
/// `session.job().terminate(code)` first.
///
/// A worker does not outlive its orchestrator on Windows ConPTY. The pseudoconsole
/// belongs to the orchestrator process, and a process attached to it ends when the
/// pseudoconsole closes, which happens on `Drop` and when the orchestrator exits. The
/// Windows test `a_worker_reports_its_true_state_after_its_orchestrator_exits` asserts
/// that outcome. A caller that restarts after its orchestrator exits therefore finds
/// the worker stopped, and still reads the job and process probes for the real state
/// rather than assuming it.
#[derive(Debug)]
pub struct Session {
    job: JobObject,
    child: ChildProcess,
    input: File,
    console: Option<ConPty>,
    reader: OutputReader,
    group: ProcessGroup,
    /// Set once a terminal write was abandoned. The abandoned bytes may still be delivered, so
    /// any later write could interleave with them, and the session refuses further input.
    /// This `Cell` makes `Session` not `Sync`, which is a public auto-trait change.
    input_stalled: Cell<bool>,
}

impl Session {
    /// Starts `program` on a new terminal of `size`, inside a new unnamed job.
    ///
    /// The reader thread starts before the child, so output is never left
    /// unread, including the output of a console that fails to launch a child.
    /// The program and `args` follow the rules of [`process::spawn_in_job`].
    ///
    /// Production callers pass [`crate::conpty::negotiated_size`], which applies the
    /// DPES-886 AC8 rules: the `SIZE` environment variable, then the console window,
    /// then 120x40, never below 80x24. Tests pass a fixed size so their output does
    /// not depend on the runner.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` for a bad console size, program, or argument.
    /// Otherwise returns the OS error from creating the job, the pipes, the
    /// pseudoconsole, the reader thread, or the child.
    pub fn spawn(program: &Path, args: &[&str], size: ConsoleSize) -> io::Result<Self> {
        Self::spawn_in_group(program, args, size, ProcessGroup::Inherit)
    }

    /// Like [`Session::spawn`], but the job is named `name`, so a later process can
    /// reopen it with [`JobObject::open`] or [`crate::job::probe_named`]. `limits`,
    /// when given, is applied to the job before the child joins it.
    ///
    /// The caller chooses `name`, for example with
    /// `mcao_core::process_identity::job_object_name`. Recording the name against the
    /// task is the caller's step.
    ///
    /// # Errors
    ///
    /// Returns `AlreadyExists` when a job named `name` already exists, and
    /// `InvalidInput` for a name [`JobObject::named`] refuses. Returns the error of
    /// applying `limits`, and otherwise the errors of [`Session::spawn`].
    pub fn spawn_named(
        program: &Path,
        args: &[&str],
        size: ConsoleSize,
        name: &str,
        limits: Option<JobLimits>,
    ) -> io::Result<Self> {
        let job = JobObject::named(name)?;
        if let Some(limits) = limits {
            job.set_limits(limits)?;
        }
        Self::start_in_job(job, program, args, size, ProcessGroup::Inherit)
    }

    /// Like [`Session::spawn`], but starts the child in `group`.
    ///
    /// Use [`ProcessGroup::New`] for a session whose child must be able to receive
    /// `CTRL_BREAK_EVENT`. Ctrl+C does not reach such a child.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Session::spawn`].
    pub fn spawn_in_group(
        program: &Path,
        args: &[&str],
        size: ConsoleSize,
        group: ProcessGroup,
    ) -> io::Result<Self> {
        Self::start_in_job(JobObject::new()?, program, args, size, group)
    }

    /// Starts the terminal and child inside `job`, which the session then owns.
    fn start_in_job(
        job: JobObject,
        program: &Path,
        args: &[&str],
        size: ConsoleSize,
        group: ProcessGroup,
    ) -> io::Result<Self> {
        // Cleared before ConPty::open, which creates the pseudoconsole host from this process,
        // so the host is never created while this process ignores CTRL+C. Whether the host is
        // affected by that attribute is not observed on Windows.
        process::clear_inherited_ctrl_c_ignore();
        let (console, io) = ConPty::open(size)?;
        let mut reader = OutputReader::start(io.output)?;
        match process::spawn_in_job_with_console(&job, program, args, &console, group) {
            Ok(child) => Ok(Self {
                job,
                child,
                input: io.input,
                console: Some(console),
                reader,
                group,
                input_stalled: Cell::new(false),
            }),
            Err(error) => {
                // The console closes first, which ends the output stream and lets
                // the reader finish.
                drop(console);
                reader.shutdown();
                Err(error)
            }
        }
    }

    /// Returns the job that owns the child and its descendants.
    pub fn job(&self) -> &JobObject {
        &self.job
    }

    /// Returns the child process.
    pub fn child(&self) -> &ChildProcess {
        &self.child
    }

    /// Returns the process group the child was started in.
    pub fn group(&self) -> ProcessGroup {
        self.group
    }

    /// Returns a snapshot of the terminal output captured so far.
    ///
    /// # Errors
    ///
    /// Returns an error when the reader thread panicked and poisoned the buffer.
    pub fn capture(&self) -> io::Result<Capture> {
        let mut state = self.reader.shared.lock()?;
        Ok(Capture {
            bytes: state.output.make_contiguous().to_vec(),
            discarded: state.discarded,
            closed: state.closed,
            read_error: state.read_error.clone(),
        })
    }

    /// Waits up to `timeout` until the retained output contains `needle`.
    ///
    /// Returns `true` when it does, which includes an empty `needle`. Returns
    /// `false` when the wait expires, or when the stream ends without the bytes.
    ///
    /// # Errors
    ///
    /// Returns an error when the reader thread panicked and poisoned the buffer.
    pub fn wait_for_output(&self, needle: &[u8], timeout: Duration) -> io::Result<bool> {
        let shared = &self.reader.shared;
        let state = shared.lock()?;
        let (mut state, _) = shared
            .changed
            .wait_timeout_while(state, timeout, |state| {
                !state.contains(needle) && !state.closed
            })
            .map_err(|_| poisoned())?;
        Ok(state.contains(needle))
    }

    /// Returns how many output bytes the session has received so far, including
    /// bytes already dropped from the front of the buffer.
    ///
    /// Pass the value to [`Session::wait_for_output_after`] to look only at output
    /// that arrives later.
    ///
    /// # Errors
    ///
    /// Returns an error when the reader thread panicked and poisoned the buffer.
    pub fn output_mark(&self) -> io::Result<u64> {
        Ok(self.reader.shared.lock()?.received())
    }

    /// Waits up to `timeout` until output received after `mark` contains `needle`.
    ///
    /// `mark` comes from [`Session::output_mark`]. Output received before it never
    /// matches, so an old marker cannot pass for new evidence. Returns `true` when
    /// the later output contains `needle`, which includes an empty `needle`.
    /// Returns `false` when the wait expires, or when the stream ends without the
    /// bytes.
    ///
    /// # Errors
    ///
    /// Returns an error when the reader thread panicked and poisoned the buffer.
    pub fn wait_for_output_after(
        &self,
        mark: u64,
        needle: &[u8],
        timeout: Duration,
    ) -> io::Result<bool> {
        let shared = &self.reader.shared;
        let state = shared.lock()?;
        let (mut state, _) = shared
            .changed
            .wait_timeout_while(state, timeout, |state| {
                !state.contains_after(mark, needle) && !state.closed
            })
            .map_err(|_| poisoned())?;
        Ok(state.contains_after(mark, needle))
    }

    /// Writes `text` to the child as terminal input, as UTF-8.
    ///
    /// The terminal reads a carriage return (`"\r"`) as the Enter key, so a line
    /// is submitted by ending it with `"\r"`, not `"\n"`.
    ///
    /// The call blocks while the input pipe is full, which happens only when the
    /// child is not reading its input. It takes `&mut self` so that two writers
    /// cannot interleave one text. Once an earlier bounded write was abandoned, it
    /// refuses without writing; see [`Session::send_text_within`].
    ///
    /// # Errors
    ///
    /// Returns `Other` when an earlier write was abandoned. Returns the I/O error from
    /// the write, such as a broken pipe after the pseudoconsole closed.
    pub fn send_text(&mut self, text: &str) -> io::Result<()> {
        self.check_input_open()?;
        self.input.write_all(text.as_bytes())?;
        self.input.flush()
    }

    /// Writes `text` like [`Session::send_text`], but stops waiting after `wait`.
    ///
    /// The write runs on its own thread. Returns `Ok` when the terminal accepts all of
    /// `text` within `wait`. When it does not, the call returns `TimedOut` and stops
    /// waiting. The abandoned write is not cancelled: it stays on its thread and may still
    /// complete, so whether the input was delivered is unknown. This session then refuses
    /// all further input, because a later write could interleave with the abandoned bytes.
    /// A `wait` shorter than [`MIN_INPUT_WAIT`] is refused before any write, so a fast write is
    /// never reported as stalled by a wait too short to cover thread start. A `wait` at or above
    /// the minimum can still expire before a slow write completes; that is reported as
    /// `TimedOut` and stalls input.
    /// Whether releasing the session ends the abandoned write is not verified on Windows:
    /// the write holds a duplicated handle to the input pipe, so it may stay blocked after
    /// `Drop`.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` when `wait` is shorter than [`MIN_INPUT_WAIT`] or too long to
    /// reach a deadline, and nothing is written. Returns `TimedOut` when `wait` expires, and
    /// the I/O error of a write that fails before then. Returns `Other` without writing when
    /// an earlier write was abandoned. A write thread that cannot start is an error, and
    /// nothing is written.
    pub fn send_text_within(&mut self, text: &str, wait: Duration) -> io::Result<()> {
        if wait < MIN_INPUT_WAIT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("the input wait must be at least {MIN_INPUT_WAIT:?}"),
            ));
        }
        self.check_input_open()?;
        match bounded_write(self.input_handle()?, text.as_bytes().to_vec(), wait)? {
            BoundedWrite::Finished => Ok(()),
            BoundedWrite::Pending => {
                self.mark_input_stalled();
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "the terminal did not accept the input within {wait:?}; delivery is unknown and the write may still complete"
                    ),
                ))
            }
        }
    }

    /// Writes the Ctrl+C character, byte `0x03`, to the terminal input. The console turns
    /// it into `CTRL_C_EVENT` for the processes that accept it.
    ///
    /// # Errors
    ///
    /// Returns the OS error when the input pipe cannot be written.
    pub fn send_ctrl_c(&mut self) -> io::Result<()> {
        self.send_text(CTRL_C)
    }

    /// Returns a second handle to the terminal input pipe, for a write that the caller may
    /// stop waiting for. A write through it reaches the same pipe as [`Session::send_text`].
    ///
    /// # Errors
    ///
    /// Returns the OS error when the handle cannot be duplicated.
    pub(crate) fn input_handle(&self) -> io::Result<File> {
        self.input.try_clone()
    }

    /// Records that a terminal write was abandoned, so that later input is refused.
    pub(crate) fn mark_input_stalled(&self) {
        self.input_stalled.set(true);
    }

    /// Reports whether a terminal write was abandoned, so that input is refused.
    pub(crate) fn input_is_stalled(&self) -> bool {
        self.input_stalled.get()
    }

    /// Refuses input once an earlier write was abandoned, so that no write can
    /// interleave with the abandoned bytes.
    fn check_input_open(&self) -> io::Result<()> {
        if self.input_is_stalled() {
            return Err(io::Error::other(
                "the terminal input is stalled by an earlier write that was abandoned; no further input is sent",
            ));
        }
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Closing the console ends the output stream. The reader keeps reading
        // meanwhile, which older Windows needs for the close to return.
        drop(self.console.take());
        self.reader.shutdown();
    }
}

/// The reader thread and the buffer it fills.
#[derive(Debug)]
struct OutputReader {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl OutputReader {
    /// Starts a thread that drains `output` into a new buffer.
    fn start(output: File) -> io::Result<Self> {
        let shared = Arc::new(Shared::new(MAX_CAPTURE_BYTES));
        let thread_shared = Arc::clone(&shared);
        let thread = thread::Builder::new()
            .name("mcao-conpty-reader".to_owned())
            .spawn(move || read_until_closed(output, &thread_shared))?;
        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }

    /// Waits a bounded time for the thread to finish, then joins it.
    ///
    /// The thread ends when the output stream does, so the caller closes the
    /// pseudoconsole first. If the stream is still open after the wait, the
    /// thread is detached rather than joined. It owns everything it touches, so
    /// it ends safely whenever the stream does.
    fn shutdown(&mut self) {
        let Some(thread) = self.thread.take() else {
            return;
        };
        let finished = match self.shared.lock() {
            Ok(state) => self
                .shared
                .changed
                .wait_timeout_while(state, READER_SHUTDOWN_WAIT, |state| !state.closed)
                .is_ok_and(|(state, _)| state.closed),
            // A poisoned buffer means the thread already panicked and is done.
            Err(_) => true,
        };
        if finished {
            // The reader reports a panic through the poisoned buffer, which the
            // next capture returns as an error. The join result adds nothing.
            let _ = thread.join();
        }
    }
}

/// The buffer shared by the reader thread and the session.
#[derive(Debug)]
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

#[derive(Debug)]
struct State {
    output: VecDeque<u8>,
    limit: usize,
    discarded: u64,
    closed: bool,
    read_error: Option<String>,
}

impl State {
    fn contains(&mut self, needle: &[u8]) -> bool {
        self.contains_after(0, needle)
    }

    /// Total bytes received, including those dropped from the front.
    fn received(&self) -> u64 {
        self.discarded + u64::try_from(self.output.len()).expect("a buffer length fits in u64")
    }

    /// Reports whether the bytes received after `mark` contain `needle`.
    fn contains_after(&mut self, mark: u64, needle: &[u8]) -> bool {
        // A mark older than the retained bytes starts at the oldest retained byte.
        let skip = usize::try_from(mark.saturating_sub(self.discarded)).unwrap_or(usize::MAX);
        let bytes = self.output.make_contiguous();
        let later = bytes.get(skip..).unwrap_or_default();
        needle.is_empty() || later.windows(needle.len()).any(|window| window == needle)
    }
}

impl Shared {
    fn new(limit: usize) -> Self {
        Self {
            state: Mutex::new(State {
                output: VecDeque::new(),
                limit,
                discarded: 0,
                closed: false,
                read_error: None,
            }),
            changed: Condvar::new(),
        }
    }

    /// Locks the buffer. A poisoned lock is an error: its contents are never read.
    fn lock(&self) -> io::Result<MutexGuard<'_, State>> {
        self.state.lock().map_err(|_| poisoned())
    }

    /// Appends `bytes`, dropping the oldest bytes past the limit.
    fn append(&self, bytes: &[u8]) -> io::Result<()> {
        let mut state = self.lock()?;
        state.output.extend(bytes);
        let excess = state.output.len().saturating_sub(state.limit);
        if excess > 0 {
            state.output.drain(..excess);
            state.discarded += u64::try_from(excess).expect("a buffer length fits in u64");
        }
        drop(state);
        self.changed.notify_all();
        Ok(())
    }

    /// Marks the stream as ended, with the error that ended it, if any.
    fn close(&self, read_error: Option<String>) {
        // A poisoned buffer cannot be updated safely. The session sees the
        // poison on its next lock, so there is nothing more to record here.
        if let Ok(mut state) = self.lock() {
            state.closed = true;
            state.read_error = read_error;
        }
        self.changed.notify_all();
    }
}

fn poisoned() -> io::Error {
    io::Error::other("the terminal output buffer was poisoned by a panicked reader")
}

/// Reads `output` to its end, then marks the buffer closed.
fn read_until_closed(mut output: File, shared: &Shared) {
    let mut chunk = [0_u8; READ_CHUNK_BYTES];
    loop {
        match output.read(&mut chunk) {
            Ok(0) => return shared.close(None),
            Ok(count) => {
                if shared.append(&chunk[..count]).is_err() {
                    return;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return shared.close(Some(error.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(shared: &Shared) -> (Vec<u8>, u64) {
        let mut state = shared.lock().expect("the buffer is not poisoned");
        (state.output.make_contiguous().to_vec(), state.discarded)
    }

    #[test]
    fn append_keeps_everything_under_the_limit() {
        let shared = Shared::new(8);
        shared.append(b"abc").expect("append succeeds");
        shared.append(b"de").expect("append succeeds");
        assert_eq!(snapshot(&shared), (b"abcde".to_vec(), 0));
    }

    #[test]
    fn append_drops_the_oldest_bytes_past_the_limit() {
        let shared = Shared::new(4);
        shared.append(b"abc").expect("append succeeds");
        shared.append(b"defg").expect("append succeeds");
        assert_eq!(snapshot(&shared), (b"defg".to_vec(), 3));
    }

    #[test]
    fn one_append_larger_than_the_limit_keeps_its_tail() {
        let shared = Shared::new(3);
        shared.append(b"abcdefgh").expect("append succeeds");
        assert_eq!(snapshot(&shared), (b"fgh".to_vec(), 5));
    }

    #[test]
    fn contains_finds_a_needle_across_two_appends() {
        let shared = Shared::new(16);
        shared.append(b"ab").expect("append succeeds");
        shared.append(b"cd").expect("append succeeds");
        let mut state = shared.lock().expect("the buffer is not poisoned");
        assert!(state.contains(b"bc"));
        assert!(state.contains(b""));
        assert!(!state.contains(b"ca"));
    }

    #[test]
    fn contains_after_ignores_bytes_before_the_mark() {
        let shared = Shared::new(16);
        shared.append(b"stop").expect("append succeeds");
        let mut state = shared.lock().expect("the buffer is not poisoned");
        assert_eq!(state.received(), 4);
        assert!(!state.contains_after(4, b"stop"));
        state.output.extend(b"go stop");
        assert!(state.contains_after(4, b"stop"));
        assert!(!state.contains_after(11, b"stop"));
    }

    #[test]
    fn contains_after_counts_discarded_bytes_in_the_mark() {
        let shared = Shared::new(4);
        shared.append(b"abcdef").expect("append succeeds");
        let mut state = shared.lock().expect("the buffer is not poisoned");
        assert_eq!(state.received(), 6);
        assert!(state.contains_after(0, b"cdef"));
        assert!(!state.contains_after(5, b"ef"));
        assert!(state.contains_after(5, b"f"));
    }

    #[test]
    fn close_records_the_ending_error() {
        let shared = Shared::new(4);
        shared.close(Some("boom".to_owned()));
        let state = shared.lock().expect("the buffer is not poisoned");
        assert!(state.closed);
        assert_eq!(state.read_error.as_deref(), Some("boom"));
    }

    #[test]
    fn a_poisoned_buffer_is_an_error_and_is_never_read() {
        let shared = Arc::new(Shared::new(4));
        let panicking = Arc::clone(&shared);
        let result = thread::spawn(move || {
            let _guard = panicking.state.lock().expect("first lock succeeds");
            panic!("poison the buffer");
        })
        .join();
        assert!(result.is_err());
        assert_eq!(
            shared.lock().expect_err("the lock is poisoned").kind(),
            io::ErrorKind::Other
        );
        assert!(shared.append(b"x").is_err());
    }
}
