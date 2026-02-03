/**
 * Signal Transport Worker
 *
 * Regular Worker that handles ActionCable connections and message routing.
 * NO cryptographic operations - all crypto is handled by the main thread via
 * the crypto SharedWorker.
 *
 * This worker handles:
 * - ActionCable connections and subscriptions
 * - Reliable delivery (retransmits, ordering, acks)
 * - Sending raw messages (no encryption)
 * - Receiving raw messages and forwarding to main thread (no decryption)
 */

// =============================================================================
// DOM Stubs for ActionCable compatibility
// ActionCable references document in several places:
// 1. document.visibilityState - for connection monitoring
// 2. document.createElement("a") - for URL parsing
// 3. document.head.querySelector() - for reading meta tags
// In a Worker, we stub these to avoid ReferenceErrors.
// =============================================================================

if (typeof document === "undefined") {
  globalThis.document = {
    visibilityState: "visible",
    addEventListener: () => {},
    removeEventListener: () => {},
    // createElement is used for URL parsing - create a minimal anchor mock
    createElement: (tag) => {
      if (tag === "a") {
        // Return an object that mimics anchor URL parsing behavior
        // When href is set, the browser normally parses it - we mock this
        const anchor = {
          _href: "",
          protocol: "wss:",
          host: "",
          hostname: "",
          port: "",
          pathname: "/",
          search: "",
          hash: "",
          href: "",
        }
        Object.defineProperty(anchor, "href", {
          get() { return this._href },
          set(url) {
            this._href = url
            // Parse the URL properly using the URL constructor
            try {
              const parsed = new URL(url, self.location?.origin || "https://localhost")
              this.protocol = parsed.protocol
              this.host = parsed.host
              this.hostname = parsed.hostname
              this.port = parsed.port
              this.pathname = parsed.pathname
              this.search = parsed.search
              this.hash = parsed.hash
            } catch (e) {
              // If parsing fails, leave defaults
            }
          }
        })
        return anchor
      }
      return {}
    },
    // head.querySelector is used for reading meta tags - return null
    head: {
      querySelector: () => null
    }
  }
}

// ActionCable module - loaded dynamically since workers can't use importmaps
let actionCableModule = null

async function getCreateConsumer(actionCableUrl) {
  if (actionCableModule) {
    return actionCableModule.createConsumer
  }
  // Dynamic import with full URL (passed from main thread)
  actionCableModule = await import(actionCableUrl)
  return actionCableModule.createConsumer
}

// =============================================================================
// Connection Pool
// =============================================================================

// Connection pool: hubId -> { cable, state, refCount, subscriptions, closeTimer }
const connections = new Map();

// Grace period before closing idle connections (handles Turbo navigation)
const CONNECTION_CLOSE_DELAY_MS = 2000;

// Subscription ID counter
let subscriptionIdCounter = 0;
function generateSubscriptionId() {
  return `sub_${++subscriptionIdCounter}_${Date.now()}`;
}

// =============================================================================
// Helper Functions
// =============================================================================

function getConnection(hubId) {
  return connections.get(hubId);
}

function findSubscriptionById(subscriptionId) {
  for (const [hubId, conn] of connections) {
    const subEntry = conn.subscriptions.get(subscriptionId);
    if (subEntry) {
      return { ...subEntry, hubId, conn };
    }
  }
  return null;
}

// =============================================================================
// Reliable Delivery Helpers
// =============================================================================

// Binary message type markers (must match Rust)
const MSG_TYPE_DATA = 0x01
const MSG_TYPE_ACK = 0x02

/**
 * Convert a Set of sequence numbers to ranges for efficient encoding.
 * Example: Set{1, 2, 3, 5, 7, 8} -> [[1, 3], [5, 5], [7, 8]]
 */
function setToRanges(set) {
  const sorted = Array.from(set).sort((a, b) => a - b)
  const ranges = []
  let i = 0
  while (i < sorted.length) {
    const start = sorted[i]
    let end = start
    while (i + 1 < sorted.length && sorted[i + 1] === end + 1) {
      i++
      end = sorted[i]
    }
    ranges.push([start, end])
    i++
  }
  return ranges
}

/**
 * Convert ranges back to a Set.
 * Example: [[1, 3], [5, 5]] -> Set{1, 2, 3, 5}
 */
function rangesToSet(ranges) {
  const set = new Set()
  for (const [start, end] of ranges) {
    for (let seq = start; seq <= end; seq++) {
      set.add(seq)
    }
  }
  return set
}

/**
 * Encode a reliable message to binary format.
 * @param {string} type - "data" or "ack"
 * @param {Object} msg - { seq, payload } for data, { ranges } for ack
 * @returns {Uint8Array}
 */
function encodeReliableMessage(type, msg) {
  if (type === "data") {
    const { seq, payload } = msg
    const buf = new Uint8Array(1 + 8 + payload.length)
    const view = new DataView(buf.buffer)
    buf[0] = MSG_TYPE_DATA
    view.setUint32(1, seq & 0xffffffff, true)
    view.setUint32(5, Math.floor(seq / 0x100000000), true)
    buf.set(payload, 9)
    return buf
  } else if (type === "ack") {
    const { ranges } = msg
    const count = Math.min(ranges.length, 0xffff)
    const buf = new Uint8Array(1 + 2 + count * 16)
    const view = new DataView(buf.buffer)
    buf[0] = MSG_TYPE_ACK
    view.setUint16(1, count, true)
    for (let i = 0; i < count; i++) {
      const [start, end] = ranges[i]
      const offset = 3 + i * 16
      view.setUint32(offset, start & 0xffffffff, true)
      view.setUint32(offset + 4, Math.floor(start / 0x100000000), true)
      view.setUint32(offset + 8, end & 0xffffffff, true)
      view.setUint32(offset + 12, Math.floor(end / 0x100000000), true)
    }
    return buf
  }
  throw new Error(`Unknown message type: ${type}`)
}

/**
 * Decode a binary reliable message.
 * @param {Uint8Array} bytes
 * @returns {{ type: string, seq?: number, payload?: Uint8Array, ranges?: Array }}
 */
function decodeReliableMessage(bytes) {
  if (bytes.length === 0) throw new Error("Empty message")
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength)

  switch (bytes[0]) {
    case MSG_TYPE_DATA: {
      if (bytes.length < 9) throw new Error(`Data message too short: ${bytes.length}`)
      const seqLow = view.getUint32(1, true)
      const seqHigh = view.getUint32(5, true)
      const seq = seqLow + seqHigh * 0x100000000
      const payload = bytes.slice(9)
      return { type: "data", seq, payload }
    }
    case MSG_TYPE_ACK: {
      if (bytes.length < 3) throw new Error(`Ack message too short: ${bytes.length}`)
      const count = view.getUint16(1, true)
      const expectedLen = 3 + count * 16
      if (bytes.length < expectedLen) {
        throw new Error(`Ack truncated: ${bytes.length} < ${expectedLen}`)
      }
      const ranges = []
      for (let i = 0; i < count; i++) {
        const offset = 3 + i * 16
        const startLow = view.getUint32(offset, true)
        const startHigh = view.getUint32(offset + 4, true)
        const endLow = view.getUint32(offset + 8, true)
        const endHigh = view.getUint32(offset + 12, true)
        ranges.push([startLow + startHigh * 0x100000000, endLow + endHigh * 0x100000000])
      }
      return { type: "ack", ranges }
    }
    default:
      throw new Error(`Unknown message type: 0x${bytes[0].toString(16)}`)
  }
}

// =============================================================================
// Compression Helpers
// =============================================================================

// Compression markers (match CLI's compression.rs)
const MARKER_UNCOMPRESSED = 0x00
const MARKER_RAW_TERMINAL = 0x01
const MARKER_GZIP = 0x1f

/**
 * Decompress message with compression marker handling.
 * @param {string|Uint8Array} data - Data with compression marker prefix
 * @returns {Promise<Object>} - Parsed JSON object
 */
async function decompressMessage(data) {
  const bytes = typeof data === 'string'
    ? new TextEncoder().encode(data)
    : data

  if (bytes.length === 0) throw new Error("Empty message")

  const marker = bytes[0]

  if (marker === MARKER_UNCOMPRESSED) {
    const jsonBytes = bytes.slice(1)
    const jsonString = new TextDecoder().decode(jsonBytes)
    return JSON.parse(jsonString)
  } else if (marker === MARKER_GZIP) {
    const compressedBytes = bytes.slice(1)
    const stream = new Blob([compressedBytes])
      .stream()
      .pipeThrough(new DecompressionStream("gzip"))
    const decompressed = await new Response(stream).text()
    return JSON.parse(decompressed)
  } else {
    return JSON.parse(typeof data === 'string' ? data : new TextDecoder().decode(data))
  }
}

// =============================================================================
// Reliable Delivery Classes
// =============================================================================

// Reliable delivery constants
const DEFAULT_RETRANSMIT_TIMEOUT_MS = 3000
const MAX_RETRANSMIT_TIMEOUT_MS = 30000
const BACKOFF_FACTOR = 1.5
const MAX_RETRANSMIT_ATTEMPTS = 10
const ACK_HEARTBEAT_INTERVAL_MS = 5000
const BUFFER_TTL_MS = 30000
const DUPLICATE_WINDOW = 1000

/**
 * Reliable sender state.
 * Tracks pending (unacked) messages and handles retransmission.
 * Caches raw messages (no encryption knowledge).
 */
class ReliableSender {
  constructor(options = {}) {
    this.nextSeq = 1
    this.pending = new Map()
    this.retransmitTimeout = options.retransmitTimeout || DEFAULT_RETRANSMIT_TIMEOUT_MS
    this.onSend = options.onSend || (async () => null)
    this.onRetransmit = options.onRetransmit || (() => {})
    this.retransmitTimer = null
    this.paused = false
  }

  reset() {
    this.nextSeq = 1
    this.pending.clear()
    if (this.retransmitTimer) {
      clearTimeout(this.retransmitTimer)
      this.retransmitTimer = null
    }
    this.paused = false
  }

  calculateTimeout(attempts) {
    const base = this.retransmitTimeout
    const backoff = base * Math.pow(BACKOFF_FACTOR, attempts - 1)
    return Math.min(backoff, MAX_RETRANSMIT_TIMEOUT_MS)
  }

  pause() {
    this.paused = true
    if (this.retransmitTimer) {
      clearTimeout(this.retransmitTimer)
      this.retransmitTimer = null
    }
  }

  resume() {
    this.paused = false
    this.scheduleRetransmit()
  }

  async send(payload) {
    const seq = this.nextSeq++
    const now = Date.now()
    const payloadBytes = new TextEncoder().encode(JSON.stringify(payload))

    // Encode as binary: [0x01][seq 8B LE][payload bytes]
    const binaryMessage = encodeReliableMessage("data", { seq, payload: payloadBytes })

    // Send raw message (no encryption - main thread handles that)
    const cachedMsg = await this.onSend(binaryMessage)

    this.pending.set(seq, {
      payloadBytes,
      cachedMsg,  // Cache the raw message for retransmits
      firstSentAt: now,
      lastSentAt: now,
      attempts: 1,
    })

    this.scheduleRetransmit()
    return seq
  }

  processAck(ranges) {
    const acked = rangesToSet(ranges)
    let count = 0

    // Find highest acked sequence
    const maxAcked = Math.max(...acked, 0)

    for (const seq of acked) {
      if (this.pending.has(seq)) {
        this.pending.delete(seq)
        count++
      }
    }

    // Find pending messages with seq < maxAcked that weren't acked (gaps)
    // These are inferred lost and should be retransmitted immediately.
    // When peer explicitly tells us via SACK they have higher seqs but not this one,
    // we should retransmit right away - the peer is waiting for this message.
    const immediateRetransmits = []
    const now = Date.now()

    for (const [seq, entry] of this.pending) {
      if (seq < maxAcked) {
        entry.lastSentAt = now
        entry.attempts++
        immediateRetransmits.push({ seq, cachedMsg: entry.cachedMsg })
        console.log(`[Reliable] Immediate retransmit seq=${seq} (gap detected)`)
      }
    }

    if (this.pending.size === 0 && this.retransmitTimer) {
      clearTimeout(this.retransmitTimer)
      this.retransmitTimer = null
    }
    return { count, immediateRetransmits }
  }

  getRetransmits() {
    const now = Date.now()
    const retransmits = []
    const failedSeqs = []

    for (const [seq, entry] of this.pending) {
      if (entry.attempts >= MAX_RETRANSMIT_ATTEMPTS) {
        failedSeqs.push(seq)
        continue
      }
      const timeout = this.calculateTimeout(entry.attempts)
      if (now - entry.lastSentAt >= timeout) {
        entry.lastSentAt = now
        entry.attempts++
        retransmits.push({ seq, cachedMsg: entry.cachedMsg })
      }
    }

    for (const seq of failedSeqs) {
      this.pending.delete(seq)
    }
    return retransmits
  }

  scheduleRetransmit() {
    if (this.paused || this.retransmitTimer || this.pending.size === 0) return

    this.retransmitTimer = setTimeout(() => {
      this.retransmitTimer = null
      if (this.paused) return

      const retransmits = this.getRetransmits()
      for (const { cachedMsg } of retransmits) {
        this.onRetransmit(cachedMsg)
      }

      if (this.pending.size > 0) {
        this.scheduleRetransmit()
      }
    }, this.retransmitTimeout)
  }

  destroy() {
    if (this.retransmitTimer) {
      clearTimeout(this.retransmitTimer)
      this.retransmitTimer = null
    }
  }
}

/**
 * Reliable receiver state.
 * Buffers out-of-order messages and delivers in sequence.
 */
class ReliableReceiver {
  constructor(options = {}) {
    this.received = new Set()
    this.nextExpected = 1
    this.buffer = new Map()
    this.lastAckSent = Date.now()
    this.onDeliver = options.onDeliver || (() => {})
    this.onAck = options.onAck || (() => {})
    this.ackTimer = null
  }

  reset() {
    this.received.clear()
    this.nextExpected = 1
    this.buffer.clear()
  }

  cleanupStaleBuffer() {
    const now = Date.now()
    const staleThreshold = now - BUFFER_TTL_MS
    for (const [seq, entry] of this.buffer) {
      if (entry.receivedAt < staleThreshold) {
        this.buffer.delete(seq)
      }
    }
  }

  pruneReceivedSet() {
    const minKeep = Math.max(1, this.nextExpected - DUPLICATE_WINDOW)
    for (const seq of this.received) {
      if (seq < minKeep) {
        this.received.delete(seq)
      }
    }
  }

  async receive(seq, payloadBytes) {
    if (seq === 1 && this.nextExpected > 1) {
      this.reset()
    }

    this.cleanupStaleBuffer()

    if (this.received.has(seq)) {
      if (seq < 10 && this.nextExpected > seq + 5) {
        this.reset()
      } else {
        return []
      }
    }

    this.received.add(seq)
    this.scheduleAck()

    let payload = await this.deserializePayload(payloadBytes)
    if (payload === null) return []

    if (seq === this.nextExpected) {
      const deliverable = [payload]
      this.nextExpected++

      while (this.buffer.has(this.nextExpected)) {
        const entry = this.buffer.get(this.nextExpected)
        deliverable.push(entry.payload)
        this.buffer.delete(this.nextExpected)
        this.nextExpected++
      }

      // Prune received set periodically to prevent unbounded growth
      if (this.nextExpected % 100 === 0) {
        this.pruneReceivedSet()
      }

      for (const p of deliverable) {
        this.onDeliver(p)
      }
      return deliverable
    } else if (seq > this.nextExpected) {
      this.buffer.set(seq, { payload, receivedAt: Date.now() })
      return []
    }
    return []
  }

  async deserializePayload(payloadBytes) {
    try {
      const bytes = Array.isArray(payloadBytes) ? new Uint8Array(payloadBytes) : payloadBytes
      if (bytes.length === 0) return null

      const marker = bytes[0]

      if (marker === MARKER_UNCOMPRESSED) {
        const innerBytes = bytes.slice(1)
        // Check for nested raw terminal data (CLI sends 0x01 prefix, compression wraps as 0x00 + 0x01)
        if (innerBytes.length > 0 && innerBytes[0] === MARKER_RAW_TERMINAL) {
          return { type: "raw_output", data: innerBytes.slice(1) }
        }
        return JSON.parse(new TextDecoder().decode(innerBytes))
      } else if (marker === MARKER_RAW_TERMINAL) {
        // Direct raw terminal data (no compression wrapper)
        return { type: "raw_output", data: bytes.slice(1) }
      } else if (marker === MARKER_GZIP) {
        const compressedBytes = bytes.slice(1)
        const stream = new Blob([compressedBytes]).stream().pipeThrough(new DecompressionStream("gzip"))
        const decompressed = await new Response(stream).text()
        return JSON.parse(decompressed)
      } else {
        return JSON.parse(new TextDecoder().decode(bytes))
      }
    } catch (error) {
      console.error("[Reliable] Payload deserialization error:", error)
      return null
    }
  }

  generateAck() {
    this.lastAckSent = Date.now()
    // Return binary-encoded ACK
    return encodeReliableMessage("ack", { ranges: setToRanges(this.received) })
  }

  scheduleAck() {
    if (this.ackTimer) return
    this.ackTimer = setTimeout(() => {
      this.ackTimer = null
      const ack = this.generateAck()
      this.onAck(ack)
    }, 50)
  }

  shouldSendAckHeartbeat() {
    return Date.now() - this.lastAckSent >= ACK_HEARTBEAT_INTERVAL_MS
  }

  destroy() {
    if (this.ackTimer) {
      clearTimeout(this.ackTimer)
      this.ackTimer = null
    }
  }
}

// =============================================================================
// Message Handler
// =============================================================================

async function handleMessage(event) {
  const { id, action, ...params } = event.data;

  // Handle pong (heartbeat response from main thread)
  if (action === "pong") {
    return; // No reply needed for pong
  }

  try {
    let result;

    switch (action) {
      case "init":
        // Just initialize (ActionCable is loaded on demand during connect)
        result = { initialized: true };
        break;
      case "connect":
        result = await handleConnect(params.hubId, params.cableUrl, params.actionCableModuleUrl);
        break;
      case "disconnect":
        result = await handleDisconnect(params.hubId);
        break;
      case "subscribe":
        result = await handleSubscribe(params.hubId, params.channel, params.params, params.reliable);
        break;
      case "unsubscribe":
        result = await handleUnsubscribe(params.subscriptionId);
        break;
      case "sendRaw":
        result = await handleSendRaw(params.subscriptionId, params.message);
        break;
      case "perform":
        result = await handlePerform(params.subscriptionId, params.actionName, params.data);
        break;
      case "resetReliable":
        result = handleResetReliable(params.subscriptionId);
        break;
      default:
        throw new Error(`Unknown action: ${action}`);
    }

    self.postMessage({ id, success: true, result });
  } catch (error) {
    console.error("[TransportWorker] Error:", action, error);
    self.postMessage({ id, success: false, error: error.message });
  }
}

// =============================================================================
// Worker Entry Point
// =============================================================================

self.onmessage = handleMessage;

// =============================================================================
// Action Handlers
// =============================================================================

async function handleConnect(hubId, cableUrl, actionCableModuleUrl) {
  // Get or create connection
  let conn = connections.get(hubId);

  if (conn) {
    // Cancel any pending close timer (handles Turbo navigation reconnect)
    if (conn.closeTimer) {
      clearTimeout(conn.closeTimer);
      conn.closeTimer = null;
      console.log(`[TransportWorker] Cancelled pending close for hub ${hubId} (reconnected)`);
    }

    // Increment ref count
    conn.refCount++;

    return {
      state: conn.state,
      refCount: conn.refCount
    };
  }

  // Create new connection
  const createConsumer = await getCreateConsumer(actionCableModuleUrl)
  const cable = createConsumer(cableUrl)

  conn = {
    cable,
    state: "connecting",
    refCount: 1,
    subscriptions: new Map()
  };
  connections.set(hubId, conn);

  // Set up connection state monitoring
  // ActionCable connection object exposes these callbacks
  const originalOpen = cable.connection.events.open;
  const originalClose = cable.connection.events.close;

  cable.connection.events.open = () => {
    if (originalOpen) originalOpen();
    conn.state = "connected";
    self.postMessage({
      event: "connection:state",
      hubId,
      state: "connected"
    });
  };

  cable.connection.events.close = (event) => {
    if (originalClose) originalClose(event);
    conn.state = "disconnected";
    self.postMessage({
      event: "connection:state",
      hubId,
      state: "disconnected",
      reason: "closed"
    });
  };

  return {
    state: conn.state,
    refCount: conn.refCount
  };
}

async function handleDisconnect(hubId) {
  const conn = connections.get(hubId);
  if (!conn) {
    return { refCount: 0, closed: false };
  }

  conn.refCount--;

  // If no more refs, schedule close after grace period
  // This handles Turbo navigation where disconnect fires before new page's connect
  if (conn.refCount <= 0) {
    // Cancel any existing timer (shouldn't happen, but be safe)
    if (conn.closeTimer) {
      clearTimeout(conn.closeTimer);
    }

    conn.closeTimer = setTimeout(() => {
      // Re-check refCount - might have reconnected during grace period
      const currentConn = connections.get(hubId);
      if (currentConn && currentConn.refCount <= 0) {
        console.log(`[TransportWorker] Closing idle connection to hub ${hubId}`);
        currentConn.cable.disconnect();
        connections.delete(hubId);
      }
    }, CONNECTION_CLOSE_DELAY_MS);

    return { refCount: 0, closing: true };
  }

  return { refCount: conn.refCount, closed: false };
}

// =============================================================================
// Subscription Handlers
// =============================================================================

async function handleSubscribe(hubId, channelName, channelParams, reliable = false) {
  console.log(`[TransportWorker] handleSubscribe: hubId=${hubId}, channel=${channelName}`);

  const conn = connections.get(hubId);
  if (!conn) {
    throw new Error(`No connection to hub ${hubId}`);
  }

  const subscriptionId = generateSubscriptionId();
  console.log(`[TransportWorker] Creating subscription ${subscriptionId}`);

  // Create reliable sender/receiver if enabled (before subscription so they're ready)
  let sender = null;
  let receiver = null;

  if (reliable) {
    sender = new ReliableSender({
      retransmitTimeout: 3000,
      onSend: async (msg) => {
        // Send raw message (no encryption - main thread handles that)
        return await sendRaw(subEntry.subscription, msg);
      },
      onRetransmit: (cachedMsg) => {
        // Retransmit the cached raw message
        sendRawDirect(subEntry.subscription, cachedMsg);
      }
    });

    receiver = new ReliableReceiver({
      onDeliver: (payload) => {
        // Route in-order message to main thread
        console.log(`[TransportWorker] Delivering message type=${payload?.type}, size=${JSON.stringify(payload)?.length}`);
        try {
          self.postMessage({
            event: "subscription:message",
            subscriptionId,
            message: payload
          });
        } catch (e) {
          console.error(`[TransportWorker] Failed to post message:`, e);
        }
      },
      onAck: async (ack) => {
        // Send ACK raw (no encryption)
        await sendRaw(subEntry.subscription, ack);
      }
    });
  }

  // Pre-create the subscription entry so sender/receiver can reference it
  const subEntry = {
    subscription: null, // Will be set after create()
    hubId,
    reliable,
    sender,
    receiver
  };
  conn.subscriptions.set(subscriptionId, subEntry);

  // Wait for server to confirm subscription before returning
  // This prevents the race condition where caller sends messages before subscription exists
  return new Promise((resolve, reject) => {
    const SUBSCRIBE_TIMEOUT = 10000; // 10 second timeout
    let settled = false;

    const timeout = setTimeout(() => {
      if (settled) return;
      settled = true;
      // Clean up on timeout
      conn.subscriptions.delete(subscriptionId);
      reject(new Error(`Subscription timeout for ${channelName}`));
    }, SUBSCRIBE_TIMEOUT);

    const subscription = conn.cable.subscriptions.create(
      { channel: channelName, ...channelParams },
      {
        connected: () => {
          if (settled) return;
          settled = true;
          clearTimeout(timeout);
          console.log(`[TransportWorker] Subscription ${subscriptionId} confirmed by server`);
          // Emit subscription:confirmed to main thread
          try {
            self.postMessage({
              event: "subscription:confirmed",
              subscriptionId
            });
          } catch (e) {}
          resolve({ subscriptionId });
        },

        rejected: () => {
          if (settled) return;
          settled = true;
          clearTimeout(timeout);
          // Clean up on rejection
          conn.subscriptions.delete(subscriptionId);
          // Emit subscription:rejected to main thread
          try {
            self.postMessage({
              event: "subscription:rejected",
              subscriptionId,
              reason: "Subscription rejected by server"
            });
          } catch (e) {}
          reject(new Error(`Subscription rejected for ${channelName}`));
        },

        received: async (data) => {
          console.log(`[TransportWorker] Received raw message on ${subscriptionId}`);
          await processIncomingMessage(subscriptionId, hubId, data);
        },

        disconnected: () => {
          // Connection lost - reliable sender will pause automatically
          const entry = findSubscriptionById(subscriptionId);
          if (entry?.sender) {
            entry.sender.pause();
          }
        }
      }
    );

    // Store subscription reference
    subEntry.subscription = subscription;
  });
}

async function handleUnsubscribe(subscriptionId) {
  console.log(`[TransportWorker] handleUnsubscribe: subscriptionId=${subscriptionId}`);

  const subInfo = findSubscriptionById(subscriptionId);
  if (!subInfo) {
    console.log(`[TransportWorker] Subscription ${subscriptionId} not found`);
    return { unsubscribed: false, reason: "Subscription not found" };
  }

  const { subscription, sender, receiver, conn } = subInfo;

  // Clean up reliable delivery
  if (sender) sender.destroy();
  if (receiver) receiver.destroy();

  // Unsubscribe from ActionCable
  subscription.unsubscribe();
  console.log(`[TransportWorker] Unsubscribed ${subscriptionId} from ActionCable`);

  // Remove from connection
  conn.subscriptions.delete(subscriptionId);

  return { unsubscribed: true };
}

async function handleSendRaw(subscriptionId, message) {
  const subInfo = findSubscriptionById(subscriptionId);
  if (!subInfo) {
    throw new Error(`Subscription ${subscriptionId} not found`);
  }

  const { subscription, sender, reliable } = subInfo;

  if (reliable && sender) {
    // Use reliable sender (handles caching for retransmits)
    const seq = await sender.send(message);
    return { seq };
  } else {
    // Direct send raw (no encryption - main thread handles that)
    await sendRaw(subscription, message);
    return { sent: true };
  }
}

async function handlePerform(subscriptionId, actionName, data) {
  const subInfo = findSubscriptionById(subscriptionId);
  if (!subInfo) {
    throw new Error(`Subscription ${subscriptionId} not found`);
  }

  const { subscription } = subInfo;
  subscription.perform(actionName, data);
  return { performed: true };
}

/**
 * Reset reliable delivery state for a subscription.
 * Called when CLI disconnects so next connection starts fresh.
 */
function handleResetReliable(subscriptionId) {
  const subInfo = findSubscriptionById(subscriptionId);
  if (!subInfo) {
    throw new Error(`Subscription ${subscriptionId} not found`);
  }

  const { sender, receiver } = subInfo;
  if (sender) {
    console.log(`[TransportWorker] Resetting reliable sender for ${subscriptionId}`);
    sender.reset();
  }
  if (receiver) {
    console.log(`[TransportWorker] Resetting reliable receiver for ${subscriptionId}`);
    receiver.reset();
  }
  return { reset: true };
}


// =============================================================================
// Raw Send Helpers (no encryption - main thread handles crypto)
// =============================================================================

/**
 * Send a raw message via the subscription.
 * Returns the message for caching (used by reliable sender for retransmits).
 * @param {Object} subscription - ActionCable subscription
 * @param {Uint8Array|Object} message - Raw message to send
 * @returns {Promise<Uint8Array|Object>} - The sent message (for caching)
 */
async function sendRaw(subscription, message) {
  // Convert Uint8Array to array for JSON serialization
  const data = message instanceof Uint8Array ? Array.from(message) : message;
  subscription.perform("relay", { data });
  return message;
}

/**
 * Send a raw message directly (for retransmits).
 * @param {Object} subscription - ActionCable subscription
 * @param {Uint8Array|Object} message - Cached raw message
 */
function sendRawDirect(subscription, message) {
  const data = message instanceof Uint8Array ? Array.from(message) : message;
  subscription.perform("relay", { data });
}

async function processIncomingMessage(subscriptionId, hubId, data) {
  const subInfo = findSubscriptionById(subscriptionId);
  if (!subInfo) return;

  const { receiver, reliable } = subInfo;

  // Forward raw data to main thread - main thread handles decryption
  // For reliable delivery, we still need to handle the reliable protocol here

  // Check if this is a binary reliable message
  if (reliable && receiver) {
    // Get bytes from data (could be array, Uint8Array, or object with data field)
    const rawData = data.data || data;
    const bytes = getBytes(rawData);
    if (bytes && bytes.length > 0) {
      const msgType = bytes[0];
      if (msgType === MSG_TYPE_DATA || msgType === MSG_TYPE_ACK) {
        try {
          const decoded = decodeReliableMessage(bytes);
          if (decoded.type === "data") {
            // Reliable data message - receiver handles payload deserialization
            await receiver.receive(decoded.seq, decoded.payload);
          } else if (decoded.type === "ack") {
            // ACK message - update sender's pending set
            if (subInfo.sender) {
              const { immediateRetransmits } = subInfo.sender.processAck(decoded.ranges);
              for (const { cachedMsg } of immediateRetransmits) {
                subInfo.sender.onRetransmit(cachedMsg);
              }
            }
          }
          return;
        } catch (error) {
          console.error("[TransportWorker] Failed to decode binary reliable message:", error);
          // Fall through to non-reliable handling
        }
      }
    }
  }

  // Forward raw message to main thread (main thread handles decryption)
  try {
    self.postMessage({
      event: "subscription:message",
      subscriptionId,
      message: data
    });
  } catch (e) {}
}

/**
 * Convert data to Uint8Array for binary parsing.
 */
function getBytes(data) {
  if (data instanceof Uint8Array) return data;
  if (typeof data === 'string') return new TextEncoder().encode(data);
  if (Array.isArray(data)) return new Uint8Array(data);
  return null;
}
