//! Attach detach-key scanner and replay cursor for mcao-core.
//!
//! Provides utilities for scanning input streams for the detach escape key
//! and tracking replay offsets against an output ring buffer.

/// Detach key byte sequence (`0x1d`, corresponding to `Ctrl+]`).
pub const DETACH_KEY: u8 = 0x1d;

/// Finds the index of the first [`DETACH_KEY`] byte in `input`.
///
/// Returns `Some(index)` if the detach key is present, or `None` if it is not found.
pub fn find_detach(input: &[u8]) -> Option<usize> {
    input.iter().position(|&b| b == DETACH_KEY)
}

/// Strips bytes following and including the detach key.
///
/// Returns a tuple containing the slice of `input` before the first [`DETACH_KEY`]
/// and a boolean indicating whether the key was found.
pub fn strip_after_detach(input: &[u8]) -> (&[u8], bool) {
    match find_detach(input) {
        Some(index) => (&input[..index], true),
        None => (input, false),
    }
}

/// Tracks how much of an output ring buffer a client has been sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReplayCursor {
    next_offset: u64,
}

impl ReplayCursor {
    /// Creates a new cursor starting at `start`.
    pub fn new(start: u64) -> Self {
        Self { next_offset: start }
    }

    /// Returns the cursor's next read offset.
    pub fn next_offset(&self) -> u64 {
        self.next_offset
    }

    /// Given the buffer's oldest and current offsets, returns the offset to read from and whether data was lost (cursor behind oldest). A cursor at or past current returns current with lost=false.
    pub fn plan(&self, oldest: u64, current: u64) -> (u64, bool) {
        if self.next_offset >= current {
            (current, false)
        } else if self.next_offset < oldest {
            (oldest, true)
        } else {
            (self.next_offset, false)
        }
    }

    /// Advances the cursor by `bytes`, saturating at [`u64::MAX`].
    pub fn advance(&mut self, bytes: usize) {
        self.next_offset = self.next_offset.saturating_add(bytes as u64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_detach_none() {
        assert_eq!(find_detach(b"hello world"), None);
    }

    #[test]
    fn find_detach_first_byte() {
        assert_eq!(find_detach(&[DETACH_KEY, b'a', b'b']), Some(0));
    }

    #[test]
    fn find_detach_middle() {
        assert_eq!(find_detach(&[b'a', DETACH_KEY, b'b']), Some(1));
    }

    #[test]
    fn find_detach_multiple_first_wins() {
        assert_eq!(
            find_detach(&[b'a', DETACH_KEY, b'b', DETACH_KEY, b'c']),
            Some(1)
        );
    }

    #[test]
    fn find_detach_empty() {
        assert_eq!(find_detach(&[]), None);
    }

    #[test]
    fn strip_after_detach_none() {
        assert_eq!(
            strip_after_detach(b"hello world"),
            (b"hello world".as_slice(), false)
        );
    }

    #[test]
    fn strip_after_detach_first_byte() {
        assert_eq!(
            strip_after_detach(&[DETACH_KEY, b'a', b'b']),
            (b"".as_slice(), true)
        );
    }

    #[test]
    fn strip_after_detach_middle() {
        assert_eq!(
            strip_after_detach(&[b'a', b'b', DETACH_KEY, b'c']),
            (b"ab".as_slice(), true)
        );
    }

    #[test]
    fn strip_after_detach_multiple() {
        assert_eq!(
            strip_after_detach(&[b'a', DETACH_KEY, b'b', DETACH_KEY]),
            (b"a".as_slice(), true)
        );
    }

    #[test]
    fn strip_after_detach_empty() {
        assert_eq!(strip_after_detach(&[]), (b"".as_slice(), false));
    }

    #[test]
    fn strip_after_detach_at_end() {
        assert_eq!(
            strip_after_detach(&[b'f', b'o', b'o', DETACH_KEY]),
            (b"foo".as_slice(), true)
        );
    }

    #[test]
    fn replay_cursor_inside_range() {
        let cursor = ReplayCursor::new(15);
        assert_eq!(cursor.plan(10, 20), (15, false));
        assert_eq!(cursor.next_offset(), 15);
    }

    #[test]
    fn replay_cursor_behind_oldest() {
        let cursor = ReplayCursor::new(5);
        assert_eq!(cursor.plan(10, 20), (10, true));
    }

    #[test]
    fn replay_cursor_at_current() {
        let cursor = ReplayCursor::new(20);
        assert_eq!(cursor.plan(10, 20), (20, false));
    }

    #[test]
    fn replay_cursor_beyond_current() {
        let cursor = ReplayCursor::new(25);
        assert_eq!(cursor.plan(10, 20), (20, false));
    }

    #[test]
    fn replay_cursor_advance() {
        let mut cursor = ReplayCursor::new(10);
        cursor.advance(15);
        assert_eq!(cursor.next_offset(), 25);
    }

    #[test]
    fn replay_cursor_advance_saturating() {
        let mut cursor = ReplayCursor::new(u64::MAX - 5);
        cursor.advance(10);
        assert_eq!(cursor.next_offset(), u64::MAX);

        let mut cursor_max = ReplayCursor::new(u64::MAX);
        cursor_max.advance(100);
        assert_eq!(cursor_max.next_offset(), u64::MAX);
    }

    #[test]
    fn replay_cursor_empty_buffer() {
        let cursor_at_zero = ReplayCursor::new(0);
        assert_eq!(cursor_at_zero.plan(0, 0), (0, false));

        let cursor_ahead = ReplayCursor::new(5);
        assert_eq!(cursor_ahead.plan(0, 0), (0, false));

        let cursor_behind = ReplayCursor::new(5);
        assert_eq!(cursor_behind.plan(10, 10), (10, true));
    }
}
