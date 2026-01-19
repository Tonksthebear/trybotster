import { Controller } from "@hotwired/stimulus";
import consumer from "channels/consumer";
import {
  initSignal,
  SignalSession,
  parseBundleFromFragment,
  getHubIdFromPath,
  ConnectionState,
  ConnectionError,
} from "signal";
import { Channel } from "channels/channel";

/**
 * Connection Controller - Signal Protocol E2E Encryption
 *
 * This controller manages the secure connection between browser and CLI
 * via Action Cable with Signal Protocol E2E encryption.
 *
 * Connection Flow:
 * 1. LOADING_WASM - Loading Signal Protocol WASM module
 * 2. CREATING_SESSION - Setting up encryption from QR bundle
 * 3. SUBSCRIBING - Connecting to Action Cable channel
 * 4. CHANNEL_CONNECTED - Action Cable confirmed (CLI is reachable)
 * 5. HANDSHAKE_SENT - Sent encrypted handshake, waiting for CLI ACK
 * 6. CONNECTED - CLI acknowledged, E2E encryption active
 *
 * Each step shows clear status. Failures show specific reasons.
 */

// Handshake timeout in milliseconds
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

// Status display configuration per state
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
    "terminalBadge",
    "shareBtn",
    "shareStatus",
  ];

  static values = {
    hubId: String,
    workerUrl: String,
    wasmJsUrl: String,
    wasmBinaryUrl: String,
    agentIndex: { type: Number, default: 0 }, // Per-agent channel routing
  };

  static classes = ["securityBannerBase"];

  connect() {
    this.signalSession = null;
    this.hubChannel = null; // Hub channel with reliability (agent list, creation progress)
    this.hubSubscription = null; // Raw ActionCable subscription for hub (internal)
    this.terminalChannel = null; // Terminal channel with reliability (PTY I/O)
    this.terminalSubscription = null; // Raw ActionCable subscription for terminal (internal)
    this.subscription = null; // Legacy alias for terminalSubscription
    this.hubId = null;
    this.ourIdentityKey = null;
    this.connected = false;
    this.state = ConnectionState.DISCONNECTED;
    this.errorReason = null;
    this.handshakeTimer = null;
    this.currentAgentIndex = this.agentIndexValue; // Track which agent we're subscribed to

    // Don't overwrite listeners - outlet callbacks may have already registered
    if (!this.listeners) {
      this.listeners = new Map();
    }

    // Initialize and connect
    this.initializeConnection();
  }

  disconnect() {
    this.cleanup();
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
    if (this.connected && this.signalSession) {
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

  // Notify all listeners of an event
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

  // ========== Connection Logic ==========

  async initializeConnection() {
    // Get hub ID from URL path
    this.hubId = getHubIdFromPath();
    if (!this.hubId && this.hubIdValue) {
      this.hubId = this.hubIdValue;
    }

    if (!this.hubId) {
      this.setError(ConnectionError.NO_BUNDLE, "Hub ID not found in URL");
      return;
    }

    try {
      // Step 1: Load Signal WASM
      this.setState(ConnectionState.LOADING_WASM);
      this.updateStatus(
        "Loading encryption...",
        "Initializing Signal Protocol",
      );

      try {
        await initSignal(
          this.workerUrlValue,
          this.wasmJsUrlValue,
          this.wasmBinaryUrlValue,
        );
      } catch (wasmError) {
        this.setError(
          ConnectionError.WASM_LOAD_FAILED,
          `Failed to load encryption: ${wasmError.message}`,
        );
        return;
      }

      // Step 2: Set up Signal session
      this.setState(ConnectionState.CREATING_SESSION);
      this.updateStatus("Setting up encryption...", "Processing security keys");

      try {
        await this.setupSignalSession();
      } catch (sessionError) {
        this.setError(
          ConnectionError.SESSION_CREATE_FAILED,
          `Encryption setup failed: ${sessionError.message}`,
        );
        return;
      }

      if (!this.signalSession) {
        this.setError(
          ConnectionError.NO_BUNDLE,
          "No encryption bundle. Scan QR code to connect.",
        );
        return;
      }

      // Get our identity key to filter out our own messages
      this.ourIdentityKey = await this.signalSession.getIdentityKey();

      // Step 3: Subscribe to Action Cable channel
      this.setState(ConnectionState.SUBSCRIBING);
      this.updateStatus(
        "Connecting to server...",
        "Establishing secure channel",
      );

      try {
        await this.subscribeToChannel();
      } catch (subError) {
        this.setError(
          ConnectionError.SUBSCRIBE_REJECTED,
          `Connection rejected: ${subError.message}`,
        );
        return;
      }

      // Step 4: Channel connected - CLI is reachable
      this.setState(ConnectionState.CHANNEL_CONNECTED);
      this.updateStatus("CLI reachable", "Sending encrypted handshake...");

      // Step 5: Send handshake and wait for ACK
      this.setState(ConnectionState.HANDSHAKE_SENT);
      await this.sendHandshakeWithTimeout();

      // Note: Connection completes when we receive handshake_ack in handleDecryptedMessage
    } catch (error) {
      console.error("[Connection] Failed to initialize:", error);
      this.setError(
        ConnectionError.WEBSOCKET_ERROR,
        `Connection error: ${error.message}`,
      );
    }
  }

  async setupSignalSession() {
    // Check for bundle in URL fragment (fresh QR code scan)
    const urlBundle = parseBundleFromFragment();

    if (urlBundle) {
      // Fresh bundle from QR code - always use it (replaces any cached session)
      console.log("[Connection] Creating new session from URL bundle");
      this.signalSession = await SignalSession.create(urlBundle, this.hubId);
      // Clear fragment after successful session creation (clean URL)
      if (window.history.replaceState) {
        window.history.replaceState(
          null,
          "",
          window.location.pathname + window.location.search,
        );
      }
    } else {
      // No URL bundle - try to restore from IndexedDB
      this.signalSession = await SignalSession.load(this.hubId);

      if (this.signalSession) {
        console.log(
          "[Connection] Restored cached session for hub:",
          this.hubId,
        );
      }
      // If no cached session, user needs to scan QR code
    }
  }

  subscribeToChannel(agentIndex = this.currentAgentIndex) {
    // Subscribe to both HubChannel and TerminalRelayChannel
    return Promise.all([
      this.subscribeToHubChannel(),
      this.subscribeToTerminalChannel(agentIndex),
    ]).then(() => {
      // Both connected successfully
    });
  }

  subscribeToHubChannel() {
    return new Promise((resolve, reject) => {
      // Create raw ActionCable subscription
      this.hubSubscription = consumer.subscriptions.create(
        {
          channel: "HubChannel",
          hub_id: this.hubId,
          browser_identity: this.ourIdentityKey,
        },
        {
          connected: () => {
            console.log("[Connection] HubChannel connected");
            // Create Channel wrapper with E2E encryption and reliable delivery
            this.hubChannel = Channel.builder(this.hubSubscription)
              .session(this.signalSession)
              .reliable(true)
              .onMessage((msg) => this.handleDecryptedMessage(msg))
              .onConnect(() => console.log("[Connection] Hub channel ready"))
              .onDisconnect(() => this.handleDisconnect())
              .build();
            this.hubChannel.markConnected();
            resolve();
          },
          disconnected: () => {
            console.log("[Connection] HubChannel disconnected");
            // Clean up channel
            if (this.hubChannel) {
              this.hubChannel.destroy();
              this.hubChannel = null;
            }
            // Hub disconnect is critical - trigger reconnect
            this.handleDisconnect();
          },
          rejected: () => {
            console.error("[Connection] HubChannel subscription rejected");
            reject(new Error("Hub subscription rejected - hub may be offline"));
          },
          received: async (data) => {
            // Route through Channel's receive method (handles decryption + reliability)
            if (this.hubChannel) {
              await this.hubChannel.receive(data);
            }
          },
        },
      );
    });
  }

  subscribeToTerminalChannel(agentIndex = this.currentAgentIndex) {
    return new Promise((resolve, reject) => {
      this.terminalSubscription = consumer.subscriptions.create(
        {
          channel: "TerminalRelayChannel",
          hub_id: this.hubId,
          agent_index: agentIndex, // Per-agent channel routing
          browser_identity: this.ourIdentityKey,
        },
        {
          connected: () => {
            console.log(
              `[Connection] TerminalRelayChannel connected to agent ${agentIndex}`,
            );
            // Create Channel wrapper with E2E encryption and reliable delivery
            this.terminalChannel = Channel.builder(this.terminalSubscription)
              .session(this.signalSession)
              .reliable(true)
              .onMessage((msg) => this.handleDecryptedMessage(msg))
              .onConnect(() =>
                console.log("[Connection] Terminal channel ready"),
              )
              .onDisconnect(() =>
                console.log("[Connection] Terminal channel disconnected"),
              )
              .build();
            this.terminalChannel.markConnected();
            this.currentAgentIndex = agentIndex;
            resolve();
          },
          disconnected: () => {
            console.log("[Connection] TerminalRelayChannel disconnected");
            // Clean up channel
            if (this.terminalChannel) {
              this.terminalChannel.destroy();
              this.terminalChannel = null;
            }
          },
          rejected: () => {
            console.error(
              "[Connection] TerminalRelayChannel subscription rejected",
            );
            reject(new Error("Terminal subscription rejected"));
          },
          received: async (data) => {
            // Handle special server messages before Channel processing
            if (data.sender_key_distribution) {
              await this.signalSession?.processSenderKeyDistribution(
                data.sender_key_distribution,
              );
              console.log("[Connection] Processed SenderKey distribution");
              return;
            }
            if (data.error) {
              this.setError(ConnectionError.WEBSOCKET_ERROR, data.error);
              return;
            }
            // Route encrypted messages through Channel (handles decryption + reliability)
            if (this.terminalChannel) {
              await this.terminalChannel.receive(data);
            }
          },
        },
      );
      // Legacy alias
      this.subscription = this.terminalSubscription;
    });
  }

  async sendHandshakeWithTimeout() {
    // Send encrypted handshake
    const handshake = {
      type: "connected",
      device_name: this.getDeviceName(),
      timestamp: Date.now(),
    };

    console.log("[Connection] Sending handshake:", handshake);
    const sent = await this.sendEncrypted(handshake);

    if (!sent) {
      this.setError(
        ConnectionError.HANDSHAKE_FAILED,
        "Failed to send handshake",
      );
      return;
    }

    this.updateStatus("Handshake sent", "Waiting for CLI acknowledgment...");

    // Start timeout for handshake ACK
    this.handshakeTimer = setTimeout(() => {
      if (this.state === ConnectionState.HANDSHAKE_SENT) {
        console.warn("[Connection] Handshake timeout - no ACK from CLI");
        this.setError(
          ConnectionError.HANDSHAKE_TIMEOUT,
          "CLI did not respond. Try refreshing the page.",
        );
        // DON'T clear session on timeout - this is likely a transient network issue,
        // not a session problem. Clearing would force unnecessary QR re-scan.
        // User can refresh to retry with existing session.
      }
    }, HANDSHAKE_TIMEOUT_MS);
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

  handleDecryptedMessage(message) {
    // Handle handshake acknowledgment
    if (message.type === "handshake_ack") {
      console.log("[Connection] Received handshake ACK from CLI");

      // Clear timeout
      if (this.handshakeTimer) {
        clearTimeout(this.handshakeTimer);
        this.handshakeTimer = null;
      }

      // Complete connection
      this.connected = true;
      this.setState(ConnectionState.CONNECTED);
      this.updateStatus(
        "Connected",
        `E2E encrypted to ${this.hubId.substring(0, 8)}...`,
      );

      // Notify all registered listeners
      this.notifyListeners("connected", this);
      return;
    }

    // Handle invite bundle response
    if (message.type === "invite_bundle") {
      console.log("[Connection] Received invite bundle from CLI");
      this.handleInviteBundle(message);
      return;
    }

    // Route other messages to listeners
    if (typeof message === "object" && message.type) {
      switch (message.type) {
        case "output":
        case "agents":
        case "worktrees":
        case "agent_selected":
        case "agent_creating":
        case "agent_created":
        case "scrollback":
          this.notifyListeners("message", message);
          break;
        default:
          this.notifyListeners("message", message);
      }
    } else {
      // Raw output
      this.notifyListeners("message", { type: "output", data: message });
    }
  }

  handleDisconnect() {
    this.connected = false;
    this.setState(ConnectionState.DISCONNECTED);
    this.updateStatus("Disconnected", "Connection lost");
    this.notifyListeners("disconnected");
  }

  // ========== Public API for Outlets ==========

  /**
   * Send a JSON message to CLI (encrypted).
   */
  async send(type, data = {}) {
    if (!this.subscription || !this.connected || !this.signalSession) {
      console.warn("[Connection] Cannot send - not connected");
      return false;
    }

    const message = { type, ...data };
    console.log("[Connection] Sending message:", type);
    return await this.sendEncrypted(message);
  }

  /**
   * Send encrypted message via Hub Channel with reliable delivery.
   * The Channel handles reliability (seq numbers, ACKs, retransmit) and
   * encryption internally.
   */
  async sendEncrypted(message) {
    if (this.hubChannel) {
      // Send through hub channel (handles reliability + encryption)
      return await this.hubChannel.send(message);
    } else {
      // Fallback: no channel yet (during early setup)
      try {
        const envelope = await this.signalSession.encrypt(message);
        this.hubSubscription.perform("relay", { envelope });
        return true;
      } catch (error) {
        console.error("[Connection] Encryption failed:", error);
        return false;
      }
    }
  }

  /**
   * Send raw input to CLI (terminal keystrokes).
   * Uses terminal channel for PTY I/O (not hub channel).
   */
  async sendInput(inputData) {
    return await this.sendTerminalMessage("input", { data: inputData });
  }

  /**
   * Resize the terminal.
   * Uses terminal channel for PTY I/O (not hub channel).
   */
  async sendResize(cols, rows) {
    return await this.sendTerminalMessage("resize", { cols, rows });
  }

  /**
   * Send message via terminal channel (for PTY I/O).
   * Separate from hub channel which handles agent list, creation, etc.
   */
  async sendTerminalMessage(type, data = {}) {
    if (!this.terminalChannel || !this.connected || !this.signalSession) {
      console.warn("[Connection] Cannot send terminal message - not connected");
      return false;
    }

    const message = { type, ...data };
    console.log("[Connection] Sending terminal message:", type);
    return await this.terminalChannel.send(message);
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

  /**
   * Switch to a different agent's channel.
   * This resubscribes to the new agent's terminal stream.
   * @param {number} agentIndex - Index of the agent to switch to
   */
  async switchToAgentChannel(agentIndex) {
    if (agentIndex === this.currentAgentIndex) {
      console.log(`[Connection] Already subscribed to agent ${agentIndex}`);
      return true;
    }

    if (!this.signalSession || !this.connected) {
      console.warn("[Connection] Cannot switch agent - not connected");
      return false;
    }

    console.log(
      `[Connection] Switching from agent ${this.currentAgentIndex} to ${agentIndex}`,
    );

    // Clean up current agent's terminal channel (keep hub subscription)
    if (this.terminalChannel) {
      this.terminalChannel.destroy();
      this.terminalChannel = null;
    }
    if (this.terminalSubscription) {
      this.terminalSubscription.unsubscribe();
      this.terminalSubscription = null;
      this.subscription = null;
    }

    // Subscribe to new agent's terminal channel
    try {
      await this.subscribeToTerminalChannel(agentIndex);
      console.log(`[Connection] Switched to agent ${agentIndex}`);

      // Notify listeners of agent switch
      this.notifyListeners("message", {
        type: "agent_channel_switched",
        agent_index: agentIndex,
      });

      return true;
    } catch (error) {
      console.error(
        `[Connection] Failed to switch to agent ${agentIndex}:`,
        error,
      );
      // Try to reconnect to previous agent's terminal channel
      try {
        await this.subscribeToTerminalChannel(this.currentAgentIndex);
      } catch (reconnectError) {
        console.error(
          "[Connection] Failed to reconnect to previous agent:",
          reconnectError,
        );
        this.handleDisconnect();
      }
      return false;
    }
  }

  /**
   * Get the current agent index we're subscribed to.
   */
  getCurrentAgentIndex() {
    return this.currentAgentIndex;
  }

  deleteAgent(agentId, deleteWorktree = false) {
    return this.send("delete_agent", {
      id: agentId,
      delete_worktree: deleteWorktree,
    });
  }

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
    if (this.signalSession) {
      await this.signalSession.clear();
      this.signalSession = null;
    }
    this.cleanup();
    this.setError(
      ConnectionError.SESSION_CREATE_FAILED,
      "Session cleared. Scan QR code to reconnect.",
    );
  }

  // ========== Share Hub ==========

  /**
   * Request an invite bundle from CLI for sharing hub connection.
   * Triggered by Share Hub button click.
   */
  async requestInviteBundle() {
    if (!this.connected || !this.signalSession) {
      console.warn("[Connection] Cannot request invite - not connected");
      this.updateShareStatus("Not connected", "error");
      return;
    }

    console.log("[Connection] Requesting invite bundle from CLI");
    this.updateShareStatus("Generating...", "loading");

    const success = await this.send("generate_invite");
    if (!success) {
      this.updateShareStatus("Failed to request", "error");
    }
    // Response will be handled by handleInviteBundle()
  }

  /**
   * Handle invite bundle response from CLI.
   * Copies URL to clipboard and/or uses native share.
   */
  async handleInviteBundle(message) {
    const { url, bundle } = message;

    if (!url) {
      console.error("[Connection] Invite bundle missing URL");
      this.updateShareStatus("Invalid response", "error");
      return;
    }

    console.log(
      "[Connection] Received invite URL:",
      url.substring(0, 50) + "...",
    );

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
        // User cancelled or share failed, fall back to clipboard
        if (err.name !== "AbortError") {
          console.warn("[Connection] Native share failed:", err);
        }
      }
    }

    // Copy to clipboard
    try {
      await navigator.clipboard.writeText(url);
      this.updateShareStatus("Copied to clipboard!", "success");
      console.log("[Connection] Invite URL copied to clipboard");
    } catch (err) {
      console.error("[Connection] Failed to copy to clipboard:", err);
      // Show URL in a prompt as fallback
      prompt("Copy this link to share:", url);
      this.updateShareStatus("Copy the link above", "info");
    }
  }

  /**
   * Update share button status display.
   */
  updateShareStatus(text, state) {
    if (this.hasShareStatusTarget) {
      this.shareStatusTarget.textContent = text;

      // Clear after delay (except for loading state)
      if (state !== "loading") {
        setTimeout(() => {
          if (this.hasShareStatusTarget) {
            this.shareStatusTarget.textContent = "";
          }
        }, 3000);
      }
    }

    // Update button state
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

  // ========== State Management ==========

  setState(state) {
    const prevState = this.state;
    this.state = state;
    if (state !== ConnectionState.ERROR) {
      this.errorReason = null;
    }
    console.log(`[Connection] State: ${prevState} -> ${state}`);
    this.notifyListeners("stateChange", { state, reason: this.errorReason });
  }

  setError(reason, message) {
    this.errorReason = reason;
    this.setState(ConnectionState.ERROR);
    this.updateStatus("Connection failed", message);
    console.error(`[Connection] Error (${reason}): ${message}`);
    this.notifyListeners("error", { reason, message });
  }

  // ========== Cleanup ==========

  cleanup() {
    if (this.handshakeTimer) {
      clearTimeout(this.handshakeTimer);
      this.handshakeTimer = null;
    }
    // Clean up hub channel (includes reliable delivery layer)
    if (this.hubChannel) {
      this.hubChannel.destroy();
      this.hubChannel = null;
    }
    // Unsubscribe from hub subscription
    if (this.hubSubscription) {
      this.hubSubscription.unsubscribe();
      this.hubSubscription = null;
    }
    // Clean up terminal channel
    if (this.terminalChannel) {
      this.terminalChannel.destroy();
      this.terminalChannel = null;
    }
    if (this.terminalSubscription) {
      this.terminalSubscription.unsubscribe();
      this.terminalSubscription = null;
    }
    this.subscription = null; // Legacy alias
    this.connected = false;
    this.listeners?.clear();
  }

  disconnectAction() {
    this.cleanup();
    this.updateStatus("Disconnected", "");
    this.notifyListeners("disconnected");
  }

  // ========== Helpers ==========

  updateStatus(text, detail = "") {
    // Update based on current state
    const config = STATUS_CONFIG[this.state] || STATUS_CONFIG.disconnected;

    // Find status elements - use targets if available, fallback to global query
    // This supports both: controller wrapping content, and separate permanent container
    const statusIcon = this.hasStatusIconTarget
      ? this.statusIconTarget
      : document.querySelector("[data-connection-target='statusIcon']");
    const statusText = this.hasStatusTextTarget
      ? this.statusTextTarget
      : document.querySelector("[data-connection-target='statusText']");

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
      : document.querySelector("[data-connection-target='statusDetail']");
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

    // Update mobile status (compact)
    const statusIconMobile = this.hasStatusIconMobileTarget
      ? this.statusIconMobileTarget
      : document.querySelector("[data-connection-target='statusIconMobile']");
    if (statusIconMobile) {
      const mobileIcon = config.icon.replace(/size-4/g, "size-3");
      statusIconMobile.innerHTML = mobileIcon;
      statusIconMobile.className = `shrink-0 ${config.iconClass}`;
    }

    const statusTextMobile = this.hasStatusTextMobileTarget
      ? this.statusTextMobileTarget
      : document.querySelector("[data-connection-target='statusTextMobile']");
    if (statusTextMobile) {
      const shortText = (text || config.text)
        .replace("Initializing...", "Init...")
        .replace("Connecting...", "...")
        .replace("Connected", "Live");
      statusTextMobile.textContent = shortText;
      statusTextMobile.className = `text-xs shrink-0 ${config.textClass}`;
    }

    // Show/hide disconnect button
    const disconnectBtn = this.hasDisconnectBtnTarget
      ? this.disconnectBtnTarget
      : document.querySelector("[data-connection-target='disconnectBtn']");
    if (disconnectBtn) {
      if (this.state === ConnectionState.CONNECTED) {
        disconnectBtn.classList.remove("hidden");
      } else {
        disconnectBtn.classList.add("hidden");
      }
    }

    // Update security banner
    this.updateSecurityBanner();
  }

  updateSecurityBanner() {
    // Find elements with fallback to global query
    const securityBanner = this.hasSecurityBannerTarget
      ? this.securityBannerTarget
      : document.querySelector("[data-connection-target='securityBanner']");
    const securityIcon = this.hasSecurityIconTarget
      ? this.securityIconTarget
      : document.querySelector("[data-connection-target='securityIcon']");
    const securityText = this.hasSecurityTextTarget
      ? this.securityTextTarget
      : document.querySelector("[data-connection-target='securityText']");

    if (!securityBanner) {
      // No security banner on this page, just update terminal badge
      this.updateTerminalBadge();
      return;
    }

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

    // Update terminal badge
    this.updateTerminalBadge();
  }

  updateTerminalBadge() {
    // Find terminal badge with fallback to global query
    const terminalBadge = this.hasTerminalBadgeTarget
      ? this.terminalBadgeTarget
      : document.querySelector("[data-connection-target='terminalBadge']");

    if (!terminalBadge) return;

    const lockIcon = `<svg class="w-3 h-3" fill="currentColor" viewBox="0 0 20 20">
      <path fill-rule="evenodd" d="M5 9V7a5 5 0 0110 0v2a2 2 0 012 2v5a2 2 0 01-2 2H5a2 2 0 01-2-2v-5a2 2 0 012-2zm8-2v2H7V7a3 3 0 016 0z" clip-rule="evenodd"/>
    </svg>`;
    const unlockIcon = `<svg class="w-3 h-3" fill="none" stroke="currentColor" viewBox="0 0 24 24">
      <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M12 15v2m-6 4h12a2 2 0 002-2v-6a2 2 0 00-2-2H6a2 2 0 00-2 2v6a2 2 0 002 2zm10-10V7a4 4 0 00-8 0v4h8z"/>
    </svg>`;

    switch (this.state) {
      case ConnectionState.CONNECTED:
        terminalBadge.className =
          "inline-flex items-center gap-1 px-2 py-0.5 text-xs font-medium bg-emerald-500/10 text-emerald-400 rounded";
        terminalBadge.innerHTML = `${lockIcon}<span>E2E Encrypted</span>`;
        break;

      case ConnectionState.ERROR:
        terminalBadge.className =
          "inline-flex items-center gap-1 px-2 py-0.5 text-xs font-medium bg-red-500/10 text-red-400 rounded";
        terminalBadge.innerHTML = `${unlockIcon}<span>Not Connected</span>`;
        break;

      case ConnectionState.CHANNEL_CONNECTED:
      case ConnectionState.HANDSHAKE_SENT:
        terminalBadge.className =
          "inline-flex items-center gap-1 px-2 py-0.5 text-xs font-medium bg-amber-500/10 text-amber-400 rounded";
        terminalBadge.innerHTML = `${unlockIcon}<span>Handshaking...</span>`;
        break;

      default:
        terminalBadge.className =
          "inline-flex items-center gap-1 px-2 py-0.5 text-xs font-medium bg-zinc-700/50 text-zinc-500 rounded";
        terminalBadge.innerHTML = `${unlockIcon}<span>Connecting...</span>`;
        break;
    }
  }

  // Update status display for state change
  updateStatusForState() {
    const config = STATUS_CONFIG[this.state] || STATUS_CONFIG.disconnected;
    this.updateStatus(config.text, "");
  }

  // Legacy method for backwards compatibility
  emitError(message) {
    this.setError(ConnectionError.WEBSOCKET_ERROR, message);
  }
}
