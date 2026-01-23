import { Controller } from "@hotwired/stimulus";
import consumer from "channels/consumer";
import {
  initSignal,
  SignalSession,
  getHubIdFromPath,
} from "signal";
import { Channel } from "channels/channel";

/**
 * Preview Proxy Controller
 *
 * Establishes E2E WebSocket connection and relays HTTP requests
 * between the service worker and CLI.
 *
 * Flow:
 * 1. Service worker intercepts fetch request
 * 2. SW posts message to this page with request details
 * 3. This controller sends request over E2E WebSocket to CLI
 * 4. CLI proxies to localhost, returns response
 * 5. This controller posts response back to SW
 * 6. SW returns response to original fetch
 */
export default class extends Controller {
  static targets = ["statusBar", "statusText", "frame"];

  static values = {
    hubId: String,
    agentIndex: Number,
    ptyIndex: Number,
    scope: String,
    workerUrl: String,
    wasmJsUrl: String,
    wasmBinaryUrl: String,
  };

  async connect() {
    this.signalSession = null;
    this.channel = null;
    this.subscription = null;
    this.connected = false;
    this.pendingRequests = new Map();

    // Listen for messages from service worker
    navigator.serviceWorker.addEventListener("message", this.handleSwMessage.bind(this));

    // Initialize connection
    await this.initializeConnection();
  }

  disconnect() {
    navigator.serviceWorker.removeEventListener("message", this.handleSwMessage.bind(this));
    this.cleanup();
  }

  async initializeConnection() {
    try {
      this.updateStatus("Loading encryption...");

      // Load Signal WASM
      await initSignal(
        this.workerUrlValue,
        this.wasmJsUrlValue,
        this.wasmBinaryUrlValue
      );

      this.updateStatus("Setting up session...");

      // Load session from IndexedDB (must have been established via terminal page)
      const hubId = this.hubIdValue || getHubIdFromPath();
      this.signalSession = await SignalSession.load(hubId);

      if (!this.signalSession) {
        this.updateStatus("No session. Open terminal first to connect.", "error");
        return;
      }

      this.ourIdentityKey = await this.signalSession.getIdentityKey();

      this.updateStatus("Connecting...");

      // Subscribe to terminal channel for HTTP proxy
      await this.subscribeToChannel();

      this.updateStatus("Connected - Preview ready", "connected");
      this.connected = true;

    } catch (error) {
      console.error("[PreviewProxy] Connection error:", error);
      this.updateStatus(`Error: ${error.message}`, "error");
    }
  }

  subscribeToChannel() {
    return new Promise((resolve, reject) => {
      this.subscription = consumer.subscriptions.create(
        {
          channel: "TerminalRelayChannel",
          hub_id: this.hubIdValue,
          agent_index: this.agentIndexValue,
          pty_index: this.ptyIndexValue,
          browser_identity: this.ourIdentityKey,
        },
        {
          connected: () => {
            console.log("[PreviewProxy] Channel connected");

            this.channel = Channel.builder(this.subscription)
              .session(this.signalSession)
              .reliable(true)
              .onMessage((msg) => this.handleCliMessage(msg))
              .onDisconnect(() => this.handleDisconnect())
              .onError((err) => this.handleError(err))
              .build();
            this.channel.markConnected();

            resolve();
          },
          disconnected: () => {
            console.log("[PreviewProxy] Channel disconnected");
            this.handleDisconnect();
          },
          rejected: () => {
            reject(new Error("Subscription rejected"));
          },
          received: async (data) => {
            if (data.sender_key_distribution) {
              await this.signalSession?.processSenderKeyDistribution(data.sender_key_distribution);
              return;
            }
            if (this.channel) {
              await this.channel.receive(data);
            }
          },
        }
      );
    });
  }

  // Handle messages from CLI
  handleCliMessage(message) {
    if (message.type === "http_response") {
      const pending = this.pendingRequests.get(message.request_id);
      if (pending) {
        this.pendingRequests.delete(message.request_id);
        // Send response back to service worker
        this.postToServiceWorker({
          type: "http_response",
          requestId: message.request_id,
          response: {
            status: message.status,
            statusText: message.status_text,
            headers: message.headers,
            body: message.body, // base64 encoded
          },
        });
      }
    }
  }

  // Handle messages from service worker
  handleSwMessage(event) {
    const { type, requestId, method, path, headers, body } = event.data;

    if (type === "http_request") {
      this.proxyRequest(requestId, method, path, headers, body);
    }
  }

  async proxyRequest(requestId, method, path, headers, body) {
    if (!this.connected || !this.channel) {
      this.postToServiceWorker({
        type: "http_response",
        requestId,
        error: "Not connected to CLI",
      });
      return;
    }

    // Track pending request
    this.pendingRequests.set(requestId, { requestId });

    // Send HTTP request to CLI
    const message = {
      type: "http_request",
      request_id: requestId,
      method,
      path,
      headers,
      body,
    };

    const sent = await this.channel.send(message);
    if (!sent) {
      this.pendingRequests.delete(requestId);
      this.postToServiceWorker({
        type: "http_response",
        requestId,
        error: "Failed to send request",
      });
    }
  }

  postToServiceWorker(message) {
    if (navigator.serviceWorker.controller) {
      navigator.serviceWorker.controller.postMessage(message);
    }
  }

  handleDisconnect() {
    this.connected = false;
    this.updateStatus("Disconnected", "error");

    // Fail all pending requests
    for (const [requestId] of this.pendingRequests) {
      this.postToServiceWorker({
        type: "http_response",
        requestId,
        error: "Connection lost",
      });
    }
    this.pendingRequests.clear();
  }

  handleError(error) {
    console.error("[PreviewProxy] Error:", error);
    this.updateStatus(`Error: ${error.message}`, "error");
  }

  updateStatus(text, state = "loading") {
    if (this.hasStatusTextTarget) {
      this.statusTextTarget.textContent = text;
    }
    if (this.hasStatusBarTarget) {
      this.statusBarTarget.classList.remove("connected", "error");
      if (state === "connected") {
        this.statusBarTarget.classList.add("connected");
      } else if (state === "error") {
        this.statusBarTarget.classList.add("error");
      }
    }
  }

  cleanup() {
    if (this.channel) {
      this.channel.destroy();
      this.channel = null;
    }
    if (this.subscription) {
      this.subscription.unsubscribe();
      this.subscription = null;
    }
    this.connected = false;
    this.pendingRequests.clear();
  }
}
