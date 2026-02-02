import { Controller } from "@hotwired/stimulus";
import { ConnectionManager, HubConnection, TerminalConnection, BrowserStatus, CliStatus } from "connections";

/**
 * Connection Status Controller
 *
 * Shows the full connection lifecycle with three sections:
 * [Browser → Server] | [E2E Channel] | [CLI → Server]
 *
 * Browser states: disconnected, connecting, connected, error
 * Channel states: disconnected, connecting, handshake, connected, expired, cli-offline
 * CLI online: derived from health messages (not server-rendered)
 *
 * Usage:
 *   <div data-controller="connection-status"
 *        data-connection-status-hub-id-value="123"
 *        data-connection-status-type-value="hub">
 */
export default class extends Controller {
  static values = {
    hubId: String,
    type: { type: String, default: "hub" }, // hub, terminal, preview
    agentIndex: { type: Number, default: 0 },
    ptyIndex: { type: Number, default: 0 },
    browserState: { type: String, default: "disconnected" },
    channelState: { type: String, default: "disconnected" },
  };

  static targets = ["browserSection", "channelSection", "cliSection"];

  connect() {
    if (!this.hubIdValue) return;

    // Always acquire connection - browser should be ready for when CLI comes online
    // The channel state will show cli-offline if CLI isn't connected
    this.#acquireConnection();
  }

  disconnect() {
    // Clean up event listeners
    this.unsubscribers?.forEach(unsub => unsub());
    this.unsubscribers = [];

    this.connection?.release();
    this.connection = null;
  }

  // ========== Private ==========

  async #acquireConnection() {
    this.browserStateValue = "connecting";

    try {
      const ConnectionClass = this.#getConnectionClass();
      const key = this.#getConnectionKey();
      const options = this.#getConnectionOptions();

      this.connection = await ConnectionManager.acquire(ConnectionClass, key, options);
      this.unsubscribers = [];

      // Set up listeners BEFORE subscribe to catch health messages during subscription
      this.unsubscribers.push(
        this.connection.on("healthChange", ({ browser, cli }) => {
          this.#handleHealthChange(browser, cli);
        })
      );

      this.unsubscribers.push(
        this.connection.on("cliConnected", () => {
          this.channelStateValue = "handshake";
        })
      );

      this.unsubscribers.push(
        this.connection.on("connected", () => {
          this.channelStateValue = "connected";
        })
      );

      this.unsubscribers.push(
        this.connection.on("error", ({ reason, message }) => {
          this.browserStateValue = "error";
          if (reason === "session_invalid" || message?.includes("expired")) {
            this.channelStateValue = "expired";
          }
        })
      );

      // Subscribe (may already be subscribed - will emit healthChange with current status)
      await this.connection.subscribe();

      // Explicitly apply current status in case health message arrived before our listener
      this.#handleHealthChange(this.connection.browserStatus, this.connection.cliStatus);
    } catch (error) {
      console.error("[ConnectionStatus] Failed to acquire connection:", error);
      this.browserStateValue = "error";
      this.channelStateValue = "disconnected";
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
    const base = { hubId: this.hubIdValue, fromFragment: true };

    switch (this.typeValue) {
      case "terminal":
      case "preview":
        return { ...base, agentIndex: this.agentIndexValue, ptyIndex: this.ptyIndexValue };
      default:
        return base;
    }
  }

  #handleHealthChange(browser, cli) {
    // Map BrowserStatus to display state
    const browserStateMap = {
      [BrowserStatus.DISCONNECTED]: "disconnected",
      [BrowserStatus.CONNECTING]: "connecting",
      [BrowserStatus.SUBSCRIBING]: "connecting",
      [BrowserStatus.SUBSCRIBED]: "connected",
      [BrowserStatus.ERROR]: "error",
    };
    this.browserStateValue = browserStateMap[browser] || "disconnected";

    // Determine CLI online status from CLI status
    const cliOnline = cli === CliStatus.CONNECTED || cli === CliStatus.ONLINE ||
                      cli === CliStatus.NOTIFIED || cli === CliStatus.CONNECTING;

    // Update CLI section directly
    if (this.hasCliSectionTarget) {
      this.cliSectionTarget.dataset.cliOnline = String(cliOnline);
    }

    // Channel state - check handshake completion first (may complete before health message arrives)
    if (this.connection?.isConnected()) {
      this.channelStateValue = "connected";
      return;
    }

    // Channel state depends on both browser and CLI
    if (browser === BrowserStatus.DISCONNECTED || browser === BrowserStatus.CONNECTING) {
      this.channelStateValue = "disconnected";
    } else if (cli === CliStatus.OFFLINE || cli === CliStatus.DISCONNECTED) {
      this.channelStateValue = "cli-offline";
    } else if (cli === CliStatus.UNKNOWN || cli === CliStatus.ONLINE || cli === CliStatus.NOTIFIED) {
      // CLI is online but not yet on our E2E channel
      this.channelStateValue = "connecting";
    } else if (cli === CliStatus.CONNECTING || cli === CliStatus.CONNECTED) {
      // CLI connecting or connected but handshake not complete yet
      this.channelStateValue = "handshake";
    }
  }

  // Value change callbacks - update data attributes
  browserStateValueChanged(value) {
    if (this.hasBrowserSectionTarget) {
      this.browserSectionTarget.dataset.browserState = value;
    }
  }

  channelStateValueChanged(value) {
    if (this.hasChannelSectionTarget) {
      this.channelSectionTarget.dataset.channelState = value;
    }
  }
}
