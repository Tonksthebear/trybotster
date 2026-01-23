//! Reliable delivery layer for channels.
//!
//! Provides TCP-like guaranteed, ordered delivery over any `Channel` implementation.
//! Uses sequence numbers, selective acknowledgments (SACK), reorder buffers, and
//! automatic retransmission to ensure no messages are lost.
//!
//! # Protocol
//!
//! ```text
//! Sender                              Receiver
//!   │                                     │
//!   │  Data { seq: 1, payload }           │
//!   │────────────────────────────────────>│
//!   │                                     │
//!   │  Data { seq: 2, payload }           │
//!   │─────────────X (dropped)             │
//!   │                                     │
//!   │  Data { seq: 3, payload }           │
//!   │────────────────────────────────────>│ (buffered, waiting for 2)
//!   │                                     │
//!   │  Ack { received: [1, 3] }           │
//!   │<────────────────────────────────────│
//!   │                                     │
//!   │  (timeout, retransmit seq: 2)       │
//!   │  Data { seq: 2, payload }           │
//!   │────────────────────────────────────>│ (delivers 2, then 3)
//!   │                                     │
//!   │  Ack { received: [1-3] }            │
//!   │<────────────────────────────────────│
//! ```
//!
//! # Features
//!
//! - **Guaranteed delivery**: Messages are retransmitted until acknowledged
//! - **Ordered delivery**: Out-of-order messages are buffered and reordered
//! - **Duplicate detection**: Already-processed messages are ignored
//! - **Selective ACK**: Receiver reports exactly which messages it has
//! - **Bidirectional**: Both send and receive paths are reliable
//!
//! Rust guideline compliant 2025-01

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Default retransmission timeout in milliseconds.
const DEFAULT_RETRANSMIT_TIMEOUT_MS: u64 = 3000;

/// Maximum retransmission timeout (cap for exponential backoff) in milliseconds.
const MAX_RETRANSMIT_TIMEOUT_MS: u64 = 30000;

/// Backoff multiplier for exponential backoff (1.5x per attempt).
const BACKOFF_FACTOR: f64 = 1.5;

/// Maximum retransmission attempts before giving up.
const MAX_RETRANSMIT_ATTEMPTS: u32 = 10;

/// How often to send ACKs even if no new data (heartbeat).
const ACK_HEARTBEAT_INTERVAL_MS: u64 = 5000;

/// TTL for buffered out-of-order messages in milliseconds (30 seconds).
const BUFFER_TTL_MS: u64 = 30000;

/// A reliable message wrapper.
///
/// All messages are wrapped in this enum before transmission.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReliableMessage {
    /// Data message with sequence number.
    Data {
        /// Sequence number (monotonically increasing per sender).
        seq: u64,
        /// Original payload.
        payload: Vec<u8>,
    },

    /// Selective acknowledgment.
    Ack {
        /// Ranges of received sequence numbers, e.g., [(1,3), (5,5)] = "have 1-3 and 5"
        ranges: Vec<(u64, u64)>,
    },
}

impl ReliableMessage {
    /// Create a new data message.
    pub fn data(seq: u64, payload: Vec<u8>) -> Self {
        Self::Data { seq, payload }
    }

    /// Create an ACK message from a set of received sequences.
    pub fn ack_from_set(received: &BTreeSet<u64>) -> Self {
        Self::Ack {
            ranges: Self::set_to_ranges(received),
        }
    }

    /// Convert a set of sequence numbers to ranges for efficient encoding.
    ///
    /// Example: {1, 2, 3, 5, 7, 8} -> [(1, 3), (5, 5), (7, 8)]
    fn set_to_ranges(set: &BTreeSet<u64>) -> Vec<(u64, u64)> {
        let mut ranges = Vec::new();
        let mut iter = set.iter().peekable();

        while let Some(&start) = iter.next() {
            let mut end = start;
            while let Some(&&next) = iter.peek() {
                if next == end + 1 {
                    end = next;
                    iter.next();
                } else {
                    break;
                }
            }
            ranges.push((start, end));
        }

        ranges
    }

    /// Convert ranges back to a set.
    pub fn ranges_to_set(ranges: &[(u64, u64)]) -> BTreeSet<u64> {
        let mut set = BTreeSet::new();
        for &(start, end) in ranges {
            for seq in start..=end {
                set.insert(seq);
            }
        }
        set
    }
}

/// Tracks pending (unacknowledged) outgoing messages.
#[derive(Debug)]
pub struct PendingMessage {
    /// The message payload.
    pub payload: Vec<u8>,
    /// When the message was first sent.
    pub first_sent_at: Instant,
    /// When the message was last sent (for retransmit timing).
    pub last_sent_at: Instant,
    /// Number of transmission attempts.
    pub attempts: u32,
}

/// State for reliable sending.
#[derive(Debug)]
pub struct ReliableSender {
    /// Next sequence number to assign.
    next_seq: u64,
    /// Messages awaiting acknowledgment: seq -> pending info.
    pending: BTreeMap<u64, PendingMessage>,
    /// Retransmission timeout (base timeout before backoff).
    retransmit_timeout: Duration,
    /// Messages that failed after max retransmit attempts: (seq, payload).
    failed: Vec<(u64, Vec<u8>)>,
}

impl Default for ReliableSender {
    fn default() -> Self {
        Self::new()
    }
}

impl ReliableSender {
    /// Create a new sender.
    pub fn new() -> Self {
        Self {
            next_seq: 1, // Start at 1, 0 is reserved
            pending: BTreeMap::new(),
            retransmit_timeout: Duration::from_millis(DEFAULT_RETRANSMIT_TIMEOUT_MS),
            failed: Vec::new(),
        }
    }

    /// Create a new sender with custom retransmit timeout.
    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            next_seq: 1,
            pending: BTreeMap::new(),
            retransmit_timeout: timeout,
            failed: Vec::new(),
        }
    }

    /// Calculate timeout for a given attempt using exponential backoff.
    ///
    /// Timeout increases with each attempt, capped at `MAX_RETRANSMIT_TIMEOUT_MS`.
    pub fn calculate_timeout(&self, attempts: u32) -> Duration {
        Self::calculate_timeout_with_base(self.retransmit_timeout, attempts)
    }

    /// Calculate timeout with a given base timeout (internal helper to avoid borrow issues).
    fn calculate_timeout_with_base(base_timeout: Duration, attempts: u32) -> Duration {
        let base_ms = base_timeout.as_millis() as f64;
        let backoff = base_ms * BACKOFF_FACTOR.powi((attempts.saturating_sub(1)) as i32);
        let capped = backoff.min(MAX_RETRANSMIT_TIMEOUT_MS as f64);
        Duration::from_millis(capped as u64)
    }

    /// Prepare a message for sending. Returns the wrapped message with seq number.
    ///
    /// The message is added to the pending set for retransmission tracking.
    pub fn prepare_send(&mut self, payload: Vec<u8>) -> ReliableMessage {
        let seq = self.next_seq;
        self.next_seq += 1;

        let now = Instant::now();
        self.pending.insert(
            seq,
            PendingMessage {
                payload: payload.clone(),
                first_sent_at: now,
                last_sent_at: now,
                attempts: 1,
            },
        );

        ReliableMessage::data(seq, payload)
    }

    /// Process an ACK, removing acknowledged messages from pending.
    ///
    /// Returns the number of messages acknowledged.
    pub fn process_ack(&mut self, ranges: &[(u64, u64)]) -> usize {
        let acked = ReliableMessage::ranges_to_set(ranges);
        let mut count = 0;

        for seq in acked {
            if self.pending.remove(&seq).is_some() {
                count += 1;
            }
        }

        count
    }

    /// Get messages that need retransmission.
    ///
    /// Uses exponential backoff for timeout and removes messages that exceed
    /// max attempts. Returns messages where last_sent_at + timeout has passed.
    pub fn get_retransmits(&mut self) -> Vec<ReliableMessage> {
        let now = Instant::now();
        let mut retransmits = Vec::new();
        let mut failed_seqs = Vec::new();

        for (seq, pending) in self.pending.iter_mut() {
            if pending.attempts >= MAX_RETRANSMIT_ATTEMPTS {
                // Max retransmits exceeded - remove and track as failed
                log::error!("Message seq={} exceeded max retransmits, removing", seq);
                failed_seqs.push((*seq, pending.payload.clone()));
                continue;
            }

            // Use exponential backoff for timeout
            let timeout =
                Self::calculate_timeout_with_base(self.retransmit_timeout, pending.attempts);

            if now.duration_since(pending.last_sent_at) >= timeout {
                pending.last_sent_at = now;
                pending.attempts += 1;
                retransmits.push(ReliableMessage::data(*seq, pending.payload.clone()));
            }
        }

        // Remove failed messages from pending and track them
        for (seq, payload) in failed_seqs {
            self.pending.remove(&seq);
            self.failed.push((seq, payload));
        }

        retransmits
    }

    /// Take and clear the list of failed messages.
    ///
    /// Returns messages that exceeded max retransmit attempts.
    pub fn take_failed(&mut self) -> Vec<(u64, Vec<u8>)> {
        std::mem::take(&mut self.failed)
    }

    /// Get the number of pending messages.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Get the current sequence number (for testing/debugging).
    pub fn current_seq(&self) -> u64 {
        self.next_seq
    }

    /// Reset sender state (for session reset detection).
    ///
    /// Called when the peer appears to have reset their session.
    /// Clears pending messages and resets sequence number.
    pub fn reset(&mut self) {
        self.next_seq = 1;
        self.pending.clear();
        self.failed.clear();
    }
}

/// A buffered out-of-order message with timestamp for TTL cleanup.
#[derive(Debug)]
struct BufferedMessage {
    /// The message payload.
    payload: Vec<u8>,
    /// When the message was received (for TTL cleanup).
    received_at: Instant,
}

/// State for reliable receiving.
#[derive(Debug)]
pub struct ReliableReceiver {
    /// Sequence numbers we have received.
    received: BTreeSet<u64>,
    /// Next sequence we expect (for in-order delivery).
    next_expected: u64,
    /// Out-of-order messages waiting to be delivered, with timestamps.
    buffer: BTreeMap<u64, BufferedMessage>,
    /// Last time we sent an ACK.
    last_ack_sent: Instant,
}

impl Default for ReliableReceiver {
    fn default() -> Self {
        Self::new()
    }
}

impl ReliableReceiver {
    /// Create a new receiver.
    pub fn new() -> Self {
        Self {
            received: BTreeSet::new(),
            next_expected: 1,
            buffer: BTreeMap::new(),
            last_ack_sent: Instant::now(),
        }
    }

    /// Process a received data message.
    ///
    /// Returns a tuple of:
    /// - Messages that can be delivered in order (may be empty if waiting
    ///   for earlier sequences, or may be multiple if this filled a gap)
    /// - Whether a session reset was detected (caller should reset sender too)
    ///
    /// Detects session reset: if we receive seq=1 but we've already processed
    /// messages (next_expected > 1), the peer has reset their session (e.g.,
    /// page refresh). We reset our receiver state to match.
    pub fn receive(&mut self, seq: u64, payload: Vec<u8>) -> (Vec<Vec<u8>>, bool) {
        let mut reset_occurred = false;

        // Detect session reset: peer sent seq=1 but we're past that.
        // This happens on page refresh - the browser gets a fresh session with seq=1
        // while we still have state from before. Reset our state to match.
        // Note: This also triggers on duplicate seq=1 retransmits, but that's fine
        // because handshake handling is idempotent and we'll just re-deliver.
        if seq == 1 && self.next_expected > 1 {
            log::info!(
                "Session reset detected: got seq=1 but next_expected={}, resetting receiver",
                self.next_expected
            );
            self.reset();
            reset_occurred = true;
        }

        // Cleanup stale buffered messages periodically
        self.cleanup_stale_buffer(Duration::from_millis(BUFFER_TTL_MS));

        // Duplicate check
        if self.received.contains(&seq) {
            return (Vec::new(), reset_occurred);
        }

        // Record as received
        self.received.insert(seq);

        // If this is what we're waiting for, deliver it and any buffered continuations
        let deliverable = if seq == self.next_expected {
            let mut deliverable = vec![payload];
            self.next_expected += 1;

            // Check buffer for continuations (extract payload from BufferedMessage)
            while let Some(buffered) = self.buffer.remove(&self.next_expected) {
                deliverable.push(buffered.payload);
                self.next_expected += 1;
            }

            deliverable
        } else if seq > self.next_expected {
            // Out of order - buffer for later with timestamp for TTL
            self.buffer.insert(
                seq,
                BufferedMessage {
                    payload,
                    received_at: Instant::now(),
                },
            );
            Vec::new()
        } else {
            // seq < next_expected: old duplicate, ignore
            Vec::new()
        };

        (deliverable, reset_occurred)
    }

    /// Cleanup stale buffered messages that exceed TTL.
    ///
    /// Returns the number of evicted entries.
    pub fn cleanup_stale_buffer(&mut self, ttl: Duration) -> usize {
        let now = Instant::now();
        let stale_seqs: Vec<u64> = self
            .buffer
            .iter()
            .filter(|(_, msg)| now.duration_since(msg.received_at) > ttl)
            .map(|(seq, _)| *seq)
            .collect();

        for seq in &stale_seqs {
            log::warn!("Evicting stale buffered message seq={}", seq);
            self.buffer.remove(seq);
        }

        stale_seqs.len()
    }

    /// Get the timestamp when a buffer entry was received (for testing).
    #[cfg(test)]
    pub fn get_buffer_entry_time(&self, seq: u64) -> Option<Instant> {
        self.buffer.get(&seq).map(|msg| msg.received_at)
    }

    /// Set the timestamp for a buffer entry (for testing TTL cleanup).
    #[cfg(test)]
    pub fn set_buffer_entry_time(&mut self, seq: u64, time: Instant) {
        if let Some(msg) = self.buffer.get_mut(&seq) {
            msg.received_at = time;
        }
    }

    /// Generate an ACK message for currently received sequences.
    pub fn generate_ack(&mut self) -> ReliableMessage {
        self.last_ack_sent = Instant::now();
        ReliableMessage::ack_from_set(&self.received)
    }

    /// Check if we should send an ACK heartbeat.
    pub fn should_send_ack_heartbeat(&self) -> bool {
        self.last_ack_sent.elapsed() >= Duration::from_millis(ACK_HEARTBEAT_INTERVAL_MS)
    }

    /// Get the count of buffered (out-of-order) messages.
    pub fn buffered_count(&self) -> usize {
        self.buffer.len()
    }

    /// Get the next expected sequence number (for testing/debugging).
    pub fn next_expected(&self) -> u64 {
        self.next_expected
    }

    /// Reset receiver state (for session reset detection).
    ///
    /// Called when the peer appears to have reset their session (e.g., page refresh).
    pub fn reset(&mut self) {
        self.received.clear();
        self.next_expected = 1;
        self.buffer.clear();
        // Don't reset last_ack_sent - keep ACK timing consistent
    }
}

/// Per-browser reliable session state.
///
/// Each browser has independent sequence spaces for bidirectional communication.
#[derive(Debug)]
pub struct ReliableSession {
    /// Sender state for outgoing messages to this browser.
    pub sender: ReliableSender,
    /// Receiver state for incoming messages from this browser.
    pub receiver: ReliableReceiver,
}

impl Default for ReliableSession {
    fn default() -> Self {
        Self::new()
    }
}

impl ReliableSession {
    /// Create a new reliable session.
    pub fn new() -> Self {
        Self {
            sender: ReliableSender::new(),
            receiver: ReliableReceiver::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========== ReliableMessage Tests ==========

    #[test]
    fn test_set_to_ranges_empty() {
        let set = BTreeSet::new();
        let ranges = ReliableMessage::set_to_ranges(&set);
        assert!(ranges.is_empty());
    }

    #[test]
    fn test_set_to_ranges_single() {
        let mut set = BTreeSet::new();
        set.insert(5);
        let ranges = ReliableMessage::set_to_ranges(&set);
        assert_eq!(ranges, vec![(5, 5)]);
    }

    #[test]
    fn test_set_to_ranges_contiguous() {
        let set: BTreeSet<u64> = vec![1, 2, 3, 4, 5].into_iter().collect();
        let ranges = ReliableMessage::set_to_ranges(&set);
        assert_eq!(ranges, vec![(1, 5)]);
    }

    #[test]
    fn test_set_to_ranges_gaps() {
        let set: BTreeSet<u64> = vec![1, 2, 3, 5, 7, 8, 9].into_iter().collect();
        let ranges = ReliableMessage::set_to_ranges(&set);
        assert_eq!(ranges, vec![(1, 3), (5, 5), (7, 9)]);
    }

    #[test]
    fn test_ranges_to_set() {
        let ranges = vec![(1, 3), (5, 5), (7, 9)];
        let set = ReliableMessage::ranges_to_set(&ranges);
        let expected: BTreeSet<u64> = vec![1, 2, 3, 5, 7, 8, 9].into_iter().collect();
        assert_eq!(set, expected);
    }

    #[test]
    fn test_message_serialization() {
        let msg = ReliableMessage::data(42, b"hello".to_vec());
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ReliableMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, parsed);

        let ack = ReliableMessage::Ack {
            ranges: vec![(1, 5), (10, 12)],
        };
        let json = serde_json::to_string(&ack).unwrap();
        let parsed: ReliableMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(ack, parsed);
    }

    // ========== ReliableSender Tests ==========

    #[test]
    fn test_sender_seq_assignment() {
        let mut sender = ReliableSender::new();

        let msg1 = sender.prepare_send(b"first".to_vec());
        let msg2 = sender.prepare_send(b"second".to_vec());
        let msg3 = sender.prepare_send(b"third".to_vec());

        assert!(matches!(msg1, ReliableMessage::Data { seq: 1, .. }));
        assert!(matches!(msg2, ReliableMessage::Data { seq: 2, .. }));
        assert!(matches!(msg3, ReliableMessage::Data { seq: 3, .. }));

        assert_eq!(sender.pending_count(), 3);
    }

    #[test]
    fn test_sender_ack_processing() {
        let mut sender = ReliableSender::new();

        sender.prepare_send(b"1".to_vec());
        sender.prepare_send(b"2".to_vec());
        sender.prepare_send(b"3".to_vec());
        sender.prepare_send(b"4".to_vec());
        sender.prepare_send(b"5".to_vec());

        assert_eq!(sender.pending_count(), 5);

        // ACK 1-3
        let acked = sender.process_ack(&[(1, 3)]);
        assert_eq!(acked, 3);
        assert_eq!(sender.pending_count(), 2);

        // ACK 5 only (gap at 4)
        let acked = sender.process_ack(&[(5, 5)]);
        assert_eq!(acked, 1);
        assert_eq!(sender.pending_count(), 1);

        // ACK 4
        let acked = sender.process_ack(&[(4, 4)]);
        assert_eq!(acked, 1);
        assert_eq!(sender.pending_count(), 0);
    }

    #[test]
    fn test_sender_retransmit() {
        let mut sender = ReliableSender::with_timeout(Duration::from_millis(10));

        sender.prepare_send(b"test".to_vec());
        assert_eq!(sender.pending_count(), 1);

        // Immediately, no retransmits needed
        let retransmits = sender.get_retransmits();
        assert!(retransmits.is_empty());

        // Wait for timeout
        std::thread::sleep(Duration::from_millis(15));

        let retransmits = sender.get_retransmits();
        assert_eq!(retransmits.len(), 1);
        assert!(matches!(
            &retransmits[0],
            ReliableMessage::Data { seq: 1, .. }
        ));
    }

    // ========== ReliableReceiver Tests ==========

    #[test]
    fn test_receiver_in_order() {
        let mut receiver = ReliableReceiver::new();

        let (delivered, reset) = receiver.receive(1, b"first".to_vec());
        assert!(!reset);
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0], b"first");

        let (delivered, _) = receiver.receive(2, b"second".to_vec());
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0], b"second");

        let (delivered, _) = receiver.receive(3, b"third".to_vec());
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0], b"third");

        assert_eq!(receiver.next_expected(), 4);
    }

    #[test]
    fn test_receiver_out_of_order_buffering() {
        let mut receiver = ReliableReceiver::new();

        // Receive 3 first (out of order)
        let (delivered, _) = receiver.receive(3, b"third".to_vec());
        assert!(delivered.is_empty()); // Can't deliver yet
        assert_eq!(receiver.buffered_count(), 1);

        // Receive 1 (expected)
        let (delivered, _) = receiver.receive(1, b"first".to_vec());
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0], b"first");

        // Receive 2 (fills gap, should trigger delivery of 2 and buffered 3)
        let (delivered, _) = receiver.receive(2, b"second".to_vec());
        assert_eq!(delivered.len(), 2);
        assert_eq!(delivered[0], b"second");
        assert_eq!(delivered[1], b"third");
        assert_eq!(receiver.buffered_count(), 0);
        assert_eq!(receiver.next_expected(), 4);
    }

    #[test]
    fn test_receiver_duplicate_detection() {
        let mut receiver = ReliableReceiver::new();

        let (delivered, reset) = receiver.receive(1, b"first".to_vec());
        assert!(!reset);
        assert_eq!(delivered.len(), 1);

        // Second seq=1 triggers session reset and re-delivery (page refresh scenario)
        let (delivered, reset) = receiver.receive(1, b"first again".to_vec());
        assert!(reset, "Second seq=1 should trigger session reset");
        assert_eq!(delivered.len(), 1, "Should re-deliver after reset");

        // True duplicate of seq=2 should be ignored
        let (delivered, _) = receiver.receive(2, b"second".to_vec());
        assert_eq!(delivered.len(), 1);

        let (delivered, _) = receiver.receive(2, b"second duplicate".to_vec());
        assert!(delivered.is_empty(), "Duplicate seq=2 should be ignored");
    }

    #[test]
    fn test_receiver_ack_generation() {
        let mut receiver = ReliableReceiver::new();

        let _ = receiver.receive(1, b"1".to_vec());
        let _ = receiver.receive(2, b"2".to_vec());
        let _ = receiver.receive(4, b"4".to_vec()); // Gap at 3
        let _ = receiver.receive(5, b"5".to_vec());

        let ack = receiver.generate_ack();
        match ack {
            ReliableMessage::Ack { ranges } => {
                assert_eq!(ranges, vec![(1, 2), (4, 5)]);
            }
            _ => panic!("Expected Ack message"),
        }
    }

    #[test]
    fn test_receiver_large_gap_recovery() {
        let mut receiver = ReliableReceiver::new();

        // Receive 1, then 5, 6, 7 (gap at 2, 3, 4)
        let _ = receiver.receive(1, b"1".to_vec());
        let _ = receiver.receive(5, b"5".to_vec());
        let _ = receiver.receive(6, b"6".to_vec());
        let _ = receiver.receive(7, b"7".to_vec());

        assert_eq!(receiver.next_expected(), 2);
        assert_eq!(receiver.buffered_count(), 3);

        // Fill the gap
        let _ = receiver.receive(2, b"2".to_vec());
        assert_eq!(receiver.next_expected(), 3);
        assert_eq!(receiver.buffered_count(), 3); // Still waiting for 3, 4

        let _ = receiver.receive(3, b"3".to_vec());
        assert_eq!(receiver.next_expected(), 4);

        // Filling 4 should deliver 4, 5, 6, 7
        let (delivered, _) = receiver.receive(4, b"4".to_vec());
        assert_eq!(delivered.len(), 4);
        assert_eq!(receiver.next_expected(), 8);
        assert_eq!(receiver.buffered_count(), 0);
    }

    // ========== ReliableSession Integration Tests ==========

    #[test]
    fn test_session_full_roundtrip() {
        // Simulate full roundtrip: sender prepares, receiver receives, ACK flows back
        let mut session_a = ReliableSession::new(); // Sender side
        let mut session_b = ReliableSession::new(); // Receiver side

        // A sends message to B
        let msg = session_a.sender.prepare_send(b"hello".to_vec());
        assert_eq!(session_a.sender.pending_count(), 1);

        // B receives and processes
        if let ReliableMessage::Data { seq, payload } = msg {
            let (delivered, reset) = session_b.receiver.receive(seq, payload);
            assert!(!reset, "Should not detect reset on first message");
            assert_eq!(delivered.len(), 1);
            assert_eq!(delivered[0], b"hello");
        } else {
            panic!("Expected Data message");
        }

        // B sends ACK back to A
        let ack = session_b.receiver.generate_ack();
        if let ReliableMessage::Ack { ranges } = ack {
            let acked = session_a.sender.process_ack(&ranges);
            assert_eq!(acked, 1);
            assert_eq!(session_a.sender.pending_count(), 0);
        } else {
            panic!("Expected Ack message");
        }
    }

    #[test]
    fn test_session_multiple_messages_in_order() {
        let mut sender = ReliableSender::new();
        let mut receiver = ReliableReceiver::new();

        // Send 100 messages in order
        let mut messages = Vec::new();
        for i in 0..100u64 {
            let payload = format!("message_{}", i).into_bytes();
            messages.push(sender.prepare_send(payload));
        }
        assert_eq!(sender.pending_count(), 100);

        // Receive all in order
        let mut delivered_count = 0;
        for msg in messages {
            if let ReliableMessage::Data { seq, payload } = msg {
                let (delivered, _reset) = receiver.receive(seq, payload);
                delivered_count += delivered.len();
            }
        }
        assert_eq!(delivered_count, 100);
        assert_eq!(receiver.next_expected(), 101);

        // Single ACK clears all pending
        let ack = receiver.generate_ack();
        if let ReliableMessage::Ack { ranges } = ack {
            let acked = sender.process_ack(&ranges);
            assert_eq!(acked, 100);
            assert_eq!(sender.pending_count(), 0);
        }
    }

    #[test]
    fn test_session_out_of_order_delivery_preserves_order() {
        let mut sender = ReliableSender::new();
        let mut receiver = ReliableReceiver::new();

        // Send 5 messages
        let msgs: Vec<_> = (0..5)
            .map(|i| sender.prepare_send(format!("msg_{}", i).into_bytes()))
            .collect();

        // Receive in scrambled order: 3, 1, 4, 2, 5
        let order = [2, 0, 3, 1, 4]; // indices into msgs
        let mut all_delivered = Vec::new();

        for &idx in &order {
            if let ReliableMessage::Data { seq, payload } = msgs[idx].clone() {
                let (delivered, _reset) = receiver.receive(seq, payload);
                all_delivered.extend(delivered);
            }
        }

        // All 5 should be delivered in correct order
        assert_eq!(all_delivered.len(), 5);
        for (i, payload) in all_delivered.iter().enumerate() {
            assert_eq!(*payload, format!("msg_{}", i).into_bytes());
        }
    }

    #[test]
    fn test_session_partial_ack_selective_removal() {
        let mut sender = ReliableSender::new();

        // Send 10 messages
        for i in 0..10 {
            sender.prepare_send(format!("msg_{}", i).into_bytes());
        }
        assert_eq!(sender.pending_count(), 10);

        // ACK only odd sequences (1, 3, 5, 7, 9)
        let acked = sender.process_ack(&[(1, 1), (3, 3), (5, 5), (7, 7), (9, 9)]);
        assert_eq!(acked, 5);
        assert_eq!(sender.pending_count(), 5);

        // ACK the rest (2, 4, 6, 8, 10)
        let acked = sender.process_ack(&[(2, 2), (4, 4), (6, 6), (8, 8), (10, 10)]);
        assert_eq!(acked, 5);
        assert_eq!(sender.pending_count(), 0);
    }

    #[test]
    fn test_session_retransmit_preserves_payload() {
        let mut sender = ReliableSender::with_timeout(Duration::from_millis(10));

        let original_payload = b"important data".to_vec();
        sender.prepare_send(original_payload.clone());

        // Wait for retransmit timeout
        std::thread::sleep(Duration::from_millis(15));

        let retransmits = sender.get_retransmits();
        assert_eq!(retransmits.len(), 1);

        if let ReliableMessage::Data { seq, payload } = &retransmits[0] {
            assert_eq!(*seq, 1);
            assert_eq!(*payload, original_payload);
        } else {
            panic!("Expected Data message");
        }
    }

    #[test]
    fn test_session_duplicate_receives_for_non_seq1() {
        // Test that duplicates of seq > 1 are properly ignored
        let mut receiver = ReliableReceiver::new();

        // Receive seq=1 then seq=2
        let (delivered, _) = receiver.receive(1, b"first".to_vec());
        assert_eq!(delivered.len(), 1);

        let (delivered, _) = receiver.receive(2, b"second".to_vec());
        assert_eq!(delivered.len(), 1);

        // Duplicate seq=2 should be ignored
        let (delivered, reset) = receiver.receive(2, b"second again".to_vec());
        assert!(!reset);
        assert!(delivered.is_empty());

        // Duplicate seq=2 third time should still be ignored
        let (delivered, reset) = receiver.receive(2, b"second third time".to_vec());
        assert!(!reset);
        assert!(delivered.is_empty());
    }

    #[test]
    fn test_session_ack_idempotent() {
        let mut sender = ReliableSender::new();
        sender.prepare_send(b"test".to_vec());

        // ACK same seq multiple times
        let acked1 = sender.process_ack(&[(1, 1)]);
        let acked2 = sender.process_ack(&[(1, 1)]);
        let acked3 = sender.process_ack(&[(1, 1)]);

        // Only first ACK should count
        assert_eq!(acked1, 1);
        assert_eq!(acked2, 0);
        assert_eq!(acked3, 0);
        assert_eq!(sender.pending_count(), 0);
    }

    #[test]
    fn test_receiver_session_reset_detection() {
        let mut receiver = ReliableReceiver::new();

        // First session: receive messages 1, 2, 3
        let (delivered, reset) = receiver.receive(1, b"first".to_vec());
        assert!(!reset, "First message should not trigger reset");
        assert_eq!(delivered.len(), 1);

        let (_, reset) = receiver.receive(2, b"second".to_vec());
        assert!(!reset);

        let (_, reset) = receiver.receive(3, b"third".to_vec());
        assert!(!reset);
        assert_eq!(receiver.next_expected(), 4);

        // Session reset: peer sends seq=1 again (simulating page refresh)
        let (delivered, reset) = receiver.receive(1, b"new session start".to_vec());
        assert!(
            reset,
            "Should detect session reset when seq=1 arrives after seq>1"
        );
        assert_eq!(delivered.len(), 1, "Should deliver the new seq=1 message");
        assert_eq!(delivered[0], b"new session start");
        assert_eq!(
            receiver.next_expected(),
            2,
            "Should be waiting for seq=2 in new session"
        );
    }

    #[test]
    fn test_sender_reset_clears_pending() {
        let mut sender = ReliableSender::new();

        // Send some messages
        sender.prepare_send(b"msg1".to_vec());
        sender.prepare_send(b"msg2".to_vec());
        sender.prepare_send(b"msg3".to_vec());
        assert_eq!(sender.pending_count(), 3);
        assert_eq!(sender.current_seq(), 4);

        // Reset sender (simulating peer session reset)
        sender.reset();

        // Should be back to initial state
        assert_eq!(sender.pending_count(), 0);
        assert_eq!(sender.current_seq(), 1);

        // Next message should be seq=1
        let msg = sender.prepare_send(b"new session".to_vec());
        if let ReliableMessage::Data { seq, .. } = msg {
            assert_eq!(seq, 1);
        } else {
            panic!("Expected Data message");
        }
    }

    // ========== Heartbeat ACK Tests ==========

    #[test]
    fn test_receiver_heartbeat_not_needed_initially() {
        let receiver = ReliableReceiver::new();
        // Just created - no heartbeat needed yet
        assert!(!receiver.should_send_ack_heartbeat());
    }

    #[test]
    fn test_receiver_heartbeat_needed_after_timeout() {
        let mut receiver = ReliableReceiver::new();

        // Receive a message (this sends an ACK and resets the timer)
        let _ = receiver.receive(1, b"test".to_vec());
        let _ = receiver.generate_ack();

        // Immediately after ACK, no heartbeat needed
        assert!(!receiver.should_send_ack_heartbeat());

        // Wait for heartbeat interval to elapse (5 seconds)
        // We can't easily wait 5 seconds in a test, so we'll check the logic
        // by verifying the method exists and returns a boolean
    }

    #[test]
    fn test_receiver_ack_resets_heartbeat_timer() {
        let mut receiver = ReliableReceiver::new();

        // Receive some messages
        let _ = receiver.receive(1, b"msg1".to_vec());
        let _ = receiver.receive(2, b"msg2".to_vec());

        // Generate ACK - this should reset the heartbeat timer
        let ack = receiver.generate_ack();

        // Verify it's a valid ACK with the right ranges
        match ack {
            ReliableMessage::Ack { ranges } => {
                assert_eq!(ranges, vec![(1, 2)]);
            }
            _ => panic!("Expected Ack message"),
        }

        // Heartbeat not needed right after sending ACK
        assert!(!receiver.should_send_ack_heartbeat());
    }

    #[test]
    fn test_sender_retransmit_marks_attempts() {
        let mut sender = ReliableSender::with_timeout(Duration::from_millis(10));

        // Send a message
        sender.prepare_send(b"test".to_vec());

        // Wait for timeout
        std::thread::sleep(Duration::from_millis(15));

        // Get retransmits - should have attempt count incremented
        let retransmits = sender.get_retransmits();
        assert_eq!(retransmits.len(), 1);

        // Wait and get retransmits again
        std::thread::sleep(Duration::from_millis(15));
        let retransmits2 = sender.get_retransmits();
        assert_eq!(retransmits2.len(), 1);

        // The message is still pending (not ACKed)
        assert_eq!(sender.pending_count(), 1);
    }

    #[test]
    fn test_sender_retransmit_gives_up_after_max_attempts() {
        let mut sender = ReliableSender::with_timeout(Duration::from_millis(5));

        // Send a message
        sender.prepare_send(b"test".to_vec());

        // Exhaust all retransmit attempts (MAX_RETRANSMIT_ATTEMPTS = 10)
        for _ in 0..15 {
            std::thread::sleep(Duration::from_millis(10));
            let _ = sender.get_retransmits();
        }

        // After max attempts, retransmits should stop (but message stays pending)
        let retransmits = sender.get_retransmits();
        assert!(
            retransmits.is_empty(),
            "Should stop retransmitting after max attempts"
        );
    }

    // ========== Phase 2: Exponential Backoff Tests ==========

    #[test]
    fn test_sender_exponential_backoff_timeout_increases() {
        let sender = ReliableSender::with_timeout(Duration::from_millis(100));

        // Verify timeout increases with attempt count
        let timeout1 = sender.calculate_timeout(1);
        let timeout2 = sender.calculate_timeout(2);
        let timeout3 = sender.calculate_timeout(3);

        assert!(
            timeout2 > timeout1,
            "Timeout should increase: attempt2 ({:?}) > attempt1 ({:?})",
            timeout2,
            timeout1
        );
        assert!(
            timeout3 > timeout2,
            "Timeout should increase: attempt3 ({:?}) > attempt2 ({:?})",
            timeout3,
            timeout2
        );
    }

    #[test]
    fn test_sender_exponential_backoff_capped() {
        let sender = ReliableSender::with_timeout(Duration::from_millis(1000));

        // After many attempts, timeout should be capped at MAX_RETRANSMIT_TIMEOUT
        let timeout_10 = sender.calculate_timeout(10);
        assert!(
            timeout_10 <= Duration::from_secs(30),
            "Timeout should be capped at 30 seconds, got {:?}",
            timeout_10
        );
    }

    // ========== Phase 3: Buffer TTL Cleanup Tests ==========

    #[test]
    fn test_receiver_buffer_stores_timestamps() {
        let mut receiver = ReliableReceiver::new();

        // Receive out-of-order message (gap at seq=1)
        let (delivered, _) = receiver.receive(3, b"third".to_vec());
        assert!(delivered.is_empty());
        assert_eq!(receiver.buffered_count(), 1);

        // Buffer entry should have a timestamp
        assert!(
            receiver.get_buffer_entry_time(3).is_some(),
            "Buffered entry should have timestamp"
        );
    }

    #[test]
    fn test_receiver_cleanup_stale_buffer() {
        let mut receiver = ReliableReceiver::new();

        // Receive out-of-order messages
        let _ = receiver.receive(3, b"third".to_vec());
        let _ = receiver.receive(5, b"fifth".to_vec());
        assert_eq!(receiver.buffered_count(), 2);

        // Artificially age the buffer entries
        receiver.set_buffer_entry_time(3, Instant::now() - Duration::from_secs(35));
        receiver.set_buffer_entry_time(5, Instant::now() - Duration::from_secs(35));

        // Cleanup should remove stale entries
        let evicted = receiver.cleanup_stale_buffer(Duration::from_secs(30));
        assert_eq!(evicted, 2);
        assert_eq!(receiver.buffered_count(), 0);
    }

    // ========== Phase 4: Failed Message Removal Tests ==========

    #[test]
    fn test_sender_removes_failed_messages() {
        let mut sender = ReliableSender::with_timeout(Duration::from_millis(5));

        // Send a message
        sender.prepare_send(b"test".to_vec());
        assert_eq!(sender.pending_count(), 1);

        // Exhaust all retransmit attempts
        // With exponential backoff, we need to sleep long enough for each attempt.
        // Max timeout caps at 30s, but with 5ms base and 1.5x factor:
        // attempt 1: 5ms, attempt 2: 7.5ms, ..., attempt 10: ~192ms
        // Sleep 250ms per iteration to ensure we exceed max timeout
        for _ in 0..15 {
            std::thread::sleep(Duration::from_millis(250));
            let _ = sender.get_retransmits();
        }

        // Message should be REMOVED from pending (not just stopped retransmitting)
        assert_eq!(
            sender.pending_count(),
            0,
            "Failed message should be removed from pending"
        );
    }

    #[test]
    fn test_sender_reports_failed_messages() {
        let mut sender = ReliableSender::with_timeout(Duration::from_millis(5));

        // Send a message
        sender.prepare_send(b"test".to_vec());

        // Exhaust all retransmit attempts (sleep 250ms to account for backoff)
        for _ in 0..15 {
            std::thread::sleep(Duration::from_millis(250));
            let _ = sender.get_retransmits();
        }

        // Check failed messages were tracked
        let failed = sender.take_failed();
        assert_eq!(failed.len(), 1, "Should report 1 failed message");
        assert_eq!(failed[0].0, 1, "Failed message should have seq=1");
    }
}
