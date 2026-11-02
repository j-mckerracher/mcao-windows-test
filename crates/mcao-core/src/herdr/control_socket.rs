//! Socket transport shared by the herdr control-socket commands: a connect with a time budget and
//! a line reader under an absolute deadline. `eventwait` and `workspace-move` both use it, so the
//! two bounds the references share are stated once.

use std::io::{self, Read};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

/// How long one socket read may block before the reader checks its deadline again.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// Size of one socket read, as in the reference readers.
const RECV_CHUNK: usize = 65_536;

/// Opens the control socket at `path`. `connector` runs on a helper thread, and the caller waits
/// at most `budget` for the stream. The standard library cannot cancel a blocked connect, so an
/// overrun is abandoned rather than stopped: the helper stays blocked until the connect returns,
/// then drops the stream it produced, which closes the descriptor. The CLI exits on this error,
/// so the helper does not outlive the process in practice.
pub fn connect_within<F>(path: &Path, budget: Duration, connector: F) -> io::Result<UnixStream>
where
    F: FnOnce(PathBuf) -> io::Result<UnixStream> + Send + 'static,
{
    let (sender, receiver) = mpsc::channel();
    let owned = path.to_path_buf();
    thread::Builder::new()
        .name("herdr-connect".to_owned())
        .spawn(move || {
            // After a timeout the receiver is gone, so a late stream is dropped here.
            let _ = sender.send(connector(owned));
        })?;
    match receiver.recv_timeout(budget) {
        Ok(result) => result,
        Err(RecvTimeoutError::Timeout) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("no connection within {} seconds", budget.as_secs_f64()),
        )),
        Err(RecvTimeoutError::Disconnected) => Err(io::Error::other(
            "the connect thread ended without a result",
        )),
    }
}

/// Sets the read poll interval and the write budget on a freshly connected socket. It runs while
/// the peer is still open: macOS refuses a receive timeout on a socket whose peer has closed, so
/// a per-read timeout would fail exactly when herdr ends the stream.
pub fn set_timeouts(stream: &UnixStream, write: Duration) -> io::Result<()> {
    stream.set_read_timeout(Some(POLL_INTERVAL))?;
    stream.set_write_timeout(Some(write))
}

/// The outcome of one attempt to read a line.
pub enum Next {
    /// One complete line, without its newline.
    Line(Vec<u8>),
    /// The deadline passed with no complete line buffered.
    Timeout,
    /// The peer closed the stream.
    Closed,
    /// A read failed, or the bytes buffered without a newline passed the size limit.
    Failed(io::Error),
}

/// Splits a socket into newline-terminated lines under an absolute deadline. It mirrors
/// `_read_line` in the reference readers: a complete line already buffered is returned even after
/// the deadline, and a deadline with no line is a timeout rather than a failure.
pub struct LineReader {
    stream: UnixStream,
    buffered: Vec<u8>,
    chunk: Box<[u8]>,
    limit: Option<usize>,
}

impl LineReader {
    /// Wraps `stream`. `limit` caps the bytes buffered while no line is complete, as the
    /// workspace-move reference does. `None` is unbounded, as the eventwait reference is.
    pub fn new(stream: UnixStream, limit: Option<usize>) -> Self {
        Self {
            stream,
            buffered: Vec::new(),
            chunk: vec![0; RECV_CHUNK].into_boxed_slice(),
            limit,
        }
    }

    /// Returns the next line, or why none arrived before `deadline`.
    pub fn next_line(&mut self, deadline: Instant) -> Next {
        loop {
            if let Some(end) = self.buffered.iter().position(|&byte| byte == b'\n') {
                let mut line: Vec<u8> = self.buffered.drain(..=end).collect();
                line.pop();
                return Next::Line(line);
            }
            if Instant::now() >= deadline {
                return Next::Timeout;
            }
            match self.stream.read(&mut self.chunk) {
                Ok(0) => return Next::Closed,
                Ok(count) => {
                    self.buffered.extend_from_slice(&self.chunk[..count]);
                    if self.limit.is_some_and(|limit| self.buffered.len() > limit) {
                        return Next::Failed(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "reply exceeds the size limit",
                        ));
                    }
                }
                // A poll slice ran out or the call was interrupted: check the deadline again.
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::Interrupted
                            | io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                    ) => {}
                Err(error) => return Next::Failed(error),
            }
        }
    }
}
