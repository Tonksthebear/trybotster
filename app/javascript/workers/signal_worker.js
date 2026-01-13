/**
 * Signal Protocol Web Worker
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

// WASM module (loaded on init)
let wasmModule = null;

// In-memory session cache (hubId -> SignalSession)
const sessions = new Map();

// IndexedDB config
const DB_NAME = "botster_signal";
const DB_VERSION = 2;
const STORE_NAME = "sessions";
const KEY_STORE_NAME = "encryption_keys";
const WRAPPING_KEY_ID = "session_wrapping_key";

// Cached wrapping key (non-extractable CryptoKey)
let wrappingKeyCache = null;

// =============================================================================
// Message Handler
// =============================================================================

self.onmessage = async (event) => {
  const { id, action, ...params } = event.data;

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
        result = await handleEncrypt(params.hubId, params.message);
        break;
      case "decrypt":
        result = await handleDecrypt(params.hubId, params.envelope);
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
          params.distributionB64
        );
        break;
      default:
        throw new Error(`Unknown action: ${action}`);
    }

    self.postMessage({ id, success: true, result });
  } catch (error) {
    console.error("[SignalWorker] Error:", error);
    self.postMessage({ id, success: false, error: error.message });
  }
};

// =============================================================================
// Action Handlers
// =============================================================================

async function handleInit(wasmJsUrl, wasmBinaryUrl) {
  if (wasmModule) return { alreadyInitialized: true };

  // Import WASM JS glue code and initialize with binary
  const module = await import(wasmJsUrl);
  await module.default(wasmBinaryUrl);
  wasmModule = module;

  console.log("[SignalWorker] WASM initialized:", module.ping());
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
  const pickled = await loadSessionFromStorage(hubId);
  if (!pickled) {
    return { loaded: false };
  }

  if (!wasmModule) throw new Error("WASM not initialized");

  try {
    const wasmSession = wasmModule.SignalSession.from_pickle(pickled);
    sessions.set(hubId, wasmSession);
    return { loaded: true, fromCache: false };
  } catch (error) {
    console.warn("[SignalWorker] Failed to restore session:", error);
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

  // Try to load existing key
  const existingKey = await new Promise((resolve, reject) => {
    const tx = db.transaction(KEY_STORE_NAME, "readonly");
    const store = tx.objectStore(KEY_STORE_NAME);
    const request = store.get(WRAPPING_KEY_ID);
    request.onerror = () => reject(request.error);
    request.onsuccess = () => resolve(request.result?.key || null);
  });

  if (existingKey) {
    console.log("[SignalWorker] Loaded existing wrapping key");
    wrappingKeyCache = existingKey;
    return existingKey;
  }

  // Generate new non-extractable key
  console.log("[SignalWorker] Generating new wrapping key");
  const newKey = await crypto.subtle.generateKey(
    { name: "AES-GCM", length: 256 },
    false, // NON-EXTRACTABLE - XSS cannot export this
    ["encrypt", "decrypt"]
  );

  // Store the CryptoKey object directly (IndexedDB supports structured clone)
  await new Promise((resolve, reject) => {
    const tx = db.transaction(KEY_STORE_NAME, "readwrite");
    const store = tx.objectStore(KEY_STORE_NAME);
    const request = store.put({ id: WRAPPING_KEY_ID, key: newKey });
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
    encoded
  );

  return { iv, ciphertext };
}

async function decryptWithWrappingKey(iv, ciphertext) {
  const key = await getOrCreateWrappingKey();

  const decrypted = await crypto.subtle.decrypt(
    { name: "AES-GCM", iv },
    key,
    ciphertext
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
    console.warn("[SignalWorker] Failed to persist session:", error);
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

  if (!record) return null;

  try {
    const iv = new Uint8Array(record.iv);
    return await decryptWithWrappingKey(iv, record.ciphertext);
  } catch (error) {
    console.error("[SignalWorker] Failed to decrypt session:", error);
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
