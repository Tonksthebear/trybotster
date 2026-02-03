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
  #eventListeners = new Map() // eventName -> Set<callback>
  #subscriptionListeners = new Map() // subscriptionId -> Set<callback>
  #subscriptionIdCounter = 0
  #pollingTimers = new Map() // hubId -> timer

  static get instance() {
    if (!instance) {
      instance = new WebRTCTransport()
    }
    return instance
  }

  /**
   * Connect to a hub via WebRTC
   */
  async connect(hubId, browserIdentity) {
    let conn = this.#connections.get(hubId)

    if (conn) {
      // Already connected or connecting
      return { state: conn.state }
    }

    // Fetch ICE server configuration
    const iceConfig = await this.#fetchIceConfig(hubId)

    // Create peer connection
    const pc = new RTCPeerConnection({ iceServers: iceConfig.ice_servers })

    conn = {
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
          // Handle compression marker bytes:
          // 0x00 = uncompressed (just strip the marker)
          // 0x1f = gzip compressed (would need decompression)
          let cleanPlaintext = plaintext
          if (typeof plaintext === "string" && plaintext.length > 0) {
            const marker = plaintext.charCodeAt(0)
            if (marker === 0x00) {
              // Uncompressed - strip marker
              cleanPlaintext = plaintext.slice(1)
            } else if (marker === 0x1f) {
              // Gzip compressed - for now, log warning (shouldn't happen with small messages)
              console.warn("[WebRTCTransport] Received gzip compressed message, decompression not implemented")
              return
            }
          }
          msg = typeof cleanPlaintext === "string" ? JSON.parse(cleanPlaintext) : cleanPlaintext
          console.log("[WebRTCTransport] Decrypted message:", msg)
        } catch (decryptErr) {
          console.error("[WebRTCTransport] Decryption failed:", decryptErr, decryptErr.stack)
          return
        }
      }

      if (msg.subscriptionId) {
        this.#emit("subscription:message", {
          subscriptionId: msg.subscriptionId,
          message: msg.data || msg,
        })
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
