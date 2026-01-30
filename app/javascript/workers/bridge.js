/**
 * WorkerBridge - Single point of contact with the SharedWorker
 *
 * Handles all communication with the consolidated SharedWorker that manages
 * both Signal encryption and ActionCable connections.
 */

// Singleton instance
let instance = null

class WorkerBridge {
  #worker = null
  #workerPort = null
  #pendingRequests = new Map()
  #requestId = 0
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
   * Initialize the worker (idempotent)
   * @param {Object} options
   * @param {string} options.workerUrl - URL to the SharedWorker script
   * @param {string} options.wasmJsUrl - URL to libsignal_wasm.js
   * @param {string} options.wasmBinaryUrl - URL to libsignal_wasm_bg.wasm
   */
  async init({ workerUrl, wasmJsUrl, wasmBinaryUrl }) {
    if (this.#initialized) return
    if (this.#initPromise) return this.#initPromise

    this.#initPromise = this.#doInit({ workerUrl, wasmJsUrl, wasmBinaryUrl })
    return this.#initPromise
  }

  async #doInit({ workerUrl, wasmJsUrl, wasmBinaryUrl }) {
    try {
      // Try SharedWorker first
      if (typeof SharedWorker !== "undefined") {
        try {
          this.#worker = new SharedWorker(workerUrl, {
            type: "module",
            name: "signal",
          })
          this.#workerPort = this.#worker.port
          this.#workerPort.onmessage = (e) => this.#handleMessage(e)
          this.#workerPort.onmessageerror = (e) =>
            console.error("[WorkerBridge] Message error:", e)
          this.#worker.onerror = (e) =>
            console.error("[WorkerBridge] Worker error:", e)
          this.#workerPort.start()
        } catch (sharedError) {
          console.warn(
            "[WorkerBridge] SharedWorker failed, falling back:",
            sharedError
          )
          this.#worker = null
          this.#workerPort = null
        }
      }

      // Fallback to regular Worker
      if (!this.#workerPort) {
        console.warn(
          "[WorkerBridge] Using regular Worker - multi-tab features disabled"
        )
        this.#worker = new Worker(workerUrl, { type: "module" })
        this.#workerPort = this.#worker
        this.#worker.onmessage = (e) => this.#handleMessage(e)
        this.#worker.onerror = (e) =>
          console.error("[WorkerBridge] Worker error:", e)
      }

      // Initialize WASM
      await this.send("init", { wasmJsUrl, wasmBinaryUrl })
      this.#initialized = true
    } catch (error) {
      console.error("[WorkerBridge] Failed to initialize:", error)
      this.#initPromise = null
      throw error
    }
  }

  /**
   * Handle messages from the worker
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
   * Send a request to the worker and wait for response
   * @param {string} action - The action to perform
   * @param {Object} params - Parameters for the action
   * @param {number} timeout - Timeout in milliseconds (default: 10000)
   * @returns {Promise<any>} - The result from the worker
   */
  send(action, params = {}, timeout = 10000) {
    return new Promise((resolve, reject) => {
      if (!this.#workerPort) {
        reject(new Error("Worker not initialized"))
        return
      }

      const id = ++this.#requestId

      const timer = setTimeout(() => {
        this.#pendingRequests.delete(id)
        reject(new Error(`Worker timeout: ${action}`))
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
}

// Export singleton getter and class
export { WorkerBridge }
export default WorkerBridge.instance
