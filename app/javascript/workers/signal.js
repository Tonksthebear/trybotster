/**
 * Signal Protocol SharedWorker
 *
 * SharedWorker ensures all tabs share a single Signal session state.
 * This prevents ratchet desync when multiple tabs encrypt/decrypt.
 *
 * Isolates all sensitive cryptographic operations:
 * - Non-extractable AES-GCM wrapping key (cannot be exported, even by XSS)
 * - Signal session state (decrypted pickles never leave this worker)
 * - WASM module instance
 *
 * Security model:
 * - XSS in main thread can send encrypt/decrypt requests
 * - XSS CANNOT steal session state for use elsewhere
 * - Main thread only ever sees encrypted envelopes and decrypted messages
 */

// =============================================================================
// DOM Stubs for ActionCable compatibility
// ActionCable references document in several places:
// 1. document.visibilityState - for connection monitoring
// 2. document.createElement("a") - for URL parsing
// 3. document.head.querySelector() - for reading meta tags
// In a SharedWorker, we stub these to avoid ReferenceErrors.
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

// WASM module (loaded on init)
let wasmModule = null;

// In-memory session cache (hubId -> wasmSession)
const sessions = new Map();

// Port registry: portId -> { port, lastPong, hubRefs, subscriptions }
const ports = new Map();

// Connection pool: hubId -> { cable, state, refCount, portRefs, subscriptions, closeTimer }
const connections = new Map();

// Grace period before closing idle connections (handles Turbo navigation)
const CONNECTION_CLOSE_DELAY_MS = 2000;

// Port ID counter
let portIdCounter = 0;
function generatePortId() {
  return `port_${++portIdCounter}_${Date.now()}`;
}

// Subscription ID counter
let subscriptionIdCounter = 0;
function generateSubscriptionId() {
  return `sub_${++subscriptionIdCounter}_${Date.now()}`;
}

// Mutex to serialize encrypt/decrypt operations per hub
// This prevents race conditions where multiple operations interleave
const operationQueues = new Map(); // hubId -> Promise chain

async function withMutex(hubId, operation) {
  const queue = operationQueues.get(hubId) || Promise.resolve();

  // Create a deferred promise for this operation's result
  let resolve, reject;
  const resultPromise = new Promise((res, rej) => {
    resolve = res;
    reject = rej;
  });

  // Chain onto the queue - always wait for previous to complete before starting ours
  const newQueue = queue.then(async () => {
    try {
      const result = await operation();
      resolve(result);
    } catch (error) {
      reject(error);
    }
  });

  // Update the queue (swallow errors so chain continues)
  operationQueues.set(
    hubId,
    newQueue.catch(() => {}),
  );

  return resultPromise;
}

// IndexedDB config
// Note: Keys stored as JWK (not CryptoKey) for Safari SharedWorker compatibility (WebKit #177350)
const DB_NAME = "botster";
const DB_VERSION = 1;
const STORE_NAME = "sessions";
const KEY_STORE_NAME = "encryption_keys";
const WRAPPING_KEY_ID = "session_wrapping_key";

// Cached wrapping key (non-extractable CryptoKey)
let wrappingKeyCache = null;

// =============================================================================
// Helper Functions
// =============================================================================

function emitToHubPorts(hubId, event) {
  for (const [portId, portState] of ports) {
    if (portState.hubRefs.has(hubId)) {
      try {
        portState.port.postMessage(event);
      } catch (e) {
        // Port closed, will be cleaned up by heartbeat
      }
    }
  }
}

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

/**
 * Reliable sender state.
 * Tracks pending (unacked) messages and handles retransmission.
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
    const payloadBytes = Array.from(new TextEncoder().encode(JSON.stringify(payload)))
    const message = { type: "data", seq, payload: payloadBytes }

    const encryptedEnvelope = await this.onSend(message)

    this.pending.set(seq, {
      payloadBytes,
      encryptedEnvelope,
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
    for (const seq of acked) {
      if (this.pending.has(seq)) {
        this.pending.delete(seq)
        count++
      }
    }
    if (this.pending.size === 0 && this.retransmitTimer) {
      clearTimeout(this.retransmitTimer)
      this.retransmitTimer = null
    }
    return count
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
        retransmits.push({ seq, encryptedEnvelope: entry.encryptedEnvelope })
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
      for (const { encryptedEnvelope } of retransmits) {
        this.onRetransmit(encryptedEnvelope)
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
    return { type: "ack", ranges: setToRanges(this.received) }
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
// Port Cleanup
// =============================================================================

function cleanupPort(portId) {
  const state = ports.get(portId);
  if (!state) return;

  // Clean up subscriptions owned by this port
  for (const subscriptionId of state.subscriptions) {
    const subInfo = findSubscriptionById(subscriptionId);
    if (subInfo) {
      const { subscription, sender, receiver, conn } = subInfo;
      if (sender) sender.destroy();
      if (receiver) receiver.destroy();
      subscription.unsubscribe();
      conn.subscriptions.delete(subscriptionId);
    }
  }

  // Release all hub connection refs for this port
  for (const hubId of state.hubRefs) {
    const conn = connections.get(hubId);
    if (conn) {
      const portRefs = conn.portRefs.get(portId) || 0;
      conn.refCount -= portRefs;
      conn.portRefs.delete(portId);

      // Close connection if no refs remain (no grace period needed - port is dead)
      if (conn.refCount <= 0) {
        if (conn.closeTimer) {
          clearTimeout(conn.closeTimer);
          conn.closeTimer = null;
        }
        conn.cable.disconnect();
        connections.delete(hubId);
        console.log(`[SignalWorker] Closed connection to hub ${hubId}`);
      }
    }
  }

  ports.delete(portId);
  console.log(`[SignalWorker] Cleaned up port ${portId}, ${ports.size} ports remaining`);
}

// =============================================================================
// Message Handler (shared between SharedWorker and regular Worker)
// =============================================================================

async function handleMessage(event, portId, replyFn) {
  const { id, action, ...params } = event.data;

  // Handle pong (heartbeat response)
  if (action === "pong") {
    const portState = ports.get(portId);
    if (portState) {
      portState.lastPong = Date.now();
    }
    return; // No reply needed for pong
  }

  try {
    let result;

    switch (action) {
      case "init":
        result = await handleInit(params.wasmJsUrl, params.wasmBinaryUrl);
        break;
      case "createSession":
        result = await handleCreateSession(params.bundleJson, params.hubId);
        break;
      case "loadSession":
        // Use the proper handleLoadSession now that getOrCreateWrappingKey is fixed
        result = await handleLoadSession(params.hubId);
        break;
      case "hasSession":
        result = await handleHasSession(params.hubId);
        break;
      case "encrypt":
        // Serialize encrypt operations to prevent counter race conditions
        result = await withMutex(params.hubId, () =>
          handleEncrypt(params.hubId, params.message),
        );
        break;
      case "decrypt":
        // Serialize decrypt operations to prevent session state races
        result = await withMutex(params.hubId, () =>
          handleDecrypt(params.hubId, params.envelope),
        );
        break;
      case "getIdentityKey":
        result = await handleGetIdentityKey(params.hubId);
        break;
      case "clearSession":
        result = await handleClearSession(params.hubId);
        break;
      case "processSenderKeyDistribution":
        result = await handleProcessSenderKeyDistribution(
          params.hubId,
          params.distributionB64,
        );
        break;
      case "connect":
        result = await handleConnect(portId, params.hubId, params.cableUrl, params.actionCableModuleUrl, params.sessionBundle);
        break;
      case "disconnect":
        result = await handleDisconnect(portId, params.hubId);
        break;
      case "subscribe":
        result = await handleSubscribe(portId, params.hubId, params.channel, params.params, params.reliable);
        break;
      case "unsubscribe":
        result = await handleUnsubscribe(portId, params.subscriptionId);
        break;
      case "send":
        result = await handleSend(params.subscriptionId, params.message);
        break;
      case "perform":
        result = await handlePerform(params.subscriptionId, params.actionName, params.data);
        break;
      default:
        throw new Error(`Unknown action: ${action}`);
    }

    replyFn({ id, success: true, result });
  } catch (error) {
    console.error("[SignalWorker] Error:", action, error);
    replyFn({ id, success: false, error: error.message });
  }
}

// =============================================================================
// SharedWorker Connection Handler
// =============================================================================

self.onconnect = (event) => {
  const port = event.ports[0];
  const portId = generatePortId();

  ports.set(portId, {
    port,
    lastPong: Date.now(),
    hubRefs: new Set(), // Will track which hubs this port has connected to
    subscriptions: new Set(), // Will track subscription IDs owned by this port
  });

  port.onmessage = (msgEvent) => {
    handleMessage(msgEvent, portId, (msg) => port.postMessage(msg));
  };

  port.onmessageerror = () => {
    cleanupPort(portId);
  };

  port.start();
};

// =============================================================================
// Heartbeat: ping all ports every 5 seconds, cleanup dead ones after 21 seconds
// =============================================================================

const HEARTBEAT_INTERVAL = 5000;
const PORT_TTL = 21000;

setInterval(() => {
  const now = Date.now();

  for (const [portId, state] of ports) {
    // Check for dead ports
    if (now - state.lastPong > PORT_TTL) {
      console.log(`[SignalWorker] Port ${portId} timed out, cleaning up`);
      cleanupPort(portId);
      continue;
    }

    // Send ping
    try {
      state.port.postMessage({ event: "ping" });
    } catch (e) {
      // Port likely closed, clean up
      console.log(`[SignalWorker] Port ${portId} unreachable, cleaning up`);
      cleanupPort(portId);
    }
  }
}, HEARTBEAT_INTERVAL);

// =============================================================================
// Regular Worker Fallback (for browsers without SharedWorker support)
// =============================================================================

self.onmessage = (event) => {
  handleMessage(event, null, (msg) => self.postMessage(msg));
};

// =============================================================================
// Action Handlers
// =============================================================================

async function handleInit(wasmJsUrl, wasmBinaryUrl) {
  if (wasmModule) {
    return { alreadyInitialized: true };
  }

  const module = await import(wasmJsUrl);
  await module.default({ module_or_path: wasmBinaryUrl });
  wasmModule = module;

  return { initialized: true };
}

async function handleCreateSession(bundleJson, hubId) {
  if (!wasmModule) throw new Error("WASM not initialized");

  // Clear any existing session
  await deleteSessionFromStorage(hubId);
  sessions.delete(hubId);

  // Create new WASM session
  const bundleStr =
    typeof bundleJson === "string" ? bundleJson : JSON.stringify(bundleJson);
  const wasmSession = await new wasmModule.SignalSession(bundleStr);

  // Store in memory
  sessions.set(hubId, wasmSession);

  // Persist encrypted
  await persistSession(hubId, wasmSession);

  // Return identity key for the main thread
  const identityKey = await wasmSession.get_identity_key();
  return { created: true, identityKey };
}

async function handleLoadSession(hubId) {
  // Check memory cache first
  if (sessions.has(hubId)) {
    return { loaded: true, fromCache: true };
  }

  // Try loading from IndexedDB
  let pickled;
  try {
    pickled = await loadSessionFromStorage(hubId);
  } catch (idbError) {
    return { loaded: false, error: idbError.message };
  }

  if (!pickled) {
    return { loaded: false };
  }

  if (!wasmModule) {
    throw new Error("WASM not initialized");
  }

  try {
    const wasmSession = wasmModule.SignalSession.from_pickle(pickled);
    sessions.set(hubId, wasmSession);
    return { loaded: true, fromCache: false };
  } catch (error) {
    await deleteSessionFromStorage(hubId);
    return { loaded: false, error: error.message };
  }
}

async function handleHasSession(hubId) {
  if (sessions.has(hubId)) {
    return { hasSession: true };
  }

  const pickled = await loadSessionFromStorage(hubId);
  return { hasSession: !!pickled };
}

async function handleEncrypt(hubId, message) {
  const session = sessions.get(hubId);
  if (!session) throw new Error(`No session for hub ${hubId}`);

  const messageStr =
    typeof message === "string" ? message : JSON.stringify(message);

  const envelope = await session.encrypt(messageStr);

  // Persist after encryption (Double Ratchet state changed)
  await persistSession(hubId, session);

  return { envelope };
}

async function handleDecrypt(hubId, envelope) {
  const session = sessions.get(hubId);
  if (!session) throw new Error(`No session for hub ${hubId}`);

  const envelopeStr =
    typeof envelope === "string" ? envelope : JSON.stringify(envelope);
  const plaintext = await session.decrypt(envelopeStr);

  // Persist after decryption (Double Ratchet state changed)
  await persistSession(hubId, session);

  // Try to parse as JSON
  try {
    return { plaintext: JSON.parse(plaintext) };
  } catch {
    return { plaintext };
  }
}

async function handleGetIdentityKey(hubId) {
  const session = sessions.get(hubId);
  if (!session) throw new Error(`No session for hub ${hubId}`);

  const identityKey = await session.get_identity_key();
  return { identityKey };
}

async function handleClearSession(hubId) {
  sessions.delete(hubId);
  await deleteSessionFromStorage(hubId);
  return { cleared: true };
}

async function handleProcessSenderKeyDistribution(hubId, distributionB64) {
  const session = sessions.get(hubId);
  if (!session) throw new Error(`No session for hub ${hubId}`);

  await session.process_sender_key_distribution(distributionB64);
  await persistSession(hubId, session);
  return { processed: true };
}

async function handleConnect(portId, hubId, cableUrl, actionCableModuleUrl, sessionBundle) {
  // Get or create connection
  let conn = connections.get(hubId);

  if (conn) {
    // Cancel any pending close timer (handles Turbo navigation reconnect)
    if (conn.closeTimer) {
      clearTimeout(conn.closeTimer);
      conn.closeTimer = null;
      console.log(`[SignalWorker] Cancelled pending close for hub ${hubId} (reconnected)`);
    }

    // Increment ref count for this port
    const portRefs = conn.portRefs.get(portId) || 0;
    conn.portRefs.set(portId, portRefs + 1);
    conn.refCount++;

    // Track hub ref on port
    const portState = ports.get(portId);
    if (portState) {
      portState.hubRefs.add(hubId);
    }

    // Handle session: create from bundle or load from storage
    if (sessionBundle) {
      const hasSession = sessions.has(hubId) || (await loadSessionFromStorage(hubId));
      if (!hasSession) {
        await handleCreateSession(sessionBundle, hubId);
      }
    } else if (!sessions.has(hubId)) {
      // Try to load existing session from IndexedDB
      await handleLoadSession(hubId);
    }

    return {
      state: conn.state,
      sessionExists: sessions.has(hubId),
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
    portRefs: new Map([[portId, 1]]),
    subscriptions: new Map()
  };
  connections.set(hubId, conn);

  // Track hub ref on port
  const portState = ports.get(portId);
  if (portState) {
    portState.hubRefs.add(hubId);
  }

  // Set up connection state monitoring
  // ActionCable connection object exposes these callbacks
  const originalOpen = cable.connection.events.open;
  const originalClose = cable.connection.events.close;

  cable.connection.events.open = () => {
    if (originalOpen) originalOpen();
    conn.state = "connected";
    emitToHubPorts(hubId, {
      event: "connection:state",
      hubId,
      state: "connected"
    });
  };

  cable.connection.events.close = (event) => {
    if (originalClose) originalClose(event);
    conn.state = "disconnected";
    emitToHubPorts(hubId, {
      event: "connection:state",
      hubId,
      state: "disconnected",
      reason: "closed"
    });
  };

  // Handle session: create from bundle or load from storage
  if (sessionBundle) {
    await handleCreateSession(sessionBundle, hubId);
  } else if (!sessions.has(hubId)) {
    // Try to load existing session from IndexedDB
    await handleLoadSession(hubId);
  }

  return {
    state: conn.state,
    sessionExists: sessions.has(hubId),
    refCount: conn.refCount
  };
}

async function handleDisconnect(portId, hubId) {
  const conn = connections.get(hubId);
  if (!conn) {
    return { refCount: 0, closed: false };
  }

  // Decrement port's ref count
  const portRefs = conn.portRefs.get(portId) || 0;
  if (portRefs > 1) {
    conn.portRefs.set(portId, portRefs - 1);
  } else {
    conn.portRefs.delete(portId);
    // Remove hub from port's hubRefs
    const portState = ports.get(portId);
    if (portState) {
      portState.hubRefs.delete(hubId);
    }
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
        console.log(`[SignalWorker] Closing idle connection to hub ${hubId}`);
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

async function handleSubscribe(portId, hubId, channelName, channelParams, reliable = false) {
  console.log(`[SignalWorker] handleSubscribe: hubId=${hubId}, channel=${channelName}, portId=${portId}`);

  const conn = connections.get(hubId);
  if (!conn) {
    throw new Error(`No connection to hub ${hubId}`);
  }

  const subscriptionId = generateSubscriptionId();
  console.log(`[SignalWorker] Creating subscription ${subscriptionId}`);
  const portState = ports.get(portId);

  // Create reliable sender/receiver if enabled (before subscription so they're ready)
  let sender = null;
  let receiver = null;

  if (reliable) {
    sender = new ReliableSender({
      retransmitTimeout: 3000,
      onSend: async (msg) => {
        return await encryptAndSend(hubId, subEntry.subscription, msg);
      },
      onRetransmit: (envelope) => {
        sendPreEncrypted(subEntry.subscription, envelope);
      }
    });

    receiver = new ReliableReceiver({
      onDeliver: (payload) => {
        // Route decrypted, in-order message to owning port
        if (portState) {
          try {
            portState.port.postMessage({
              event: "subscription:message",
              subscriptionId,
              message: payload
            });
          } catch (e) {}
        }
      },
      onAck: async (ack) => {
        // Send ACK through encrypted channel
        await encryptAndSend(hubId, subEntry.subscription, ack);
      }
    });
  }

  // Pre-create the subscription entry so sender/receiver can reference it
  const subEntry = {
    subscription: null, // Will be set after create()
    portId,
    hubId,
    reliable,
    sender,
    receiver,
    decryptionFailureCount: 0,
    maxDecryptionFailures: 3
  };
  conn.subscriptions.set(subscriptionId, subEntry);

  // Track subscription in port
  if (portState) {
    portState.subscriptions.add(subscriptionId);
  }

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
      if (portState) portState.subscriptions.delete(subscriptionId);
      reject(new Error(`Subscription timeout for ${channelName}`));
    }, SUBSCRIBE_TIMEOUT);

    const subscription = conn.cable.subscriptions.create(
      { channel: channelName, ...channelParams },
      {
        connected: () => {
          if (settled) return;
          settled = true;
          clearTimeout(timeout);
          console.log(`[SignalWorker] Subscription ${subscriptionId} confirmed by server`);
          // Emit subscription:confirmed to owning port
          if (portState) {
            try {
              portState.port.postMessage({
                event: "subscription:confirmed",
                subscriptionId
              });
            } catch (e) {}
          }
          resolve({ subscriptionId });
        },

        rejected: () => {
          if (settled) return;
          settled = true;
          clearTimeout(timeout);
          // Clean up on rejection
          conn.subscriptions.delete(subscriptionId);
          if (portState) portState.subscriptions.delete(subscriptionId);
          // Emit subscription:rejected to owning port
          if (portState) {
            try {
              portState.port.postMessage({
                event: "subscription:rejected",
                subscriptionId,
                reason: "Subscription rejected by server"
              });
            } catch (e) {}
          }
          reject(new Error(`Subscription rejected for ${channelName}`));
        },

        received: async (data) => {
          console.log(`[SignalWorker] Received message on ${subscriptionId}`, data?.type || data?.envelope ? "(encrypted)" : data);
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

async function handleUnsubscribe(portId, subscriptionId) {
  console.log(`[SignalWorker] handleUnsubscribe: subscriptionId=${subscriptionId}, portId=${portId}`);

  const subInfo = findSubscriptionById(subscriptionId);
  if (!subInfo) {
    console.log(`[SignalWorker] Subscription ${subscriptionId} not found`);
    return { unsubscribed: false, reason: "Subscription not found" };
  }

  const { subscription, sender, receiver, conn } = subInfo;

  // Clean up reliable delivery
  if (sender) sender.destroy();
  if (receiver) receiver.destroy();

  // Unsubscribe from ActionCable
  subscription.unsubscribe();
  console.log(`[SignalWorker] Unsubscribed ${subscriptionId} from ActionCable`);

  // Remove from connection
  conn.subscriptions.delete(subscriptionId);

  // Remove from port
  const portState = ports.get(portId);
  if (portState) {
    portState.subscriptions.delete(subscriptionId);
  }

  return { unsubscribed: true };
}

async function handleSend(subscriptionId, message) {
  const subInfo = findSubscriptionById(subscriptionId);
  if (!subInfo) {
    throw new Error(`Subscription ${subscriptionId} not found`);
  }

  const { subscription, sender, reliable, hubId } = subInfo;

  if (reliable && sender) {
    // Use reliable sender (handles encryption + caching)
    const seq = await sender.send(message);
    return { seq };
  } else {
    // Direct send with encryption
    await encryptAndSend(hubId, subscription, message);
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

// =============================================================================
// Encryption Helpers for Subscriptions
// =============================================================================

async function encryptAndSend(hubId, subscription, message) {
  const session = sessions.get(hubId);

  if (session) {
    // Encrypt using mutex to prevent counter race
    const envelope = await withMutex(hubId, async () => {
      const messageStr = typeof message === 'string' ? message : JSON.stringify(message);
      const result = await session.encrypt(messageStr);
      await persistSession(hubId, session);
      return result;
    });
    subscription.perform("relay", { envelope });
    return envelope;
  } else {
    // No session, send unencrypted
    subscription.perform("relay", { data: message });
    return null;
  }
}

function sendPreEncrypted(subscription, envelope) {
  if (envelope) {
    subscription.perform("relay", { envelope });
  }
}

async function processIncomingMessage(subscriptionId, hubId, data) {
  const subInfo = findSubscriptionById(subscriptionId);
  if (!subInfo) return;

  const { receiver, reliable, portId } = subInfo;
  const portState = ports.get(portId);

  let decrypted = data;

  // Decrypt if we have a session and data has envelope
  if (data.envelope) {
    const session = sessions.get(hubId);
    if (session) {
      try {
        decrypted = await withMutex(hubId, async () => {
          const envelopeStr = typeof data.envelope === 'string'
            ? data.envelope
            : JSON.stringify(data.envelope);
          const plaintext = await session.decrypt(envelopeStr);
          await persistSession(hubId, session);

          // Try to parse as JSON
          try {
            return JSON.parse(plaintext);
          } catch {
            return plaintext;
          }
        });
        subInfo.decryptionFailureCount = 0;
      } catch (error) {
        subInfo.decryptionFailureCount++;
        console.error(`[SignalWorker] Decryption failed (${subInfo.decryptionFailureCount}/${subInfo.maxDecryptionFailures}):`, error);

        if (subInfo.decryptionFailureCount >= subInfo.maxDecryptionFailures) {
          if (portState) {
            try {
              portState.port.postMessage({
                event: "session:invalid",
                hubId,
                message: "Encryption session expired. Please re-scan the QR code to reconnect."
              });
            } catch (e) {}
          }
        }
        return;
      }
    }
  }

  // Decompress if needed (string with compression marker)
  if (typeof decrypted === 'string') {
    try {
      decrypted = await decompressMessage(decrypted);
    } catch (error) {
      console.error("[SignalWorker] Decompression failed:", error);
      return;
    }
  }

  // Process through reliable layer if enabled
  if (reliable && receiver) {
    if (decrypted.type === "data" && decrypted.seq != null) {
      await receiver.receive(decrypted.seq, decrypted.payload);
    } else if (decrypted.type === "ack" && decrypted.ranges) {
      if (subInfo.sender) {
        subInfo.sender.processAck(decrypted.ranges);
      }
    } else {
      // Non-reliable message, deliver directly
      if (portState) {
        try {
          portState.port.postMessage({
            event: "subscription:message",
            subscriptionId,
            message: decrypted
          });
        } catch (e) {}
      }
    }
  } else {
    // Non-reliable: deliver directly
    if (portState) {
      try {
        portState.port.postMessage({
          event: "subscription:message",
          subscriptionId,
          message: decrypted
        });
      } catch (e) {}
    }
  }
}

// =============================================================================
// IndexedDB + Encryption
// =============================================================================

function openDatabase() {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open(DB_NAME, DB_VERSION);

    request.onerror = () => reject(request.error);
    request.onsuccess = () => resolve(request.result);

    request.onupgradeneeded = (event) => {
      const db = event.target.result;
      if (!db.objectStoreNames.contains(STORE_NAME)) {
        db.createObjectStore(STORE_NAME, { keyPath: "hubId" });
      }
      if (!db.objectStoreNames.contains(KEY_STORE_NAME)) {
        db.createObjectStore(KEY_STORE_NAME, { keyPath: "id" });
      }
    };
  });
}

async function getOrCreateWrappingKey() {
  if (wrappingKeyCache) {
    return wrappingKeyCache;
  }

  const db = await openDatabase();

  // Try to load existing key (stored as JWK for Safari SharedWorker compatibility)
  const record = await new Promise((resolve, reject) => {
    const tx = db.transaction(KEY_STORE_NAME, "readonly");
    const store = tx.objectStore(KEY_STORE_NAME);
    const request = store.get(WRAPPING_KEY_ID);
    request.onerror = () => reject(request.error);
    request.onsuccess = () => resolve(request.result);
  });

  if (record?.jwk) {
    // Import JWK as non-extractable CryptoKey
    const key = await crypto.subtle.importKey(
      "jwk",
      record.jwk,
      { name: "AES-GCM", length: 256 },
      false, // non-extractable
      ["encrypt", "decrypt"]
    );
    wrappingKeyCache = key;
    return key;
  }

  // Generate new key
  const tempKey = await crypto.subtle.generateKey(
    { name: "AES-GCM", length: 256 },
    true, // extractable for JWK export
    ["encrypt", "decrypt"],
  );

  // Export to JWK for storage (Safari SharedWorker can't read CryptoKey from IndexedDB)
  const jwk = await crypto.subtle.exportKey("jwk", tempKey);

  // Re-import as non-extractable for actual use
  const newKey = await crypto.subtle.importKey(
    "jwk",
    jwk,
    { name: "AES-GCM", length: 256 },
    false, // NON-EXTRACTABLE - XSS cannot export this
    ["encrypt", "decrypt"],
  );

  // Store JWK
  await new Promise((resolve, reject) => {
    const tx = db.transaction(KEY_STORE_NAME, "readwrite");
    const store = tx.objectStore(KEY_STORE_NAME);
    const request = store.put({ id: WRAPPING_KEY_ID, jwk });
    request.onerror = () => reject(request.error);
    request.onsuccess = () => resolve();
  });

  wrappingKeyCache = newKey;
  return newKey;
}

async function encryptWithWrappingKey(plaintext) {
  const key = await getOrCreateWrappingKey();
  const iv = crypto.getRandomValues(new Uint8Array(12));
  const encoded = new TextEncoder().encode(plaintext);

  const ciphertext = await crypto.subtle.encrypt(
    { name: "AES-GCM", iv },
    key,
    encoded,
  );

  return { iv, ciphertext };
}

async function decryptWithWrappingKey(iv, ciphertext) {
  const key = await getOrCreateWrappingKey();
  const decrypted = await crypto.subtle.decrypt(
    { name: "AES-GCM", iv },
    key,
    ciphertext,
  );
  return new TextDecoder().decode(decrypted);
}

async function persistSession(hubId, wasmSession) {
  try {
    const pickled = wasmSession.pickle();
    const { iv, ciphertext } = await encryptWithWrappingKey(pickled);
    const db = await openDatabase();

    await new Promise((resolve, reject) => {
      const tx = db.transaction(STORE_NAME, "readwrite");
      const store = tx.objectStore(STORE_NAME);
      const request = store.put({
        hubId,
        iv: Array.from(iv),
        ciphertext: ciphertext,
        updatedAt: Date.now(),
      });
      request.onerror = () => reject(request.error);
      request.onsuccess = () => resolve();
    });
  } catch (error) {
    console.error(`[SignalWorker] persistSession failed for ${hubId}:`, error);
  }
}

async function loadSessionFromStorage(hubId) {
  const db = await openDatabase();

  const record = await new Promise((resolve, reject) => {
    const tx = db.transaction(STORE_NAME, "readonly");
    const store = tx.objectStore(STORE_NAME);
    const request = store.get(hubId);
    request.onerror = () => reject(request.error);
    request.onsuccess = () => resolve(request.result);
  });

  if (!record) {
    return null;
  }

  try {
    const iv = new Uint8Array(record.iv);
    return await decryptWithWrappingKey(iv, record.ciphertext);
  } catch (error) {
    await deleteSessionFromStorage(hubId);
    return null;
  }
}

async function deleteSessionFromStorage(hubId) {
  const db = await openDatabase();
  return new Promise((resolve, reject) => {
    const tx = db.transaction(STORE_NAME, "readwrite");
    const store = tx.objectStore(STORE_NAME);
    const request = store.delete(hubId);
    request.onerror = () => reject(request.error);
    request.onsuccess = () => resolve();
  });
}
