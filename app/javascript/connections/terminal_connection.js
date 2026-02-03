/**
 * TerminalConnection - Typed wrapper for terminal data plane.
 *
 * Manages PTY I/O (input/output streams) and terminal resize events.
 * Uses the shared Signal session from IndexedDB (same as HubConnection).
 *
 * Handshake is handled by base Connection class.
 *
 * Events:
 *   - connected - Handshake completed, E2E active
 *   - disconnected - Channel closed
 *   - stateChange - { state, prevState, error }
 *   - error - { reason, message }
 *   - output - PTY output data (Uint8Array for raw, string for scrollback)
 *
 * Flow:
 *   1. Browser subscribes to TerminalRelayChannel
 *   2. Rails notifies CLI via Bot::Message
 *   3. CLI subscribes → health broadcast → browser knows CLI is there
 *   4. Whoever is "last" sends handshake, other side acks
 *   5. Browser emits "connected" event
 *
 * Usage:
 *   const key = TerminalConnection.key(hubId, agentIndex, ptyIndex);
 *   const term = await ConnectionManager.acquire(TerminalConnection, key, {
 *     hubId, agentIndex, ptyIndex
 *   });
 *   term.onOutput((data) => xterm.write(data));
 *   term.sendInput("ls -la\n");
 */

import { Connection } from "connections/connection";

export class TerminalConnection extends Connection {
  constructor(key, options, manager) {
    super(key, options, manager);
    this.agentIndex = options.agentIndex;
    this.ptyIndex = options.ptyIndex ?? 0;
  }

  // ========== Connection overrides ==========

  channelName() {
    return "TerminalRelayChannel";
  }

  channelParams() {
    // Each browser tab has dedicated streams with CLI (like TUI has dedicated I/O)
    // Browser subscribes to: terminal_relay:{hub}:{agent}:{pty}:{browser_identity}
    return {
      hub_id: this.getHubId(),
      agent_index: this.agentIndex,
      pty_index: this.ptyIndex,
      browser_identity: this.browserIdentity,
    };
  }

  handleMessage(message) {
    // Let base class handle handshake and health messages
    if (this.processMessage(message)) {
      return;
    }

    console.log(`[TerminalConnection] handleMessage:`, message.type, message.data?.length || message);

    switch (message.type) {
      case "raw_output":
        // Raw bytes from CLI (Uint8Array with 0x01 prefix) - pass to xterm
        // Strip the 0x01 prefix byte before emitting
        if (message.data && message.data.length > 0) {
          const prefix = message.data[0];
          if (prefix === 0x01) {
            // Raw terminal data - strip prefix
            const terminalData = message.data.slice(1);
            console.log(`[TerminalConnection] Emitting raw_output, ${terminalData.length} bytes (stripped 0x01 prefix)`);
            this.emit("output", terminalData);
          } else {
            // JSON control message (0x00 prefix) - parse and handle
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
    return this.send("input", { data });
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
