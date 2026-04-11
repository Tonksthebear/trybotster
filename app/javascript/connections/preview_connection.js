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
 * Single-PTY model: session UUID is the sole routing key.
 *
 * Usage:
 *   const key = PreviewConnection.key(hubId, sessionUuid);
 *   const preview = await HubConnectionManager.acquire(PreviewConnection, key, {
 *     hubId, sessionUuid, port: 3000
 *   });
 *   const response = await preview.fetch({ method: "GET", path: "/" });
 */

import { HubRoute } from "connections/hub_route"
import { StreamMultiplexer } from "transport/stream_mux"
import { serializeRequest, HttpResponseParser } from "transport/http_codec"
import bridge from "workers/bridge"

export class PreviewConnection extends HubRoute {
  #mux = null
  #streamFrameUnsub = null

  constructor(key, options, manager) {
    super(key, options, manager)
    this.sessionUuid = options.sessionUuid
  }

  // ========== Connection overrides ==========

  channelName() {
    return "preview"
  }

  computeSubscriptionId() {
    return `preview_${this.sessionUuid}`
  }

  channelParams() {
    return {
      hub_id: this.getHubId(),
      session_uuid: this.sessionUuid,
      browser_identity: this.browserIdentity,
    }
  }

  handleMessage(message) {
    if (this.processMessage(message)) {
      return
    }

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

    const port = this.getPort()
    const stream = this.#mux.open(port)

    try {
      await stream.waitOpen()
    } catch (e) {
      throw new Error(`Failed to open stream to port ${port}: ${e.message}`)
    }

    const reqBytes = serializeRequest(
      request.method,
      request.path,
      request.headers || {},
      request.body,
      `localhost:${port}`,
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

  getSessionUuid() {
    return this.sessionUuid
  }

  getPort() {
    return Number(this.options.port ?? 3000)
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

  static key(hubId, sessionUuid) {
    return `preview:${hubId}:${sessionUuid}`
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
