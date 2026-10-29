//! Bounded in-memory ring buffer for worker console output.

/// Default ring buffer capacity in bytes (2 MiB).
pub const DEFAULT_CAPACITY: usize = 2 * 1024 * 1024;

/// Maximum allowed capacity when parsed from an environment variable (64 MiB).
pub const MAX_ENV_CAPACITY: usize = 64 * 1024 * 1024;

/// Bounded append-only ring buffer with wrap-around overwrite.
pub struct RingBuffer {
    data: Vec<u8>,
    write_pos: usize,
    total_written: u64,
}

impl RingBuffer {
    /// Creates a new ring buffer with the given capacity.
    ///
    /// A capacity of 0 is treated as 1.
    pub fn new(capacity: usize) -> Self {
        let cap = capacity.max(1);
        Self {
            data: vec![0u8; cap],
            write_pos: 0,
            total_written: 0,
        }
    }

    /// Creates a new ring buffer with [`DEFAULT_CAPACITY`].
    pub fn with_default_capacity() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }

    /// Returns the capacity of the ring buffer in bytes.
    pub fn capacity(&self) -> usize {
        self.data.len()
    }

    /// Appends a byte slice to the ring buffer.
    ///
    /// Overwrites the oldest bytes if capacity is exceeded. If the chunk is longer
    /// than capacity, only its last `capacity` bytes are kept, but `total_written`
    /// advances by the full chunk length.
    pub fn append(&mut self, chunk: &[u8]) {
        if chunk.is_empty() {
            return;
        }
        self.total_written = self.total_written.saturating_add(chunk.len() as u64);
        let cap = self.data.len();
        let (to_write, start) = if chunk.len() >= cap {
            let to_write = &chunk[chunk.len() - cap..];
            let start = (self.write_pos + chunk.len()) % cap;
            (to_write, start)
        } else {
            (chunk, self.write_pos)
        };

        let len = to_write.len();
        let first = (cap - start).min(len);
        self.data[start..start + first].copy_from_slice(&to_write[..first]);
        let second = len - first;
        if second > 0 {
            self.data[..second].copy_from_slice(&to_write[first..]);
        }
        self.write_pos = (start + len) % cap;
    }

    /// Returns the total number of bytes written across the buffer's lifetime.
    ///
    /// This value monotonically increases and never wraps.
    pub fn current_offset(&self) -> u64 {
        self.total_written
    }

    /// Returns the oldest absolute byte offset still stored in the buffer.
    pub fn oldest_offset(&self) -> u64 {
        self.total_written.saturating_sub(self.data.len() as u64)
    }

    /// Reads bytes starting from absolute `offset` up to [`current_offset`](Self::current_offset).
    ///
    /// Returns an empty vector if `offset >= current_offset()` or `offset < oldest_offset()`.
    /// Otherwise returns `current_offset() - offset` bytes in original write order.
    pub fn read_from(&self, offset: u64) -> Vec<u8> {
        let current = self.current_offset();
        let oldest = self.oldest_offset();
        if offset >= current || offset < oldest {
            return Vec::new();
        }
        let cap = self.data.len();
        let count = (self.total_written - offset) as usize;
        let start = if count == cap {
            self.write_pos
        } else {
            (self.write_pos + cap - (count % cap)) % cap
        };
        let mut out = Vec::with_capacity(count);
        let first = (cap - start).min(count);
        out.extend_from_slice(&self.data[start..start + first]);
        let second = count - first;
        if second > 0 {
            out.extend_from_slice(&self.data[..second]);
        }
        out
    }
}

impl Default for RingBuffer {
    fn default() -> Self {
        Self::with_default_capacity()
    }
}

impl std::fmt::Debug for RingBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RingBuffer")
            .field("capacity", &self.capacity())
            .field("write_pos", &self.write_pos)
            .field("total_written", &self.total_written)
            .finish()
    }
}

/// Resolves a buffer capacity from an environment variable value, falling back
/// to [`DEFAULT_CAPACITY`] if missing, empty, non-numeric, zero, or above 64 MiB.
pub fn capacity_from_env_value(value: Option<&str>) -> usize {
    match value {
        Some(s) if !s.is_empty() => match s.parse::<usize>() {
            Ok(n) if (1..=MAX_ENV_CAPACITY).contains(&n) => n,
            _ => DEFAULT_CAPACITY,
        },
        _ => DEFAULT_CAPACITY,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_buffer() {
        let ring = RingBuffer::new(10);
        assert_eq!(ring.capacity(), 10);
        assert_eq!(ring.current_offset(), 0);
        assert_eq!(ring.oldest_offset(), 0);
        assert!(ring.read_from(0).is_empty());
        assert!(ring.read_from(1).is_empty());
    }

    #[test]
    fn append_less_than_capacity_and_read_from_zero() {
        let mut ring = RingBuffer::new(10);
        ring.append(b"hello");
        assert_eq!(ring.capacity(), 10);
        assert_eq!(ring.current_offset(), 5);
        assert_eq!(ring.oldest_offset(), 0);
        assert_eq!(ring.read_from(0), b"hello");
        assert_eq!(ring.read_from(2), b"llo");
    }

    #[test]
    fn exact_capacity() {
        let mut ring = RingBuffer::new(5);
        ring.append(b"12345");
        assert_eq!(ring.capacity(), 5);
        assert_eq!(ring.current_offset(), 5);
        assert_eq!(ring.oldest_offset(), 0);
        assert_eq!(ring.read_from(0), b"12345");
        assert_eq!(ring.read_from(1), b"2345");
        assert_eq!(ring.read_from(4), b"5");
    }

    #[test]
    fn wrap_around_keeps_order() {
        // capacity 8, append 5 then 6 bytes, read from oldest and from a middle offset
        let mut ring = RingBuffer::new(8);
        ring.append(b"abcde");
        assert_eq!(ring.current_offset(), 5);
        assert_eq!(ring.oldest_offset(), 0);

        ring.append(b"fghijk");
        assert_eq!(ring.current_offset(), 11);
        assert_eq!(ring.oldest_offset(), 3);

        // Read from oldest offset
        assert_eq!(ring.read_from(3), b"defghijk");

        // Read from middle offsets
        assert_eq!(ring.read_from(5), b"fghijk");
        assert_eq!(ring.read_from(6), b"ghijk");
        assert_eq!(ring.read_from(10), b"k");
    }

    #[test]
    fn read_from_below_oldest_returns_empty() {
        let mut ring = RingBuffer::new(8);
        ring.append(b"abcde");
        ring.append(b"fghijk");
        assert_eq!(ring.oldest_offset(), 3);
        assert!(ring.read_from(0).is_empty());
        assert!(ring.read_from(1).is_empty());
        assert!(ring.read_from(2).is_empty());
    }

    #[test]
    fn read_from_at_or_beyond_current_returns_empty() {
        let mut ring = RingBuffer::new(8);
        ring.append(b"abcde");
        assert_eq!(ring.current_offset(), 5);
        assert!(ring.read_from(5).is_empty());
        assert!(ring.read_from(6).is_empty());
        assert!(ring.read_from(100).is_empty());
    }

    #[test]
    fn single_chunk_larger_than_capacity() {
        let mut ring = RingBuffer::new(5);
        ring.append(b"0123456789");
        assert_eq!(ring.current_offset(), 10);
        assert_eq!(ring.oldest_offset(), 5);
        assert_eq!(ring.read_from(5), b"56789");
        assert!(ring.read_from(4).is_empty());
        assert_eq!(ring.read_from(7), b"789");
    }

    #[test]
    fn many_small_appends_equal_one_large_append() {
        let data = b"the quick brown fox jumps over the lazy dog";
        let mut ring_batch = RingBuffer::new(16);
        let mut ring_single = RingBuffer::new(16);

        ring_batch.append(data);
        for byte in data {
            ring_single.append(&[*byte]);
        }

        assert_eq!(ring_batch.current_offset(), ring_single.current_offset());
        assert_eq!(ring_batch.oldest_offset(), ring_single.oldest_offset());
        let oldest = ring_batch.oldest_offset();
        let current = ring_batch.current_offset();

        for off in oldest..=current + 5 {
            assert_eq!(ring_batch.read_from(off), ring_single.read_from(off));
        }
    }

    #[test]
    fn capacity_zero_treated_as_one() {
        let mut ring = RingBuffer::new(0);
        assert_eq!(ring.capacity(), 1);
        ring.append(b"a");
        assert_eq!(ring.current_offset(), 1);
        assert_eq!(ring.oldest_offset(), 0);
        assert_eq!(ring.read_from(0), b"a");

        ring.append(b"bc");
        assert_eq!(ring.current_offset(), 3);
        assert_eq!(ring.oldest_offset(), 2);
        assert_eq!(ring.read_from(2), b"c");
        assert!(ring.read_from(1).is_empty());
    }

    #[test]
    fn capacity_from_env_value_test_cases() {
        assert_eq!(capacity_from_env_value(None), DEFAULT_CAPACITY);
        assert_eq!(capacity_from_env_value(Some("")), DEFAULT_CAPACITY);
        assert_eq!(capacity_from_env_value(Some("abc")), DEFAULT_CAPACITY);
        assert_eq!(capacity_from_env_value(Some("0")), DEFAULT_CAPACITY);
        assert_eq!(capacity_from_env_value(Some("1048576")), 1048576);
        assert_eq!(
            capacity_from_env_value(Some("99999999999")),
            DEFAULT_CAPACITY
        );
        assert_eq!(capacity_from_env_value(Some("67108864")), 67108864);
        assert_eq!(capacity_from_env_value(Some("67108865")), DEFAULT_CAPACITY);
    }
}
