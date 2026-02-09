/**
 * Preview Controller - HTTP preview tunneling via Service Worker.
 *
 * Bridges the Service Worker (which intercepts fetch requests in the iframe)
 * with the PreviewConnection (E2E encrypted TCP streams to CLI's dev server).
 *
 * Architecture:
 *   1. SW intercepts fetch requests in preview iframe
 *   2. SW sends http_request via client.postMessage() to this page
 *   3. This controller receives request via navigator.serviceWorker message event
 *   4. Controller proxies request through PreviewConnection's stream multiplexer
 *   5. CLI forwards TCP stream to localhost dev server
 *   6. Controller sends response back to SW via controller.postMessage()
 *   7. SW resolves the fetch with the proxied response
 *
 * Usage:
 *   <div data-controller="preview"
 *        data-preview-hub-id-value="123"
 *        data-preview-agent-index-value="0"
 *        data-preview-scope-value="/hubs/123/agents/0/1/preview">
 *     <iframe data-preview-target="iframe"></iframe>
 *     <span data-preview-target="status"></span>
 *   </div>
 */

import { Controller } from "@hotwired/stimulus";
import { ConnectionManager, PreviewConnection } from "connections";

export default class extends Controller {
  static targets = ["iframe", "status", "error"];
  static values = {
    hubId: String,
    agentIndex: Number,
    ptyIndex: { type: Number, default: 1 },
    scope: String,
    initialUrl: { type: String, default: "/" },
    port: { type: Number, default: 3000 },
  };

  #connection = null;
  #swMessageHandler = null;
  #unsubscribers = [];

  connect() {
    this.#initialize();
  }

  disconnect() {
    this.#cleanup();
  }

  // ========== Initialization ==========

  async #initialize() {
    try {
      this.#updateStatus("Connecting...", "default");

      // Acquire preview connection via ConnectionManager
      const key = PreviewConnection.key(
        this.hubIdValue,
        this.agentIndexValue,
        this.ptyIndexValue,
      );

      this.#connection = await ConnectionManager.acquire(
        PreviewConnection,
        key,
        {
          hubId: this.hubIdValue,
          agentIndex: this.agentIndexValue,
          ptyIndex: this.ptyIndexValue,
          port: this.portValue,
        },
      );

      // Set up connection event handlers
      this.#unsubscribers.push(
        this.#connection.onStateChange((state) =>
          this.#handleStateChange(state),
        ),
        this.#connection.onError((error) => this.#handleError(error)),
      );

      // Set up SW message listener
      this.#setupServiceWorkerListener();
    } catch (error) {
      this.#updateStatus("Connection failed", "error");
      this.#showError(error.message);
    }
  }

  #cleanup() {
    // Remove SW message listener
    this.#removeServiceWorkerListener();

    // Unsubscribe from connection events
    for (const unsub of this.#unsubscribers) {
      unsub();
    }
    this.#unsubscribers = [];

    // Unsubscribe from channel then release connection ref
    const conn = this.#connection;
    this.#connection = null;
    if (conn) {
      conn.unsubscribe().finally(() => conn.release());
    }
  }

  // ========== Service Worker Communication ==========

  #setupServiceWorkerListener() {
    this.#removeServiceWorkerListener();

    this.#swMessageHandler = (event) => {
      this.#handleServiceWorkerMessage(event.data);
    };

    navigator.serviceWorker.addEventListener("message", this.#swMessageHandler);
  }

  #removeServiceWorkerListener() {
    if (this.#swMessageHandler) {
      navigator.serviceWorker.removeEventListener(
        "message",
        this.#swMessageHandler,
      );
      this.#swMessageHandler = null;
    }
  }

  async #handleServiceWorkerMessage(data) {
    if (data.type !== "http_request") return;

    try {
      const response = await this.#connection.fetch({
        method: data.method,
        path: data.path,
        headers: data.headers,
        body: data.body,
      });

      // Response is a real Response object — extract for postMessage
      const body = await response.arrayBuffer();
      const headers = {};
      response.headers.forEach((value, key) => { headers[key] = value; });

      this.#sendToServiceWorker({
        type: "http_response",
        requestId: data.requestId,
        response: {
          status: response.status,
          statusText: response.statusText,
          headers,
          body: this.#arrayBufferToBase64(body),
        },
      });
    } catch (error) {
      this.#sendToServiceWorker({
        type: "http_response",
        requestId: data.requestId,
        error: error.message,
      });
    }
  }

  #sendToServiceWorker(message) {
    navigator.serviceWorker.controller?.postMessage(message);
  }

  // ========== Connection Event Handlers ==========

  #handleStateChange({ state }) {
    switch (state) {
      case "connected":
        this.#updateStatus("Connected", "success");
        this.#loadIframe();
        break;
      case "connecting":
        this.#updateStatus("Connecting...", "default");
        break;
      case "loading":
        this.#updateStatus("Loading session...", "default");
        break;
      case "disconnected":
        this.#updateStatus("Disconnected", "warning");
        break;
      case "error":
        this.#updateStatus("Connection error", "error");
        break;
    }
  }

  #handleError(error) {
    this.#showError(error.message);
  }

  // ========== UI Helpers ==========

  #loadIframe() {
    if (this.hasIframeTarget && this.scopeValue) {
      this.iframeTarget.src = `${this.scopeValue}${this.initialUrlValue}`;
    }
  }

  #updateStatus(text, type) {
    if (this.hasStatusTarget) {
      this.statusTarget.textContent = text;
      this.statusTarget.className = this.#statusClasses(type);
    }
  }

  #showError(message) {
    if (this.hasErrorTarget) {
      this.errorTarget.textContent = message;
      this.errorTarget.classList.remove("hidden");
    }
  }

  #statusClasses(type) {
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

  // ========== Utility ==========

  #arrayBufferToBase64(buffer) {
    const bytes = new Uint8Array(buffer);
    const CHUNK = 8192;
    const parts = [];
    for (let i = 0; i < bytes.length; i += CHUNK) {
      parts.push(String.fromCharCode(...bytes.subarray(i, i + CHUNK)));
    }
    return btoa(parts.join(""));
  }
}
