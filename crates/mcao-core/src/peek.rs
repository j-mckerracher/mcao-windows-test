//! Peek line extraction and client backlog tracking (DPES-883).

use std::error::Error;
use std::fmt;

/// The default maximum number of queued backlog bytes per client (1 MiB).
pub const MAX_BACKLOG_BYTES: usize = 1024 * 1024;

/// Tracks the number of bytes queued for a client and enforces a maximum limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BacklogTracker {
    queued: usize,
    limit: usize,
}

/// An error indicating that enqueuing data would exceed the backlog limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BacklogExceeded {
    /// The number of queued bytes that would result from the operation.
    pub queued: usize,
    /// The maximum allowed backlog limit in bytes.
    pub limit: usize,
}

impl fmt::Display for BacklogExceeded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "backlog exceeded: {} of {} bytes",
            self.queued, self.limit
        )
    }
}

impl Error for BacklogExceeded {}

impl BacklogTracker {
    /// Creates a new backlog tracker with the default limit of [`MAX_BACKLOG_BYTES`].
    pub fn new() -> Self {
        Self::with_limit(MAX_BACKLOG_BYTES)
    }

    /// Creates a new backlog tracker with a custom byte limit.
    pub fn with_limit(limit: usize) -> Self {
        Self { queued: 0, limit }
    }

    /// Enqueues additional bytes into the backlog.
    ///
    /// Returns `Ok(())` if the resulting backlog does not strictly exceed the limit
    /// (an exact match to the limit is permitted). If the limit would be exceeded,
    /// returns `Err(BacklogExceeded)` and leaves the tracker state unchanged.
    /// Uses checked arithmetic so overflow does not panic.
    pub fn enqueue(&mut self, bytes: usize) -> Result<(), BacklogExceeded> {
        let new_queued = self.queued.checked_add(bytes);
        match new_queued {
            Some(total) if total <= self.limit => {
                self.queued = total;
                Ok(())
            }
            Some(total) => Err(BacklogExceeded {
                queued: total,
                limit: self.limit,
            }),
            None => Err(BacklogExceeded {
                queued: usize::MAX,
                limit: self.limit,
            }),
        }
    }

    /// Drains bytes from the backlog, saturating at zero.
    pub fn drained(&mut self, bytes: usize) {
        self.queued = self.queued.saturating_sub(bytes);
    }

    /// Returns the number of bytes currently queued in the backlog.
    pub fn queued(&self) -> usize {
        self.queued
    }
}

impl Default for BacklogTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Returns the last `n` lines of the raw terminal output, bytes unchanged,
/// including the line terminator of each returned line.
///
/// A line ends at `\n`; `\r\n` is kept as is. If the final line has no terminating
/// newline it still counts as a line and is returned without one added.
/// If `n == 0` or `output` is empty, an empty vector is returned. If `output`
/// has fewer than `n` lines, all of it is returned.
///
/// Runs in O(len) with one backward scan and a single copy.
pub fn last_lines(output: &[u8], n: usize) -> Vec<u8> {
    if n == 0 || output.is_empty() {
        return Vec::new();
    }

    let search_end = if output.last() == Some(&b'\n') {
        output.len() - 1
    } else {
        output.len()
    };

    let mut count = 0;
    let mut start = 0;
    for (i, &byte) in output[..search_end].iter().enumerate().rev() {
        if byte == b'\n' {
            count += 1;
            if count == n {
                start = i + 1;
                break;
            }
        }
    }

    output[start..].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_last_lines_zero_n() {
        assert_eq!(last_lines(b"hello\nworld\n", 0), b"");
        assert_eq!(last_lines(b"", 0), b"");
        assert_eq!(last_lines(b"unterminated", 0), b"");
    }

    #[test]
    fn test_last_lines_empty_input() {
        assert_eq!(last_lines(b"", 1), b"");
        assert_eq!(last_lines(b"", 5), b"");
    }

    #[test]
    fn test_last_lines_fewer_lines_than_n() {
        assert_eq!(last_lines(b"alpha\nbeta\n", 5), b"alpha\nbeta\n");
        assert_eq!(last_lines(b"alpha\nbeta", 5), b"alpha\nbeta");
        assert_eq!(last_lines(b"single line", 2), b"single line");
    }

    #[test]
    fn test_last_lines_exact_n() {
        assert_eq!(last_lines(b"one\ntwo\nthree\n", 3), b"one\ntwo\nthree\n");
        assert_eq!(last_lines(b"one\ntwo\nthree", 3), b"one\ntwo\nthree");
    }

    #[test]
    fn test_last_lines_unterminated_final_line() {
        assert_eq!(last_lines(b"first\nsecond\nthird", 1), b"third");
        assert_eq!(last_lines(b"first\nsecond\nthird", 2), b"second\nthird");
        assert_eq!(
            last_lines(b"first\nsecond\nthird", 3),
            b"first\nsecond\nthird"
        );
        assert_eq!(last_lines(b"only", 1), b"only");
    }

    #[test]
    fn test_last_lines_crlf_kept() {
        assert_eq!(last_lines(b"line1\r\nline2\r\n", 1), b"line2\r\n");
        assert_eq!(last_lines(b"line1\r\nline2\r\n", 2), b"line1\r\nline2\r\n");
        assert_eq!(last_lines(b"line1\r\nline2", 1), b"line2");
        assert_eq!(last_lines(b"\r\n", 1), b"\r\n");
    }

    #[test]
    fn test_last_lines_lone_newline() {
        assert_eq!(last_lines(b"\n", 1), b"\n");
        assert_eq!(last_lines(b"\n", 2), b"\n");
    }

    #[test]
    fn test_last_lines_many_lines_taking_last_three() {
        let mut input = Vec::new();
        for i in 0..1000 {
            input.extend_from_slice(format!("line {}\n", i).as_bytes());
        }
        let expected = b"line 997\nline 998\nline 999\n";
        assert_eq!(last_lines(&input, 3), expected);
    }

    #[test]
    fn test_last_lines_only_newlines() {
        assert_eq!(last_lines(b"\n\n\n", 1), b"\n");
        assert_eq!(last_lines(b"\n\n\n", 2), b"\n\n");
        assert_eq!(last_lines(b"\n\n\n", 3), b"\n\n\n");
        assert_eq!(last_lines(b"\n\n\n", 4), b"\n\n\n");
    }

    #[test]
    fn test_last_lines_not_utf8() {
        let raw = b"\xff\xfe\x00\n\x80\x81\n\xaa\xbb";
        assert_eq!(last_lines(raw, 1), b"\xaa\xbb");
        assert_eq!(last_lines(raw, 2), b"\x80\x81\n\xaa\xbb");
        assert_eq!(last_lines(raw, 3), b"\xff\xfe\x00\n\x80\x81\n\xaa\xbb");
    }

    #[test]
    fn test_backlog_tracker_exactly_at_limit() {
        let mut tracker = BacklogTracker::with_limit(100);
        assert_eq!(tracker.enqueue(100), Ok(()));
        assert_eq!(tracker.queued(), 100);
    }

    #[test]
    fn test_backlog_tracker_one_over_refused_unchanged_state() {
        let mut tracker = BacklogTracker::with_limit(100);
        assert_eq!(tracker.enqueue(100), Ok(()));
        let res = tracker.enqueue(1);
        assert_eq!(
            res,
            Err(BacklogExceeded {
                queued: 101,
                limit: 100,
            })
        );
        assert_eq!(tracker.queued(), 100);
    }

    #[test]
    fn test_backlog_tracker_drain_then_enqueue_succeeds() {
        let mut tracker = BacklogTracker::with_limit(100);
        assert_eq!(tracker.enqueue(100), Ok(()));
        tracker.drained(40);
        assert_eq!(tracker.queued(), 60);
        assert_eq!(tracker.enqueue(40), Ok(()));
        assert_eq!(tracker.queued(), 100);
    }

    #[test]
    fn test_backlog_tracker_drain_below_zero_saturates() {
        let mut tracker = BacklogTracker::with_limit(100);
        assert_eq!(tracker.enqueue(30), Ok(()));
        tracker.drained(50);
        assert_eq!(tracker.queued(), 0);
        tracker.drained(10);
        assert_eq!(tracker.queued(), 0);
    }

    #[test]
    fn test_backlog_tracker_enqueue_max_refused() {
        let mut tracker = BacklogTracker::new();
        assert_eq!(
            tracker.enqueue(usize::MAX),
            Err(BacklogExceeded {
                queued: usize::MAX,
                limit: MAX_BACKLOG_BYTES,
            })
        );
        assert_eq!(tracker.queued(), 0);

        assert_eq!(tracker.enqueue(10), Ok(()));
        assert_eq!(
            tracker.enqueue(usize::MAX),
            Err(BacklogExceeded {
                queued: usize::MAX,
                limit: MAX_BACKLOG_BYTES,
            })
        );
        assert_eq!(tracker.queued(), 10);
    }

    #[test]
    fn test_backlog_exceeded_display_text() {
        let err = BacklogExceeded {
            queued: 101,
            limit: 100,
        };
        assert_eq!(err.to_string(), "backlog exceeded: 101 of 100 bytes");
        let dyn_err: &dyn std::error::Error = &err;
        assert_eq!(dyn_err.to_string(), "backlog exceeded: 101 of 100 bytes");
    }

    #[test]
    fn test_backlog_tracker_default() {
        let tracker = BacklogTracker::default();
        assert_eq!(tracker.queued(), 0);
        assert_eq!(tracker, BacklogTracker::new());
    }
}
