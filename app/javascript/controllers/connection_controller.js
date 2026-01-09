import { Controller } from "@hotwired/stimulus";
import consumer from "channels/consumer";
import { initOlm, OlmSession, serializeEnvelope, deserializeEnvelope } from "crypto/olm";

/**
 * Connection Controller - WebSocket + Olm E2E Encryption
 *
 * This controller manages the secure connection between browser and CLI.
 *
 * Architecture:
 * - Other controllers register via `registerListener()` in their `connectionOutletConnected()`
 * - Callbacks are immediately invoked if connection is already established
 * - This eliminates race conditions from event-based communication
 *
 * Usage in dependent controllers:
 * ```
 * connectionOutletConnected(outlet) {
 *   outlet.registerListener(this, {
 *     onConnected: (hubIdentifier) => { ... },
 *     onDisconnected: () => { ... },
 *     onMessage: (message) => { ... },
 *     onError: (error) => { ... },
 *   });
 * }
 *
 * connectionOutletDisconnected(outlet) {
 *   outlet.unregisterListener(this);
 * }
 * ```
 */

const DB_NAME = "botster_olm";
const DB_VERSION = 1;
const SESSION_STORE = "sessions";

export default class extends Controller {
  static targets = ["status"];

  static values = {
    hubIdentifier: String,
  };

  connect() {
    this.subscription = null;
    this.olmSession = null;
    this.hubIdentifier = null;
    this.connected = false;

    // Don't overwrite listeners - outlet callbacks may have already registered
    // (Stimulus can call outlet callbacks before connect())
    if (!this.listeners) {
      this.listeners = new Map();
    }

    // CLI's Olm keys from URL fragment (if present)
    this.cliEd25519 = null;
    this.cliCurve25519 = null;
    this.cliOneTimeKey = null;

    // Initialize and connect
    this.initializeConnection();
  }

  disconnect() {
    this.cleanup();
  }

  // ========== Listener Registration API ==========

  /**
   * Register a controller to receive connection callbacks.
   * If already connected, onConnected is called immediately.
   *
   * @param {Controller} controller - The Stimulus controller registering
   * @param {Object} callbacks - Callback functions
   * @param {Function} callbacks.onConnected - Called with hubIdentifier when E2E established
   * @param {Function} callbacks.onDisconnected - Called when connection lost
   * @param {Function} callbacks.onMessage - Called with decrypted message from CLI
   * @param {Function} callbacks.onError - Called with error message
   */
  registerListener(controller, callbacks) {
    // Lazy init in case outlet callback fires before connect()
    if (!this.listeners) {
      this.listeners = new Map();
    }
    this.listeners.set(controller, callbacks);

    // If already connected, immediately notify
    if (this.connected && this.olmSession) {
      callbacks.onConnected?.(this);
    }
  }

  /**
   * Unregister a controller from receiving callbacks.
   *
   * @param {Controller} controller - The controller to unregister
   */
  unregisterListener(controller) {
    this.listeners?.delete(controller);
  }

  // Notify all listeners of an event
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
      }
    }
  }

  // ========== Connection Logic ==========

  async initializeConnection() {
    // Extract hub ID from URL path: /hubs/{hub_id}
    const pathMatch = window.location.pathname.match(/\/hubs\/([^\/]+)/);
    if (pathMatch) {
      this.hubIdentifier = pathMatch[1];
    } else if (this.hubIdentifierValue) {
      this.hubIdentifier = this.hubIdentifierValue;
    }

    if (!this.hubIdentifier) {
      this.emitError("Hub ID not found in URL");
      return;
    }

    this.updateStatus("Initializing encryption...");

    try {
      // Initialize vodozemac WASM first
      await initOlm();

      // Try to load saved session from IndexedDB
      const savedSession = await this.loadSession(this.hubIdentifier);
      if (savedSession) {
        console.log("Loaded saved Olm session from IndexedDB");
        this.olmSession = savedSession;
        this.updateStatus("Connecting with saved session...");
        this.subscribeToChannel();
        return;
      }

      // No saved session - check URL fragment for keys
      const keysFromUrl = this.parseUrlFragment();
      if (keysFromUrl) {
        console.log("Creating new Olm session from URL keys");
        this.cliEd25519 = keysFromUrl.ed25519;
        this.cliCurve25519 = keysFromUrl.curve25519;
        this.cliOneTimeKey = keysFromUrl.oneTimeKey;

        // Create new Olm session
        this.olmSession = new OlmSession(
          this.cliCurve25519,
          this.cliOneTimeKey
        );

        // Save session to IndexedDB for reconnection
        await this.saveSession(this.hubIdentifier, this.olmSession);
        console.log("Saved new Olm session to IndexedDB");

        this.updateStatus("Connecting...");
        this.subscribeToChannel();
        return;
      }

      // No saved session and no keys in URL
      this.emitError("No secure key found. Scan QR code from CLI to connect.");
    } catch (error) {
      console.error("Failed to initialize connection:", error);
      this.emitError(`Connection error: ${error.message}`);
    }
  }

  parseUrlFragment() {
    const hash = window.location.hash;
    if (!hash || hash.length < 2) {
      return null;
    }

    const params = new URLSearchParams(hash.substring(1));
    let ed25519 = params.get("e");
    let curve25519 = params.get("c");
    let oneTimeKey = params.get("o");

    if (!ed25519 || !curve25519 || !oneTimeKey) {
      return null;
    }

    // Fix base64 encoding: URLSearchParams decodes '+' as space
    ed25519 = ed25519.replace(/ /g, "+");
    curve25519 = curve25519.replace(/ /g, "+");
    oneTimeKey = oneTimeKey.replace(/ /g, "+");

    return { ed25519, curve25519, oneTimeKey };
  }

  // ========== IndexedDB Session Storage ==========

  async openDB() {
    return new Promise((resolve, reject) => {
      const request = indexedDB.open(DB_NAME, DB_VERSION);
      request.onerror = () => reject(request.error);
      request.onsuccess = () => resolve(request.result);
      request.onupgradeneeded = (event) => {
        const db = event.target.result;
        if (!db.objectStoreNames.contains(SESSION_STORE)) {
          db.createObjectStore(SESSION_STORE, { keyPath: "hubIdentifier" });
        }
      };
    });
  }

  async saveSession(hubIdentifier, olmSession) {
    try {
      const db = await this.openDB();
      const pickled = olmSession.pickle();

      return new Promise((resolve, reject) => {
        const tx = db.transaction(SESSION_STORE, "readwrite");
        const store = tx.objectStore(SESSION_STORE);
        const request = store.put({
          hubIdentifier,
          pickled,
          savedAt: new Date().toISOString(),
        });
        request.onerror = () => reject(request.error);
        request.onsuccess = () => resolve();
      });
    } catch (error) {
      console.warn("Failed to save Olm session:", error);
    }
  }

  async loadSession(hubIdentifier) {
    try {
      const db = await this.openDB();

      const record = await new Promise((resolve, reject) => {
        const tx = db.transaction(SESSION_STORE, "readonly");
        const store = tx.objectStore(SESSION_STORE);
        const request = store.get(hubIdentifier);
        request.onerror = () => reject(request.error);
        request.onsuccess = () => resolve(request.result);
      });

      if (record && record.pickled) {
        return OlmSession.fromPickle(record.pickled);
      }
    } catch (error) {
      console.warn("Failed to load Olm session:", error);
    }
    return null;
  }

  async deleteSession(hubIdentifier) {
    try {
      const db = await this.openDB();

      return new Promise((resolve, reject) => {
        const tx = db.transaction(SESSION_STORE, "readwrite");
        const store = tx.objectStore(SESSION_STORE);
        const request = store.delete(hubIdentifier);
        request.onerror = () => reject(request.error);
        request.onsuccess = () => resolve();
      });
    } catch (error) {
      console.warn("Failed to delete Olm session:", error);
    }
  }

  // ========== ActionCable Subscription ==========

  subscribeToChannel() {
    if (this.subscription) {
      this.subscription.unsubscribe();
    }

    this.subscription = consumer.subscriptions.create(
      {
        channel: "TerminalChannel",
        hub_identifier: this.hubIdentifier,
        device_type: "browser",
      },
      {
        connected: () => this.handleConnected(),
        disconnected: () => this.handleDisconnected(),
        rejected: () => this.handleRejected(),
        received: (data) => this.handleReceived(data),
      }
    );
  }

  handleConnected() {
    this.connected = true;
    this.updateStatus(`Connected to ${this.hubIdentifier.substring(0, 8)}...`);

    // Send PreKey message to establish Olm session
    this.sendPreKeyMessage();

    // Notify all registered listeners
    this.notifyListeners("connected", this);
  }

  handleDisconnected() {
    this.connected = false;
    this.updateStatus("Disconnected");
    this.notifyListeners("disconnected");
  }

  handleRejected() {
    this.connected = false;
    this.emitError("Connection rejected - hub may be offline");
  }

  handleReceived(data) {
    switch (data.type) {
      case "terminal":
        if (data.from === "browser") return;
        this.handleEncryptedMessage(data);
        break;

      case "presence":
        // Could notify listeners if needed
        break;

      case "resize":
        break;

      default:
        console.log("Unknown message type:", data.type);
    }
  }

  sendPreKeyMessage() {
    if (!this.olmSession) return;

    const handshake = {
      type: "handshake",
      device_name: this.getBrowserName(),
      browser_curve25519: this.olmSession.getCurve25519Key(),
    };

    const envelope = this.olmSession.encrypt(handshake);
    const serialized = serializeEnvelope(envelope);

    this.subscription.perform("presence", {
      event: "join",
      device_name: this.getBrowserName(),
      prekey_message: serialized,
    });

    // Re-save session after PreKey to keep ratchet state in sync
    this.saveSession(this.hubIdentifier, this.olmSession);

    console.log("Sent PreKey message to establish Olm session");
  }

  handleEncryptedMessage(data) {
    if (!this.olmSession) return;

    try {
      const envelope = deserializeEnvelope(data);
      const message = this.olmSession.decrypt(envelope);

      // Re-save session after decrypt to keep ratchet state in sync
      this.saveSession(this.hubIdentifier, this.olmSession);

      // Notify all registered listeners
      this.notifyListeners("message", message);
    } catch (error) {
      console.error("Failed to decrypt message:", error);

      if (error.message.includes("decrypt") || error.message.includes("session")) {
        console.log("Session may be corrupted, deleting saved session");
        this.deleteSession(this.hubIdentifier);
        this.emitError("Session expired. Scan QR code to reconnect.");
      }
    }
  }

  // ========== Public API for Outlets ==========

  send(type, data) {
    if (!this.olmSession || !this.subscription || !this.connected) {
      console.warn("Cannot send - not connected");
      return false;
    }

    const message = { type, ...data };
    const envelope = this.olmSession.encrypt(message);
    const serialized = serializeEnvelope(envelope);

    this.subscription.perform("relay", serialized);

    // Re-save session after encrypt to keep ratchet state in sync
    this.saveSession(this.hubIdentifier, this.olmSession);

    return true;
  }

  sendInput(inputData) {
    return this.send("input", { data: inputData });
  }

  sendResize(cols, rows) {
    return this.send("resize", { cols, rows });
  }

  requestAgents() {
    return this.send("list_agents", {});
  }

  selectAgent(agentId) {
    return this.send("select_agent", { agent_id: agentId });
  }

  isConnected() {
    return this.connected;
  }

  getHubIdentifier() {
    return this.hubIdentifier;
  }

  async resetSession() {
    await this.deleteSession(this.hubIdentifier);
    this.cleanup();
    this.emitError("Session cleared. Scan QR code to reconnect.");
  }

  // ========== Cleanup ==========

  cleanup() {
    if (this.subscription) {
      this.subscription.unsubscribe();
      this.subscription = null;
    }
    if (this.olmSession) {
      this.olmSession.free();
      this.olmSession = null;
    }
    this.connected = false;
    this.listeners?.clear();
  }

  disconnectAction() {
    this.cleanup();
    this.updateStatus("Disconnected");
    this.notifyListeners("disconnected");
  }

  // ========== Helpers ==========

  updateStatus(text) {
    if (this.hasStatusTarget) {
      this.statusTarget.textContent = text;
    }
  }

  emitError(message) {
    this.updateStatus(message);
    this.notifyListeners("error", message);
  }

  getBrowserName() {
    const ua = navigator.userAgent;
    if (ua.includes("Chrome")) return "Chrome Browser";
    if (ua.includes("Firefox")) return "Firefox Browser";
    if (ua.includes("Safari")) return "Safari Browser";
    return "Web Browser";
  }
}
