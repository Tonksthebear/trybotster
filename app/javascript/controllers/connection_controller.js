import { Controller } from "@hotwired/stimulus";
import consumer from "channels/consumer";
import {
  initSignal,
  SignalSession,
  parseBundleFromFragment,
  getHubIdFromPath,
  ConnectionState,
  ConnectionError,
} from "signal";
import { Channel } from "channels/channel";

/**
 * Connection Controller (Base Class)
 *
 * Manages Signal Protocol E2E encryption session lifecycle.
 * Not mounted directly in HTML — subclassed by HubConnectionController
 * and TerminalConnectionController.
 *
 * Provides:
 * - WASM initialization (idempotent, safe for concurrent calls)
 * - Signal session setup (from URL fragment or IndexedDB cache)
 * - Connection state machine
 * - Listener registration API for downstream controllers
 * - Encrypted send/receive via Channel wrapper
 */

export {
  ConnectionState,
  ConnectionError,
  consumer,
  Channel,
  initSignal,
  SignalSession,
  parseBundleFromFragment,
  getHubIdFromPath,
};

export default class extends Controller {
  static values = {
    hubId: String,
    workerUrl: String,
    wasmJsUrl: String,
    wasmBinaryUrl: String,
  };

  connect() {
    this.signalSession = null;
    this.hubId = null;
    this.ourIdentityKey = null;
    this.connected = false;
    this.state = ConnectionState.DISCONNECTED;
    this.errorReason = null;

    // Don't overwrite listeners - outlet callbacks may have already registered
    if (!this.listeners) {
      this.listeners = new Map();
    }
  }

  disconnect() {
    this.cleanup();
  }

  // ========== Session Initialization ==========

  /**
   * Initialize WASM and Signal session.
   * Safe to call from multiple controllers concurrently.
   */
  async initSession() {
    // Get hub ID from URL path or value
    this.hubId = getHubIdFromPath();
    if (!this.hubId && this.hubIdValue) {
      this.hubId = this.hubIdValue;
    }

    if (!this.hubId) {
      this.setError(ConnectionError.NO_BUNDLE, "Hub ID not found in URL");
      return false;
    }

    try {
      // Step 1: Load Signal WASM (idempotent)
      this.setState(ConnectionState.LOADING_WASM);

      await initSignal(
        this.workerUrlValue,
        this.wasmJsUrlValue,
        this.wasmBinaryUrlValue,
      );

      // Step 2: Set up Signal session
      this.setState(ConnectionState.CREATING_SESSION);

      await this.setupSignalSession();

      if (!this.signalSession) {
        this.setError(
          ConnectionError.NO_BUNDLE,
          "Not paired yet. Press Ctrl+P in CLI and select 'Show Connection Code' to scan QR code.",
        );
        return false;
      }

      // Get our identity key to filter out our own messages
      this.ourIdentityKey = await this.signalSession.getIdentityKey();

      return true;
    } catch (error) {
      console.error(`[${this.identifier}] Session init failed:`, error);
      this.setError(
        ConnectionError.WEBSOCKET_ERROR,
        `Connection error: ${error.message}`,
      );
      return false;
    }
  }

  /**
   * Set up Signal session from URL fragment (QR scan) or IndexedDB cache.
   * Subclasses can override to skip fragment parsing (e.g., TerminalConnection).
   */
  async setupSignalSession() {
    // Check for bundle in URL fragment (fresh QR code scan)
    const urlBundle = parseBundleFromFragment();

    if (urlBundle) {
      // Fresh bundle from QR code - always use it
      this.signalSession = await SignalSession.create(urlBundle, this.hubId);
      // Clear fragment after successful session creation
      if (window.history.replaceState) {
        window.history.replaceState(
          null,
          "",
          window.location.pathname + window.location.search,
        );
      }
    } else {
      // Try to restore from IndexedDB
      this.signalSession = await SignalSession.load(this.hubId);
    }
  }

  // ========== Listener Registration API ==========

  /**
   * Register a controller to receive connection callbacks.
   * If already connected, onConnected is called immediately.
   */
  registerListener(controller, callbacks) {
    if (!this.listeners) {
      this.listeners = new Map();
    }
    this.listeners.set(controller, callbacks);

    // If already connected, immediately notify
    if (this.connected && this.signalSession) {
      callbacks.onConnected?.(this);
    }

    // Notify of current state
    callbacks.onStateChange?.(this.state, this.errorReason);
  }

  /**
   * Unregister a controller from receiving callbacks.
   */
  unregisterListener(controller) {
    this.listeners?.delete(controller);
  }

  /**
   * Notify all listeners of an event.
   */
  notifyListeners(event, data) {
    if (!this.listeners) return;
    for (const [, callbacks] of this.listeners) {
      switch (event) {
        case "connected":
          callbacks.onConnected?.(data);
          break;
        case "disconnected":
          callbacks.onDisconnected?.();
          break;
        case "message":
          callbacks.onMessage?.(data);
          break;
        case "error":
          callbacks.onError?.(data);
          break;
        case "stateChange":
          callbacks.onStateChange?.(data.state, data.reason);
          break;
      }
    }
  }

  // ========== State Management ==========

  setState(state) {
    const prevState = this.state;
    this.state = state;
    if (state !== ConnectionState.ERROR) {
      this.errorReason = null;
    }
    console.log(`[${this.identifier}] State: ${prevState} -> ${state}`);
    this.notifyListeners("stateChange", { state, reason: this.errorReason });
  }

  setError(reason, message) {
    this.errorReason = reason;
    this.setState(ConnectionState.ERROR);
    console.error(`[${this.identifier}] Error (${reason}): ${message}`);
    this.notifyListeners("error", { reason, message });
  }

  // ========== Public API ==========

  isConnected() {
    return this.connected;
  }

  getHubId() {
    return this.hubId;
  }

  getState() {
    return this.state;
  }

  getErrorReason() {
    return this.errorReason;
  }

  async resetSession() {
    if (this.signalSession) {
      await this.signalSession.clear();
      this.signalSession = null;
    }
    this.cleanup();
    this.setError(
      ConnectionError.SESSION_CREATE_FAILED,
      "Session cleared. Scan QR code to reconnect.",
    );
  }

  /**
   * Clear the cached Signal session and show an error prompting user to re-scan QR.
   */
  async clearSessionAndShowError(message) {
    console.log(
      `[${this.identifier}] Clearing stale session for hub:`,
      this.hubId,
    );

    if (this.signalSession) {
      try {
        await this.signalSession.clear();
      } catch (err) {
        console.error(`[${this.identifier}] Failed to clear session:`, err);
      }
      this.signalSession = null;
    }

    this.cleanup();
    this.setError(
      ConnectionError.SESSION_INVALID,
      message || "Session expired. Please re-scan the QR code.",
    );
  }

  // ========== Cleanup ==========

  /**
   * Base cleanup — subclasses should call super.cleanup() and clean up their own channels.
   */
  cleanup() {
    this.connected = false;
    this.listeners?.clear();
  }
}
