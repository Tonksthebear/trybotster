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
 * - SharedWorker: matrix_crypto.js handles encryption/decryption
 * - Signaling: ActionCable push via HubSignalingChannel (encrypted envelopes)
 *   Rails is a dumb pipe — envelopes are opaque, only browser/CLI can decrypt.
 */

import bridge from "workers/bridge"

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
export const ConnectionMode = {
  UNKNOWN: "unknown",     // Not yet determined
  DIRECT: "direct",       // P2P (host, srflx, prflx candidates)
  RELAYED: "relayed",     // Through TURN server
}

// Grace period before closing idle connections (ms)
const DISCONNECT_GRACE_PERIOD_MS = 3000

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
        if (conn.dataChannel) conn.dataChannel.close()
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
    if (conn) return { state: conn.state }

    const conn2 = {
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
    }
    this.#connections.set(hubId, conn2)

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
    if (conn.pc) return { state: conn.state }

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
   */
  async subscribe(hubId, channelName, params, providedSubscriptionId = null, encryptedEnvelope = null) {
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

    // Send subscribe message through data channel.
    // When an encrypted envelope is provided, the subscribe message is inside it
    // as an Olm pre-key message — establishing the encrypted session on the CLI side.
    if (encryptedEnvelope) {
      conn.dataChannel.send(JSON.stringify(encryptedEnvelope))
    } else {
      conn.dataChannel.send(JSON.stringify({
        type: "subscribe",
        subscriptionId,
        channel: channelName,
        params,
      }))
    }

    // Wait for CLI to confirm subscription before allowing input.
    // This prevents race condition where input arrives before CLI registers subscription.
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
          conn.dataChannel.send(JSON.stringify({
            type: "unsubscribe",
            subscriptionId,
          }))
        }
        conn.subscriptions.delete(subscriptionId)
        return { unsubscribed: true }
      }
    }
    return { unsubscribed: false }
  }

  /**
   * Send raw data through the data channel
   */
  async sendRaw(subscriptionId, message) {
    for (const [hubId, conn] of this.#connections) {
      if (conn.subscriptions.has(subscriptionId)) {
        if (conn.dataChannel?.readyState !== "open") {
          throw new Error("DataChannel not open")
        }

        const wrapped = { subscriptionId, data: message }
        conn.dataChannel.send(JSON.stringify(wrapped))
        return { sent: true }
      }
    }
    throw new Error(`Subscription ${subscriptionId} not found`)
  }

  /**
   * Send a pre-encrypted crypto envelope directly through the DataChannel.
   * Used for browser → CLI communication where encryption happens in
   * Connection.#sendEncrypted via the bridge.
   */
  async sendEnvelope(hubId, envelope) {
    const conn = this.#connections.get(hubId)
    if (!conn) {
      throw new Error(`No connection for hub ${hubId}`)
    }
    if (conn.dataChannel?.readyState !== "open") {
      throw new Error("DataChannel not open")
    }

    conn.dataChannel.send(JSON.stringify(envelope))
    return { sent: true }
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
      conn.dataChannel.close()
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

  /**
   * Clean up everything (peer + signaling) and remove from connections map.
   */
  #cleanupConnection(hubId, conn) {
    this.#teardownPeer(conn)
    this.#emit("connection:state", { hubId, state: "disconnected" })
    this.#connections.delete(hubId)
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
   * Lazily create ActionCable consumer (shared across all hub connections).
   */
  async #getConsumer() {
    if (!cableConsumer) {
      const { createConsumer } = await import("@rails/actioncable")
      cableConsumer = createConsumer()
    }
    return cableConsumer
  }

  /**
   * Subscribe to HubSignalingChannel via ActionCable.
   * Resolves when subscription is confirmed (connected callback fires).
   * Receives encrypted signal envelopes and health status messages.
   */
  async #createSignalingChannel(hubId, browserIdentity) {
    const consumer = await this.#getConsumer()

    return new Promise((resolve) => {
      const subscription = consumer.subscriptions.create(
        { channel: "HubSignalingChannel", hub_id: hubId, browser_identity: browserIdentity },
        {
          received: (data) => {
            this.#handleSignalingMessage(hubId, data)
          },
          connected: () => {
            console.debug(`[WebRTCTransport] Signaling channel connected for hub ${hubId}`)
            resolve(subscription)
          },
          disconnected: () => {
            console.debug(`[WebRTCTransport] Signaling channel disconnected for hub ${hubId}`)
          },
        }
      )
      // Store subscription immediately — ActionCable can deliver `received`
      // before `connected` fires (initial health transmit). connectPeer()
      // needs the subscription in #cableSubscriptions to send signals.
      this.#cableSubscriptions.set(hubId, subscription)
    })
  }

  /**
   * Handle incoming ActionCable message from HubSignalingChannel.
   * Health messages are emitted directly. Signal envelopes are decrypted
   * and routed by their decrypted type (answer, ice).
   */
  async #handleSignalingMessage(hubId, data) {
    if (data.type === "health") {
      this.#emit("health", { hubId, ...data })
      return
    }

    if (data.type === "signal") {
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
   * Decrypt a signal envelope (offer, answer, ICE) from ActionCable.
   *
   * Unlike #decryptEnvelope (used for DataChannel messages which are
   * base64-encoded and optionally compressed), signaling payloads are
   * raw JSON — no base64, no compression.
   *
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
   * Returns the envelope object ready for ActionCable perform.
   */
  async #encryptSignal(hubId, payload) {
    const { envelope } = await bridge.encrypt(String(hubId), payload)
    return typeof envelope === "string" ? JSON.parse(envelope) : envelope
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
      // Handle binary data (ArrayBuffer)
      let textData = data
      if (data instanceof ArrayBuffer) {
        textData = new TextDecoder().decode(data)
      }

      const parsed = typeof textData === "string" ? JSON.parse(textData) : textData

      // Check if this is a crypto envelope (encrypted message from CLI)
      // Crypto envelopes have short keys: t (type), c (ciphertext), s (sender)
      // Control messages (subscribed, agent_list) may be plaintext during bootstrap
      // before the Olm session is established via pre-key message
      let msg = parsed
      if (parsed.t !== undefined && parsed.c && parsed.s) {
        msg = await this.#decryptEnvelope(hubId, parsed)
        if (!msg) {
          // Track consecutive decryption failures (mirrors CLI's 3-strike detection)
          const conn = this.#connections.get(hubId)
          if (conn) {
            conn.decryptFailures++
            if (conn.decryptFailures >= 3) {
              console.warn(`[WebRTCTransport] ${conn.decryptFailures} consecutive decryption failures, session invalid`)
              conn.decryptFailures = 0
              this.#emit("session:invalid", { hubId, message: "Crypto session out of sync. Please re-pair." })
            }
          }
          return
        }
        // Reset on successful decrypt
        const conn = this.#connections.get(hubId)
        if (conn) conn.decryptFailures = 0
      }

      // Handle subscription confirmation (control message, decrypted)
      if (msg.type === "subscribed" && msg.subscriptionId) {
        this.#handleSubscriptionConfirmed(msg.subscriptionId)
        return
      }

      // Handle session invalid (plaintext from CLI when decryption repeatedly fails)
      if (msg.type === "session_invalid") {
        console.warn("[WebRTCTransport] Session invalid:", msg.reason)
        this.#emit("session:invalid", { hubId, message: msg.message || msg.reason || "Session invalid" })
        return
      }

      if (msg.subscriptionId) {
        // Message with subscription routing (decrypted)
        // Check for raw binary data (base64-encoded PTY output)
        if (msg.raw) {
          // Decode base64 to Uint8Array
          const binaryString = atob(msg.raw)
          const bytes = new Uint8Array(binaryString.length)
          for (let i = 0; i < binaryString.length; i++) {
            bytes[i] = binaryString.charCodeAt(i)
          }
          this.#emit("subscription:message", {
            subscriptionId: msg.subscriptionId,
            message: bytes,
            isRaw: true,
          })
        } else {
          this.#emit("subscription:message", {
            subscriptionId: msg.subscriptionId,
            message: msg.data || msg,
          })
        }
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
    } catch (e) {
      console.error("[WebRTCTransport] Failed to parse message:", e)
    }
  }

  /**
   * Decrypt a crypto envelope and decompress the payload.
   * @returns {object|null} Decrypted message object, or null on failure
   */
  async #decryptEnvelope(hubId, envelope) {
    try {
      const { plaintext } = await bridge.decrypt(String(hubId), envelope)

      // Plaintext is base64-encoded, decode to bytes
      const binaryString = atob(plaintext)
      const bytes = new Uint8Array(binaryString.length)
      for (let i = 0; i < binaryString.length; i++) {
        bytes[i] = binaryString.charCodeAt(i)
      }

      // Handle compression marker: 0x00 = uncompressed, 0x1f = gzip
      const marker = bytes[0]
      let jsonStr
      if (marker === 0x00) {
        jsonStr = new TextDecoder().decode(bytes.slice(1))
      } else if (marker === 0x1f) {
        const stream = new Blob([bytes.slice(1)])
          .stream()
          .pipeThrough(new DecompressionStream("gzip"))
        jsonStr = await new Response(stream).text()
      } else {
        // No marker - try as raw UTF-8
        jsonStr = new TextDecoder().decode(bytes)
      }

      return JSON.parse(jsonStr)
    } catch (err) {
      console.error("[WebRTCTransport] Decryption failed:", err.message || err)
      return null
    }
  }

  async #waitForDataChannel(dataChannel) {
    if (dataChannel?.readyState === "open") return

    return new Promise((resolve, reject) => {
      const timeout = setTimeout(() => reject(new Error("DataChannel timeout")), 30000)

      const onOpen = () => {
        clearTimeout(timeout)
        dataChannel.removeEventListener("open", onOpen)
        resolve()
      }

      dataChannel.addEventListener("open", onOpen)
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
