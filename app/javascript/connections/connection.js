/**
 * Connection - Base class for typed connection wrappers.
 *
 * Provides common functionality:
 *   - WorkerBridge communication for encrypted channels
 *   - Signal session lifecycle (via SharedWorker)
 *   - Event subscription (typed subclasses add domain-specific events)
 *   - State tracking
 *
 * Lifecycle:
 *   - initialize() establishes hub connection (WebSocket + Signal session)
 *   - subscribe() creates channel subscription (triggers CLI handshake)
 *   - unsubscribe() removes channel subscription (keeps hub alive)
 *   - destroy() tears down everything
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
  CONNECTING: "connecting",  // Hub connected, not subscribed
  CONNECTED: "connected",    // Hub connected AND subscribed
  ERROR: "error",
}

export class Connection {
  #unsubscribers = []
  #subscriptionUnsubscribers = []
  #hubConnected = false
  #subscribing = false      // Lock to prevent concurrent subscribe/unsubscribe
  #resubscribing = false    // Lock to prevent concurrent resubscribe on stale send

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

    // CLI ready signal handling.
    // Prevents race condition where browser sends before CLI subscribes,
    // causing seq=1 to be lost and multi-second delays.
    // Only enabled for channels that override requiresCliReady() to return true.
    this.cliReady = !this.requiresCliReady() // Start ready if not required
    this.inputBuffer = []
  }

  // ========== Lifecycle (called by ConnectionManager) ==========

  /**
   * Initialize the connection. Called by ConnectionManager.acquire().
   * Establishes hub connection (WebSocket + Signal session) and subscribes.
   */
  async initialize() {
    try {
      this.#setState(ConnectionState.LOADING)

      // Ensure worker is initialized
      const workerUrl = document.querySelector('meta[name="signal-worker-url"]')?.content
      const wasmJsUrl = document.querySelector('meta[name="signal-wasm-js-url"]')?.content
      const wasmBinaryUrl = document.querySelector('meta[name="signal-wasm-binary-url"]')?.content
      await ensureSignalReady(workerUrl, wasmJsUrl, wasmBinaryUrl)

      // Connect to hub
      await this.#connectHub()
      if (this.state === ConnectionState.ERROR) return

      // Subscribe to channel
      await this.subscribe()
    } catch (error) {
      console.error(`[${this.constructor.name}] Initialize failed:`, error)
      this.#setError("init_failed", error.message)
    }
  }

  /**
   * Connect to the hub (WebSocket + Signal session).
   * Called by initialize() or can be used to reconnect.
   */
  async #connectHub() {
    if (this.#hubConnected) return

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
    const connectResult = await bridge.send("connect", {
      hubId,
      cableUrl,
      actionCableModuleUrl,
      sessionBundle
    })

    if (!connectResult.sessionExists) {
      this.#setError("no_session", "No session available. Scan QR code to pair.")
      return
    }

    // Get identity key for channel params
    const keyResult = await bridge.send("getIdentityKey", { hubId })
    this.identityKey = keyResult.identityKey
    this.#hubConnected = true

    // Set up hub-level event listeners (connection state, session invalid)
    this.#setupHubEventListeners()
  }

  /**
   * Subscribe to the channel. Creates a new subscription in the worker,
   * which triggers Rails subscribed callback and CLI handshake.
   *
   * @param {Object} options
   * @param {boolean} options.force - If true, unsubscribe existing subscription first
   *                                  to get fresh handshake. Default false.
   */
  async subscribe({ force = false } = {}) {
    if (!this.#hubConnected) {
      throw new Error("Cannot subscribe: hub not connected")
    }

    // If already subscribed and not forcing refresh, just emit connected and return
    if (this.subscriptionId && !force) {
      this.#setState(ConnectionState.CONNECTED)
      this.emit("connected", this)
      return
    }

    // Wait for any in-progress subscribe/unsubscribe
    while (this.#subscribing) {
      await new Promise(resolve => setTimeout(resolve, 10))
    }

    // Re-check after waiting - another caller might have subscribed
    if (this.subscriptionId && !force) {
      this.#setState(ConnectionState.CONNECTED)
      this.emit("connected", this)
      return
    }

    this.#subscribing = true

    try {
      // Unsubscribe first if forcing refresh
      if (this.subscriptionId && force) {
        await this.#doUnsubscribe()
      }

      // Reset CLI ready state - need fresh handshake (only if required)
      this.cliReady = !this.requiresCliReady()
      this.inputBuffer = []

      const hubId = this.getHubId()

      const subscribeResult = await bridge.send("subscribe", {
        hubId,
        channel: this.channelName(),
        params: this.channelParams(),
        reliable: this.isReliable()
      })

      this.subscriptionId = subscribeResult.subscriptionId
      this.#setupSubscriptionEventListeners()

      this.#setState(ConnectionState.CONNECTED)
      this.emit("connected", this)
    } finally {
      this.#subscribing = false
    }
  }

  /**
   * Unsubscribe from the channel. Keeps hub connection alive.
   * Call this when controller disconnects during navigation.
   */
  async unsubscribe() {
    // Wait for any in-progress subscribe to complete
    while (this.#subscribing) {
      await new Promise(resolve => setTimeout(resolve, 10))
    }

    if (!this.subscriptionId) return

    this.#subscribing = true
    try {
      await this.#doUnsubscribe()
    } finally {
      this.#subscribing = false
    }
  }

  /**
   * Internal unsubscribe implementation (no locking).
   */
  async #doUnsubscribe() {
    if (!this.subscriptionId) return

    // Capture and clear subscriptionId FIRST to prevent race conditions
    // where send() tries to use it while we're unsubscribing
    const oldSubscriptionId = this.subscriptionId
    this.subscriptionId = null

    // Back to CONNECTING state (hub still connected, but not subscribed)
    this.#setState(ConnectionState.CONNECTING)

    // Clean up subscription event listeners
    this.#clearSubscriptionEventListeners()

    // Unsubscribe in worker
    try {
      await bridge.send("unsubscribe", { subscriptionId: oldSubscriptionId })
    } catch (e) {
      console.warn(`[${this.constructor.name}] Unsubscribe error (ignored):`, e)
    }

    bridge.clearSubscriptionListeners(oldSubscriptionId)
  }

  /**
   * Set up listeners for hub-level events (connection state, session invalid).
   * These persist across subscribe/unsubscribe cycles.
   */
  #setupHubEventListeners() {
    const hubId = this.getHubId()

    // Listen for connection state changes
    const unsubState = bridge.on("connection:state", (event) => {
      if (event.hubId !== hubId) return

      if (event.state === "disconnected") {
        this.#hubConnected = false
        this.#setState(ConnectionState.DISCONNECTED)
        this.emit("disconnected")
      } else if (event.state === "connected" && !this.#hubConnected) {
        this.#hubConnected = true
        this.emit("reconnected")

        // Auto-resubscribe after reconnection - old subscription is stale
        if (this.subscriptionId) {
          console.log(`[${this.constructor.name}] Auto-resubscribing after reconnect`)
          // Clear stale subscription ID and resubscribe
          const oldSubId = this.subscriptionId
          this.subscriptionId = null
          this.#clearSubscriptionEventListeners()
          bridge.clearSubscriptionListeners(oldSubId)

          this.subscribe().catch(err => {
            console.error(`[${this.constructor.name}] Resubscribe failed:`, err)
            this.#setError("resubscribe_failed", err.message)
          })
        }
      }
    })
    this.#unsubscribers.push(unsubState)

    // Listen for session invalid
    const unsubSession = bridge.on("session:invalid", (event) => {
      if (event.hubId !== hubId) return
      this.#setError("session_invalid", event.message)
    })
    this.#unsubscribers.push(unsubSession)
  }

  /**
   * Set up listeners for subscription-specific events.
   * These are cleared on unsubscribe().
   */
  #setupSubscriptionEventListeners() {
    // Listen for subscription messages
    const unsubMsg = bridge.onSubscriptionMessage(this.subscriptionId, (message) => {
      this.handleMessage(message)
    })
    this.#subscriptionUnsubscribers.push(unsubMsg)

    // Listen for subscription confirmed
    const unsubConfirmed = bridge.on("subscription:confirmed", (event) => {
      if (event.subscriptionId !== this.subscriptionId) return
      // Subclasses may use this
    })
    this.#subscriptionUnsubscribers.push(unsubConfirmed)

    // Listen for subscription rejected
    const unsubRejected = bridge.on("subscription:rejected", (event) => {
      if (event.subscriptionId !== this.subscriptionId) return
      this.#setError("subscription_rejected", event.reason || "Subscription rejected")
    })
    this.#subscriptionUnsubscribers.push(unsubRejected)
  }

  #clearSubscriptionEventListeners() {
    for (const unsub of this.#subscriptionUnsubscribers) {
      unsub()
    }
    this.#subscriptionUnsubscribers = []
  }

  /**
   * Destroy the connection. Called by ConnectionManager.destroy().
   * Unsubscribes from channel, disconnects hub, cleans up everything.
   * NOTE: Cleanup is done asynchronously to avoid blocking other operations.
   */
  destroy() {
    // Clear state immediately to prevent any new operations
    const oldSubscriptionId = this.subscriptionId
    const hubId = this.getHubId()
    const wasHubConnected = this.#hubConnected

    this.subscriptionId = null
    this.#hubConnected = false
    this.identityKey = null
    this.session = null

    // Cleanup hub event listeners
    for (const unsub of this.#unsubscribers) {
      unsub()
    }
    this.#unsubscribers = []
    this.#clearSubscriptionEventListeners()

    this.#setState(ConnectionState.DISCONNECTED)
    this.emit("destroyed")
    this.subscribers.clear()

    // Async cleanup - fire and forget to avoid blocking
    // The worker will clean up orphaned subscriptions
    if (oldSubscriptionId) {
      bridge.send("unsubscribe", { subscriptionId: oldSubscriptionId }).catch(() => {})
      bridge.clearSubscriptionListeners(oldSubscriptionId)
    }

    if (hubId && wasHubConnected) {
      bridge.send("disconnect", { hubId }).catch(() => {})
    }
  }

  /**
   * Release this connection (decrement ref count).
   * Called by controllers in their disconnect().
   */
  release() {
    this.manager.release(this.key)
  }

  /**
   * Notify worker that this connection is idle (refCount hit 0).
   * Worker will start grace period and close if not reacquired.
   * Called by ConnectionManager.release() when refCount becomes 0.
   */
  notifyIdle() {
    const hubId = this.getHubId()
    if (hubId && this.#hubConnected) {
      // Tell worker to start grace period for this hub connection
      // Fire and forget - worker handles the timing
      bridge.send("disconnect", { hubId }).catch(() => {})
    }
  }

  /**
   * Notify worker that this connection is being reacquired.
   * Cancels any pending grace period in the worker.
   * Called by ConnectionManager.acquire() when reusing a wrapper.
   */
  async reacquire() {
    const hubId = this.getHubId()
    if (!hubId) return

    // Tell worker we're reacquiring - this cancels any pending close timer
    // and reconnects if the connection was closed during grace period
    const cableUrl = document.querySelector('meta[name="action-cable-url"]')?.content || "/cable"
    const actionCableModuleUrl = document.querySelector('meta[name="actioncable-module-url"]')?.content

    const result = await bridge.send("connect", {
      hubId,
      cableUrl,
      actionCableModuleUrl
    })

    // If worker had to create a new connection (old one was closed),
    // we need to resubscribe
    if (!this.#hubConnected || !result.sessionExists) {
      // Connection was lost, need to reinitialize
      this.#hubConnected = result.sessionExists
      if (this.subscriptionId) {
        // Our subscription is gone, clear it so subscribe() creates a new one
        this.subscriptionId = null
      }
    }
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
   * Whether this channel requires CLI ready signal before sending.
   * Override in subclasses that need to wait for CLI subscription.
   * Default false - most channels can send immediately.
   * @returns {boolean}
   */
  requiresCliReady() {
    return false
  }

  /**
   * Handle a decrypted message. Subclasses route to domain-specific events.
   * Base class handles input_ready; subclasses handle domain-specific messages.
   * @param {Object} message
   */
  handleMessage(message) {
    // Handle CLI ready signal first
    if (this.processMessage(message)) {
      return
    }
    // Default: emit as generic message
    this.emit("message", message)
  }

  // ========== Public API ==========

  /**
   * Send a message through the secure channel.
   * Buffers messages until CLI signals ready to prevent race conditions.
   * Auto-resubscribes if subscription is stale (e.g., after wake from sleep).
   * @param {string} type - Message type
   * @param {Object} data - Message payload
   * @returns {Promise<boolean>}
   */
  async send(type, data = {}) {
    if (!this.subscriptionId) {
      return false
    }

    // Buffer if CLI not ready yet (prevents race condition on channel startup)
    if (!this.cliReady) {
      this.inputBuffer.push({ type, data })
      return true
    }

    try {
      await bridge.send("send", {
        subscriptionId: this.subscriptionId,
        message: { type, ...data }
      })
      return true
    } catch (error) {
      // Stale subscription (e.g., SharedWorker restarted during sleep)
      // Resubscribe and retry once
      if (error.message?.includes("not found") && !this.#resubscribing) {
        console.log(`[${this.constructor.name}] Subscription stale, resubscribing...`)
        this.#resubscribing = true

        try {
          const oldSubId = this.subscriptionId
          this.subscriptionId = null
          this.#clearSubscriptionEventListeners()
          bridge.clearSubscriptionListeners(oldSubId)

          await this.subscribe()

          // Retry the send
          await bridge.send("send", {
            subscriptionId: this.subscriptionId,
            message: { type, ...data }
          })
          return true
        } catch (retryError) {
          console.error(`[${this.constructor.name}] Resubscribe/retry failed:`, retryError)
          return false
        } finally {
          this.#resubscribing = false
        }
      }

      console.error(`[${this.constructor.name}] Send failed:`, error)
      return false
    }
  }

  /**
   * Handle CLI ready signal - flush buffered messages.
   * Called when CLI sends input_ready after subscribing.
   */
  #handleCliReady() {
    if (this.cliReady) return // Already ready

    this.cliReady = true
    console.log(`[${this.constructor.name}] CLI ready, flushing ${this.inputBuffer.length} buffered messages`)

    // Flush buffered messages
    for (const { type, data } of this.inputBuffer) {
      bridge.send("send", {
        subscriptionId: this.subscriptionId,
        message: { type, ...data }
      }).catch(err => console.error(`[${this.constructor.name}] Flush failed:`, err))
    }
    this.inputBuffer = []

    this.emit("cliReady")
  }

  /**
   * Process incoming message, handling CLI ready signal before subclass routing.
   * Subclasses should call super.processMessage(message) or handle input_ready themselves.
   * @param {Object} message - Decrypted message
   * @returns {boolean} - True if message was handled (input_ready), false otherwise
   */
  processMessage(message) {
    if (message.type === "input_ready") {
      this.#handleCliReady()
      return true
    }
    return false
  }

  /**
   * Check if connected (hub connected AND subscribed to channel).
   * @returns {boolean}
   */
  isConnected() {
    return this.state === ConnectionState.CONNECTED
  }

  /**
   * Check if hub is connected (WebSocket alive, can subscribe).
   * @returns {boolean}
   */
  isHubConnected() {
    return this.#hubConnected
  }

  /**
   * Check if subscribed to channel.
   * @returns {boolean}
   */
  isSubscribed() {
    return this.subscriptionId !== null
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
