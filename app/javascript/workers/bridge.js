/**
 * WorkerBridge - Single point of contact with Workers
 *
 * Architecture:
 * - Main thread (bridge.js) proxies all crypto operations
 * - Crypto Worker (matrix_crypto.js) - SharedWorker handling Matrix Olm/Megolm crypto
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
  #pendingRequests = new Map()
  #requestId = 0

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
   * @param {string} options.cryptoWorkerUrl - URL to the crypto SharedWorker (matrix_crypto.js)
   * @param {string} options.wasmJsUrl - URL to matrix-sdk-crypto-wasm JS
   * @param {string} options.wasmBinaryUrl - URL to WASM binary (optional, Matrix SDK loads internally)
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
      this.#cryptoWorker = new SharedWorker(cryptoWorkerUrl, { type: "module", name: "matrix-crypto" })
      this.#cryptoWorkerPort = this.#cryptoWorker.port
      this.#cryptoWorkerPort.onmessage = (e) => this.#handleCryptoMessage(e)
      this.#cryptoWorkerPort.start()

      // Initialize WASM via crypto worker
      await this.sendCrypto("init", { wasmJsUrl, wasmBinaryUrl })

      // 2. Create WebRTC transport (runs in main thread - RTCPeerConnection not available in Workers)
      console.debug(`[WorkerBridge] Using WebRTC transport`)
      const { default: transport } = await import("transport/webrtc")
      webrtcTransport = transport

      // Wire up event forwarding from WebRTCTransport
      webrtcTransport.on("connection:state", (data) => this.#dispatchEvent(data))
      webrtcTransport.on("connection:mode", (data) => this.#dispatchEvent({ event: "connection:mode", ...data }))
      webrtcTransport.on("subscription:message", (data) => this.#dispatchEvent({ event: "subscription:message", ...data }))
      webrtcTransport.on("subscription:confirmed", (data) => this.#dispatchEvent({ event: "subscription:confirmed", ...data }))

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
      case "init":
        return { initialized: true }
      case "connect":
        return webrtcTransport.connect(params.hubId, params.browserIdentity)
      case "connectSignaling":
        return webrtcTransport.connectSignaling(params.hubId, params.browserIdentity)
      case "connectPeer":
        return webrtcTransport.connectPeer(params.hubId)
      case "disconnectPeer":
        return webrtcTransport.disconnectPeer(params.hubId)
      case "disconnect":
        return webrtcTransport.disconnect(params.hubId)
      case "subscribe": {
        // Encrypt the subscribe message so the browser's first message is a
        // CryptoEnvelope - establishes the Matrix session on the CLI side.
        const subscribeMsg = {
          type: "subscribe",
          subscriptionId: params.subscriptionId,
          channel: params.channel,
          params: params.params,
        }
        const { envelope } = await this.encrypt(params.hubId, subscribeMsg)
        const envelopeObj = typeof envelope === "string" ? JSON.parse(envelope) : envelope
        return webrtcTransport.subscribe(params.hubId, params.channel, params.params, params.subscriptionId, envelopeObj)
      }
      case "unsubscribe":
        return webrtcTransport.unsubscribe(params.subscriptionId)
      case "sendRaw":
        return webrtcTransport.sendRaw(params.subscriptionId, params.message)
      case "sendEnvelope":
        return webrtcTransport.sendEnvelope(params.hubId, params.envelope)
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
   * Create a new Matrix session from a device key bundle
   * @param {string} hubId - The hub ID
   * @param {Object|string} bundleJson - The device key bundle (Matrix format)
   * @returns {Promise<{created: boolean, identityKey: string}>}
   */
  async createSession(hubId, bundleJson) {
    return this.sendCrypto("createSession", { hubId, bundleJson })
  }

  /**
   * Load an existing session from storage
   * @param {string} hubId - The hub ID
   * @returns {Promise<{loaded: boolean, fromCache?: boolean, error?: string}>}
   */
  async loadSession(hubId) {
    return this.sendCrypto("loadSession", { hubId })
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
   * Encrypt a message for a hub
   * @param {string} hubId - The hub ID
   * @param {string|Uint8Array|Object} message - The message to encrypt
   * @returns {Promise<{envelope: string}>} CryptoEnvelope as JSON string
   */
  async encrypt(hubId, message) {
    // Convert to string if needed (handles Uint8Array binary messages)
    const messageStr = this.#messageToString(message)
    return this.sendCrypto("encrypt", { hubId, message: messageStr })
  }

  /**
   * Decrypt a CryptoEnvelope from a hub
   * @param {string} hubId - The hub ID
   * @param {string|Object} envelope - The encrypted envelope { t, c, s, d }
   * @returns {Promise<{plaintext: any}>}
   */
  async decrypt(hubId, envelope) {
    const envelopeStr = typeof envelope === "string" ? envelope : JSON.stringify(envelope)
    return this.sendCrypto("decrypt", { hubId, envelope: envelopeStr })
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
   * Process a sender key distribution message (for group sessions)
   * @param {string} hubId - The hub ID
   * @param {string} distributionB64 - The distribution message in base64
   * @returns {Promise<{processed: boolean}>}
   */
  async processSenderKeyDistribution(hubId, distributionB64) {
    return this.sendCrypto("processSenderKeyDistribution", { hubId, distributionB64 })
  }

  /**
   * Convert message to string for encryption.
   * Uint8Array -> Latin-1 string (each byte -> char code).
   * Objects -> JSON string.
   * @private
   */
  #messageToString(message) {
    if (message instanceof Uint8Array) {
      // Binary data: convert to Latin-1 string (byte values 0-255 -> char codes)
      return String.fromCharCode.apply(null, message)
    } else if (typeof message === "string") {
      return message
    } else {
      return JSON.stringify(message)
    }
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

    // Also register with WebRTCTransport
    const webrtcUnsub = webrtcTransport?.on(eventName, callback)

    // Return unsubscribe function
    return () => {
      const listeners = this.#eventListeners.get(eventName)
      if (listeners) {
        listeners.delete(callback)
        if (listeners.size === 0) {
          this.#eventListeners.delete(eventName)
        }
      }
      if (webrtcUnsub) webrtcUnsub()
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
