import ConnectionController, {
  ConnectionState,
  ConnectionError,
  consumer,
  Channel,
} from "controllers/connection_controller";

/**
 * Hub Connection Controller
 *
 * Manages the hub channel (control plane) — agent management, handshake,
 * and connection status UI. One instance per browser session, persists
 * across Turbo navigations via data-turbo-permanent.
 *
 * Connection Flow:
 * 1. LOADING_WASM - Loading Signal Protocol WASM module
 * 2. CREATING_SESSION - Setting up encryption from QR bundle
 * 3. SUBSCRIBING - Connecting to HubChannel (control plane)
 * 4. CHANNEL_CONNECTED - HubChannel confirmed (CLI is reachable)
 * 5. HANDSHAKE_SENT - Sent encrypted handshake, waiting for CLI ACK
 * 6. CONNECTED - CLI acknowledged, E2E encryption active
 */

const HANDSHAKE_TIMEOUT_MS = 8000;

// SVG icons for status display
const ICONS = {
  spinner: `<svg class="w-4 h-4 animate-spin" fill="none" viewBox="0 0 24 24">
    <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
    <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"></path>
  </svg>`,
  check: `<svg class="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
    <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M5 13l4 4L19 7"></path>
  </svg>`,
  error: `<svg class="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
    <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M12 8v4m0 4h.01M21 12a9 9 0 11-18 0 9 9 0 0118 0z"></path>
  </svg>`,
  disconnected: `<svg class="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
    <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M18.364 5.636a9 9 0 010 12.728m-2.829-2.829a5 5 0 000-7.07m-4.243 4.243a1 1 0 11-1.414-1.414 1 1 0 011.414 1.414z"></path>
  </svg>`,
  lock: `<svg class="w-4 h-4" fill="currentColor" viewBox="0 0 20 20">
    <path fill-rule="evenodd" d="M5 9V7a5 5 0 0110 0v2a2 2 0 012 2v5a2 2 0 01-2 2H5a2 2 0 01-2-2v-5a2 2 0 012-2zm8-2v2H7V7a3 3 0 016 0z" clip-rule="evenodd"></path>
  </svg>`,
};

const STATUS_CONFIG = {
  disconnected: {
    text: "Disconnected",
    icon: ICONS.disconnected,
    iconClass: "text-zinc-500",
    textClass: "text-zinc-500",
  },
  loading_wasm: {
    text: "Loading encryption",
    icon: ICONS.spinner,
    iconClass: "text-cyan-400",
    textClass: "text-zinc-400",
  },
  creating_session: {
    text: "Setting up encryption",
    icon: ICONS.spinner,
    iconClass: "text-cyan-400",
    textClass: "text-zinc-400",
  },
  subscribing: {
    text: "Connecting to server",
    icon: ICONS.spinner,
    iconClass: "text-cyan-400",
    textClass: "text-zinc-400",
  },
  channel_connected: {
    text: "CLI reachable",
    icon: ICONS.spinner,
    iconClass: "text-amber-400",
    textClass: "text-amber-400",
  },
  handshake_sent: {
    text: "Handshake sent",
    icon: ICONS.spinner,
    iconClass: "text-amber-400",
    textClass: "text-amber-400",
  },
  connected: {
    text: "Connected",
    icon: ICONS.lock,
    iconClass: "text-emerald-400",
    textClass: "text-emerald-400",
  },
  error: {
    text: "Connection failed",
    icon: ICONS.error,
    iconClass: "text-red-400",
    textClass: "text-red-400",
  },
};

export default class extends ConnectionController {
  static targets = [
    "status",
    "statusContainer",
    "statusIcon",
    "statusText",
    "statusDetail",
    "statusIconMobile",
    "statusTextMobile",
    "disconnectBtn",
    "securityBanner",
    "securityIcon",
    "securityText",
    "shareBtn",
    "shareStatus",
  ];

  static values = {
    hubId: String,
    workerUrl: String,
    wasmJsUrl: String,
    wasmBinaryUrl: String,
  };

  static classes = ["securityBannerBase"];

  connect() {
    super.connect();

    this.hubChannel = null;
    this.hubSubscription = null;
    this.handshakeTimer = null;
    this.hasCompletedInitialSetup = false;

    this.initializeConnection();
  }

  disconnect() {
    super.disconnect();
  }

  // ========== Connection Flow ==========

  async initializeConnection() {
    try {
      // Step 1-2: WASM + session (base class)
      this.updateStatus("Loading encryption...", "Initializing Signal Protocol");
      const success = await this.initSession();
      if (!success) {
        this.updateStatus("Connection failed", this.errorReason);
        return;
      }

      // Step 3: Subscribe to hub channel
      this.setState(ConnectionState.SUBSCRIBING);
      this.updateStatus("Connecting to server...", "Establishing secure channel");

      try {
        await this.subscribeToHubChannel();
      } catch (subError) {
        this.setError(
          ConnectionError.SUBSCRIBE_REJECTED,
          `Connection rejected: ${subError.message}`,
        );
        this.updateStatus("Connection failed", subError.message);
        return;
      }

      // Step 4-5: Handshake
      this.setState(ConnectionState.CHANNEL_CONNECTED);
      this.updateStatus("CLI reachable", "Sending encrypted handshake...");

      this.setState(ConnectionState.HANDSHAKE_SENT);
      await this.sendHandshakeWithTimeout();
    } catch (error) {
      console.error("[HubConnection] Failed to initialize:", error);
      this.setError(
        ConnectionError.WEBSOCKET_ERROR,
        `Connection error: ${error.message}`,
      );
      this.updateStatus("Connection failed", error.message);
    }
  }

  // ========== Hub Channel ==========

  subscribeToHubChannel() {
    return new Promise((resolve, reject) => {
      let initialConnectionResolved = false;

      this.hubSubscription = consumer.subscriptions.create(
        {
          channel: "HubChannel",
          hub_id: this.hubId,
          browser_identity: this.ourIdentityKey,
        },
        {
          connected: () => {
            console.log("[HubConnection] HubChannel connected");

            if (this.hubChannel) {
              this.hubChannel.destroy();
            }

            this.hubChannel = Channel.builder(this.hubSubscription)
              .session(this.signalSession)
              .reliable(true)
              .onMessage((msg) => this.handleDecryptedMessage(msg))
              .onConnect(() => console.log("[HubConnection] Hub channel ready"))
              .onDisconnect(() => this.handleDisconnect())
              .onError((err) => this.handleChannelError(err))
              .build();
            this.hubChannel.markConnected();

            if (!initialConnectionResolved) {
              initialConnectionResolved = true;
              resolve();
            } else if (this.hasCompletedInitialSetup) {
              console.log("[HubConnection] HubChannel reconnected, restarting handshake");
              this.restartHandshake();
            }
          },
          disconnected: () => {
            console.log("[HubConnection] HubChannel disconnected");
            if (this.hubChannel) {
              this.hubChannel.destroy();
              this.hubChannel = null;
            }
            this.handleDisconnect();
          },
          rejected: () => {
            console.error("[HubConnection] HubChannel subscription rejected");
            reject(new Error("Hub subscription rejected - hub may be offline"));
          },
          received: async (data) => {
            if (this.hubChannel) {
              await this.hubChannel.receive(data);
            }
          },
        },
      );
    });
  }

  // ========== Handshake ==========

  async sendHandshakeWithTimeout() {
    const handshake = {
      type: "connected",
      device_name: this.getDeviceName(),
      timestamp: Date.now(),
    };

    const sent = await this.sendEncrypted(handshake);

    if (!sent) {
      this.setError(ConnectionError.HANDSHAKE_FAILED, "Failed to send handshake");
      this.updateStatus("Connection failed", "Failed to send handshake");
      return;
    }

    this.updateStatus("Handshake sent", "Waiting for CLI acknowledgment...");

    this.handshakeTimer = setTimeout(async () => {
      if (this.state === ConnectionState.HANDSHAKE_SENT) {
        console.warn("[HubConnection] Handshake timeout - no ACK from CLI");
        await this.handleHandshakeTimeout();
      }
    }, HANDSHAKE_TIMEOUT_MS);
  }

  async handleHandshakeTimeout() {
    try {
      const response = await fetch(`/hubs/${this.hubId}.json`, {
        credentials: "same-origin",
        headers: { "Accept": "application/json" },
      });

      if (response.ok) {
        const status = await response.json();
        const isCliOnline = status.seconds_since_heartbeat !== null &&
                           status.seconds_since_heartbeat < 30;

        if (isCliOnline) {
          this.setError(
            ConnectionError.SESSION_INVALID,
            "Session expired. Re-scan QR code from CLI (Ctrl+P).",
          );
        } else {
          this.setError(
            ConnectionError.HANDSHAKE_TIMEOUT,
            "CLI not responding. Is botster-hub running?",
          );
        }
      } else {
        this.setError(
          ConnectionError.HANDSHAKE_TIMEOUT,
          "CLI did not respond. Is botster-hub running?",
        );
      }
    } catch (error) {
      this.setError(
        ConnectionError.HANDSHAKE_TIMEOUT,
        "CLI did not respond. Is botster-hub running?",
      );
    }
    this.updateStatus("Connection failed", this.errorReason);
  }

  async restartHandshake() {
    if (!this.signalSession) return;
    if (!this.hubChannel) return;

    if (this.handshakeTimer) {
      clearTimeout(this.handshakeTimer);
      this.handshakeTimer = null;
    }

    this.setState(ConnectionState.CHANNEL_CONNECTED);
    this.updateStatus("Reconnected", "Re-establishing secure connection...");

    this.setState(ConnectionState.HANDSHAKE_SENT);
    await this.sendHandshakeWithTimeout();
  }

  getDeviceName() {
    const ua = navigator.userAgent;
    if (ua.includes("iPhone")) return "iPhone";
    if (ua.includes("iPad")) return "iPad";
    if (ua.includes("Android")) return "Android";
    if (ua.includes("Mac")) return "Mac Browser";
    if (ua.includes("Windows")) return "Windows Browser";
    if (ua.includes("Linux")) return "Linux Browser";
    return "Browser";
  }

  // ========== Message Handling ==========

  handleDecryptedMessage(message) {
    // Handle handshake acknowledgment
    if (message.type === "handshake_ack") {
      console.log("[HubConnection] Received handshake ACK from CLI");

      if (this.handshakeTimer) {
        clearTimeout(this.handshakeTimer);
        this.handshakeTimer = null;
      }

      this.connected = true;
      this.hasCompletedInitialSetup = true;
      this.setState(ConnectionState.CONNECTED);
      this.updateStatus(
        "Connected",
        `E2E encrypted to ${this.hubId.substring(0, 8)}...`,
      );

      this.notifyListeners("connected", this);
      return;
    }

    // Handle invite bundle response
    if (message.type === "invite_bundle") {
      console.log("[HubConnection] Received invite bundle from CLI");
      this.handleInviteBundle(message);
      return;
    }

    // Route other messages to listeners
    this.notifyListeners("message", message);
  }

  handleDisconnect() {
    this.connected = false;
    this.setState(ConnectionState.DISCONNECTED);
    this.updateStatus("Disconnected", "Connection lost");
    this.notifyListeners("disconnected");
  }

  handleChannelError(error) {
    console.error("[HubConnection] Channel error:", error);

    if (error.type === "session_invalid") {
      this.clearSessionAndShowError(error.message);
    } else {
      this.setError(ConnectionError.WEBSOCKET_ERROR, error.message || "Channel error");
    }
    this.updateStatus("Connection failed", error.message || "Channel error");
  }

  // ========== Hub Commands ==========

  /**
   * Send a JSON message to CLI (encrypted) via hub channel.
   */
  async send(type, data = {}) {
    if (!this.connected || !this.signalSession) {
      console.warn("[HubConnection] Cannot send - not connected");
      return false;
    }

    const message = { type, ...data };
    return await this.sendEncrypted(message);
  }

  /**
   * Send encrypted message via hub channel with reliable delivery.
   */
  async sendEncrypted(message) {
    if (!this.hubChannel) {
      console.warn("[HubConnection] Cannot send encrypted - no hub channel");
      return false;
    }

    try {
      return await this.hubChannel.send(message);
    } catch (error) {
      console.error("[HubConnection] Send failed:", error);
      return false;
    }
  }

  requestAgents() {
    return this.send("list_agents");
  }

  requestWorktrees() {
    return this.send("list_worktrees");
  }

  selectAgent(agentId) {
    return this.send("select_agent", { id: agentId });
  }

  deleteAgent(agentId, deleteWorktree = false) {
    return this.send("delete_agent", {
      id: agentId,
      delete_worktree: deleteWorktree,
    });
  }

  // ========== Reconnect ==========

  async reconnect() {
    console.log("[HubConnection] Manual reconnect requested");

    if (this.signalSession && this.hubChannel) {
      await this.restartHandshake();
      return;
    }

    const savedListeners = this.listeners;

    if (this.handshakeTimer) {
      clearTimeout(this.handshakeTimer);
      this.handshakeTimer = null;
    }
    if (this.hubChannel) {
      this.hubChannel.destroy();
      this.hubChannel = null;
    }
    if (this.hubSubscription) {
      this.hubSubscription.unsubscribe();
      this.hubSubscription = null;
    }
    this.connected = false;
    this.hasCompletedInitialSetup = false;

    this.listeners = savedListeners;
    await this.initializeConnection();
  }

  // ========== Share Hub ==========

  async requestInviteBundle() {
    if (!this.connected || !this.signalSession) {
      console.warn("[HubConnection] Cannot request invite - not connected");
      this.updateShareStatus("Not connected", "error");
      return;
    }

    this.updateShareStatus("Generating...", "loading");

    const success = await this.send("generate_invite");
    if (!success) {
      this.updateShareStatus("Failed to request", "error");
    }
  }

  async handleInviteBundle(message) {
    const { url, bundle } = message;

    if (!url) {
      this.updateShareStatus("Invalid response", "error");
      return;
    }

    // Try native share first (mobile), fall back to clipboard
    if (navigator.share && /iPhone|iPad|Android/i.test(navigator.userAgent)) {
      try {
        await navigator.share({
          title: "Join Hub",
          text: "Connect to my Botster hub",
          url: url,
        });
        this.updateShareStatus("Shared!", "success");
        return;
      } catch (err) {
        if (err.name !== "AbortError") {
          console.warn("[HubConnection] Native share failed:", err);
        }
      }
    }

    try {
      await navigator.clipboard.writeText(url);
      this.updateShareStatus("Copied to clipboard!", "success");
    } catch (err) {
      prompt("Copy this link to share:", url);
      this.updateShareStatus("Copy the link above", "info");
    }
  }

  updateShareStatus(text, state) {
    if (this.hasShareStatusTarget) {
      this.shareStatusTarget.textContent = text;

      if (state !== "loading") {
        setTimeout(() => {
          if (this.hasShareStatusTarget) {
            this.shareStatusTarget.textContent = "";
          }
        }, 3000);
      }
    }

    if (this.hasShareBtnTarget) {
      const btn = this.shareBtnTarget;
      btn.disabled = state === "loading";

      if (state === "loading") {
        btn.classList.add("opacity-50", "cursor-wait");
      } else {
        btn.classList.remove("opacity-50", "cursor-wait");
      }
    }
  }

  // ========== Override State Management ==========

  setError(reason, message) {
    super.setError(reason, message);
    this.updateStatus("Connection failed", message);
  }

  // ========== Cleanup ==========

  cleanup() {
    if (this.handshakeTimer) {
      clearTimeout(this.handshakeTimer);
      this.handshakeTimer = null;
    }
    if (this.hubChannel) {
      this.hubChannel.destroy();
      this.hubChannel = null;
    }
    if (this.hubSubscription) {
      this.hubSubscription.unsubscribe();
      this.hubSubscription = null;
    }
    this.hasCompletedInitialSetup = false;
    super.cleanup();
  }

  disconnectAction() {
    this.cleanup();
    this.updateStatus("Disconnected", "");
    this.notifyListeners("disconnected");
  }

  // ========== Status UI ==========

  updateStatus(text, detail = "") {
    const config = STATUS_CONFIG[this.state] || STATUS_CONFIG.disconnected;

    const statusIcon = this.hasStatusIconTarget
      ? this.statusIconTarget
      : document.querySelector("[data-hub-connection-target='statusIcon']");
    const statusText = this.hasStatusTextTarget
      ? this.statusTextTarget
      : document.querySelector("[data-hub-connection-target='statusText']");

    if (statusIcon) {
      statusIcon.innerHTML = config.icon;
      statusIcon.className = `shrink-0 ${config.iconClass}`;
    }

    if (statusText) {
      statusText.textContent = text || config.text;
      statusText.className = config.textClass;
    }

    const statusDetail = this.hasStatusDetailTarget
      ? this.statusDetailTarget
      : document.querySelector("[data-hub-connection-target='statusDetail']");
    if (statusDetail) {
      statusDetail.textContent = detail;
      if (this.state === ConnectionState.ERROR) {
        statusDetail.className = "text-xs text-red-400/80 font-mono max-w-xs text-right";
      } else if (this.state === ConnectionState.CONNECTED) {
        statusDetail.className = "text-xs text-emerald-400/60 font-mono";
      } else {
        statusDetail.className = "text-xs text-zinc-500 font-mono";
      }
    }

    // Mobile status
    const statusIconMobile = this.hasStatusIconMobileTarget
      ? this.statusIconMobileTarget
      : document.querySelector("[data-hub-connection-target='statusIconMobile']");
    if (statusIconMobile) {
      const mobileIcon = config.icon.replace(/size-4/g, "size-3");
      statusIconMobile.innerHTML = mobileIcon;
      statusIconMobile.className = `shrink-0 ${config.iconClass}`;
    }

    const statusTextMobile = this.hasStatusTextMobileTarget
      ? this.statusTextMobileTarget
      : document.querySelector("[data-hub-connection-target='statusTextMobile']");
    if (statusTextMobile) {
      const shortText = (text || config.text)
        .replace("Initializing...", "Init...")
        .replace("Connecting...", "...")
        .replace("Connected", "Live");
      statusTextMobile.textContent = shortText;
      statusTextMobile.className = `text-xs shrink-0 ${config.textClass}`;
    }

    // Disconnect button
    const disconnectBtn = this.hasDisconnectBtnTarget
      ? this.disconnectBtnTarget
      : document.querySelector("[data-hub-connection-target='disconnectBtn']");
    if (disconnectBtn) {
      if (this.state === ConnectionState.CONNECTED) {
        disconnectBtn.classList.remove("hidden");
      } else {
        disconnectBtn.classList.add("hidden");
      }
    }

    this.updateSecurityBanner();
  }

  updateSecurityBanner() {
    const securityBanner = this.hasSecurityBannerTarget
      ? this.securityBannerTarget
      : document.querySelector("[data-hub-connection-target='securityBanner']");
    const securityIcon = this.hasSecurityIconTarget
      ? this.securityIconTarget
      : document.querySelector("[data-hub-connection-target='securityIcon']");
    const securityText = this.hasSecurityTextTarget
      ? this.securityTextTarget
      : document.querySelector("[data-hub-connection-target='securityText']");

    if (!securityBanner) return;

    const lockIcon = `<svg class="w-4 h-4" fill="currentColor" viewBox="0 0 20 20">
      <path fill-rule="evenodd" d="M5 9V7a5 5 0 0110 0v2a2 2 0 012 2v5a2 2 0 01-2 2H5a2 2 0 01-2-2v-5a2 2 0 012-2zm8-2v2H7V7a3 3 0 016 0z" clip-rule="evenodd"/>
    </svg>`;
    const unlockIcon = `<svg class="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
      <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M12 15v2m-6 4h12a2 2 0 002-2v-6a2 2 0 00-2-2H6a2 2 0 00-2 2v6a2 2 0 002 2zm10-10V7a4 4 0 00-8 0v4h8z"/>
    </svg>`;
    const errorIcon = `<svg class="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
      <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M12 9v2m0 4h.01m-6.938 4h13.856c1.54 0 2.502-1.667 1.732-3L13.732 4c-.77-1.333-2.694-1.333-3.464 0L3.34 16c-.77 1.333.192 3 1.732 3z"/>
    </svg>`;

    const baseClasses = this.hasSecurityBannerBaseClass
      ? this.securityBannerBaseClass
      : "";

    switch (this.state) {
      case ConnectionState.CONNECTED:
        securityBanner.className = `${baseClasses} border-b border-emerald-500/20 bg-emerald-500/5 transition-colors duration-300`;
        if (securityIcon) {
          securityIcon.innerHTML = lockIcon;
          securityIcon.className = "shrink-0 text-emerald-400";
        }
        if (securityText) {
          securityText.innerHTML = `
            <strong class="text-emerald-300">Signal Protocol E2E Encryption</strong>
            <span class="text-emerald-200/80">&mdash; Double Ratchet + Post-Quantum (Kyber)</span>
          `;
        }
        break;

      case ConnectionState.ERROR:
        securityBanner.className = `${baseClasses} border-b border-red-500/20 bg-red-500/5 transition-colors duration-300`;
        if (securityIcon) {
          securityIcon.innerHTML = errorIcon;
          securityIcon.className = "shrink-0 text-red-400";
        }
        if (securityText) {
          securityText.innerHTML = `
            <strong class="text-red-300">Connection Failed</strong>
            <span class="text-red-200/80">&mdash; ${this.errorReason || "Unable to establish secure connection"}</span>
          `;
        }
        break;

      case ConnectionState.CHANNEL_CONNECTED:
      case ConnectionState.HANDSHAKE_SENT:
        securityBanner.className = `${baseClasses} border-b border-amber-500/20 bg-amber-500/5 transition-colors duration-300`;
        if (securityIcon) {
          securityIcon.innerHTML = unlockIcon;
          securityIcon.className = "shrink-0 text-amber-400";
        }
        if (securityText) {
          securityText.innerHTML = `
            <strong class="text-amber-300">Establishing E2E Encryption</strong>
            <span class="text-amber-200/80">&mdash; Waiting for CLI acknowledgment...</span>
          `;
        }
        break;

      default:
        securityBanner.className = `${baseClasses} border-b border-zinc-700/50 bg-zinc-800/30 transition-colors duration-300`;
        if (securityIcon) {
          securityIcon.innerHTML = unlockIcon;
          securityIcon.className = "shrink-0 text-zinc-500";
        }
        if (securityText) {
          securityText.innerHTML = `
            <span class="text-zinc-400">Establishing secure connection...</span>
          `;
        }
        break;
    }
  }
}
