/**
 * Connection - Base class for typed connection wrappers.
 *
 * Provides common functionality:
 *   - SecureChannel handle management
 *   - Signal session lifecycle
 *   - Event subscription (typed subclasses add domain-specific events)
 *   - State tracking
 *
 * Subclasses implement:
 *   - channelName() - ActionCable channel class name
 *   - channelParams() - Subscription params
 *   - handleMessage(msg) - Domain-specific message routing
 */

import { loadSession, open } from "channels/secure_channel";

export const ConnectionState = {
  DISCONNECTED: "disconnected",
  LOADING: "loading",
  CONNECTING: "connecting",
  CONNECTED: "connected",
  ERROR: "error",
};

export class Connection {
  constructor(key, options, manager) {
    this.key = key;
    this.options = options;
    this.manager = manager;

    this.handle = null;
    this.session = null;
    this.identityKey = null;
    this.state = ConnectionState.DISCONNECTED;
    this.errorReason = null;

    // Event subscribers: Map<eventName, Set<callback>>
    this.subscribers = new Map();
  }

  // ========== Lifecycle (called by ConnectionManager) ==========

  /**
   * Initialize the connection. Called by ConnectionManager.acquire().
   * Loads Signal session and opens the secure channel.
   */
  async initialize() {
    try {
      this.#setState(ConnectionState.LOADING);

      // Load or create Signal session
      this.session = await loadSession(this.getHubId(), {
        fromFragment: this.options.fromFragment ?? false,
      });

      if (!this.session) {
        this.#setError(
          "no_session",
          "No session available. Scan QR code to pair.",
        );
        return;
      }

      this.identityKey = await this.session.getIdentityKey();
      this.#setState(ConnectionState.CONNECTING);

      // Open secure channel
      this.handle = await open({
        channel: this.channelName(),
        params: this.channelParams(),
        session: this.session,
        reliable: this.isReliable(),
        onMessage: (msg) => this.#onMessage(msg),
        onDisconnect: () => this.#onDisconnect(),
        onError: (err) => this.#onError(err),
      });

      this.#setState(ConnectionState.CONNECTED);
      this.emit("connected", this);
    } catch (error) {
      console.error(`[${this.constructor.name}] Initialize failed:`, error);
      this.#setError("init_failed", error.message);
    }
  }

  /**
   * Destroy the connection. Called by ConnectionManager.destroy().
   * Closes handle, clears session reference, notifies subscribers.
   */
  destroy() {
    this.handle?.close();
    this.handle = null;
    this.session = null;
    this.#setState(ConnectionState.DISCONNECTED);
    this.emit("destroyed");
    this.subscribers.clear();
  }

  /**
   * Release this connection (decrement ref count).
   * Called by controllers in their disconnect().
   */
  release() {
    this.manager.release(this.key);
  }

  // ========== Abstract methods (override in subclasses) ==========

  /**
   * ActionCable channel class name.
   * @returns {string}
   */
  channelName() {
    throw new Error("Subclass must implement channelName()");
  }

  /**
   * Subscription params for the channel.
   * @returns {Object}
   */
  channelParams() {
    throw new Error("Subclass must implement channelParams()");
  }

  /**
   * Extract hubId from options. Override if hubId comes from elsewhere.
   * @returns {string}
   */
  getHubId() {
    return this.options.hubId;
  }

  /**
   * Whether to use reliable delivery. Default true.
   * @returns {boolean}
   */
  isReliable() {
    return true;
  }

  /**
   * Handle a decrypted message. Subclasses route to domain-specific events.
   * @param {Object} message
   */
  handleMessage(message) {
    // Default: emit as generic message
    this.emit("message", message);
  }

  // ========== Public API ==========

  /**
   * Send a message through the secure channel.
   * @param {string} type - Message type
   * @param {Object} data - Message payload
   * @returns {Promise<boolean>}
   */
  async send(type, data = {}) {
    if (!this.handle) {
      return false;
    }

    try {
      return await this.handle.send({ type, ...data });
    } catch (error) {
      console.error(`[${this.constructor.name}] Send failed:`, error);
      return false;
    }
  }

  /**
   * Check if connected.
   * @returns {boolean}
   */
  isConnected() {
    return this.state === ConnectionState.CONNECTED;
  }

  /**
   * Get current state.
   * @returns {string}
   */
  getState() {
    return this.state;
  }

  /**
   * Get error reason if in error state.
   * @returns {string|null}
   */
  getError() {
    return this.errorReason;
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
      this.subscribers.set(event, new Set());
    }
    this.subscribers.get(event).add(callback);

    // Return unsubscribe function
    return () => this.off(event, callback);
  }

  /**
   * Unsubscribe from an event.
   * @param {string} event - Event name
   * @param {Function} callback - Event handler
   */
  off(event, callback) {
    this.subscribers.get(event)?.delete(callback);
  }

  /**
   * Emit an event to all subscribers.
   * @param {string} event - Event name
   * @param {*} data - Event data
   */
  emit(event, data) {
    const callbacks = this.subscribers.get(event);
    if (!callbacks) return;

    for (const callback of callbacks) {
      try {
        callback(data);
      } catch (error) {
        console.error(`[${this.constructor.name}] Event handler error:`, error);
      }
    }
  }

  // ========== Private ==========

  #onMessage(message) {
    // Let subclass handle domain-specific routing
    this.handleMessage(message);
  }

  #onDisconnect() {
    this.#setState(ConnectionState.DISCONNECTED);
    this.emit("disconnected");
  }

  #onError(error) {
    console.error(`[${this.constructor.name}] Channel error:`, error);

    if (error.type === "session_invalid") {
      this.#setError("session_invalid", error.message || "Session expired");
    } else {
      this.#setError("channel_error", error.message || "Channel error");
    }
  }

  #setState(newState) {
    const prevState = this.state;
    this.state = newState;

    if (newState !== ConnectionState.ERROR) {
      this.errorReason = null;
    }

    const stateInfo = { state: newState, prevState, error: this.errorReason };
    this.emit("stateChange", stateInfo);

    // Notify ConnectionManager subscribers (passive observers)
    this.manager.notifySubscribers(this.key, stateInfo);
  }

  #setError(reason, message) {
    this.errorReason = message;
    this.#setState(ConnectionState.ERROR);
    this.emit("error", { reason, message });
  }
}
