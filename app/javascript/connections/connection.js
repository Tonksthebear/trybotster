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
import { ensureMatrixReady } from "matrix/bundle"
import { HandshakeManager } from "connections/handshake_manager"
import { HealthTracker } from "connections/health_tracker"
import { setupHubEventListeners } from "connections/hub_event_handlers"
import { ConnectionState, BrowserStatus, CliStatus, ConnectionMode } from "connections/constants"

// Re-export constants so existing consumers (controllers, subclasses) keep working
export { ConnectionState, BrowserStatus, CliStatus, ConnectionMode }

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
  #initRetryCount = 0       // Retry counter for failed initialize()
  #initRetryTimer = null    // Pending retry timer
  #peerReconnectTimer = null  // Pending peer reconnect timer
  #peerReconnectAttempts = 0  // Retry counter for peer reconnection
  #reacquirePromise = null    // Serializes concurrent reacquire() calls
  #handshake = null           // HandshakeManager instance
  #health = null              // HealthTracker instance

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
    this.connectionMode = ConnectionMode.UNKNOWN

    // Event subscribers: Map<eventName, Set<callback>>
    this.subscribers = new Map()

    // Handshake protocol (E2E encryption verification)
    const log = (msg) => console.debug(`[${this.constructor.name}] ${msg}`)
    this.#handshake = new HandshakeManager({
      sendEncrypted: (msg) => this.#sendEncrypted(msg),
      emit: (event, data) => this.emit(event, data),
      onComplete: () => this.#onHandshakeComplete(),
      log,
    })

    // CLI status tracking + health event processing
    this.#health = new HealthTracker({
      emit: (event, data) => this.emit(event, data),
      ensureConnected: () => this.#ensureConnected().catch(() => {}),
      disconnectPeer: () => this.#disconnectPeer(),
      setState: (s) => this.#setState(s),
      sendHandshake: () => this.#handshake.send(),
      resetHandshake: () => this.#handshake.reset(),
      resetPeerReconnect: () => { this.#peerReconnectAttempts = 0 },
      getSubscriptionId: () => this.subscriptionId,
      getErrorCode: () => this.errorCode,
      getBrowserStatus: () => this.browserStatus,
      notifyManager: (data) => this.manager.notifySubscribers(this.key, { ...data, state: this.state }),
      log,
    })

  }

  // ========== Lifecycle (called by ConnectionManager) ==========

  /**
   * Initialize the connection. Called by ConnectionManager.acquire().
   * Sets up crypto + signaling; #connectSignaling() calls #ensureConnected() at the end.
   */
  async initialize() {
    try {
      this.#setState(ConnectionState.LOADING)

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

    // 1. Check for existing Olm session (created on /pairing page)
    const { hasSession: hasOlmSession } = await bridge.hasSession(hubId)
    if (!hasOlmSession) {
      console.debug(`[${this.constructor.name}] No Olm session — WebRTC disabled until pairing`)
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
    if (this.browserStatus !== BrowserStatus.SUBSCRIBED) return  // browser not connected

    // Step 1: Peer (only if CLI is reachable)
    if (this.#health.isCliReachable()) {
      const hubId = this.getHubId()
      try {
        await bridge.send("connectPeer", { hubId })  // idempotent + deduped in transport
      } catch (e) {
        // Signaling not ready (e.g., ActionCable reconnecting after iOS wake).
        // The signaling connected callback will trigger health events → retry.
        if (e.message?.includes("Signaling not connected")) {
          console.debug(`[${this.constructor.name}] connectPeer deferred (signaling not ready)`)
          return
        }
        throw e
      }
    }

    // Step 2: Subscribe virtual channel
    if (this.#health.isCliReachable() && !this.subscriptionId) {
      try {
        await this.subscribe()  // has its own lock, early-returns if subscribed
      } catch (e) {
        // Transport failures are retriable. Probe peer health before deciding:
        // - Peer alive (DC open) but CLI didn't respond → genuinely stale, tear down
        // - Peer connecting (DC not open yet) → defer, DC onopen will retry
        // - Peer dead → handlePeerDisconnected will fire naturally
        if (e.message?.includes("timeout") || e.message?.includes("stale")) {
          try {
            const hubId = this.getHubId()
            const { dcState } = await bridge.send("probePeerHealth", { hubId })
            if (dcState === "open") {
              // DC is open but CLI didn't respond — genuinely stale (iOS sleep etc.)
              console.debug(`[${this.constructor.name}] Peer stale (DC open, CLI unresponsive), tearing down`)
              await this.#disconnectPeer()
              this.#schedulePeerReconnect()
            } else {
              // DC still connecting — defer, DC onopen will trigger ensureConnected → subscribe
              console.debug(`[${this.constructor.name}] Subscribe timed out (dc=${dcState}), deferring to DC open`)
            }
          } catch {
            // Worker bridge down — assume peer is dead, tear down
            console.debug(`[${this.constructor.name}] Probe failed, tearing down peer`)
            await this.#disconnectPeer()
            this.#schedulePeerReconnect()
          }
          return
        }
        if (e.message?.includes("DataChannel") || e.message?.includes("No connection")) {
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

    this.#handshake.reset()

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
      if (this.browserStatus !== BrowserStatus.SUBSCRIBED) this.#setBrowserStatus(BrowserStatus.SUBSCRIBED)
      this.#health.emitHealthChange()
      return
    }

    // Wait for any in-progress subscribe/unsubscribe
    if (this.#subscribing) {
      await this.#subscribeLock
      // Re-check after waiting - another caller might have subscribed
      if (this.subscriptionId && !force) {
        if (this.browserStatus !== BrowserStatus.SUBSCRIBED) this.#setBrowserStatus(BrowserStatus.SUBSCRIBED)
        this.#health.emitHealthChange()
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

      // Reset handshake state for fresh connection.
      // Don't reset cliStatus — it's managed by HealthTracker via health events.
      this.#handshake.reset()

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

      const subscribeResult = await new Promise((resolve, reject) => {
        const timer = setTimeout(() => reject(new Error("Subscribe timeout — peer may be stale")), 5000)
        bridge.send("subscribe", {
          hubId,
          channel: this.channelName(),
          params: this.channelParams(),
          subscriptionId,
        }).then(
          (v) => { clearTimeout(timer); resolve(v) },
          (e) => { clearTimeout(timer); reject(e) },
        )
      })

      // WebRTC: DataChannel open = ready, complete handshake FIRST
      // so input isn't buffered when listeners fire
      this.#onHandshakeComplete()

      this.#setBrowserStatus(BrowserStatus.SUBSCRIBED)
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
    const unsubs = setupHubEventListeners(bridge, hubId, {
      // Event system
      emit: (event, data) => this.emit(event, data),
      log: (msg) => console.debug(`[${this.constructor.name}] ${msg}`),

      // State queries
      isSessionInvalid: () => this.state === ConnectionState.ERROR && this.errorCode === "session_invalid",
      isCliReachable: () => this.#health.isCliReachable(),
      hasSubscription: () => !!this.subscriptionId,
      getErrorCode: () => this.errorCode,
      getBrowserStatus: () => this.browserStatus,

      // Subscription cleanup
      clearStaleSubscription: () => {
        if (this.subscriptionId) {
          this.#clearSubscriptionEventListeners()
          bridge.clearSubscriptionListeners(this.subscriptionId)
          this.subscriptionId = null
          this.#handshake.reset()
        }
      },

      // Peer reconnection
      schedulePeerReconnect: () => this.#schedulePeerReconnect(),
      cancelPeerReconnect: () => this.#cancelPeerReconnect(),

      // State setters
      setConnectionMode: (m) => this.#setConnectionMode(m),
      setBrowserStatus: (s) => this.#setBrowserStatus(s),

      // Health probe
      probePeerHealth: () => bridge.send("probePeerHealth", { hubId }),

      // Lifecycle
      ensureConnected: () => this.#ensureConnected().catch(() => {}),
      ensureConnectedAsync: () => this.#ensureConnected(),
      disconnectPeer: () => this.#disconnectPeer(),
      handleHealthMessage: (msg) => this.#health.handleHealthMessage(msg),

      // Session errors
      clearIdentity: () => { this.identityKey = null },
      setError: (code, msg) => {
        this.errorCode = code
        this.errorReason = msg
        this.#setState(ConnectionState.ERROR)
        this.emit("error", { reason: code, message: msg })
      },
      clearSessionError: () => {
        if (this.errorCode === "session_invalid") {
          this.errorCode = null
          this.errorReason = null
        }
      },
    })
    this.#unsubscribers.push(...unsubs)
  }

  /** Schedule peer reconnect with exponential backoff. */
  #schedulePeerReconnect() {
    if (this.#peerReconnectTimer) return
    this.#peerReconnectAttempts++

    if (this.#peerReconnectAttempts > 5) {
      console.debug(`[${this.constructor.name}] Peer reconnect exhausted after ${this.#peerReconnectAttempts} attempts, waiting for health event`)
      return
    }

    const delay = Math.min(2000 * Math.pow(1.5, this.#peerReconnectAttempts - 1), 15000)
    this.#peerReconnectTimer = setTimeout(() => {
      this.#peerReconnectTimer = null
      if (!this.#handshake.complete) {
        console.debug(`[${this.constructor.name}] Peer lost but hub online, reconnecting peer (attempt ${this.#peerReconnectAttempts})...`)
        this.#ensureConnected().catch(() => {})
      }
    }, delay)
  }

  /** Cancel pending peer reconnect timer. */
  #cancelPeerReconnect() {
    if (this.#peerReconnectTimer) {
      clearTimeout(this.#peerReconnectTimer)
      this.#peerReconnectTimer = null
    }
    this.#peerReconnectAttempts = 0
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
      // Don't start a grace period if another connection sharing this hubId
      // is still active. Multiple connection types (HubConnection,
      // TerminalConnection) share the same transport-level WebRTC connection.
      // Without this check, a stale TerminalConnection's notifyIdle can start
      // a NEW grace period after a HubConnection's reacquire already cancelled
      // the previous one, causing the shared connection to close.
      if (this.manager.hasActiveConnectionForHub(hubId)) return

      // Tell transport to start grace period for this hub connection.
      // If reacquired before grace period expires, connection is reused.
      bridge.send("disconnect", { hubId }).catch(() => {})
    }
  }

  /**
   * Notify worker that this connection is being reacquired.
   * Cancels any pending grace period in the worker.
   * Called by ConnectionManager.acquire() when reusing a wrapper.
   *
   * Serialized: if multiple controllers call acquire() concurrently,
   * they share the same reacquire work instead of each clearing
   * and re-creating the subscription.
   */
  async reacquire() {
    if (this.#reacquirePromise) {
      return this.#reacquirePromise
    }
    this.#reacquirePromise = this.#doReacquire()
    try {
      return await this.#reacquirePromise
    } finally {
      this.#reacquirePromise = null
    }
  }

  async #doReacquire() {
    const hubId = this.getHubId()
    if (!hubId) return

    // Cancel grace period FIRST by touching signaling, before any async
    // SharedWorker calls. connectSignaling is idempotent — returns existing
    // state if connected, creates a new channel if the grace timer already
    // destroyed it. This prevents the 3s grace timer from racing with the
    // hasSession() round-trip to the crypto SharedWorker.
    const result = await bridge.send("connectSignaling", {
      hubId,
      browserIdentity: this.browserIdentity
    })
    this.#hubConnected = true

    const { hasSession } = await bridge.hasSession(hubId)

    if (!hasSession) {
      this.#hubConnected = false
      this.subscriptionId = null
      this.identityKey = null
      this.#setError("unpaired", "Scan connection code")
      return
    }

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
      this.#handshake.reset()
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
    const fullMessage = { subscriptionId: this.subscriptionId, ...message }

    // Binary inner: [0x00][JSON bytes] (control message)
    const jsonBytes = new TextEncoder().encode(JSON.stringify(fullMessage))
    const plaintext = new Uint8Array(1 + jsonBytes.length)
    plaintext[0] = 0x00 // CONTENT_MSG
    plaintext.set(jsonBytes, 1)

    const { data: encrypted } = await bridge.encryptBinary(hubId, plaintext)
    await bridge.send("sendEncrypted", { hubId, encrypted })
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
      // Stale subscription (e.g., SharedWorker restarted during sleep).
      // Clear it and reconnect — #ensureConnected() will re-subscribe.
      if (error.message?.includes("not found") && this.subscriptionId) {
        console.debug(`[${this.constructor.name}] Subscription stale, reconnecting`)
        const oldSubId = this.subscriptionId
        this.subscriptionId = null
        this.#clearSubscriptionEventListeners()
        bridge.clearSubscriptionListeners(oldSubId)

        await this.#ensureConnected()
        if (!this.subscriptionId) return false

        // Retry the send once after reconnecting
        try {
          await this.#sendEncrypted({ type, ...data })
          return true
        } catch {
          return false
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
   * Send a file (image paste/drop) through the encrypted channel.
   * @param {Uint8Array} data - Raw file bytes
   * @param {string} filename - Original filename
   * @returns {Promise<boolean>}
   */
  async sendBinaryFile(data, filename) {
    if (!this.subscriptionId) {
      await this.#ensureConnected()
      if (!this.subscriptionId) return false
    }

    try {
      const hubId = this.getHubId()
      await bridge.send("sendFileInput", {
        hubId,
        subscriptionId: this.subscriptionId,
        data,
        filename,
      })
      return true
    } catch (error) {
      console.error(`[${this.constructor.name}] sendBinaryFile failed:`, error)
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
      this.#health.handleHealthMessage(message)
      return true
    }
    if (message.type === "connected") {
      console.debug(`[${this.constructor.name}] Received handshake from CLI:`, message.device_name)
      this.#handshake.handleIncoming(message)
      return true
    }
    if (message.type === "ack") {
      this.#handshake.handleAck(message)
      return true
    }
    if (message.type === "cli_disconnected") {
      this.#health.handleCliDisconnected()
      return true
    }
    return false
  }

  // ========== Handshake Callback ==========

  /**
   * Called by HandshakeManager when handshake completes (via onComplete callback).
   * Also called directly from subscribe() since DataChannel is already open.
   */
  #onHandshakeComplete() {
    // Update CLI status to CONNECTED — definitive "CLI is talking to us" signal.
    // Health messages via ActionCable may lag behind actual WebRTC state.
    this.cliStatus = CliStatus.CONNECTED
    this.#setState(ConnectionState.CONNECTED)
    this.#health.emitHealthChange()
    this.emit("connected", this)
  }

  /** Proxy cliStatus through HealthTracker for external access. */
  get cliStatus() { return this.#health.cliStatus }
  set cliStatus(value) { this.#health.cliStatus = value }

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
    this.#health.emitHealthChange()

    // Reactive readiness: when browser health changes, re-evaluate connection.
    // ensureConnected gates on browser + CLI + crypto being ready.
    this.#ensureConnected().catch(() => {})
  }

  #setConnectionMode(newMode) {
    const prevMode = this.connectionMode
    if (newMode === prevMode) return

    this.connectionMode = newMode
    console.debug(`[${this.constructor.name}] Connection mode: ${prevMode} → ${newMode}`)

    this.emit("connectionModeChange", { mode: newMode, prevMode })
  }

}
