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
  #awaitingReconnectSnapshot = false;
  #onReconnect = null;
  #onConnect = null;
  #onDisconnect = null;
  #onBinarySnapshot = null;
  #onFocusReportingChanged = null;
  #desiredSize = null; // { cols, rows }
  #resizeTimer = null;
  #destroyed = false;
  #connectGeneration = 0;

  constructor({ hubId, sessionUuid }) {
    this.#hubId = hubId;
    this.#sessionUuid = sessionUuid;
  }

  /**
   * Called by Restty via connectPty(). Acquires the TerminalConnection
   * (subscribing to the CLI's terminal channel) and wires up events.
   */
  async connect(options) {
    if (this.#destroyed) return;
    const generation = ++this.#connectGeneration;
    const requestedSize = this.#connectSize(options);
    const termKey = TerminalConnection.key(this.#hubId, this.#sessionUuid);
    const existingConn = this.#terminalConn ?? HubConnectionManager.get(termKey);
    const hadSubscription = existingConn?.hasSubscription?.() ?? false;
    this.disconnect();
    this.#desiredSize = requestedSize;
    this.#callbacks = options.callbacks;
    console.debug(
      `[WebRtcPtyTransport] connect start hub=${this.#hubId} session=${this.#sessionUuid} size=${requestedSize.cols}x${requestedSize.rows}`,
    );

    let terminalConn = this.#terminalConn;
    if (!terminalConn) {
      terminalConn = await HubConnectionManager.acquire(
        TerminalConnection,
        termKey,
        {
          hubId: this.#hubId,
          sessionUuid: this.#sessionUuid,
          rows: requestedSize.rows,
          cols: requestedSize.cols,
        },
      );
    }

    if (this.#destroyed || generation !== this.#connectGeneration) {
      terminalConn?.release?.();
      return;
    }

    this.#terminalConn = terminalConn;
    this.#awaitingReconnectSnapshot = hadSubscription;
    this.#wireEvents();
    await this.#terminalConn.sendResize(requestedSize.cols, requestedSize.rows);
    if (hadSubscription) {
      await this.#terminalConn.requestSnapshot(requestedSize);
    }
    console.debug(
      `[WebRtcPtyTransport] connect ready hub=${this.#hubId} session=${this.#sessionUuid}`,
    );
  }

  disconnect() {
    this.#clearResizeTimer();
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

  sendFocusChanged(focused) {
    if (!this.#terminalConn?.isConnected()) return false;
    this.#terminalConn.sendFocusChanged(focused);
    return true;
  }

  sendFile(data, filename) {
    if (!this.#terminalConn?.isConnected()) return false;
    this.#terminalConn.sendFile(data, filename);
    return true;
  }

  resize(cols, rows) {
    this.#desiredSize = { cols, rows };
    if (!this.#terminalConn?.isConnected()) return true;

    this.#clearResizeTimer();

    this.#resizeTimer = setTimeout(() => {
      const size = this.#desiredSize;
      this.#resizeTimer = null;
      if (!size || !this.#terminalConn?.isConnected()) return;
      this.#terminalConn.sendResize(size.cols, size.rows);
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
    this.#destroyed = true;
    this.#connectGeneration++;
    this.disconnect();
    this.#desiredSize = null;
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

  #connectSize(options) {
    if (this.#terminalConn && options?.cols && options?.rows) {
      return {
        cols: options.cols,
        rows: options.rows,
      };
    }
    return this.#desiredSize ?? {
      cols: options.cols,
      rows: options.rows,
    };
  }

  #clearResizeTimer() {
    if (!this.#resizeTimer) return;
    clearTimeout(this.#resizeTimer);
    this.#resizeTimer = null;
  }
}
