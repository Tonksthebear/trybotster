/**
 * WebRTC PTY Transport for Restty
 *
 * Implements Restty's PtyTransport interface backed by our TerminalConnection.
 * Bridges E2E-encrypted WebRTC DataChannel I/O into Restty's native transport
 * layer for SSH-like terminal integration.
 *
 * Lifecycle:
 *   constructor({ hubId, agentIndex, ptyIndex }) — stores params, no connection
 *   connect(options)  — called by Restty via connectPty(), acquires TerminalConnection
 *   disconnect()      — unsubscribes events
 *   destroy()         — releases TerminalConnection
 *
 * Data flow:
 *   CLI PTY → WebRTC DataChannel → TerminalConnection.onOutput → onData → Restty WASM
 *   Restty input → sendInput() → TerminalConnection.sendInput → WebRTC → CLI PTY
 */
import { ConnectionManager, TerminalConnection } from "connections";

export class WebRtcPtyTransport {
  #hubId;
  #agentIndex;
  #ptyIndex;
  #terminalConn = null;
  #callbacks = null;
  #unsubscribers = [];
  #decoder = new TextDecoder();

  constructor({ hubId, agentIndex, ptyIndex }) {
    this.#hubId = hubId;
    this.#agentIndex = agentIndex;
    this.#ptyIndex = ptyIndex;
  }

  /**
   * Called by Restty via connectPty(). Acquires the TerminalConnection
   * (subscribing to the CLI's terminal channel) and wires up events.
   */
  async connect(options) {
    this.#callbacks = options.callbacks;

    const termKey = TerminalConnection.key(
      this.#hubId,
      this.#agentIndex,
      this.#ptyIndex,
    );

    this.#terminalConn = await ConnectionManager.acquire(
      TerminalConnection,
      termKey,
      {
        hubId: this.#hubId,
        agentIndex: this.#agentIndex,
        ptyIndex: this.#ptyIndex,
        rows: options.rows || 24,
        cols: options.cols || 80,
      },
    );

    this.#wireEvents();
  }

  disconnect() {
    this.#unsubscribers.forEach((unsub) => unsub());
    this.#unsubscribers = [];
    this.#callbacks = null;
  }

  sendInput(data) {
    if (!this.#terminalConn?.isConnected()) return false;
    this.#terminalConn.sendInput(data);
    return true;
  }

  resize(cols, rows) {
    if (!this.#terminalConn?.isConnected()) return false;
    this.#terminalConn.sendResize(cols, rows);
    return true;
  }

  isConnected() {
    return this.#terminalConn?.isConnected() ?? false;
  }

  destroy() {
    this.disconnect();
    this.#terminalConn?.release();
    this.#terminalConn = null;
  }

  #wireEvents() {
    // Pure passthrough — no batching. Restty/ghostty's VT parser maintains
    // its own state machine and handles partial sequences correctly.
    // Ghostty batches renders internally via its own frame scheduling,
    // so transport-level batching is unnecessary and can interfere.

    this.#unsubscribers.push(
      this.#terminalConn.onOutput((data) => {
        const text = data instanceof Uint8Array
          ? this.#decoder.decode(data, { stream: true })
          : data;
        this.#callbacks?.onData?.(text);
      }),
    );

    this.#unsubscribers.push(
      this.#terminalConn.onConnected(() => {
        this.#callbacks?.onConnect?.();
      }),
    );

    this.#unsubscribers.push(
      this.#terminalConn.onDisconnected(() => {
        this.#callbacks?.onDisconnect?.();
      }),
    );

    this.#unsubscribers.push(
      this.#terminalConn.onError((err) => {
        this.#callbacks?.onError?.(err.message || "Connection error");
      }),
    );

    if (this.#terminalConn.isConnected()) {
      this.#callbacks?.onConnect?.();
    }
  }
}
