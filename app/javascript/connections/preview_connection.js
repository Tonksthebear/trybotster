/**
 * PreviewConnection - Typed wrapper for HTTP preview tunneling.
 *
 * Manages E2E encrypted HTTP proxying between browser and agent's dev server.
 * Uses the shared Signal session from IndexedDB (same as HubConnection).
 *
 * Events:
 *   - connected - Channel established
 *   - disconnected - Channel closed
 *   - stateChange - { state, prevState, error }
 *   - error - { reason, message }
 *   - status - { server_running, port }
 *
 * Flow:
 *   1. Loads Signal session from IndexedDB (same session as HubConnection)
 *   2. Subscribes to PreviewChannel
 *   3. Rails notifies CLI via Bot::Message when browser subscribes
 *   4. CLI creates HttpChannel, subscribes to its stream
 *   5. Bidirectional HTTP proxy established
 *
 * Usage:
 *   const key = PreviewConnection.key(hubId, agentIndex, ptyIndex);
 *   const preview = await ConnectionManager.acquire(PreviewConnection, key, {
 *     hubId, agentIndex, ptyIndex
 *   });
 *   const response = await preview.fetch({ method: "GET", path: "/" });
 */

import { Connection } from "connections/connection";

export class PreviewConnection extends Connection {
  constructor(key, options, manager) {
    super(key, options, manager);
    this.agentIndex = options.agentIndex;
    this.ptyIndex = options.ptyIndex ?? 1; // Default to server PTY

    // Pending HTTP requests: Map<requestId, { resolve, reject, timer }>
    this.pendingRequests = new Map();
    this.nextRequestId = 1;

    // CLI readiness tracking
    this.cliReady = false;
    this.readyPromise = null;
    this.readyResolve = null;
  }

  // ========== Connection overrides ==========

  channelName() {
    return "preview";
  }

  /**
   * Compute semantic subscription ID.
   * Format: preview_{agentIndex}_{ptyIndex}
   */
  computeSubscriptionId() {
    return `preview_${this.agentIndex}_${this.ptyIndex}`;
  }

  channelParams() {
    // Browser subscribes to: preview:{hub}:{agent}:{pty}:{browser_identity}
    return {
      hub_id: this.getHubId(),
      agent_index: this.agentIndex,
      pty_index: this.ptyIndex,
      browser_identity: this.browserIdentity,
    };
  }

  handleMessage(message) {
    switch (message.type) {
      case "preview_ready":
        console.log("[PreviewConnection] CLI is ready");
        this.cliReady = true;
        if (this.readyResolve) {
          this.readyResolve();
          this.readyResolve = null;
        }
        this.emit("ready");
        break;

      case "http_response":
        this.#handleHttpResponse(message);
        break;

      case "preview_error":
        this.#handlePreviewError(message);
        break;

      case "preview_status":
        this.emit("status", {
          serverRunning: message.server_running,
          port: message.port,
        });
        break;

      default:
        this.emit("message", message);
    }
  }

  // ========== HTTP Proxy API ==========

  /**
   * Send an HTTP request through the tunnel.
   *
   * @param {Object} request - HTTP request details
   * @param {string} request.method - HTTP method (GET, POST, etc.)
   * @param {string} request.path - Request path with query string
   * @param {Object} [request.headers] - Request headers
   * @param {string} [request.body] - Request body (will be base64 encoded)
   * @param {number} [timeout=30000] - Request timeout in ms
   * @returns {Promise<Object>} - { status, statusText, headers, body }
   */
  async fetch(request, timeout = 30000) {
    if (!this.isConnected()) {
      throw new Error("Preview connection not established");
    }

    // Wait for CLI to be ready before sending requests
    // This prevents message loss from requests sent before CLI subscribed
    await this.waitForReady();

    const requestId = this.nextRequestId++;

    return new Promise((resolve, reject) => {
      // Set up timeout
      const timer = setTimeout(() => {
        this.pendingRequests.delete(requestId);
        reject(new Error(`Request timeout: ${request.method} ${request.path}`));
      }, timeout);

      // Store pending request
      this.pendingRequests.set(requestId, { resolve, reject, timer });

      // Send request through secure channel
      // Format matches CLI's PreviewCommand::HttpRequest
      this.send("http_request", {
        request_id: requestId,
        method: request.method,
        url: request.path, // CLI expects 'url' field
        headers: request.headers || {},
        body: request.body ? this.#encodeBody(request.body) : null,
      }).catch((error) => {
        this.pendingRequests.delete(requestId);
        clearTimeout(timer);
        reject(error);
      });
    });
  }

  /**
   * Request server status from CLI.
   *
   * @returns {Promise<void>}
   */
  async getStatus() {
    return this.send("get_status", {});
  }

  // ========== Getters ==========

  getAgentIndex() {
    return this.agentIndex;
  }

  getPtyIndex() {
    return this.ptyIndex;
  }

  /**
   * Check if CLI is ready to receive requests.
   * @returns {boolean}
   */
  isCliReady() {
    return this.cliReady;
  }

  /**
   * Wait for CLI to signal readiness.
   * Returns immediately if already ready.
   * @param {number} [timeout=10000] - Max wait time in ms
   * @returns {Promise<void>}
   */
  async waitForReady(timeout = 10000) {
    if (this.cliReady) {
      return;
    }

    // Create promise if not already waiting
    if (!this.readyPromise) {
      this.readyPromise = new Promise((resolve, reject) => {
        this.readyResolve = resolve;

        // Timeout fallback
        setTimeout(() => {
          if (!this.cliReady) {
            this.readyResolve = null;
            this.readyPromise = null;
            reject(new Error("Timeout waiting for CLI ready signal"));
          }
        }, timeout);
      });
    }

    return this.readyPromise;
  }

  // ========== Private helpers ==========

  #handleHttpResponse(response) {
    const pending = this.pendingRequests.get(response.request_id);
    if (!pending) {
      console.warn(
        "[PreviewConnection] Received response for unknown request:",
        response.request_id,
      );
      return;
    }

    this.pendingRequests.delete(response.request_id);
    clearTimeout(pending.timer);

    // Decode body if present
    let body = null;
    if (response.body) {
      body = this.#decodeBody(response.body, response.compressed);
    }

    pending.resolve({
      status: response.status,
      statusText: response.status_text || "",
      headers: response.headers || {},
      body,
    });
  }

  #handlePreviewError(error) {
    const pending = this.pendingRequests.get(error.request_id);
    if (pending) {
      this.pendingRequests.delete(error.request_id);
      clearTimeout(pending.timer);
      pending.reject(new Error(error.error || "Preview error"));
    } else {
      // General error not tied to a request
      this.emit("error", {
        reason: "preview_error",
        message: error.error || "Preview error",
      });
    }
  }

  #encodeBody(body) {
    if (typeof body === "string") {
      return btoa(body);
    }
    if (body instanceof Uint8Array) {
      let binary = "";
      for (let i = 0; i < body.length; i++) {
        binary += String.fromCharCode(body[i]);
      }
      return btoa(binary);
    }
    return null;
  }

  #decodeBody(base64Body, compressed) {
    try {
      const binaryString = atob(base64Body);
      const bytes = new Uint8Array(binaryString.length);
      for (let i = 0; i < binaryString.length; i++) {
        bytes[i] = binaryString.charCodeAt(i);
      }

      if (compressed) {
        // Return bytes for async decompression by caller
        return { compressed: true, bytes };
      }

      return bytes;
    } catch (e) {
      console.error("[PreviewConnection] Failed to decode body:", e);
      return null;
    }
  }

  // ========== Event helpers ==========

  onConnected(callback) {
    if (this.isConnected()) callback(this);
    return this.on("connected", callback);
  }

  onDisconnected(callback) {
    return this.on("disconnected", callback);
  }

  onStateChange(callback) {
    callback({ state: this.state, prevState: null, error: this.errorReason });
    return this.on("stateChange", callback);
  }

  onError(callback) {
    return this.on("error", callback);
  }

  onStatus(callback) {
    return this.on("status", callback);
  }

  onReady(callback) {
    if (this.cliReady) callback();
    return this.on("ready", callback);
  }

  // ========== Cleanup ==========

  async destroy() {
    // Reject all pending requests
    for (const [requestId, pending] of this.pendingRequests) {
      clearTimeout(pending.timer)
      pending.reject(new Error("Connection destroyed"))
    }
    this.pendingRequests.clear()

    // Reset ready state
    this.cliReady = false
    this.readyPromise = null
    this.readyResolve = null

    await super.destroy()
  }

  // ========== Static helper ==========

  static key(hubId, agentIndex, ptyIndex = 1) {
    return `preview:${hubId}:${agentIndex}:${ptyIndex}`;
  }
}
