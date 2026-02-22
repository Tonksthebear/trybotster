/**
 * WebRTCTransport - Main thread WebRTC connection manager
 *
 * Singleton that manages WebRTC peer connections in the main thread.
 * RTCPeerConnection is not available in Workers, so this must run in main thread.
 *
 * Persists across Turbo navigation:
 * - Connections survive Turbo link clicks (no cleanup on turbo:before-visit)
 * - When controllers release connections, a 3s grace period starts
 * - If reacquired before grace period expires, connection is reused instantly
 * - Only closes on actual page unload (beforeunload)
 *
 * Architecture:
 * - Main thread: WebRTCTransport (this) handles RTCPeerConnection, DataChannel
 * - SharedWorker: olm_crypto.js handles encryption/decryption (vodozemac)
 * - Signaling: ActionCable push via HubSignalingChannel (encrypted OlmEnvelopes)
 *   Rails is a dumb pipe — envelopes are opaque, only browser/CLI can decrypt.
 *
 * Wire format (OlmEnvelope):
 *   PreKey:  { t: 0, b: "<base64 ciphertext>", k: "<sender curve25519 key>" }
 *   Normal:  { t: 1, b: "<base64 ciphertext>" }
 */

import bridge from "workers/bridge"
import { parseBinaryBundle } from "matrix/bundle"

// Singleton instance
let instance = null

// ActionCable consumer (lazy singleton, shared across all connections)
let cableConsumer = null

// Connection states
export const TransportState = {
  DISCONNECTED: "disconnected",
  CONNECTING: "connecting",
  CONNECTED: "connected",
  ERROR: "error",
}

// Connection mode (P2P vs relayed)
const ConnectionMode = {
  UNKNOWN: "unknown",     // Not yet determined
  DIRECT: "direct",       // P2P (host, srflx, prflx candidates)
  RELAYED: "relayed",     // Through TURN server
}

// Binary inner content types (must match CLI's CONTENT_MSG / CONTENT_PTY / CONTENT_STREAM / CONTENT_FILE)
const CONTENT_MSG = 0x00
const CONTENT_PTY = 0x01
const CONTENT_STREAM = 0x02
const CONTENT_FILE = 0x03

// Olm session restart wire type (unencrypted control, CLI → Browser)
const MSG_TYPE_BUNDLE_REFRESH = 0x02

// Grace period before closing idle connections (ms)
const DISCONNECT_GRACE_PERIOD_MS = 3000

/**
 * Decode unpadded Base64 to Uint8Array.
 */
function base64ToBytes(b64) {
  const binary = atob(b64)
  return Uint8Array.from(binary, c => c.charCodeAt(0))
}

/**
 * Build a binary control message frame: [CONTENT_MSG][JSON bytes].
 * @param {Object} payload - JSON-serializable message
 * @returns {Uint8Array}
 */
function buildControlFrame(payload) {
  const jsonBytes = new TextEncoder().encode(JSON.stringify(payload))
  const frame = new Uint8Array(1 + jsonBytes.length)
  frame[0] = CONTENT_MSG
  frame.set(jsonBytes, 1)
  return frame
}

// ICE restart configuration
const ICE_RESTART_DELAY_MS = 1000        // Wait before first restart attempt
const ICE_RESTART_MAX_ATTEMPTS = 3       // Max restarts before full reconnect
const ICE_RESTART_BACKOFF_MULTIPLIER = 2 // Exponential backoff

class WebRTCTransport {
  #connections = new Map() // hubId -> { pc, dataChannel, state, subscriptions }
  #connectPromises = new Map() // hubId -> Promise (pending connect())
  #peerConnectPromises = new Map() // hubId -> Promise (pending connectPeer())
  #eventListeners = new Map() // eventName -> Set<callback>
  #subscriptionListeners = new Map() // subscriptionId -> Set<callback>
  #pendingSubscriptions = new Map() // subscriptionId -> { resolve, reject, timeout }
  #subscriptionIdCounter = 0
  #cableSubscriptions = new Map() // hubId -> ActionCable subscription
  #graceTimers = new Map() // hubId -> timer (pending disconnects)

  constructor() {
    // Clean up connections on actual page unload only.
    // Turbo navigation preserves connections - they're cleaned up via grace periods
    // when controllers release them.
    window.addEventListener("beforeunload", () => {
      // Cancel all grace timers and close immediately
      for (const timer of this.#graceTimers.values()) {
        clearTimeout(timer)
      }
      this.#graceTimers.clear()

      // Clean up ActionCable signaling subscriptions
      for (const sub of this.#cableSubscriptions.values()) {
        sub.unsubscribe()
      }
      this.#cableSubscriptions.clear()

      for (const [hubId, conn] of this.#connections) {
        // Skip dc.close() — pc.close() handles it, and explicit DC close
        // triggers an SCTP stream reset that crashes webrtc-rs/sctp.
        if (conn.pc) conn.pc.close()
      }
      this.#connections.clear()
      this.#connectPromises.clear()
    })
  }

  static get instance() {
    if (!instance) {
      instance = new WebRTCTransport()
    }
    return instance
  }

  /**
   * Connect to a hub via WebRTC (signaling + peer connection).
   * Multiple callers share the same connection - subsequent calls wait for
   * the pending connection or return the existing one.
   */
  async connect(hubId, browserIdentity) {
    this.#cancelGracePeriod(hubId)

    let conn = this.#connections.get(hubId)
    if (conn?.pc) return { state: conn.state }

    // If signaling exists but no peer, just add peer
    if (conn && !conn.pc) return this.connectPeer(hubId)

    const pendingPromise = this.#connectPromises.get(hubId)
    if (pendingPromise) return pendingPromise

    const connectPromise = (async () => {
      await this.connectSignaling(hubId, browserIdentity)
      return this.connectPeer(hubId)
    })()
    this.#connectPromises.set(hubId, connectPromise)

    try {
      return await connectPromise
    } finally {
      this.#connectPromises.delete(hubId)
    }
  }

  /**
   * Connect ActionCable signaling channel only (no WebRTC peer connection).
   * Health messages flow immediately. Call connectPeer() later to start WebRTC.
   */
  async connectSignaling(hubId, browserIdentity) {
    this.#cancelGracePeriod(hubId)

    let conn = this.#connections.get(hubId)
    if (conn) {
      // Re-emit cached health so new Connections (Turbo navigation) get the
      // current CLI status. Without this, cliStatus stays UNKNOWN because no
      // new ActionCable subscription is created (no server-side transmit).
      if (conn.lastHealth) {
        queueMicrotask(() => this.#emit("health", { hubId, ...conn.lastHealth }))
      }
      return { state: conn.state }
    }

    const newConn = {
      pc: null,
      dataChannel: null,
      state: TransportState.DISCONNECTED,
      mode: ConnectionMode.UNKNOWN,
      hubId,
      browserIdentity,
      subscriptions: new Map(),
      pendingCandidates: [],
      iceRestartAttempts: 0,
      iceRestartTimer: null,
      iceDisrupted: false,
      decryptFailures: 0,
      // Start true — ActionCable buffers messages until confirmed.
      // Set false by disconnected callback, true again by connected.
      signalingConnected: true,
    }
    this.#connections.set(hubId, newConn)

    await this.#createSignalingChannel(hubId, browserIdentity)

    return { state: TransportState.DISCONNECTED }
  }

  /**
   * Create WebRTC peer connection on an existing signaling channel.
   * Fetches ICE config, creates RTCPeerConnection + DataChannel, sends offer.
   * Deduplicates concurrent callers (e.g., multiple Connection instances
   * reacting to the same health "online" event).
   */
  async connectPeer(hubId) {
    const conn = this.#connections.get(hubId)
    if (!conn) throw new Error(`No signaling connection for hub ${hubId}`)

    if (conn.pc) {
      const pcState = conn.pc.connectionState
      const dcState = conn.dataChannel?.readyState
      // Dead peer — tear down so we can create a fresh one.
      // Terminal PC states (closed/failed/disconnected) are obviously dead.
      // Also dead: PC is "connected" but DC is closed/gone (iOS sleep: PC
      // reports connected but DC silently died). DC "connecting" is OK — brief
      // window between DTLS connect and SCTP open.
      // NOT dead: PC is "new" or "connecting" — peer is actively establishing,
      // DC will naturally be "connecting" until ICE completes. Tearing down a
      // connecting peer sends a new offer that the CLI rejects ("Connection in
      // progress"), causing a reconnect storm.
      const dcAlive = dcState === "open" || dcState === "connecting"
      const dead = pcState === "closed" || pcState === "failed" || pcState === "disconnected" ||
                   (pcState === "connected" && !dcAlive)
      if (dead) {
        this.#teardownPeer(conn)
        this.#emit("connection:state", { hubId, state: "disconnected" })
      } else {
        return { state: conn.state }
      }
    }

    // Deduplicate: if another caller is already creating the peer, wait for it
    const pending = this.#peerConnectPromises.get(hubId)
    if (pending) return pending

    const promise = this.#doConnectPeer(hubId, conn)
    this.#peerConnectPromises.set(hubId, promise)

    try {
      return await promise
    } finally {
      this.#peerConnectPromises.delete(hubId)
    }
  }

  async #doConnectPeer(hubId, conn) {
    const subscription = this.#cableSubscriptions.get(hubId)
    if (!subscription) throw new Error(`No signaling subscription for hub ${hubId}`)
    if (!conn.signalingConnected) throw new Error(`Signaling not connected for hub ${hubId}`)

    console.debug(`[WebRTCTransport] Creating peer connection for hub ${hubId}`)

    const iceConfig = await this.#fetchIceConfig(hubId)
    const pc = new RTCPeerConnection({ iceServers: iceConfig.ice_servers })
    conn.pc = pc
    conn.state = TransportState.CONNECTING

    // ICE candidate handler
    pc.onicecandidate = async (event) => {
      if (event.candidate) {
        try {
          const envelope = await this.#encryptSignal(hubId, {
            type: "ice",
            candidate: event.candidate.toJSON(),
          })
          subscription.perform("signal", { envelope })
        } catch (e) {
          console.error("[WebRTCTransport] Failed to send ICE candidate:", e)
        }
      }
    }

    // ICE connection state handler - triggers ICE restart on network changes
    pc.oniceconnectionstatechange = () => {
      const state = pc.iceConnectionState
      console.debug(`[WebRTCTransport] ICE connection state: ${state}`)

      if (state === "connected" || state === "completed") {
        conn.iceRestartAttempts = 0
        if (conn.iceRestartTimer) {
          clearTimeout(conn.iceRestartTimer)
          conn.iceRestartTimer = null
        }
        if (conn.iceDisrupted) {
          conn.iceDisrupted = false
          this.#detectConnectionMode(hubId, conn).then(mode => {
            this.#emit("connection:state", { hubId, state: "connected", mode })
            this.#emit("connection:mode", { hubId, mode })
          })
        }
      } else if (state === "disconnected" || state === "failed") {
        conn.mode = ConnectionMode.UNKNOWN
        conn.iceDisrupted = true
        this.#emit("connection:mode", { hubId, mode: ConnectionMode.UNKNOWN })
        this.#scheduleIceRestart(hubId, conn)
      }
    }

    // Overall connection state handler - for terminal states
    pc.onconnectionstatechange = () => {
      const state = pc.connectionState
      console.debug(`[WebRTCTransport] Connection state: ${state}`)

      if (state === "connected") {
        conn.state = TransportState.CONNECTED
        this.#detectConnectionMode(hubId, conn).then(mode => {
          this.#emit("connection:state", { hubId, state: "connected", mode })
          this.#emit("connection:mode", { hubId, mode })
        }).catch(() => {
          this.#emit("connection:state", { hubId, state: "connected", mode: "unknown" })
        })
      } else if (state === "closed") {
        // Only clean up peer on explicit close — don't remove signaling
        this.#cleanupPeer(hubId, conn)
      } else if (state === "failed") {
        if (conn.iceRestartAttempts >= ICE_RESTART_MAX_ATTEMPTS) {
          console.debug(`[WebRTCTransport] Connection failed after ${conn.iceRestartAttempts} ICE restarts, cleaning up peer`)
          this.#cleanupPeer(hubId, conn)
        }
      }
    }

    // Create data channel
    const dataChannel = pc.createDataChannel("relay", { ordered: true })
    conn.dataChannel = dataChannel
    this.#setupDataChannel(hubId, dataChannel)

    // Create offer, encrypt, and send via ActionCable
    const offer = await pc.createOffer()
    await pc.setLocalDescription(offer)

    const envelope = await this.#encryptSignal(hubId, {
      type: "offer",
      sdp: offer.sdp,
    })
    subscription.perform("signal", { envelope })

    return { state: TransportState.CONNECTING }
  }

  /**
   * Probe WebRTC peer health for a specific hub.
   * If the peer is dead (PC failed/closed/disconnected, or DC not open),
   * cleans it up and emits connection:state disconnected.
   * @returns {{ alive: boolean, pcState: string, dcState: string }}
   */
  probePeerHealth(hubId) {
    const conn = this.#connections.get(hubId)
    if (!conn?.pc) return { alive: false, pcState: "none", dcState: "none" }

    const pcState = conn.pc.connectionState
    const dcState = conn.dataChannel?.readyState || "none"

    const dead = pcState === "failed" || pcState === "closed" || pcState === "disconnected" ||
                 dcState !== "open"

    if (dead) {
      console.debug(`[WebRTCTransport] Probe: peer dead for hub ${hubId} (pc=${pcState}, dc=${dcState}), cleaning up`)
      conn.iceRestartAttempts = 0
      this.#cleanupPeer(hubId, conn)
    }

    return { alive: !dead, pcState, dcState }
  }

  /**
   * Close WebRTC peer connection but keep ActionCable signaling alive.
   * Used when hub goes offline — signaling stays up for health events.
   */
  disconnectPeer(hubId) {
    const conn = this.#connections.get(hubId)
    if (!conn?.pc) return

    console.debug(`[WebRTCTransport] Disconnecting peer for hub ${hubId} (keeping signaling)`)
    this.#teardownPeer(conn)
    this.#emit("connection:state", { hubId, state: "disconnected" })
  }

  /**
   * Disconnect from a hub with grace period.
   * Connection stays alive for DISCONNECT_GRACE_PERIOD_MS to allow reacquisition
   * during Turbo navigation. Call connect() to cancel the grace period.
   */
  async disconnect(hubId) {
    const conn = this.#connections.get(hubId)
    if (!conn) return

    // If grace timer already pending, don't restart it
    if (this.#graceTimers.has(hubId)) return

    console.debug(`[WebRTCTransport] Starting ${DISCONNECT_GRACE_PERIOD_MS}ms grace period for hub ${hubId}`)

    const timer = setTimeout(() => {
      this.#graceTimers.delete(hubId)
      this.#closeConnection(hubId)
    }, DISCONNECT_GRACE_PERIOD_MS)

    this.#graceTimers.set(hubId, timer)
  }

  /**
   * Cancel a pending grace period disconnect.
   * Called when a connection is reacquired before the grace period expires.
   */
  #cancelGracePeriod(hubId) {
    const timer = this.#graceTimers.get(hubId)
    if (timer) {
      console.debug(`[WebRTCTransport] Cancelled grace period for hub ${hubId} (reacquired)`)
      clearTimeout(timer)
      this.#graceTimers.delete(hubId)
    }
  }

  /**
   * Actually close a connection (called after grace period or on page unload).
   */
  #closeConnection(hubId) {
    const conn = this.#connections.get(hubId)
    if (!conn) return

    console.debug(`[WebRTCTransport] Closing connection for hub ${hubId}`)

    // Tear down peer connection (if any)
    this.#teardownPeer(conn)

    // Unsubscribe ActionCable signaling channel
    const cableSub = this.#cableSubscriptions.get(hubId)
    if (cableSub) {
      cableSub.unsubscribe()
      this.#cableSubscriptions.delete(hubId)
    }

    this.#connections.delete(hubId)
    this.#emit("connection:state", { hubId, state: "disconnected" })
  }

  /**
   * Subscribe to a channel (maps to DataChannel usage)
   * @param {string} hubId - Hub identifier
   * @param {string} channelName - Channel name (e.g., "terminal", "hub", "preview")
   * @param {Object} params - Subscription params
   * @param {string} [providedSubscriptionId] - Optional semantic subscription ID
   * @param {Uint8Array} encryptedBinary - Binary Olm frame for subscribe message
   */
  async subscribe(hubId, channelName, params, providedSubscriptionId = null, encryptedBinary = null) {
    const conn = this.#connections.get(hubId)
    if (!conn) {
      throw new Error(`No connection for hub ${hubId}`)
    }

    // Use provided semantic ID or fall back to generated unique ID
    const subscriptionId = providedSubscriptionId || `sub_${++this.#subscriptionIdCounter}_${Date.now()}`

    conn.subscriptions.set(subscriptionId, {
      channelName,
      params,
    })

    // Wait for data channel to be open
    if (conn.dataChannel?.readyState !== "open") {
      await this.#waitForDataChannel(conn.dataChannel)
    }

    // Send binary Olm frame directly (zero JSON, zero base64)
    if (encryptedBinary) {
      conn.dataChannel.send(encryptedBinary.buffer)
    } else {
      console.error("[WebRTCTransport] subscribe called without encrypted payload — CLI will reject")
      throw new Error("Cannot subscribe without encrypted payload")
    }

    // Wait for CLI to confirm subscription before allowing input
    await this.#waitForSubscriptionConfirmed(subscriptionId)

    this.#emit("subscription:confirmed", { subscriptionId })

    return { subscriptionId }
  }

  /**
   * Unsubscribe from a channel
   */
  async unsubscribe(subscriptionId) {
    for (const [hubId, conn] of this.#connections) {
      if (conn.subscriptions.has(subscriptionId)) {
        if (conn.dataChannel?.readyState === "open") {
          try {
            const plaintext = buildControlFrame({ type: "unsubscribe", subscriptionId })
            const { data: encrypted } = await bridge.encryptBinary(String(hubId), plaintext)
            conn.dataChannel.send(encrypted.buffer)
          } catch (e) {
            console.warn("[WebRTCTransport] Failed to encrypt unsubscribe:", e)
          }
        }
        conn.subscriptions.delete(subscriptionId)
        return { unsubscribed: true }
      }
    }
    return { unsubscribed: false }
  }

  /**
   * Send data through the data channel (Olm-encrypted).
   * Wraps in m.botster.msg with subscriptionId for CLI routing.
   */
  async sendRaw(subscriptionId, message) {
    for (const [hubId, conn] of this.#connections) {
      if (conn.subscriptions.has(subscriptionId)) {
        if (conn.dataChannel?.readyState !== "open") {
          throw new Error("DataChannel not open")
        }

        const plaintext = buildControlFrame({ subscriptionId, data: message })
        const { data: encrypted } = await bridge.encryptBinary(String(hubId), plaintext)
        conn.dataChannel.send(encrypted.buffer)
        return { sent: true }
      }
    }
    throw new Error(`Subscription ${subscriptionId} not found`)
  }

  /**
   * Send pre-encrypted binary frame through the DataChannel.
   * @param {string} hubId - Hub identifier
   * @param {Uint8Array} encrypted - Binary Olm frame
   */
  async sendEncrypted(hubId, encrypted) {
    const conn = this.#connections.get(hubId)
    if (!conn) {
      throw new Error(`No connection for hub ${hubId}`)
    }
    if (conn.dataChannel?.readyState !== "open") {
      throw new Error("DataChannel not open")
    }

    conn.dataChannel.send(encrypted instanceof Uint8Array ? encrypted.buffer : encrypted)
    return { sent: true }
  }

  /**
   * Send a stream multiplexer frame through the encrypted DataChannel.
   * @param {string} hubId - Hub identifier
   * @param {number} frameType - Frame type (OPEN, DATA, CLOSE)
   * @param {number} streamId - Stream identifier
   * @param {Uint8Array} payload - Frame payload
   */
  async sendStreamFrame(hubId, frameType, streamId, payload) {
    const conn = this.#connections.get(hubId)
    if (!conn) throw new Error(`No connection for hub ${hubId}`)
    if (conn.dataChannel?.readyState !== "open") throw new Error("DataChannel not open")

    // Build plaintext: [CONTENT_STREAM][frameType][streamId_hi][streamId_lo][payload]
    const plaintext = new Uint8Array(4 + (payload?.length || 0))
    plaintext[0] = CONTENT_STREAM
    plaintext[1] = frameType
    plaintext[2] = (streamId >> 8) & 0xFF
    plaintext[3] = streamId & 0xFF
    if (payload?.length) plaintext.set(payload, 4)

    const { data: encrypted } = await bridge.encryptBinary(String(hubId), plaintext)
    conn.dataChannel.send(encrypted instanceof Uint8Array ? encrypted.buffer : encrypted)
  }

  /**
   * Send binary PTY input through the encrypted DataChannel.
   * Bypasses JSON serialization for the keystroke hot path.
   * @param {string} hubId - Hub identifier
   * @param {string} subscriptionId - Terminal subscription ID (e.g., "terminal_0_0")
   * @param {string|Uint8Array} data - Raw input data
   */
  async sendPtyInput(hubId, subscriptionId, data) {
    const conn = this.#connections.get(hubId)
    if (!conn) throw new Error(`No connection for hub ${hubId}`)
    if (conn.dataChannel?.readyState !== "open") throw new Error("DataChannel not open")

    // Build plaintext: [CONTENT_PTY][flags=0x02 input][sub_id_len][sub_id][payload]
    const subIdBytes = new TextEncoder().encode(subscriptionId)
    const dataBytes = typeof data === "string" ? new TextEncoder().encode(data) : data
    const plaintext = new Uint8Array(3 + subIdBytes.length + dataBytes.length)
    plaintext[0] = CONTENT_PTY      // 0x01
    plaintext[1] = 0x02             // flags: input direction
    plaintext[2] = subIdBytes.length
    plaintext.set(subIdBytes, 3)
    plaintext.set(dataBytes, 3 + subIdBytes.length)

    const { data: encrypted } = await bridge.encryptBinary(String(hubId), plaintext)
    conn.dataChannel.send(encrypted instanceof Uint8Array ? encrypted.buffer : encrypted)
  }

  /**
   * Send a file (image paste/drop) through the encrypted DataChannel.
   * Frame: [CONTENT_FILE][sub_id_len][sub_id][filename_len_lo][filename_len_hi][filename][data]
   * @param {string} hubId - Hub identifier
   * @param {string} subscriptionId - Terminal subscription ID (e.g., "terminal_0_0")
   * @param {Uint8Array} data - Raw file bytes
   * @param {string} filename - Original filename (e.g., "screenshot.png")
   */
  async sendFileInput(hubId, subscriptionId, data, filename) {
    const conn = this.#connections.get(hubId)
    if (!conn) throw new Error(`No connection for hub ${hubId}`)
    if (conn.dataChannel?.readyState !== "open") throw new Error("DataChannel not open")

    const subIdBytes = new TextEncoder().encode(subscriptionId)
    const filenameBytes = new TextEncoder().encode(filename)
    // Frame: [0x03][sub_id_len:1][sub_id][filename_len:2 LE][filename][data]
    const plaintext = new Uint8Array(1 + 1 + subIdBytes.length + 2 + filenameBytes.length + data.length)
    let offset = 0
    plaintext[offset++] = CONTENT_FILE
    plaintext[offset++] = subIdBytes.length
    plaintext.set(subIdBytes, offset); offset += subIdBytes.length
    plaintext[offset++] = filenameBytes.length & 0xFF
    plaintext[offset++] = (filenameBytes.length >> 8) & 0xFF
    plaintext.set(filenameBytes, offset); offset += filenameBytes.length
    plaintext.set(data, offset)

    // Chrome caps SCTP user messages at 256KB (RTCSctpTransport.maxMessageSize).
    // Olm encryption adds ~80 bytes overhead. Chunk the plaintext so each
    // encrypted message stays well under the limit.
    const maxMsg = conn.pc?.sctp?.maxMessageSize || 262144
    // Leave headroom for Olm envelope overhead (~100 bytes)
    const chunkLimit = Math.max(maxMsg - 256, 16384)

    if (plaintext.length <= chunkLimit) {
      // Small enough to send in one shot
      const { data: encrypted } = await bridge.encryptBinary(String(hubId), plaintext)
      conn.dataChannel.send(encrypted instanceof Uint8Array ? encrypted.buffer : encrypted)
    } else {
      // Split into CONTENT_FILE_CHUNK messages
      // First chunk: [0x04][transfer_id][flags=START][original header][chunk data]
      // Middle:      [0x04][transfer_id][flags=0][chunk data]
      // Last:        [0x04][transfer_id][flags=END][chunk data]
      const CONTENT_FILE_CHUNK = 0x04
      const transferId = Math.floor(Math.random() * 256)
      // Header = everything before the file data (content type + sub_id + filename metadata)
      const headerLen = 1 + 1 + subIdBytes.length + 2 + filenameBytes.length
      const header = plaintext.slice(1, headerLen) // skip the 0x03 content type byte
      const fileData = plaintext.slice(headerLen)
      // Chunk size for file data (leave room for chunk envelope + header in first chunk)
      const dataChunkSize = chunkLimit - 4 // [0x04][transferId][flags][...data]

      let pos = 0
      while (pos < fileData.length) {
        const isFirst = pos === 0
        const end = Math.min(pos + (isFirst ? dataChunkSize - header.length : dataChunkSize), fileData.length)
        const isLast = end >= fileData.length
        const flags = (isFirst ? 0x01 : 0) | (isLast ? 0x02 : 0)

        let chunk
        if (isFirst) {
          // First chunk includes the original file header (sub_id, filename)
          chunk = new Uint8Array(3 + header.length + (end - pos))
          chunk[0] = CONTENT_FILE_CHUNK
          chunk[1] = transferId
          chunk[2] = flags
          chunk.set(header, 3)
          chunk.set(fileData.slice(pos, end), 3 + header.length)
        } else {
          chunk = new Uint8Array(3 + (end - pos))
          chunk[0] = CONTENT_FILE_CHUNK
          chunk[1] = transferId
          chunk[2] = flags
          chunk.set(fileData.slice(pos, end), 3)
        }

        const { data: encrypted } = await bridge.encryptBinary(String(hubId), chunk)
        conn.dataChannel.send(encrypted instanceof Uint8Array ? encrypted.buffer : encrypted)
        pos = end
      }
    }
  }

  /**
   * Get the current connection mode for a hub.
   * @returns {string} ConnectionMode value (direct, relayed, unknown)
   */
  getConnectionMode(hubId) {
    const conn = this.#connections.get(hubId)
    return conn?.mode || ConnectionMode.UNKNOWN
  }

  /**
   * Subscribe to events
   */
  on(eventName, callback) {
    if (!this.#eventListeners.has(eventName)) {
      this.#eventListeners.set(eventName, new Set())
    }
    this.#eventListeners.get(eventName).add(callback)

    return () => {
      const listeners = this.#eventListeners.get(eventName)
      if (listeners) {
        listeners.delete(callback)
      }
    }
  }

  /**
   * Subscribe to messages for a specific subscription
   */
  onSubscriptionMessage(subscriptionId, callback) {
    if (!this.#subscriptionListeners.has(subscriptionId)) {
      this.#subscriptionListeners.set(subscriptionId, new Set())
    }
    this.#subscriptionListeners.get(subscriptionId).add(callback)

    return () => {
      const listeners = this.#subscriptionListeners.get(subscriptionId)
      if (listeners) {
        listeners.delete(callback)
      }
    }
  }

  /**
   * Clear subscription listeners
   */
  clearSubscriptionListeners(subscriptionId) {
    this.#subscriptionListeners.delete(subscriptionId)
  }

  // ========== Private Methods ==========

  /**
   * Schedule an ICE restart with exponential backoff.
   * If max attempts exceeded, allows connection to fail for full reconnect.
   */
  #scheduleIceRestart(hubId, conn) {
    // Don't schedule if already pending or max attempts reached
    if (conn.iceRestartTimer) return
    if (conn.iceRestartAttempts >= ICE_RESTART_MAX_ATTEMPTS) {
      console.debug(`[WebRTCTransport] ICE restart max attempts (${ICE_RESTART_MAX_ATTEMPTS}) reached for hub ${hubId}`)
      return
    }

    const delay = ICE_RESTART_DELAY_MS * Math.pow(ICE_RESTART_BACKOFF_MULTIPLIER, conn.iceRestartAttempts)
    console.debug(`[WebRTCTransport] Scheduling ICE restart for hub ${hubId} in ${delay}ms (attempt ${conn.iceRestartAttempts + 1}/${ICE_RESTART_MAX_ATTEMPTS})`)

    conn.iceRestartTimer = setTimeout(() => {
      conn.iceRestartTimer = null
      this.#performIceRestart(hubId, conn)
    }, delay)
  }

  /**
   * Perform ICE restart - renegotiate ICE candidates without tearing down the connection.
   */
  async #performIceRestart(hubId, conn) {
    const { pc } = conn
    if (!pc || pc.connectionState === "closed") return

    conn.iceRestartAttempts++
    console.debug(`[WebRTCTransport] Performing ICE restart for hub ${hubId} (attempt ${conn.iceRestartAttempts})`)

    try {
      // Trigger ICE restart
      pc.restartIce()

      // Create new offer with iceRestart flag
      const offer = await pc.createOffer({ iceRestart: true })
      await pc.setLocalDescription(offer)

      // Send encrypted offer via ActionCable
      const subscription = this.#cableSubscriptions.get(hubId)
      if (!subscription) {
        console.error(`[WebRTCTransport] No signaling subscription for ICE restart`)
        return
      }

      const envelope = await this.#encryptSignal(hubId, {
        type: "offer",
        sdp: offer.sdp,
      })
      subscription.perform("signal", { envelope })

      console.debug(`[WebRTCTransport] ICE restart offer sent for hub ${hubId}`)
    } catch (e) {
      console.error(`[WebRTCTransport] ICE restart failed for hub ${hubId}:`, e)
    }
  }

  /**
   * Detect connection mode (P2P vs relayed) from ICE candidate pair.
   * Returns the mode and updates conn.mode.
   */
  async #detectConnectionMode(hubId, conn) {
    const { pc } = conn
    if (!pc) return ConnectionMode.UNKNOWN

    try {
      const stats = await pc.getStats()
      let selectedPairId = null
      let localCandidateId = null

      // Find the selected candidate pair
      stats.forEach(report => {
        if (report.type === "transport" && report.selectedCandidatePairId) {
          selectedPairId = report.selectedCandidatePairId
        }
      })

      // Get the candidate pair
      if (selectedPairId) {
        const pair = stats.get(selectedPairId)
        if (pair) {
          localCandidateId = pair.localCandidateId
        }
      }

      // Get the local candidate type
      if (localCandidateId) {
        const localCandidate = stats.get(localCandidateId)
        if (localCandidate) {
          const candidateType = localCandidate.candidateType
          console.debug(`[WebRTCTransport] Selected candidate type: ${candidateType}`)

          // relay = TURN, anything else = P2P
          const mode = candidateType === "relay" ? ConnectionMode.RELAYED : ConnectionMode.DIRECT
          conn.mode = mode
          return mode
        }
      }

      // Fallback: check all candidate pairs for the nominated one
      stats.forEach(report => {
        if (report.type === "candidate-pair" && report.nominated && report.state === "succeeded") {
          const localCandidate = stats.get(report.localCandidateId)
          if (localCandidate) {
            const candidateType = localCandidate.candidateType
            console.debug(`[WebRTCTransport] Nominated candidate type: ${candidateType}`)
            conn.mode = candidateType === "relay" ? ConnectionMode.RELAYED : ConnectionMode.DIRECT
          }
        }
      })

      return conn.mode
    } catch (e) {
      console.error(`[WebRTCTransport] Failed to detect connection mode:`, e)
      return ConnectionMode.UNKNOWN
    }
  }

  /**
   * Tear down peer connection internals without emitting events.
   * Removes handlers before closing to prevent cascading cleanup.
   */
  #teardownPeer(conn) {
    if (conn.iceRestartTimer) {
      clearTimeout(conn.iceRestartTimer)
      conn.iceRestartTimer = null
    }

    if (conn.dataChannel) {
      conn.dataChannel.onopen = null
      conn.dataChannel.onclose = null
      conn.dataChannel.onerror = null
      conn.dataChannel.onmessage = null
      // Don't call dc.close() — it sends an SCTP stream reset that
      // webrtc-rs/sctp 0.11.0 can't handle (ErrChunk), crashing the
      // SCTP association before pc.close() can tear down cleanly.
      // pc.close() below handles DC cleanup implicitly.
      conn.dataChannel = null
    }
    if (conn.pc) {
      conn.pc.oniceconnectionstatechange = null
      conn.pc.onconnectionstatechange = null
      conn.pc.onicecandidate = null
      conn.pc.close()
      conn.pc = null
    }

    conn.state = TransportState.DISCONNECTED
    conn.mode = ConnectionMode.UNKNOWN
    conn.iceDisrupted = false
    conn.iceRestartAttempts = 0
    conn.decryptFailures = 0
    conn.pendingCandidates = []
  }

  /**
   * Clean up peer connection on failure/close but keep signaling alive.
   * Called from onconnectionstatechange handlers.
   */
  #cleanupPeer(hubId, conn) {
    this.#teardownPeer(conn)
    this.#emit("connection:state", { hubId, state: "disconnected" })
  }

  #emit(eventName, data) {
    const listeners = this.#eventListeners.get(eventName)
    if (listeners) {
      for (const callback of listeners) {
        try {
          callback(data)
        } catch (e) {
          console.error(`[WebRTCTransport] Event listener error:`, e)
        }
      }
    }

    // Handle subscription messages
    if (eventName === "subscription:message" && data.subscriptionId) {
      const subListeners = this.#subscriptionListeners.get(data.subscriptionId)
      if (subListeners) {
        for (const callback of subListeners) {
          try {
            callback(data.message)
          } catch (e) {
            console.error(`[WebRTCTransport] Subscription listener error:`, e)
          }
        }
      }
    }
  }

  async #fetchIceConfig(hubId) {
    const response = await fetch(`/hubs/${hubId}/webrtc`, {
      credentials: "include",
    })

    if (!response.ok) {
      throw new Error(`Failed to fetch ICE config: ${response.status}`)
    }

    return response.json()
  }

  // ========== ActionCable Signaling ==========

  /**
   * Get the shared ActionCable consumer (same one Turbo uses).
   * Uses getConsumer() instead of createConsumer() to share the existing
   * WebSocket connection that Turbo already manages, avoiding a redundant
   * second WebSocket that may fail to connect independently.
   */
  async #getConsumer() {
    if (!cableConsumer) {
      const { cable } = await import("@hotwired/turbo-rails")
      cableConsumer = await cable.getConsumer()
    }
    return cableConsumer
  }

  /**
   * Subscribe to HubSignalingChannel via ActionCable.
   * Returns immediately — ActionCable buffers subscriptions and confirms
   * them when the WebSocket opens (same pattern as Turbo's subscribeTo).
   * Health events arrive via `received` once the subscription is confirmed.
   */
  async #createSignalingChannel(hubId, browserIdentity) {
    const consumer = await this.#getConsumer()

    console.debug(`[WebRTCTransport] Creating signaling channel: hub=${hubId}, identity=${browserIdentity?.slice(0, 16)}...`)

    const subscription = consumer.subscriptions.create(
      { channel: "HubSignalingChannel", hub_id: hubId, browser_identity: browserIdentity },
      {
        received: (data) => {
          this.#handleSignalingMessage(hubId, data)
        },
        connected: () => {
          console.debug(`[WebRTCTransport] Signaling channel connected for hub ${hubId}`)
          const conn = this.#connections.get(hubId)
          if (conn) conn.signalingConnected = true
          this.#emit("signaling:state", { hubId, state: "connected" })
        },
        disconnected: () => {
          console.debug(`[WebRTCTransport] Signaling channel disconnected for hub ${hubId}`)
          const conn = this.#connections.get(hubId)
          if (conn) conn.signalingConnected = false
          this.#emit("signaling:state", { hubId, state: "disconnected" })
        },
        rejected: () => {
          console.error(`[WebRTCTransport] Signaling channel REJECTED for hub ${hubId} (auth or hub not found)`)
        },
      }
    )
    this.#cableSubscriptions.set(hubId, subscription)
    return subscription
  }

  /**
   * Handle incoming ActionCable message from HubSignalingChannel.
   * Health messages are emitted directly. Signal envelopes are decrypted
   * and routed by their decrypted type (answer, ice).
   */
  async #handleSignalingMessage(hubId, data) {
    if (data.type === "health") {
      // Cache last health status so reconnecting Connections get it immediately
      const conn = this.#connections.get(hubId)
      if (conn) conn.lastHealth = data
      this.#emit("health", { hubId, ...data })
      return
    }

    if (data.type === "signal") {
      // Bundle refresh (type 2) from CLI — ratchet restart via ActionCable
      if (data.envelope?.t === 2 && data.envelope?.b) {
        console.debug("[WebRTCTransport] Received bundle refresh from CLI via ActionCable")
        try {
          const bundleBytes = base64ToBytes(data.envelope.b)
          const bundle = parseBinaryBundle(bundleBytes)
          await bridge.createSession(String(hubId), bundle)
          const conn = this.#connections.get(hubId)
          if (conn) conn.decryptFailures = 0
          this.#emit("session:refreshed", { hubId })
        } catch (err) {
          console.error("[WebRTCTransport] Bundle refresh via AC failed:", err.message)
          this.#emit("session:invalid", { hubId, message: err.message })
        }
        return
      }

      try {
        const decrypted = await this.#decryptSignalEnvelope(hubId, data.envelope)
        if (!decrypted) return

        if (decrypted.type === "answer") {
          console.debug("[WebRTCTransport] Received answer via ActionCable")
          await this.#handleAnswer(hubId, decrypted.sdp)
        } else if (decrypted.type === "ice") {
          console.debug("[WebRTCTransport] Received ICE candidate via ActionCable")
          await this.#handleIceCandidate(hubId, decrypted.candidate)
        }
      } catch (e) {
        console.error("[WebRTCTransport] Signal decryption/handling error:", e)
      }
    }
  }

  /**
   * Decrypt a signal envelope (OlmEnvelope) from ActionCable.
   * Uses unified Olm decryption (same as DataChannel).
   * @returns {object|null} Decrypted signal payload, or null on failure
   */
  async #decryptSignalEnvelope(hubId, envelope) {
    try {
      const { plaintext } = await bridge.decrypt(String(hubId), envelope)
      return typeof plaintext === "string" ? JSON.parse(plaintext) : plaintext
    } catch (err) {
      console.error("[WebRTCTransport] Signal decryption failed:", err.message || err)
      return null
    }
  }

  /**
   * Encrypt a signaling payload (offer, answer, ICE) for transmission.
   * Uses unified Olm encryption. Returns OlmEnvelope object for ActionCable.
   */
  async #encryptSignal(hubId, payload) {
    const { encrypted } = await bridge.encrypt(String(hubId), payload)
    return encrypted
  }

  // ========== WebRTC Signal Handling ==========

  async #handleAnswer(hubId, sdp) {
    const conn = this.#connections.get(hubId)
    if (!conn?.pc) return // No peer connection (signaling-only state)

    // Skip if we've already processed an answer (connection is stable or connected)
    if (conn.pc.signalingState === "stable") {
      console.debug("[WebRTCTransport] Ignoring stale answer (already in stable state)")
      return
    }

    const answer = new RTCSessionDescription({ type: "answer", sdp })
    await conn.pc.setRemoteDescription(answer)

    // Apply pending ICE candidates
    for (const candidate of conn.pendingCandidates) {
      await conn.pc.addIceCandidate(candidate)
    }
    conn.pendingCandidates = []
  }

  async #handleIceCandidate(hubId, candidateData) {
    const conn = this.#connections.get(hubId)
    if (!conn?.pc) return // No peer connection (signaling-only state)

    const candidate = new RTCIceCandidate(candidateData)

    if (conn.pc.remoteDescription) {
      await conn.pc.addIceCandidate(candidate)
    } else {
      conn.pendingCandidates.push(candidate)
    }
  }

  #setupDataChannel(hubId, dataChannel) {
    dataChannel.binaryType = "arraybuffer"

    dataChannel.onopen = () => {
      console.debug(`[WebRTCTransport] DataChannel open for hub ${hubId}`)
      const conn = this.#connections.get(hubId)
      if (conn) {
        conn.state = TransportState.CONNECTED
      }
      this.#emit("connection:state", { hubId, state: "connected" })
    }

    dataChannel.onclose = () => {
      console.debug(`[WebRTCTransport] DataChannel closed for hub ${hubId}`)
      this.#emit("connection:state", { hubId, state: "disconnected" })
    }

    dataChannel.onerror = (error) => {
      console.error(`[WebRTCTransport] DataChannel error:`, error)
    }

    dataChannel.onmessage = (event) => {
      this.#handleDataChannelMessage(hubId, event.data).catch(err => {
        console.error("[WebRTCTransport] Message handler error:", err)
      })
    }
  }

  async #handleDataChannelMessage(hubId, data) {
    try {
      // All DataChannel messages are binary Olm frames:
      // [msg_type:1][raw ciphertext] or [msg_type:1][key:32][ciphertext]
      const raw = data instanceof ArrayBuffer ? new Uint8Array(data) : new Uint8Array(data.buffer || data)

      // Type 2: Bundle refresh from CLI (unencrypted ratchet restart)
      if (raw.length > 0 && raw[0] === MSG_TYPE_BUNDLE_REFRESH) {
        const bundleBytes = raw.slice(1)
        console.debug("[WebRTCTransport] Received bundle refresh from CLI via DataChannel")
        try {
          const bundle = parseBinaryBundle(bundleBytes)
          await bridge.createSession(String(hubId), bundle)
          const conn = this.#connections.get(hubId)
          if (conn) conn.decryptFailures = 0
          this.#emit("session:refreshed", { hubId })
        } catch (err) {
          console.error("[WebRTCTransport] Bundle refresh failed:", err.message)
          this.#emit("session:invalid", { hubId, message: err.message })
        }
        return
      }

      // First byte: 0x00 = PreKey Olm frame, 0x01 = Normal Olm frame
      if (raw.length > 0 && raw[0] <= 0x01) {
        // Binary Olm frame — decrypt
        let plaintext
        try {
          const result = await bridge.decryptBinary(String(hubId), raw)
          plaintext = result.data

          // Reset on successful decrypt
          const conn = this.#connections.get(hubId)
          if (conn) conn.decryptFailures = 0
        } catch (err) {
          console.error("[WebRTCTransport] Olm decryption failed:", err.message || err)
          // Decrypt failures are handled by the CLI — it detects desync via its
          // own failure counter and sends a bundle refresh (type 2). The browser
          // just logs and drops the message.
          return
        }

        if (!plaintext || plaintext.length === 0) return

        // Route by inner content type (first byte of decrypted plaintext)
        const contentType = plaintext[0]

        if (contentType === CONTENT_MSG) {
          // Control message: [CONTENT_MSG][JSON bytes]
          const json = new TextDecoder().decode(plaintext.slice(1))
          const msg = JSON.parse(json)
          this.#routeControlMessage(hubId, msg)
          return
        }

        if (contentType === CONTENT_PTY) {
          // PTY: [CONTENT_PTY][flags:1][sub_id_len:1][sub_id][payload]
          await this.#handlePtyBinary(hubId, plaintext)
          return
        }

        if (contentType === CONTENT_STREAM) {
          // Stream mux: [CONTENT_STREAM][frame_type:1][stream_id:2 BE][payload]
          if (plaintext.length < 4) return
          const frameType = plaintext[1]
          const streamId = (plaintext[2] << 8) | plaintext[3]
          const payload = plaintext.slice(4)
          this.#emit("stream:frame", { hubId, frameType, streamId, payload })
          return
        }

        console.warn("[WebRTCTransport] Unknown content type:", contentType)
        return
      }

      console.warn("[WebRTCTransport] Unexpected non-Olm message on DataChannel, dropping")
    } catch (e) {
      console.error("[WebRTCTransport] Failed to handle message:", e)
    }
  }

  /**
   * Handle binary PTY output (zero base64, zero JSON).
   * Format: [CONTENT_PTY][flags:1][sub_id_len:1][sub_id][raw payload]
   *
   * Emits payload as raw Uint8Array — NOT decoded to text here.
   * The payload includes the CLI's 0x01 prefix byte which gets stripped
   * downstream by TerminalConnection's raw_output handler. Text decoding
   * happens in WebRtcPtyTransport's TextDecoder({ stream: true }) which
   * correctly handles multi-byte UTF-8 split across chunk boundaries.
   */
  async #handlePtyBinary(hubId, plaintext) {
    if (plaintext.length < 4) return // Minimum: type + flags + len + at least 0

    const flags = plaintext[1]
    const compressed = (flags & 0x01) !== 0
    const subIdLen = plaintext[2]
    const subIdStart = 3
    const payloadStart = subIdStart + subIdLen

    if (plaintext.length < payloadStart) return

    const subscriptionId = new TextDecoder().decode(plaintext.slice(subIdStart, payloadStart))
    const payload = plaintext.slice(payloadStart)

    let rawBytes
    if (compressed) {
      const stream = new Blob([payload])
        .stream()
        .pipeThrough(new DecompressionStream("gzip"))
      rawBytes = new Uint8Array(await new Response(stream).arrayBuffer())
    } else {
      rawBytes = payload instanceof Uint8Array ? payload : new Uint8Array(payload)
    }

    this.#emit("subscription:message", {
      subscriptionId,
      message: rawBytes,
    })
  }

  /**
   * Route a decrypted control message (m.botster.msg body).
   * These are the same message types as before, just unwrapped from Olm.
   */
  #routeControlMessage(hubId, msg) {
    // Subscription confirmation
    if (msg.type === "subscribed" && msg.subscriptionId) {
      this.#handleSubscriptionConfirmed(msg.subscriptionId)
      return
    }

    // Push notification protocol messages
    if (msg.type === "vapid_pub") {
      this.#emit("push:vapid_key", { hubId, key: msg.key })
      return
    }
    if (msg.type === "push_sub_ack") {
      this.#emit("push:sub_ack", { hubId })
      return
    }
    if (msg.type === "vapid_keys") {
      this.#emit("push:vapid_keys", { hubId, pub: msg.pub, priv: msg.priv })
      return
    }
    if (msg.type === "push_test_ack") {
      this.#emit("push:test_ack", { hubId, sent: msg.sent })
      return
    }
    if (msg.type === "push_disable_ack") {
      this.#emit("push:disable_ack", { hubId })
      return
    }
    if (msg.type === "push_status") {
      this.#emit("push:status", { hubId, hasKeys: msg.has_keys, browserSubscribed: msg.browser_subscribed, vapidPub: msg.vapid_pub })
      return
    }

    if (msg.subscriptionId) {
      // Message with subscription routing
      this.#emit("subscription:message", {
        subscriptionId: msg.subscriptionId,
        message: msg.data || msg,
      })
    } else if (msg.type === "health") {
      // Health messages via DataChannel (fallback — primary path is ActionCable)
      const conn = this.#connections.get(hubId)
      if (conn) {
        for (const subId of conn.subscriptions.keys()) {
          this.#emit("subscription:message", {
            subscriptionId: subId,
            message: msg,
          })
        }
      }
    }
  }

  async #waitForDataChannel(dataChannel) {
    if (dataChannel?.readyState === "open") return
    if (!dataChannel || dataChannel.readyState === "closed" || dataChannel.readyState === "closing") {
      throw new Error("DataChannel closed")
    }

    return new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        cleanup()
        reject(new Error("DataChannel timeout"))
      }, 30000)

      const cleanup = () => {
        clearTimeout(timeout)
        dataChannel.removeEventListener("open", onOpen)
        dataChannel.removeEventListener("close", onClose)
        dataChannel.removeEventListener("error", onClose)
      }

      const onOpen = () => {
        cleanup()
        resolve()
      }

      const onClose = () => {
        cleanup()
        reject(new Error("DataChannel closed"))
      }

      dataChannel.addEventListener("open", onOpen)
      dataChannel.addEventListener("close", onClose)
      dataChannel.addEventListener("error", onClose)
    })
  }

  /**
   * Wait for CLI to confirm subscription registration.
   * Resolves when CLI sends { type: "subscribed", subscriptionId }.
   */
  async #waitForSubscriptionConfirmed(subscriptionId) {
    return new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        this.#pendingSubscriptions.delete(subscriptionId)
        reject(new Error(`Subscription confirmation timeout for ${subscriptionId}`))
      }, 10000)

      this.#pendingSubscriptions.set(subscriptionId, {
        resolve: () => {
          clearTimeout(timeout)
          this.#pendingSubscriptions.delete(subscriptionId)
          resolve()
        },
        reject,
        timeout,
      })
    })
  }

  /**
   * Handle subscription confirmation from CLI.
   * Called when receiving { type: "subscribed", subscriptionId }.
   */
  #handleSubscriptionConfirmed(subscriptionId) {
    const pending = this.#pendingSubscriptions.get(subscriptionId)
    if (pending) {
      console.debug(`[WebRTCTransport] Subscription confirmed: ${subscriptionId}`)
      pending.resolve()
    }
  }
}

export default WebRTCTransport.instance
