//! Fixed-capacity ring buffer for PTY output scrollback.
//!
//! When the buffer is full, pushing new bytes silently evicts the oldest
//! bytes from the front.  Memory usage is bounded at `capacity` bytes
//! regardless of PTY output volume.
//!
//! # Usage in the broker
//!
//! Each registered PTY session owns a `RingBuffer`.  The broker appends PTY
//! output via [`RingBuffer::push`]; on hub reconnect the hub calls
//! `GetSnapshot` and the broker responds with [`RingBuffer::to_vec`] in a
//! `Snapshot` frame.  The hub feeds those bytes into a fresh `vt100::Parser`
//! to restore terminal state without replaying raw escape sequences from the
//! beginning of time.

// Rust guideline compliant 2026-02

use std::collections::VecDeque;

/// Default ring-buffer capacity: 1 MiB.
pub const DEFAULT_RING_CAPACITY: usize = 1024 * 1024;

/// Fixed-capacity byte ring buffer.
///
/// Pushing more bytes than `capacity` silently drops the oldest data.
/// The buffer never panics or reallocates beyond its configured limit.
pub struct RingBuffer {
    buf: VecDeque<u8>,
    capacity: usize,
}

impl RingBuffer {
    /// Create a new ring buffer with the given byte capacity.
    ///
    /// # Panics
    ///
    /// Panics if `capacity == 0`.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "RingBuffer capacity must be > 0");
        Self {
            // Pre-allocate a modest chunk; VecDeque grows lazily.
            buf: VecDeque::with_capacity(capacity.min(65_536)),
            capacity,
        }
    }

    /// Create a ring buffer with [`DEFAULT_RING_CAPACITY`] (1 MiB).
    #[must_use]
    pub fn with_default_capacity() -> Self {
        Self::new(DEFAULT_RING_CAPACITY)
    }

    /// Append `data` to the buffer, evicting the oldest bytes if needed.
    ///
    /// If `data.len() >= capacity`, only the **last** `capacity` bytes of
    /// `data` are retained (the buffer is cleared first).
    pub fn push(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }

        if data.len() >= self.capacity {
            // Single push larger than the whole buffer — keep only the tail.
            self.buf.clear();
            let start = data.len() - self.capacity;
            self.buf.extend(&data[start..]);
            return;
        }

        // Evict oldest bytes to make room for `data`.
        let needed = self.buf.len() + data.len();
        if needed > self.capacity {
            let to_drain = needed - self.capacity;
            self.buf.drain(..to_drain);
        }

        self.buf.extend(data);
    }

    /// Return a contiguous copy of all buffered bytes (oldest first).
    #[must_use]
    pub fn to_vec(&self) -> Vec<u8> {
        let (a, b) = self.buf.as_slices();
        let mut v = Vec::with_capacity(a.len() + b.len());
        v.extend_from_slice(a);
        v.extend_from_slice(b);
        v
    }

    /// Current number of buffered bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// True if no bytes are buffered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Discard all buffered bytes without changing capacity.
    pub fn clear(&mut self) {
        self.buf.clear();
    }

    /// Configured maximum capacity in bytes.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Construction ──────────────────────────────────────────────────────

    #[test]
    fn test_new_buffer_is_empty() {
        let rb = RingBuffer::new(1024);
        assert!(rb.is_empty());
        assert_eq!(rb.len(), 0);
        assert_eq!(rb.capacity(), 1024);
        assert!(rb.to_vec().is_empty());
    }

    #[test]
    #[should_panic(expected = "capacity must be > 0")]
    fn test_zero_capacity_panics() {
        let _ = RingBuffer::new(0);
    }

    #[test]
    fn test_default_capacity_is_1mb() {
        let rb = RingBuffer::with_default_capacity();
        assert_eq!(rb.capacity(), DEFAULT_RING_CAPACITY);
        assert_eq!(rb.capacity(), 1024 * 1024);
    }

    // ── Basic push/read ───────────────────────────────────────────────────

    #[test]
    fn test_push_and_read_bytes() {
        let mut rb = RingBuffer::new(64);
        rb.push(b"hello");
        rb.push(b" world");
        assert_eq!(rb.to_vec(), b"hello world");
        assert_eq!(rb.len(), 11);
    }

    #[test]
    fn test_push_empty_slice_is_noop() {
        let mut rb = RingBuffer::new(64);
        rb.push(b"data");
        rb.push(b"");
        assert_eq!(rb.len(), 4);
        assert_eq!(rb.to_vec(), b"data");
    }

    #[test]
    fn test_push_exactly_capacity_bytes() {
        let cap = 16usize;
        let mut rb = RingBuffer::new(cap);
        let data = vec![0xAAu8; cap];
        rb.push(&data);
        assert_eq!(rb.len(), cap);
        assert_eq!(rb.to_vec(), data);
    }

    // ── Overflow / eviction ───────────────────────────────────────────────

    #[test]
    fn test_overflow_drops_oldest_bytes() {
        let mut rb = RingBuffer::new(8);
        rb.push(b"AAAAAAAA"); // fills exactly
        rb.push(b"BB");       // pushes out first 2 bytes
        let contents = rb.to_vec();
        assert_eq!(rb.len(), 8);
        // First 2 'A's are gone; remaining 6 'A's + "BB"
        assert_eq!(&contents[..6], b"AAAAAA");
        assert_eq!(&contents[6..], b"BB");
    }

    #[test]
    fn test_overflow_oldest_bytes_not_present() {
        let mut rb = RingBuffer::new(10);
        rb.push(b"12345");   // first 5
        rb.push(b"67890");   // fills exactly
        rb.push(b"ABCDE");   // evicts first 5 bytes ("12345")
        let contents = rb.to_vec();
        assert_eq!(rb.len(), 10);
        assert!(!contents.starts_with(b"12345"), "oldest bytes should be gone");
        assert_eq!(&contents[..5], b"67890");
        assert_eq!(&contents[5..], b"ABCDE");
    }

    #[test]
    fn test_caps_at_capacity_no_panic_on_1mb() {
        let mut rb = RingBuffer::with_default_capacity();
        // Push 1.5× capacity — should stay at exactly 1MB
        let chunk = vec![0x42u8; 512 * 1024]; // 512KB
        rb.push(&chunk); // 512KB
        rb.push(&chunk); // 1MB total (exact)
        rb.push(&chunk); // 1.5MB — evict oldest 512KB
        assert_eq!(rb.len(), DEFAULT_RING_CAPACITY);
    }

    #[test]
    fn test_single_push_larger_than_capacity_keeps_tail() {
        let mut rb = RingBuffer::new(8);
        // Push 12 bytes into an 8-byte buffer — only last 8 kept
        rb.push(b"XXXXYYYYZZZZ");
        assert_eq!(rb.len(), 8);
        assert_eq!(rb.to_vec(), b"YYYYZZZZ");
    }

    #[test]
    fn test_single_push_exactly_2x_capacity_keeps_last_capacity_bytes() {
        let mut rb = RingBuffer::new(4);
        rb.push(b"AAAABBBB"); // 8 bytes into a 4-byte buffer
        assert_eq!(rb.len(), 4);
        assert_eq!(rb.to_vec(), b"BBBB");
    }

    #[test]
    fn test_incremental_overflow_preserves_order() {
        let mut rb = RingBuffer::new(5);
        for i in 0u8..10 {
            rb.push(&[i]);
        }
        // After 10 single-byte pushes into a 5-byte buffer,
        // last 5 values (5,6,7,8,9) should be present in order.
        assert_eq!(rb.len(), 5);
        assert_eq!(rb.to_vec(), vec![5, 6, 7, 8, 9]);
    }

    // ── Clear ─────────────────────────────────────────────────────────────

    #[test]
    fn test_clear_empties_buffer() {
        let mut rb = RingBuffer::new(64);
        rb.push(b"some data here");
        assert!(!rb.is_empty());
        rb.clear();
        assert!(rb.is_empty());
        assert_eq!(rb.len(), 0);
        assert!(rb.to_vec().is_empty());
    }

    #[test]
    fn test_push_after_clear_works() {
        let mut rb = RingBuffer::new(16);
        rb.push(b"old data");
        rb.clear();
        rb.push(b"new");
        assert_eq!(rb.to_vec(), b"new");
    }

    // ── to_vec fidelity ───────────────────────────────────────────────────

    #[test]
    fn test_to_vec_does_not_consume_buffer() {
        let mut rb = RingBuffer::new(64);
        rb.push(b"hello");
        let v1 = rb.to_vec();
        let v2 = rb.to_vec();
        assert_eq!(v1, v2);
        assert_eq!(rb.len(), 5);
    }

    #[test]
    fn test_binary_data_round_trips() {
        let mut rb = RingBuffer::new(256);
        let data: Vec<u8> = (0u8..=255).collect();
        rb.push(&data);
        assert_eq!(rb.to_vec(), data);
    }

    // ── Replay into vt100::Parser ─────────────────────────────────────────
    //
    // Validates the broker's snapshot-replay path: push raw PTY output into
    // the ring buffer, read it back, and feed it into a fresh vt100::Parser.
    // The parsed screen must contain the expected text.

    #[test]
    fn test_snapshot_replay_into_vt100_parser() {
        let mut rb = RingBuffer::new(DEFAULT_RING_CAPACITY);

        // Simulate a PTY session emitting a prompt and some output
        let pty_output = b"$ ls -la\r\ntotal 8\r\ndrwxr-xr-x  2 user user 4096 Jan 01 00:00 .\r\n";
        rb.push(pty_output);

        // Retrieve snapshot and feed into a fresh parser (as hub does on reconnect)
        let snapshot = rb.to_vec();
        let mut parser = vt100::Parser::new(24, 80, 1000);
        parser.process(&snapshot);

        // Screen should contain visible text from the output
        let screen_contents = parser.screen().contents();
        assert!(
            screen_contents.contains("ls -la") || screen_contents.contains("total 8"),
            "parser screen should contain PTY output; got: {:?}",
            screen_contents
        );
    }
}
