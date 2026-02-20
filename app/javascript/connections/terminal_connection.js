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
 *   2. Browser subscribes with channel="TerminalRelayChannel" (virtual routing)
 *   3. CLI sets up PTY output forwarder for this subscription
 *   4. Browser receives raw PTY output via DataChannel
 *
 * Usage:
 *   const key = TerminalConnection.key(hubId, agentIndex, ptyIndex);
 *   const term = await ConnectionManager.acquire(TerminalConnection, key, {
 *     hubId, agentIndex, ptyIndex
 *   });
 *   term.onOutput((data) => terminal.write(data));
 *   term.sendInput("ls -la\n");
 */

import { Connection } from "connections/connection";

export class TerminalConnection extends Connection {
  // Snapshot buffering: chunks are held until all arrive, preventing garbled
  // output from partial delivery if the connection drops mid-snapshot.
  #snapshotBuffer = null; // { id, total, chunks: Map<index, Uint8Array>, timer }

  constructor(key, options, manager) {
    super(key, options, manager);
    this.agentIndex = options.agentIndex;
    this.ptyIndex = options.ptyIndex ?? 0;
  }

  // ========== Connection overrides ==========

  channelName() {
    return "terminal";
  }

  /**
   * Compute semantic subscription ID.
   * Format: terminal_{agentIndex}_{ptyIndex}
   */
  computeSubscriptionId() {
    return `terminal_${this.agentIndex}_${this.ptyIndex}`;
  }

  channelParams() {
    // WebRTC subscription params - used by CLI to route PTY I/O
    // CLI keys forwarders by (browser_identity, agent_index, pty_index)
    // rows/cols included so CLI can resize PTY immediately at subscription
    // time, eliminating the race between subscribe and resize messages.
    return {
      hub_id: this.getHubId(),
      agent_index: this.agentIndex,
      pty_index: this.ptyIndex,
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
            this.emit("output", terminalData);
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
        this.#emitOutput(message.data, message.compressed);
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
    return this.send("resize", { cols, rows });
  }

  // ========== Getters ==========

  getAgentIndex() {
    return this.agentIndex;
  }

  getPtyIndex() {
    return this.ptyIndex;
  }

  destroy() {
    if (this.#snapshotBuffer) {
      clearTimeout(this.#snapshotBuffer.timer);
      this.#snapshotBuffer = null;
    }
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
      this.emit("output", combined);
    }
  }

  // ========== Private helpers ==========

  async #emitOutput(data, compressed) {
    if (!data) return;

    try {
      const text = compressed ? await this.#decompress(data) : data;
      this.emit("output", text);
    } catch (error) {
      console.error("[TerminalConnection] Failed to decompress:", error);
      this.emit("output", data);
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
    return this.on("output", callback);
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

  // ========== Static helper ==========

  static key(hubId, agentIndex, ptyIndex = 0) {
    return `terminal:${hubId}:${agentIndex}:${ptyIndex}`;
  }
}
