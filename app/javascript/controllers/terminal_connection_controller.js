import ConnectionController, {
  ConnectionState,
  ConnectionError,
  SignalSession,
  consumer,
  Channel,
} from "controllers/connection_controller";

/**
 * Terminal Connection Controller
 *
 * Manages the terminal channel (data plane) — PTY I/O, resize events,
 * and PTY stream switching. One instance per agent terminal view.
 *
 * Uses a hub-connection outlet to:
 * - Send connect_to_pty commands (control plane, goes over hub channel)
 * - Wait for hub handshake completion before subscribing
 *
 * Loads its own Signal session independently from IndexedDB.
 */
export default class extends ConnectionController {
  static targets = ["terminalBadge"];

  static values = {
    hubId: String,
    workerUrl: String,
    wasmJsUrl: String,
    wasmBinaryUrl: String,
  };

  static outlets = ["hub-connection"];

  connect() {
    super.connect();

    this.terminalChannel = null;
    this.terminalSubscription = null;
    this.currentAgentIndex = null;
    this.currentPtyIndex = 0;
    this.pendingConnectToPty = null;

    // Don't initialize until hub connection is ready
    // (session may not exist in IndexedDB until hub processes QR scan)
  }

  disconnect() {
    super.disconnect();
  }

  // ========== Hub Connection Outlet ==========

  hubConnectionOutletConnected(outlet) {
    // Register as listener on hub connection to know when it's connected.
    // registerListener fires onConnected immediately if hub is already connected,
    // so no explicit isConnected() check needed.
    outlet.registerListener(this, {
      onConnected: () => this.handleHubConnected(),
      onDisconnected: () => this.handleHubDisconnected(),
      onStateChange: (state) => {
        // Mirror hub state for terminal badge until we have our own connection
        if (!this.connected) {
          this.state = state;
          this.updateTerminalBadge();
        }
      },
    });
  }

  hubConnectionOutletDisconnected(outlet) {
    outlet.unregisterListener(this);
  }

  async handleHubConnected() {
    // Guard against duplicate initialization
    if (this.connected) return;

    // Hub is connected — now we can initialize our session
    const success = await this.initSession();
    if (!success) return;

    this.connected = true;
    this.setState(ConnectionState.CONNECTED);
    this.updateTerminalBadge();

    // Notify our listeners (terminal_display, terminal_view)
    this.notifyListeners("connected", this);

    // Drain any queued connectToPty call that arrived before session was ready
    if (this.pendingConnectToPty) {
      const { agentIndex, ptyIndex } = this.pendingConnectToPty;
      this.pendingConnectToPty = null;
      console.log(
        `[TerminalConnection] Draining queued connectToPty: agent ${agentIndex}, pty ${ptyIndex}`,
      );
      await this.connectToPty(agentIndex, ptyIndex);
    }
  }

  handleHubDisconnected() {
    this.connected = false;
    this.setState(ConnectionState.DISCONNECTED);
    this.updateTerminalBadge();
    this.notifyListeners("disconnected");
  }

  /**
   * Override: Terminal connection always loads from IndexedDB, never from URL fragment.
   * The QR scan bundle is consumed by HubConnection.
   */
  async setupSignalSession() {
    this.signalSession = await SignalSession.load(this.hubId);
  }

  // ========== Terminal Channel ==========

  subscribeToTerminalChannel(
    agentIndex = this.currentAgentIndex,
    ptyIndex = this.currentPtyIndex,
  ) {
    return new Promise((resolve, reject) => {
      let initialConnectionResolved = false;

      this.terminalSubscription = consumer.subscriptions.create(
        {
          channel: "TerminalRelayChannel",
          hub_id: this.hubId,
          agent_index: agentIndex,
          pty_index: ptyIndex,
          browser_identity: this.ourIdentityKey,
        },
        {
          connected: () => {
            console.log(
              `[TerminalConnection] TerminalRelayChannel connected to agent ${agentIndex} pty ${ptyIndex}`,
            );

            if (this.terminalChannel) {
              this.terminalChannel.destroy();
            }

            this.terminalChannel = Channel.builder(this.terminalSubscription)
              .session(this.signalSession)
              .reliable(true)
              .onMessage((msg) => this.handleDecryptedMessage(msg))
              .onConnect(() =>
                console.log("[TerminalConnection] Terminal channel ready"),
              )
              .onDisconnect(() =>
                console.log(
                  "[TerminalConnection] Terminal channel disconnected",
                ),
              )
              .onError((err) => this.handleChannelError(err))
              .build();
            this.terminalChannel.markConnected();
            this.currentAgentIndex = agentIndex;
            this.currentPtyIndex = ptyIndex;

            if (!initialConnectionResolved) {
              initialConnectionResolved = true;
              resolve();
            }
          },
          disconnected: () => {
            console.log(
              "[TerminalConnection] TerminalRelayChannel disconnected",
            );
            if (this.terminalChannel) {
              this.terminalChannel.destroy();
              this.terminalChannel = null;
            }
          },
          rejected: () => {
            console.error(
              "[TerminalConnection] TerminalRelayChannel subscription rejected",
            );
            reject(new Error("Terminal subscription rejected"));
          },
          received: async (data) => {
            if (data.sender_key_distribution) {
              await this.signalSession?.processSenderKeyDistribution(
                data.sender_key_distribution,
              );
              return;
            }
            if (data.error) {
              this.setError(ConnectionError.WEBSOCKET_ERROR, data.error);
              return;
            }
            if (this.terminalChannel) {
              await this.terminalChannel.receive(data);
            }
          },
        },
      );
    });
  }

  // ========== Message Handling ==========

  handleDecryptedMessage(message) {
    // Route terminal messages to listeners
    this.notifyListeners("message", message);
  }

  handleChannelError(error) {
    console.error("[TerminalConnection] Channel error:", error);

    if (error.type === "session_invalid") {
      this.clearSessionAndShowError(error.message);
    } else {
      this.setError(
        ConnectionError.WEBSOCKET_ERROR,
        error.message || "Channel error",
      );
    }
  }

  // ========== PTY Operations ==========

  /**
   * Switch to a different PTY stream.
   * @param {number} agentIndex - Index of the agent
   * @param {number} ptyIndex - Index of the PTY (0=CLI, 1=Server)
   */
  async switchToPtyStream(agentIndex, ptyIndex) {
    if (
      this.terminalSubscription &&
      agentIndex === this.currentAgentIndex &&
      ptyIndex === this.currentPtyIndex
    ) {
      return true;
    }

    if (!this.signalSession || !this.connected) {
      console.warn("[TerminalConnection] Cannot switch PTY - not connected");
      return false;
    }

    console.log(
      `[TerminalConnection] Switching from agent ${this.currentAgentIndex} pty ${this.currentPtyIndex} to agent ${agentIndex} pty ${ptyIndex}`,
    );

    const prevAgentIndex = this.currentAgentIndex;
    const prevPtyIndex = this.currentPtyIndex;

    if (this.terminalChannel) {
      this.terminalChannel.destroy();
      this.terminalChannel = null;
    }
    if (this.terminalSubscription) {
      this.terminalSubscription.unsubscribe();
      this.terminalSubscription = null;
    }

    try {
      await this.subscribeToTerminalChannel(agentIndex, ptyIndex);

      this.notifyListeners("message", {
        type: "pty_channel_switched",
        agent_index: agentIndex,
        pty_index: ptyIndex,
      });

      return true;
    } catch (error) {
      console.error(
        `[TerminalConnection] Failed to switch to agent ${agentIndex} pty ${ptyIndex}:`,
        error,
      );
      try {
        await this.subscribeToTerminalChannel(prevAgentIndex, prevPtyIndex);
      } catch (reconnectError) {
        console.error(
          "[TerminalConnection] Failed to reconnect to previous PTY:",
          reconnectError,
        );
      }
      return false;
    }
  }

  /**
   * Connect to a specific agent's PTY.
   * Sends connect_to_pty to CLI via hub channel, then subscribes to terminal channel.
   * @param {number} agentIndex - Index of the agent
   * @param {number} ptyIndex - Index of the PTY (0=CLI, 1=Server)
   */
  async connectToPty(agentIndex, ptyIndex = 0) {
    if (!this.connected || !this.signalSession) {
      // Queue for when session is ready (race: agents_controller calls before initSession completes)
      console.log(
        `[TerminalConnection] Queuing connectToPty (not ready): agent ${agentIndex}, pty ${ptyIndex}`,
      );
      this.pendingConnectToPty = { agentIndex, ptyIndex };
      return false;
    }

    console.log(
      `[TerminalConnection] Connecting to PTY: agent ${agentIndex}, pty ${ptyIndex}`,
    );

    // 1. Tell CLI to establish its side via hub channel (control plane)
    if (this.hasHubConnectionOutlet) {
      await this.hubConnectionOutlet.send("connect_to_pty", {
        agent_index: agentIndex,
        pty_index: ptyIndex,
      });
    }

    // 2. Subscribe browser-side to terminal channel (data plane)
    await this.switchToPtyStream(agentIndex, ptyIndex);

    // 3. Update URL
    this.updateUrlForPty(ptyIndex);

    return true;
  }

  updateUrlForPty(ptyIndex) {
    const url = new URL(window.location);
    if (ptyIndex > 0) {
      url.searchParams.set("pty", ptyIndex);
    } else {
      url.searchParams.delete("pty");
    }
    history.replaceState(null, "", url);
  }

  getCurrentAgentIndex() {
    return this.currentAgentIndex;
  }

  getCurrentPtyIndex() {
    return this.currentPtyIndex;
  }

  // ========== Hub Channel Proxy ==========

  /**
   * Proxy send to hub connection for control-plane messages.
   * Allows downstream controllers to call send() without knowing about hub connection.
   */
  async send(type, data = {}) {
    if (this.hasHubConnectionOutlet) {
      return this.hubConnectionOutlet.send(type, data);
    }
    console.warn("[TerminalConnection] Cannot send - no hub connection outlet");
    return false;
  }

  // ========== Terminal I/O ==========

  async sendInput(inputData) {
    return this.sendTerminalMessage({ data: inputData });
  }

  async sendResize(cols, rows) {
    return this.sendTerminalMessage({ type: "resize", cols, rows });
  }

  async sendTerminalMessage(message) {
    if (!this.terminalChannel) {
      console.warn(
        "[TerminalConnection] Cannot send terminal message - no terminal channel",
      );
      return false;
    }

    try {
      const msg = { type: message.type || "input", ...message };
      return await this.terminalChannel.send(msg);
    } catch (error) {
      console.error("[TerminalConnection] Terminal send failed:", error);
      return false;
    }
  }

  // ========== Terminal Badge UI ==========

  updateTerminalBadge() {
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

  // ========== Cleanup ==========

  cleanup() {
    if (this.terminalChannel) {
      this.terminalChannel.destroy();
      this.terminalChannel = null;
    }
    if (this.terminalSubscription) {
      this.terminalSubscription.unsubscribe();
      this.terminalSubscription = null;
    }
    super.cleanup();
  }
}
