/**
 * Connection - Base class for typed connection wrappers.
 *
 * Provides common functionality:
 *   - WorkerBridge communication for encrypted channels
 *   - Olm session lifecycle (via SharedWorker)
 *   - Event subscription (typed subclasses add domain-specific events)
 *   - State tracking
 *
 * Lifecycle:
 *   - initialize() establishes hub connection (WebRTC + Olm session)
 *   - subscribe() creates virtual channel subscription (CLI routing)
 *   - unsubscribe() removes channel subscription (keeps peer connection alive)
 *   - destroy() tears down everything
 *
 * Subclasses implement:
 *   - channelName() - Virtual channel name for CLI routing (e.g., "TerminalRelayChannel")
 *   - channelParams() - Subscription params
 *   - handleMessage(msg) - Domain-specific message routing
 */

import bridge from "workers/bridge"
import { ensureMatrixReady, parseBundleFromFragment } from "matrix/bundle"

// Connection state (combines browser subscription + CLI handshake status)
export const ConnectionState = {
  DISCONNECTED: "disconnected",
  LOADING: "loading",
  CONNECTING: "connecting",
  CONNECTED: "connected",
  CLI_DISCONNECTED: "cli_disconnected",
  ERROR: "error",
}

// Browser connection status (from this tab's perspective)
export const BrowserStatus = {
  DISCONNECTED: "disconnected",
  CONNECTING: "connecting",
  SUBSCRIBING: "subscribing",
  SUBSCRIBED: "subscribed",
  ERROR: "error",
}

// CLI connection status (reported by Rails via health messages)
export const CliStatus = {
  UNKNOWN: "unknown",           // Initial state, waiting for health message
  OFFLINE: "offline",           // CLI not connected to Rails at all
  ONLINE: "online",             // CLI connected to Rails, but not yet on this E2E channel
  NOTIFIED: "notified",         // Bot::Message sent to tell CLI about browser
  CONNECTING: "connecting",     // CLI connecting to this channel
  CONNECTED: "connected",       // CLI connected to this channel, ready for handshake
  DISCONNECTED: "disconnected", // CLI was connected but disconnected
}

// Handshake timeout in milliseconds
const HANDSHAKE_TIMEOUT_MS = 8000

// Tab-unique identifier (generated once per page load).
// Used to distinguish multiple browser tabs sharing the same Olm session.
const TAB_ID = crypto.randomUUID()

export class Connection {
  // Static tab identifier shared by all connections in this tab
  static tabId = TAB_ID
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
    this.identityKey = null         // E2E identity key (shared across tabs)
    this.browserIdentity = null     // Tab-unique identity for routing (identityKey:tabId)
    this.state = ConnectionState.DISCONNECTED
    this.errorCode = null
    this.errorReason = null

    // Two-sided status tracking
    this.browserStatus = BrowserStatus.DISCONNECTED
    this.cliStatus = CliStatus.UNKNOWN

    // Event subscribers: Map<eventName, Set<callback>>
    this.subscribers = new Map()

    // Handshake state - tracks E2E connection confirmation.
    // With WebRTC, subscription confirmation is sufficient for readiness.
    // Handshake is kept for E2E encryption verification and UI status.
    this.handshakeComplete = false
    this.handshakeSent = false
    this.handshakeTimer = null
  }

  // ========== Lifecycle (called by ConnectionManager) ==========

  /**
   * Initialize the connection. Called by ConnectionManager.acquire().
   * Establishes hub connection (WebRTC + Olm session) and subscribes.
   */
  async initialize() {
    try {
      this.#setState(ConnectionState.LOADING)

      // Ensure crypto worker is initialized
      const cryptoWorkerUrl = document.querySelector('meta[name="crypto-worker-url"]')?.content
      const wasmJsUrl = document.querySelector('meta[name="crypto-wasm-js-url"]')?.content

      await ensureMatrixReady(cryptoWorkerUrl, wasmJsUrl)

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
   * Connect to the hub (WebRTC + Olm session).
   * Called by initialize() or can be used to reconnect.
   */
  async #connectHub() {
    if (this.#hubConnected) return

    this.#setBrowserStatus(BrowserStatus.CONNECTING)
    this.#setState(ConnectionState.CONNECTING)

    const hubId = this.getHubId()

    // 1. Handle Olm session via crypto worker
    // Parse session bundle from fragment if requested
    let sessionBundle = this.options.sessionBundle || null
    if (!sessionBundle && this.options.fromFragment) {
      sessionBundle = parseBundleFromFragment()
      if (sessionBundle) {
        // Strip the fragment so the bundle isn't reprocessed on reload
        history.replaceState(null, "", location.pathname + location.search)
      }
    }

    // Create or load session via crypto worker
    if (sessionBundle) {
      // Always create fresh session from new QR bundle.
      // createSession() deletes any stale session before establishing the new one.
      await bridge.createSession(hubId, sessionBundle)
    } else {
      // Try to load existing session
      const loadResult = await bridge.loadSession(hubId)
      if (!loadResult.loaded) {
        this.#setError("no_session", "No session available. Scan QR code to pair.")
        return
      }
    }

    // Get identity key for channel params
    const keyResult = await bridge.getIdentityKey(hubId)
    this.identityKey = keyResult.identityKey
    // Generate tab-unique browser identity for routing.
    // Multiple tabs share the same Olm session but need separate WebRTC connections.
    this.browserIdentity = `${this.identityKey}:${Connection.tabId}`

    // Set up hub-level event listeners BEFORE connecting transport
    // so we catch the initial health transmit from HubSignalingChannel
    this.#setupHubEventListeners()

    // 2. Connect transport via WebRTC (also subscribes to ActionCable signaling)
    const signalingUrl = window.location.origin
    await bridge.send("connect", {
      hubId,
      signalingUrl,
      browserIdentity: this.browserIdentity
    })

    this.#hubConnected = true
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

    // If already subscribed and not forcing refresh, ensure status is correct and return
    if (this.subscriptionId && !force) {
      this.#ensureSubscribedStatus()
      return
    }

    // Wait for any in-progress subscribe/unsubscribe
    while (this.#subscribing) {
      await new Promise(resolve => setTimeout(resolve, 10))
    }

    // Re-check after waiting - another caller might have subscribed
    if (this.subscriptionId && !force) {
      this.#ensureSubscribedStatus()
      return
    }

    this.#subscribing = true
    this.#setBrowserStatus(BrowserStatus.SUBSCRIBING)

    try {
      // Unsubscribe first if forcing refresh
      if (this.subscriptionId && force) {
        await this.#doUnsubscribe()
      }

      // Reset handshake state for fresh connection
      this.handshakeComplete = false
      this.handshakeSent = false
      if (this.handshakeTimer) {
        clearTimeout(this.handshakeTimer)
        this.handshakeTimer = null
      }
      this.cliStatus = CliStatus.UNKNOWN

      const hubId = this.getHubId()

      // Compute semantic subscription ID from channel + params
      // This allows both sides to derive the same ID independently
      const subscriptionId = this.computeSubscriptionId()

      const subscribeResult = await bridge.send("subscribe", {
        hubId,
        channel: this.channelName(),
        params: this.channelParams(),
        subscriptionId,
      })

      this.subscriptionId = subscriptionId
      this.#setupSubscriptionEventListeners()

      // WebRTC: DataChannel open = ready, complete handshake FIRST
      // so input isn't buffered when listeners fire
      this.#completeHandshake()

      this.#setBrowserStatus(BrowserStatus.SUBSCRIBED)
      this.#setState(ConnectionState.CONNECTED)
      this.emit("subscribed", this)
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
    this.#setBrowserStatus(BrowserStatus.CONNECTING)
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
        // Preserve session_invalid error state — user must re-pair, not auto-reconnect
        if (this.state === ConnectionState.ERROR && this.errorCode === "session_invalid") {
          return
        }
        this.#setBrowserStatus(BrowserStatus.DISCONNECTED)
        this.#setState(ConnectionState.DISCONNECTED)
        this.emit("disconnected")
      } else if (event.state === "connected" && !this.#hubConnected) {
        this.#hubConnected = true
        this.emit("reconnected")

        // Auto-resubscribe after reconnection - old subscription is stale
        if (this.subscriptionId) {
          console.debug(`[${this.constructor.name}] Auto-resubscribing after reconnect`)
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

    // Listen for health events from ActionCable signaling channel
    // Health messages arrive via HubSignalingChannel → WebRTCTransport → bridge
    const unsubHealth = bridge.on("health", (event) => {
      if (event.hubId !== hubId) return
      this.#handleHealthMessage(event)
    })
    this.#unsubscribers.push(unsubHealth)

    // Listen for session invalid (Olm session desync detected by CLI)
    const unsubSession = bridge.on("session:invalid", (event) => {
      if (event.hubId !== hubId) return
      console.warn(`[${this.constructor.name}] Session invalid:`, event.message)
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
    // Transport layer handles decryption - we receive plaintext here
    const unsubMsg = bridge.onSubscriptionMessage(this.subscriptionId, async (message) => {
      // Raw binary data (Uint8Array) from PTY output
      if (message instanceof Uint8Array) {
        this.handleMessage({ type: "raw_output", data: message })
        return
      }

      // Decrypted message from transport
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
    this.browserIdentity = null

    // Cleanup hub event listeners
    for (const unsub of this.#unsubscribers) {
      unsub()
    }
    this.#unsubscribers = []
    this.#clearSubscriptionEventListeners()

    this.browserStatus = BrowserStatus.DISCONNECTED
    this.cliStatus = CliStatus.UNKNOWN
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
   * Notify transport that this connection is idle (refCount hit 0).
   * Starts a grace period - connection closes after ~3s if not reacquired.
   * Called by ConnectionManager.release() when refCount becomes 0.
   */
  notifyIdle() {
    const hubId = this.getHubId()
    if (hubId && this.#hubConnected) {
      // Tell transport to start grace period for this hub connection.
      // If reacquired before grace period expires, connection is reused.
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

    // Check if session exists via crypto worker
    const { hasSession } = await bridge.hasSession(hubId)

    // Tell transport worker we're reacquiring - this cancels any pending close timer
    // and reconnects if the connection was closed during grace period
    const signalingUrl = window.location.origin
    await bridge.send("connect", {
      hubId,
      signalingUrl,
      browserIdentity: this.browserIdentity
    })

    // If session is gone or we weren't connected, need to reinitialize
    if (!this.#hubConnected || !hasSession) {
      this.#hubConnected = hasSession
      if (this.subscriptionId) {
        // Our subscription is gone, clear it so subscribe() creates a new one
        this.subscriptionId = null
      }
    }
  }

  // ========== Abstract methods (override in subclasses) ==========

  /**
   * Virtual channel name for CLI routing (e.g., "TerminalRelayChannel", "HubChannel").
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
   * Compute semantic subscription ID from channel + params.
   * Override in subclasses for domain-specific IDs.
   * Default: channel name (works for singleton subscriptions like hub).
   * @returns {string}
   */
  computeSubscriptionId() {
    return this.channelName()
  }

  /**
   * Extract hubId from options. Override if hubId comes from elsewhere.
   * @returns {string}
   */
  getHubId() {
    return this.options.hubId
  }

  /**
   * Handle a decrypted message. Subclasses route to domain-specific events.
   * Base class handles handshake protocol; subclasses handle domain-specific messages.
   * @param {Object} message
   */
  handleMessage(message) {
    // Handle handshake/health messages first
    if (this.processMessage(message)) {
      return
    }
    // Default: emit as generic message
    this.emit("message", message)
  }

  // ========== Public API ==========

  /**
   * Send an encrypted message through the transport worker.
   * Encrypts via crypto worker, then sends envelope directly.
   * @private
   */
  async #sendEncrypted(message) {
    const hubId = this.getHubId()
    // Include subscriptionId in the encrypted payload for CLI routing
    const fullMessage = { subscriptionId: this.subscriptionId, ...message }

    const t0 = performance.now()
    const { envelope } = await bridge.encrypt(hubId, fullMessage)
    const t1 = performance.now()

    // Crypto worker may return envelope as JSON string - ensure it's an object
    const envelopeObj = typeof envelope === "string" ? JSON.parse(envelope) : envelope
    // Send envelope directly (CLI decrypts and finds subscriptionId inside)
    await bridge.send("sendEnvelope", { hubId, envelope: envelopeObj })
    const t2 = performance.now()

    const encryptTime = t1 - t0
    const sendTime = t2 - t1
    if (encryptTime > 20 || sendTime > 20) {
      console.debug(`[${this.constructor.name}] send timing: encrypt=${encryptTime.toFixed(1)}ms, send=${sendTime.toFixed(1)}ms`)
    }
  }

  /**
   * Send a message through the secure channel.
   * Auto-resubscribes if subscription is stale (e.g., after wake from sleep).
   * @param {string} type - Message type
   * @param {Object} data - Message payload
   * @returns {Promise<boolean>}
   */
  async send(type, data = {}) {
    if (!this.subscriptionId) {
      return false
    }

    try {
      await this.#sendEncrypted({ type, ...data })
      return true
    } catch (error) {
      // Stale subscription (e.g., SharedWorker restarted during sleep)
      // Resubscribe and retry once
      if (error.message?.includes("not found") && !this.#resubscribing) {
        console.debug(`[${this.constructor.name}] Subscription stale, resubscribing`)
        this.#resubscribing = true

        try {
          const oldSubId = this.subscriptionId
          this.subscriptionId = null
          this.#clearSubscriptionEventListeners()
          bridge.clearSubscriptionListeners(oldSubId)

          await this.subscribe()

          // Retry the send
          await this.#sendEncrypted({ type, ...data })
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
   * Process incoming message, handling health/status/handshake messages before subclass routing.
   * Subclasses should call super.processMessage(message) or handle these themselves.
   * @param {Object} message - Decrypted message
   * @returns {boolean} - True if message was handled, false otherwise
   */
  processMessage(message) {
    if (message.type === "health") {
      console.debug(`[${this.constructor.name}] Received health message:`, message)
      this.#handleHealthMessage(message)
      return true
    }
    if (message.type === "connected") {
      // CLI sent handshake - respond with ack
      console.debug(`[${this.constructor.name}] Received handshake from CLI:`, message.device_name)
      this.#handleIncomingHandshake(message)
      return true
    }
    if (message.type === "ack") {
      // CLI acknowledged our handshake
      this.#handleHandshakeAck(message)
      return true
    }
    if (message.type === "cli_disconnected") {
      this.#handleCliDisconnected()
      return true
    }
    return false
  }

  // ========== Handshake Protocol ==========

  /**
   * Send handshake to CLI indicating browser is ready.
   * Called when browser detects CLI is connected (browser is "last").
   */
  #sendHandshake() {
    if (this.handshakeSent || this.handshakeComplete) {
      return
    }

    this.handshakeSent = true
    console.debug(`[${this.constructor.name}] Sending handshake`)

    // Send handshake directly (bypasses buffer since handshake isn't complete yet)
    this.#sendEncrypted({
      type: "connected",
      device_name: this.#getDeviceName(),
      timestamp: Date.now()
    }).catch(err => {
      console.error(`[${this.constructor.name}] Handshake send failed:`, err)
      this.handshakeSent = false
    })

    // Start timeout
    this.handshakeTimer = setTimeout(() => {
      if (!this.handshakeComplete) {
        console.warn(`[${this.constructor.name}] Handshake timeout`)
        this.emit("error", {
          reason: "handshake_timeout",
          message: "CLI did not respond to handshake"
        })
      }
    }, HANDSHAKE_TIMEOUT_MS)
  }

  /**
   * Handle incoming handshake from CLI.
   * CLI was "last" to connect, respond with ack.
   */
  #handleIncomingHandshake(message) {

    // Send ack back
    this.#sendEncrypted({ type: "ack", timestamp: Date.now() })
      .catch(err => console.error(`[${this.constructor.name}] Ack send failed:`, err))

    // Mark complete and flush buffer
    this.#completeHandshake()
  }

  /**
   * Handle handshake acknowledgment from CLI.
   * CLI confirmed our handshake.
   */
  #handleHandshakeAck(message) {

    if (this.handshakeTimer) {
      clearTimeout(this.handshakeTimer)
      this.handshakeTimer = null
    }

    this.#completeHandshake()
  }

  /**
   * Complete handshake - mark E2E as verified and emit connected event.
   */
  #completeHandshake() {
    if (this.handshakeComplete) return

    this.handshakeComplete = true
    console.debug(`[${this.constructor.name}] Handshake complete`)

    // Update state to CONNECTED now that E2E is established
    this.#setState(ConnectionState.CONNECTED)
    this.#emitHealthChange()

    this.emit("connected", this)
  }

  /**
   * Get device name for handshake.
   */
  #getDeviceName() {
    const ua = navigator.userAgent
    if (ua.includes("iPhone")) return "iPhone"
    if (ua.includes("iPad")) return "iPad"
    if (ua.includes("Android")) return "Android"
    if (ua.includes("Mac")) return "Mac Browser"
    if (ua.includes("Windows")) return "Windows Browser"
    if (ua.includes("Linux")) return "Linux Browser"
    return "Browser"
  }

  /**
   * Handle health message from Rails - updates CLI status.
   * Hub-wide: { type: "health", cli: "online" | "offline" } - CLI connected to Rails
   * Per-browser: { type: "health", cli: "connected" | "disconnected" } - CLI on E2E channel
   */
  #handleHealthMessage(message) {
    const cliStatusMap = {
      offline: CliStatus.OFFLINE,
      online: CliStatus.ONLINE,
      notified: CliStatus.NOTIFIED,
      connecting: CliStatus.CONNECTING,
      connected: CliStatus.CONNECTED,
      disconnected: CliStatus.DISCONNECTED,
    }

    const newCliStatus = cliStatusMap[message.cli] || this.cliStatus
    if (newCliStatus !== this.cliStatus) {
      const prevStatus = this.cliStatus
      this.cliStatus = newCliStatus
      console.debug(`[${this.constructor.name}] CLI status: ${prevStatus} → ${newCliStatus}`)

      this.emit("cliStatusChange", { status: newCliStatus, prevStatus })
      this.#emitHealthChange()

      // If CLI just connected to E2E channel, initiate handshake (browser is "last")
      if (newCliStatus === CliStatus.CONNECTED && prevStatus !== CliStatus.CONNECTED) {
        this.emit("cliConnected")
        this.#sendHandshake()
      }

      // If CLI just disconnected, reset handshake state for fresh start on reconnect
      if ((newCliStatus === CliStatus.DISCONNECTED || newCliStatus === CliStatus.OFFLINE) &&
          prevStatus !== CliStatus.DISCONNECTED && prevStatus !== CliStatus.OFFLINE) {
        this.handshakeComplete = false
        this.handshakeSent = false
        if (this.handshakeTimer) {
          clearTimeout(this.handshakeTimer)
          this.handshakeTimer = null
        }
        this.emit("cliDisconnected")
      }
    }

    // Also update legacy state for backward compatibility
    if (message.cli === "disconnected" || message.cli === "offline") {
      this.#setState(ConnectionState.CLI_DISCONNECTED)
    }
  }

  /**
   * Handle CLI disconnection - server notifies us when CLI unsubscribes.
   */
  #handleCliDisconnected() {
    // Reset handshake state
    this.handshakeComplete = false
    this.handshakeSent = false
    if (this.handshakeTimer) {
      clearTimeout(this.handshakeTimer)
      this.handshakeTimer = null
    }
    this.cliStatus = CliStatus.DISCONNECTED
    this.#setState(ConnectionState.CLI_DISCONNECTED)
    this.emit("cliStatusChange", { status: CliStatus.DISCONNECTED, prevStatus: this.cliStatus })
    this.#emitHealthChange()
    this.emit("cliDisconnected")
  }

  /**
   * Emit combined health change event with both browser and CLI status.
   * Also includes current state so passive subscribers can react to it.
   */
  #emitHealthChange() {
    this.emit("healthChange", {
      browser: this.browserStatus,
      cli: this.cliStatus,
    })
    this.manager.notifySubscribers(this.key, {
      type: "health",
      state: this.state,
      browser: this.browserStatus,
      cli: this.cliStatus,
    })
  }

  /**
   * Check if fully connected (subscribed AND handshake complete).
   * @returns {boolean}
   */
  isConnected() {
    return this.state === ConnectionState.CONNECTED && this.handshakeComplete
  }

  /**
   * Check if hub is connected (WebRTC DataChannel open, can subscribe).
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
      this.errorCode = null
      this.errorReason = null
    }

    const stateInfo = { state: newState, prevState, error: this.errorReason }
    this.emit("stateChange", stateInfo)

    // Notify ConnectionManager subscribers (passive observers)
    this.manager.notifySubscribers(this.key, stateInfo)
  }

  #setError(reason, message) {
    this.errorCode = reason
    this.errorReason = message
    this.#setBrowserStatus(BrowserStatus.ERROR)
    this.#setState(ConnectionState.ERROR)
    this.emit("error", { reason, message })
  }

  #setBrowserStatus(newStatus) {
    const prevStatus = this.browserStatus
    if (newStatus === prevStatus) return

    this.browserStatus = newStatus
    console.debug(`[${this.constructor.name}] Browser status: ${prevStatus} → ${newStatus}`)

    this.emit("browserStatusChange", { status: newStatus, prevStatus })
    this.#emitHealthChange()
  }

  /**
   * Ensure browser status reflects subscribed state.
   * Called on early return from subscribe() when already subscribed.
   */
  #ensureSubscribedStatus() {
    if (this.browserStatus !== BrowserStatus.SUBSCRIBED) {
      this.#setBrowserStatus(BrowserStatus.SUBSCRIBED)
    }
    // Always emit health change so new listeners get current state
    this.#emitHealthChange()
  }
}
