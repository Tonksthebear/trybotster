import bridge from "workers/bridge";
import { ensureMatrixReady } from "matrix/bundle";
import { observeBrowserSocketState } from "transport/hub_signaling_client";
import { ConnectionState, BrowserStatus, CliStatus, ConnectionMode } from "connections/constants";

export { ConnectionState, BrowserStatus, CliStatus, ConnectionMode };

const TAB_ID = crypto.randomUUID();
const RECONNECT_DELAY_MS = 1000;
const SESSION_TIMEOUT_MS = 5000;

const HEALTH_STATUS_MAP = {
  offline: CliStatus.OFFLINE,
  online: CliStatus.ONLINE,
  notified: CliStatus.ONLINE,
  connecting: CliStatus.ONLINE,
  connected: CliStatus.ONLINE,
  disconnected: CliStatus.OFFLINE,
};

export class HubRoute {
  static tabId = TAB_ID;

  #hubConnected = false;
  #listenersBound = false;
  #subscriptionListeners = [];
  #unsubscribers = [];
  #signalingConnected = false;
  #subscriptionPending = null;
  #sessionPending = null;
  #connectPending = null;
  #reconnectTimer = null;
  #destroyed = false;
  #browserSocketObserverCleanup = null;

  constructor(key, options = {}, manager) {
    this.key = key;
    this.options = options;
    this.manager = manager;

    this.subscriptionId = null;
    this.identityKey = null;
    this.browserIdentity = null;
    this.state = ConnectionState.DISCONNECTED;
    this.browserSocketState = "disconnected";
    this.cliStatus = CliStatus.UNKNOWN;
    this.connectionMode = ConnectionMode.UNKNOWN;
    this.errorCode = null;
    this.errorReason = null;
    this.lastError = null;
    this.subscribers = new Map();
  }

  async initialize() {
    if (this.#destroyed) return;

    this.#setState(ConnectionState.LOADING);
    await this.#ensureMatrix();
    await this.#refreshIdentity();
    this.#bindBridgeListeners();
    await this.#connectSignaling();
    await this.#ensureConnected();
  }

  async reacquire() {
    if (this.#destroyed) return;

    if (!this.#listenersBound) {
      this.#bindBridgeListeners();
    }

    await this.#refreshIdentity();
    await this.#connectSignaling();
    await this.#ensureConnected();
  }

  destroy() {
    this.#destroyed = true;
    this.#clearReconnectTimer();
    this.#clearSubscription();
    this.#browserSocketObserverCleanup?.();
    this.#browserSocketObserverCleanup = null;

    for (const unsubscribe of this.#unsubscribers) {
      unsubscribe();
    }
    this.#unsubscribers = [];
    this.#listenersBound = false;

    this.subscriptionId = null;
    this.identityKey = null;
    this.browserIdentity = null;
    this.#hubConnected = false;
    this.#signalingConnected = false;
    this.browserSocketState = "disconnected";
    this.cliStatus = CliStatus.UNKNOWN;
    this.connectionMode = ConnectionMode.UNKNOWN;
    this.state = ConnectionState.DISCONNECTED;

    const hubId = this.getHubId();
    if (hubId && !this.manager.hasActiveConnectionForHub(hubId)) {
      bridge.send("disconnect", { hubId }).catch(() => {});
    }

    this.emit("destroyed");
    this.subscribers.clear();
  }

  release() {
    this.manager.release(this.key);
  }

  notifyIdle() {
    this.#clearReconnectTimer();
    this.#clearSubscription();

    const hubId = this.getHubId();
    if (!hubId) return;
    if (this.manager.hasActiveConnectionForHub(hubId)) return;

    bridge.send("disconnect", { hubId }).catch(() => {});
  }

  on(event, callback) {
    if (!this.subscribers.has(event)) {
      this.subscribers.set(event, new Set());
    }
    this.subscribers.get(event).add(callback);
    return () => this.off(event, callback);
  }

  off(event, callback) {
    this.subscribers.get(event)?.delete(callback);
  }

  emit(event, data) {
    const callbacks = this.subscribers.get(event);
    if (!callbacks) return;

    for (const callback of callbacks) {
      try {
        callback(data);
      } catch (error) {
        console.error(`[${this.constructor.name}] Event handler error:`, error);
      }
    }
  }

  getHubId() {
    return this.options.hubId;
  }

  getError() {
    return this.errorReason;
  }

  isConnected() {
    return this.state === ConnectionState.CONNECTED;
  }

  isHubConnected() {
    return this.#hubConnected;
  }

  processMessage(message) {
    if (!message?.type) return false;

    if (message.type === "connected") {
      this.#sendEncrypted({ type: "ack", timestamp: Date.now() }).catch(() => {});
      return true;
    }

    if (message.type === "ack") {
      return true;
    }

    if (message.type === "dc_ping") {
      this.send("dc_pong").catch(() => {});
      return true;
    }

    if (message.type === "dc_pong") {
      return true;
    }

    if (message.type === "cli_disconnected") {
      this.#clearSubscription();
      if (this.state === ConnectionState.CONNECTED) {
        this.emit("disconnected", this);
      }
      this.#setState(ConnectionState.CLI_DISCONNECTED);
      return true;
    }

    return false;
  }

  async send(type, data = {}) {
    await this.#ensureConnected();
    if (!this.subscriptionId) return false;

    try {
      await this.#sendEncrypted({ type, ...data });
      return true;
    } catch (error) {
      console.error(`[${this.constructor.name}] Send failed:`, error);
      this.#scheduleReconnect();
      return false;
    }
  }

  async sendBinaryPty(data) {
    await this.#ensureConnected();
    if (!this.subscriptionId) return false;

    try {
      await bridge.send("sendPtyInput", {
        hubId: this.getHubId(),
        subscriptionId: this.subscriptionId,
        data,
      });
      return true;
    } catch (error) {
      console.error(`[${this.constructor.name}] PTY send failed:`, error);
      this.#scheduleReconnect();
      return false;
    }
  }

  async sendBinaryFile(data, filename) {
    await this.#ensureConnected();
    if (!this.subscriptionId) return false;

    try {
      await bridge.send("sendFileInput", {
        hubId: this.getHubId(),
        subscriptionId: this.subscriptionId,
        data,
        filename,
      });
      return true;
    } catch (error) {
      console.error(`[${this.constructor.name}] File send failed:`, error);
      this.#scheduleReconnect();
      return false;
    }
  }

  async #ensureMatrix() {
    const cryptoWorkerUrl = document.querySelector('meta[name="crypto-worker-url"]')?.content;
    const wasmJsUrl = document.querySelector('meta[name="crypto-wasm-js-url"]')?.content;
    const wasmBinaryUrl = document.querySelector('meta[name="crypto-wasm-binary-url"]')?.content;
    await ensureMatrixReady(cryptoWorkerUrl, wasmJsUrl, wasmBinaryUrl);
  }

  async #refreshIdentity() {
    const hubId = this.getHubId();
    const { hasPairing } = await bridge.hasPairing(hubId);

    if (!hasPairing) {
      this.identityKey = null;
      this.browserIdentity = `anon:${HubRoute.tabId}`;
      this.errorCode = "unpaired";
      this.errorReason = "Scan connection code";
      this.lastError = this.errorReason;
      this.emit("error", { reason: this.errorCode, message: this.errorReason });
      return;
    }

    const keyResult = await bridge.getIdentityKey(hubId);
    this.identityKey = keyResult.identityKey;
    this.browserIdentity = `${this.identityKey}:${HubRoute.tabId}`;

    if (this.errorCode === "unpaired" || this.errorCode === "session_invalid") {
      this.errorCode = null;
      this.errorReason = null;
      this.lastError = null;
    }
  }

  #bindBridgeListeners() {
    if (this.#listenersBound) return;
    this.#listenersBound = true;

    const hubId = this.getHubId();

    // Route gating must use the same browser-socket source as the UI badge.
    // Otherwise the page can look "browser connected" while ensureConnected()
    // still believes signaling is unavailable and never attempts WebRTC.
    observeBrowserSocketState((state) => {
      if (this.#destroyed) return;
      this.#setBrowserSocketState(state);
      if (state === "connected") {
        this.#ensureConnected().catch(() => {});
      } else if (!this.isConnected()) {
        this.#setState(ConnectionState.DISCONNECTED);
      }
    }).then((cleanup) => {
      if (this.#destroyed) {
        cleanup?.();
        return;
      }
      this.#browserSocketObserverCleanup = cleanup;
    }).catch((error) => {
      console.error(`[${this.constructor.name}] Failed to observe browser socket state:`, error);
    });

    this.#unsubscribers.push(
      bridge.on("signaling:state", (event) => {
        if (event.hubId !== hubId) return;
        this.#signalingConnected = event.state === "connected";
        if (this.#signalingConnected) {
          this.#ensureConnected().catch(() => {});
        }
      }),
      bridge.on("health", (event) => {
        if (event.hubId !== hubId) return;
        // A live health payload can only arrive over an active signaling
        // subscription, so treat it as proof that signaling is up even if the
        // ActionCable "connected" callback was missed or delayed.
        this.#signalingConnected = true;
        const status = HEALTH_STATUS_MAP[event.cli] || CliStatus.UNKNOWN;
        this.#setCliStatus(status);
        if (status === CliStatus.ONLINE) {
          this.#ensureConnected().catch(() => {});
        } else if (!this.isConnected()) {
          this.#setState(ConnectionState.CLI_DISCONNECTED);
        }
      }),
      bridge.on("connection:state", (event) => {
        if (event.hubId !== hubId) return;
        if (event.state === "connected") {
          this.#clearReconnectTimer();
          if (event.mode) this.#setConnectionMode(event.mode);
          this.#ensureSubscribed().catch((error) => {
            console.error(`[${this.constructor.name}] Subscribe failed after peer connect:`, error);
            this.#scheduleReconnect();
          });
        } else {
          const wasConnected = this.state === ConnectionState.CONNECTED;
          this.#clearSubscription();
          this.#setState(ConnectionState.DISCONNECTED);
          if (wasConnected) {
            this.emit("disconnected", this);
          }
          if (this.browserSocketState === "connected" && this.cliStatus === CliStatus.ONLINE) {
            this.#scheduleReconnect();
          }
        }
      }),
      bridge.on("connection:mode", (event) => {
        if (event.hubId !== hubId) return;
        this.#setConnectionMode(event.mode || ConnectionMode.UNKNOWN);
      }),
      bridge.on("session:invalid", (event) => {
        if (event.hubId !== hubId) return;
        this.errorCode = "session_invalid";
        this.errorReason = event.message || "Session invalid";
        this.lastError = this.errorReason;
        this.#clearSubscription();
        this.#setState(ConnectionState.ERROR);
        this.emit("error", { reason: this.errorCode, message: this.errorReason });
      }),
      bridge.on("session:refreshed", (event) => {
        if (event.hubId !== hubId) return;
        if (this.errorCode === "session_invalid") {
          this.errorCode = null;
          this.errorReason = null;
          this.lastError = null;
          this.#setState(ConnectionState.DISCONNECTED);
        }
        this.#ensureConnected().catch(() => {});
      }),
    );
  }

  async #connectSignaling() {
    if (this.#destroyed) return;

    this.#setState(ConnectionState.CONNECTING);

    const result = await bridge.send("connectSignaling", {
      hubId: this.getHubId(),
      browserIdentity: this.browserIdentity,
    });

    this.#hubConnected = true;
    this.#setBrowserSocketState(result?.browserSocketState || "disconnected");

    if (result?.state === "connected") {
      this.#signalingConnected = true;
      this.#setConnectionMode(result?.mode || this.connectionMode);
    }
  }

  async #ensureConnected() {
    if (this.#destroyed) return;
    if (this.errorCode === "session_invalid") return;
    if (!this.#hubConnected) return;
    if (!this.identityKey) return;
    // WebRTC is gated only by the raw browser->Rails socket plus live hub
    // health. If both are ready, attempt the peer even if prior callbacks were
    // delayed or missed.
    if (this.browserSocketState !== "connected") return;
    if (this.cliStatus !== CliStatus.ONLINE) return;

    if (this.subscriptionId) {
      this.#setState(ConnectionState.CONNECTED);
      return;
    }

    if (this.#connectPending) {
      return this.#connectPending;
    }

    this.#connectPending = (async () => {
      this.#setState(ConnectionState.CONNECTING);
      await this.#ensureActiveSession();
      await bridge.send("connectPeer", { hubId: this.getHubId() });
      // Reused shared peers may already be connected and therefore not emit a
      // fresh connection:state event for this route. Ensure the route's
      // subscription exists even when connectPeer() is effectively a no-op.
      await this.#ensureSubscribed();
    })();

    try {
      await this.#connectPending;
    } finally {
      this.#connectPending = null;
    }
  }

  async #ensureActiveSession() {
    if (this.#sessionPending) {
      return this.#sessionPending;
    }

    this.#sessionPending = (async () => {
      const hubId = this.getHubId();
      const hasSession = await bridge.hasSession(hubId, this.browserIdentity);
      if (hasSession.hasSession) return true;

      const bundleArrived = await bridge.send("awaitFreshBundle", {
        hubId,
        timeoutMs: 750,
      });
      if (!bundleArrived?.refreshed) {
        await bridge.send("requestFreshBundle", { hubId });
      }

      const refreshed = await bridge.hasSession(hubId, this.browserIdentity);
      if (!refreshed.hasSession) {
        throw new Error("No active session");
      }
      return true;
    })();

    try {
      return await this.#sessionPending;
    } finally {
      this.#sessionPending = null;
    }
  }

  async #ensureSubscribed() {
    if (this.subscriptionId) {
      this.#setState(ConnectionState.CONNECTED);
      return;
    }

    if (this.#subscriptionPending) {
      return this.#subscriptionPending;
    }

    this.subscriptionId = this.computeSubscriptionId();
    this.#setupSubscriptionListeners();

    this.#subscriptionPending = bridge.send("subscribe", {
      hubId: this.getHubId(),
      channel: this.channelName(),
      params: this.channelParams(),
      subscriptionId: this.subscriptionId,
    }).then(() => {
      this.#setState(ConnectionState.CONNECTED);
      this.emit("connected", this);
    }).catch((error) => {
      this.#clearSubscription();
      throw error;
    }).finally(() => {
      this.#subscriptionPending = null;
    });

    return this.#subscriptionPending;
  }

  #setupSubscriptionListeners() {
    this.#clearSubscriptionListeners();

    const subscriptionId = this.subscriptionId;
    if (!subscriptionId) return;

    this.#subscriptionListeners.push(
      bridge.onSubscriptionMessage(subscriptionId, (message) => {
        if (message instanceof Uint8Array) {
          this.handleMessage({ type: "raw_output", data: message });
          return;
        }

        if (this.processMessage(message)) return;
        this.handleMessage(message);
      }),
    );
  }

  #clearSubscriptionListeners() {
    for (const unsubscribe of this.#subscriptionListeners) {
      unsubscribe();
    }
    this.#subscriptionListeners = [];
  }

  #clearSubscription() {
    const subscriptionId = this.subscriptionId;
    this.subscriptionId = null;
    this.#clearSubscriptionListeners();
    if (subscriptionId) {
      bridge.clearSubscriptionListeners(subscriptionId);
      bridge.send("unsubscribe", { subscriptionId }).catch(() => {});
    }
  }

  async #sendEncrypted(message) {
    if (!this.subscriptionId) {
      throw new Error("No subscription");
    }

    const fullMessage = {
      subscriptionId: this.subscriptionId,
      ...message,
    };

    const jsonBytes = new TextEncoder().encode(JSON.stringify(fullMessage));
    const plaintext = new Uint8Array(1 + jsonBytes.length);
    plaintext[0] = 0x00;
    plaintext.set(jsonBytes, 1);

    const { data: encrypted } = await bridge.encryptBinary(this.getHubId(), plaintext);
    await bridge.send("sendEncrypted", { hubId: this.getHubId(), encrypted });
  }

  #scheduleReconnect() {
    if (this.#reconnectTimer || this.#destroyed) return;

    this.#reconnectTimer = setTimeout(() => {
      this.#reconnectTimer = null;
      this.#ensureConnected().catch(() => {});
    }, RECONNECT_DELAY_MS);
  }

  #clearReconnectTimer() {
    if (this.#reconnectTimer) {
      clearTimeout(this.#reconnectTimer);
      this.#reconnectTimer = null;
    }
  }

  #setState(nextState) {
    const prevState = this.state;
    if (nextState === prevState) return;

    this.state = nextState;
    if (nextState !== ConnectionState.ERROR) {
      this.lastError = this.errorReason;
    }

    const stateInfo = { state: nextState, prevState, error: this.errorReason };
    this.emit("stateChange", stateInfo);
    this.manager.notifySubscribers(this.key, stateInfo);
  }

  #setBrowserSocketState(nextState) {
    const prevState = this.browserSocketState;
    if (nextState === prevState) return;
    this.browserSocketState = nextState;
    this.emit("browserSocketStateChange", { status: nextState, prevStatus: prevState });
  }

  #setCliStatus(nextStatus) {
    const prevStatus = this.cliStatus;
    if (nextStatus === prevStatus) return;
    this.cliStatus = nextStatus;
    this.emit("cliStatusChange", { status: nextStatus, prevStatus });
  }

  #setConnectionMode(nextMode) {
    const prevMode = this.connectionMode;
    if (nextMode === prevMode) return;
    this.connectionMode = nextMode || ConnectionMode.UNKNOWN;
    this.emit("connectionModeChange", {
      mode: this.connectionMode,
      prevMode,
    });
  }

  channelName() {
    throw new Error("channelName() must be implemented by subclass");
  }

  computeSubscriptionId() {
    throw new Error("computeSubscriptionId() must be implemented by subclass");
  }

  channelParams() {
    return {};
  }

  handleMessage(message) {
    this.emit("message", message);
  }
}
