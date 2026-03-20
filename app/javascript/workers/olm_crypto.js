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
 * State is intentionally memory-only. When the SharedWorker exits,
 * the ratchet exits with it.
 *
 * Wire format (OlmEnvelope):
 *   PreKey:  { t: 0, b: "<base64 ciphertext>", k: "<sender curve25519 key>" }
 *   Normal:  { t: 1, b: "<base64 ciphertext>" }
 */

// WASM module state
let wasmModule = null

// Per-hub crypto state (memory only)
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

  const identityKey = account.curve25519Key()

  console.log(`[VodozemacCrypto] Created session for hub ${hubId.substring(0, 8)}...`)
  return { created: true, identityKey }
}

/**
 * Start a fresh outbound ratchet from the latest in-memory bundle.
 *
 * Keeps the browser identity stable while ensuring each new offer starts from
 * a fresh outbound session.
 */
async function handleResetSession(hubId) {
  if (!wasmModule) throw new Error("WASM not initialized")

  const account = accounts.get(hubId)
  const bundle = bundles.get(hubId)
  if (!account) throw new Error(`No account for hub ${hubId}`)
  if (!bundle?.identityKey || !bundle?.oneTimeKey) {
    throw new Error(`No bundle for hub ${hubId}`)
  }

  const session = account.createOutboundSession(bundle.identityKey, bundle.oneTimeKey)
  sessions.set(hubId, session)

  return { reset: true }
}

/**
 * Check if we have an active session for a hub.
 */
async function handleHasSession(hubId) {
  return { hasSession: sessions.has(hubId) }
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

  return { data: new Uint8Array(plaintextBytes) }
}

/**
 * Get the browser's identity key (Curve25519) for a hub.
 */
async function handleGetIdentityKey(hubId) {
  const account = accounts.get(hubId)
  if (!account) throw new Error(`No account for hub ${hubId}`)
  return { identityKey: account.curve25519Key() }
}

/**
 * Clear session state for a hub.
 */
async function handleClearSession(hubId) {
  accounts.delete(hubId)
  sessions.delete(hubId)
  bundles.delete(hubId)

  console.log(`[VodozemacCrypto] Cleared session for hub ${hubId.substring(0, 8)}...`)
  return { cleared: true }
}

/**
 * Clear ALL session state.
 * Used by test teardown to prevent session leakage between tests.
 */
async function handleClearAllSessions() {
  accounts.clear()
  sessions.clear()
  bundles.clear()

  console.log("[VodozemacCrypto] Cleared all sessions")
  return { cleared: true, count: 0 }
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
      case "resetSession":
        result = await handleResetSession(params.hubId)
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
  port.postMessage({ event: "ready", portId })

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
  if (typeof self.postMessage === "function") {
    self.postMessage({ event: "ready", portId: "worker" })
  }
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
