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
 *   - initialize()/reacquire() bootstrap signaling (ActionCable + Olm)
 *   - #ensureConnected() is the idempotent entry point for peer + subscribe (not signaling)
 *   - Health events call #ensureConnected(); offline calls #disconnectPeer()
 *   - destroy() tears down everything (signaling + peer)
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
  NOTIFIED: "notified",         // HubCommand sent to tell CLI about browser
  CONNECTING: "connecting",     // CLI connecting to this channel
  CONNECTED: "connected",       // CLI connected to this channel, ready for handshake
  DISCONNECTED: "disconnected", // CLI was connected but disconnected
}

// Connection mode (P2P vs relayed through TURN)
export const ConnectionMode = {
  UNKNOWN: "unknown",
  DIRECT: "direct",    // P2P connection (host, srflx, prflx)
  RELAYED: "relayed",  // Relayed through TURN server
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
  #subscribeLock = null     // Promise-based lock (resolves when subscribe/unsubscribe finishes)
  #subscribeLockResolve = null
  #resubscribing = false    // Lock to prevent concurrent resubscribe on stale send
  #visibilityHandler = null // Cleanup ref for visibilitychange listener
  #initRetryCount = 0       // Retry counter for failed initialize()
  #initRetryTimer = null    // Pending retry timer
  #peerReconnectTimer = null  // Pending peer reconnect timer
  #peerReconnectAttempts = 0  // Retry counter for peer reconnection

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
    this.connectionMode = ConnectionMode.UNKNOWN

    // Event subscribers: Map<eventName, Set<callback>>
    this.subscribers = new Map()

    // Handshake state - tracks E2E connection confirmation.
    // With WebRTC, subscription confirmation is sufficient for readiness.
    // Handshake is kept for E2E encryption verification and UI status.
    this.handshakeComplete = false
    this.handshakeSent = false
    this.handshakeTimer = null

    // Resume connections when tab becomes visible after backgrounding.
    // The transport layer probes stale peers; this picks up the pieces
    // by re-running the connect flow (peer + subscribe).
    this.#visibilityHandler = () => {
      if (document.visibilityState !== "visible") return
      if (!this.#hubConnected || !this.identityKey) return
      if (this.state === ConnectionState.ERROR && (this.errorCode === "session_invalid" || this.errorCode === "unpaired")) return
      this.#ensureConnected().catch(() => {})
    }
    document.addEventListener("visibilitychange", this.#visibilityHandler)
  }

  // ========== Lifecycle (called by ConnectionManager) ==========

  /**
   * Initialize the connection. Called by ConnectionManager.acquire().
   * Sets up crypto + signaling; #connectSignaling() calls #ensureConnected() at the end.
   */
  async initialize() {
    try {
      this.#setState(ConnectionState.LOADING)

      // Parse bundle from URL fragment BEFORE async worker init.
      // The hash can get stripped during the ensureMatrixReady() async gap
      // (SharedWorker + WASM loading), so read it synchronously now.
      if (!this.options.sessionBundle) {
        const bundle = parseBundleFromFragment()
        if (bundle) {
          this.options.sessionBundle = bundle
          history.replaceState(null, "", location.pathname + location.search)
        }
      }

      const cryptoWorkerUrl = document.querySelector('meta[name="crypto-worker-url"]')?.content
      const wasmJsUrl = document.querySelector('meta[name="crypto-wasm-js-url"]')?.content
      const wasmBinaryUrl = document.querySelector('meta[name="crypto-wasm-binary-url"]')?.content
      await ensureMatrixReady(cryptoWorkerUrl, wasmJsUrl, wasmBinaryUrl)

      await this.#connectSignaling()
    } catch (error) {
      console.error(`[${this.constructor.name}] Initialize failed:`, error)
      // Don't overwrite session_invalid — it's already showing "Scan Code"
      if (this.errorCode !== "session_invalid") {
        // No crypto session means user needs to scan QR code, not a generic init error.
        // Use lightweight errorCode (not #setError) to keep browserStatus/state intact —
        // browser signaling may still be functional, only crypto is missing.
        if (!this.identityKey) {
          this.errorCode = "unpaired"
          this.errorReason = "Scan connection code"
          this.emit("error", { reason: "unpaired", message: "Scan connection code" })
        } else {
          // Retry transient failures (WASM timeout, ActionCable timeout) with backoff.
          // Non-retryable errors (unpaired, session_invalid) are handled above.
          this.#scheduleInitRetry(error)
        }
      }
    }
  }

  /**
   * Schedule a retry of initialize() with exponential backoff.
   * Retries up to 3 times: 2s, 4s, 8s.
   */
  #scheduleInitRetry(error) {
    const MAX_RETRIES = 3
    if (this.#initRetryCount >= MAX_RETRIES) {
      console.error(`[${this.constructor.name}] Init failed after ${MAX_RETRIES} retries`)
      this.#setError("init_failed", error.message)
      return
    }

    this.#initRetryCount++
    const delay = 2000 * Math.pow(2, this.#initRetryCount - 1) // 2s, 4s, 8s
    console.debug(`[${this.constructor.name}] Retrying init in ${delay}ms (attempt ${this.#initRetryCount}/${MAX_RETRIES})`)

    this.#initRetryTimer = setTimeout(() => {
      this.#initRetryTimer = null
      this.initialize()
    }, delay)
  }

  /**
   * Connect ActionCable signaling (WebSocket + Olm session).
   * Fast path: if a sibling Connection already has signaling for this hub,
   * inherit hub state and skip full setup. Otherwise, full signaling flow.
   */
  async #connectSignaling() {
    if (this.#hubConnected) return

    // Fast path: sibling Connection already has signaling for this hub.
    // Inherit hub state, set up listeners, proceed to peer + subscribe.
    // No BrowserStatus.CONNECTING → no status flicker during Turbo navigation.
    const sibling = this.manager.findHubConnection(this.getHubId())
    if (sibling && sibling !== this) {
      this.identityKey = sibling.identityKey
      this.browserIdentity = sibling.browserIdentity
      this.cliStatus = sibling.cliStatus
      this.connectionMode = sibling.connectionMode

      this.#setupHubEventListeners()

      // Ping transport to cancel any grace period
      await bridge.send("connectSignaling", {
        hubId: this.getHubId(),
        browserIdentity: this.browserIdentity,
      })

      this.#hubConnected = true
      this.#setBrowserStatus(BrowserStatus.SUBSCRIBED)

      await this.#ensureConnected()
      return
    }

    // Full signaling setup (first connection to this hub)
    this.#setBrowserStatus(BrowserStatus.CONNECTING)
    this.#setState(ConnectionState.CONNECTING)

    const hubId = this.getHubId()

    // 1. Handle Olm session (optional — not required for ActionCable health)
    const sessionBundle = this.options.sessionBundle || null
    let hasOlmSession = false

    if (sessionBundle) {
      await bridge.createSession(hubId, sessionBundle)
      hasOlmSession = true
    } else {
      const result = await bridge.hasSession(hubId)
      hasOlmSession = result.hasSession
      if (!hasOlmSession) {
        console.debug(`[${this.constructor.name}] No Olm session — WebRTC disabled until QR scan`)
      }
    }

    // Get identity key only when a session exists. An account (keypair) can exist
    // without a session due to cleanup race conditions — checking hasSession is
    // the authoritative test for whether we can encrypt.
    if (hasOlmSession) {
      try {
        const keyResult = await bridge.getIdentityKey(hubId)
        this.identityKey = keyResult.identityKey
      } catch {
        this.identityKey = null
      }
    } else {
      this.identityKey = null
    }

    // Browser identity: crypto key when available, anonymous for health-only
    this.browserIdentity = this.identityKey
      ? `${this.identityKey}:${Connection.tabId}`
      : `anon:${Connection.tabId}`

    // Set up hub-level event listeners BEFORE connecting transport
    // so we catch the initial health transmit from HubSignalingChannel
    this.#setupHubEventListeners()

    // 2. Connect ActionCable signaling (health + WebRTC signal relay)
    // Always connects — browser status tracks WebSocket, not crypto state.
    const result = await bridge.send("connectSignaling", {
      hubId,
      browserIdentity: this.browserIdentity
    })

    this.#hubConnected = true
    this.#setBrowserStatus(BrowserStatus.SUBSCRIBED)

    // Transport reports peer already connected (grace period cancelled, peer alive).
    // Seed cliStatus so #ensureConnected() can proceed without waiting for a health
    // event that won't re-fire (ActionCable channel wasn't re-subscribed).
    if (result?.state === "connected" && this.cliStatus === CliStatus.UNKNOWN) {
      this.cliStatus = CliStatus.ONLINE
    }

    await this.#ensureConnected()  // continues to peer+subscribe if CLI online + session exists

    // No crypto session — WebRTC unavailable, user must scan connection code.
    // Use lightweight errorCode (not #setError) to keep browserStatus SUBSCRIBED —
    // signaling IS connected, only crypto is missing.
    if (!this.identityKey) {
      this.errorCode = "unpaired"
      this.emit("error", { reason: "unpaired", message: "Scan connection code" })
    }
  }

  /**
   * Idempotent entry point for establishing peer + subscription.
   * Assumes signaling is already connected (or in progress).
   * Safe to call from any code path: health events, reacquire, send, connectSignaling.
   *
   * Does NOT bootstrap signaling — that's initialize()/reacquire()'s job.
   * If signaling isn't ready yet, this is a no-op; the in-progress
   * connectSignaling() will call us again when it completes.
   */
  async #ensureConnected() {
    if (this.state === ConnectionState.ERROR) return
    if (!this.#hubConnected) return  // signaling not ready, nothing to do yet
    if (!this.identityKey) return    // no crypto session, WebRTC unavailable

    // Step 1: Peer (only if CLI is reachable)
    const cliOnline = this.cliStatus === CliStatus.ONLINE ||
                      this.cliStatus === CliStatus.NOTIFIED ||
                      this.cliStatus === CliStatus.CONNECTING ||
                      this.cliStatus === CliStatus.CONNECTED
    if (cliOnline) {
      const hubId = this.getHubId()
      await bridge.send("connectPeer", { hubId })  // idempotent + deduped in transport
    }

    // Step 2: Subscribe virtual channel
    if (cliOnline && !this.subscriptionId) {
      try {
        await this.subscribe()  // has its own lock, early-returns if subscribed
      } catch (e) {
        // DataChannel/transport failures are retriable — the disconnected event
        // handler will schedule a peer reconnect. Don't propagate.
        if (e.message?.includes("DataChannel") || e.message?.includes("No connection") || e.message?.includes("timeout")) {
          console.debug(`[${this.constructor.name}] Subscribe deferred (peer not ready): ${e.message}`)
          return
        }
        throw e  // Re-throw non-transport errors (auth, crypto, etc.)
      }
    }
  }

  /**
   * Tear down WebRTC peer connection (hub went offline).
   * Keeps ActionCable signaling alive for health events.
   */
  async #disconnectPeer() {
    const hubId = this.getHubId()

    // Unsubscribe virtual channel first
    if (this.subscriptionId) {
      await this.unsubscribe()
    }

    // Close WebRTC peer connection (keeps signaling)
    bridge.send("disconnectPeer", { hubId }).catch(() => {})

    // Reset handshake state
    this.handshakeComplete = false
    this.handshakeSent = false
    if (this.handshakeTimer) {
      clearTimeout(this.handshakeTimer)
      this.handshakeTimer = null
    }

    // Cancel any pending peer reconnect so next online transition starts fresh
    if (this.#peerReconnectTimer) {
      clearTimeout(this.#peerReconnectTimer)
      this.#peerReconnectTimer = null
    }
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
    if (this.#subscribing) {
      await this.#subscribeLock
      // Re-check after waiting - another caller might have subscribed
      if (this.subscriptionId && !force) {
        this.#ensureSubscribedStatus()
        return
      }
    }

    this.#subscribing = true
    this.#subscribeLock = new Promise(resolve => { this.#subscribeLockResolve = resolve })

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
      // Don't reset cliStatus here — it's managed by health events.
      // Resetting would cause hub status to blip "offline" when
      // health-driven #ensureConnected() calls subscribe().

      const hubId = this.getHubId()

      // Compute semantic subscription ID from channel + params
      // This allows both sides to derive the same ID independently
      const subscriptionId = this.computeSubscriptionId()

      // Register listener BEFORE sending subscribe so scrollback chunks
      // that arrive immediately after CLI confirms aren't dropped.
      // Without this, the CLI can send snapshot data between the
      // "subscribed" confirmation and listener registration — a race
      // that causes missing scrollback on slow clients (phones).
      this.subscriptionId = subscriptionId
      this.#setupSubscriptionEventListeners()

      const subscribeResult = await bridge.send("subscribe", {
        hubId,
        channel: this.channelName(),
        params: this.channelParams(),
        subscriptionId,
      })

      // WebRTC: DataChannel open = ready, complete handshake FIRST
      // so input isn't buffered when listeners fire
      this.#completeHandshake()

      this.#setState(ConnectionState.CONNECTED)
      this.emit("subscribed", this)
    } catch (e) {
      // Subscribe failed — clean up the listener we registered eagerly
      this.#clearSubscriptionEventListeners()
      this.subscriptionId = null
      throw e
    } finally {
      this.#subscribing = false
      this.#subscribeLockResolve?.()
    }
  }

  /**
   * Unsubscribe from the channel. Keeps hub connection alive.
   * Call this when controller disconnects during navigation.
   */
  async unsubscribe() {
    // Wait for any in-progress subscribe to complete
    if (this.#subscribing) {
      await this.#subscribeLock
    }

    if (!this.subscriptionId) return

    this.#subscribing = true
    this.#subscribeLock = new Promise(resolve => { this.#subscribeLockResolve = resolve })
    try {
      await this.#doUnsubscribe()
    } finally {
      this.#subscribing = false
      this.#subscribeLockResolve?.()
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
    // Browser status stays green — WebSocket is still up
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

    // Listen for WebRTC peer connection state changes
    const unsubState = bridge.on("connection:state", (event) => {
      if (event.hubId !== hubId) return

      if (event.state === "disconnected") {
        // Preserve session_invalid error state — user must re-pair, not auto-reconnect
        if (this.state === ConnectionState.ERROR && this.errorCode === "session_invalid") {
          return
        }

        // Peer connection lost — retry with backoff if hub is still online.
        // Exponential backoff prevents hammering when offers are being dropped
        // (e.g., server just rebooted, async adapter not ready).
        // Phone unlock reconnects instantly via visibilitychange (separate path).
        this.emit("disconnected")

        if (this.cliStatus === CliStatus.ONLINE || this.cliStatus === CliStatus.NOTIFIED) {
          if (this.#peerReconnectTimer) return // already scheduled
          this.#peerReconnectAttempts++

          if (this.#peerReconnectAttempts > 5) {
            console.debug(`[${this.constructor.name}] Peer reconnect exhausted after ${this.#peerReconnectAttempts} attempts, waiting for health event`)
            return
          }

          const delay = Math.min(2000 * Math.pow(1.5, this.#peerReconnectAttempts - 1), 15000)
          this.#peerReconnectTimer = setTimeout(() => {
            this.#peerReconnectTimer = null
            if (!this.handshakeComplete) {
              console.debug(`[${this.constructor.name}] Peer lost but hub online, reconnecting peer (attempt ${this.#peerReconnectAttempts})...`)
              this.#ensureConnected().catch(() => {})
            }
          }, delay)
        }
      } else if (event.state === "connected") {
        if (this.#peerReconnectTimer) {
          clearTimeout(this.#peerReconnectTimer)
          this.#peerReconnectTimer = null
        }
        this.#peerReconnectAttempts = 0
        if (event.mode) {
          this.#setConnectionMode(event.mode)
        }
      }
    })
    this.#unsubscribers.push(unsubState)

    // Listen for connection mode changes (after ICE restart, path may change)
    const unsubMode = bridge.on("connection:mode", (event) => {
      if (event.hubId !== hubId) return
      this.#setConnectionMode(event.mode)
    })
    this.#unsubscribers.push(unsubMode)

    // Listen for health events from ActionCable signaling channel
    // Health messages arrive via HubSignalingChannel → WebRTCTransport → bridge
    const unsubHealth = bridge.on("health", (event) => {
      if (event.hubId !== hubId) return
      this.#handleHealthMessage(event)
    })
    this.#unsubscribers.push(unsubHealth)

    // Listen for session invalid (Olm session desync detected by CLI)
    // Don't use #setError — ActionCable is still connected, only crypto is bad
    const unsubSession = bridge.on("session:invalid", (event) => {
      if (event.hubId !== hubId) return
      if (this.errorCode === "session_invalid") return  // already handled
      console.warn(`[${this.constructor.name}] Session invalid:`, event.message)
      this.#disconnectPeer()
      // Clear stale Olm session so it can't interfere with fresh sessions (e.g., new tab)
      bridge.clearSession(hubId).catch(() => {})
      this.identityKey = null
      this.errorCode = "session_invalid"
      this.errorReason = event.message
      this.#setState(ConnectionState.ERROR)
      this.emit("error", { reason: "session_invalid", message: event.message })
    })
    this.#unsubscribers.push(unsubSession)

    // Listen for session refreshed (ratchet restart succeeded)
    const unsubRefresh = bridge.on("session:refreshed", async (event) => {
      if (event.hubId !== hubId) return
      console.debug(`[${this.constructor.name}] Session refreshed via ratchet restart`)

      // Clear any previous session_invalid error state
      if (this.errorCode === "session_invalid") {
        this.errorCode = null
        this.errorReason = null
      }

      // Tear down the dead WebRTC peer and reconnect with the fresh session.
      // The old peer's offer was encrypted with the stale Olm session and was
      // never answered, so we need a completely new peer connection + offer.
      await this.#disconnectPeer()
      await this.#ensureConnected()
    })
    this.#unsubscribers.push(unsubRefresh)
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
    // Cancel any pending init retry
    if (this.#initRetryTimer) {
      clearTimeout(this.#initRetryTimer)
      this.#initRetryTimer = null
    }

    // Cancel any pending peer reconnect
    if (this.#peerReconnectTimer) {
      clearTimeout(this.#peerReconnectTimer)
      this.#peerReconnectTimer = null
    }

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

    // Remove visibility listener
    if (this.#visibilityHandler) {
      document.removeEventListener("visibilitychange", this.#visibilityHandler)
      this.#visibilityHandler = null
    }

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

    // Check for new session bundle in URL fragment (QR code scan)
    const sessionBundle = parseBundleFromFragment()
    if (sessionBundle) {
      history.replaceState(null, "", location.pathname + location.search)
      await bridge.createSession(hubId, sessionBundle)
      // Update identity key now that session exists
      try {
        const keyResult = await bridge.getIdentityKey(hubId)
        this.identityKey = keyResult.identityKey
        this.browserIdentity = `${this.identityKey}:${Connection.tabId}`
      } catch { /* handled below via hasSession check */ }
      // Clear unpaired error
      this.errorCode = null
      this.errorReason = null
    }

    const { hasSession } = await bridge.hasSession(hubId)

    if (!hasSession) {
      this.#hubConnected = false
      this.subscriptionId = null
      this.identityKey = null
      this.#setError("unpaired", "Scan connection code")
      return
    }

    // Re-establish signaling (cancels any pending grace period).
    const result = await bridge.send("connectSignaling", {
      hubId,
      browserIdentity: this.browserIdentity
    })

    this.#hubConnected = true

    // Seed cliStatus from transport if no health event has updated it yet
    if (result?.state === "connected" && this.cliStatus === CliStatus.UNKNOWN) {
      this.cliStatus = CliStatus.ONLINE
    }

    // Always clear subscription on reacquire. Turbo navigation destroys the DOM
    // (terminal instance, etc.) so the CLI must re-send initial content via a
    // fresh subscription. #ensureConnected() will re-subscribe below.
    if (this.subscriptionId) {
      this.#clearSubscriptionEventListeners()
      bridge.clearSubscriptionListeners(this.subscriptionId)
      this.subscriptionId = null
      this.handshakeComplete = false
      this.handshakeSent = false
    }

    await this.#ensureConnected()  // re-subscribes → CLI sends fresh content
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
   * Send an Olm-encrypted message through the transport worker.
   * Encrypts via crypto worker, then sends as binary on DataChannel.
   * @private
   */
  async #sendEncrypted(message) {
    const hubId = this.getHubId()
    // Include subscriptionId for CLI routing.
    const fullMessage = { subscriptionId: this.subscriptionId, ...message }

    // Binary inner: [0x00][JSON bytes] (control message)
    const jsonBytes = new TextEncoder().encode(JSON.stringify(fullMessage))
    const plaintext = new Uint8Array(1 + jsonBytes.length)
    plaintext[0] = 0x00 // CONTENT_MSG
    plaintext.set(jsonBytes, 1)

    const t0 = performance.now()
    const { data: encrypted } = await bridge.encryptBinary(hubId, plaintext)
    const t1 = performance.now()

    // Send binary Olm frame directly (zero JSON, zero base64)
    await bridge.send("sendEncrypted", { hubId, encrypted })
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
    // Auto-heal: if not subscribed, try to connect
    if (!this.subscriptionId) {
      await this.#ensureConnected()
      if (!this.subscriptionId) return false  // still no luck
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
   * Send binary PTY data through the encrypted channel.
   * Bypasses JSON serialization for the keystroke hot path.
   * @param {string|Uint8Array} data - Raw PTY input data
   * @returns {Promise<boolean>}
   */
  async sendBinaryPty(data) {
    if (!this.subscriptionId) {
      await this.#ensureConnected()
      if (!this.subscriptionId) return false
    }

    try {
      const hubId = this.getHubId()
      await bridge.send("sendPtyInput", {
        hubId,
        subscriptionId: this.subscriptionId,
        data,
      })
      return true
    } catch (error) {
      console.error(`[${this.constructor.name}] sendBinaryPty failed:`, error)
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

    // Update CLI status to CONNECTED now that E2E handshake is done.
    // This is the definitive "CLI is talking to us" signal - health messages
    // via ActionCable may lag behind the actual WebRTC connection state.
    this.cliStatus = CliStatus.CONNECTED

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
    // Don't process health events when session is unpaired/invalid — user must re-pair
    if (this.errorCode === "unpaired" || this.errorCode === "session_invalid") return

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

      // CLI became reachable — ensure peer + subscribe via idempotent entry point
      const wasInactive = prevStatus === CliStatus.UNKNOWN || prevStatus === CliStatus.OFFLINE || prevStatus === CliStatus.DISCONNECTED
      const isActive = newCliStatus === CliStatus.ONLINE || newCliStatus === CliStatus.NOTIFIED ||
                       newCliStatus === CliStatus.CONNECTING || newCliStatus === CliStatus.CONNECTED
      if (isActive && wasInactive) {
        this.#peerReconnectAttempts = 0  // fresh health cycle, reset backoff
        this.#ensureConnected().catch(() => {})
      }

      // CLI connected to E2E channel while we're already subscribed — initiate handshake.
      // If not yet subscribed, subscribe() → #completeHandshake() handles it.
      if (newCliStatus === CliStatus.CONNECTED && prevStatus !== CliStatus.CONNECTED) {
        this.emit("cliConnected")
        if (this.subscriptionId) {
          this.#sendHandshake()
        }
      }

      // Hub went offline — tear down WebRTC, keep signaling for health
      if ((newCliStatus === CliStatus.DISCONNECTED || newCliStatus === CliStatus.OFFLINE) &&
          prevStatus !== CliStatus.DISCONNECTED && prevStatus !== CliStatus.OFFLINE &&
          prevStatus !== CliStatus.UNKNOWN) {
        this.#disconnectPeer()
        this.emit("cliDisconnected")
        this.#setState(ConnectionState.CLI_DISCONNECTED)
      }
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
    return this.state === ConnectionState.CONNECTED
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

  #setConnectionMode(newMode) {
    const prevMode = this.connectionMode
    if (newMode === prevMode) return

    this.connectionMode = newMode
    console.debug(`[${this.constructor.name}] Connection mode: ${prevMode} → ${newMode}`)

    this.emit("connectionModeChange", { mode: newMode, prevMode })
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
