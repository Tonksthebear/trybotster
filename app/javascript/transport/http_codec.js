/**
 * HTTP/1.1 Codec - Serialize requests and parse responses as raw bytes.
 *
 * Used with StreamMultiplexer to tunnel HTTP over TCP streams.
 * Supports Content-Length, chunked transfer encoding, and Connection: close semantics.
 * Supports streaming responses (SSE, chunked) via ReadableStream.
 */

const encoder = new TextEncoder()
const decoder = new TextDecoder()

/**
 * Serialize an HTTP/1.1 request into raw bytes.
 * @param {string} method - HTTP method (GET, POST, etc.)
 * @param {string} path - Request path with query string
 * @param {Object} headers - Request headers (host/connection are overridden)
 * @param {string|Uint8Array|null} body - Request body
 * @returns {Uint8Array}
 */
export function serializeRequest(method, path, headers = {}, body = null) {
  let bodyBytes = null
  if (body != null) {
    bodyBytes = typeof body === "string" ? encoder.encode(body) : body
  }

  let request = `${method} ${path} HTTP/1.1\r\n`

  // Add headers, skipping host and connection (we set our own)
  for (const [key, value] of Object.entries(headers)) {
    const lower = key.toLowerCase()
    if (lower === "host" || lower === "connection") continue
    request += `${key}: ${value}\r\n`
  }

  request += "Connection: close\r\n"

  if (bodyBytes) {
    request += `Content-Length: ${bodyBytes.length}\r\n`
  }

  request += "\r\n"

  const headerBytes = encoder.encode(request)

  if (!bodyBytes) return headerBytes

  const result = new Uint8Array(headerBytes.length + bodyBytes.length)
  result.set(headerBytes, 0)
  result.set(bodyBytes, headerBytes.length)
  return result
}

/**
 * Streaming HTTP/1.1 response parser.
 * Fed raw byte chunks from a TcpStream, produces a Response object.
 */
export class HttpResponseParser {
  #buffer = new Uint8Array(0)
  #status = 0
  #statusText = ""
  #headers = {}
  #bodyStrategy = null   // 'content-length' | 'chunked' | 'close'
  #contentLength = 0
  #bodyReceived = 0
  #headersParsedFlag = false
  #complete = false

  // ReadableStream controller for streaming body
  #streamController = null
  #bodyStream = null

  // For non-streaming: accumulated body chunks
  #bodyChunks = []

  /**
   * Feed incoming bytes from the TCP stream.
   * @param {Uint8Array} chunk
   */
  feed(chunk) {
    if (this.#complete) return

    this.#appendToBuffer(chunk)

    if (!this.#headersParsedFlag) {
      this.#tryParseHeaders()
    }

    if (this.#headersParsedFlag && !this.#complete) {
      this.#processBody()
    }
  }

  /** @returns {boolean} True once all headers have been parsed */
  headersParsed() {
    return this.#headersParsedFlag
  }

  /** @returns {boolean} True if response body is fully received */
  isComplete() {
    return this.#complete
  }

  /** @returns {boolean} True if this is a streaming response (chunked or SSE) */
  get isStreaming() {
    return this.#bodyStrategy === "chunked" || this.#bodyStrategy === "close"
  }

  /**
   * Mark the response as complete (called on stream CLOSE for Connection: close).
   */
  finalize() {
    if (this.#complete) return
    this.#complete = true

    if (this.#streamController) {
      try { this.#streamController.close() } catch (_) { /* already closed */ }
      this.#streamController = null
    }
  }

  /**
   * Build a Response object from the parsed data.
   * For streaming responses, returns immediately with a ReadableStream body.
   * For complete responses, returns with accumulated body bytes.
   * @returns {Response}
   */
  toResponse() {
    let body = null

    if (this.#bodyStream) {
      // Streaming: use the ReadableStream we built
      body = this.#bodyStream
    } else if (this.#bodyChunks.length > 0) {
      // Complete: concatenate all chunks
      body = this.#concatChunks(this.#bodyChunks)
    }

    return new Response(body, {
      status: this.#status,
      statusText: this.#statusText,
      headers: new Headers(this.#headers),
    })
  }

  // ========== Internal ==========

  #appendToBuffer(chunk) {
    const newBuf = new Uint8Array(this.#buffer.length + chunk.length)
    newBuf.set(this.#buffer, 0)
    newBuf.set(chunk, this.#buffer.length)
    this.#buffer = newBuf
  }

  #tryParseHeaders() {
    // Look for \r\n\r\n
    const headerEnd = this.#findHeaderEnd()
    if (headerEnd === -1) return

    const headerBytes = this.#buffer.slice(0, headerEnd)
    const headerText = decoder.decode(headerBytes)
    const remaining = this.#buffer.slice(headerEnd + 4)  // skip \r\n\r\n

    // Parse status line
    const firstLine = headerText.indexOf("\r\n")
    const statusLine = firstLine === -1 ? headerText : headerText.slice(0, firstLine)
    const statusMatch = statusLine.match(/^HTTP\/\d\.\d (\d+)(?: (.*))?$/)
    if (statusMatch) {
      this.#status = parseInt(statusMatch[1], 10)
      this.#statusText = statusMatch[2] || ""
    }

    // Parse headers
    const headerLines = firstLine === -1 ? "" : headerText.slice(firstLine + 2)
    for (const line of headerLines.split("\r\n")) {
      const colonIdx = line.indexOf(":")
      if (colonIdx === -1) continue
      const key = line.slice(0, colonIdx).trim().toLowerCase()
      const value = line.slice(colonIdx + 1).trim()
      this.#headers[key] = value
    }

    // Determine body strategy
    if (this.#headers["content-length"]) {
      this.#bodyStrategy = "content-length"
      this.#contentLength = parseInt(this.#headers["content-length"], 10)
    } else if ((this.#headers["transfer-encoding"] || "").toLowerCase().includes("chunked")) {
      this.#bodyStrategy = "chunked"
    } else {
      this.#bodyStrategy = "close"
    }

    this.#headersParsedFlag = true

    // Set up streaming body for chunked/close strategies
    if (this.#bodyStrategy === "chunked" || this.#bodyStrategy === "close") {
      this.#bodyStream = new ReadableStream({
        start: (controller) => {
          this.#streamController = controller
        },
      })
    }

    // Process any body bytes that came with the headers
    this.#buffer = remaining
  }

  #processBody() {
    if (this.#buffer.length === 0) return

    switch (this.#bodyStrategy) {
      case "content-length":
        this.#processContentLength()
        break
      case "chunked":
        this.#processChunked()
        break
      case "close":
        this.#processClose()
        break
    }
  }

  #processContentLength() {
    const remaining = this.#contentLength - this.#bodyReceived
    const toConsume = Math.min(this.#buffer.length, remaining)

    if (toConsume > 0) {
      this.#bodyChunks.push(this.#buffer.slice(0, toConsume))
      this.#bodyReceived += toConsume
      this.#buffer = this.#buffer.slice(toConsume)
    }

    if (this.#bodyReceived >= this.#contentLength) {
      this.#complete = true
    }
  }

  #processChunked() {
    // Parse chunked transfer encoding
    while (this.#buffer.length > 0 && !this.#complete) {
      // Find chunk size line
      const lineEnd = this.#findCRLF(this.#buffer)
      if (lineEnd === -1) break  // need more data

      const sizeLine = decoder.decode(this.#buffer.slice(0, lineEnd))
      const chunkSize = parseInt(sizeLine, 16)

      if (isNaN(chunkSize)) break  // malformed

      if (chunkSize === 0) {
        // Terminal chunk
        this.#complete = true
        if (this.#streamController) {
          try { this.#streamController.close() } catch (_) {}
          this.#streamController = null
        }
        break
      }

      // Need: size line + \r\n + chunk data + \r\n
      const needed = lineEnd + 2 + chunkSize + 2
      if (this.#buffer.length < needed) break  // need more data

      const chunkData = this.#buffer.slice(lineEnd + 2, lineEnd + 2 + chunkSize)

      if (this.#streamController) {
        try { this.#streamController.enqueue(chunkData) } catch (_) {}
      }
      this.#bodyChunks.push(chunkData)
      this.#bodyReceived += chunkSize

      this.#buffer = this.#buffer.slice(needed)
    }
  }

  #processClose() {
    // Connection: close — stream all data as it arrives
    if (this.#buffer.length > 0) {
      const chunk = this.#buffer
      this.#buffer = new Uint8Array(0)

      if (this.#streamController) {
        try { this.#streamController.enqueue(chunk) } catch (_) {}
      }
      this.#bodyChunks.push(chunk)
      this.#bodyReceived += chunk.length
    }
  }

  #findHeaderEnd() {
    // Find \r\n\r\n in buffer
    for (let i = 0; i <= this.#buffer.length - 4; i++) {
      if (this.#buffer[i] === 0x0D &&
          this.#buffer[i + 1] === 0x0A &&
          this.#buffer[i + 2] === 0x0D &&
          this.#buffer[i + 3] === 0x0A) {
        return i
      }
    }
    return -1
  }

  #findCRLF(buf) {
    for (let i = 0; i <= buf.length - 2; i++) {
      if (buf[i] === 0x0D && buf[i + 1] === 0x0A) return i
    }
    return -1
  }

  #concatChunks(chunks) {
    let totalLength = 0
    for (const c of chunks) totalLength += c.length
    const result = new Uint8Array(totalLength)
    let offset = 0
    for (const c of chunks) {
      result.set(c, offset)
      offset += c.length
    }
    return result
  }
}
