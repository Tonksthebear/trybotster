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
  // Backlog output that can arrive before transport wires onOutput().
  #earlyOutputBuffer = [];
  #earlyOutputBytes = 0;
  // Initial/recovery snapshot can arrive before transport wires snapshot listeners.
  // Keep the latest one and replay it atomically once onBinarySnapshot() attaches.
  #earlyBinarySnapshot = null;
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

    switch (message.type) {
      case "raw_output":
        // Raw bytes from CLI with prefix byte routing:
        //   0x00 = JSON control message
        //   0x01 = live PTY output (immediate passthrough)
        //   0x02 = opaque terminal snapshot blob
        if (message.data && message.data.length > 0) {
          const prefix = message.data[0];
          if (prefix === 0x01) {
            this.#emitOutput(message.data.slice(1));
          } else if (prefix === 0x02) {
            this.#handleSnapshot(message.data.slice(1));
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

  sendColorProfile(colors) {
    return this.send("terminal_color_profile", {
      session_uuid: this.sessionUuid,
      colors,
    });
  }

  sendResize(cols, rows) {
    // Keep local geometry in sync so requestSnapshot() sends current bounds.
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
    this.#earlyOutputBuffer = [];
    this.#earlyOutputBytes = 0;
    this.#earlyBinarySnapshot = null;
    super.destroy();
  }

  // ========== Snapshot handling ==========

  /**
   * Handle a complete snapshot (prefix 0x02).
   *
   * The payload is an opaque terminal snapshot blob exported by ghostty.
   * The hub routes it unchanged and the client imports it in one call.
   *
   * The snapshot is a single atomic message — no chunking, no reassembly.
   * WebRTC SCTP handles message fragmentation at the transport layer.
   */
  #handleSnapshot(data) {
    if (data.length === 0) return;

    const listenerCount = this.subscribers.get("binarySnapshot")?.size ?? 0;
    console.debug(
      `[TerminalConnection] Binary snapshot: ${data.byteLength} bytes listeners=${listenerCount}`,
    );
    const listeners = this.subscribers.get("binarySnapshot");
    if (listeners && listeners.size > 0) {
      this.#emitSnapshot(data);
      return;
    }

    console.debug(
      `[TerminalConnection] Buffering early binary snapshot: ${data.byteLength} bytes`,
    );
    this.#earlyBinarySnapshot = data;
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

  onBinarySnapshot(callback) {
    const unsubscribe = this.on("binarySnapshot", callback);
    this.#flushEarlyBinarySnapshot();
    return unsubscribe;
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

  #flushEarlyBinarySnapshot() {
    const data = this.#earlyBinarySnapshot;
    if (!data) return;
    console.debug(
      `[TerminalConnection] Flushing early binary snapshot: ${data.byteLength} bytes`,
    );
    this.#earlyBinarySnapshot = null;
    this.#emitSnapshot(data);
  }

  #emitSnapshot(data) {
    const listenerCount = this.subscribers.get("binarySnapshot")?.size ?? 0;
    console.debug(
      `[TerminalConnection] Emitting binary snapshot: ${data.byteLength} bytes listeners=${listenerCount}`,
    );
    this.emit("snapshotStart", { byteLength: data.byteLength });
    this.emit("binarySnapshot", data);
    this.emit("snapshotComplete", { byteLength: data.byteLength });
  }

  #outputByteLength(data) {
    if (data instanceof Uint8Array) return data.byteLength;
    if (typeof data === "string") return data.length;
    return 0;
  }
}
