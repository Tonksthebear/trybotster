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
 * 1. Controller establishes PreviewChannel (encrypted WebSocket)
 * 2. Registers service worker to intercept fetch requests
 * 3. Service worker communicates with controller via MessageChannel
 * 4. Controller encrypts requests, sends via PreviewChannel
 * 5. Agent receives, forwards to local dev server, returns encrypted response
 * 6. Controller decrypts response, sends to service worker
 * 7. Service worker returns response to iframe's fetch
 *
 * Usage:
 * ```erb
 * <div data-controller="preview"
 *      data-preview-hub-id-value="123"
 *      data-preview-agent-index-value="0"
 *      data-preview-service-worker-url-value="<%= asset_path('preview/service-worker.js') %>">
 *   <iframe data-preview-target="iframe" src="/__preview__/"></iframe>
 *   <div data-preview-target="status"></div>
 * </div>
 * ```
 */
export default class extends Controller {
  static targets = ["iframe", "status", "error"];

  static values = {
    hubId: String,
    agentIndex: Number,
    serviceWorkerUrl: String,
    initialUrl: { type: String, default: "/" },
  };

  #hub = null;
  #unsubscribers = [];

  connect() {
    this.channel = null;
    this.serviceWorker = null;
    this.messageChannel = null;
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
   * Initialize the preview channel and service worker.
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

      // Register service worker
      await this.registerServiceWorker();

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

      // Set up service worker communication
      this.setupServiceWorkerChannel();

      // Load initial URL in iframe
      if (this.hasIframeTarget) {
        this.iframeTarget.src = `/__preview__${this.initialUrlValue}`;
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
   * Register the preview service worker.
   */
  async registerServiceWorker() {
    if (!("serviceWorker" in navigator)) {
      throw new Error("Service workers not supported");
    }

    const swUrl = this.serviceWorkerUrlValue;
    if (!swUrl) {
      throw new Error("Service worker URL not configured");
    }

    // Register with preview scope
    const registration = await navigator.serviceWorker.register(swUrl, {
      scope: "/__preview__/",
    });

    // Wait for activation
    if (registration.installing) {
      await new Promise((resolve) => {
        registration.installing.addEventListener("statechange", (e) => {
          if (e.target.state === "activated") resolve();
        });
      });
    } else if (registration.waiting) {
      registration.waiting.postMessage({ type: "skipWaiting" });
      await new Promise((resolve) => {
        navigator.serviceWorker.addEventListener("controllerchange", resolve, {
          once: true,
        });
      });
    }

    this.serviceWorker =
      registration.active || registration.waiting || registration.installing;
  }

  /**
   * Set up MessageChannel for service worker communication.
   */
  setupServiceWorkerChannel() {
    this.messageChannel = new MessageChannel();

    // Get client ID for service worker
    const clientId = this.getClientId();

    // Send port to service worker
    this.serviceWorker.postMessage(
      {
        type: "connect",
        clientId,
        port: this.messageChannel.port2,
      },
      [this.messageChannel.port2],
    );

    // Listen for requests from service worker
    this.messageChannel.port1.onmessage = (event) => {
      this.handleServiceWorkerMessage(event.data);
    };
    this.messageChannel.port1.start();
  }

  /**
   * Get unique client ID for service worker routing.
   */
  getClientId() {
    if (!this._clientId) {
      this._clientId = `preview-${this.hubIdValue}-${this.agentIndexValue}-${Date.now()}`;
    }
    return this._clientId;
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
          url: data.url,
          headers: data.headers,
          body: data.body,
        });

        // Send response back to service worker
        this.messageChannel.port1.postMessage({
          type: "http_response",
          request_id: data.request_id,
          status: response.status,
          status_text: response.statusText,
          headers: this.headersToObject(response.headers),
          body: response.body ? this.arrayToBase64(response.body) : null,
        });
      } catch (error) {
        console.error("[Preview] Request failed:", error);
        this.messageChannel.port1.postMessage({
          type: "http_error",
          request_id: data.request_id,
          error: error.message,
        });
      }
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
    if (this.hasIframeTarget && this.channel?.isConnected()) {
      this.iframeTarget.src = `/__preview__${url}`;
    }
  }

  /**
   * Clean up resources.
   */
  cleanup() {
    if (this.messageChannel) {
      this.messageChannel.port1.close();
      this.messageChannel = null;
    }

    if (this.channel) {
      this.channel.disconnect();
      this.channel = null;
    }

    // Notify service worker of disconnect
    if (this.serviceWorker) {
      this.serviceWorker.postMessage({
        type: "disconnect",
        clientId: this.getClientId(),
      });
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
    const base = "text-xs px-2 py-1 rounded";
    switch (type) {
      case "success":
        return `${base} bg-emerald-500/10 text-emerald-400`;
      case "warning":
        return `${base} bg-amber-500/10 text-amber-400`;
      case "error":
        return `${base} bg-red-500/10 text-red-400`;
      default:
        return `${base} bg-zinc-500/10 text-zinc-400`;
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

  headersToObject(headers) {
    const obj = {};
    if (headers instanceof Headers) {
      headers.forEach((value, key) => {
        obj[key] = value;
      });
    } else if (headers) {
      Object.assign(obj, headers);
    }
    return obj;
  }

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
