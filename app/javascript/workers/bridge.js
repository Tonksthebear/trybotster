/**
 * WorkerBridge - Single point of contact with Workers
 *
 * Architecture:
 * - Main thread (bridge.js) proxies all crypto operations
 * - Crypto Worker (olm_crypto.js) - SharedWorker handling vodozemac Olm crypto
 * - Transport: WebRTCTransport in main thread (RTCPeerConnection not available in Workers)
 *
 * The main thread talks directly to crypto SharedWorker for encrypt/decrypt,
 * and to WebRTCTransport for send/receive.
 */

// Singleton instance
let instance = null

// WebRTC transport (lazily imported)
let webrtcTransport = null

class WorkerBridge {
  // Crypto SharedWorker
  #cryptoWorker = null
  #cryptoWorkerPort = null
  #pendingCryptoRequests = new Map()
  #cryptoRequestId = 0

  #initialized = false
  #initPromise = null
  #eventListeners = new Map() // eventName -> Set<callback>
  #subscriptionListeners = new Map() // subscriptionId -> Set<callback>

  /**
   * Get the singleton instance
   */
  static get instance() {
    if (!instance) {
      instance = new WorkerBridge()
    }
    return instance
  }

  /**
   * Initialize the workers (idempotent)
   * @param {Object} options
   * @param {string} options.cryptoWorkerUrl - URL to the crypto SharedWorker
   * @param {string} options.wasmJsUrl - URL to vodozemac-wasm JS glue
   * @param {string} options.wasmBinaryUrl - URL to vodozemac-wasm binary (.wasm)
   */
  async init({ cryptoWorkerUrl, wasmJsUrl, wasmBinaryUrl }) {
    if (this.#initialized) return
    if (this.#initPromise) return this.#initPromise

    this.#initPromise = this.#doInit({ cryptoWorkerUrl, wasmJsUrl, wasmBinaryUrl })
    return this.#initPromise
  }

  async #doInit({ cryptoWorkerUrl, wasmJsUrl, wasmBinaryUrl }) {
    try {
      // 1. Create crypto SharedWorker first and initialize WASM
      this.#cryptoWorker = new SharedWorker(cryptoWorkerUrl, { type: "module", name: "vodozemac-crypto" })
      this.#cryptoWorkerPort = this.#cryptoWorker.port
      this.#cryptoWorkerPort.onmessage = (e) => this.#handleCryptoMessage(e)
      this.#cryptoWorkerPort.start()

      // Initialize WASM via crypto worker
      await this.sendCrypto("init", { wasmJsUrl, wasmBinaryUrl })

      // 2. Create WebRTC transport (runs in main thread - RTCPeerConnection not available in Workers)
      console.debug(`[WorkerBridge] Using WebRTC transport`)
      const { default: transport } = await import("transport/webrtc")
      webrtcTransport = transport

      // Wire up event forwarding from WebRTCTransport.
      // Every event must include { event: "<name>" } so #dispatchEvent can route it.
      webrtcTransport.on("connection:state", (data) => this.#dispatchEvent({ event: "connection:state", ...data }))
      webrtcTransport.on("connection:mode", (data) => this.#dispatchEvent({ event: "connection:mode", ...data }))
      webrtcTransport.on("subscription:message", (data) => this.#dispatchEvent({ event: "subscription:message", ...data }))
      webrtcTransport.on("subscription:confirmed", (data) => this.#dispatchEvent({ event: "subscription:confirmed", ...data }))
      webrtcTransport.on("health", (data) => this.#dispatchEvent({ event: "health", ...data }))
      webrtcTransport.on("session:invalid", (data) => this.#dispatchEvent({ event: "session:invalid", ...data }))
      webrtcTransport.on("session:refreshed", (data) => this.#dispatchEvent({ event: "session:refreshed", ...data }))
      webrtcTransport.on("signaling:state", (data) => this.#dispatchEvent({ event: "signaling:state", ...data }))
      webrtcTransport.on("stream:frame", (data) => this.#dispatchEvent({ event: "stream:frame", ...data }))
      webrtcTransport.on("push:status", (data) => this.#dispatchEvent({ event: "push:status", ...data }))
      webrtcTransport.on("push:vapid_key", (data) => this.#dispatchEvent({ event: "push:vapid_key", ...data }))
      webrtcTransport.on("push:sub_ack", (data) => this.#dispatchEvent({ event: "push:sub_ack", ...data }))
      webrtcTransport.on("push:vapid_keys", (data) => this.#dispatchEvent({ event: "push:vapid_keys", ...data }))
      webrtcTransport.on("push:test_ack", (data) => this.#dispatchEvent({ event: "push:test_ack", ...data }))
      webrtcTransport.on("push:disable_ack", (data) => this.#dispatchEvent({ event: "push:disable_ack", ...data }))

      this.#initialized = true
    } catch (error) {
      console.error("[WorkerBridge] Failed to initialize:", error)
      this.#initPromise = null
      throw error
    }
  }

  /**
   * Handle messages from the crypto SharedWorker
   */
  #handleCryptoMessage(messageEvent) {
    const data = messageEvent.data

    // Handle ping (heartbeat) - respond with pong
    if (data.event === "ping") {
      this.#cryptoWorkerPort.postMessage({ action: "pong" })
      return
    }

    // Handle request/response (has id)
    if (data.id !== undefined) {
      const pending = this.#pendingCryptoRequests.get(data.id)
      if (!pending) return

      this.#pendingCryptoRequests.delete(data.id)

      if (data.success) {
        pending.resolve(data.result)
      } else {
        pending.reject(new Error(data.error))
      }
    }
  }

  /**
   * Dispatch an event to registered listeners
   */
  #dispatchEvent(data) {
    const { event, subscriptionId } = data

    // Dispatch to event listeners
    const listeners = this.#eventListeners.get(event)
    if (listeners) {
      for (const callback of listeners) {
        try {
          callback(data)
        } catch (e) {
          console.error(`[WorkerBridge] Event listener error for ${event}:`, e)
        }
      }
    }

    // Dispatch subscription messages to subscription listeners
    if (event === "subscription:message" && subscriptionId) {
      const subListeners = this.#subscriptionListeners.get(subscriptionId)
      if (subListeners) {
        for (const callback of subListeners) {
          try {
            callback(data.message)
          } catch (e) {
            console.error(`[WorkerBridge] Subscription listener error:`, e)
          }
        }
      }
    }
  }

  /**
   * Send a request to the WebRTC transport
   * @param {string} action - The action to perform
   * @param {Object} params - Parameters for the action
   * @returns {Promise<any>} - The result from the transport
   */
  async send(action, params = {}) {
    if (!webrtcTransport) {
      throw new Error("WebRTC transport not initialized")
    }

    switch (action) {
      case "connect":
        return webrtcTransport.connect(params.hubId, params.browserIdentity)
      case "connectSignaling":
        return webrtcTransport.connectSignaling(params.hubId, params.browserIdentity)
      case "connectPeer":
        return webrtcTransport.connectPeer(params.hubId)
      case "disconnectPeer":
        return webrtcTransport.disconnectPeer(params.hubId)
      case "probePeerHealth":
        return webrtcTransport.probePeerHealth(params.hubId)
      case "disconnect":
        return webrtcTransport.disconnect(params.hubId)
      case "subscribe": {
        // Build binary control frame: [0x00][JSON bytes]
        const subscribePayload = {
          type: "subscribe",
          subscriptionId: params.subscriptionId,
          channel: params.channel,
          params: params.params,
        }
        const jsonBytes = new TextEncoder().encode(JSON.stringify(subscribePayload))
        const plaintext = new Uint8Array(1 + jsonBytes.length)
        plaintext[0] = 0x00  // CONTENT_MSG
        plaintext.set(jsonBytes, 1)

        const { data: encrypted } = await this.encryptBinary(params.hubId, plaintext)
        return webrtcTransport.subscribe(params.hubId, params.channel, params.params, params.subscriptionId, encrypted)
      }
      case "unsubscribe":
        return webrtcTransport.unsubscribe(params.subscriptionId)
      case "sendRaw":
        return webrtcTransport.sendRaw(params.subscriptionId, params.message)
      case "sendEncrypted":
        return webrtcTransport.sendEncrypted(params.hubId, params.encrypted)
      case "sendStreamFrame":
        return webrtcTransport.sendStreamFrame(params.hubId, params.frameType, params.streamId, params.payload)
      case "sendPtyInput":
        return webrtcTransport.sendPtyInput(params.hubId, params.subscriptionId, params.data)
      case "sendFileInput":
        return webrtcTransport.sendFileInput(params.hubId, params.subscriptionId, params.data, params.filename)
      case "sendControlMessage": {
        // Send arbitrary JSON control message via encrypted DataChannel
        const jsonBytes = new TextEncoder().encode(JSON.stringify(params.message))
        const plaintext = new Uint8Array(1 + jsonBytes.length)
        plaintext[0] = 0x00  // CONTENT_MSG
        plaintext.set(jsonBytes, 1)

        const { data: encrypted } = await this.encryptBinary(params.hubId, plaintext)
        return webrtcTransport.sendEncrypted(params.hubId, encrypted)
      }
      default:
        throw new Error(`Unknown action: ${action}`)
    }
  }

  /**
   * Send a request to the crypto SharedWorker and wait for response
   * @param {string} action - The action to perform
   * @param {Object} params - Parameters for the action
   * @param {number} timeout - Timeout in milliseconds (default: 10000)
   * @returns {Promise<any>} - The result from the crypto worker
   */
  sendCrypto(action, params = {}, timeout = 10000) {
    return new Promise((resolve, reject) => {
      if (!this.#cryptoWorkerPort) {
        reject(new Error("Crypto worker not initialized"))
        return
      }

      const id = ++this.#cryptoRequestId

      const timer = setTimeout(() => {
        this.#pendingCryptoRequests.delete(id)
        reject(new Error(`Crypto worker timeout: ${action}`))
      }, timeout)

      this.#pendingCryptoRequests.set(id, {
        resolve: (result) => {
          clearTimeout(timer)
          resolve(result)
        },
        reject: (error) => {
          clearTimeout(timer)
          reject(error)
        },
      })

      this.#cryptoWorkerPort.postMessage({ id, action, ...params })
    })
  }

  // ===========================================================================
  // Crypto convenience methods (delegate to crypto SharedWorker)
  // ===========================================================================

  /**
   * Create a new Olm session from a device key bundle
   * @param {string} hubId - The hub ID
   * @param {Object|string} bundleJson - The device key bundle
   * @returns {Promise<{created: boolean, identityKey: string}>}
   */
  async createSession(hubId, bundleJson) {
    return this.sendCrypto("createSession", { hubId, bundleJson })
  }

  /**
   * Check if a session exists for a hub
   * @param {string} hubId - The hub ID
   * @returns {Promise<{hasSession: boolean}>}
   */
  async hasSession(hubId) {
    return this.sendCrypto("hasSession", { hubId })
  }

  /**
   * Encrypt a message (JSON envelope for ActionCable signaling).
   * @param {string} hubId - The hub ID
   * @param {string|Object} message - The message to encrypt (string or JSON-serializable)
   * @returns {Promise<{encrypted: Object}>} OlmEnvelope { t, b, k? }
   */
  async encrypt(hubId, message) {
    const messageStr = typeof message === "string" ? message : JSON.stringify(message)
    return this.sendCrypto("encrypt", { hubId, message: messageStr })
  }

  /**
   * Decrypt a JSON OlmEnvelope (ActionCable signaling).
   * @param {string} hubId - The hub ID
   * @param {string|Object} encryptedData - OlmEnvelope { t, b, k? }
   * @returns {Promise<{plaintext: any}>}
   */
  async decrypt(hubId, encryptedData) {
    const dataStr = typeof encryptedData === "string" ? encryptedData : JSON.stringify(encryptedData)
    return this.sendCrypto("decrypt", { hubId, encryptedData: dataStr })
  }

  /**
   * Encrypt raw bytes into a binary DataChannel frame (zero base64).
   * @param {string} hubId - The hub ID
   * @param {Uint8Array} plaintext - Raw bytes to encrypt
   * @returns {Promise<{data: Uint8Array}>} Binary frame
   */
  async encryptBinary(hubId, plaintext) {
    return this.sendCrypto("encryptBinary", { hubId, plaintext })
  }

  /**
   * Decrypt a binary DataChannel frame (zero base64).
   * @param {string} hubId - The hub ID
   * @param {Uint8Array} data - Binary frame from DataChannel
   * @returns {Promise<{data: Uint8Array}>} Decrypted plaintext bytes
   */
  async decryptBinary(hubId, data) {
    return this.sendCrypto("decryptBinary", { hubId, data })
  }

  /**
   * Get the identity key for a session
   * @param {string} hubId - The hub ID
   * @returns {Promise<{identityKey: string}>}
   */
  async getIdentityKey(hubId) {
    return this.sendCrypto("getIdentityKey", { hubId })
  }

  /**
   * Clear a session
   * @param {string} hubId - The hub ID
   * @returns {Promise<{cleared: boolean}>}
   */
  async clearSession(hubId) {
    return this.sendCrypto("clearSession", { hubId })
  }

  /**
   * Clear all sessions (memory + IndexedDB).
   * @returns {Promise<{cleared: boolean, count: number}>}
   */
  async clearAllSessions() {
    return this.sendCrypto("clearAllSessions", {})
  }

  /**
   * Subscribe to transport events
   * @param {string} eventName - Event name (e.g., "connection:state", "subscription:message")
   * @param {Function} callback - Callback function receiving the event data
   * @returns {Function} - Unsubscribe function
   */
  on(eventName, callback) {
    if (!this.#eventListeners.has(eventName)) {
      this.#eventListeners.set(eventName, new Set())
    }
    this.#eventListeners.get(eventName).add(callback)

    // Events flow: WebRTCTransport → bridge.#dispatchEvent → local listeners.
    // Do NOT also register with WebRTCTransport directly (set up in #doInit).

    // Return unsubscribe function
    return () => {
      const listeners = this.#eventListeners.get(eventName)
      if (listeners) {
        listeners.delete(callback)
        if (listeners.size === 0) {
          this.#eventListeners.delete(eventName)
        }
      }
    }
  }

  /**
   * Subscribe to messages for a specific subscription
   * @param {string} subscriptionId - The subscription ID
   * @param {Function} callback - Callback function receiving the message
   * @returns {Function} - Unsubscribe function
   */
  onSubscriptionMessage(subscriptionId, callback) {
    if (!this.#subscriptionListeners.has(subscriptionId)) {
      this.#subscriptionListeners.set(subscriptionId, new Set())
    }
    this.#subscriptionListeners.get(subscriptionId).add(callback)

    // NOTE: For WebRTC, messages are already forwarded via the
    // webrtcTransport.on("subscription:message") listener set up in constructor.
    // Do NOT also register with webrtcTransport.onSubscriptionMessage() here,
    // as that would cause duplicate message delivery.

    // Return unsubscribe function
    return () => {
      const listeners = this.#subscriptionListeners.get(subscriptionId)
      if (listeners) {
        listeners.delete(callback)
        if (listeners.size === 0) {
          this.#subscriptionListeners.delete(subscriptionId)
        }
      }
    }
  }

  /**
   * Remove all listeners for a subscription (used when unsubscribing)
   */
  clearSubscriptionListeners(subscriptionId) {
    this.#subscriptionListeners.delete(subscriptionId)
    webrtcTransport?.clearSubscriptionListeners(subscriptionId)
  }

  /**
   * Check if the bridge is initialized
   */
  get isInitialized() {
    return this.#initialized
  }
}

// Export singleton getter and class
export { WorkerBridge }
export default WorkerBridge.instance
