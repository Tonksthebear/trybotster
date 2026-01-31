import { Controller } from "@hotwired/stimulus";
import { loadSession, open } from "channels/secure_channel";

export default class extends Controller {
  static values = {
    state: String,
    id: Number,
  };

  connect() {}

  disconnect() {
    if (this.subscription) {
      consumer.subscriptions.remove(this.subscription);
      this.subscription = null;
    }
  }

  stateValueChanged(oldState, newState) {
    this.notifyListeners("stateChange", {
      state: newState,
    });
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
    if (this.connected && this.session) {
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

  // ========== Connection ==========

  async #initializeConnection() {
    try {
      this.stateValue = "Loading encryption...";

      this.session = await loadSession(this.idValue, { fromFragment: true });
      if (!this.session) {
        this.stateValue = "Pairing Needed";
        return;
      }

      this.stateValue = "Setting up encryption";
      this.identityKey = await this.session.getIdentityKey();

      this.stateValue = "Connecting to server";

      this.connection = await open({
        channel: "HubChannel",
        params: { hub_id: this.idValue, browser_identity: this.identityKey },
        session: this.session,
        reliable: true,
        onMessage: (msg) => this.#handleMessage(msg),
        onDisconnect: () => this.#handleDisconnect(),
        onError: (err) => this.#handleChannelError(err),
      });

      this.stateValue = "Connected to channel";

      this.stateValue = "Sending handshake";
      // await this.#sendHandshake();
    } catch (error) {
      console.error("[HubConnection] Failed to initialize:", error);
      this.#setError("websocket_error", `Connection error: ${error.message}`);
      this.updateStatus("Connection failed", error.message);
    }
  }

  // ========== Message Handling ==========

  #handleMessage(message) {
    // Handle handshake acknowledgment
    if (message.type === "handshake_ack") {
      if (this.handshakeTimer) {
        clearTimeout(this.handshakeTimer);
        this.handshakeTimer = null;
      }

      this.connected = true;
      this.hasCompletedInitialSetup = true;
      this.stateValue = "Connected";
      this.updateStatus(
        "Connected",
        `E2E encrypted to ${this.idValue.substring(0, 8)}...`,
      );

      this.notifyListeners("connected", this);
      return;
    }

    // Handle connection code response
    if (message.type === "connection_code") {
      this.handleConnectionCode(message);
      return;
    }

    // Route other messages to listeners
    this.notifyListeners("message", message);
  }

  #handleDisconnect() {
    this.connected = false;
    this.stateValue = "Disconnected";
    this.notifyListeners("disconnected");
  }

  #handleChannelError(error) {
    console.error("[HubConnection] Channel error:", error);

    if (error.type === "session_invalid") {
      this.clearSessionAndShowError(error.message);
    } else {
      this.#setError("websocket_error", error.message || "Channel error");
    }
    this.updateStatus("Connection failed", error.message || "Channel error");
  }

  // ========== State Management ==========

  #setState(newState) {
    const prevState = this.state;
    this.state = newState;
    this.connectionStateValue = newState; // Persist in DOM for Turbo survival
    if (newState !== State.ERROR) {
      this.errorReason = null;
    }
    this.notifyListeners("stateChange", {
      state: newState,
      reason: this.errorReason,
    });
  }

  #setError(reason, message) {
    this.errorReason = reason;
    this.#setState(State.ERROR);
    console.error(`[HubConnection] Error (${reason}): ${message}`);
    this.notifyListeners("error", { reason, message });
    this.updateStatus("Connection failed", message);
  }

  isOnNewPage() {
    const stillPresent = document.querySelector(
      `[data-controller~="${this.identifier}"][data-turbo-permanent]`,
    );
    return stillPresent;
  }
}
