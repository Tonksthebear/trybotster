import { Controller } from "@hotwired/stimulus";
import { loadSession, open, getHubIdFromPath } from "channels/secure_channel";

/**
 * Terminal Connection Controller
 *
 * Manages the terminal channel (data plane) — PTY I/O, resize events,
 * and PTY stream switching. One instance per agent terminal view.
 *
 * Loads its own Signal session independently via SecureChannel.
 * No base class, no hub-connection outlet — fully self-contained.
 */
export default class extends Controller {
  static targets = ["terminalBadge"];

  static values = {
    hubId: String,
  };

  #terminalHandle = null;
  #retryTimer = null;
  #pendingConnect = null;

  connect() {
    this.listeners = new Map();
    this.session = null;
    this.identityKey = null;
    this.connected = false;
    this.hubId = getHubIdFromPath() || this.hubIdValue;
    this.currentAgentIndex = null;
    this.currentPtyIndex = 0;
    this.ptyConnecting = false;
    this.#initializeSession();
  }

  disconnect() {
    clearTimeout(this.#retryTimer);
    this.#terminalHandle?.close();
    this.#terminalHandle = null;
    this.listeners?.clear();
  }

  // ========== Session Initialization ==========

  async #initializeSession() {
    try {
      this.session = await loadSession(this.hubId);
      if (!this.session) {
        this.#retryTimer = setTimeout(() => this.#initializeSession(), 2000);
        return;
      }
      this.identityKey = await this.session.getIdentityKey();
      this.connected = true;
      this.#updateTerminalBadge();
      this.notifyListeners("connected", this);

      if (this.#pendingConnect) {
        const { agentIndex, ptyIndex } = this.#pendingConnect;
        this.#pendingConnect = null;
        await this.connectToPty(agentIndex, ptyIndex);
      }
    } catch (error) {
      console.error("[TerminalConnection] Session init failed:", error);
      this.#retryTimer = setTimeout(() => this.#initializeSession(), 2000);
    }
  }

  // ========== PTY Operations ==========

  /**
   * Connect to a specific agent's PTY.
   * Subscribes to TerminalRelayChannel for data-plane I/O.
   * @param {number} agentIndex - Index of the agent
   * @param {number} ptyIndex - Index of the PTY (0=CLI, 1=Server)
   */
  async connectToPty(agentIndex, ptyIndex = 0) {
    if (!this.session) {
      this.#pendingConnect = { agentIndex, ptyIndex };
      return false;
    }

    if (
      agentIndex === this.currentAgentIndex &&
      ptyIndex === this.currentPtyIndex &&
      this.#terminalHandle
    ) {
      return true;
    }

    this.#terminalHandle?.close();

    this.ptyConnecting = true;
    this.#updateTerminalBadge();

    this.#terminalHandle = await open({
      channel: "TerminalRelayChannel",
      params: {
        hub_id: this.hubId,
        agent_index: agentIndex,
        pty_index: ptyIndex,
        browser_identity: this.identityKey,
      },
      session: this.session,
      onMessage: (msg) => {
        if (this.ptyConnecting) {
          this.ptyConnecting = false;
          this.#updateTerminalBadge();
        }
        this.notifyListeners("message", msg);
      },
      onDisconnect: () => this.#handleTerminalDisconnect(),
      onError: (err) => this.#handleChannelError(err),
    });

    this.currentAgentIndex = agentIndex;
    this.currentPtyIndex = ptyIndex;
    this.#updateUrlForPty(ptyIndex);
    this.notifyListeners("message", {
      type: "pty_channel_switched",
      agent_index: agentIndex,
      pty_index: ptyIndex,
    });
    return true;
  }

  // ========== Terminal I/O ==========

  async sendInput(data) {
    return this.#terminalHandle?.send({ type: "input", data }) ?? false;
  }

  async sendResize(cols, rows) {
    return this.#terminalHandle?.send({ type: "resize", cols, rows }) ?? false;
  }

  // ========== Public Getters ==========

  getCurrentAgentIndex() {
    return this.currentAgentIndex;
  }

  getCurrentPtyIndex() {
    return this.currentPtyIndex;
  }

  getHubId() {
    return this.hubId;
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

    if (this.connected && this.session) {
      callbacks.onConnected?.(this);
    }
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
      }
    }
  }

  // ========== Private: Disconnect / Error Handling ==========

  #handleTerminalDisconnect() {
    console.debug("[TerminalConnection] Terminal channel disconnected");
    this.connected = false;
    this.#updateTerminalBadge();
    this.notifyListeners("disconnected");
  }

  #handleChannelError(error) {
    console.error("[TerminalConnection] Channel error:", error);
    this.notifyListeners("error", error);
  }

  // ========== Private: UI ==========

  #updateTerminalBadge() {
    const terminalBadge = this.hasTerminalBadgeTarget
      ? this.terminalBadgeTarget
      : document.querySelector(
          "[data-terminal-connection-target='terminalBadge']",
        );

    if (!terminalBadge) return;

    const lockIcon = `<svg class="w-3 h-3" fill="currentColor" viewBox="0 0 20 20">
      <path fill-rule="evenodd" d="M5 9V7a5 5 0 0110 0v2a2 2 0 012 2v5a2 2 0 01-2 2H5a2 2 0 01-2-2v-5a2 2 0 012-2zm8-2v2H7V7a3 3 0 016 0z" clip-rule="evenodd"/>
    </svg>`;
    const unlockIcon = `<svg class="w-3 h-3" fill="none" stroke="currentColor" viewBox="0 0 24 24">
      <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M12 15v2m-6 4h12a2 2 0 002-2v-6a2 2 0 00-2-2H6a2 2 0 00-2 2v6a2 2 0 002 2zm10-10V7a4 4 0 00-8 0v4h8z"/>
    </svg>`;
    const spinnerIcon = `<svg class="w-3 h-3 animate-spin" fill="none" viewBox="0 0 24 24">
      <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
      <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"></path>
    </svg>`;

    if (this.connected && this.ptyConnecting) {
      terminalBadge.className =
        "inline-flex items-center gap-1 px-2 py-0.5 text-xs font-medium bg-amber-500/10 text-amber-400 rounded";
      terminalBadge.innerHTML = `${spinnerIcon}<span>Connecting to PTY...</span>`;
    } else if (this.connected && this.#terminalHandle) {
      terminalBadge.className =
        "inline-flex items-center gap-1 px-2 py-0.5 text-xs font-medium bg-emerald-500/10 text-emerald-400 rounded";
      terminalBadge.innerHTML = `${lockIcon}<span>E2E Encrypted</span>`;
    } else {
      terminalBadge.className =
        "inline-flex items-center gap-1 px-2 py-0.5 text-xs font-medium bg-zinc-700/50 text-zinc-500 rounded";
      terminalBadge.innerHTML = `${unlockIcon}<span>Connecting...</span>`;
    }
  }

  #updateUrlForPty(ptyIndex) {
    const url = new URL(window.location);
    if (ptyIndex > 0) {
      url.searchParams.set("pty", ptyIndex);
    } else {
      url.searchParams.delete("pty");
    }
    history.replaceState(null, "", url);
  }
}
