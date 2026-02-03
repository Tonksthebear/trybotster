/**
 * WebRTCTransport - Main thread WebRTC connection manager
 *
 * Singleton that manages WebRTC peer connections in the main thread.
 * RTCPeerConnection is not available in Workers, so this must run in main thread.
 *
 * Persists across Turbo navigation via singleton pattern.
 *
 * Architecture:
 * - Main thread: WebRTCTransport (this) handles RTCPeerConnection, DataChannel
 * - SharedWorker: signal_crypto.js handles encryption/decryption
 * - Signaling: HTTP polling to Rails
 */

import bridge from "workers/bridge"

// Singleton instance
let instance = null

// Connection states
export const TransportState = {
  DISCONNECTED: "disconnected",
  CONNECTING: "connecting",
  CONNECTED: "connected",
  ERROR: "error",
}

class WebRTCTransport {
  #connections = new Map() // hubId -> { pc, dataChannel, state, subscriptions }
  #connectPromises = new Map() // hubId -> Promise (pending connection)
  #eventListeners = new Map() // eventName -> Set<callback>
  #subscriptionListeners = new Map() // subscriptionId -> Set<callback>
  #subscriptionIdCounter = 0
  #pollingTimers = new Map() // hubId -> timer

  constructor() {
    // Clean up connections on page unload to prevent zombie ICE packets
    const cleanup = () => {
      for (const [hubId, conn] of this.#connections) {
        if (conn.dataChannel) conn.dataChannel.close()
        if (conn.pc) conn.pc.close()
      }
      this.#connections.clear()
      this.#connectPromises.clear()
    }

    window.addEventListener("beforeunload", cleanup)
    // Also handle Turbo navigation
    document.addEventListener("turbo:before-visit", cleanup)
  }

  static get instance() {
    if (!instance) {
      instance = new WebRTCTransport()
    }
    return instance
  }

  /**
   * Connect to a hub via WebRTC.
   * Multiple callers share the same connection - subsequent calls wait for
   * the pending connection or return the existing one.
   */
  async connect(hubId, browserIdentity) {
    // If already connected, return existing connection
    let conn = this.#connections.get(hubId)
    if (conn) {
      return { state: conn.state }
    }

    // If connection in progress, wait for it
    const pendingPromise = this.#connectPromises.get(hubId)
    if (pendingPromise) {
      return pendingPromise
    }

    // Create connection promise for other callers to wait on
    const connectPromise = this.#doConnect(hubId, browserIdentity)
    this.#connectPromises.set(hubId, connectPromise)

    try {
      const result = await connectPromise
      return result
    } finally {
      this.#connectPromises.delete(hubId)
    }
  }

  /**
   * Internal: Actually establish the WebRTC connection
   */
  async #doConnect(hubId, browserIdentity) {
    // Fetch ICE server configuration
    const iceConfig = await this.#fetchIceConfig(hubId)

    // Create peer connection
    const pc = new RTCPeerConnection({ iceServers: iceConfig.ice_servers })

    const conn = {
      pc,
      dataChannel: null,
      state: TransportState.CONNECTING,
      hubId,
      browserIdentity,
      subscriptions: new Map(),
      pendingCandidates: [],
    }
    this.#connections.set(hubId, conn)

    // Set up ICE candidate handling
    pc.onicecandidate = async (event) => {
      if (event.candidate) {
        await this.#sendSignal(hubId, browserIdentity, {
          signal_type: "ice",
          candidate: event.candidate.toJSON(),
        })
      }
    }

    // Set up connection state handling
    pc.onconnectionstatechange = () => {
      const state = pc.connectionState
      console.log(`[WebRTCTransport] Connection state: ${state}`)

      if (state === "connected") {
        conn.state = TransportState.CONNECTED
        this.#emit("connection:state", { hubId, state: "connected" })
      } else if (state === "failed" || state === "disconnected" || state === "closed") {
        conn.state = TransportState.DISCONNECTED
        this.#emit("connection:state", { hubId, state: "disconnected" })
        this.#stopPolling(hubId)
        // Clean up so reconnect can create fresh connection
        if (conn.dataChannel) {
          conn.dataChannel.close()
        }
        pc.close()
        this.#connections.delete(hubId)
      }
    }

    // Create data channel (browser initiates)
    const dataChannel = pc.createDataChannel("relay", { ordered: true })
    conn.dataChannel = dataChannel
    this.#setupDataChannel(hubId, dataChannel)

    // Create and send offer
    const offer = await pc.createOffer()
    await pc.setLocalDescription(offer)

    await this.#sendSignal(hubId, browserIdentity, {
      signal_type: "offer",
      sdp: offer.sdp,
    })

    // Start polling for answer
    this.#startPolling(hubId, browserIdentity)

    return { state: TransportState.CONNECTING }
  }

  /**
   * Disconnect from a hub
   */
  async disconnect(hubId) {
    const conn = this.#connections.get(hubId)
    if (!conn) return

    this.#stopPolling(hubId)

    if (conn.dataChannel) {
      conn.dataChannel.close()
    }
    if (conn.pc) {
      conn.pc.close()
    }

    this.#connections.delete(hubId)
    this.#emit("connection:state", { hubId, state: "disconnected" })
  }

  /**
   * Subscribe to a channel (maps to DataChannel usage)
   */
  async subscribe(hubId, channelName, params) {
    const conn = this.#connections.get(hubId)
    if (!conn) {
      throw new Error(`No connection for hub ${hubId}`)
    }

    const subscriptionId = `sub_${++this.#subscriptionIdCounter}_${Date.now()}`

    conn.subscriptions.set(subscriptionId, {
      channelName,
      params,
    })

    // Wait for data channel to be open
    if (conn.dataChannel?.readyState !== "open") {
      await this.#waitForDataChannel(conn.dataChannel)
    }

    // Send subscribe message through data channel
    const msg = JSON.stringify({
      type: "subscribe",
      subscriptionId,
      channel: channelName,
      params,
    })
    conn.dataChannel.send(msg)

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
   * Send a pre-encrypted Signal envelope directly through the DataChannel.
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

  async #sendSignal(hubId, browserIdentity, signal) {
    const response = await fetch(`/hubs/${hubId}/webrtc_signals`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      credentials: "include",
      body: JSON.stringify({
        ...signal,
        browser_identity: browserIdentity,
      }),
    })

    if (!response.ok) {
      throw new Error(`Failed to send signal: ${response.status}`)
    }
  }

  #startPolling(hubId, browserIdentity) {
    if (this.#pollingTimers.has(hubId)) return

    const poll = async () => {
      const conn = this.#connections.get(hubId)
      if (!conn || conn.state === TransportState.CONNECTED) {
        this.#stopPolling(hubId)
        return
      }

      try {
        const response = await fetch(
          `/hubs/${hubId}/webrtc_signals?browser_identity=${encodeURIComponent(browserIdentity)}`,
          { credentials: "include" }
        )

        if (response.ok) {
          const { signals } = await response.json()

          for (const signal of signals) {
            if (signal.type === "answer") {
              console.log("[WebRTCTransport] Received answer")
              await this.#handleAnswer(hubId, signal.sdp)
            } else if (signal.type === "ice") {
              console.log("[WebRTCTransport] Received ICE candidate")
              await this.#handleIceCandidate(hubId, signal.candidate)
            }
          }
        }
      } catch (e) {
        console.warn("[WebRTCTransport] Poll error:", e)
      }

      // Continue polling
      if (this.#connections.has(hubId)) {
        this.#pollingTimers.set(hubId, setTimeout(poll, 1000))
      }
    }

    poll()
  }

  #stopPolling(hubId) {
    const timer = this.#pollingTimers.get(hubId)
    if (timer) {
      clearTimeout(timer)
      this.#pollingTimers.delete(hubId)
    }
  }

  async #handleAnswer(hubId, sdp) {
    const conn = this.#connections.get(hubId)
    if (!conn) return

    // Skip if we've already processed an answer (connection is stable or connected)
    if (conn.pc.signalingState === "stable") {
      console.log("[WebRTCTransport] Ignoring stale answer (already in stable state)")
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
    if (!conn) return

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
      console.log(`[WebRTCTransport] DataChannel open for hub ${hubId}`)
      const conn = this.#connections.get(hubId)
      if (conn) {
        conn.state = TransportState.CONNECTED
      }
      this.#stopPolling(hubId)
      this.#emit("connection:state", { hubId, state: "connected" })
    }

    dataChannel.onclose = () => {
      console.log(`[WebRTCTransport] DataChannel closed for hub ${hubId}`)
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
      console.log("[WebRTCTransport] Received raw data:", typeof data, data instanceof ArrayBuffer ? `ArrayBuffer(${data.byteLength})` : (typeof data === "string" ? data.substring(0, 100) : data))

      // Handle binary data (ArrayBuffer)
      let textData = data
      if (data instanceof ArrayBuffer) {
        textData = new TextDecoder().decode(data)
        console.log("[WebRTCTransport] Decoded ArrayBuffer to text:", textData.substring(0, 100))
      }

      const parsed = typeof textData === "string" ? JSON.parse(textData) : textData
      console.log("[WebRTCTransport] Parsed message keys:", Object.keys(parsed))

      // Check if this is a Signal envelope (encrypted message from CLI)
      // Signal envelopes have short keys: t (type), c (ciphertext), s (sender), d (device)
      let msg = parsed
      if (parsed.t !== undefined && parsed.c && parsed.s) {
        console.log("[WebRTCTransport] Detected Signal envelope, decrypting for hub:", hubId, typeof hubId)
        // Decrypt using bridge
        try {
          // Ensure hubId is a string
          const hubIdStr = String(hubId)
          const { plaintext } = await bridge.decrypt(hubIdStr, parsed)

          // Plaintext is base64-encoded (for UTF-8 safety through Signal WASM).
          // Decode to get the compressed/uncompressed bytes, then decompress if needed.
          let bytes
          try {
            const binaryString = atob(plaintext)
            bytes = new Uint8Array(binaryString.length)
            for (let i = 0; i < binaryString.length; i++) {
              bytes[i] = binaryString.charCodeAt(i)
            }
          } catch (b64Err) {
            console.error("[WebRTCTransport] Base64 decode failed:", b64Err)
            return
          }

          // Handle compression marker:
          // 0x00 = uncompressed (strip marker, rest is JSON)
          // 0x1f = gzip compressed (decompress, then JSON)
          let jsonStr
          const marker = bytes[0]
          if (marker === 0x00) {
            // Uncompressed - strip marker and decode as UTF-8
            jsonStr = new TextDecoder().decode(bytes.slice(1))
          } else if (marker === 0x1f) {
            // Gzip compressed - decompress
            try {
              const stream = new Blob([bytes.slice(1)])
                .stream()
                .pipeThrough(new DecompressionStream("gzip"))
              jsonStr = await new Response(stream).text()
            } catch (decompressErr) {
              console.error("[WebRTCTransport] Gzip decompression failed:", decompressErr)
              return
            }
          } else {
            // No recognized marker - try as raw UTF-8
            jsonStr = new TextDecoder().decode(bytes)
          }

          msg = JSON.parse(jsonStr)
          console.log("[WebRTCTransport] Decrypted message:", msg.subscriptionId, msg.raw ? `(raw ${msg.raw.length} chars)` : Object.keys(msg.data || {}))
        } catch (decryptErr) {
          console.error("[WebRTCTransport] Decryption failed:", decryptErr.message || decryptErr)
          // Log envelope details for debugging
          console.error("[WebRTCTransport] Failed envelope:", {
            type: parsed.t,
            senderPrefix: parsed.s?.substring(0, 20),
            ciphertextLength: parsed.c?.length,
          })
          return
        }
      }

      if (msg.subscriptionId) {
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
        // Broadcast to all subscriptions for this hub
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
}

export default WebRTCTransport.instance
