/**
 * Signal Protocol E2E Encryption - Main Thread Proxy
 *
 * This module provides a proxy to the Signal Web Worker.
 * All sensitive operations (crypto keys, session state) are isolated
 * in the worker to protect against XSS attacks.
 *
 * Security model:
 * - Non-extractable CryptoKey lives only in worker
 * - Decrypted Signal session state lives only in worker
 * - Main thread only sees: encrypted envelopes and decrypted messages
 * - XSS can use the session while tab is open, but cannot steal it
 */

// Worker instance (initialized on first use)
let worker = null;
let workerReady = false;
let initPromise = null;

// Pending request callbacks (id -> {resolve, reject})
const pendingRequests = new Map();
let nextRequestId = 1;

/**
 * Initialize the Signal worker with WASM module.
 *
 * @param {string} workerUrl - URL to workers/signal.js (from asset_path)
 * @param {string} wasmJsUrl - URL to libsignal_wasm.js (from asset_path)
 * @param {string} wasmBinaryUrl - URL to libsignal_wasm_bg.wasm (from asset_path)
 */
export async function initSignal(workerUrl, wasmJsUrl, wasmBinaryUrl) {
  if (workerReady) return;
  if (initPromise) return initPromise;

  initPromise = (async () => {
    try {
      // Spawn worker
      worker = new Worker(workerUrl, { type: "module" });

      // Set up message handler
      worker.onmessage = handleWorkerMessage;
      worker.onerror = (e) => console.error("[Signal] Worker error:", e);

      // Initialize WASM in worker
      await sendToWorker("init", { wasmJsUrl, wasmBinaryUrl });

      workerReady = true;
      console.log("[Signal] Worker initialized");
    } catch (error) {
      console.error("[Signal] Failed to initialize worker:", error);
      initPromise = null;
      throw error;
    }
  })();

  return initPromise;
}

/**
 * Handle messages from the worker.
 */
function handleWorkerMessage(event) {
  const { id, success, result, error } = event.data;

  const pending = pendingRequests.get(id);
  if (!pending) {
    console.warn("[Signal] Received response for unknown request:", id);
    return;
  }

  pendingRequests.delete(id);

  if (success) {
    pending.resolve(result);
  } else {
    pending.reject(new Error(error));
  }
}

/**
 * Send a request to the worker and wait for response.
 */
function sendToWorker(action, params = {}) {
  return new Promise((resolve, reject) => {
    const id = nextRequestId++;
    pendingRequests.set(id, { resolve, reject });
    worker.postMessage({ id, action, ...params });
  });
}

/**
 * Decode Base32 (RFC 4648) to Uint8Array.
 * Used for QR code URLs which use Base32 for alphanumeric mode efficiency.
 */
function base32Decode(base32) {
  base32 = base32
    .toUpperCase()
    .replace(/=+$/, "")
    .replace(/[^A-Z2-7]/g, "");

  const alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
  let bits = "";
  for (const c of base32) {
    const i = alphabet.indexOf(c);
    if (i < 0) throw new Error(`Invalid Base32 character: ${c}`);
    bits += i.toString(2).padStart(5, "0");
  }

  const byteCount = Math.floor(bits.length / 8);
  const bytes = new Uint8Array(byteCount);
  for (let i = 0; i < byteCount; i++) {
    bytes[i] = parseInt(bits.slice(i * 8, i * 8 + 8), 2);
  }
  return bytes;
}

/**
 * Convert Uint8Array to Base64 string.
 */
function bytesToBase64(bytes) {
  let binary = "";
  for (let i = 0; i < bytes.length; i++) {
    binary += String.fromCharCode(bytes[i]);
  }
  return btoa(binary);
}

/**
 * Read little-endian u32 from byte array.
 */
function readU32LE(bytes, offset) {
  return (
    (bytes[offset] |
      (bytes[offset + 1] << 8) |
      (bytes[offset + 2] << 16) |
      (bytes[offset + 3] << 24)) >>>
    0
  );
}

/**
 * Parse binary PreKeyBundle format from CLI.
 *
 * Binary format (1813 bytes total):
 * - version: 1 byte
 * - registration_id: 4 bytes (LE)
 * - identity_key: 33 bytes
 * - signed_prekey_id: 4 bytes (LE)
 * - signed_prekey: 33 bytes
 * - signed_prekey_signature: 64 bytes
 * - prekey_id: 4 bytes (LE)
 * - prekey: 33 bytes
 * - kyber_prekey_id: 4 bytes (LE)
 * - kyber_prekey: 1569 bytes
 * - kyber_prekey_signature: 64 bytes
 */
function parseBinaryBundle(bytes) {
  const VERSION_OFFSET = 0;
  const REGISTRATION_ID_OFFSET = 1;
  const IDENTITY_KEY_OFFSET = 5;
  const SIGNED_PREKEY_ID_OFFSET = 38;
  const SIGNED_PREKEY_OFFSET = 42;
  const SIGNED_PREKEY_SIG_OFFSET = 75;
  const PREKEY_ID_OFFSET = 139;
  const PREKEY_OFFSET = 143;
  const KYBER_PREKEY_ID_OFFSET = 176;
  const KYBER_PREKEY_OFFSET = 180;
  const KYBER_PREKEY_SIG_OFFSET = 1749;
  const TOTAL_SIZE = 1813;

  if (bytes.length !== TOTAL_SIZE) {
    throw new Error(
      `Invalid bundle size: ${bytes.length}, expected ${TOTAL_SIZE}`,
    );
  }

  const bundle = {
    version: bytes[VERSION_OFFSET],
    registration_id: readU32LE(bytes, REGISTRATION_ID_OFFSET),
    identity_key: bytesToBase64(
      bytes.slice(IDENTITY_KEY_OFFSET, IDENTITY_KEY_OFFSET + 33),
    ),
    signed_prekey_id: readU32LE(bytes, SIGNED_PREKEY_ID_OFFSET),
    signed_prekey: bytesToBase64(
      bytes.slice(SIGNED_PREKEY_OFFSET, SIGNED_PREKEY_OFFSET + 33),
    ),
    signed_prekey_signature: bytesToBase64(
      bytes.slice(SIGNED_PREKEY_SIG_OFFSET, SIGNED_PREKEY_SIG_OFFSET + 64),
    ),
    prekey_id: readU32LE(bytes, PREKEY_ID_OFFSET),
    prekey: bytesToBase64(bytes.slice(PREKEY_OFFSET, PREKEY_OFFSET + 33)),
    kyber_prekey_id: readU32LE(bytes, KYBER_PREKEY_ID_OFFSET),
    kyber_prekey: bytesToBase64(
      bytes.slice(KYBER_PREKEY_OFFSET, KYBER_PREKEY_OFFSET + 1569),
    ),
    kyber_prekey_signature: bytesToBase64(
      bytes.slice(KYBER_PREKEY_SIG_OFFSET, KYBER_PREKEY_SIG_OFFSET + 64),
    ),
  };

  if (bundle.prekey_id === 0) {
    bundle.prekey_id = null;
    bundle.prekey = null;
  }

  return bundle;
}

/**
 * Parse PreKeyBundle from URL fragment.
 * Expected format: #<base32_binary>
 */
export function parseBundleFromFragment() {
  const hash = window.location.hash;
  console.log("[Signal] Parsing fragment, hash length:", hash?.length || 0);

  if (!hash) {
    console.log("[Signal] No hash in URL");
    return null;
  }

  const base32Data = hash.startsWith("#") ? hash.slice(1) : hash;
  if (!base32Data || base32Data.length < 100) {
    console.log("[Signal] No valid bundle in hash");
    return null;
  }

  console.log("[Signal] Found bundle, Base32 length:", base32Data.length);

  try {
    const bytes = base32Decode(base32Data);
    console.log("[Signal] Decoded to", bytes.length, "bytes");

    const bundle = parseBinaryBundle(bytes);

    const hubMatch = window.location.pathname.match(/\/hubs\/([^\/]+)/);
    bundle.hub_id = hubMatch ? hubMatch[1] : "";
    bundle.device_id = 1;

    console.log(
      "[Signal] Successfully parsed binary bundle, version:",
      bundle.version,
      "hub:",
      bundle.hub_id,
    );
    return bundle;
  } catch (error) {
    console.error("[Signal] Failed to parse bundle from fragment:", error);
    return null;
  }
}

/**
 * Get hub identifier from URL path.
 */
export function getHubIdFromPath() {
  const match = window.location.pathname.match(/\/hubs\/([^\/]+)/);
  return match ? match[1] : null;
}

/**
 * Signal Protocol session proxy.
 *
 * This class proxies all operations to the Web Worker.
 * The actual session state never exists in the main thread.
 */
export class SignalSession {
  constructor(hubId, identityKey) {
    this._hubId = hubId;
    this._identityKey = identityKey;
  }

  /**
   * Create a new session from a PreKeyBundle.
   */
  static async create(bundleJson, hubId) {
    const result = await sendToWorker("createSession", { bundleJson, hubId });
    return new SignalSession(hubId, result.identityKey);
  }

  /**
   * Load an existing session from storage.
   * Returns null if no session exists.
   */
  static async load(hubId) {
    const result = await sendToWorker("loadSession", { hubId });
    if (!result.loaded) return null;

    const keyResult = await sendToWorker("getIdentityKey", { hubId });
    return new SignalSession(hubId, keyResult.identityKey);
  }

  /**
   * Load existing session or create new from bundle.
   */
  static async loadOrCreate(bundleJson, hubId) {
    const existing = await SignalSession.load(hubId);
    if (existing) {
      console.log("[Signal] Restored existing session for hub:", hubId);
      return existing;
    }

    console.log("[Signal] Creating new session for hub:", hubId);
    return SignalSession.create(bundleJson, hubId);
  }

  /**
   * Check if a session exists for a hub.
   */
  static async hasSession(hubId) {
    const result = await sendToWorker("hasSession", { hubId });
    return result.hasSession;
  }

  /**
   * Encrypt a message for the CLI.
   */
  async encrypt(message) {
    const result = await sendToWorker("encrypt", {
      hubId: this._hubId,
      message,
    });
    return result.envelope;
  }

  /**
   * Decrypt a message from the CLI.
   */
  async decrypt(envelope) {
    const result = await sendToWorker("decrypt", {
      hubId: this._hubId,
      envelope,
    });
    return result.plaintext;
  }

  /**
   * Process a SenderKey distribution message from CLI.
   */
  async processSenderKeyDistribution(distributionB64) {
    await sendToWorker("processSenderKeyDistribution", {
      hubId: this._hubId,
      distributionB64,
    });
  }

  /**
   * Get our identity public key (base64).
   */
  async getIdentityKey() {
    return this._identityKey;
  }

  /**
   * Get the hub ID this session is connected to.
   */
  getHubId() {
    return this._hubId;
  }

  /**
   * Clear session from storage.
   */
  async clear() {
    await sendToWorker("clearSession", { hubId: this._hubId });
  }
}

/**
 * Connection states for UI feedback.
 */
export const ConnectionState = {
  DISCONNECTED: "disconnected",
  LOADING_WASM: "loading_wasm",
  CREATING_SESSION: "creating_session",
  SUBSCRIBING: "subscribing",
  CHANNEL_CONNECTED: "channel_connected",
  HANDSHAKE_SENT: "handshake_sent",
  CONNECTED: "connected",
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
  HANDSHAKE_TIMEOUT: "handshake_timeout",
  HANDSHAKE_FAILED: "handshake_failed",
  DECRYPT_FAILED: "decrypt_failed",
  WEBSOCKET_ERROR: "websocket_error",
  SESSION_INVALID: "session_invalid", // CLI restarted, keys don't match
};

export default {
  initSignal,
  parseBundleFromFragment,
  getHubIdFromPath,
  SignalSession,
  ConnectionState,
  ConnectionError,
};
