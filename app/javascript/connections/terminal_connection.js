/**
 * TerminalConnection - Typed wrapper for terminal data plane.
 *
 * Manages:
 *   - PTY I/O (input/output streams)
 *   - Terminal resize events
 *   - PTY channel switching between agents
 *
 * Events:
 *   - connected - Channel established
 *   - disconnected - Channel closed
 *   - stateChange - { state, prevState, error }
 *   - error - { reason, message }
 *   - output - PTY output data (string)
 *   - ptySwitch - { agentIndex, ptyIndex }
 *
 * Key differences from HubConnection:
 *   - No handshake required
 *   - Key includes agent/pty indices: "hubId:agentIndex:ptyIndex"
 *   - Lower-level I/O (raw terminal data)
 *
 * Usage:
 *   const key = `${hubId}:${agentIndex}:${ptyIndex}`;
 *   const term = await ConnectionManager.acquire(TerminalConnection, key, {
 *     hubId, agentIndex, ptyIndex
 *   });
 *   term.on("output", (data) => xterm.write(data));
 *   term.sendInput("ls -la\n");
 */

import { Connection, ConnectionState } from "connections/connection";

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
    return {
      hub_id: this.getHubId(),
      agent_index: this.agentIndex,
      pty_index: this.ptyIndex,
      browser_identity: this.identityKey,
    };
  }

  /**
   * Terminal connections are connected as soon as channel opens.
   * No handshake required.
   */
  async initialize() {
    await super.initialize();

    // Emit connected immediately on successful channel open
    if (this.state === ConnectionState.CONNECTED) {
      this.emit("connected", this);
    }
  }

  /**
   * Handle terminal-specific messages.
   */
  handleMessage(message) {
    switch (message.type) {
      case "output":
        // PTY output data
        this.emit("output", message.data);
        break;

      case "pty_channel_switched":
        this.emit("ptySwitch", {
          agentIndex: message.agent_index,
          ptyIndex: message.pty_index,
        });
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
        // Emit as generic message
        this.emit("message", message);
    }
  }

  // ========== Terminal Commands ==========

  /**
   * Send input to the PTY.
   * @param {string} data - Raw terminal input
   */
  sendInput(data) {
    return this.send("input", { data });
  }

  /**
   * Send resize event to the PTY.
   * @param {number} cols - Number of columns
   * @param {number} rows - Number of rows
   */
  sendResize(cols, rows) {
    return this.send("resize", { cols, rows });
  }

  // ========== Getters ==========

  /**
   * Get current agent index.
   */
  getAgentIndex() {
    return this.agentIndex;
  }

  /**
   * Get current PTY index (0=CLI, 1=Server, etc).
   */
  getPtyIndex() {
    return this.ptyIndex;
  }

  // ========== Convenience event helpers ==========

  /**
   * Subscribe to PTY output.
   */
  onOutput(callback) {
    return this.on("output", callback);
  }

  /**
   * Subscribe to connection established.
   */
  onConnected(callback) {
    if (this.isConnected()) {
      callback(this);
    }
    return this.on("connected", callback);
  }

  /**
   * Subscribe to disconnection.
   */
  onDisconnected(callback) {
    return this.on("disconnected", callback);
  }

  /**
   * Subscribe to state changes.
   */
  onStateChange(callback) {
    callback({ state: this.state, prevState: null, error: this.errorReason });
    return this.on("stateChange", callback);
  }

  /**
   * Subscribe to errors.
   */
  onError(callback) {
    return this.on("error", callback);
  }

  /**
   * Subscribe to PTY switch events.
   */
  onPtySwitch(callback) {
    return this.on("ptySwitch", callback);
  }

  // ========== Static helper ==========

  /**
   * Generate a connection key from components.
   * @param {string} hubId
   * @param {number} agentIndex
   * @param {number} ptyIndex
   * @returns {string}
   */
  static key(hubId, agentIndex, ptyIndex = 0) {
    return `terminal:${hubId}:${agentIndex}:${ptyIndex}`;
  }
}
