/**
 * Connection - Base class for typed connection wrappers.
 *
 * Provides common functionality:
 *   - WorkerBridge communication for encrypted channels
 *   - Signal session lifecycle (via SharedWorker)
 *   - Event subscription (typed subclasses add domain-specific events)
 *   - State tracking
 *
 * Subclasses implement:
 *   - channelName() - ActionCable channel class name
 *   - channelParams() - Subscription params
 *   - handleMessage(msg) - Domain-specific message routing
 */

import bridge from "workers/bridge"
import { ensureSignalReady, parseBundleFromFragment } from "signal"

export const ConnectionState = {
  DISCONNECTED: "disconnected",
  LOADING: "loading",
  CONNECTING: "connecting",
  CONNECTED: "connected",
  ERROR: "error",
}

export class Connection {
  #unsubscribers = []

  constructor(key, options, manager) {
    this.key = key
    this.options = options
    this.manager = manager

    this.subscriptionId = null      // Worker subscription ID
    this.session = null             // Kept for backward compatibility
    this.identityKey = null
    this.state = ConnectionState.DISCONNECTED
    this.errorReason = null

    // Event subscribers: Map<eventName, Set<callback>>
    this.subscribers = new Map()
  }

  // ========== Lifecycle (called by ConnectionManager) ==========

  /**
   * Initialize the connection. Called by ConnectionManager.acquire().
   * Ensures worker is ready, connects to hub, and subscribes to channel.
   */
  async initialize() {
    try {
      this.#setState(ConnectionState.LOADING)

      // Ensure worker is initialized
      const workerUrl = document.querySelector('meta[name="signal-worker-url"]')?.content
      const wasmJsUrl = document.querySelector('meta[name="signal-wasm-js-url"]')?.content
      const wasmBinaryUrl = document.querySelector('meta[name="signal-wasm-binary-url"]')?.content
      await ensureSignalReady(workerUrl, wasmJsUrl, wasmBinaryUrl)

      this.#setState(ConnectionState.CONNECTING)

      // Get cable URL and ActionCable module URL
      const cableUrl = document.querySelector('meta[name="action-cable-url"]')?.content || "/cable"
      const actionCableModuleUrl = document.querySelector('meta[name="actioncable-module-url"]')?.content

      // Parse session bundle from fragment if requested
      let sessionBundle = this.options.sessionBundle || null
      if (!sessionBundle && this.options.fromFragment) {
        sessionBundle = parseBundleFromFragment()
        if (sessionBundle) {
          // Strip the fragment so the bundle isn't reprocessed on reload
          history.replaceState(null, "", location.pathname + location.search)
        }
      }

      // Connect to hub (creates or reuses connection, may create session from bundle)
      const hubId = this.getHubId()
      console.log("[Connection] Connecting to hub:", hubId, { cableUrl, actionCableModuleUrl, hasBundle: !!sessionBundle })
      const connectResult = await bridge.send("connect", {
        hubId,
        cableUrl,
        actionCableModuleUrl,
        sessionBundle
      })
      console.log("[Connection] Connect result:", connectResult)

      if (!connectResult.sessionExists) {
        this.#setError("no_session", "No session available. Scan QR code to pair.")
        return
      }

      // Get identity key for channel params
      const keyResult = await bridge.send("getIdentityKey", { hubId })
      this.identityKey = keyResult.identityKey

      // Subscribe to channel
      const subscribeResult = await bridge.send("subscribe", {
        hubId,
        channel: this.channelName(),
        params: this.channelParams(),
        reliable: this.isReliable()
      })

      this.subscriptionId = subscribeResult.subscriptionId

      // Listen for subscription events
      this.#setupEventListeners()

      this.#setState(ConnectionState.CONNECTED)
      this.emit("connected", this)
    } catch (error) {
      console.error(`[${this.constructor.name}] Initialize failed:`, error)
      this.#setError("init_failed", error.message)
    }
  }

  /**
   * Set up listeners for worker events related to this connection.
   */
  #setupEventListeners() {
    const hubId = this.getHubId()

    // Listen for subscription messages
    const unsubMsg = bridge.onSubscriptionMessage(this.subscriptionId, (message) => {
      this.handleMessage(message)
    })
    this.#unsubscribers.push(unsubMsg)

    // Listen for connection state changes
    const unsubState = bridge.on("connection:state", (event) => {
      if (event.hubId !== hubId) return

      if (event.state === "disconnected") {
        this.#setState(ConnectionState.DISCONNECTED)
        this.emit("disconnected")
      } else if (event.state === "connected" && this.state === ConnectionState.DISCONNECTED) {
        // Reconnected - restore to connected state
        this.#setState(ConnectionState.CONNECTED)
        this.emit("reconnected")
      }
    })
    this.#unsubscribers.push(unsubState)

    // Listen for subscription confirmed
    const unsubConfirmed = bridge.on("subscription:confirmed", (event) => {
      if (event.subscriptionId !== this.subscriptionId) return
      // Already in CONNECTED state from initialize, but subclasses may use this
    })
    this.#unsubscribers.push(unsubConfirmed)

    // Listen for subscription rejected
    const unsubRejected = bridge.on("subscription:rejected", (event) => {
      if (event.subscriptionId !== this.subscriptionId) return
      this.#setError("subscription_rejected", event.reason || "Subscription rejected")
    })
    this.#unsubscribers.push(unsubRejected)

    // Listen for session invalid
    const unsubSession = bridge.on("session:invalid", (event) => {
      if (event.hubId !== hubId) return
      this.#setError("session_invalid", event.message)
    })
    this.#unsubscribers.push(unsubSession)
  }

  /**
   * Destroy the connection. Called by ConnectionManager.destroy().
   * Unsubscribes from channel, cleans up listeners, notifies subscribers.
   */
  async destroy() {
    // Cleanup event listeners
    for (const unsub of this.#unsubscribers) {
      unsub()
    }
    this.#unsubscribers = []

    // Unsubscribe from channel
    if (this.subscriptionId) {
      try {
        await bridge.send("unsubscribe", { subscriptionId: this.subscriptionId })
      } catch (e) {
        // Ignore errors during cleanup
      }
      bridge.clearSubscriptionListeners(this.subscriptionId)
      this.subscriptionId = null
    }

    // Disconnect from hub (decrements ref count in worker)
    const hubId = this.getHubId()
    if (hubId) {
      try {
        await bridge.send("disconnect", { hubId })
      } catch (e) {
        // Ignore errors during cleanup
      }
    }

    this.identityKey = null
    this.session = null
    this.#setState(ConnectionState.DISCONNECTED)
    this.emit("destroyed")
    this.subscribers.clear()
  }

  /**
   * Release this connection (decrement ref count).
   * Called by controllers in their disconnect().
   */
  release() {
    this.manager.release(this.key)
  }

  // ========== Abstract methods (override in subclasses) ==========

  /**
   * ActionCable channel class name.
   * @returns {string}
   */
  channelName() {
    throw new Error("Subclass must implement channelName()")
  }

  /**
   * Subscription params for the channel.
   * @returns {Object}
   */
  channelParams() {
    throw new Error("Subclass must implement channelParams()")
  }

  /**
   * Extract hubId from options. Override if hubId comes from elsewhere.
   * @returns {string}
   */
  getHubId() {
    return this.options.hubId
  }

  /**
   * Whether to use reliable delivery. Default true.
   * @returns {boolean}
   */
  isReliable() {
    return true
  }

  /**
   * Handle a decrypted message. Subclasses route to domain-specific events.
   * @param {Object} message
   */
  handleMessage(message) {
    // Default: emit as generic message
    this.emit("message", message)
  }

  // ========== Public API ==========

  /**
   * Send a message through the secure channel.
   * @param {string} type - Message type
   * @param {Object} data - Message payload
   * @returns {Promise<boolean>}
   */
  async send(type, data = {}) {
    if (!this.subscriptionId) {
      return false
    }

    try {
      await bridge.send("send", {
        subscriptionId: this.subscriptionId,
        message: { type, ...data }
      })
      return true
    } catch (error) {
      console.error(`[${this.constructor.name}] Send failed:`, error)
      return false
    }
  }

  /**
   * Check if connected.
   * @returns {boolean}
   */
  isConnected() {
    return this.state === ConnectionState.CONNECTED
  }

  /**
   * Get current state.
   * @returns {string}
   */
  getState() {
    return this.state
  }

  /**
   * Get error reason if in error state.
   * @returns {string|null}
   */
  getError() {
    return this.errorReason
  }

  // ========== Event System ==========

  /**
   * Subscribe to an event.
   * @param {string} event - Event name
   * @param {Function} callback - Event handler
   * @returns {Function} - Unsubscribe function
   */
  on(event, callback) {
    if (!this.subscribers.has(event)) {
      this.subscribers.set(event, new Set())
    }
    this.subscribers.get(event).add(callback)

    // Return unsubscribe function
    return () => this.off(event, callback)
  }

  /**
   * Unsubscribe from an event.
   * @param {string} event - Event name
   * @param {Function} callback - Event handler
   */
  off(event, callback) {
    this.subscribers.get(event)?.delete(callback)
  }

  /**
   * Emit an event to all subscribers.
   * @param {string} event - Event name
   * @param {*} data - Event data
   */
  emit(event, data) {
    const callbacks = this.subscribers.get(event)
    if (!callbacks) return

    for (const callback of callbacks) {
      try {
        callback(data)
      } catch (error) {
        console.error(`[${this.constructor.name}] Event handler error:`, error)
      }
    }
  }

  // ========== Private ==========

  #setState(newState) {
    const prevState = this.state
    this.state = newState

    if (newState !== ConnectionState.ERROR) {
      this.errorReason = null
    }

    const stateInfo = { state: newState, prevState, error: this.errorReason }
    this.emit("stateChange", stateInfo)

    // Notify ConnectionManager subscribers (passive observers)
    this.manager.notifySubscribers(this.key, stateInfo)
  }

  #setError(reason, message) {
    this.errorReason = message
    this.#setState(ConnectionState.ERROR)
    this.emit("error", { reason, message })
  }
}
