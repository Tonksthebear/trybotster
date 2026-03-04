/**
 * WebRTC PTY Transport for Restty
 *
 * Implements Restty's PtyTransport interface backed by our TerminalConnection.
 * Bridges E2E-encrypted WebRTC DataChannel I/O into Restty's native transport
 * layer for SSH-like terminal integration.
 *
 * Lifecycle:
 *   constructor({ hubId, sessionUuid }) — stores params, no connection
 *   connect(options)  — called by Restty via connectPty(), acquires TerminalConnection
 *   disconnect()      — unsubscribes events
 *   destroy()         — releases TerminalConnection
 *
 * Data flow:
 *   CLI PTY → WebRTC DataChannel → TerminalConnection.onOutput → onData → Restty WASM
 *   Restty input → sendInput() → TerminalConnection.sendInput → WebRTC → CLI PTY
 */
import { HubConnectionManager, TerminalConnection } from "connections";

export class WebRtcPtyTransport {
  static #RESIZE_DEBOUNCE_MS = 30;
  #hubId;
  #sessionUuid;
  #terminalConn = null;
  #callbacks = null;
  #unsubscribers = [];
  #decoder = new TextDecoder();
  #wasConnected = false;
  #onReconnect = null;
  #onConnect = null;
  #onDisconnect = null;
  #pendingResize = null; // { cols, rows }
  #pendingResizeTimer = null;

  constructor({ hubId, sessionUuid }) {
    this.#hubId = hubId;
    this.#sessionUuid = sessionUuid;
  }

  /**
   * Called by Restty via connectPty(). Acquires the TerminalConnection
   * (subscribing to the CLI's terminal channel) and wires up events.
   */
  async connect(options) {
    this.#callbacks = options.callbacks;
    console.debug(
      `[WebRtcPtyTransport] connect start hub=${this.#hubId} session=${this.#sessionUuid} size=${options.cols}x${options.rows}`,
    );

    const termKey = TerminalConnection.key(this.#hubId, this.#sessionUuid);

    this.#terminalConn = await HubConnectionManager.acquire(
      TerminalConnection,
      termKey,
      {
        hubId: this.#hubId,
        sessionUuid: this.#sessionUuid,
        rows: options.rows,
        cols: options.cols,
      },
    );

    // Restty is reconstructed on view teardown/navigation. Force a fresh
    // terminal subscribe so CLI replays snapshot/scrollback for this mount.
    await this.#terminalConn.subscribe({ force: true });

    this.#wireEvents();
    console.debug(
      `[WebRtcPtyTransport] connect ready hub=${this.#hubId} session=${this.#sessionUuid}`,
    );
  }

  disconnect() {
    if (this.#pendingResizeTimer) {
      clearTimeout(this.#pendingResizeTimer);
      this.#pendingResizeTimer = null;
      this.#pendingResize = null;
    }
    this.#unsubscribers.forEach((unsub) => unsub());
    this.#unsubscribers = [];
    this.#callbacks = null;
  }

  sendInput(data) {
    if (!this.#terminalConn?.isConnected()) return false;
    this.#terminalConn.sendInput(data);
    return true;
  }

  sendFile(data, filename) {
    if (!this.#terminalConn?.isConnected()) return false;
    this.#terminalConn.sendFile(data, filename);
    return true;
  }

  resize(cols, rows) {
    if (!this.#terminalConn?.isConnected()) return false;
    this.#pendingResize = { cols, rows };

    if (this.#pendingResizeTimer) {
      clearTimeout(this.#pendingResizeTimer);
    }

    this.#pendingResizeTimer = setTimeout(() => {
      const pending = this.#pendingResize;
      this.#pendingResizeTimer = null;
      this.#pendingResize = null;
      if (!pending || !this.#terminalConn?.isConnected()) return;
      this.#terminalConn.sendResize(pending.cols, pending.rows);
    }, WebRtcPtyTransport.#RESIZE_DEBOUNCE_MS);
    return true;
  }

  isConnected() {
    return this.#terminalConn?.isConnected() ?? false;
  }

  /**
   * Register a callback for reconnection events (DataChannel restored after drop).
   * Fires before the CLI sends fresh snapshot data, allowing consumers to
   * clear local state (e.g., Restty scrollback) before the snapshot repopulates it.
   */
  set onReconnect(callback) { this.#onReconnect = callback; }
  set onConnect(callback) { this.#onConnect = callback; }
  set onDisconnect(callback) { this.#onDisconnect = callback; }

  destroy() {
    this.disconnect();
    this.#onReconnect = null;
    this.#onConnect = null;
    this.#onDisconnect = null;
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
        if (this.#wasConnected) this.#onReconnect?.();
        this.#wasConnected = true;
        this.#onConnect?.();
        this.#callbacks?.onConnect?.();
      }),
    );

    this.#unsubscribers.push(
      this.#terminalConn.onDisconnected(() => {
        this.#onDisconnect?.();
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
