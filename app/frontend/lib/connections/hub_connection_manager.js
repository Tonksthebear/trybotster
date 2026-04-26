/**
 * HubConnectionManager - Generic connection pool with route-aware lifecycle.
 *
 * Manages typed connection wrappers (HubTransport, TerminalConnection) keyed
 * by URL/identifier. Handles reference counting via subscribers and deferred
 * cleanup across client-side route transitions.
 *
 * Usage:
 *   import { HubConnectionManager } from "connections/hub_connection_manager";
 *   import { HubTransport } from "connections/hub_connection";
 *
 *   // On component mount:
 *   HubTransport is control-plane state. In the React app it is owned by
 *   hub-store -> hub-bridge -> HubSession; leaf components should not acquire
 *   another HubTransport.
 *
 *   // On component unmount:
 *   this.transport?.release();
 *
 * Lifecycle:
 *   1. acquire() - Returns existing or creates new typed connection
 *   2. release() - Decrements ref count, queues for deletion if zero
 *   3. idle cleanup - Destroys connections still queued (no one reclaimed)
 */

class HubConnectionManagerSingleton {
  constructor() {
    this.IDLE_DESTROY_DELAY_MS = 5000; // Keep wrapper briefly across route changes, then GC if still unused
    this.connections = new Map(); // key -> { wrapper, refCount }
    this.pendingCreation = new Map(); // key -> Promise<wrapper> (prevents race conditions)
    this.subscribers = new Map(); // key -> Set<callback>
    // WebRTCTransport handles connection lifecycle with grace periods.
    // Connections persist across client-side navigation.
  }

  /**
   * Acquire a typed connection wrapper.
   *
   * @param {Function} ConnectionClass - HubTransport or TerminalConnection class
   * @param {string} key - Unique identifier (e.g., hubId or "terminal:hubId:sessionUuid")
   * @param {Object} options - Options passed to ConnectionClass constructor
   * @returns {Promise<Connection>} - The typed connection wrapper
   */
  async acquire(ConnectionClass, key, options = {}) {
    // Check for existing connection
    let entry = this.connections.get(key);
    if (entry) {
      // Refresh wrapper options so reacquire/subscription uses latest params
      // (e.g., terminal rows/cols after layout changes).
      entry.wrapper.options = { ...entry.wrapper.options, ...options };

      if (entry.idleDestroyTimer) {
        clearTimeout(entry.idleDestroyTimer);
        entry.idleDestroyTimer = null;
      }
      const wasIdle = entry.refCount <= 0;
      entry.refCount++;
      // Only reacquire when connection was idle (refCount was 0).
      // During client-side navigation, multiple consumers can acquire the same key —
      // only the first needs to re-subscribe. Subsequent acquires reuse the
      // active connection without disrupting the in-flight subscription.
      if (wasIdle) {
        await entry.wrapper.reacquire();
      }
      return entry.wrapper;
    }

    // Check for in-progress creation (prevents race condition)
    const pending = this.pendingCreation.get(key);
    if (pending) {
      const wrapper = await pending;
      // Now it should exist in connections
      entry = this.connections.get(key);
      if (entry) {
        entry.wrapper.options = { ...entry.wrapper.options, ...options };
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

    // When refCount hits 0, defer idle notification to the next microtask.
    // During client-side navigation, one view can release while the next
    // acquires in the same frame. Deferring lets the refCount
    // bounce 0→N before we decide to start the grace period, avoiding
    // unnecessary disconnect/reconnect churn on rapid link clicks.
    if (entry.refCount <= 0 && !entry.idlePending) {
      entry.idlePending = true;
      queueMicrotask(() => {
        entry.idlePending = false;
        if (entry.refCount <= 0) {
          entry.wrapper.notifyIdle();
        }
      });
    }

    // Hard cleanup for stale zero-ref wrappers. Without this, old wrappers
    // can linger indefinitely and later re-subscribe phantom PTYs.
    if (entry.refCount <= 0 && !entry.idleDestroyTimer) {
      entry.idleDestroyTimer = setTimeout(() => {
        const current = this.connections.get(key);
        if (!current || current !== entry) return;
        if (current.refCount <= 0) {
          this.destroy(key);
        }
      }, this.IDLE_DESTROY_DELAY_MS);
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
 *   const unsubscribe = HubConnectionManager.subscribe(hubId, ({ state }) => {
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
        console.error(`[HubConnectionManager] Subscriber error for ${key}:`, e);
      }
    }
  }

  /**
   * Check if any connection sharing a hubId still has active references.
   * Used by notifyIdle() to avoid starting a grace period on the shared
   * transport when another connection type (e.g., HubTransport) is still
   * alive for the same hub.
   *
   * @param {string} hubId - Hub identifier
   * @returns {boolean}
   */
  hasActiveConnectionForHub(hubId) {
    for (const [key, entry] of this.connections) {
      if (entry.wrapper.getHubId() === hubId && entry.refCount > 0) {
        return true;
      }
    }
    return false;
  }

  /**
   * Find any connection to a given hub that has signaling established.
   * Used by Connection to inherit hub state from a sibling (same hubId, different key).
   *
   * @param {string} hubId - Hub identifier
   * @returns {Connection|null}
   */
  findHubConnectedSibling(hubId) {
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

    if (entry.idleDestroyTimer) {
      clearTimeout(entry.idleDestroyTimer);
      entry.idleDestroyTimer = null;
    }
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
export const HubConnectionManager = new HubConnectionManagerSingleton();
