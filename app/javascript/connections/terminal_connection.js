/**
 * TerminalConnection - Typed wrapper for terminal data plane.
 *
 * Manages PTY I/O (input/output streams) and terminal resize events.
 * Uses WebRTC DataChannel for E2E encrypted communication with CLI.
 *
 * Events:
 *   - connected - WebRTC DataChannel open, ready for I/O
 *   - disconnected - Channel closed
 *   - stateChange - { state, prevState, error }
 *   - error - { reason, message }
 *   - output - PTY output data (Uint8Array for raw, string for scrollback)
 *
 * Flow:
 *   1. Browser establishes WebRTC peer connection with CLI
 *   2. Browser subscribes with channel="terminal" (virtual routing)
 *   3. CLI sets up PTY output forwarder for this subscription
 *   4. Browser receives raw PTY output via DataChannel
 *
 * Single-PTY model: each session has exactly one PTY. Session UUID is the
 * sole routing key — no more agent_index/pty_index.
 *
 * Usage:
 *   const key = TerminalConnection.key(hubId, sessionUuid);
 *   const term = await HubConnectionManager.acquire(TerminalConnection, key, {
 *     hubId, sessionUuid
 *   });
 *   term.onOutput((data) => terminal.write(data));
 *   term.sendInput("ls -la\n");
 */

import { HubRoute } from "connections/hub_route";

export class TerminalConnection extends HubRoute {
  // Snapshot buffering: chunks are held until all arrive, preventing garbled
  // output from partial delivery if the connection drops mid-snapshot.
  #snapshotBuffer = null; // { id, total, chunks: Map<index, Uint8Array>, timer }
  // Backlog output that can arrive before transport wires onOutput().
  #earlyOutputBuffer = [];
  #earlyOutputBytes = 0;
  static #EARLY_OUTPUT_MAX_BYTES = 2 * 1024 * 1024;
  static #EARLY_OUTPUT_MAX_ITEMS = 512;

  constructor(key, options, manager) {
    super(key, options, manager);
    this.sessionUuid = options.sessionUuid;
  }

  // ========== Connection overrides ==========

  channelName() {
    return "terminal";
  }

  computeSubscriptionId() {
    return `terminal_${this.sessionUuid}`;
  }

  channelParams() {
    return {
      hub_id: this.getHubId(),
      session_uuid: this.sessionUuid,
      browser_identity: this.browserIdentity,
      rows: this.options.rows,
      cols: this.options.cols,
    };
  }

  handleMessage(message) {
    // Let base class handle handshake and health messages
    if (this.processMessage(message)) {
      return;
    }

    // console.log(`[TerminalConnection] handleMessage:`, message.type, message.data?.length || message);

    switch (message.type) {
      case "raw_output":
        // Raw bytes from CLI with prefix byte routing:
        //   0x00 = JSON control message
        //   0x01 = live PTY output (immediate passthrough)
        //   0x02 = snapshot chunk (buffered until complete)
        if (message.data && message.data.length > 0) {
          const prefix = message.data[0];
          if (prefix === 0x01) {
            const terminalData = message.data.slice(1);
            this.#emitOutput(terminalData);
          } else if (prefix === 0x02) {
            this.#handleSnapshotChunk(message.data);
          } else {
            // JSON control message (0x00 prefix)
            const jsonData = new TextDecoder().decode(message.data.slice(1));
            try {
              const parsed = JSON.parse(jsonData);
              this.handleMessage(parsed);
            } catch (e) {
              console.error(`[TerminalConnection] Failed to parse control message:`, e);
            }
          }
        }
        break;

      case "output":
      case "scrollback":
        this.#emitDecodedOutput(message.data, message.compressed);
        break;

      case "pty_closed":
        this.emit("ptyClosed", message);
        break;

      case "pty_error":
        this.emit("error", {
          reason: "pty_error",
          message: message.error || "PTY error",
        });
        break;

      default:
        this.emit("message", message);
    }
  }

  // ========== Terminal Commands ==========

  sendInput(data) {
    return this.sendBinaryPty(data);
  }

  sendFile(data, filename) {
    return this.sendBinaryFile(data, filename);
  }

  sendResize(cols, rows) {
    // Keep local geometry in sync with live terminal size so snapshot cursor
    // validation uses current bounds instead of stale subscribe-time values.
    this.options.cols = cols;
    this.options.rows = rows;
    return this.send("resize", { cols, rows });
  }

  requestSnapshot() {
    return this.send("request_snapshot", {
      rows: this.options.rows,
      cols: this.options.cols,
    });
  }

  // ========== Getters ==========

  getSessionUuid() {
    return this.sessionUuid;
  }

  hasSubscription() {
    return !!this.subscriptionId;
  }

  destroy() {
    if (this.#snapshotBuffer) {
      clearTimeout(this.#snapshotBuffer.timer);
      this.#snapshotBuffer = null;
    }
    this.#earlyOutputBuffer = [];
    this.#earlyOutputBytes = 0;
    super.destroy();
  }

  // ========== Snapshot buffering ==========

  /**
   * Handle a snapshot chunk (prefix 0x02).
   * Header: [0x02][snapshot_id:4 LE][chunk_idx:2 LE][total_chunks:2 LE][data]
   *
   * Chunks are buffered until the complete set arrives, then concatenated
   * and emitted as a single output. If a new snapshot_id arrives, any
   * partial buffer from the previous snapshot is discarded.
   */
  #handleSnapshotChunk(data) {
    if (data.length < 9) return; // too short for header

    const view = new DataView(data.buffer, data.byteOffset, data.byteLength);
    const snapshotId = view.getUint32(1, true);
    const chunkIdx = view.getUint16(5, true);
    const totalChunks = view.getUint16(7, true);
    const chunkData = data.slice(9);

    // New snapshot supersedes any in-progress buffer
    if (this.#snapshotBuffer && this.#snapshotBuffer.id !== snapshotId) {
      clearTimeout(this.#snapshotBuffer.timer);
      this.#snapshotBuffer = null;
    }

    // Initialize buffer for this snapshot
    if (!this.#snapshotBuffer) {
      this.emit("snapshotStart", {
        snapshotId,
        totalChunks,
      });
      this.#snapshotBuffer = {
        id: snapshotId,
        total: totalChunks,
        chunks: new Map(),
        timer: setTimeout(() => {
          // Discard incomplete snapshot after 10s
          console.debug(`[TerminalConnection] Snapshot ${snapshotId.toString(16)} timed out (${this.#snapshotBuffer?.chunks.size}/${totalChunks} chunks)`);
          this.#snapshotBuffer = null;
        }, 10000),
      };
    }

    this.#snapshotBuffer.chunks.set(chunkIdx, chunkData);

    // All chunks received — concatenate and emit
    if (this.#snapshotBuffer.chunks.size === totalChunks) {
      clearTimeout(this.#snapshotBuffer.timer);

      let totalLen = 0;
      for (let i = 0; i < totalChunks; i++) {
        totalLen += this.#snapshotBuffer.chunks.get(i).length;
      }

      const combined = new Uint8Array(totalLen);
      let offset = 0;
      for (let i = 0; i < totalChunks; i++) {
        const chunk = this.#snapshotBuffer.chunks.get(i);
        combined.set(chunk, offset);
        offset += chunk.length;
      }

      this.#snapshotBuffer = null;
      console.debug(`[TerminalConnection] Snapshot ${snapshotId.toString(16)} complete: ${totalChunks} chunks, ${totalLen} bytes`);
      const validated = this.#validateSnapshotCursor(combined);
      this.#emitOutput(validated);
      this.emit("snapshotComplete", {
        snapshotId,
        totalChunks,
        byteLength: validated.byteLength,
      });
    }
  }

  // ========== Private helpers ==========

  /**
   * Validate the trailing cursor-position escape in a snapshot byte array.
   *
   * snapshot_with_scrollback() always ends with \x1b[ROW;COLH. If the row
   * or col is out of bounds (e.g., due to vt100 cursor tracking lag during
   * scrollback manipulation), strip the escape and let the live forwarder
   * correct the cursor position instead.
   *
   * Uses latin1 decoding on the tail so every byte maps 1:1 to a char,
   * avoiding multi-byte edge cases in the pattern search.
   */
  #validateSnapshotCursor(data) {
    const tailLen = Math.min(30, data.length);
    const tail = new TextDecoder("latin1").decode(data.subarray(data.length - tailLen));
    const match = tail.match(/\x1b\[(\d+);(\d+)H$/);
    if (!match) return data;

    const row = parseInt(match[1], 10);
    const col = parseInt(match[2], 10);
    const maxRows = this.options.rows ?? 24;
    const maxCols = this.options.cols ?? 80;

    if (row < 1 || col < 1 || row > maxRows || col > maxCols) {
      console.warn(`[TerminalConnection] Snapshot cursor ${row};${col} out of bounds (${maxRows}x${maxCols}), stripping`);
      const escLen = new TextEncoder().encode(match[0]).length;
      return data.slice(0, data.length - escLen);
    }

    return data;
  }

  async #emitDecodedOutput(data, compressed) {
    if (!data) return;

    try {
      const text = compressed ? await this.#decompress(data) : data;
      this.#emitOutput(text);
    } catch (error) {
      console.error("[TerminalConnection] Failed to decompress:", error);
      this.#emitOutput(data);
    }
  }

  async #decompress(base64Data) {
    const binaryString = atob(base64Data);
    const bytes = new Uint8Array(binaryString.length);
    for (let i = 0; i < binaryString.length; i++) {
      bytes[i] = binaryString.charCodeAt(i);
    }
    const stream = new Blob([bytes])
      .stream()
      .pipeThrough(new DecompressionStream("gzip"));
    return new Response(stream).text();
  }

  // ========== Event helpers ==========

  onOutput(callback) {
    const unsubscribe = this.on("output", callback);
    this.#flushEarlyOutput();
    return unsubscribe;
  }

  onConnected(callback) {
    // Fire immediately if already fully connected
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

  onSnapshotStart(callback) {
    return this.on("snapshotStart", callback);
  }

  onSnapshotComplete(callback) {
    return this.on("snapshotComplete", callback);
  }

  // ========== Static helper ==========

  static key(hubId, sessionUuid) {
    return `terminal:${hubId}:${sessionUuid}`;
  }

  #emitOutput(data) {
    const listeners = this.subscribers.get("output");
    if (listeners && listeners.size > 0) {
      this.emit("output", data);
      return;
    }

    const bytes = this.#outputByteLength(data);
    this.#earlyOutputBuffer.push(data);
    this.#earlyOutputBytes += bytes;

    while (
      this.#earlyOutputBuffer.length > TerminalConnection.#EARLY_OUTPUT_MAX_ITEMS ||
      this.#earlyOutputBytes > TerminalConnection.#EARLY_OUTPUT_MAX_BYTES
    ) {
      const dropped = this.#earlyOutputBuffer.shift();
      this.#earlyOutputBytes -= this.#outputByteLength(dropped);
    }
  }

  #flushEarlyOutput() {
    if (this.#earlyOutputBuffer.length === 0) return;
    const buffered = this.#earlyOutputBuffer;
    this.#earlyOutputBuffer = [];
    this.#earlyOutputBytes = 0;
    for (const chunk of buffered) {
      this.emit("output", chunk);
    }
  }

  #outputByteLength(data) {
    if (data instanceof Uint8Array) return data.byteLength;
    if (typeof data === "string") return data.length;
    return 0;
  }
}
