import { Controller } from "@hotwired/stimulus";
import { loadSession, open, getHubIdFromPath } from "channels/secure_channel";

/**
 * Hub Connection Controller
 *
 * Manages the hub channel (control plane) — agent management, handshake,
 * and connection status UI. One instance per browser session, persists
 * across Turbo navigations via data-turbo-permanent.
 *
 * Uses SecureChannel module for all encryption and channel mechanics.
 * WASM URLs come from <meta> tags — no stimulus values needed.
 *
 * Connection Flow:
 * 1. Load Signal session (WASM + IndexedDB / QR fragment)
 * 2. Open encrypted HubChannel via SecureChannel
 * 3. Send handshake, wait for CLI ACK
 * 4. CONNECTED — E2E encryption active
 */

const HANDSHAKE_TIMEOUT_MS = 8000;

// Internal state constants
const State = {
  DISCONNECTED: "disconnected",
  LOADING_WASM: "loading_wasm",
  CREATING_SESSION: "creating_session",
  SUBSCRIBING: "subscribing",
  CHANNEL_CONNECTED: "channel_connected",
  HANDSHAKE_SENT: "handshake_sent",
  CONNECTED: "connected",
  ERROR: "error",
};

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

export default class extends Controller {
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
    connectionState: { type: String, default: "disconnected" },
  };

  static classes = ["securityBannerBase"];

  // -- Turbo permanent lifecycle -----------------------------------------
  // data-turbo-permanent means the same DOM element (and controller instance)
  // survives page navigations. Turbo calls disconnect() then connect() on
  // the same instance during reinserts. We schedule cleanup on turbo:load
  // so the channel survives the disconnect/connect cycle. If connect() fires
  // before turbo:load, we cancel the cleanup. If turbo:load fires and the
  // element is no longer in the DOM, we know the user navigated away.

  #handle = null;
  #turboCleanup = null;
  #beforeUnloadHandler = null;

  connect() {
    // Cancel any pending turbo:load cleanup from a previous disconnect
    if (this.#turboCleanup) {
      document.removeEventListener("turbo:load", this.#turboCleanup);
      this.#turboCleanup = null;
    }

    // Turbo permanent reinsert: same instance, connection still alive
    if (this.connectionStateValue !== "disconnected" && this.#handle) {
      this.#refreshStatusUI();
      return;
    }

    // First mount or after real cleanup
    this.listeners = this.listeners || new Map();
    this.session = null;
    this.identityKey = null;
    this.connected = false;
    this.state = State.DISCONNECTED;
    this.connectionStateValue = "disconnected";
    this.errorReason = null;
    this.handshakeTimer = null;
    this.hasCompletedInitialSetup = false;
    this.hubId = getHubIdFromPath() || this.hubIdValue;

    // Ensure explicit cleanup on hard page refresh / tab close.
    // beforeunload does NOT fire during Turbo navigation — only real page unloads.
    // This guarantees the ActionCable subscription is explicitly unsubscribed,
    // giving the CLI time to tear down the BrowserClient before a new connection arrives.
    if (!this.#beforeUnloadHandler) {
      this.#beforeUnloadHandler = () => this.#cleanup();
      window.addEventListener("beforeunload", this.#beforeUnloadHandler);
    }

    this.#initializeConnection();
  }

  disconnect() {
    // Don't destroy the connection immediately — Turbo permanent elements
    // get disconnect()/connect() during navigation. Schedule a check after
    // the new page renders to see if the element was truly removed.
    this.#turboCleanup = () => {
      this.#turboCleanup = null;
      if (!this.element.isConnected) {
        this.#cleanup();
      }
    };
    document.addEventListener("turbo:load", this.#turboCleanup, { once: true });
  }

  // ========== Connection Flow ==========

  async #initializeConnection() {
    try {
      this.#setState(State.LOADING_WASM);
      this.updateStatus(
        "Loading encryption...",
        "Initializing Signal Protocol",
      );

      this.session = await loadSession(this.hubId, { fromFragment: true });
      if (!this.session) {
        this.showError("Not paired. Press Ctrl+P in CLI to show QR code.");
        return;
      }

      this.#setState(State.CREATING_SESSION);
      this.identityKey = await this.session.getIdentityKey();

      this.#setState(State.SUBSCRIBING);
      this.updateStatus(
        "Connecting to server...",
        "Establishing secure channel",
      );

      this.#handle = await open({
        channel: "HubChannel",
        params: { hub_id: this.hubId, browser_identity: this.identityKey },
        session: this.session,
        reliable: true,
        onMessage: (msg) => this.#handleMessage(msg),
        onDisconnect: () => this.#handleDisconnect(),
        onError: (err) => this.#handleChannelError(err),
      });

      this.#setState(State.CHANNEL_CONNECTED);
      this.updateStatus("CLI reachable", "Sending encrypted handshake...");

      this.#setState(State.HANDSHAKE_SENT);
      await this.#sendHandshake();
    } catch (error) {
      console.error("[HubConnection] Failed to initialize:", error);
      this.#setError("websocket_error", `Connection error: ${error.message}`);
      this.updateStatus("Connection failed", error.message);
    }
  }

  // ========== Handshake ==========

  async #sendHandshake() {
    const handshake = {
      type: "connected",
      device_name: this.#getDeviceName(),
      timestamp: Date.now(),
    };

    const sent = await this.send("connected", {
      device_name: handshake.device_name,
      timestamp: handshake.timestamp,
    });

    if (!sent) {
      this.#setError("handshake_failed", "Failed to send handshake");
      this.updateStatus("Connection failed", "Failed to send handshake");
      return;
    }

    this.updateStatus("Handshake sent", "Waiting for CLI acknowledgment...");

    this.handshakeTimer = setTimeout(async () => {
      if (this.state === State.HANDSHAKE_SENT) {
        console.warn("[HubConnection] Handshake timeout - no ACK from CLI");
        await this.#handleHandshakeTimeout();
      }
    }, HANDSHAKE_TIMEOUT_MS);
  }

  async #handleHandshakeTimeout() {
    try {
      const response = await fetch(`/hubs/${this.hubId}.json`, {
        credentials: "same-origin",
        headers: { Accept: "application/json" },
      });

      if (response.ok) {
        const status = await response.json();
        const isCliOnline =
          status.seconds_since_heartbeat !== null &&
          status.seconds_since_heartbeat < 30;

        if (isCliOnline) {
          this.#setError(
            "session_invalid",
            "Session expired. Re-scan QR code from CLI (Ctrl+P).",
          );
        } else {
          this.#setError(
            "handshake_timeout",
            "CLI not responding. Is botster-hub running?",
          );
        }
      } else {
        this.#setError(
          "handshake_timeout",
          "CLI did not respond. Is botster-hub running?",
        );
      }
    } catch (error) {
      this.#setError(
        "handshake_timeout",
        "CLI did not respond. Is botster-hub running?",
      );
    }
    this.updateStatus("Connection failed", this.errorReason);
  }

  async #restartHandshake() {
    if (!this.session) return;
    if (!this.#handle) return;

    if (this.handshakeTimer) {
      clearTimeout(this.handshakeTimer);
      this.handshakeTimer = null;
    }

    this.#setState(State.CHANNEL_CONNECTED);
    this.updateStatus("Reconnected", "Re-establishing secure connection...");

    this.#setState(State.HANDSHAKE_SENT);
    await this.#sendHandshake();
  }

  #getDeviceName() {
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

  #handleMessage(message) {
    // Handle handshake acknowledgment
    if (message.type === "handshake_ack") {
      console.debug("[HubConnection] Received handshake ACK from CLI");

      if (this.handshakeTimer) {
        clearTimeout(this.handshakeTimer);
        this.handshakeTimer = null;
      }

      this.connected = true;
      this.hasCompletedInitialSetup = true;
      this.#setState(State.CONNECTED);
      this.updateStatus(
        "Connected",
        `E2E encrypted to ${this.hubId.substring(0, 8)}...`,
      );

      this.notifyListeners("connected", this);
      return;
    }

    // Handle invite bundle response
    if (message.type === "connection_code") {
      console.debug("[HubConnection] Received invite bundle from CLI");
      this.handleConnectionCode(message);
      return;
    }

    // Route other messages to listeners
    this.notifyListeners("message", message);
  }

  #handleDisconnect() {
    this.connected = false;
    this.#setState(State.DISCONNECTED);
    this.updateStatus("Disconnected", "Connection lost");
    this.notifyListeners("disconnected");
  }

  #handleChannelError(error) {
    console.error("[HubConnection] Channel error:", error);

    if (error.type === "session_invalid") {
      this.clearSessionAndShowError(error.message);
    } else {
      this.#setError("websocket_error", error.message || "Channel error");
    }
    this.updateStatus("Connection failed", error.message || "Channel error");
  }

  // ========== Hub Commands (Public API) ==========

  /**
   * Send a JSON message to CLI (encrypted) via hub channel.
   * Used by outlets (agents, terminal-connection, preview).
   */
  async send(type, data = {}) {
    if (!this.#handle) {
      console.warn("[HubConnection] Cannot send - no channel handle");
      return false;
    }

    try {
      return await this.#handle.send({ type, ...data });
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
    console.debug("[HubConnection] Manual reconnect requested");

    if (this.session && this.#handle) {
      await this.#restartHandshake();
      return;
    }

    const savedListeners = this.listeners;

    if (this.handshakeTimer) {
      clearTimeout(this.handshakeTimer);
      this.handshakeTimer = null;
    }
    this.#handle?.close();
    this.#handle = null;
    this.connected = false;
    this.hasCompletedInitialSetup = false;

    this.listeners = savedListeners;
    this.hubId = getHubIdFromPath() || this.hubIdValue;
    await this.#initializeConnection();
  }

  // ========== Share Hub ==========

  async requestConnectionCode() {
    if (!this.connected || !this.session) {
      console.warn("[HubConnection] Cannot request invite - not connected");
      this.updateShareStatus("Not connected", "error");
      return;
    }

    this.updateShareStatus("Generating...", "loading");

    const success = await this.send("get_connection_code");
    if (!success) {
      this.updateShareStatus("Failed to request", "error");
    }
  }

  async handleConnectionCode(message) {
    const { url } = message;

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

  // ========== Listener Registration API ==========

  /**
   * Register a controller to receive connection callbacks.
   * If already connected, onConnected is called immediately.
   */
  registerListener(controller, callbacks) {
    if (!this.listeners) {
      this.listeners = new Map();
    }
    this.listeners.set(controller, callbacks);

    // If already connected, immediately notify
    if (this.connected && this.session) {
      callbacks.onConnected?.(this);
    }

    // Notify of current state
    callbacks.onStateChange?.(this.state, this.errorReason);
  }

  /**
   * Unregister a controller from receiving callbacks.
   */
  unregisterListener(controller) {
    this.listeners?.delete(controller);
  }

  /**
   * Notify all listeners of an event.
   */
  notifyListeners(event, data) {
    if (!this.listeners) return;
    for (const [, callbacks] of this.listeners) {
      switch (event) {
        case "connected":
          callbacks.onConnected?.(data);
          break;
        case "disconnected":
          callbacks.onDisconnected?.();
          break;
        case "message":
          callbacks.onMessage?.(data);
          break;
        case "error":
          callbacks.onError?.(data);
          break;
        case "stateChange":
          callbacks.onStateChange?.(data.state, data.reason);
          break;
      }
    }
  }

  // ========== State Management ==========

  #setState(newState) {
    const prevState = this.state;
    this.state = newState;
    this.connectionStateValue = newState; // Persist in DOM for Turbo survival
    if (newState !== State.ERROR) {
      this.errorReason = null;
    }
    console.debug(`[HubConnection] State: ${prevState} -> ${newState}`);
    this.notifyListeners("stateChange", {
      state: newState,
      reason: this.errorReason,
    });
  }

  #setError(reason, message) {
    this.errorReason = reason;
    this.#setState(State.ERROR);
    console.error(`[HubConnection] Error (${reason}): ${message}`);
    this.notifyListeners("error", { reason, message });
    this.updateStatus("Connection failed", message);
  }

  // ========== Public API (used by outlets) ==========

  isConnected() {
    return this.connected;
  }

  getHubId() {
    return this.hubId;
  }

  getState() {
    return this.state;
  }

  getErrorReason() {
    return this.errorReason;
  }

  async resetSession() {
    if (this.session) {
      await this.session.clear();
      this.session = null;
    }
    this.#cleanup();
    this.#setError(
      "session_create_failed",
      "Session cleared. Scan QR code to reconnect.",
    );
  }

  /**
   * Clear the cached Signal session and show an error prompting user to re-scan QR.
   */
  async clearSessionAndShowError(message) {
    console.debug(
      "[HubConnection] Clearing stale session for hub:",
      this.hubId,
    );

    if (this.session) {
      try {
        await this.session.clear();
      } catch (err) {
        console.error("[HubConnection] Failed to clear session:", err);
      }
      this.session = null;
    }

    this.#cleanup();
    this.#setError(
      "session_invalid",
      message || "Session expired. Please re-scan the QR code.",
    );
  }

  /**
   * Show an error in the status UI. Called from #initializeConnection when
   * loadSession returns null (not paired).
   */
  showError(message) {
    this.#setError("no_bundle", message);
  }

  // ========== Cleanup ==========

  #cleanup() {
    if (this.handshakeTimer) {
      clearTimeout(this.handshakeTimer);
      this.handshakeTimer = null;
    }
    this.#handle?.close();
    this.#handle = null;
    this.session = null;
    this.connected = false;
    this.hasCompletedInitialSetup = false;
    this.connectionStateValue = "disconnected";
    if (this.#beforeUnloadHandler) {
      window.removeEventListener("beforeunload", this.#beforeUnloadHandler);
      this.#beforeUnloadHandler = null;
    }
    this.listeners?.clear();
  }

  /**
   * Stimulus action: data-action="hub-connection#disconnectAction"
   */
  disconnectAction() {
    this.#cleanup();
    this.updateStatus("Disconnected", "");
    this.notifyListeners("disconnected");
  }

  // ========== Status UI ==========

  #refreshStatusUI() {
    const config = STATUS_CONFIG[this.state] || STATUS_CONFIG.disconnected;
    this.updateStatus(config.text);
  }

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
      if (this.state === State.ERROR) {
        statusDetail.className =
          "text-xs text-red-400/80 font-mono max-w-xs text-right";
      } else if (this.state === State.CONNECTED) {
        statusDetail.className = "text-xs text-emerald-400/60 font-mono";
      } else {
        statusDetail.className = "text-xs text-zinc-500 font-mono";
      }
    }

    // Mobile status
    const statusIconMobile = this.hasStatusIconMobileTarget
      ? this.statusIconMobileTarget
      : document.querySelector(
          "[data-hub-connection-target='statusIconMobile']",
        );
    if (statusIconMobile) {
      const mobileIcon = config.icon.replace(/size-4/g, "size-3");
      statusIconMobile.innerHTML = mobileIcon;
      statusIconMobile.className = `shrink-0 ${config.iconClass}`;
    }

    const statusTextMobile = this.hasStatusTextMobileTarget
      ? this.statusTextMobileTarget
      : document.querySelector(
          "[data-hub-connection-target='statusTextMobile']",
        );
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
      if (this.state === State.CONNECTED) {
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
      case State.CONNECTED:
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

      case State.ERROR:
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

      case State.CHANNEL_CONNECTED:
      case State.HANDSHAKE_SENT:
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
