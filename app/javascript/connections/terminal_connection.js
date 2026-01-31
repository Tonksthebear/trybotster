/**
 * TerminalConnection - Typed wrapper for terminal data plane.
 *
 * Manages PTY I/O (input/output streams) and terminal resize events.
 * Uses the shared Signal session from IndexedDB (same as HubConnection).
 *
 * Events:
 *   - connected - Channel established
 *   - disconnected - Channel closed
 *   - stateChange - { state, prevState, error }
 *   - error - { reason, message }
 *   - output - PTY output data (Uint8Array for raw, string for scrollback)
 *
 * Flow:
 *   1. Loads Signal session from IndexedDB (same session as HubConnection)
 *   2. Subscribes to TerminalRelayChannel
 *   3. Rails notifies CLI via Bot::Message when browser subscribes
 *   4. CLI subscribes to its stream, bidirectional channel established
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

    // Input buffering until CLI signals ready.
    // Prevents race condition where browser sends input before CLI subscribes,
    // causing seq=1 to be lost and a 3-7 second delay waiting for retransmit.
    this.cliReady = false;
    this.inputBuffer = [];
  }

  // ========== Connection overrides ==========

  async subscribe(options = {}) {
    // Reset ready state on new subscription - need fresh handshake from CLI
    this.cliReady = false;
    this.inputBuffer = [];
    return super.subscribe(options);
  }

  channelName() {
    return "TerminalRelayChannel";
  }

  channelParams() {
    // Each browser has dedicated streams with CLI (like TUI has dedicated I/O)
    // Browser subscribes to: terminal_relay:{hub}:{agent}:{pty}:{browser_identity}
    return {
      hub_id: this.getHubId(),
      agent_index: this.agentIndex,
      pty_index: this.ptyIndex,
      browser_identity: this.identityKey,
    };
  }

  handleMessage(message) {
    switch (message.type) {
      case "input_ready":
        // CLI is subscribed and ready to receive input - flush buffer
        this.#handleCliReady();
        break;

      case "raw_output":
        // Raw bytes from CLI - pass directly to xterm
        this.emit("output", message.data);
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
    if (!this.cliReady) {
      this.inputBuffer.push({ type: "input", data: { data } });
      return Promise.resolve(true);
    }
    return this.send("input", { data });
  }

  sendResize(cols, rows) {
    if (!this.cliReady) {
      this.inputBuffer.push({ type: "resize", data: { cols, rows } });
      return Promise.resolve(true);
    }
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

  #handleCliReady() {
    if (this.cliReady) return; // Already ready, ignore duplicate

    this.cliReady = true;
    console.log(`[TerminalConnection] CLI ready, flushing ${this.inputBuffer.length} buffered messages`);

    // Flush buffered input
    for (const { type, data } of this.inputBuffer) {
      this.send(type, data);
    }
    this.inputBuffer = [];

    this.emit("cliReady");
  }

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
