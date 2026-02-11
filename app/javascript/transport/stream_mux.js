/**
 * StreamMultiplexer - TCP stream multiplexing over WebRTC DataChannel.
 *
 * Opens TCP connections on the CLI side and forwards raw bytes bidirectionally.
 * Each stream maps to one TCP connection on the CLI.
 *
 * Frame format (after Olm decryption):
 *   [0x02][frame_type:1][stream_id:2 BE][payload...]
 */

const FRAME_OPEN = 0x00
const FRAME_DATA = 0x01
const FRAME_CLOSE = 0x02
const FRAME_OPENED = 0x03
const FRAME_ERROR = 0x04

export { FRAME_OPEN, FRAME_DATA, FRAME_CLOSE, FRAME_OPENED, FRAME_ERROR }

export class TcpStream {
  #streamId
  #sendFrame
  #opened = false
  #closed = false
  #error = null
  #openResolve = null
  #openReject = null
  #dataCallbacks = []
  #closeCallbacks = []
  #errorCallbacks = []

  /**
   * @param {number} streamId - Stream identifier
   * @param {function(number, number, Uint8Array): void} sendFrame - Send frame callback
   */
  constructor(streamId, sendFrame) {
    this.#streamId = streamId
    this.#sendFrame = sendFrame
  }

  /** @returns {number} */
  get streamId() {
    return this.#streamId
  }

  /** @returns {boolean} */
  get opened() {
    return this.#opened
  }

  /** @returns {boolean} */
  get closed() {
    return this.#closed
  }

  /**
   * Wait for the CLI to confirm the TCP connection is open.
   * Resolves on OPENED frame, rejects on ERROR frame.
   * @returns {Promise<void>}
   */
  waitOpen() {
    if (this.#opened) return Promise.resolve()
    if (this.#error) return Promise.reject(new Error(this.#error))
    if (this.#closed) return Promise.reject(new Error("Stream closed before opening"))

    return new Promise((resolve, reject) => {
      this.#openResolve = resolve
      this.#openReject = reject
    })
  }

  /**
   * Send data over the stream.
   * @param {Uint8Array} data - Raw bytes to send
   */
  write(data) {
    if (this.#closed) throw new Error("Stream is closed")
    this.#sendFrame(FRAME_DATA, this.#streamId, data)
  }

  /**
   * Close the stream.
   */
  close() {
    if (this.#closed) return
    this.#closed = true
    this.#sendFrame(FRAME_CLOSE, this.#streamId, new Uint8Array(0))
  }

  /**
   * Register a callback for incoming data.
   * @param {function(Uint8Array): void} cb
   */
  onData(cb) {
    this.#dataCallbacks.push(cb)
  }

  /**
   * Register a callback for stream close.
   * @param {function(): void} cb
   */
  onClose(cb) {
    this.#closeCallbacks.push(cb)
  }

  /**
   * Register a callback for stream error.
   * @param {function(string): void} cb
   */
  onError(cb) {
    this.#errorCallbacks.push(cb)
  }

  // ========== Internal (called by StreamMultiplexer) ==========

  /** @internal */
  _handleOpened() {
    this.#opened = true
    if (this.#openResolve) {
      this.#openResolve()
      this.#openResolve = null
      this.#openReject = null
    }
  }

  /**
   * @internal
   * @param {Uint8Array} payload
   */
  _handleData(payload) {
    for (const cb of this.#dataCallbacks) {
      try { cb(payload) } catch (e) { console.error("[TcpStream] Data callback error:", e) }
    }
  }

  /** @internal */
  _handleClose() {
    this.#closed = true
    for (const cb of this.#closeCallbacks) {
      try { cb() } catch (e) { console.error("[TcpStream] Close callback error:", e) }
    }
  }

  /**
   * @internal
   * @param {string} message
   */
  _handleError(message) {
    this.#error = message
    this.#closed = true

    if (this.#openReject) {
      this.#openReject(new Error(message))
      this.#openResolve = null
      this.#openReject = null
    }

    for (const cb of this.#errorCallbacks) {
      try { cb(message) } catch (e) { console.error("[TcpStream] Error callback error:", e) }
    }
  }
}

export class StreamMultiplexer {
  #streams = new Map()
  #nextStreamId = 1
  #sendFrame

  /**
   * @param {function(number, number, Uint8Array): void} sendFrame
   *   Callback to send a frame: (frameType, streamId, payload) => void
   */
  constructor(sendFrame) {
    this.#sendFrame = sendFrame
  }

  /**
   * Open a new TCP stream to the given port on the CLI.
   * @param {number} port - TCP port on the CLI side
   * @returns {TcpStream}
   */
  open(port) {
    const streamId = this.#nextStreamId++
    const stream = new TcpStream(streamId, this.#sendFrame)
    this.#streams.set(streamId, stream)

    // Send OPEN frame with port as 2-byte big-endian payload
    const payload = new Uint8Array(2)
    payload[0] = (port >> 8) & 0xFF
    payload[1] = port & 0xFF
    this.#sendFrame(FRAME_OPEN, streamId, payload)

    return stream
  }

  /**
   * Route an incoming frame to the appropriate stream.
   * @param {number} frameType - Frame type constant
   * @param {number} streamId - Stream identifier
   * @param {Uint8Array} payload - Frame payload
   */
  handleFrame(frameType, streamId, payload) {
    const stream = this.#streams.get(streamId)
    if (!stream) {
      console.warn(`[StreamMultiplexer] Frame for unknown stream ${streamId}`)
      return
    }

    switch (frameType) {
      case FRAME_OPENED:
        stream._handleOpened()
        break
      case FRAME_DATA:
        stream._handleData(payload)
        break
      case FRAME_CLOSE:
        stream._handleClose()
        this.#streams.delete(streamId)
        break
      case FRAME_ERROR: {
        const message = new TextDecoder().decode(payload)
        stream._handleError(message)
        this.#streams.delete(streamId)
        break
      }
      default:
        console.warn(`[StreamMultiplexer] Unknown frame type: ${frameType}`)
    }
  }

  /**
   * Close a specific stream.
   * @param {number} streamId
   */
  close(streamId) {
    const stream = this.#streams.get(streamId)
    if (stream) {
      stream.close()
      this.#streams.delete(streamId)
    }
  }

  /**
   * Close all open streams.
   */
  closeAll() {
    for (const [streamId, stream] of this.#streams) {
      stream.close()
    }
    this.#streams.clear()
  }
}
