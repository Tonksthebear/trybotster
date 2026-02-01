/**
 * Reliable delivery components for ActionCable channels.
 *
 * Provides TCP-like guaranteed, ordered delivery over ActionCable.
 * Uses sequence numbers, selective acknowledgments (SACK), reorder buffers,
 * and automatic retransmission to ensure no messages are lost.
 *
 * Binary Wire Protocol:
 * - Data: [0x01][seq: 8B LE][payload bytes...]
 * - Ack:  [0x02][count: 2B LE][ranges: (start: 8B LE, end: 8B LE)...]
 *
 * Data overhead: 9 bytes (vs ~40+ bytes for JSON)
 * Ack overhead: 3 + 16*N bytes (vs ~20+ bytes per range for JSON)
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

// Binary message type markers (must match Rust)
const MSG_TYPE_DATA = 0x01;
const MSG_TYPE_ACK = 0x02;

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

// Window size for duplicate detection (prevents unbounded growth of received set)
const DUPLICATE_WINDOW = 1000;

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

// =============================================================================
// Binary Encoding/Decoding
// =============================================================================

/**
 * Encode a reliable message to binary format.
 *
 * @param {string} type - "data" or "ack"
 * @param {Object} msg - { seq, payload } for data, { ranges } for ack
 * @returns {Uint8Array} - Binary encoded message
 */
function encodeReliableMessage(type, msg) {
  if (type === "data") {
    const { seq, payload } = msg;
    // payload is already Uint8Array
    const buf = new Uint8Array(1 + 8 + payload.length);
    const view = new DataView(buf.buffer);

    buf[0] = MSG_TYPE_DATA;
    // Write seq as 64-bit LE (split into two 32-bit writes for browser compat)
    view.setUint32(1, seq & 0xffffffff, true); // low 32 bits
    view.setUint32(5, Math.floor(seq / 0x100000000), true); // high 32 bits
    buf.set(payload, 9);

    return buf;
  } else if (type === "ack") {
    const { ranges } = msg;
    const count = Math.min(ranges.length, 0xffff);
    const buf = new Uint8Array(1 + 2 + count * 16);
    const view = new DataView(buf.buffer);

    buf[0] = MSG_TYPE_ACK;
    view.setUint16(1, count, true);

    for (let i = 0; i < count; i++) {
      const [start, end] = ranges[i];
      const offset = 3 + i * 16;
      // Write start as 64-bit LE
      view.setUint32(offset, start & 0xffffffff, true);
      view.setUint32(offset + 4, Math.floor(start / 0x100000000), true);
      // Write end as 64-bit LE
      view.setUint32(offset + 8, end & 0xffffffff, true);
      view.setUint32(offset + 12, Math.floor(end / 0x100000000), true);
    }

    return buf;
  }
  throw new Error(`Unknown message type: ${type}`);
}

/**
 * Decode a binary reliable message.
 *
 * @param {Uint8Array} bytes - Binary encoded message
 * @returns {{ type: string, seq?: number, payload?: Uint8Array, ranges?: Array }}
 */
function decodeReliableMessage(bytes) {
  if (bytes.length === 0) {
    throw new Error("Empty message");
  }

  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);

  switch (bytes[0]) {
    case MSG_TYPE_DATA: {
      if (bytes.length < 9) {
        throw new Error(`Data message too short: ${bytes.length} bytes`);
      }
      // Read seq as 64-bit LE (combine two 32-bit reads)
      const seqLow = view.getUint32(1, true);
      const seqHigh = view.getUint32(5, true);
      const seq = seqLow + seqHigh * 0x100000000;
      const payload = bytes.slice(9);
      return { type: "data", seq, payload };
    }
    case MSG_TYPE_ACK: {
      if (bytes.length < 3) {
        throw new Error(`Ack message too short: ${bytes.length} bytes`);
      }
      const count = view.getUint16(1, true);
      const expectedLen = 3 + count * 16;
      if (bytes.length < expectedLen) {
        throw new Error(
          `Ack message truncated: ${bytes.length} bytes, expected ${expectedLen}`,
        );
      }
      const ranges = [];
      for (let i = 0; i < count; i++) {
        const offset = 3 + i * 16;
        const startLow = view.getUint32(offset, true);
        const startHigh = view.getUint32(offset + 4, true);
        const endLow = view.getUint32(offset + 8, true);
        const endHigh = view.getUint32(offset + 12, true);
        const start = startLow + startHigh * 0x100000000;
        const end = endLow + endHigh * 0x100000000;
        ranges.push([start, end]);
      }
      return { type: "ack", ranges };
    }
    default:
      throw new Error(`Unknown message type: 0x${bytes[0].toString(16)}`);
  }
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
   * The payload is serialized to JSON, then encoded as binary with the
   * reliable message header for minimal wire overhead.
   *
   * IMPORTANT: onSend is expected to encrypt and return the encrypted envelope.
   * We cache this envelope for retransmission to avoid re-encrypting (which would
   * advance Signal counters and cause decryption failures on the receiver).
   */
  async send(payload) {
    const seq = this.nextSeq++;
    const now = Date.now();

    // Serialize payload to JSON bytes
    const payloadBytes = new TextEncoder().encode(JSON.stringify(payload));

    // Encode as binary reliable message: [0x01][seq 8B LE][payload]
    const binaryMessage = encodeReliableMessage("data", { seq, payload: payloadBytes });

    // Encrypt and send - onSend returns the encrypted envelope for caching
    const encryptedEnvelope = await this.onSend(binaryMessage);

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
   * Returns object with:
   * - count: number of messages acknowledged
   * - immediateRetransmits: array of cached encrypted envelopes to retransmit
   *
   * When ACK indicates receiver has seq N but we have unacked seq < N pending,
   * that lower seq is likely lost and should be retransmitted immediately.
   */
  processAck(ranges) {
    const acked = rangesToSet(ranges);
    let count = 0;

    // Find highest acked sequence
    const maxAcked = Math.max(...acked, 0);

    for (const seq of acked) {
      if (this.pending.has(seq)) {
        this.pending.delete(seq);
        count++;
      }
    }

    // Find pending messages with seq < maxAcked that weren't acked (gaps)
    // These are inferred lost and should be retransmitted immediately.
    // When peer explicitly tells us via SACK they have higher seqs but not this one,
    // we should retransmit right away - the peer is waiting for this message.
    const immediateRetransmits = [];
    const now = Date.now();

    for (const [seq, entry] of this.pending) {
      if (seq < maxAcked) {
        // This message wasn't acked but receiver has higher seqs - it's lost
        entry.lastSentAt = now;
        entry.attempts++;
        immediateRetransmits.push({
          seq,
          encryptedEnvelope: entry.encryptedEnvelope,
        });
        console.log(
          `[Reliable] Immediate retransmit seq=${seq} (gap detected, receiver has up to ${maxAcked})`
        );
      }
    }

    // If nothing pending, stop retransmit timer
    if (this.pending.size === 0 && this.retransmitTimer) {
      clearTimeout(this.retransmitTimer);
      this.retransmitTimer = null;
    }

    return { count, immediateRetransmits };
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
   * Prune old entries from received set to prevent unbounded growth.
   * Keeps sequences >= (nextExpected - DUPLICATE_WINDOW).
   */
  pruneReceivedSet() {
    const minKeep = Math.max(1, this.nextExpected - DUPLICATE_WINDOW);
    for (const seq of this.received) {
      if (seq < minKeep) {
        this.received.delete(seq);
      }
    }
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

      // Prune received set periodically to prevent unbounded growth
      if (this.nextExpected % 100 === 0) {
        this.pruneReceivedSet();
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
   * Handles the CLI's message format:
   * - 0x00: uncompressed JSON (strip marker, parse as JSON)
   * - 0x01: raw terminal data (strip marker, return as Uint8Array)
   * - 0x1f: gzip compressed JSON (strip marker, decompress, parse as JSON)
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

      // CLI's compression layer adds 0x00/0x1f prefix, so raw terminal
      // data (0x01 prefix) arrives as [0x00, 0x01, ...] - we handle both layers.
      if (marker === 0x00) {
        // Uncompressed - strip marker byte, then check inner content
        const innerBytes = bytes.slice(1);
        if (innerBytes.length > 0 && innerBytes[0] === 0x01) {
          // Raw terminal data nested inside uncompressed wrapper
          return { type: "raw_output", data: innerBytes.slice(1) };
        }
        // Regular JSON
        const jsonString = new TextDecoder().decode(innerBytes);
        return JSON.parse(jsonString);
      } else if (marker === 0x01) {
        // Raw terminal data (direct, no compression wrapper)
        return { type: "raw_output", data: bytes.slice(1) };
      } else if (marker === 0x1f) {
        // Gzip compressed JSON - strip marker, decompress
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
   * Returns binary-encoded ACK message.
   */
  generateAck() {
    this.lastAckSent = Date.now();
    const ranges = setToRanges(this.received);
    return encodeReliableMessage("ack", { ranges });
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

// Export utilities for channel.js and testing
export {
  setToRanges,
  rangesToSet,
  encodeReliableMessage,
  decodeReliableMessage,
  MSG_TYPE_DATA,
  MSG_TYPE_ACK,
};
