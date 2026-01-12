/**
 * Signal Protocol E2E Encryption for Browser-CLI Communication
 *
 * This module provides a wrapper around libsignal-wasm for E2E encryption
 * using the Signal Protocol (X3DH + Double Ratchet).
 *
 * Architecture:
 * - CLI publishes PreKeyBundle via QR code
 * - Browser scans QR, creates session from bundle
 * - All messages are encrypted before sending to Rails
 * - Rails is a pure relay (cannot decrypt)
 */

// WASM module state
let wasmModule = null;
let wasmInitPromise = null;

// IndexedDB for session persistence
const DB_NAME = "botster_signal";
const DB_VERSION = 1;
const STORE_NAME = "sessions";

/**
 * Initialize the Signal WASM module.
 * Call this once before creating sessions.
 *
 * @param {string} wasmJsPath - Path to libsignal_wasm.js
 * @param {string} wasmBgPath - Path to libsignal_wasm_bg.wasm
 */
export async function initSignal(wasmJsPath, wasmBgPath) {
  if (wasmModule) return wasmModule;

  if (wasmInitPromise) return wasmInitPromise;

  wasmInitPromise = (async () => {
    try {
      // Dynamic import of the WASM module using provided path
      const module = await import(wasmJsPath);
      await module.default(wasmBgPath);
      wasmModule = module;
      console.log("[Signal] WASM module initialized:", module.ping());
      return module;
    } catch (error) {
      console.error("[Signal] Failed to initialize WASM:", error);
      wasmInitPromise = null;
      throw error;
    }
  })();

  return wasmInitPromise;
}

/**
 * Parse PreKeyBundle from URL fragment.
 * Expected format: #bundle=<base64_json>
 *
 * Handles both URL-safe base64 (-/_) and standard base64 (+/).
 */
export function parseBundleFromFragment() {
  const hash = window.location.hash;
  console.log("[Signal] Parsing fragment, hash length:", hash?.length || 0);

  if (!hash) {
    console.log("[Signal] No hash in URL");
    return null;
  }

  const match = hash.match(/bundle=([^&]+)/);
  if (!match) {
    console.log("[Signal] No bundle= found in hash");
    return null;
  }

  console.log("[Signal] Found bundle, length:", match[1].length);

  try {
    // Convert URL-safe base64 to standard base64 (handles both formats)
    let base64 = match[1].replace(/-/g, "+").replace(/_/g, "/");

    // Add padding if needed
    const padding = base64.length % 4;
    if (padding === 2) base64 += "==";
    else if (padding === 3) base64 += "=";

    const decoded = atob(base64);
    const bundle = JSON.parse(decoded);
    console.log("[Signal] Successfully parsed bundle for hub:", bundle.hub_id);
    return bundle;
  } catch (error) {
    console.error("[Signal] Failed to parse bundle from fragment:", error);
    return null;
  }
}

/**
 * Get hub identifier from URL path.
 * Expected: /hubs/{hub_id}
 */
export function getHubIdFromPath() {
  const match = window.location.pathname.match(/\/hubs\/([^\/]+)/);
  return match ? match[1] : null;
}

/**
 * Open IndexedDB for session storage.
 */
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
    };
  });
}

/**
 * Save pickled session to IndexedDB.
 */
async function saveSession(hubId, pickled) {
  const db = await openDatabase();
  return new Promise((resolve, reject) => {
    const tx = db.transaction(STORE_NAME, "readwrite");
    const store = tx.objectStore(STORE_NAME);
    const request = store.put({ hubId, pickled, updatedAt: Date.now() });
    request.onerror = () => reject(request.error);
    request.onsuccess = () => resolve();
  });
}

/**
 * Load pickled session from IndexedDB.
 */
async function loadSession(hubId) {
  const db = await openDatabase();
  return new Promise((resolve, reject) => {
    const tx = db.transaction(STORE_NAME, "readonly");
    const store = tx.objectStore(STORE_NAME);
    const request = store.get(hubId);
    request.onerror = () => reject(request.error);
    request.onsuccess = () => resolve(request.result?.pickled || null);
  });
}

/**
 * Delete session from IndexedDB.
 */
async function deleteSession(hubId) {
  const db = await openDatabase();
  return new Promise((resolve, reject) => {
    const tx = db.transaction(STORE_NAME, "readwrite");
    const store = tx.objectStore(STORE_NAME);
    const request = store.delete(hubId);
    request.onerror = () => reject(request.error);
    request.onsuccess = () => resolve();
  });
}

/**
 * Signal Protocol session wrapper.
 *
 * Provides encrypt/decrypt with automatic session persistence.
 */
export class SignalSession {
  constructor(wasmSession, hubId) {
    this._session = wasmSession;
    this._hubId = hubId;
  }

  /**
   * Create a new session from a PreKeyBundle.
   * This performs X3DH key agreement.
   * Clears any existing session for this hub first.
   */
  static async create(bundleJson, hubId) {
    // Clear any existing stale session first
    await deleteSession(hubId);

    const module = await initSignal();
    const bundleStr =
      typeof bundleJson === "string" ? bundleJson : JSON.stringify(bundleJson);

    // WASM uses #[wasm_bindgen(constructor)] so call with 'new'
    const wasmSession = await new module.SignalSession(bundleStr);
    const session = new SignalSession(wasmSession, hubId);

    // Persist immediately
    await session.persist();

    return session;
  }

  /**
   * Load an existing session from IndexedDB.
   * Returns null if no session exists.
   */
  static async load(hubId) {
    const pickled = await loadSession(hubId);
    if (!pickled) return null;

    try {
      const module = await initSignal();
      const wasmSession = module.SignalSession.from_pickle(pickled);
      return new SignalSession(wasmSession, hubId);
    } catch (error) {
      console.warn("[Signal] Failed to restore session:", error);
      await deleteSession(hubId);
      return null;
    }
  }

  /**
   * Load existing session or create new from bundle.
   */
  static async loadOrCreate(bundleJson, hubId) {
    // Try to load existing session first
    const existing = await SignalSession.load(hubId);
    if (existing) {
      console.log("[Signal] Restored existing session for hub:", hubId);
      return existing;
    }

    // Create new session from bundle
    console.log("[Signal] Creating new session for hub:", hubId);
    return SignalSession.create(bundleJson, hubId);
  }

  /**
   * Encrypt a message for the CLI.
   * Returns SignalEnvelope JSON string.
   */
  async encrypt(message) {
    const messageStr =
      typeof message === "string" ? message : JSON.stringify(message);
    const envelope = await this._session.encrypt(messageStr);

    // Persist after encryption (Double Ratchet state changed)
    await this.persist();

    return envelope;
  }

  /**
   * Decrypt a message from the CLI.
   * Takes SignalEnvelope JSON string, returns decrypted message.
   */
  async decrypt(envelopeJson) {
    const envelopeStr =
      typeof envelopeJson === "string"
        ? envelopeJson
        : JSON.stringify(envelopeJson);
    const plaintext = await this._session.decrypt(envelopeStr);

    // Persist after decryption (Double Ratchet state changed)
    await this.persist();

    // Try to parse as JSON
    try {
      return JSON.parse(plaintext);
    } catch {
      return plaintext;
    }
  }

  /**
   * Process a SenderKey distribution message from CLI.
   * Call this when you receive a sender_key_distribution message.
   */
  async processSenderKeyDistribution(distributionB64) {
    await this._session.process_sender_key_distribution(distributionB64);
    await this.persist();
  }

  /**
   * Get our identity public key (base64).
   */
  async getIdentityKey() {
    return await this._session.get_identity_key();
  }

  /**
   * Get the hub ID this session is connected to.
   */
  getHubId() {
    return this._session.get_hub_id();
  }

  /**
   * Persist session to IndexedDB.
   */
  async persist() {
    try {
      const pickled = this._session.pickle();
      await saveSession(this._hubId, pickled);
    } catch (error) {
      console.warn("[Signal] Failed to persist session:", error);
    }
  }

  /**
   * Clear session from storage and memory.
   */
  async clear() {
    await deleteSession(this._hubId);
    this._session = null;
  }
}

/**
 * Connection states for UI feedback.
 *
 * Flow: DISCONNECTED -> LOADING_WASM -> CREATING_SESSION -> SUBSCRIBING
 *       -> CHANNEL_CONNECTED -> HANDSHAKE_SENT -> CONNECTED
 *
 * Errors can occur at any stage with specific reasons.
 */
export const ConnectionState = {
  DISCONNECTED: "disconnected",
  LOADING_WASM: "loading_wasm",
  CREATING_SESSION: "creating_session",
  SUBSCRIBING: "subscribing",
  CHANNEL_CONNECTED: "channel_connected", // Action Cable confirmed, CLI reachable
  HANDSHAKE_SENT: "handshake_sent", // Sent handshake, waiting for CLI ACK
  CONNECTED: "connected", // CLI acknowledged, E2E active
  ERROR: "error",
};

/**
 * Error reasons for connection failures.
 */
export const ConnectionError = {
  WASM_LOAD_FAILED: "wasm_load_failed",
  NO_BUNDLE: "no_bundle",
  SESSION_CREATE_FAILED: "session_create_failed",
  SUBSCRIBE_REJECTED: "subscribe_rejected",
  HANDSHAKE_TIMEOUT: "handshake_timeout", // CLI didn't ACK - likely stale session
  HANDSHAKE_FAILED: "handshake_failed", // CLI explicitly rejected
  DECRYPT_FAILED: "decrypt_failed",
  WEBSOCKET_ERROR: "websocket_error",
};

export default {
  initSignal,
  parseBundleFromFragment,
  getHubIdFromPath,
  SignalSession,
  ConnectionState,
  ConnectionError,
};
