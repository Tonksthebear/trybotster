import { Controller } from "@hotwired/stimulus";
import { ConnectionManager, HubConnection, TerminalConnection, BrowserStatus, CliStatus, ConnectionMode } from "connections";

/**
 * Connection Status Controller
 *
 * Shows connection status in three sections:
 * [Browser] | [Connection] | [Hub]
 *
 * Browser: Signaling/ActionCable connection status
 *   - disconnected (gray)
 *   - connecting (amber, pulsing)
 *   - connected (green)
 *   - error (red)
 *
 * Connection: WebRTC data channel status (4 states)
 *   - disconnected (gray, X icon) - not connected
 *   - connecting (amber, spinning) - establishing, renegotiating, or detecting mode
 *   - direct (green, bolt icon) - P2P connection
 *   - relay (blue, cloud icon) - through TURN server
 *
 * Hub: CLI health status from ActionCable health messages
 *   - offline (gray)
 *   - connecting (amber, pulsing)
 *   - online (green)
 *   - error (red)
 */
export default class extends Controller {
  static values = {
    hubId: String,
    type: { type: String, default: "hub" },
    agentIndex: { type: Number, default: 0 },
    ptyIndex: { type: Number, default: 0 },
  };

  static targets = ["browserSection", "connectionSection", "hubSection"];

  #modePolling = false;

  connect() {
    if (!this.hubIdValue) return;
    this.unsubscribers = [];
    this.#modePolling = false;
    this.#acquireConnection();
  }

  disconnect() {
    this.unsubscribers?.forEach(unsub => unsub());
    this.unsubscribers = [];
    this.connection?.release();
    this.connection = null;
  }

  // ========== Private ==========

  async #acquireConnection() {
    // HTML defaults to "connecting" for both sections - keep that state
    // while the async connection work happens
    this.#setBrowserStatus("connecting");
    this.#setConnectionState("connecting");

    try {
      const ConnectionClass = this.#getConnectionClass();
      const key = this.#getConnectionKey();
      const options = this.#getConnectionOptions();

      this.connection = await ConnectionManager.acquire(ConnectionClass, key, options);

      // Listen for browser status changes
      this.unsubscribers.push(
        this.connection.on("browserStatusChange", ({ status }) => {
          this.#handleBrowserStatusChange(status);
        })
      );

      // Listen for connection mode changes (P2P vs relay)
      this.unsubscribers.push(
        this.connection.on("connectionModeChange", ({ mode }) => {
          this.#handleConnectionModeChange(mode);
        })
      );

      // Listen for health changes (CLI status)
      this.unsubscribers.push(
        this.connection.on("healthChange", ({ browser, cli }) => {
          this.#handleHealthChange(browser, cli);
        })
      );

      // Listen for connected event
      this.unsubscribers.push(
        this.connection.on("connected", () => {
          this.#updateConnectionState();
        })
      );

      // Listen for disconnected event
      this.unsubscribers.push(
        this.connection.on("disconnected", () => {
          this.#setConnectionState("disconnected");
        })
      );

      // Listen for errors - crypto issues show "Scan Code", others show "disconnected"
      this.unsubscribers.push(
        this.connection.on("error", ({ reason }) => {
          if (reason === "session_invalid" || reason === "unpaired") {
            this.#setConnectionState("unpaired");
          } else {
            this.#setConnectionState("disconnected");
          }
        })
      );

      // Apply current browser status — browser is already green
      // (WebSocket connected via signaling), connection stays orange.
      // Don't apply health here — CLI status is likely UNKNOWN and would
      // regress the server-rendered hub status to "offline". The health
      // event from ActionCable will update the hub section when it arrives.
      this.#handleBrowserStatusChange(this.connection.browserStatus);

      // Sync initial connection state — error may have been set during initialize()
      // (before event listeners were attached), so check errorCode directly.
      const errCode = this.connection.errorCode
      if (errCode === "unpaired" || errCode === "session_invalid") {
        this.#setConnectionState("unpaired");
      } else if (this.connection.isConnected()) {
        // Already connected (reacquired after Turbo nav), sync state immediately.
        this.#updateConnectionState();
        this.#handleHealthChange(this.connection.browserStatus, this.connection.cliStatus);
      }
    } catch (error) {
      console.error("[ConnectionStatus] Failed to acquire connection:", error);
      this.#setBrowserStatus("error");
      this.#setConnectionState("disconnected");
    }
  }

  #getConnectionClass() {
    switch (this.typeValue) {
      case "terminal":
      case "preview":
        return TerminalConnection;
      default:
        return HubConnection;
    }
  }

  #getConnectionKey() {
    switch (this.typeValue) {
      case "terminal":
      case "preview":
        return TerminalConnection.key(this.hubIdValue, this.agentIndexValue, this.ptyIndexValue);
      default:
        return this.hubIdValue;
    }
  }

  #getConnectionOptions() {
    const base = { hubId: this.hubIdValue };

    switch (this.typeValue) {
      case "terminal":
      case "preview":
        return { ...base, agentIndex: this.agentIndexValue, ptyIndex: this.ptyIndexValue };
      default:
        return base;
    }
  }

  // ========== Status Handlers ==========

  #handleBrowserStatusChange(status) {
    const statusMap = {
      [BrowserStatus.DISCONNECTED]: "disconnected",
      [BrowserStatus.CONNECTING]: "connecting",
      [BrowserStatus.SUBSCRIBING]: "connecting",
      [BrowserStatus.SUBSCRIBED]: "connected",
      [BrowserStatus.ERROR]: "error",
    };
    this.#setBrowserStatus(statusMap[status] || "disconnected");
  }

  #handleConnectionModeChange(mode) {
    if (this.connection?.isConnected()) {
      this.#updateConnectionState();
    }
  }

  #handleHealthChange(browser, cli) {
    // Update browser status
    this.#handleBrowserStatusChange(browser);

    // Update hub (CLI) status
    // Hub status = is CLI online/heartbeating to Rails? (separate from WebRTC E2E state)
    // ONLINE/NOTIFIED/CONNECTING/CONNECTED all mean CLI is reachable via Rails
    const hubStatusMap = {
      [CliStatus.UNKNOWN]: "offline",
      [CliStatus.OFFLINE]: "offline",
      [CliStatus.ONLINE]: "online",
      [CliStatus.NOTIFIED]: "online",
      [CliStatus.CONNECTING]: "online",
      [CliStatus.CONNECTED]: "online",
      [CliStatus.DISCONNECTED]: "offline",
    };
    this.#setHubStatus(hubStatusMap[cli] || "offline");

    // Don't let health events overwrite scan-code state — user must re-pair first
    const err = this.connection?.errorCode
    if (err === "unpaired" || err === "session_invalid") return;

    // Update connection state based on overall state
    if (this.connection?.isConnected()) {
      this.#updateConnectionState();
    } else if (browser === BrowserStatus.SUBSCRIBED && cli === CliStatus.CONNECTED) {
      this.#setConnectionState("connecting");
    } else if (browser === BrowserStatus.SUBSCRIBED) {
      // Browser ready but CLI not connected yet
      this.#setConnectionState("connecting");
    }
  }

  /**
   * Update connection state based on current connection status and mode.
   * 4 states: disconnected, connecting, direct, relay
   * Shows "connecting" while detecting mode, then switches to direct/relay.
   */
  #updateConnectionState() {
    if (!this.connection?.isConnected()) {
      this.#setConnectionState("connecting");
      return;
    }

    const mode = this.connection.connectionMode;
    switch (mode) {
      case ConnectionMode.DIRECT:
        this.#setConnectionState("direct");
        break;
      case ConnectionMode.RELAYED:
        this.#setConnectionState("relay");
        break;
      default:
        // Mode not yet detected - show connecting and poll for result
        this.#setConnectionState("connecting");
        this.#pollForConnectionMode();
    }
  }

  /**
   * Poll for connection mode until detected.
   * Retries up to 10 times with 200ms intervals (2s total).
   */
  async #pollForConnectionMode() {
    // Avoid multiple concurrent polls
    if (this.#modePolling) return;
    this.#modePolling = true;

    for (let i = 0; i < 10; i++) {
      await new Promise(r => setTimeout(r, 200));

      // Check if still connected and mode still unknown
      if (!this.connection?.isConnected()) {
        this.#modePolling = false;
        return;
      }

      const mode = this.connection.connectionMode;
      if (mode === ConnectionMode.DIRECT) {
        this.#setConnectionState("direct");
        this.#modePolling = false;
        return;
      } else if (mode === ConnectionMode.RELAYED) {
        this.#setConnectionState("relay");
        this.#modePolling = false;
        return;
      }
    }

    // After 2s, if still unknown, keep showing connecting
    // (rare edge case - stats API not responding)
    this.#modePolling = false;
  }

  // ========== DOM Updates ==========

  #setBrowserStatus(status) {
    if (this.hasBrowserSectionTarget) {
      this.browserSectionTarget.dataset.status = status;
    }
  }

  #setConnectionState(state) {
    if (this.hasConnectionSectionTarget) {
      this.connectionSectionTarget.dataset.state = state;
    }
  }

  #setHubStatus(status) {
    if (this.hasHubSectionTarget) {
      this.hubSectionTarget.dataset.status = status;
    }
  }
}
