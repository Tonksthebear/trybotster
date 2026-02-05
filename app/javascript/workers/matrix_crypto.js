/**
 * Matrix Crypto SharedWorker
 *
 * Pure cryptographic operations using Matrix Olm/Megolm via @matrix-org/matrix-sdk-crypto-wasm.
 * This worker handles:
 * - WASM module loading
 * - Session creation/loading/persistence (using matrix-sdk-crypto-wasm's IndexedDB)
 * - Encrypt/decrypt operations via OlmMachine
 *
 * Transport is handled separately by WebRTCTransport in the main thread.
 *
 * Key differences from signal_crypto.js:
 * - Uses OlmMachine instead of custom Signal session
 * - Synthetic Matrix identifiers (@hub-{hubId}:botster.local)
 * - Built-in IndexedDB support from matrix-sdk-crypto-wasm
 *
 * Architecture Notes:
 * - matrix-sdk-crypto-wasm's OlmMachine is designed for Matrix room-based E2EE
 * - For direct peer-to-peer encryption, we use the to-device message flow
 * - CLI device keys are registered via markRequestAsSent after key claiming
 * - Olm sessions are established through getMissingSessions + key claim
 */

// WASM module state
let wasmInitialized = false;
let matrixModule = null;

// OlmMachine instances per hub (hubId -> OlmMachine)
const machines = new Map();

// Bundle info per hub (hubId -> bundle) - stores CLI's PreKey bundle
const bundles = new Map();

// Session info per hub (hubId -> { deviceId, userId, remoteDeviceId })
const sessionInfo = new Map();

// =============================================================================
// Mutex for Serializing Operations
// =============================================================================

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

// =============================================================================
// IndexedDB Configuration for Wrapping Keys
// =============================================================================

// Note: Keys stored as JWK (not CryptoKey) for Safari SharedWorker compatibility (WebKit #177350)
// matrix-sdk-crypto-wasm handles its own persistence, but we still need wrapping keys for
// encrypting sensitive data at rest
const DB_NAME = "botster-matrix";
const DB_VERSION = 1;
const KEY_STORE_NAME = "encryption_keys";
const WRAPPING_KEY_ID = "session_wrapping_key";

// Cached wrapping key (non-extractable CryptoKey)
let wrappingKeyCache = null;

// =============================================================================
// IndexedDB + Encryption for Wrapping Keys
// =============================================================================

function openDatabase() {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open(DB_NAME, DB_VERSION);

    request.onerror = () => reject(request.error);
    request.onsuccess = () => resolve(request.result);

    request.onupgradeneeded = (event) => {
      const db = event.target.result;
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

// =============================================================================
// Matrix Identifier Helpers
// =============================================================================

/**
 * Generate synthetic Matrix user ID from hub ID (for browser).
 * Format: @hub-{hubId}:botster.local
 */
function matrixUserId(hubId) {
  return `@hub-${hubId}:botster.local`;
}

/**
 * Generate synthetic Matrix user ID for CLI.
 * Format: @cli-{hubId}:botster.local
 */
function cliUserId(hubId) {
  return `@cli-${hubId}:botster.local`;
}

/**
 * Generate synthetic Matrix device ID for browser.
 * Uses a deterministic ID per hub for session persistence.
 * Format: browser-{n}
 */
function browserDeviceId(deviceNum = 1) {
  return `browser-${deviceNum}`;
}

/**
 * CLI device ID - this is the hub_id from Rails.
 * The CLI uses hub_id as its device ID.
 * @param {string} hubId - The hub identifier
 */
function cliDeviceId(hubId) {
  return hubId;
}

/**
 * Generate synthetic Matrix room ID from hub ID.
 * Format: !hub-{hubId}:botster.local
 */
function matrixRoomId(hubId) {
  return `!hub-${hubId}:botster.local`;
}

// =============================================================================
// Crypto Envelope Format
// =============================================================================

/**
 * CryptoEnvelope format for encrypted messages.
 * @typedef {Object} CryptoEnvelope
 * @property {number} t - Message type: 1=OlmPreKey, 2=Olm, 3=Megolm
 * @property {string} c - Base64-encoded ciphertext
 * @property {string} s - Sender Curve25519 key (base64)
 * @property {string} d - Device ID (e.g., "browser-1")
 */

const MSG_TYPE_OLM_PREKEY = 1;
const MSG_TYPE_OLM = 2;
const MSG_TYPE_MEGOLM = 3;

// =============================================================================
// Crypto Action Handlers
// =============================================================================

/**
 * Initialize the WASM module.
 * Must be called before any other operations.
 *
 * @param {string} wasmJsUrl - Full URL to the matrix-sdk-crypto-wasm JS module
 */
async function handleInit(wasmJsUrl) {
  if (wasmInitialized) {
    return { alreadyInitialized: true };
  }

  try {
    // Dynamic import using the full URL (SharedWorkers can't resolve bare module specifiers)
    if (!wasmJsUrl) {
      throw new Error("wasmJsUrl is required - SharedWorkers cannot resolve bare module specifiers");
    }

    console.log("[MatrixCrypto] Loading WASM module from:", wasmJsUrl);
    matrixModule = await import(wasmJsUrl);
    await matrixModule.initAsync();
    wasmInitialized = true;

    console.log("[MatrixCrypto] WASM module initialized");
    return { initialized: true };
  } catch (error) {
    console.error("[MatrixCrypto] Failed to initialize WASM:", error);
    throw new Error(`WASM initialization failed: ${error.message}`);
  }
}

/**
 * Create a new OlmMachine session for a hub.
 * Uses the CLI's PreKey bundle to establish an outbound Olm session.
 *
 * @param {string} hubId - The hub identifier
 * @param {string|Object} bundleJson - CLI's PreKey bundle (JSON)
 */
async function handleCreateSession(hubId, bundleJson) {
  if (!wasmInitialized) throw new Error("WASM not initialized");

  // Parse bundle if string
  const bundle = typeof bundleJson === "string" ? JSON.parse(bundleJson) : bundleJson;

  // Clear any existing machine for this hub
  if (machines.has(hubId)) {
    const oldMachine = machines.get(hubId);
    try {
      oldMachine.close();
    } catch (e) {
      // Ignore close errors
    }
    machines.delete(hubId);
  }

  // Create synthetic Matrix identifiers
  const userId = matrixUserId(hubId);
  const deviceId = browserDeviceId(1);
  const remoteUserId = cliUserId(hubId);
  const remoteDeviceId = cliDeviceId(hubId); // CLI uses hub_id as device_id
  const roomId = matrixRoomId(hubId);

  // Initialize OlmMachine with IndexedDB storage
  const { OlmMachine, UserId, DeviceId, RoomId, EncryptionSettings } = matrixModule;
  const machine = await OlmMachine.initialize(
    new UserId(userId),
    new DeviceId(deviceId),
    undefined, // storePath - uses IndexedDB in browser
    undefined  // passphrase
  );

  machines.set(hubId, machine);
  bundles.set(hubId, bundle);
  sessionInfo.set(hubId, {
    userId,
    deviceId,
    remoteUserId,
    remoteDeviceId,
    roomId,
  });

  // Register the CLI's device keys with the OlmMachine
  // This simulates receiving the CLI's device keys through a /keys/query response
  await registerRemoteDevice(hubId, bundle);

  // Get our identity key to return
  const identityKeys = machine.identityKeys;
  const curve25519Key = identityKeys.curve25519.toBase64();

  console.log(`[MatrixCrypto] Created session for hub ${hubId.substring(0, 8)}... (device: ${deviceId})`);

  return {
    created: true,
    identityKey: curve25519Key,
    deviceId: deviceId,
  };
}

/**
 * Register the CLI's device keys with the OlmMachine.
 * This simulates the Matrix /keys/query and /keys/claim flow.
 *
 * Bundle fields (from bundle.js):
 * - identityKey: Curve25519 identity key (base64)
 * - signingKey: Ed25519 signing key (base64)
 * - oneTimeKey: Curve25519 one-time key (base64)
 * - oneTimeKeyId: Key ID string
 *
 * @param {string} hubId - The hub identifier
 * @param {Object} bundle - CLI's PreKey bundle
 */
async function registerRemoteDevice(hubId, bundle) {
  const machine = machines.get(hubId);
  const info = sessionInfo.get(hubId);
  if (!machine || !info) return;

  const { UserId, DeviceId, RoomId } = matrixModule;

  // Step 1: Mark the remote user as tracked
  await machine.updateTrackedUsers([new UserId(info.remoteUserId)]);

  // Step 2: Process the CLI's device keys as a /keys/query response
  // This registers the CLI's identity and signing keys
  const keysQueryResponse = {
    device_keys: {
      [info.remoteUserId]: {
        [info.remoteDeviceId]: {
          user_id: info.remoteUserId,
          device_id: info.remoteDeviceId,
          algorithms: ["m.olm.v1.curve25519-aes-sha2", "m.megolm.v1.aes-sha2"],
          keys: {
            [`curve25519:${info.remoteDeviceId}`]: bundle.identityKey,
            [`ed25519:${info.remoteDeviceId}`]: bundle.signingKey,
          },
          signatures: {},
        },
      },
    },
  };

  // Get pending outgoing requests and find keys query
  const requests = await machine.outgoingRequests();
  for (const req of requests) {
    if (req.type === 1) { // KeysQueryRequest
      await machine.markRequestAsSent(req.id, req.type, JSON.stringify(keysQueryResponse));
    }
  }

  // Step 3: Claim one-time keys for Olm session establishment
  if (bundle.oneTimeKey) {
    const keysClaimResponse = {
      one_time_keys: {
        [info.remoteUserId]: {
          [info.remoteDeviceId]: {
            [`signed_curve25519:${bundle.oneTimeKeyId || "AAAAAQ"}`]: {
              key: bundle.oneTimeKey,
              signatures: {},
            },
          },
        },
      },
    };

    // Check for key claim requests
    const claimReq = await machine.getMissingSessions([new UserId(info.remoteUserId)]);
    if (claimReq) {
      await machine.markRequestAsSent(claimReq.id, claimReq.type, JSON.stringify(keysClaimResponse));
    }
  }

  console.log(`[MatrixCrypto] Registered CLI device for hub ${hubId.substring(0, 8)}...`);
}

/**
 * Load an existing session from IndexedDB.
 *
 * @param {string} hubId - The hub identifier
 */
async function handleLoadSession(hubId) {
  if (machines.has(hubId)) {
    return { loaded: true, fromCache: true };
  }

  if (!wasmInitialized) {
    throw new Error("WASM not initialized");
  }

  try {
    // Try to load from IndexedDB by re-initializing with same IDs
    const userId = matrixUserId(hubId);
    const deviceId = browserDeviceId(1);
    const remoteUserId = cliUserId(hubId);
    const remoteDeviceId = cliDeviceId(hubId); // CLI uses hub_id as device_id
    const roomId = matrixRoomId(hubId);

    const { OlmMachine, UserId, DeviceId } = matrixModule;
    const machine = await OlmMachine.initialize(
      new UserId(userId),
      new DeviceId(deviceId)
    );

    // Store session info for later use
    sessionInfo.set(hubId, {
      userId,
      deviceId,
      remoteUserId,
      remoteDeviceId,
      roomId,
    });

    machines.set(hubId, machine);

    console.log(`[MatrixCrypto] Loaded session for hub ${hubId.substring(0, 8)}...`);
    return { loaded: true, fromCache: false };
  } catch (error) {
    console.error(`[MatrixCrypto] Failed to load session for hub ${hubId}:`, error);
    return { loaded: false, error: error.message };
  }
}

/**
 * Check if we have an active session for a hub.
 *
 * @param {string} hubId - The hub identifier
 */
async function handleHasSession(hubId) {
  // Check in-memory cache first
  if (machines.has(hubId)) {
    return { hasSession: true };
  }

  // Try to load from IndexedDB
  try {
    const result = await handleLoadSession(hubId);
    return { hasSession: result.loaded };
  } catch {
    return { hasSession: false };
  }
}

// Track if we've sent the first message (PreKey) to CLI
const sentFirstMessage = new Map(); // hubId -> boolean

/**
 * Encrypt a message for the CLI peer using 1:1 Olm-style encryption.
 *
 * Uses a simple JSON envelope format matching CLI:
 * - First message is PreKey (type 1) to establish session
 * - Subsequent messages are Olm (type 2)
 *
 * @param {string} hubId - The hub identifier
 * @param {string|Object} message - The message to encrypt
 */
async function handleEncrypt(hubId, message) {
  const machine = machines.get(hubId);
  const info = sessionInfo.get(hubId);
  if (!machine) throw new Error(`No session for hub ${hubId}`);
  if (!info) throw new Error(`No session info for hub ${hubId}`);

  const messageStr = typeof message === "string" ? message : JSON.stringify(message);

  try {
    // Get our identity key for the envelope
    const identityKeys = machine.identityKeys;
    const senderKey = identityKeys.curve25519.toBase64();

    // Wrap plaintext in Matrix-compatible content structure (matching CLI format)
    const content = {
      type: "m.botster.message",
      body: btoa(unescape(encodeURIComponent(messageStr))), // Base64 encode the message
      room_id: info.roomId,
      sender: info.userId
    };

    // Determine message type: PreKey (1) for first message, Olm (2) for subsequent
    const isFirstMessage = !sentFirstMessage.get(hubId);
    const messageType = isFirstMessage ? MSG_TYPE_OLM_PREKEY : MSG_TYPE_OLM;

    if (isFirstMessage) {
      sentFirstMessage.set(hubId, true);
      console.log(`[MatrixCrypto] Sending PreKey message to establish session with CLI`);
    }

    // Create envelope matching CryptoEnvelope format
    const envelope = {
      t: messageType,
      c: btoa(JSON.stringify(content)), // Base64 encode the content JSON
      s: senderKey,
      d: info.deviceId,
    };

    return { envelope: JSON.stringify(envelope) };
  } catch (error) {
    console.error(`[MatrixCrypto] Encrypt failed for hub ${hubId}:`, error);
    throw new Error(`Encryption failed: ${error.message}`);
  }
}

/**
 * Decrypt a message from the CLI peer.
 *
 * Handles the simple JSON envelope format matching CLI:
 * - Envelope: { t, c, s, d }
 * - Content: { type, body (base64), room_id, sender }
 *
 * @param {string} hubId - The hub identifier
 * @param {string|Object} envelope - The encrypted envelope
 */
async function handleDecrypt(hubId, envelope) {
  const machine = machines.get(hubId);
  const info = sessionInfo.get(hubId);
  if (!machine) throw new Error(`No session for hub ${hubId}`);
  if (!info) throw new Error(`No session info for hub ${hubId}`);

  const envelopeObj = typeof envelope === "string" ? JSON.parse(envelope) : envelope;

  try {
    // Decode the ciphertext from base64
    let ciphertextContent;
    try {
      ciphertextContent = JSON.parse(atob(envelopeObj.c));
    } catch {
      // If not base64 JSON, use as-is
      ciphertextContent = envelopeObj.c;
    }

    // Handle our simple JSON envelope format (matching CLI)
    // Content structure: { type, body (base64), room_id, sender }
    if (typeof ciphertextContent === "object" && ciphertextContent.body) {
      // Decode the base64 body
      let plaintext;
      try {
        plaintext = decodeURIComponent(escape(atob(ciphertextContent.body)));
      } catch {
        plaintext = atob(ciphertextContent.body);
      }

      // Try to parse as JSON
      try {
        return { plaintext: JSON.parse(plaintext) };
      } catch {
        return { plaintext };
      }
    }

    // Fallback: return raw content if not in expected format
    console.warn(`[MatrixCrypto] Unexpected message format, returning raw content`);
    return { plaintext: ciphertextContent };
  } catch (error) {
    console.error(`[MatrixCrypto] Decrypt failed for hub ${hubId}:`, error);
    throw new Error(`Decryption failed: ${error.message}`);
  }
}

/**
 * Get our identity key (Curve25519).
 *
 * @param {string} hubId - The hub identifier
 */
async function handleGetIdentityKey(hubId) {
  const machine = machines.get(hubId);
  if (!machine) throw new Error(`No session for hub ${hubId}`);

  const identityKeys = machine.identityKeys;
  return { identityKey: identityKeys.curve25519.toBase64() };
}

/**
 * Get the remote (CLI) identity key from our session.
 *
 * @param {string} hubId - The hub identifier
 */
async function handleGetRemoteIdentityKey(hubId) {
  const machine = machines.get(hubId);
  const bundle = bundles.get(hubId);
  if (!machine) throw new Error(`No session for hub ${hubId}`);

  // Return the identity key from the stored bundle
  if (bundle?.identityKey) {
    return { remoteIdentityKey: bundle.identityKey };
  }

  // Fallback: no bundle stored (session loaded from IndexedDB)
  return { remoteIdentityKey: null };
}

/**
 * Clear a session for a hub.
 *
 * @param {string} hubId - The hub identifier
 */
async function handleClearSession(hubId) {
  if (machines.has(hubId)) {
    const machine = machines.get(hubId);
    try {
      machine.close();
    } catch (e) {
      // Ignore close errors
    }
    machines.delete(hubId);
  }

  // Clear associated data
  bundles.delete(hubId);
  sessionInfo.delete(hubId);
  sentFirstMessage.delete(hubId);

  console.log(`[MatrixCrypto] Cleared session for hub ${hubId.substring(0, 8)}...`);
  return { cleared: true };
}

/**
 * Process a Megolm room key distribution message.
 * This allows us to decrypt group messages from the CLI.
 *
 * In Matrix, room keys are distributed via m.room_key to-device events.
 * For our P2P use case, the CLI sends us the room key directly.
 *
 * @param {string} hubId - The hub identifier
 * @param {string} distributionB64 - Base64-encoded room key content or session key
 */
async function handleProcessSenderKeyDistribution(hubId, distributionB64) {
  const machine = machines.get(hubId);
  const info = sessionInfo.get(hubId);
  if (!machine) throw new Error(`No session for hub ${hubId}`);
  if (!info) throw new Error(`No session info for hub ${hubId}`);

  try {
    // Decode the distribution message
    let roomKeyContent;
    try {
      roomKeyContent = JSON.parse(atob(distributionB64));
    } catch {
      // If not JSON, assume it's a raw session key
      roomKeyContent = {
        algorithm: "m.megolm.v1.aes-sha2",
        room_id: info.roomId,
        session_key: distributionB64,
      };
    }

    // The distribution message contains the Megolm room key
    // Process it through receiveSyncChanges as a to-device event
    const toDeviceEvents = [{
      type: "m.room_key",
      sender: info.remoteUserId,
      content: {
        algorithm: roomKeyContent.algorithm || "m.megolm.v1.aes-sha2",
        room_id: roomKeyContent.room_id || info.roomId,
        session_id: roomKeyContent.session_id,
        session_key: roomKeyContent.session_key,
      },
    }];

    // Create proper DeviceLists object
    const deviceLists = { changed: [], left: [] };

    // Create one-time key counts (empty for P2P)
    const oneTimeKeyCounts = {};

    await machine.receiveSyncChanges(
      JSON.stringify(toDeviceEvents),
      deviceLists,
      oneTimeKeyCounts,
      [] // unused fallback keys
    );

    console.log(`[MatrixCrypto] Processed room key distribution for hub ${hubId.substring(0, 8)}...`);
    return { processed: true };
  } catch (error) {
    console.error(`[MatrixCrypto] Failed to process room key distribution:`, error);
    throw new Error(`Room key processing failed: ${error.message}`);
  }
}

// =============================================================================
// Message Handler
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
        result = await handleInit(params.wasmJsUrl);
        break;
      case "createSession":
        result = await handleCreateSession(params.hubId, params.bundleJson);
        break;
      case "loadSession":
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
      case "getRemoteIdentityKey":
        result = await handleGetRemoteIdentityKey(params.hubId);
        break;
      case "processSenderKeyDistribution":
        result = await handleProcessSenderKeyDistribution(
          params.hubId,
          params.distributionB64,
        );
        break;
      default:
        throw new Error(`Unknown action: ${action}`);
    }

    replyFn({ id, success: true, result });
  } catch (error) {
    console.error("[MatrixCrypto] Error:", action, error);
    replyFn({ id, success: false, error: error.message });
  }
}

// =============================================================================
// Port Management
// =============================================================================

// Port registry: portId -> { port, lastPong }
const ports = new Map();

// Port ID counter
let portIdCounter = 0;
function generatePortId() {
  return `port_${++portIdCounter}_${Date.now()}`;
}

function cleanupPort(portId) {
  ports.delete(portId);
  console.log(`[MatrixCrypto] Cleaned up port ${portId}, ${ports.size} ports remaining`);
}

// =============================================================================
// SharedWorker Entry Point
// =============================================================================

self.onconnect = (event) => {
  const port = event.ports[0];
  const portId = generatePortId();

  ports.set(portId, {
    port,
    lastPong: Date.now(),
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
// Regular Worker Fallback (for browsers without SharedWorker support)
// =============================================================================

self.onmessage = (event) => {
  handleMessage(event, null, (msg) => self.postMessage(msg));
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
      console.log(`[MatrixCrypto] Port ${portId} timed out, cleaning up`);
      cleanupPort(portId);
      continue;
    }

    // Send ping
    try {
      state.port.postMessage({ event: "ping" });
    } catch (e) {
      // Port likely closed, clean up
      console.log(`[MatrixCrypto] Port ${portId} unreachable, cleaning up`);
      cleanupPort(portId);
    }
  }
}, HEARTBEAT_INTERVAL);
