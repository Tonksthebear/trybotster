/**
 * PreviewConnection - HTTP preview tunneling via TCP stream multiplexing.
 *
 * Manages E2E encrypted HTTP proxying between browser and agent's dev server.
 * Uses StreamMultiplexer to open TCP connections on the CLI side, then sends
 * raw HTTP/1.1 requests and parses responses as byte streams.
 *
 * Events:
 *   - connected - Channel established
 *   - disconnected - Channel closed
 *   - stateChange - { state, prevState, error }
 *   - error - { reason, message }
 *
 * Flow:
 *   1. Connection base class establishes WebRTC + Olm session
 *   2. StreamMultiplexer sends OPEN frame to CLI with target port
 *   3. CLI opens TCP connection to localhost:port, sends OPENED
 *   4. Browser serializes HTTP request as raw bytes, sends via DATA frames
 *   5. CLI forwards TCP bytes, browser parses HTTP response
 *
 * Usage:
 *   const key = PreviewConnection.key(hubId, agentIndex, ptyIndex);
 *   const preview = await ConnectionManager.acquire(PreviewConnection, key, {
 *     hubId, agentIndex, ptyIndex, port: 3000
 *   });
 *   const response = await preview.fetch({ method: "GET", path: "/" });
 */

import { Connection } from "connections/connection"
import { StreamMultiplexer } from "transport/stream_mux"
import { serializeRequest, HttpResponseParser } from "transport/http_codec"
import bridge from "workers/bridge"

export class PreviewConnection extends Connection {
  #mux = null
  #streamFrameUnsub = null

  constructor(key, options, manager) {
    super(key, options, manager)
    this.agentIndex = options.agentIndex
    this.ptyIndex = options.ptyIndex ?? 1
    this.port = options.port ?? 3000
  }

  // ========== Connection overrides ==========

  channelName() {
    return "preview"
  }

  /**
   * Compute semantic subscription ID.
   * Format: preview_{agentIndex}_{ptyIndex}
   */
  computeSubscriptionId() {
    return `preview_${this.agentIndex}_${this.ptyIndex}`
  }

  channelParams() {
    return {
      hub_id: this.getHubId(),
      agent_index: this.agentIndex,
      pty_index: this.ptyIndex,
      browser_identity: this.browserIdentity,
    }
  }

  handleMessage(message) {
    // Stream frames are handled via the stream:frame event, not subscription messages.
    // Only handle control messages that come through the subscription path.
    switch (message.type) {
      case "preview_status":
        this.emit("status", {
          serverRunning: message.server_running,
          port: message.port,
        })
        break

      default:
        this.emit("message", message)
    }
  }

  // ========== Lifecycle ==========

  async initialize() {
    this.#setupMux()
    await super.initialize()
  }

  async reacquire() {
    this.#setupMux()
    await super.reacquire()
  }

  // ========== HTTP Proxy API ==========

  /**
   * Send an HTTP request through a TCP stream.
   *
   * Opens a new stream to the configured port, serializes the request as
   * raw HTTP/1.1 bytes, and parses the response from the byte stream.
   *
   * @param {Object} request - HTTP request details
   * @param {string} request.method - HTTP method (GET, POST, etc.)
   * @param {string} request.path - Request path with query string
   * @param {Object} [request.headers] - Request headers
   * @param {string|Uint8Array} [request.body] - Request body
   * @param {number} [timeout=30000] - Request timeout in ms
   * @returns {Promise<Response>} - Standard Response object
   */
  async fetch(request, timeout = 30000) {
    if (!this.isConnected()) {
      throw new Error("Preview connection not established")
    }

    if (!this.#mux) {
      throw new Error("Stream multiplexer not initialized")
    }

    const stream = this.#mux.open(this.port)

    try {
      await stream.waitOpen()
    } catch (e) {
      throw new Error(`Failed to open stream to port ${this.port}: ${e.message}`)
    }

    const reqBytes = serializeRequest(
      request.method,
      request.path,
      request.headers || {},
      request.body,
    )
    stream.write(reqBytes)

    const parser = new HttpResponseParser()

    return new Promise((resolve, reject) => {
      let settled = false
      const settle = (fn, value) => {
        if (settled) return
        settled = true
        clearTimeout(timer)
        fn(value)
      }

      const timer = setTimeout(() => {
        stream.close()
        settle(reject, new Error(`Request timeout: ${request.method} ${request.path}`))
      }, timeout)

      stream.onData((data) => {
        parser.feed(data)
        if (parser.headersParsed() && !parser.isStreaming && parser.isComplete()) {
          settle(resolve, parser.toResponse())
        }
      })

      stream.onClose(() => {
        parser.finalize()
        settle(resolve, parser.toResponse())
      })

      stream.onError((msg) => {
        settle(reject, new Error(msg))
      })
    })
  }

  // ========== Getters ==========

  getAgentIndex() {
    return this.agentIndex
  }

  getPtyIndex() {
    return this.ptyIndex
  }

  getPort() {
    return this.port
  }

  // ========== Event helpers ==========

  onConnected(callback) {
    if (this.isConnected()) callback(this)
    return this.on("connected", callback)
  }

  onDisconnected(callback) {
    return this.on("disconnected", callback)
  }

  onStateChange(callback) {
    callback({ state: this.state, prevState: null, error: this.errorReason })
    return this.on("stateChange", callback)
  }

  onError(callback) {
    return this.on("error", callback)
  }

  onStatus(callback) {
    return this.on("status", callback)
  }

  // ========== Cleanup ==========

  destroy() {
    if (this.#mux) {
      this.#mux.closeAll()
      this.#mux = null
    }

    if (this.#streamFrameUnsub) {
      this.#streamFrameUnsub()
      this.#streamFrameUnsub = null
    }

    super.destroy()
  }

  // ========== Static helper ==========

  static key(hubId, agentIndex, ptyIndex = 1) {
    return `preview:${hubId}:${agentIndex}:${ptyIndex}`
  }

  // ========== Private ==========

  #setupMux() {
    // Clean up old mux if re-initializing
    if (this.#mux) {
      this.#mux.closeAll()
    }
    if (this.#streamFrameUnsub) {
      this.#streamFrameUnsub()
    }

    const hubId = this.getHubId()

    // Create multiplexer with send callback that goes through the bridge
    this.#mux = new StreamMultiplexer((frameType, streamId, payload) => {
      bridge.send("sendStreamFrame", { hubId, frameType, streamId, payload })
    })

    // Listen for incoming stream frames from the transport
    this.#streamFrameUnsub = bridge.on("stream:frame", (event) => {
      if (event.hubId !== hubId) return
      this.#mux.handleFrame(event.frameType, event.streamId, event.payload)
    })
  }
}
