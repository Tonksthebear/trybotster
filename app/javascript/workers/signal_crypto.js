/**
 * Signal Crypto SharedWorker
 *
 * Pure cryptographic operations for Signal Protocol sessions.
 * Extracted from signal.js - this worker handles only:
 * - WASM module loading
 * - Session creation/loading/persistence
 * - Encrypt/decrypt operations
 *
 * No ActionCable, no reliable delivery, no subscriptions.
 */

// WASM module (loaded on init)
let wasmModule = null;

// In-memory session cache (hubId -> wasmSession)
const sessions = new Map();

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
// IndexedDB Configuration
// =============================================================================

// Note: Keys stored as JWK (not CryptoKey) for Safari SharedWorker compatibility (WebKit #177350)
const DB_NAME = "botster";
const DB_VERSION = 1;
const STORE_NAME = "sessions";
const KEY_STORE_NAME = "encryption_keys";
const WRAPPING_KEY_ID = "session_wrapping_key";

// Cached wrapping key (non-extractable CryptoKey)
let wrappingKeyCache = null;

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
    console.error(`[SignalCrypto] persistSession failed for ${hubId}:`, error);
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

// =============================================================================
// Crypto Action Handlers
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

  // Parse envelope to check sender
  const envelopeObj = typeof envelope === "string" ? JSON.parse(envelope) : envelope;

  let plaintext;
  try {
    plaintext = await session.decrypt(envelopeStr);
  } catch (decryptError) {
    // Try to get session's expected remote identity for comparison
    let expectedRemote = "unknown";
    try {
      expectedRemote = await session.get_remote_identity_key();
    } catch (e) {
      // Method might not exist
    }

    // Log detailed error info for debugging
    console.error("[SignalCrypto] Decrypt failed:", {
      hubId,
      error: decryptError.message || decryptError.toString(),
      envelopeType: envelopeObj.t,
      senderPrefix: envelopeObj.s?.substring(0, 30),
      expectedRemotePrefix: expectedRemote?.substring?.(0, 30) || expectedRemote,
      ciphertextLength: envelopeObj.c?.length,
    });
    throw new Error(`Decrypt failed: ${decryptError.message || decryptError.toString()}`);
  }

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

async function handleGetRemoteIdentityKey(hubId) {
  const session = sessions.get(hubId);
  if (!session) throw new Error(`No session for hub ${hubId}`);

  // Get the remote (CLI) identity key that this session expects
  const remoteKey = await session.get_remote_identity_key();
  return { remoteIdentityKey: remoteKey };
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
        result = await handleInit(params.wasmJsUrl, params.wasmBinaryUrl);
        break;
      case "createSession":
        result = await handleCreateSession(params.bundleJson, params.hubId);
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
    console.error("[SignalCrypto] Error:", action, error);
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
  console.log(`[SignalCrypto] Cleaned up port ${portId}, ${ports.size} ports remaining`);
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
      console.log(`[SignalCrypto] Port ${portId} timed out, cleaning up`);
      cleanupPort(portId);
      continue;
    }

    // Send ping
    try {
      state.port.postMessage({ event: "ping" });
    } catch (e) {
      // Port likely closed, clean up
      console.log(`[SignalCrypto] Port ${portId} unreachable, cleaning up`);
      cleanupPort(portId);
    }
  }
}, HEARTBEAT_INTERVAL);
