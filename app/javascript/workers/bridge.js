/**
 * WorkerBridge - Single point of contact with Workers
 *
 * Architecture:
 * - Main thread (bridge.js) proxies all crypto operations
 * - Crypto Worker (signal_crypto.js) - SharedWorker handling Signal Protocol
 * - Transport Worker - regular Worker handling transport (no crypto)
 *   - signal.js for ActionCable WebSocket transport
 *   - webrtc.js for WebRTC DataChannel transport
 *
 * The main thread talks directly to crypto SharedWorker for encrypt/decrypt,
 * and to transport Worker for send/receive.
 */

// Singleton instance
let instance = null

class WorkerBridge {
  // Transport worker
  #worker = null
  #workerPort = null
  #pendingRequests = new Map()
  #requestId = 0
  #transport = "actioncable" // "actioncable" or "webrtc"

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
   * @param {string} options.workerUrl - URL to ActionCable transport Worker (signal.js)
   * @param {string} options.webrtcWorkerUrl - URL to WebRTC transport Worker (webrtc.js)
   * @param {string} options.cryptoWorkerUrl - URL to the crypto SharedWorker (signal_crypto.js)
   * @param {string} options.wasmJsUrl - URL to libsignal_wasm.js
   * @param {string} options.wasmBinaryUrl - URL to libsignal_wasm_bg.wasm
   * @param {string} options.transport - Transport type: "actioncable" (default) or "webrtc"
   */
  async init({ workerUrl, webrtcWorkerUrl, cryptoWorkerUrl, wasmJsUrl, wasmBinaryUrl, transport = "actioncable" }) {
    if (this.#initialized) return
    if (this.#initPromise) return this.#initPromise

    this.#initPromise = this.#doInit({ workerUrl, webrtcWorkerUrl, cryptoWorkerUrl, wasmJsUrl, wasmBinaryUrl, transport })
    return this.#initPromise
  }

  async #doInit({ workerUrl, webrtcWorkerUrl, cryptoWorkerUrl, wasmJsUrl, wasmBinaryUrl, transport }) {
    try {
      this.#transport = transport

      // 1. Create crypto SharedWorker first and initialize WASM
      this.#cryptoWorker = new SharedWorker(cryptoWorkerUrl, { type: "module", name: "signal-crypto" })
      this.#cryptoWorkerPort = this.#cryptoWorker.port
      this.#cryptoWorkerPort.onmessage = (e) => this.#handleCryptoMessage(e)
      this.#cryptoWorkerPort.start()

      // Initialize WASM via crypto worker
      await this.sendCrypto("init", { wasmJsUrl, wasmBinaryUrl })

      // 2. Create transport Worker based on transport type
      const transportUrl = transport === "webrtc" ? webrtcWorkerUrl : workerUrl
      console.log(`[WorkerBridge] Using ${transport} transport: ${transportUrl}`)

      this.#worker = new Worker(transportUrl, { type: "module" })
      this.#workerPort = this.#worker
      this.#worker.onmessage = (e) => this.#handleMessage(e)
      this.#worker.onerror = (e) =>
        console.error("[WorkerBridge] Transport worker error:", e)

      // Initialize transport worker
      await this.send("init", {})

      this.#initialized = true
    } catch (error) {
      console.error("[WorkerBridge] Failed to initialize:", error)
      this.#initPromise = null
      throw error
    }
  }

  /**
   * Get the current transport type
   * @returns {string} - "actioncable" or "webrtc"
   */
  get transport() {
    return this.#transport
  }

  /**
   * Handle messages from the transport worker
   */
  #handleMessage(messageEvent) {
    const data = messageEvent.data

    // Handle ping (heartbeat) - respond with pong
    if (data.event === "ping") {
      this.#workerPort.postMessage({ action: "pong" })
      return
    }

    // Handle events (no id, has event field)
    if (data.event) {
      this.#dispatchEvent(data)
      return
    }

    // Handle request/response (has id)
    if (data.id !== undefined) {
      const pending = this.#pendingRequests.get(data.id)
      if (!pending) return

      this.#pendingRequests.delete(data.id)

      if (data.success) {
        pending.resolve(data.result)
      } else {
        pending.reject(new Error(data.error))
      }
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
   * Send a request to the transport worker and wait for response
   * @param {string} action - The action to perform
   * @param {Object} params - Parameters for the action
   * @param {number} timeout - Timeout in milliseconds (default: 10000)
   * @returns {Promise<any>} - The result from the worker
   */
  send(action, params = {}, timeout = 10000) {
    return new Promise((resolve, reject) => {
      if (!this.#workerPort) {
        reject(new Error("Transport worker not initialized"))
        return
      }

      const id = ++this.#requestId

      const timer = setTimeout(() => {
        this.#pendingRequests.delete(id)
        reject(new Error(`Transport worker timeout: ${action}`))
      }, timeout)

      this.#pendingRequests.set(id, {
        resolve: (result) => {
          clearTimeout(timer)
          resolve(result)
        },
        reject: (error) => {
          clearTimeout(timer)
          reject(error)
        },
      })

      this.#workerPort.postMessage({ id, action, ...params })
    })
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
   * Create a new Signal session from a bundle
   * @param {string} hubId - The hub ID
   * @param {Object|string} bundleJson - The session bundle
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
   * @returns {Promise<{envelope: string}>}
   */
  async encrypt(hubId, message) {
    // Convert to string if needed (handles Uint8Array binary messages)
    const messageStr = this.#messageToString(message)
    return this.sendCrypto("encrypt", { hubId, message: messageStr })
  }

  /**
   * Decrypt an envelope from a hub
   * @param {string} hubId - The hub ID
   * @param {string|Object} envelope - The encrypted envelope
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
   * Process a sender key distribution message
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
   * Subscribe to worker events
   * @param {string} eventName - Event name (e.g., "connection:state", "subscription:message")
   * @param {Function} callback - Callback function receiving the event data
   * @returns {Function} - Unsubscribe function
   */
  on(eventName, callback) {
    if (!this.#eventListeners.has(eventName)) {
      this.#eventListeners.set(eventName, new Set())
    }
    this.#eventListeners.get(eventName).add(callback)

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
  }

  /**
   * Check if the bridge is initialized
   */
  get isInitialized() {
    return this.#initialized
  }

  // ===========================================================================
  // WebRTC signaling methods (only used when transport is "webrtc")
  // ===========================================================================

  /**
   * Handle an incoming WebRTC answer from CLI
   * @param {string} hubId - The hub ID
   * @param {string} sdp - The SDP answer
   */
  async handleWebRTCAnswer(hubId, sdp) {
    if (this.#transport !== "webrtc") {
      console.warn("[WorkerBridge] handleWebRTCAnswer called but transport is not webrtc")
      return
    }
    return this.send("handleAnswer", { hubId, sdp })
  }

  /**
   * Handle an incoming ICE candidate from CLI
   * @param {string} hubId - The hub ID
   * @param {Object} candidate - The ICE candidate
   */
  async handleWebRTCIce(hubId, candidate) {
    if (this.#transport !== "webrtc") {
      console.warn("[WorkerBridge] handleWebRTCIce called but transport is not webrtc")
      return
    }
    return this.send("handleIce", { hubId, candidate })
  }
}

// Export singleton getter and class
export { WorkerBridge }
export default WorkerBridge.instance
