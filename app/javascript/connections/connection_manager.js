/**
 * ConnectionManager - Generic connection pool with Turbo-aware lifecycle.
 *
 * Manages typed connection wrappers (HubConnection, TerminalConnection) keyed
 * by URL/identifier. Handles reference counting via subscribers and deferred
 * cleanup after Turbo navigations.
 *
 * Usage:
 *   import { ConnectionManager } from "connections/connection_manager";
 *   import { HubConnection } from "connections/hub_connection";
 *
 *   // In Stimulus controller connect():
 *   this.hub = await ConnectionManager.acquire(HubConnection, hubId, { hubId });
 *   this.hub.onAgentList((agents) => this.#render(agents));
 *
 *   // In Stimulus controller disconnect():
 *   this.hub?.release();
 *
 * Lifecycle:
 *   1. acquire() - Returns existing or creates new typed connection
 *   2. release() - Decrements ref count, queues for deletion if zero
 *   3. turbo:render - Destroys connections still queued (no one reclaimed)
 */

class ConnectionManagerSingleton {
  constructor() {
    this.connections = new Map(); // key -> { wrapper, refCount }
    this.pendingCreation = new Map(); // key -> Promise<wrapper> (prevents race conditions)
    this.subscribers = new Map(); // key -> Set<callback>
    // No turbo:render cleanup needed - WebRTCTransport handles connection
    // lifecycle with grace periods. Connections persist across Turbo navigation.
  }

  /**
   * Acquire a typed connection wrapper.
   *
   * @param {Function} ConnectionClass - HubConnection or TerminalConnection class
   * @param {string} key - Unique identifier (e.g., hubId or "hubId:agentIndex:ptyIndex")
   * @param {Object} options - Options passed to ConnectionClass constructor
   * @returns {Promise<Connection>} - The typed connection wrapper
   */
  async acquire(ConnectionClass, key, options = {}) {
    // Check for existing connection
    let entry = this.connections.get(key);
    if (entry) {
      entry.refCount++;
      // Tell worker we're reacquiring (cancels any pending grace period)
      await entry.wrapper.reacquire();
      return entry.wrapper;
    }

    // Check for in-progress creation (prevents race condition)
    const pending = this.pendingCreation.get(key);
    if (pending) {
      const wrapper = await pending;
      // Now it should exist in connections
      entry = this.connections.get(key);
      if (entry) {
        entry.refCount++;
        return entry.wrapper;
      }
      // Fallback: pending creation failed, try creating again
    }

    // Create new connection
    const creationPromise = this.#createConnection(
      ConnectionClass,
      key,
      options,
    );
    this.pendingCreation.set(key, creationPromise);

    try {
      const wrapper = await creationPromise;
      return wrapper;
    } finally {
      this.pendingCreation.delete(key);
    }
  }

  async #createConnection(ConnectionClass, key, options) {
    const wrapper = new ConnectionClass(key, options, this);
    await wrapper.initialize();

    const entry = { wrapper, refCount: 1 };
    this.connections.set(key, entry);

    return wrapper;
  }

  /**
   * Release a connection (called by wrapper.release()).
   * Decrements ref count. When zero, tells worker to start grace period.
   * Worker handles actual cleanup after grace period expires.
   *
   * @param {string} key - Connection key
   */
  release(key) {
    const entry = this.connections.get(key);
    if (!entry) return;

    entry.refCount--;

    // When refCount hits 0, notify worker to start grace period
    // Worker will close connection if not reacquired within grace period
    // We keep the wrapper around for potential reuse
    if (entry.refCount <= 0) {
      entry.wrapper.notifyIdle();
    }
  }

  /**
   * Get a connection without incrementing ref count.
   * Useful for peeking at state.
   *
   * @param {string} key - Connection key
   * @returns {Connection|undefined}
   */
  get(key) {
    return this.connections.get(key)?.wrapper;
  }

  /**
   * Check if a connection exists and is connected.
   *
   * @param {string} key - Connection key
   * @returns {boolean}
   */
  isConnected(key) {
    return this.connections.get(key)?.wrapper?.isConnected() ?? false;
  }

  /**
   * Subscribe to connection state changes without holding a reference.
   * Perfect for UI elements that need to react to connection state
   * but shouldn't keep the connection alive.
   *
   * @param {string} key - Connection key to observe
   * @param {Function} callback - Called with { state, error } on changes
   * @returns {Function} - Unsubscribe function
   *
   * @example
   *   const unsubscribe = ConnectionManager.subscribe(hubId, ({ state }) => {
   *     this.element.disabled = state !== "connected";
   *   });
   *   // In disconnect():
   *   unsubscribe();
   */
  subscribe(key, callback) {
    // Get or create subscriber set for this key
    if (!this.subscribers.has(key)) {
      this.subscribers.set(key, new Set());
    }
    const subs = this.subscribers.get(key);
    subs.add(callback);

    // If connection exists, immediately fire with current state
    const wrapper = this.get(key);
    if (wrapper) {
      callback({ state: wrapper.state, error: wrapper.lastError });
    } else {
      // No connection yet - report as disconnected
      callback({ state: "disconnected", error: null });
    }

    // Return unsubscribe function
    return () => {
      subs.delete(callback);
      if (subs.size === 0) {
        this.subscribers.delete(key);
      }
    };
  }

  /**
   * Notify subscribers of a connection state change.
   * Called by Connection base class when state changes.
   *
   * @param {string} key - Connection key
   * @param {Object} stateInfo - { state, error }
   */
  notifySubscribers(key, stateInfo) {
    const subs = this.subscribers.get(key);
    if (!subs) return;

    for (const callback of subs) {
      try {
        callback(stateInfo);
      } catch (e) {
        console.error(`[ConnectionManager] Subscriber error for ${key}:`, e);
      }
    }
  }

  /**
   * Find any connection to a given hub that has signaling established.
   * Used by Connection to inherit hub state from a sibling (same hubId, different key).
   *
   * @param {string} hubId - Hub identifier
   * @returns {Connection|null}
   */
  findHubConnection(hubId) {
    for (const [key, entry] of this.connections) {
      if (entry.wrapper.getHubId() === hubId && entry.wrapper.isHubConnected()) {
        return entry.wrapper;
      }
    }
    return null;
  }

  /**
   * Force destroy a connection immediately.
   *
   * @param {string} key - Connection key
   */
  destroy(key) {
    const entry = this.connections.get(key);
    if (!entry) return;

    entry.wrapper.destroy();
    this.connections.delete(key);
  }

  /**
   * Destroy all connections. Called on hard page unload.
   */
  destroyAll() {
    for (const [key] of this.connections) {
      this.destroy(key);
    }
  }
}

// Singleton instance
export const ConnectionManager = new ConnectionManagerSingleton();
