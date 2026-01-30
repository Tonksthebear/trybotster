import { Controller } from "@hotwired/stimulus";
import { PreviewChannel, ChannelState } from "transport/encrypted-channel";
import { SignalSession } from "signal";
import { ConnectionManager, HubConnection } from "connections";

/**
 * Preview Controller - E2E Encrypted HTTP Tunnel for Agent Dev Server
 *
 * This controller manages the preview iframe that displays an agent's
 * dev server output. All HTTP traffic is E2E encrypted using the
 * same Signal Protocol session as the terminal.
 *
 * Architecture:
 * 1. Service worker is already registered by bootstrap page
 * 2. SW intercepts fetch requests and sends via client.postMessage()
 * 3. This controller receives requests via navigator.serviceWorker message events
 * 4. Controller encrypts requests, sends via PreviewChannel WebSocket
 * 5. Agent receives, forwards to local dev server, returns encrypted response
 * 6. Controller decrypts response, sends to SW via navigator.serviceWorker.controller.postMessage()
 * 7. Service worker returns response to iframe's fetch
 *
 * Usage:
 * ```erb
 * <div data-controller="preview"
 *      data-preview-hub-id-value="123"
 *      data-preview-agent-index-value="0"
 *      data-preview-scope-value="/hubs/123/agents/0/1/preview">
 *   <iframe data-preview-target="iframe"></iframe>
 *   <div data-preview-target="status"></div>
 * </div>
 * ```
 */
export default class extends Controller {
  static targets = ["iframe", "status", "error"];

  static values = {
    hubId: String,
    agentIndex: Number,
    scope: String,
    initialUrl: { type: String, default: "/" },
  };

  #hub = null;
  #unsubscribers = [];
  #swMessageHandler = null;

  connect() {
    this.channel = null;
    this.signalSession = null;

    this.#initConnection();
  }

  disconnect() {
    this.cleanup();
    this.#unsubscribers.forEach((unsub) => unsub());
    this.#unsubscribers = [];
    this.#hub?.release();
    this.#hub = null;
  }

  async #initConnection() {
    if (!this.hubIdValue) return;

    this.#hub = await ConnectionManager.acquire(
      HubConnection,
      this.hubIdValue,
      { hubId: this.hubIdValue },
    );

    this.#unsubscribers.push(this.#hub.onConnected(() => this.initialize()));

    this.#unsubscribers.push(
      this.#hub.onDisconnected(() => this.handleConnectionLost()),
    );

    this.#unsubscribers.push(
      this.#hub.onError((error) => this.handleError(error)),
    );

    // If already connected, initialize now
    if (this.#hub.isConnected()) {
      this.initialize();
    }
  }

  /**
   * Initialize the preview channel and service worker communication.
   * Note: SW is already registered by bootstrap page.
   */
  async initialize() {
    try {
      this.updateStatus("Initializing preview...");

      // Get Signal session from connection controller or load existing
      this.signalSession = await this.getSignalSession();

      if (!this.signalSession) {
        this.showError("No encryption session. Please scan QR code first.");
        return;
      }

      // Set up SW message listener (SW already registered by bootstrap)
      this.setupServiceWorkerListener();

      // Create preview channel
      this.channel = new PreviewChannel({
        hubId: this.hubIdValue,
        agentIndex: this.agentIndexValue,
        signal: this.signalSession,
        onMessage: (msg) => this.handleMessage(msg),
        onStateChange: (state) => this.handleStateChange(state),
        onError: (error) => this.handleError(error),
      });

      // Connect channel
      await this.channel.connect();

      // Load initial URL in iframe (use dynamic scope path)
      if (this.hasIframeTarget && this.scopeValue) {
        this.iframeTarget.src = `${this.scopeValue}${this.initialUrlValue}`;
      }

      this.updateStatus("Preview connected", "success");
    } catch (error) {
      console.error("[Preview] Initialization failed:", error);
      this.showError(`Preview initialization failed: ${error.message}`);
    }
  }

  /**
   * Get Signal session (reuse from hub connection if available).
   */
  async getSignalSession() {
    // Reuse session from hub connection
    if (this.#hub?.session) {
      return this.#hub.session;
    }

    // Fall back to loading directly
    if (this.hubIdValue) {
      return await SignalSession.load(this.hubIdValue);
    }

    return null;
  }

  /**
   * Set up listener for messages from service worker.
   * SW sends http_request via client.postMessage(), we reply via controller.postMessage().
   */
  setupServiceWorkerListener() {
    // Remove any existing listener
    this.removeServiceWorkerListener();

    // Create bound handler
    this.#swMessageHandler = (event) => {
      this.handleServiceWorkerMessage(event.data);
    };

    // Listen for messages from service worker
    navigator.serviceWorker.addEventListener("message", this.#swMessageHandler);
  }

  /**
   * Remove service worker message listener.
   */
  removeServiceWorkerListener() {
    if (this.#swMessageHandler) {
      navigator.serviceWorker.removeEventListener(
        "message",
        this.#swMessageHandler,
      );
      this.#swMessageHandler = null;
    }
  }

  /**
   * Handle message from service worker (HTTP request to proxy).
   */
  async handleServiceWorkerMessage(data) {
    if (data.type === "http_request") {
      try {
        // Send through encrypted channel
        const response = await this.channel.fetch({
          method: data.method,
          url: data.path,
          headers: data.headers,
          body: data.body,
        });

        // Send response back to service worker
        this.sendToServiceWorker({
          type: "http_response",
          requestId: data.requestId,
          response: {
            status: response.status,
            statusText: response.statusText,
            headers: response.headers,
            body: response.body ? this.arrayToBase64(response.body) : null,
          },
        });
      } catch (error) {
        console.error("[Preview] Request failed:", error);
        this.sendToServiceWorker({
          type: "http_response",
          requestId: data.requestId,
          error: error.message,
        });
      }
    }
  }

  /**
   * Send message to service worker.
   */
  sendToServiceWorker(message) {
    if (navigator.serviceWorker.controller) {
      navigator.serviceWorker.controller.postMessage(message);
    } else {
      console.warn("[Preview] No service worker controller to send to");
    }
  }

  /**
   * Handle message from preview channel.
   */
  handleMessage(message) {
    // Most messages are HTTP responses handled by PreviewChannel internally
    // But we might receive control messages here
    if (message.type === "preview_error") {
      this.showError(message.error);
    }
  }

  /**
   * Handle channel state changes.
   */
  handleStateChange(state) {
    switch (state) {
      case ChannelState.CONNECTED:
        this.updateStatus("Preview connected", "success");
        this.clearError();
        break;
      case ChannelState.CONNECTING:
        this.updateStatus("Connecting preview...");
        break;
      case ChannelState.RECONNECTING:
        this.updateStatus("Reconnecting preview...", "warning");
        break;
      case ChannelState.DISCONNECTED:
        this.updateStatus("Preview disconnected", "error");
        break;
      case ChannelState.ERROR:
        this.updateStatus("Preview error", "error");
        break;
    }
  }

  /**
   * Handle errors.
   */
  handleError(error) {
    console.error("[Preview] Error:", error);
    this.showError(error.message || error.error || "Unknown error");
  }

  /**
   * Handle connection lost from main terminal connection.
   */
  handleConnectionLost() {
    this.updateStatus("Connection lost", "error");
    this.cleanup();
  }

  /**
   * Refresh the preview iframe.
   */
  refresh() {
    if (this.hasIframeTarget && this.channel?.isConnected()) {
      this.iframeTarget.contentWindow.location.reload();
    }
  }

  /**
   * Navigate to a URL in the preview.
   */
  navigate(url) {
    if (this.hasIframeTarget && this.channel?.isConnected() && this.scopeValue) {
      this.iframeTarget.src = `${this.scopeValue}${url}`;
    }
  }

  /**
   * Clean up resources.
   */
  cleanup() {
    this.removeServiceWorkerListener();

    if (this.channel) {
      this.channel.disconnect();
      this.channel = null;
    }
  }

  // === UI Helpers ===

  updateStatus(text, type = "info") {
    if (this.hasStatusTarget) {
      this.statusTarget.textContent = text;
      this.statusTarget.className = this.statusClasses(type);
    }
  }

  statusClasses(type) {
    const base = "status-text";
    switch (type) {
      case "success":
        return `${base} text-emerald-400`;
      case "warning":
        return `${base} text-amber-400`;
      case "error":
        return `${base} text-red-400`;
      default:
        return `${base} text-zinc-400`;
    }
  }

  showError(message) {
    if (this.hasErrorTarget) {
      this.errorTarget.textContent = message;
      this.errorTarget.classList.remove("hidden");
    }
  }

  clearError() {
    if (this.hasErrorTarget) {
      this.errorTarget.textContent = "";
      this.errorTarget.classList.add("hidden");
    }
  }

  // === Utility Methods ===

  arrayToBase64(array) {
    if (!array) return null;
    let binary = "";
    const bytes = array instanceof Uint8Array ? array : new Uint8Array(array);
    for (let i = 0; i < bytes.length; i++) {
      binary += String.fromCharCode(bytes[i]);
    }
    return btoa(binary);
  }
}
