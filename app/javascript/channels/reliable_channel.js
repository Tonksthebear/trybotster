/**
 * Reliable delivery components for ActionCable channels.
 *
 * Provides TCP-like guaranteed, ordered delivery over ActionCable.
 * Uses sequence numbers, selective acknowledgments (SACK), reorder buffers,
 * and automatic retransmission to ensure no messages are lost.
 *
 * Protocol:
 * - Sender assigns monotonically increasing sequence numbers to each message
 * - Receiver buffers out-of-order messages and delivers in sequence
 * - Receiver sends SACK (selective ACK) with ranges of received sequences
 * - Sender retransmits unacked messages after timeout
 *
 * These components are used internally by Channel (see channel.js) when
 * reliability is enabled via the builder pattern:
 *
 *   import { Channel } from './channel.js';
 *
 *   const channel = Channel.builder(subscription)
 *     .session(signalSession)
 *     .reliable(true)
 *     .onMessage((payload) => handleMessage(payload))
 *     .build();
 *
 *   channel.send(payload);
 */

// Default retransmission timeout in milliseconds
const DEFAULT_RETRANSMIT_TIMEOUT_MS = 3000;

// Maximum retransmission timeout (cap for exponential backoff)
const MAX_RETRANSMIT_TIMEOUT_MS = 30000;

// Backoff multiplier for exponential backoff
const BACKOFF_FACTOR = 1.5;

// Maximum retransmission attempts before giving up
const MAX_RETRANSMIT_ATTEMPTS = 10;

// How often to send ACK heartbeat even if no new data
const ACK_HEARTBEAT_INTERVAL_MS = 5000;

// TTL for buffered out-of-order messages (30 seconds)
const BUFFER_TTL_MS = 30000;

/**
 * Convert a Set of sequence numbers to ranges for efficient encoding.
 * Example: Set{1, 2, 3, 5, 7, 8} -> [[1, 3], [5, 5], [7, 8]]
 */
function setToRanges(set) {
  const sorted = Array.from(set).sort((a, b) => a - b);
  const ranges = [];
  let i = 0;

  while (i < sorted.length) {
    const start = sorted[i];
    let end = start;

    while (i + 1 < sorted.length && sorted[i + 1] === end + 1) {
      i++;
      end = sorted[i];
    }

    ranges.push([start, end]);
    i++;
  }

  return ranges;
}

/**
 * Convert ranges back to a Set.
 * Example: [[1, 3], [5, 5]] -> Set{1, 2, 3, 5}
 */
function rangesToSet(ranges) {
  const set = new Set();
  for (const [start, end] of ranges) {
    for (let seq = start; seq <= end; seq++) {
      set.add(seq);
    }
  }
  return set;
}

/**
 * Reliable sender state.
 * Tracks pending (unacked) messages and handles retransmission.
 */
export class ReliableSender {
  constructor(options = {}) {
    this.nextSeq = 1; // Start at 1, 0 is reserved
    this.pending = new Map(); // seq -> { payload, encryptedEnvelope, firstSentAt, lastSentAt, attempts }
    this.retransmitTimeout =
      options.retransmitTimeout || DEFAULT_RETRANSMIT_TIMEOUT_MS;
    // onSend: encrypts and sends, returns encrypted envelope for caching
    this.onSend = options.onSend || (async () => null);
    // onRetransmit: sends pre-encrypted envelope (no re-encryption)
    this.onRetransmit = options.onRetransmit || (() => {});
    this.onMessageFailed = options.onMessageFailed || (() => {}); // Callback for failed messages
    this.retransmitTimer = null;
    this.paused = false; // Connection-aware: pause retransmits when disconnected
  }

  /**
   * Reset sender state (e.g., when peer session resets).
   */
  reset() {
    this.nextSeq = 1;
    this.pending.clear();
    if (this.retransmitTimer) {
      clearTimeout(this.retransmitTimer);
      this.retransmitTimer = null;
    }
    this.paused = false;
  }

  /**
   * Calculate timeout for a given attempt using exponential backoff.
   * Timeout increases with each attempt, capped at MAX_RETRANSMIT_TIMEOUT_MS.
   */
  calculateTimeout(attempts) {
    const base = this.retransmitTimeout;
    const backoff = base * Math.pow(BACKOFF_FACTOR, attempts - 1);
    return Math.min(backoff, MAX_RETRANSMIT_TIMEOUT_MS);
  }

  /**
   * Pause retransmission (call when connection lost).
   */
  pause() {
    this.paused = true;
    if (this.retransmitTimer) {
      clearTimeout(this.retransmitTimer);
      this.retransmitTimer = null;
    }
  }

  /**
   * Resume retransmission (call when connection restored).
   */
  resume() {
    this.paused = false;
    this.scheduleRetransmit();
  }

  /**
   * Prepare and send a message with reliability.
   * Returns the assigned sequence number.
   *
   * The payload is serialized to JSON bytes (array of numbers) to match
   * the Rust protocol which expects `payload: Vec<u8>`.
   *
   * IMPORTANT: onSend is expected to encrypt and return the encrypted envelope.
   * We cache this envelope for retransmission to avoid re-encrypting (which would
   * advance Signal counters and cause decryption failures on the receiver).
   */
  async send(payload) {
    const seq = this.nextSeq++;
    const now = Date.now();

    // Serialize payload to JSON bytes (matches Rust Vec<u8> format)
    const payloadBytes = Array.from(
      new TextEncoder().encode(JSON.stringify(payload)),
    );

    const message = {
      type: "data",
      seq,
      payload: payloadBytes,
    };

    // Encrypt and send - onSend returns the encrypted envelope for caching
    const encryptedEnvelope = await this.onSend(message);

    this.pending.set(seq, {
      payloadBytes, // Keep for debugging
      encryptedEnvelope, // Cache encrypted form for retransmit
      firstSentAt: now,
      lastSentAt: now,
      attempts: 1,
    });

    this.scheduleRetransmit();

    return seq;
  }

  /**
   * Process an ACK message, removing acknowledged sequences from pending.
   * Returns the number of messages acknowledged.
   */
  processAck(ranges) {
    const acked = rangesToSet(ranges);
    let count = 0;

    for (const seq of acked) {
      if (this.pending.has(seq)) {
        this.pending.delete(seq);
        count++;
      }
    }

    // If nothing pending, stop retransmit timer
    if (this.pending.size === 0 && this.retransmitTimer) {
      clearTimeout(this.retransmitTimer);
      this.retransmitTimer = null;
    }

    return count;
  }

  /**
   * Get messages that need retransmission.
   * Uses exponential backoff and removes messages that exceed max attempts.
   * Returns cached encrypted envelopes (not plaintext) to avoid re-encryption.
   */
  getRetransmits() {
    const now = Date.now();
    const retransmits = [];
    const failedSeqs = [];

    for (const [seq, entry] of this.pending) {
      if (entry.attempts >= MAX_RETRANSMIT_ATTEMPTS) {
        console.error(
          `[Reliable] Message seq=${seq} exceeded max retransmits, removing`,
        );
        failedSeqs.push(seq);
        this.onMessageFailed(seq, entry.payloadBytes);
        continue;
      }

      // Use exponential backoff for timeout
      const timeout = this.calculateTimeout(entry.attempts);

      if (now - entry.lastSentAt >= timeout) {
        entry.lastSentAt = now;
        entry.attempts++;
        // Return the cached encrypted envelope for retransmission
        retransmits.push({
          seq,
          encryptedEnvelope: entry.encryptedEnvelope,
        });
      }
    }

    // Remove failed messages from pending
    for (const seq of failedSeqs) {
      this.pending.delete(seq);
    }

    return retransmits;
  }

  /**
   * Schedule retransmission check.
   * Respects paused state for connection-aware retransmission.
   */
  scheduleRetransmit() {
    if (this.paused) return; // Don't schedule if paused
    if (this.retransmitTimer) return;
    if (this.pending.size === 0) return;

    this.retransmitTimer = setTimeout(() => {
      this.retransmitTimer = null;

      // Don't process if paused (could have been paused while timer was pending)
      if (this.paused) return;

      const retransmits = this.getRetransmits();
      for (const { seq, encryptedEnvelope } of retransmits) {
        // Use onRetransmit for pre-encrypted envelopes (no re-encryption)
        this.onRetransmit(encryptedEnvelope);
      }

      // Reschedule if still have pending
      if (this.pending.size > 0) {
        this.scheduleRetransmit();
      }
    }, this.retransmitTimeout);
  }

  /**
   * Clean up timers.
   */
  destroy() {
    if (this.retransmitTimer) {
      clearTimeout(this.retransmitTimer);
      this.retransmitTimer = null;
    }
  }
}

/**
 * Reliable receiver state.
 * Buffers out-of-order messages and delivers in sequence.
 */
export class ReliableReceiver {
  constructor(options = {}) {
    this.received = new Set(); // Sequence numbers we have received
    this.nextExpected = 1; // Next sequence we expect for in-order delivery
    this.buffer = new Map(); // seq -> { payload, receivedAt } (out-of-order messages)
    this.lastAckSent = Date.now();
    this.onDeliver = options.onDeliver || (() => {}); // Callback for delivered messages
    this.onAck = options.onAck || (() => {}); // Callback to send ACK
    this.onReset = options.onReset || (() => {}); // Callback when peer session reset detected
    this.ackTimer = null;
  }

  /**
   * Reset receiver state (e.g., when peer session resets).
   */
  reset() {
    this.received.clear();
    this.nextExpected = 1;
    this.buffer.clear();
  }

  /**
   * Cleanup stale buffered messages that exceed TTL.
   * Returns number of evicted entries.
   */
  cleanupStaleBuffer() {
    const now = Date.now();
    const staleThreshold = now - BUFFER_TTL_MS;
    let evicted = 0;

    for (const [seq, entry] of this.buffer) {
      if (entry.receivedAt < staleThreshold) {
        this.buffer.delete(seq);
        evicted++;
      }
    }

    return evicted;
  }

  /**
   * Process a received data message.
   * Returns array of payloads that can be delivered in order.
   *
   * The payloadBytes parameter is expected to be an array of numbers (byte array)
   * matching the Rust Vec<u8> format. It's deserialized back to a JSON object.
   *
   * Note: This method is async because decompression may be required for gzip payloads.
   */
  async receive(seq, payloadBytes) {
    // Session reset detection: if we receive seq=1 when we expected higher,
    // the peer has restarted their reliable channel. Reset only our receiver
    // to accept their new sequence numbers, but do NOT reset our sender.
    // Resetting sender would cause Signal counter desync (sender uses counters
    // that peer has already seen).
    if (seq === 1 && this.nextExpected > 1) {
      this.reset();
      // Note: NOT calling onReset() - sender keeps its sequence numbers
    }

    // Cleanup stale buffered messages periodically
    this.cleanupStaleBuffer();

    // Duplicate check with reset detection
    if (this.received.has(seq)) {
      // If seq is low (< 10) and we've advanced well past it, peer likely reset
      // True duplicates from retransmission would match recent sequences
      if (seq < 10 && this.nextExpected > seq + 5) {
        this.reset();
        // Note: NOT calling onReset() - sender keeps its sequence numbers
        // Continue processing this message after reset
      } else {
        return [];
      }
    }

    // Record as received
    this.received.add(seq);

    // Schedule ACK
    this.scheduleAck();

    // Deserialize payload bytes to JSON object (may be async if gzip compressed)
    let payload = this.deserializePayload(payloadBytes);
    if (payload instanceof Promise) {
      payload = await payload;
    }
    if (payload === null) {
      console.error(`[Reliable] Failed to deserialize payload for seq=${seq}`);
      return [];
    }

    // If this is what we're waiting for, deliver it and any buffered continuations
    if (seq === this.nextExpected) {
      const deliverable = [payload];
      this.nextExpected++;

      // Check buffer for continuations (extract payload from { payload, receivedAt })
      while (this.buffer.has(this.nextExpected)) {
        const entry = this.buffer.get(this.nextExpected);
        deliverable.push(entry.payload);
        this.buffer.delete(this.nextExpected);
        this.nextExpected++;
      }

      // Deliver all
      for (const p of deliverable) {
        this.onDeliver(p);
      }

      return deliverable;
    } else if (seq > this.nextExpected) {
      // Out of order - buffer for later with timestamp for TTL
      this.buffer.set(seq, { payload, receivedAt: Date.now() });
      return [];
    } else {
      // seq < nextExpected: old duplicate, ignore
      return [];
    }
  }

  /**
   * Deserialize payload bytes (array of numbers) to a JSON object.
   * Handles the CLI's compression marker format:
   * - 0x00: uncompressed (strip marker, parse as JSON)
   * - 0x1f: gzip compressed (strip marker, decompress, parse as JSON)
   * - other: raw JSON (backwards compatibility)
   * Returns null if deserialization fails.
   */
  deserializePayload(payloadBytes) {
    try {
      // Handle both array of numbers and Uint8Array
      const bytes = Array.isArray(payloadBytes)
        ? new Uint8Array(payloadBytes)
        : payloadBytes;

      if (bytes.length === 0) {
        console.error("[Reliable] Empty payload");
        return null;
      }

      const marker = bytes[0];

      // Check for CLI compression markers (must match compression.rs)
      if (marker === 0x00) {
        // Uncompressed - strip marker byte, parse as JSON
        const jsonBytes = bytes.slice(1);
        const jsonString = new TextDecoder().decode(jsonBytes);
        return JSON.parse(jsonString);
      } else if (marker === 0x1f) {
        // Gzip compressed - strip marker, decompress
        const compressedBytes = bytes.slice(1);
        return this.decompressAndParse(compressedBytes);
      } else {
        // No recognized marker - treat as raw JSON (backwards compatibility)
        const jsonString = new TextDecoder().decode(bytes);
        return JSON.parse(jsonString);
      }
    } catch (error) {
      console.error("[Reliable] Payload deserialization error:", error);
      return null;
    }
  }

  /**
   * Decompress gzip bytes and parse as JSON.
   * Uses browser's native DecompressionStream API.
   */
  async decompressAndParse(bytes) {
    try {
      const stream = new Blob([bytes])
        .stream()
        .pipeThrough(new DecompressionStream("gzip"));
      const decompressed = await new Response(stream).text();
      return JSON.parse(decompressed);
    } catch (error) {
      console.error("[Reliable] Decompression error:", error);
      return null;
    }
  }

  /**
   * Generate an ACK message for currently received sequences.
   */
  generateAck() {
    this.lastAckSent = Date.now();
    return {
      type: "ack",
      ranges: setToRanges(this.received),
    };
  }

  /**
   * Schedule ACK to be sent (batches multiple receives).
   */
  scheduleAck() {
    if (this.ackTimer) return;

    // Send ACK after short delay to batch
    this.ackTimer = setTimeout(() => {
      this.ackTimer = null;
      const ack = this.generateAck();
      this.onAck(ack);
    }, 50); // 50ms batching delay
  }

  /**
   * Check if we should send ACK heartbeat.
   */
  shouldSendAckHeartbeat() {
    return Date.now() - this.lastAckSent >= ACK_HEARTBEAT_INTERVAL_MS;
  }

  /**
   * Clean up timers.
   */
  destroy() {
    if (this.ackTimer) {
      clearTimeout(this.ackTimer);
      this.ackTimer = null;
    }
  }
}

// Export utilities for testing
export { setToRanges, rangesToSet };
