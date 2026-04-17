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
  #wasConnected = false;
  #awaitingReconnectSnapshot = false;
  #onReconnect = null;
  #onConnect = null;
  #onDisconnect = null;
  #onBinarySnapshot = null;
  #onFocusReportingChanged = null;
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
    this.disconnect();
    this.#callbacks = options.callbacks;
    console.debug(
      `[WebRtcPtyTransport] connect start hub=${this.#hubId} session=${this.#sessionUuid} size=${options.cols}x${options.rows}`,
    );

    const termKey = TerminalConnection.key(this.#hubId, this.#sessionUuid);
    const existingConn = HubConnectionManager.get(termKey);
    const hadSubscription = existingConn?.hasSubscription?.() ?? false;

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

    this.#awaitingReconnectSnapshot = hadSubscription;
    this.#wireEvents();
    if (hadSubscription) {
      await this.#terminalConn.requestSnapshot();
    }
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
    this.#awaitingReconnectSnapshot = false;
    this.#unsubscribers.forEach((unsub) => unsub());
    this.#unsubscribers = [];
    this.#callbacks = null;
  }

  sendInput(data) {
    if (!this.#terminalConn?.isConnected()) return false;
    this.#terminalConn.sendInput(data);
    return true;
  }

  sendColorProfile(colors) {
    if (!this.#terminalConn?.isConnected()) return false;
    this.#terminalConn.sendColorProfile(colors);
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
  set onBinarySnapshot(callback) { this.#onBinarySnapshot = callback; }
  set onFocusReportingChanged(callback) { this.#onFocusReportingChanged = callback; }

  destroy() {
    this.disconnect();
    this.#onReconnect = null;
    this.#onConnect = null;
    this.#onDisconnect = null;
    this.#onBinarySnapshot = null;
    this.#onFocusReportingChanged = null;
    this.#terminalConn?.release();
    this.#terminalConn = null;
  }

  #wireEvents() {
    // Pure passthrough — no batching. Restty/ghostty's VT parser maintains
    // its own state machine and handles partial sequences correctly.
    // Ghostty batches renders internally via its own frame scheduling,
    // so transport-level batching is unnecessary and can interfere.

    this.#unsubscribers.push(
      this.#terminalConn.onSnapshotStart(() => {
        if (!this.#awaitingReconnectSnapshot) return;
        this.#awaitingReconnectSnapshot = false;
        this.#onReconnect?.();
      }),
    );

    this.#unsubscribers.push(
      this.#terminalConn.onSnapshotComplete(() => {
        this.#awaitingReconnectSnapshot = false;
      }),
    );

    this.#unsubscribers.push(
      this.#terminalConn.onBinarySnapshot((data) => {
        console.debug(
          `[WebRtcPtyTransport] binary snapshot hub=${this.#hubId} session=${this.#sessionUuid} bytes=${data?.byteLength ?? 0}`,
        );
        this.#onBinarySnapshot?.(data);
      }),
    );

    this.#unsubscribers.push(
      this.#terminalConn.on("message", (message) => {
        if (message?.type === "focus_reporting_changed") {
          this.#onFocusReportingChanged?.(!!message.enabled);
        }
      }),
    );

    this.#unsubscribers.push(
      this.#terminalConn.onOutput((data) => {
        this.#callbacks?.onData?.(data);
      }),
    );

    this.#unsubscribers.push(
      this.#terminalConn.onConnected(() => {
        this.#wasConnected = true;
        this.#onConnect?.();
        this.#callbacks?.onConnect?.();
      }),
    );

    this.#unsubscribers.push(
      this.#terminalConn.onDisconnected(() => {
        this.#awaitingReconnectSnapshot = false;
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
