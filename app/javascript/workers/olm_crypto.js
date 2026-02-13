/**
 * Vodozemac Crypto SharedWorker
 *
 * Pure Olm cryptographic operations using vodozemac-wasm.
 * Replaces matrix-sdk-crypto-wasm with direct vodozemac Account/Session.
 *
 * State per hub:
 *   accounts  - VodozemacAccount (identity keys, session creation)
 *   sessions  - VodozemacSession (encrypt/decrypt)
 *   bundles   - parsed CLI bundle (identity_key, one_time_key)
 *
 * Persistence:
 *   Sessions are pickled and stored in IndexedDB so they survive
 *   SharedWorker restarts (all tabs closed). A random 32-byte pickle
 *   key is generated once and stored alongside the pickled data.
 *
 * Wire format (OlmEnvelope):
 *   PreKey:  { t: 0, b: "<base64 ciphertext>", k: "<sender curve25519 key>" }
 *   Normal:  { t: 1, b: "<base64 ciphertext>" }
 */

// WASM module state
let wasmModule = null

// Per-hub crypto state (in-memory, restored from IndexedDB on demand)
const accounts = new Map()  // hubId -> VodozemacAccount
const sessions = new Map()  // hubId -> VodozemacSession
const bundles  = new Map()  // hubId -> parsed CLI bundle

// =============================================================================
// Base64 helpers
// =============================================================================

function bytesToBase64(bytes) {
  // Spread avoids O(n) string concatenation in a loop
  return btoa(String.fromCharCode(...bytes)).replace(/=+$/, "")
}

function base64ToBytes(b64) {
  const binary = atob(b64)
  return Uint8Array.from(binary, c => c.charCodeAt(0))
}

// =============================================================================
// IndexedDB Persistence
// =============================================================================

const DB_NAME = "vodozemac-crypto"
const DB_VERSION = 1
const STORE_NAME = "sessions"
const PICKLE_KEY_ID = "__pickle_key__"

let dbInstance = null

function openDB() {
  if (dbInstance) return Promise.resolve(dbInstance)

  return new Promise((resolve, reject) => {
    const req = indexedDB.open(DB_NAME, DB_VERSION)
    req.onupgradeneeded = () => {
      req.result.createObjectStore(STORE_NAME)
    }
    req.onsuccess = () => {
      dbInstance = req.result
      resolve(dbInstance)
    }
    req.onerror = () => reject(req.error)
  })
}

function dbGet(key) {
  return openDB().then(db => new Promise((resolve, reject) => {
    const tx = db.transaction(STORE_NAME, "readonly")
    const req = tx.objectStore(STORE_NAME).get(key)
    req.onsuccess = () => resolve(req.result)
    req.onerror = () => reject(req.error)
  }))
}

function dbPut(key, value) {
  return openDB().then(db => new Promise((resolve, reject) => {
    const tx = db.transaction(STORE_NAME, "readwrite")
    const req = tx.objectStore(STORE_NAME).put(value, key)
    req.onsuccess = () => resolve()
    req.onerror = () => reject(req.error)
  }))
}

function dbDelete(key) {
  return openDB().then(db => new Promise((resolve, reject) => {
    const tx = db.transaction(STORE_NAME, "readwrite")
    const req = tx.objectStore(STORE_NAME).delete(key)
    req.onsuccess = () => resolve()
    req.onerror = () => reject(req.error)
  }))
}

/** Delete all hub:* keys from IndexedDB, preserving __pickle_key__. */
function dbDeleteAllHubs() {
  return openDB().then(db => new Promise((resolve, reject) => {
    const tx = db.transaction(STORE_NAME, "readwrite")
    const store = tx.objectStore(STORE_NAME)
    const req = store.getAllKeys()
    req.onsuccess = () => {
      const hubKeys = req.result.filter(k => typeof k === "string" && k.startsWith("hub:"))
      for (const key of hubKeys) store.delete(key)
      tx.oncomplete = () => resolve(hubKeys.length)
      tx.onerror = () => reject(tx.error)
    }
    req.onerror = () => reject(req.error)
  }))
}

// =============================================================================
// Storage Encryption (AES-GCM derived from pickle key)
// =============================================================================

/**
 * Derive an AES-GCM CryptoKey from raw pickle key bytes at runtime.
 *
 * Safari has known bugs with storing CryptoKey objects in IndexedDB
 * (WebKit #177350, #183167) — structured clone of non-extractable keys
 * fails silently, breaking session restore. Instead, we store only raw
 * bytes (the pickle key) and import as a CryptoKey on demand.
 */
async function deriveStorageKey(pickleKeyBytes) {
  if (!crypto.subtle) return null
  return crypto.subtle.importKey(
    "raw",
    pickleKeyBytes,
    { name: "AES-GCM", length: 256 },
    false,
    ["encrypt", "decrypt"]
  )
}

/**
 * Encrypt a JS object for IndexedDB storage.
 * Returns `{ iv: Uint8Array, ciphertext: ArrayBuffer }` on success, or the
 * plain data object if crypto.subtle is unavailable or fails.
 */
async function encryptForStorage(data, pickleKeyBytes) {
  try {
    const key = await deriveStorageKey(pickleKeyBytes)
    if (!key) return data
    const iv = crypto.getRandomValues(new Uint8Array(12))
    const encoded = new TextEncoder().encode(JSON.stringify(data))
    const ciphertext = await crypto.subtle.encrypt({ name: "AES-GCM", iv }, key, encoded)
    return { iv, ciphertext }
  } catch (e) {
    console.warn("[VodozemacCrypto] encryptForStorage failed, storing plain:", e.message)
    return data
  }
}

/**
 * Decrypt a `{ iv, ciphertext }` envelope from IndexedDB back to a JS object.
 * Returns `null` if decryption fails (caller should try plain format).
 */
async function decryptFromStorage(encrypted, pickleKeyBytes) {
  try {
    const key = await deriveStorageKey(pickleKeyBytes)
    if (!key) return null
    const plaintext = await crypto.subtle.decrypt(
      { name: "AES-GCM", iv: new Uint8Array(encrypted.iv) },
      key,
      encrypted.ciphertext
    )
    return JSON.parse(new TextDecoder().decode(plaintext))
  } catch (e) {
    console.warn("[VodozemacCrypto] decryptFromStorage failed:", e.message)
    return null
  }
}

/**
 * Detect encrypted format ({ iv, ciphertext }) vs plain object.
 */
function isEncryptedFormat(value) {
  return value && value.iv && value.ciphertext
}

// =============================================================================
// Pickle Key & State Persistence
// =============================================================================

/**
 * Get or create a 32-byte pickle key.
 *
 * Stored as raw bytes (Array) in IndexedDB — NOT as a CryptoKey, because
 * Safari SharedWorkers can't structured-clone CryptoKey objects (WebKit #183167).
 * The pickle key doubles as the AES-GCM storage encryption key (imported at
 * runtime via crypto.subtle.importKey).
 */
let pickleKeyCache = null

async function getPickleKey() {
  if (pickleKeyCache) return pickleKeyCache

  const stored = await dbGet(PICKLE_KEY_ID)
  if (stored) {
    // Handle both Array and Uint8Array (legacy) formats
    pickleKeyCache = new Uint8Array(stored)
    return pickleKeyCache
  }

  pickleKeyCache = new Uint8Array(32)
  crypto.getRandomValues(pickleKeyCache)
  await dbPut(PICKLE_KEY_ID, Array.from(pickleKeyCache))
  return pickleKeyCache
}

/** Pickle account + session, encrypt, and write to IndexedDB. */
async function persistState(hubId) {
  try {
    const pickleKey = await getPickleKey()
    const account = accounts.get(hubId)
    if (!account) return

    const session = sessions.get(hubId)
    const bundle = bundles.get(hubId)

    // Strip raw byte fields before persisting — Uint8Arrays don't survive
    // JSON round-tripping, and are only needed at session creation time for
    // Ed25519 verification (fresh bundles always arrive with raw bytes).
    let persistBundle = null
    if (bundle) {
      const { signedData, signingKeyRaw, signatureRaw, ...rest } = bundle
      persistBundle = rest
    }

    const state = {
      accountPickle: account.pickle(pickleKey),
      sessionPickle: session ? session.pickle(pickleKey) : null,
      bundle: persistBundle,
    }

    const encrypted = await encryptForStorage(state, pickleKey)
    await dbPut(`hub:${hubId}`, encrypted)
  } catch (e) {
    console.warn("[VodozemacCrypto] Persist failed:", e)
  }
}

/** Restore account + session from encrypted IndexedDB into memory. */
async function restoreState(hubId) {
  if (accounts.has(hubId)) return true

  if (!wasmModule) return false

  const raw = await dbGet(`hub:${hubId}`)
  if (!raw) return false

  const pickleKey = await getPickleKey()

  let state
  if (isEncryptedFormat(raw)) {
    state = await decryptFromStorage(raw, pickleKey)
    // If decrypt failed (key mismatch, Safari bug, etc.), try as plain object
    if (!state && raw.accountPickle) state = raw
  } else if (raw.accountPickle) {
    // Legacy unencrypted format — use directly, will be re-encrypted on next persist
    state = raw
  } else {
    return false
  }

  if (!state || !state.accountPickle) return false

  try {
    const account = wasmModule.VodozemacAccount.fromPickle(state.accountPickle, pickleKey)
    accounts.set(hubId, account)

    if (state.sessionPickle) {
      const session = wasmModule.VodozemacSession.fromPickle(state.sessionPickle, pickleKey)
      sessions.set(hubId, session)
    }

    if (state.bundle) {
      bundles.set(hubId, state.bundle)
    }

    // Re-encrypt legacy unencrypted data on next persist
    console.log(`[VodozemacCrypto] Restored session for hub ${hubId.substring(0, 8)}...`)
    return true
  } catch (e) {
    console.warn("[VodozemacCrypto] Failed to restore state, clearing:", e)
    await dbDelete(`hub:${hubId}`)
    accounts.delete(hubId)
    sessions.delete(hubId)
    bundles.delete(hubId)
    return false
  }
}

// =============================================================================
// Crypto Action Handlers
// =============================================================================

/**
 * Initialize the vodozemac WASM module.
 * @param {string} wasmJsUrl - Full URL to vodozemac-wasm JS glue module
 * @param {string} wasmBinaryUrl - Full URL to vodozemac-wasm binary (.wasm)
 */
async function handleInit(wasmJsUrl, wasmBinaryUrl) {
  if (wasmModule) {
    return { alreadyInitialized: true }
  }

  if (!wasmJsUrl) {
    throw new Error("wasmJsUrl is required - SharedWorkers cannot resolve bare module specifiers")
  }

  console.log("[VodozemacCrypto] Loading WASM module from:", wasmJsUrl)
  wasmModule = await import(wasmJsUrl)

  // Pass the explicit .wasm binary URL so Propshaft-fingerprinted paths resolve correctly.
  // Without this, the JS glue uses import.meta.url-relative resolution which breaks
  // when the filename is fingerprinted (e.g., vodozemac_wasm-abc123.js).
  if (typeof wasmModule.default === "function") {
    await wasmModule.default(wasmBinaryUrl || undefined)
  }

  console.log("[VodozemacCrypto] WASM module initialized")
  return { initialized: true }
}

/**
 * Create a new Olm session for a hub from the CLI's bundle.
 *
 * 1. Create a VodozemacAccount (browser's identity)
 * 2. Parse CLI bundle for identity_key + one_time_key
 * 3. Create outbound session to CLI
 * 4. Persist to IndexedDB
 *
 * @param {string} hubId
 * @param {string|Object} bundleJson - CLI's PreKey bundle
 */
async function handleCreateSession(hubId, bundleJson) {
  if (!wasmModule) throw new Error("WASM not initialized")

  const bundle = typeof bundleJson === "string" ? JSON.parse(bundleJson) : bundleJson

  // If an existing session exists, verify identity key matches the original
  // QR trust anchor before replacing. Different identity key = possible MITM.
  const existingBundle = bundles.get(hubId)
  if (existingBundle && bundle.identityKey !== existingBundle.identityKey) {
    throw new Error(
      `Identity key mismatch in session refresh! ` +
      `Expected ${existingBundle.identityKey.substring(0, 16)}..., ` +
      `got ${bundle.identityKey.substring(0, 16)}... — rejecting (possible MITM)`
    )
  }

  // Verify Ed25519 signature over the bundle's signed payload (all raw bytes, no base64)
  if (bundle.signedData && bundle.signingKeyRaw && bundle.signatureRaw) {
    const valid = wasmModule.ed25519Verify(bundle.signingKeyRaw, bundle.signedData, bundle.signatureRaw)
    if (!valid) {
      throw new Error("Bundle Ed25519 signature verification failed — possible tampering")
    }
  } else {
    throw new Error("Bundle missing signedData, signingKeyRaw, or signatureRaw — cannot verify")
  }

  // Clear any existing state for this hub
  accounts.delete(hubId)
  sessions.delete(hubId)
  bundles.delete(hubId)

  // Create browser's Olm account
  const account = wasmModule.VodozemacAccount.create()

  // Extract CLI keys from bundle
  const cliIdentityKey = bundle.identityKey
  const cliOneTimeKey = bundle.oneTimeKey
  if (!cliIdentityKey || !cliOneTimeKey) {
    throw new Error("Bundle missing identityKey or oneTimeKey")
  }

  // Create outbound session to CLI
  const session = account.createOutboundSession(cliIdentityKey, cliOneTimeKey)

  // Store state in memory
  accounts.set(hubId, account)
  sessions.set(hubId, session)
  bundles.set(hubId, bundle)

  // Persist to IndexedDB
  await persistState(hubId)

  const identityKey = account.curve25519Key()

  console.log(`[VodozemacCrypto] Created session for hub ${hubId.substring(0, 8)}...`)
  return { created: true, identityKey }
}

/**
 * Check if we have an active session for a hub.
 * Attempts to restore from IndexedDB if not in memory.
 */
async function handleHasSession(hubId) {
  if (sessions.has(hubId)) return { hasSession: true }

  // Try restoring from IndexedDB
  const restored = await restoreState(hubId)
  return { hasSession: restored && sessions.has(hubId) }
}

/**
 * Encrypt a message using the Olm session (JSON envelope for ActionCable).
 * Persists ratchet state after encryption.
 *
 * @param {string} hubId
 * @param {string|Object} message - Content to encrypt
 * @returns {{ encrypted: Object }} - OlmEnvelope { t, b, k? }
 */
async function handleEncrypt(hubId, message) {
  // Try restore if not in memory
  if (!sessions.has(hubId)) await restoreState(hubId)

  const session = sessions.get(hubId)
  const account = accounts.get(hubId)
  if (!session) throw new Error(`No session for hub ${hubId}`)
  if (!account) throw new Error(`No account for hub ${hubId}`)

  // Encode message to UTF-8 bytes
  const messageStr = typeof message === "string" ? message : JSON.stringify(message)
  const plaintext = new TextEncoder().encode(messageStr)

  // Encrypt -> { messageType: number, ciphertext: Uint8Array }
  const { messageType, ciphertext } = session.encrypt(plaintext)

  // Build OlmEnvelope (JSON for ActionCable signaling)
  const envelope = { t: messageType, b: bytesToBase64(ciphertext) }

  // Include sender key on PreKey messages so recipient can create inbound session
  if (messageType === 0) {
    envelope.k = account.curve25519Key()
  }

  // Persist ratchet advancement before returning so page eviction
  // (phone lock/unlock) cannot leave stale ratchet state in IndexedDB.
  await persistState(hubId)

  return { encrypted: envelope }
}

/**
 * Decrypt a JSON OlmEnvelope (ActionCable signaling).
 * Persists ratchet state after decryption.
 *
 * @param {string} hubId
 * @param {string|Object} encryptedData - OlmEnvelope { t, b, k? }
 * @returns {{ plaintext: any }}
 */
async function handleDecrypt(hubId, encryptedData) {
  // Try restore if not in memory
  if (!accounts.has(hubId)) await restoreState(hubId)

  const envelope = typeof encryptedData === "string" ? JSON.parse(encryptedData) : encryptedData
  const ciphertext = base64ToBytes(envelope.b)

  let plaintextBytes

  if (envelope.t === 0) {
    // PreKey message — create inbound session
    const account = accounts.get(hubId)
    if (!account) throw new Error(`No account for hub ${hubId}`)

    const senderKey = envelope.k
    if (!senderKey) throw new Error("PreKey message missing sender key (k)")

    const { session, plaintext } = account.createInboundSession(senderKey, ciphertext)

    // Replace session (CLI re-established)
    sessions.set(hubId, session)
    plaintextBytes = plaintext
  } else {
    // Normal message
    const session = sessions.get(hubId)
    if (!session) throw new Error(`No session for hub ${hubId}`)

    plaintextBytes = session.decrypt(envelope.t, ciphertext)
  }

  // Persist ratchet advancement before returning so page eviction
  // cannot leave stale ratchet state in IndexedDB.
  await persistState(hubId)

  // Decode UTF-8 and parse JSON
  const plaintextStr = new TextDecoder().decode(plaintextBytes)
  try {
    return { plaintext: JSON.parse(plaintextStr) }
  } catch {
    return { plaintext: plaintextStr }
  }
}

// =========================================================================
// Binary DataChannel API (zero base64, zero JSON envelope)
// =========================================================================

/**
 * Encrypt plaintext bytes into a binary DataChannel frame.
 *
 * Output: [msg_type:1][raw Olm ciphertext] (Normal)
 *     or: [msg_type:1][32-byte sender key][raw Olm ciphertext] (PreKey)
 *
 * @param {string} hubId
 * @param {Uint8Array} plaintext - Raw bytes to encrypt
 * @returns {{ data: Uint8Array }}
 */
async function handleEncryptBinary(hubId, plaintext) {
  if (!sessions.has(hubId)) await restoreState(hubId)

  const session = sessions.get(hubId)
  const account = accounts.get(hubId)
  if (!session) throw new Error(`No session for hub ${hubId}`)
  if (!account) throw new Error(`No account for hub ${hubId}`)

  const bytes = plaintext instanceof Uint8Array ? plaintext : new Uint8Array(plaintext)
  const { messageType, ciphertext } = session.encrypt(bytes)

  let frame
  if (messageType === 0) {
    // PreKey: [0x00][32-byte sender key][ciphertext]
    const keyB64 = account.curve25519Key()
    const keyBytes = base64ToBytes(keyB64)
    frame = new Uint8Array(1 + 32 + ciphertext.length)
    frame[0] = 0
    frame.set(keyBytes, 1)
    frame.set(ciphertext, 33)
  } else {
    // Normal: [0x01][ciphertext]
    frame = new Uint8Array(1 + ciphertext.length)
    frame[0] = 1
    frame.set(ciphertext, 1)
  }

  await persistState(hubId)
  return { data: frame }
}

/**
 * Decrypt a binary DataChannel frame, returning plaintext bytes.
 *
 * Input: [msg_type:1][raw ciphertext] (Normal)
 *    or: [msg_type:1][32-byte sender key][raw ciphertext] (PreKey)
 *
 * @param {string} hubId
 * @param {Uint8Array} data - Binary frame
 * @returns {{ data: Uint8Array }}
 */
async function handleDecryptBinary(hubId, data) {
  if (!accounts.has(hubId)) await restoreState(hubId)

  const bytes = data instanceof Uint8Array ? data : new Uint8Array(data)
  if (bytes.length === 0) throw new Error("Empty binary frame")

  const msgType = bytes[0]
  let plaintextBytes

  if (msgType === 0) {
    // PreKey: [0x00][32-byte sender key][ciphertext]
    if (bytes.length <= 33) throw new Error("PreKey frame too short")
    const senderKeyBytes = bytes.slice(1, 33)
    const senderKey = bytesToBase64(senderKeyBytes)
    const ciphertext = bytes.slice(33)

    // Try existing session first
    const session = sessions.get(hubId)
    if (session) {
      try {
        plaintextBytes = session.decrypt(0, ciphertext)
        await persistState(hubId)
        return { data: new Uint8Array(plaintextBytes) }
      } catch {
        // Session couldn't decrypt — new pairing, create inbound
      }
    }

    const account = accounts.get(hubId)
    if (!account) throw new Error(`No account for hub ${hubId}`)
    const result = account.createInboundSession(senderKey, ciphertext)
    sessions.set(hubId, result.session)
    plaintextBytes = result.plaintext
  } else {
    // Normal: [0x01][ciphertext]
    const ciphertext = bytes.slice(1)
    const session = sessions.get(hubId)
    if (!session) throw new Error(`No session for hub ${hubId}`)
    plaintextBytes = session.decrypt(1, ciphertext)
  }

  await persistState(hubId)
  return { data: new Uint8Array(plaintextBytes) }
}

/**
 * Get the browser's identity key (Curve25519) for a hub.
 * Attempts to restore from IndexedDB if not in memory.
 */
async function handleGetIdentityKey(hubId) {
  if (!accounts.has(hubId)) await restoreState(hubId)

  const account = accounts.get(hubId)
  if (!account) throw new Error(`No account for hub ${hubId}`)
  return { identityKey: account.curve25519Key() }
}

/**
 * Clear session state for a hub (memory + IndexedDB).
 */
async function handleClearSession(hubId) {
  accounts.delete(hubId)
  sessions.delete(hubId)
  bundles.delete(hubId)

  await dbDelete(`hub:${hubId}`)

  console.log(`[VodozemacCrypto] Cleared session for hub ${hubId.substring(0, 8)}...`)
  return { cleared: true }
}

/**
 * Clear ALL session state (memory + IndexedDB).
 * Used by test teardown to prevent session leakage between tests.
 *
 * Nuclear cleanup: clears in-memory Maps, ALL hub:* IndexedDB entries,
 * pickle key cache, and closes the IDB connection to prevent stale handles.
 */
async function handleClearAllSessions() {
  accounts.clear()
  sessions.clear()
  bundles.clear()
  pickleKeyCache = null

  // Clear all hub:* entries from IndexedDB (preserving __pickle_key__)
  let deleted = 0
  try {
    deleted = await dbDeleteAllHubs()
  } catch {
    // IDB may have been deleted externally — that's fine
  }

  // Close IDB connection so it can be cleanly deleted/reopened
  if (dbInstance) {
    dbInstance.close()
    dbInstance = null
  }

  console.log(`[VodozemacCrypto] Cleared all sessions (${deleted} IDB entries)`)
  return { cleared: true, count: deleted }
}

// =============================================================================
// Message Handler
// =============================================================================

async function handleMessage(event, portId, replyFn) {
  const { id, action, ...params } = event.data

  // Handle pong (heartbeat response)
  if (action === "pong") {
    const portState = ports.get(portId)
    if (portState) {
      portState.lastPong = Date.now()
    }
    return
  }

  try {
    let result

    switch (action) {
      case "init":
        result = await handleInit(params.wasmJsUrl, params.wasmBinaryUrl)
        break
      case "createSession":
        result = await handleCreateSession(params.hubId, params.bundleJson)
        break
      case "hasSession":
        result = await handleHasSession(params.hubId)
        break
      case "encrypt":
        result = await handleEncrypt(params.hubId, params.message)
        break
      case "decrypt":
        result = await handleDecrypt(params.hubId, params.encryptedData)
        break
      case "encryptBinary":
        result = await handleEncryptBinary(params.hubId, params.plaintext)
        break
      case "decryptBinary":
        result = await handleDecryptBinary(params.hubId, params.data)
        break
      case "getIdentityKey":
        result = await handleGetIdentityKey(params.hubId)
        break
      case "clearSession":
        result = await handleClearSession(params.hubId)
        break
      case "clearAllSessions":
        result = await handleClearAllSessions()
        break
      default:
        throw new Error(`Unknown action: ${action}`)
    }

    replyFn({ id, success: true, result })
  } catch (error) {
    console.error("[VodozemacCrypto] Error:", action, error)
    replyFn({ id, success: false, error: error.message })
  }
}

// =============================================================================
// Port Management
// =============================================================================

const ports = new Map()
let portIdCounter = 0

function generatePortId() {
  return `port_${++portIdCounter}_${Date.now()}`
}

function cleanupPort(portId) {
  ports.delete(portId)
  console.log(`[VodozemacCrypto] Cleaned up port ${portId}, ${ports.size} ports remaining`)
}

// =============================================================================
// SharedWorker Entry Point
// =============================================================================

self.onconnect = (event) => {
  const port = event.ports[0]
  const portId = generatePortId()

  ports.set(portId, { port, lastPong: Date.now() })

  port.onmessage = (msgEvent) => {
    handleMessage(msgEvent, portId, (msg) => port.postMessage(msg))
  }

  port.onmessageerror = () => {
    cleanupPort(portId)
  }

  port.start()
}

// =============================================================================
// Regular Worker Fallback
// =============================================================================

self.onmessage = (event) => {
  handleMessage(event, null, (msg) => self.postMessage(msg))
}

// =============================================================================
// Heartbeat: ping all ports every 5s, cleanup dead ones after 21s
// =============================================================================

const HEARTBEAT_INTERVAL = 5000
const PORT_TTL = 21000

setInterval(() => {
  const now = Date.now()

  for (const [portId, state] of ports) {
    if (now - state.lastPong > PORT_TTL) {
      console.log(`[VodozemacCrypto] Port ${portId} timed out, cleaning up`)
      cleanupPort(portId)
      continue
    }

    try {
      state.port.postMessage({ event: "ping" })
    } catch (e) {
      console.log(`[VodozemacCrypto] Port ${portId} unreachable, cleaning up`)
      cleanupPort(portId)
    }
  }
}, HEARTBEAT_INTERVAL)
