//! Serves one read-only console session over a connected client (DPES-883).
//!
//! [`serve_session`] is the transport loop. A reader thread decodes client frames, and a writer
//! thread writes the frames the loop queues. The loop owns the [`ReadOnlySession`] and never
//! blocks on the client. It queues each OUTPUT frame as soon as the session returns it, so the
//! bytes queued but not yet written are the client's backlog. The session's
//! [`BacklogTracker`](mcao_core::peek::BacklogTracker) caps that backlog at 1 MiB. A client that
//! stops reading therefore reaches the cap and is closed, while the ring keeps moving. The
//! session is generic over `Read + InputReady` and `Write`, so host tests drive it with in-memory
//! streams. On Windows, [`serve_connection`] verifies the client and runs the loop on its pipe.
//!
//! The pipe is synchronous, so a write that is stalled in the kernel holds the pipe handle, and
//! a readiness check on the same pipe may wait behind it. The reader can then stop draining input,
//! so the backlog cap is never reached. The loop therefore also closes a session whose single write
//! stays blocked for [`WRITE_STALL_LIMIT`]. When the loop closes the session, it
//! stops both workers: the reader checks a stop flag before each read, and the writer checks a
//! cancel flag before each write. Interrupts cancel synchronous I/O already in progress (on
//! Windows, `CancelSynchronousIo`). Cleanup reports API failures and never treats a worker that
//! misses its bounded exit deadline as stopped. The ring is shared with the worker's output thread,
//! so the lock is held only for each session call.

use std::io::{self, ErrorKind, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use mcao_core::console::frame::{Frame, FrameError, decode_from, encode};
use mcao_core::console::ring::RingBuffer;
use mcao_core::console::session::{ReadOnlySession, queued_len};

#[cfg(windows)]
use mcao_core::console::frame::error_frame;

#[cfg(windows)]
use std::fs::File;
#[cfg(windows)]
use std::os::windows::io::AsHandle;
#[cfg(windows)]
use std::ptr;

#[cfg(windows)]
use windows_sys::Win32::Foundation::{
    CloseHandle, DuplicateHandle, ERROR_NOT_FOUND, FALSE, HANDLE,
};
#[cfg(windows)]
use windows_sys::Win32::System::IO::CancelSynchronousIo;
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThread, THREAD_TERMINATE,
};

#[cfg(windows)]
use crate::pipe::{self, PipeClient, PipeError};

/// How long a thread sleeps when it has no work.
const IDLE_POLL: Duration = Duration::from_millis(10);

/// How long a closing session waits, in total, for queued frames to be written before it stops
/// the writer.
const CLOSE_FLUSH: Duration = Duration::from_secs(1);

/// How long a closing session keeps interrupting workers that have not exited yet.
const CANCEL_GRACE: Duration = Duration::from_secs(1);

/// How long a single write may stay blocked in the kernel before the session closes the client.
/// The reader's readiness check shares the pipe's synchronous I/O, so it may wait behind a stalled
/// write and stop draining input. The backlog cap is then never reached, so the cap alone does not
/// bound a client that stops reading.
const WRITE_STALL_LIMIT: Duration = Duration::from_secs(10);

/// When the writer's current blocking write began, shared with the session loop.
type WriteStart = Arc<Mutex<Option<Instant>>>;

/// Reports whether a read on the stream would return client bytes without blocking.
pub trait InputReady {
    /// Returns `Ok(true)` when the client has bytes ready to read.
    fn input_ready(&mut self) -> io::Result<bool>;
}

/// Stops the writer thread's blocked write when the session closes.
///
/// [`attach`](Self::attach) runs on the writer thread before its first write and
/// [`detach`](Self::detach) runs on it after its last. [`interrupt`](Self::interrupt) runs on the
/// session thread and must cancel a write the writer thread has in progress. The session calls it
/// repeatedly until the writer exits, because a write that starts just after one call would
/// otherwise be missed.
pub trait WriteInterrupt: Send + Sync + 'static {
    /// Binds the interrupt to the calling writer thread.
    fn attach(&self) {}
    /// Releases what [`attach`](Self::attach) bound.
    fn detach(&self) {}
    /// Cancels the write the writer thread is blocked in, if any.
    fn interrupt(&self) {}
    /// Binds the interrupt and reports failures.
    fn try_attach(&self) -> io::Result<()> {
        self.attach();
        Ok(())
    }
    /// Releases the interrupt binding and reports failures.
    fn try_detach(&self) -> io::Result<()> {
        self.detach();
        Ok(())
    }
    /// Cancels the blocked operation and reports failures.
    fn try_interrupt(&self) -> io::Result<()> {
        self.interrupt();
        Ok(())
    }
}

/// An interrupt that does nothing. An operation that never returns is reported as unfinished
/// cleanup after [`CANCEL_GRACE`], and its worker thread outlives the session.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoInterrupt;

impl WriteInterrupt for NoInterrupt {}

/// State shared between the session loop and the writer thread.
struct WriterControl<I> {
    cancelled: AtomicBool,
    interrupt: I,
    write_started: WriteStart,
}

struct WorkerCleanup<RI, WI> {
    reader_stop: Arc<AtomicBool>,
    writer_stop: Arc<AtomicBool>,
    reader_interrupt: Arc<RI>,
    control: Arc<WriterControl<WI>>,
    reader_done_rx: Receiver<io::Result<()>>,
    writer_done_rx: Receiver<io::Result<()>>,
    reader_thread: Option<thread::JoinHandle<()>>,
    writer_thread: Option<thread::JoinHandle<()>>,
}

enum ReaderEvent {
    Frame(Result<Option<Frame>, FrameError>),
    ReadFailure(io::Error),
}

enum WriterEvent {
    Frame {
        result: io::Result<usize>,
        interrupted: bool,
    },
    StartFailure(io::Error),
}

/// Serves one session on the read half `reader` and the write half `writer` until it closes.
///
/// Returns `Ok(())` when the session closes cleanly: after DETACH, after the client's EOF or a
/// truncated frame, and after a decode error that the session answers with an ERROR frame. Queued
/// frames are flushed first, bounded in total by [`CLOSE_FLUSH`]. Returns `Err` when a read fails
/// with an error other than EOF, when a write fails, when the client's backlog passes the limit,
/// or when a write stays blocked for [`WRITE_STALL_LIMIT`].
/// The backlog counts every queued OUTPUT and ERROR frame, so a client that stops reading is closed
/// even when it is flooded with refused INPUT. The session then queues an ERROR "backlog exceeded"
/// frame, which a client that has stopped reading never receives. The caller's connection is
/// released once both workers exit, within [`CLOSE_FLUSH`] plus [`CANCEL_GRACE`]. If either worker
/// does not exit, the returned error reports that cleanup is incomplete. The halves are owned here;
/// the caller owns the underlying connection.
pub fn serve_session<R, W>(reader: R, writer: W, ring: &Mutex<RingBuffer>) -> io::Result<()>
where
    R: Read + InputReady + Send + 'static,
    W: Write + Send + 'static,
{
    serve_session_with_interrupt(reader, writer, ring, NoInterrupt)
}

/// Like [`serve_session`], with `interrupt` used to cancel a write blocked in the kernel.
pub fn serve_session_with_interrupt<R, W, I>(
    reader: R,
    writer: W,
    ring: &Mutex<RingBuffer>,
    interrupt: I,
) -> io::Result<()>
where
    R: Read + InputReady + Send + 'static,
    W: Write + Send + 'static,
    I: WriteInterrupt,
{
    serve_session_with_interrupts(
        reader,
        writer,
        ring,
        NoInterrupt,
        interrupt,
        WRITE_STALL_LIMIT,
    )
}

fn serve_session_with_interrupts<R, W, RI, WI>(
    reader: R,
    writer: W,
    ring: &Mutex<RingBuffer>,
    reader_interrupt: RI,
    writer_interrupt: WI,
    stall_limit: Duration,
) -> io::Result<()>
where
    R: Read + InputReady + Send + 'static,
    W: Write + Send + 'static,
    RI: WriteInterrupt,
    WI: WriteInterrupt,
{
    let reader_stop = Arc::new(AtomicBool::new(false));
    let writer_stop = Arc::new(AtomicBool::new(false));
    let write_started: WriteStart = Arc::new(Mutex::new(None));
    let control = Arc::new(WriterControl {
        cancelled: AtomicBool::new(false),
        interrupt: writer_interrupt,
        write_started: Arc::clone(&write_started),
    });
    let reader_interrupt = Arc::new(reader_interrupt);
    let (client_tx, client_rx) = mpsc::channel();
    let (out_tx, out_rx) = mpsc::channel();
    let (writer_events_tx, writer_events_rx) = mpsc::channel();
    let (reader_done_tx, reader_done_rx) = mpsc::channel();
    let (writer_done_tx, writer_done_rx) = mpsc::channel();
    let reader_stop_thread = Arc::clone(&reader_stop);
    let writer_stop_thread = Arc::clone(&writer_stop);
    let reader_interrupt_thread = Arc::clone(&reader_interrupt);
    let writer_control = Arc::clone(&control);
    let reader_thread = thread::spawn(move || {
        let result = read_client(
            reader,
            client_tx,
            reader_stop_thread,
            reader_interrupt_thread,
        );
        let _ = reader_done_tx.send(result);
    });
    let writer_thread = thread::spawn(move || {
        let result = write_frames(
            writer,
            out_rx,
            writer_events_tx,
            writer_stop_thread,
            writer_control,
        );
        let _ = writer_done_tx.send(result);
    });

    let (result, pending) = drive(
        &client_rx,
        &out_tx,
        &writer_events_rx,
        &writer_stop,
        ring,
        &write_started,
        stall_limit,
    );
    let workers = WorkerCleanup {
        reader_stop,
        writer_stop,
        reader_interrupt,
        control,
        reader_done_rx,
        writer_done_rx,
        reader_thread: Some(reader_thread),
        writer_thread: Some(writer_thread),
    };
    let cleanup = finish(out_tx, writer_events_rx, pending, workers);
    finish_result(result, cleanup)
}

/// Feeds client frames to the session and queues the frames it returns.
fn drive(
    client_rx: &Receiver<ReaderEvent>,
    out_tx: &Sender<Frame>,
    writer_events_rx: &Receiver<WriterEvent>,
    writer_stop: &AtomicBool,
    ring: &Mutex<RingBuffer>,
    write_started: &WriteStart,
    stall_limit: Duration,
) -> (io::Result<()>, usize) {
    let mut session = ReadOnlySession::new();
    let mut pending = 0_usize;
    loop {
        if write_stalled(write_started, stall_limit) {
            return (overflow_close(writer_stop), pending);
        }
        let mut busy = false;

        match client_rx.try_recv() {
            Ok(item) => {
                busy = true;
                let frames = match item {
                    ReaderEvent::Frame(Ok(Some(frame))) => session.handle_frame(frame, &lock(ring)),
                    ReaderEvent::Frame(Ok(None)) => {
                        session.peer_closed();
                        Vec::new()
                    }
                    ReaderEvent::Frame(Err(FrameError::Io(kind))) => {
                        return (Err(io::Error::from(kind)), pending);
                    }
                    ReaderEvent::Frame(Err(err)) => session.handle_decode_error(err),
                    ReaderEvent::ReadFailure(err) => return (Err(err), pending),
                };
                for frame in frames {
                    if let Err(err) = send(out_tx, frame, &mut pending) {
                        return (Err(err), pending);
                    }
                }
                if session.backlog_exceeded() {
                    return (overflow_close(writer_stop), pending);
                }
            }
            Err(TryRecvError::Disconnected) => {
                return (
                    Err(io::Error::other("client reader stopped unexpectedly")),
                    pending,
                );
            }
            Err(TryRecvError::Empty) => {}
        }

        loop {
            let event = match writer_events_rx.try_recv() {
                Ok(event) => event,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    return (
                        Err(io::Error::other("writer stopped unexpectedly")),
                        pending,
                    );
                }
            };
            busy = true;
            match event {
                WriterEvent::Frame { result, .. } => {
                    pending -= 1;
                    match result {
                        Ok(bytes) => session.drained(bytes),
                        Err(err) => return (Err(err), pending),
                    }
                }
                WriterEvent::StartFailure(err) => return (Err(err), pending),
            }
        }

        while let Some(frame) = {
            let ring = lock(ring);
            session.poll_output(&ring)
        } {
            busy = true;
            if let Err(err) = send(out_tx, frame, &mut pending) {
                return (Err(err), pending);
            }
            if session.backlog_exceeded() {
                return (overflow_close(writer_stop), pending);
            }
        }

        if session.is_closed() {
            return (Ok(()), pending);
        }
        if !busy {
            thread::sleep(IDLE_POLL);
        }
    }
}

fn write_stalled(write_started: &WriteStart, limit: Duration) -> bool {
    lock_started(write_started).is_some_and(|since| since.elapsed() >= limit)
}

fn lock_started(write_started: &WriteStart) -> MutexGuard<'_, Option<Instant>> {
    write_started.lock().unwrap_or_else(PoisonError::into_inner)
}

fn overflow_close(writer_stop: &AtomicBool) -> io::Result<()> {
    writer_stop.store(true, Ordering::Relaxed);
    Err(io::Error::other("client backlog exceeded"))
}

/// Reads client frames until EOF, a decode error, a read failure, or `stop`.
///
/// `input_ready` reports only; a read happens only after it reports bytes, so the reader thread
/// never spins on the handle.
fn read_client<R: Read + InputReady, I: WriteInterrupt>(
    mut reader: R,
    events: Sender<ReaderEvent>,
    stop: Arc<AtomicBool>,
    interrupt: Arc<I>,
) -> io::Result<()> {
    if let Err(err) = interrupt.try_attach() {
        let _ = events.send(ReaderEvent::ReadFailure(err));
        return interrupt.try_detach();
    }

    while !stop.load(Ordering::Relaxed) {
        match reader.input_ready() {
            Ok(true) => {
                // On a named pipe a closed client is expected to report ERROR_BROKEN_PIPE, not
                // EOF; this is unverified natively. Treat it as a truncated frame so the session
                // closes cleanly.
                let item = match decode_from(&mut reader) {
                    Err(FrameError::Io(ErrorKind::BrokenPipe)) => Err(FrameError::Truncated),
                    result => result,
                };
                let last = !matches!(item, Ok(Some(_)));
                if events.send(ReaderEvent::Frame(item)).is_err() || last {
                    break;
                }
            }
            Ok(false) => thread::sleep(IDLE_POLL),
            Err(err) => {
                let _ = events.send(ReaderEvent::ReadFailure(err));
                break;
            }
        }
    }

    interrupt.try_detach()
}

/// Writes queued frames in order and reports each one's [`queued_len`], written or skipped.
///
/// After `stop` is set, OUTPUT frames are skipped, so a session that has closed sends nothing
/// stale. ERROR frames are still written. A write failure is reported and ends the thread. Once
/// `control` is cancelled the thread writes nothing more, and it drops its handle before it exits.
fn write_frames<W: Write, I: WriteInterrupt>(
    mut out: W,
    frames: Receiver<Frame>,
    events: Sender<WriterEvent>,
    stop: Arc<AtomicBool>,
    control: Arc<WriterControl<I>>,
) -> io::Result<()> {
    if let Err(err) = control.interrupt.try_attach() {
        let _ = events.send(WriterEvent::StartFailure(err));
    } else {
        write_queue(&mut out, &frames, &events, &stop, &control);
    }
    let detach_result = control.interrupt.try_detach();
    drop(out);
    drop(events);
    detach_result
}

fn write_queue<W: Write, I: WriteInterrupt>(
    out: &mut W,
    frames: &Receiver<Frame>,
    events: &Sender<WriterEvent>,
    stop: &AtomicBool,
    control: &WriterControl<I>,
) {
    for frame in frames {
        if control.cancelled.load(Ordering::Relaxed) {
            return;
        }
        let len = queued_len(&frame);
        let skip = stop.load(Ordering::Relaxed) && !matches!(frame, Frame::Error(_));
        let result = if skip {
            Ok(len)
        } else {
            *lock_started(&control.write_started) = Some(Instant::now());
            let written = write_frame(out, &frame).map(|()| len);
            *lock_started(&control.write_started) = None;
            written
        };
        let failed = result.is_err();
        let interrupted = control.cancelled.load(Ordering::Relaxed);
        if events
            .send(WriterEvent::Frame {
                result,
                interrupted,
            })
            .is_err()
            || failed
        {
            return;
        }
    }
}

fn send(out: &Sender<Frame>, frame: Frame, pending: &mut usize) -> io::Result<()> {
    out.send(frame)
        .map_err(|_| io::Error::from(ErrorKind::BrokenPipe))?;
    *pending += 1;
    Ok(())
}

/// Drops the queue, flushes pending frames for a bounded time, then cancels and joins workers.
fn finish<RI: WriteInterrupt, WI: WriteInterrupt>(
    out_tx: Sender<Frame>,
    writer_events_rx: Receiver<WriterEvent>,
    mut pending: usize,
    mut workers: WorkerCleanup<RI, WI>,
) -> Vec<CleanupIssue> {
    let mut cleanup = Vec::new();
    workers.reader_stop.store(true, Ordering::Relaxed);
    let mut reader_cancel_not_found = None;
    let mut writer_cancel_not_found = None;
    let mut reader_cancel_error_reported = false;
    let mut writer_cancel_error_reported = false;
    if let Err(err) = workers.reader_interrupt.try_interrupt() {
        if is_no_pending_io(&err) {
            reader_cancel_not_found = Some(err);
        } else {
            cleanup.push(("reader interrupt failed", err));
            reader_cancel_error_reported = true;
        }
    }
    drop(out_tx);
    let flush_deadline = Instant::now() + CLOSE_FLUSH;
    while pending > 0 {
        let remaining = flush_deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match writer_events_rx.recv_timeout(remaining) {
            Ok(WriterEvent::Frame {
                result,
                interrupted,
            }) => {
                pending = pending.saturating_sub(1);
                if let Err(err) = result
                    && !interrupted
                {
                    cleanup.push(("writer failed while flushing", err));
                }
            }
            Ok(WriterEvent::StartFailure(err)) => {
                cleanup.push(("writer thread attachment failed", err));
            }
            Err(RecvTimeoutError::Disconnected | RecvTimeoutError::Timeout) => break,
        }
    }

    workers.writer_stop.store(true, Ordering::Relaxed);
    workers.control.cancelled.store(true, Ordering::Relaxed);
    let cancel_deadline = Instant::now() + CANCEL_GRACE;
    let mut reader_finished = false;
    let mut writer_finished = false;

    loop {
        if !reader_finished {
            reader_finished = collect_worker_exit(
                &workers.reader_done_rx,
                &mut workers.reader_thread,
                "reader cleanup failed",
                &mut cleanup,
            );
        }
        if !writer_finished {
            writer_finished = collect_worker_exit(
                &workers.writer_done_rx,
                &mut workers.writer_thread,
                "writer cleanup failed",
                &mut cleanup,
            );
        }
        if reader_finished && writer_finished {
            break;
        }

        if Instant::now() >= cancel_deadline {
            if !reader_finished {
                if let Some(err) = reader_cancel_not_found.take() {
                    cleanup.push(("reader cancellation found no pending I/O", err));
                }
                cleanup.push((
                    "reader worker did not exit before the cancellation deadline",
                    io::Error::new(ErrorKind::TimedOut, "reader may still own the connection"),
                ));
            }
            if !writer_finished {
                if let Some(err) = writer_cancel_not_found.take() {
                    cleanup.push(("writer cancellation found no pending I/O", err));
                }
                cleanup.push((
                    "writer worker did not exit before the cancellation deadline",
                    io::Error::new(ErrorKind::TimedOut, "writer may still own the connection"),
                ));
            }
            break;
        }

        if !reader_finished && let Err(err) = workers.reader_interrupt.try_interrupt() {
            if is_no_pending_io(&err) {
                reader_cancel_not_found.get_or_insert(err);
            } else if !reader_cancel_error_reported {
                cleanup.push(("reader interrupt failed", err));
                reader_cancel_error_reported = true;
            }
        }
        if !writer_finished && let Err(err) = workers.control.interrupt.try_interrupt() {
            if is_no_pending_io(&err) {
                writer_cancel_not_found.get_or_insert(err);
            } else if !writer_cancel_error_reported {
                cleanup.push(("writer interrupt failed", err));
                writer_cancel_error_reported = true;
            }
        }
        thread::sleep(IDLE_POLL.min(cancel_deadline.saturating_duration_since(Instant::now())));
    }

    loop {
        match writer_events_rx.try_recv() {
            Ok(WriterEvent::Frame {
                result,
                interrupted,
            }) => {
                pending = pending.saturating_sub(1);
                if let Err(err) = result
                    && !interrupted
                {
                    cleanup.push(("writer failed while flushing", err));
                }
            }
            Ok(WriterEvent::StartFailure(err)) => {
                cleanup.push(("writer thread attachment failed", err));
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
        }
    }
    cleanup
}

fn collect_worker_exit(
    done_rx: &Receiver<io::Result<()>>,
    worker: &mut Option<thread::JoinHandle<()>>,
    operation: &'static str,
    cleanup: &mut Vec<CleanupIssue>,
) -> bool {
    match done_rx.try_recv() {
        Ok(Ok(())) => {}
        Ok(Err(err)) => cleanup.push((operation, err)),
        Err(TryRecvError::Empty) => return false,
        Err(TryRecvError::Disconnected) => {}
    }

    if let Some(worker) = worker.take()
        && worker.join().is_err()
    {
        cleanup.push((operation, io::Error::other("worker thread panicked")));
    }
    true
}

#[cfg(windows)]
fn is_no_pending_io(err: &io::Error) -> bool {
    err.raw_os_error() == Some(ERROR_NOT_FOUND as i32)
}

#[cfg(not(windows))]
fn is_no_pending_io(_err: &io::Error) -> bool {
    false
}

type CleanupIssue = (&'static str, io::Error);

#[derive(Debug)]
struct SessionFailure {
    primary: Option<io::Error>,
    cleanup: Vec<CleanupIssue>,
}

impl std::fmt::Display for SessionFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(primary) = &self.primary {
            write!(f, "{primary}")?;
        } else {
            f.write_str("session cleanup failed")?;
        }
        for (operation, error) in &self.cleanup {
            write!(f, "; {operation}: {error}")?;
        }
        Ok(())
    }
}

impl std::error::Error for SessionFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.primary
            .as_ref()
            .map(|error| error as &(dyn std::error::Error + 'static))
            .or_else(|| {
                self.cleanup
                    .first()
                    .map(|(_, error)| error as &(dyn std::error::Error + 'static))
            })
    }
}

fn finish_result(result: io::Result<()>, cleanup: Vec<CleanupIssue>) -> io::Result<()> {
    if cleanup.is_empty() {
        return result;
    }
    let kind = result
        .as_ref()
        .err()
        .map(io::Error::kind)
        .or_else(|| cleanup.first().map(|(_, error)| error.kind()))
        .unwrap_or(ErrorKind::Other);
    Err(io::Error::new(
        kind,
        SessionFailure {
            primary: result.err(),
            cleanup,
        },
    ))
}

fn lock(ring: &Mutex<RingBuffer>) -> MutexGuard<'_, RingBuffer> {
    // A poisoned lock still holds valid ring bytes, so keep serving instead of panicking.
    ring.lock().unwrap_or_else(PoisonError::into_inner)
}

fn write_frame<W: Write>(out: &mut W, frame: &Frame) -> io::Result<()> {
    let bytes = encode(frame).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    out.write_all(&bytes)?;
    out.flush()
}

#[cfg(windows)]
impl InputReady for File {
    fn input_ready(&mut self) -> io::Result<bool> {
        match pipe::bytes_available(self.as_handle()) {
            Ok(available) => Ok(available > 0),
            // A client that closed its end is expected to make PeekNamedPipe fail with a broken
            // pipe (unverified natively). Report it as ready, so the next read runs and read_client
            // treats the broken pipe as a truncated frame; the session then closes cleanly.
            Err(err) if err.kind() == ErrorKind::BrokenPipe => Ok(true),
            Err(err) => Err(err),
        }
    }
}

/// Cancels a worker thread's blocked synchronous I/O with `CancelSynchronousIo`.
///
/// Holds a duplicate of the worker thread's handle between attachment and detachment, and closes
/// it on detach, so it does not outlive the thread.
#[cfg(windows)]
#[derive(Default)]
pub struct ThreadInterrupt(Mutex<Option<usize>>);

#[cfg(windows)]
impl WriteInterrupt for ThreadInterrupt {
    fn attach(&self) {
        let _ = self.try_attach();
    }

    fn try_attach(&self) -> io::Result<()> {
        let mut slot = lock_slot(&self.0);
        if slot.is_some() {
            return Err(io::Error::new(
                ErrorKind::AlreadyExists,
                "thread interrupt is already attached",
            ));
        }
        let mut handle: HANDLE = ptr::null_mut();
        // SAFETY: the pseudo-handle from GetCurrentThread is valid on this thread. DuplicateHandle
        // writes a real handle to `handle`, which `try_detach` closes.
        let duplicated = unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                GetCurrentThread(),
                GetCurrentProcess(),
                &mut handle,
                THREAD_TERMINATE,
                FALSE,
                0,
            )
        };
        if duplicated == 0 {
            return Err(io::Error::last_os_error());
        }
        *slot = Some(handle as usize);
        Ok(())
    }

    fn detach(&self) {
        let _ = self.try_detach();
    }

    fn try_detach(&self) -> io::Result<()> {
        let mut slot = lock_slot(&self.0);
        let Some(handle) = *slot else {
            return Ok(());
        };
        // SAFETY: this handle was duplicated in `try_attach` and remains in the slot on failure.
        if unsafe { CloseHandle(handle as HANDLE) } == 0 {
            return Err(io::Error::last_os_error());
        }
        *slot = None;
        Ok(())
    }

    fn interrupt(&self) {
        let _ = self.try_interrupt();
    }

    fn try_interrupt(&self) -> io::Result<()> {
        let slot = lock_slot(&self.0);
        if let Some(handle) = *slot {
            // SAFETY: the handle is held under the lock, so `try_detach` cannot close it during
            // the call.
            if unsafe { CancelSynchronousIo(handle as HANDLE) } == 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }
}

#[cfg(windows)]
fn lock_slot(slot: &Mutex<Option<usize>>) -> MutexGuard<'_, Option<usize>> {
    slot.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(windows)]
impl Drop for ThreadInterrupt {
    fn drop(&mut self) {
        let slot = self.0.get_mut().unwrap_or_else(PoisonError::into_inner);
        if let Some(handle) = slot.take() {
            // SAFETY: this is the duplicated handle retained after a failed detach.
            unsafe { CloseHandle(handle as HANDLE) };
        }
    }
}

/// Verifies that `client` runs as the current user, then serves the console on its pipe.
///
/// A client that fails verification is refused and closed when dropped, before any frame is read
/// from it. A client with the wrong SID first gets a best-effort ERROR "access denied". That
/// frame is written before the client sends any bytes, to an outbound buffer the pipe requests at
/// `PIPE_BUFFER_BYTES` (64 KiB). Windows treats that size as a hint, so the write is not expected
/// to block; this is unverified natively. No native test covers the refusal: it needs a client
/// running as a different user.
#[cfg(windows)]
pub fn serve_connection(client: PipeClient, ring: &Mutex<RingBuffer>) -> io::Result<()> {
    let verified = pipe::verify_client_acl(client.as_handle());
    let mut file = File::from(client);
    match verified {
        Ok(()) => {}
        Err(PipeError::AccessDenied) => {
            let _ = write_frame(&mut file, &error_frame("access denied"));
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                PipeError::AccessDenied,
            ));
        }
        Err(err @ PipeError::Os { .. }) => return Err(io::Error::other(err)),
    }
    let writer = file.try_clone()?;
    serve_session_with_interrupts(
        file,
        writer,
        ring,
        ThreadInterrupt::default(),
        ThreadInterrupt::default(),
        WRITE_STALL_LIMIT,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, ErrorKind};
    use std::time::Instant;

    use mcao_core::console::ring::DEFAULT_CAPACITY;

    /// Client bytes for the reader half. `hold_open` keeps the stream silent after its input
    /// is consumed, instead of reporting EOF.
    struct MemoryReader {
        input: Cursor<Vec<u8>>,
        hold_open: bool,
        fail_reads: bool,
    }

    impl MemoryReader {
        fn new(bytes: Vec<u8>) -> Self {
            Self {
                input: Cursor::new(bytes),
                hold_open: false,
                fail_reads: false,
            }
        }

        fn from_frames(frames: &[Frame]) -> Self {
            Self::new(
                frames
                    .iter()
                    .flat_map(|f| encode(f).expect("encodes"))
                    .collect(),
            )
        }
    }

    impl Read for MemoryReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.fail_reads && self.input.position() as usize >= self.input.get_ref().len() {
                return Err(io::Error::from(ErrorKind::ConnectionReset));
            }
            self.input.read(buf)
        }
    }

    impl InputReady for MemoryReader {
        fn input_ready(&mut self) -> io::Result<bool> {
            let unread = (self.input.position() as usize) < self.input.get_ref().len();
            Ok(!self.hold_open || unread)
        }
    }

    struct PartialBrokenPipeReader(Cursor<Vec<u8>>);

    impl Read for PartialBrokenPipeReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.0.position() as usize >= self.0.get_ref().len() {
                return Err(io::Error::from(ErrorKind::BrokenPipe));
            }
            self.0.read(buf)
        }
    }

    impl InputReady for PartialBrokenPipeReader {
        fn input_ready(&mut self) -> io::Result<bool> {
            Ok(true)
        }
    }

    /// Server output collected in memory. Clones share the same buffer.
    #[derive(Clone, Default)]
    struct SharedOutput(Arc<Mutex<Vec<u8>>>);

    impl SharedOutput {
        fn written_frames(&self) -> Vec<Frame> {
            let bytes = self.0.lock().expect("output lock").clone();
            let mut cursor = Cursor::new(bytes.as_slice());
            let mut frames = Vec::new();
            while let Some(frame) = decode_from(&mut cursor).expect("server wrote valid frames") {
                frames.push(frame);
            }
            frames
        }
    }

    impl Write for SharedOutput {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().expect("output lock").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// A writer whose peer went away.
    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(ErrorKind::BrokenPipe, "client gone"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct BlockingReader {
        input: Cursor<Vec<u8>>,
        release: Receiver<()>,
        dropped: Arc<AtomicBool>,
    }

    impl Read for BlockingReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if (self.input.position() as usize) < self.input.get_ref().len() {
                return self.input.read(buf);
            }
            let _ = self.release.recv();
            Err(io::Error::from(ErrorKind::ConnectionReset))
        }
    }

    impl InputReady for BlockingReader {
        fn input_ready(&mut self) -> io::Result<bool> {
            Ok(true)
        }
    }

    impl Drop for BlockingReader {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::Relaxed);
        }
    }

    /// A writer whose write blocks until the session releases it. `dropped` is set when the
    /// writer is dropped, which is when a Windows pipe handle would be closed.
    struct StalledWriter {
        release: Receiver<()>,
        dropped: Arc<AtomicBool>,
    }

    impl StalledWriter {
        fn new(release: Receiver<()>) -> (Self, Arc<AtomicBool>) {
            let dropped = Arc::new(AtomicBool::new(false));
            (
                Self {
                    release,
                    dropped: Arc::clone(&dropped),
                },
                dropped,
            )
        }
    }

    impl Drop for StalledWriter {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::Relaxed);
        }
    }

    impl Write for StalledWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            let _ = self.release.recv();
            Err(io::Error::new(ErrorKind::BrokenPipe, "released"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// Releases a blocked test I/O operation on interrupt.
    struct ReleaseOnInterrupt(Mutex<Sender<()>>);

    impl WriteInterrupt for ReleaseOnInterrupt {
        fn interrupt(&self) {
            let _ = self.0.lock().expect("release lock").send(());
        }
    }

    struct FailingAttach;

    impl WriteInterrupt for FailingAttach {
        fn try_attach(&self) -> io::Result<()> {
            Err(io::Error::new(
                ErrorKind::PermissionDenied,
                "DuplicateHandle failed",
            ))
        }
    }

    struct FailingInterrupt;

    impl WriteInterrupt for FailingInterrupt {
        fn try_interrupt(&self) -> io::Result<()> {
            Err(io::Error::new(
                ErrorKind::PermissionDenied,
                "CancelSynchronousIo failed",
            ))
        }
    }

    fn ring_with(bytes: &[u8]) -> Mutex<RingBuffer> {
        let mut ring = RingBuffer::new(DEFAULT_CAPACITY);
        ring.append(bytes);
        Mutex::new(ring)
    }

    #[test]
    fn subscribe_replays_ring_then_detach_closes_cleanly() {
        let ring = ring_with(b"hello");
        let output = SharedOutput::default();
        let reader = MemoryReader::from_frames(&[Frame::Subscribe { offset: 0 }, Frame::Detach]);

        serve_session(reader, output.clone(), &ring).expect("clean detach");

        assert_eq!(
            output.written_frames(),
            vec![Frame::Output(b"hello".to_vec())]
        );
    }

    #[test]
    fn oversize_length_gets_error_and_closes() {
        let ring = ring_with(b"hello");
        let output = SharedOutput::default();
        let reader = MemoryReader::new(65_536_u32.to_le_bytes().to_vec());

        serve_session(reader, output.clone(), &ring).expect("closes after ERROR");

        assert_eq!(
            output.written_frames(),
            vec![Frame::Error("frame exceeds 65535 bytes".to_string())]
        );
    }

    #[test]
    fn client_eof_closes_without_error_frame() {
        let ring = ring_with(b"hello");
        let output = SharedOutput::default();

        serve_session(MemoryReader::new(Vec::new()), output.clone(), &ring)
            .expect("EOF is a clean close");

        assert!(output.written_frames().is_empty());
    }

    #[test]
    fn write_failure_is_returned_not_panicked() {
        let ring = ring_with(b"hello");
        let reader = MemoryReader::from_frames(&[Frame::Subscribe { offset: 0 }]);

        let err = serve_session(reader, FailingWriter, &ring).expect_err("write must fail");

        assert_eq!(err.kind(), ErrorKind::BrokenPipe);
    }

    #[test]
    fn read_failure_is_returned_not_treated_as_eof() {
        let ring = ring_with(b"hello");
        let mut reader = MemoryReader::new(Vec::new());
        reader.fail_reads = true;

        let err = serve_session(reader, SharedOutput::default(), &ring)
            .expect_err("a read error other than EOF is returned");

        assert_eq!(err.kind(), ErrorKind::ConnectionReset);
    }

    #[test]
    fn broken_pipe_during_partial_frame_is_a_clean_disconnect() {
        let err = serve_session(
            PartialBrokenPipeReader(Cursor::new(vec![1, 0])),
            SharedOutput::default(),
            &ring_with(b""),
        );

        assert!(err.is_ok(), "peer disconnect is clean: {err:?}");
    }

    /// A client that subscribes and never reads. Each OUTPUT chunk is 65,529 bytes, so 17
    /// chunks pass the 1 MiB limit. The loop must close on the cap without waiting for the
    /// writer, which is stalled on the first chunk, and must release the writer.
    #[test]
    fn stalled_writer_hits_backlog_cap_and_closes() {
        let mut ring = RingBuffer::with_default_capacity();
        ring.append(&vec![b'x'; DEFAULT_CAPACITY]);
        let ring = Mutex::new(ring);
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let mut reader = MemoryReader::from_frames(&[Frame::Subscribe { offset: 0 }]);
        reader.hold_open = true;
        let (writer, dropped) = StalledWriter::new(release_rx);

        let started = Instant::now();
        let err = serve_session_with_interrupt(
            reader,
            writer,
            &ring,
            ReleaseOnInterrupt(Mutex::new(release_tx)),
        )
        .expect_err("backlog cap must close the session");

        assert_eq!(err.to_string(), "client backlog exceeded");
        assert!(
            started.elapsed() < CLOSE_FLUSH * 3,
            "close waits at most the flush bound before it interrupts the writer"
        );
        assert!(
            dropped.load(Ordering::Relaxed),
            "the writer handle is released"
        );
    }

    /// A client that subscribes, never reads, and floods INPUT. Each refusal is an ERROR frame
    /// that counts against the backlog, so the session must close with the backlog error. It must
    /// not return `Ok` as if the client had detached, and it must release the writer.
    #[test]
    fn stalled_writer_input_flood_hits_backlog_cap_and_closes() {
        let ring = ring_with(b"");
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let mut frames = vec![Frame::Subscribe { offset: 0 }];
        frames.extend(std::iter::repeat_n(Frame::Input(b"k".to_vec()), 100_000));
        let reader = MemoryReader::from_frames(&frames);
        let (writer, dropped) = StalledWriter::new(release_rx);

        let started = Instant::now();
        let err = serve_session_with_interrupt(
            reader,
            writer,
            &ring,
            ReleaseOnInterrupt(Mutex::new(release_tx)),
        )
        .expect_err("refused INPUT must count against the backlog cap");

        assert_eq!(err.to_string(), "client backlog exceeded");
        assert!(started.elapsed() < CLOSE_FLUSH * 3);
        assert!(
            dropped.load(Ordering::Relaxed),
            "the writer handle is released"
        );
    }

    #[test]
    fn stalled_writer_is_released_when_reader_fails() {
        let ring = ring_with(b"hello");
        let mut reader = MemoryReader::from_frames(&[Frame::Subscribe { offset: 0 }]);
        reader.fail_reads = true;
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (writer, dropped) = StalledWriter::new(release_rx);

        let err = serve_session_with_interrupt(
            reader,
            writer,
            &ring,
            ReleaseOnInterrupt(Mutex::new(release_tx)),
        )
        .expect_err("the reader error remains the session failure");

        assert_eq!(err.kind(), ErrorKind::ConnectionReset);
        assert!(
            dropped.load(Ordering::Relaxed),
            "shutdown releases a writer stalled while the reader failed"
        );
    }

    #[test]
    fn shutdown_interrupts_a_blocked_reader() {
        let ring = ring_with(b"hello");
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let dropped = Arc::new(AtomicBool::new(false));
        let reader = BlockingReader {
            input: Cursor::new(encode(&Frame::Subscribe { offset: 0 }).expect("encode SUBSCR")),
            release: release_rx,
            dropped: Arc::clone(&dropped),
        };

        let err = serve_session_with_interrupts(
            reader,
            FailingWriter,
            &ring,
            ReleaseOnInterrupt(Mutex::new(release_tx)),
            NoInterrupt,
            WRITE_STALL_LIMIT,
        )
        .expect_err("the writer failure remains the session failure");

        assert_eq!(err.kind(), ErrorKind::BrokenPipe);
        assert!(
            dropped.load(Ordering::Relaxed),
            "shutdown releases the reader blocked in a partial frame"
        );
    }

    #[test]
    fn writer_attachment_failure_is_returned() {
        let err = serve_session_with_interrupt(
            MemoryReader::new(Vec::new()),
            SharedOutput::default(),
            &ring_with(b""),
            FailingAttach,
        )
        .expect_err("a failed thread-handle attachment is not a clean session");

        assert_eq!(err.kind(), ErrorKind::PermissionDenied);
        assert!(err.to_string().contains("DuplicateHandle failed"));
    }

    #[test]
    fn cancellation_failure_is_bounded_and_not_reported_as_cleaned() {
        let ring = ring_with(b"hello");
        let mut reader = MemoryReader::from_frames(&[Frame::Subscribe { offset: 0 }]);
        reader.fail_reads = true;
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (writer, dropped) = StalledWriter::new(release_rx);

        let started = Instant::now();
        let err = serve_session_with_interrupt(reader, writer, &ring, FailingInterrupt)
            .expect_err("a worker that did not stop is not cleaned up");

        assert_eq!(err.kind(), ErrorKind::ConnectionReset);
        assert!(err.to_string().contains("CancelSynchronousIo failed"));
        assert!(err.to_string().contains("may still own the connection"));
        assert!(
            started.elapsed() < CLOSE_FLUSH + CANCEL_GRACE + Duration::from_secs(2),
            "cancellation failure is bounded"
        );
        assert!(
            !dropped.load(Ordering::Relaxed),
            "an abandoned writer is not reported as cleaned up"
        );

        release_tx.send(()).expect("release the test writer");
        let deadline = Instant::now() + Duration::from_secs(1);
        while !dropped.load(Ordering::Relaxed) && Instant::now() < deadline {
            thread::sleep(IDLE_POLL);
        }
        assert!(dropped.load(Ordering::Relaxed));
    }

    /// Sends SUBSCR, then blocks in readiness. This models a reader whose check waits behind a write
    /// stalled in the kernel, so no further client bytes arrive. It returns once `release` is sent.
    struct ReadinessBlockedReader {
        input: Cursor<Vec<u8>>,
        release: Receiver<()>,
    }

    impl Read for ReadinessBlockedReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.input.read(buf)
        }
    }

    impl InputReady for ReadinessBlockedReader {
        fn input_ready(&mut self) -> io::Result<bool> {
            if (self.input.position() as usize) < self.input.get_ref().len() {
                return Ok(true);
            }
            let _ = self.release.recv();
            Ok(false)
        }
    }

    /// The reader cannot make progress while its write is stalled, so the backlog cap is never
    /// reached. The session must still close once that write has stalled for the limit, and
    /// release both workers.
    #[test]
    fn stalled_write_closes_session_when_reader_readiness_is_blocked() {
        let ring = ring_with(b"hello");
        let (reader_release_tx, reader_release_rx) = mpsc::channel::<()>();
        let reader = ReadinessBlockedReader {
            input: Cursor::new(encode(&Frame::Subscribe { offset: 0 }).expect("encode SUBSCR")),
            release: reader_release_rx,
        };
        let (writer_release_tx, writer_release_rx) = mpsc::channel::<()>();
        let (writer, dropped) = StalledWriter::new(writer_release_rx);
        let stall_limit = Duration::from_millis(200);

        let started = Instant::now();
        let err = serve_session_with_interrupts(
            reader,
            writer,
            &ring,
            ReleaseOnInterrupt(Mutex::new(reader_release_tx)),
            ReleaseOnInterrupt(Mutex::new(writer_release_tx)),
            stall_limit,
        )
        .expect_err("a write stalled past the limit must close the session");

        assert_eq!(err.to_string(), "client backlog exceeded");
        assert!(
            started.elapsed() >= stall_limit,
            "the session closes only after the stall limit"
        );
        assert!(
            started.elapsed() < stall_limit + CLOSE_FLUSH + CANCEL_GRACE + Duration::from_secs(2),
            "the close is bounded"
        );
        assert!(
            dropped.load(Ordering::Relaxed),
            "the writer handle is released"
        );
    }
}
