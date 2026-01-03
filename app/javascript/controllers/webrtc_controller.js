import { Controller } from "@hotwired/stimulus";

// Lazy load xterm to avoid blocking if import fails
let Terminal, FitAddon;

// WebRTC P2P connection to local CLI for agent management
// Supports two modes:
// - TUI mode: streams the full hub terminal interface
// - GUI mode: shows agent list with individual terminal view (mobile-friendly)
export default class extends Controller {
  static targets = [
    "terminal", // Terminal container
    "status", // Connection status indicator
    "connectButton", // Connect/disconnect button
    "agentList", // Agent list container (GUI mode)
    "modeToggle", // Toggle between TUI and GUI modes
    "tuiContainer", // Terminal panel (expands in TUI mode)
    "guiContainer", // Agent list panel (GUI mode only)
    "terminalTitle", // Terminal title text
    "selectedAgentLabel", // Shows selected agent name
    "newAgentModal", // New agent creation modal
    "worktreeList", // Available worktrees list in modal
    "issueOrBranch", // Issue number or branch name input
    "agentPrompt", // Optional prompt input
    "newAgentButton", // New agent button
    "closeAgentModal", // Close agent confirmation modal
    "closeAgentName", // Agent name in close modal
    "closeAgentButton", // Close agent button
  ];

  static values = {
    csrfToken: String,
    pollInterval: { type: Number, default: 1000 },
    mode: { type: String, default: "gui" }, // "tui" or "gui"
    iceServers: { type: Array, default: [] }, // ICE servers (STUN + TURN)
  };

  connect() {
    console.log("WebRTC controller connected");
    this.peerConnection = null;
    this.dataChannel = null;
    this.terminal = null;
    this.fitAddon = null;
    this.sessionId = null;
    this.pollTimer = null;
    this.agents = [];
    this.selectedAgentId = null;
    this.worktrees = []; // Available worktrees from CLI
    this.currentRepo = null; // Current repo from CLI

    // Reconnection state
    this.reconnectAttempts = 0;
    this.maxReconnectAttempts = 5;
    this.reconnectTimer = null;
    this.isReconnecting = false;

    this.updateStatus("disconnected", "Not connected");

    // Log ICE server configuration
    if (this.iceServersValue.length > 0) {
      const hasTurn = this.iceServersValue.some((s) =>
        (Array.isArray(s.urls) ? s.urls : [s.urls]).some((u) =>
          u.startsWith("turn:"),
        ),
      );
      console.log(
        `ICE servers configured: ${this.iceServersValue.length} server(s), TURN: ${hasTurn ? "yes" : "no"}`,
      );
    }

    // Listen for modal closed events to do cleanup
    this.boundHandleModalClosed = this.handleModalClosed.bind(this);
    this.element.addEventListener("modal:closed", this.boundHandleModalClosed);

    // Lazy load xterm
    this.loadXterm();
  }

  // Handle modal closed events for cleanup
  handleModalClosed(event) {
    const modal = event.target;
    if (modal === this.newAgentModalTarget) {
      this.cleanupNewAgentModal();
    }
    // Close agent modal doesn't need cleanup
  }

  async loadXterm() {
    try {
      const xtermModule = await import("@xterm/xterm");
      const fitModule = await import("@xterm/addon-fit");
      Terminal = xtermModule.Terminal || xtermModule.default?.Terminal;
      FitAddon = fitModule.FitAddon || fitModule.default?.FitAddon;
      console.log("xterm loaded successfully");
    } catch (error) {
      console.error("Failed to load xterm:", error);
    }
  }

  disconnect() {
    if (this.boundHandleModalClosed) {
      this.element.removeEventListener(
        "modal:closed",
        this.boundHandleModalClosed,
      );
    }
    this.cleanup();
  }

  // Toggle between TUI and GUI modes
  toggleMode() {
    this.modeValue = this.modeValue === "tui" ? "gui" : "tui";
    this.updateModeDisplay();

    // Send mode change to CLI
    this.sendMode();

    // Request agent list when switching to GUI mode
    if (this.modeValue === "gui" && this.dataChannel?.readyState === "open") {
      this.requestAgentList();
    }
  }

  // Send current mode to CLI
  sendMode() {
    if (this.dataChannel?.readyState === "open") {
      console.log(`Sending mode: ${this.modeValue}`);
      this.sendMessage({ type: "set_mode", mode: this.modeValue });
    }
  }

  updateModeDisplay() {
    if (this.hasGuiContainerTarget && this.hasTuiContainerTarget) {
      if (this.modeValue === "tui") {
        // TUI mode: hide agent list, terminal takes full width
        this.guiContainerTarget.classList.add("hidden");
        this.tuiContainerTarget.classList.remove("lg:col-span-3");
        this.tuiContainerTarget.classList.add("lg:col-span-4");
      } else {
        // GUI mode: show agent list, terminal takes 3/4 width
        this.guiContainerTarget.classList.remove("hidden");
        this.tuiContainerTarget.classList.remove("lg:col-span-4");
        this.tuiContainerTarget.classList.add("lg:col-span-3");
      }
    }

    if (this.hasModeToggleTarget) {
      this.modeToggleTarget.textContent =
        this.modeValue === "tui" ? "Switch to GUI" : "Switch to TUI";
    }

    if (this.hasTerminalTitleTarget) {
      this.terminalTitleTarget.textContent =
        this.modeValue === "tui" ? "Hub Terminal (TUI)" : "Agent Terminal";
    }

    // Clear and resize terminal when switching modes
    if (this.terminal) {
      this.terminal.clear();
      requestAnimationFrame(() => {
        if (this.fitAddon) {
          this.fitAddon.fit();
          this.sendResize();
        }
      });
    }
  }

  // Called when user clicks "Connect to Hub"
  async startConnection() {
    console.log("startConnection called");

    if (this.peerConnection) {
      console.log("Existing connection, cleaning up");
      this.cleanup();
      return;
    }

    this.updateStatus("connecting", "Creating connection...");
    this.connectButtonTarget.disabled = true;
    this.connectButtonTarget.textContent = "Connecting...";

    try {
      await this.createPeerConnection();
      await this.createOffer();
      this.startPollingForAnswer();
    } catch (error) {
      console.error("Connection failed:", error);
      this.updateStatus("error", `Connection failed: ${error.message}`);
      this.cleanup();
    }
  }

  async createPeerConnection() {
    // Use configured ICE servers (includes TURN if available)
    const iceServers =
      this.iceServersValue.length > 0
        ? this.iceServersValue
        : [
            { urls: "stun:stun.l.google.com:19302" },
            { urls: "stun:stun1.l.google.com:19302" },
          ];

    const config = { iceServers };
    console.log(
      "Creating peer connection with ICE servers:",
      iceServers.map((s) => s.urls),
    );

    this.peerConnection = new RTCPeerConnection(config);

    this.dataChannel = this.peerConnection.createDataChannel("hub", {
      ordered: true,
    });

    this.dataChannel.onopen = () => {
      console.log("Data channel opened");
      this.updateStatus("connected", "Connected to Hub");
      this.connectButtonTarget.textContent = "Disconnect";
      this.connectButtonTarget.disabled = false;

      // Reset reconnection state on successful connection
      this.reconnectAttempts = 0;
      this.isReconnecting = false;

      // Initialize terminal
      this.initializeTerminal();

      // Send current mode to CLI immediately
      this.sendMode();

      // Request agent list for GUI mode
      if (this.modeValue === "gui") {
        this.requestAgentList();
      }

      // Re-fit and send resize after a short delay to ensure everything is ready
      setTimeout(() => {
        if (this.fitAddon && this.terminal) {
          this.fitAddon.fit();
          console.log(
            `Post-connection fit: ${this.terminal.cols}x${this.terminal.rows}`,
          );
          this.sendResize();
        }
      }, 500);
    };

    this.dataChannel.onclose = () => {
      console.log("Data channel closed");
      this.handleDisconnection("Data channel closed");
    };

    this.dataChannel.onmessage = (event) => {
      this.handleMessage(JSON.parse(event.data));
    };

    this.dataChannel.onerror = (error) => {
      console.error("Data channel error:", error);
      this.updateStatus("error", "Data channel error");
    };

    this.peerConnection.oniceconnectionstatechange = () => {
      const state = this.peerConnection.iceConnectionState;
      console.log("ICE connection state:", state);

      if (state === "disconnected") {
        // Try ICE restart first before full reconnection
        console.log("ICE disconnected, attempting ICE restart...");
        this.updateStatus("connecting", "Reconnecting...");
        this.attemptIceRestart();
      } else if (state === "failed") {
        console.log("ICE failed, will attempt full reconnection");
        this.handleDisconnection("ICE connection failed");
      } else if (state === "connected") {
        // ICE restart succeeded
        if (this.isReconnecting) {
          console.log("ICE restart succeeded");
          this.updateStatus("connected", "Connected to Hub");
          this.isReconnecting = false;
          this.reconnectAttempts = 0;
        }
      }
    };

    console.log("Peer connection created successfully");
  }

  // Attempt ICE restart without full reconnection
  attemptIceRestart() {
    if (
      !this.peerConnection ||
      this.peerConnection.connectionState === "closed"
    ) {
      console.log("Cannot restart ICE - connection closed");
      this.handleDisconnection("Connection closed");
      return;
    }

    try {
      this.isReconnecting = true;
      this.peerConnection.restartIce();
      console.log("ICE restart initiated");

      // If ICE restart doesn't resolve within 5 seconds, do full reconnect
      setTimeout(() => {
        if (
          this.isReconnecting &&
          this.peerConnection?.iceConnectionState !== "connected"
        ) {
          console.log("ICE restart timed out, attempting full reconnection");
          this.handleDisconnection("ICE restart timed out");
        }
      }, 5000);
    } catch (error) {
      console.error("ICE restart failed:", error);
      this.handleDisconnection("ICE restart failed");
    }
  }

  // Handle disconnection with automatic reconnection
  handleDisconnection(reason) {
    console.log(`Disconnection: ${reason}`);

    // Don't reconnect if user manually disconnected or max attempts reached
    if (this.reconnectAttempts >= this.maxReconnectAttempts) {
      console.log("Max reconnection attempts reached");
      this.updateStatus("error", "Connection lost - max retries exceeded");
      this.cleanup();
      return;
    }

    // Clean up current connection but preserve terminal
    this.cleanupConnection();

    // Calculate backoff delay: 1s, 2s, 4s, 8s, 16s
    const delay = Math.min(1000 * Math.pow(2, this.reconnectAttempts), 16000);
    this.reconnectAttempts++;

    console.log(
      `Reconnection attempt ${this.reconnectAttempts}/${this.maxReconnectAttempts} in ${delay}ms`,
    );
    this.updateStatus(
      "connecting",
      `Reconnecting (${this.reconnectAttempts}/${this.maxReconnectAttempts})...`,
    );

    this.reconnectTimer = setTimeout(() => {
      this.isReconnecting = true;
      this.startConnection();
    }, delay);
  }

  // Clean up connection state but preserve terminal
  cleanupConnection() {
    if (this.pollTimer) {
      clearInterval(this.pollTimer);
      this.pollTimer = null;
    }

    if (this.dataChannel) {
      this.dataChannel.close();
      this.dataChannel = null;
    }

    if (this.peerConnection) {
      this.peerConnection.close();
      this.peerConnection = null;
    }

    this.sessionId = null;
  }

  async createOffer() {
    const offer = await this.peerConnection.createOffer();
    await this.peerConnection.setLocalDescription(offer);

    await new Promise((resolve) => {
      if (this.peerConnection.iceGatheringState === "complete") {
        resolve();
      } else {
        const checkState = () => {
          if (this.peerConnection.iceGatheringState === "complete") {
            resolve();
          }
        };
        this.peerConnection.onicegatheringstatechange = checkState;
        setTimeout(resolve, 5000); // Timeout
      }
    });

    const completeOffer = this.peerConnection.localDescription;
    this.updateStatus("connecting", "Sending offer to server...");

    const response = await fetch("/api/webrtc/sessions", {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        "X-CSRF-Token": this.csrfTokenValue,
      },
      body: JSON.stringify({
        offer: {
          type: completeOffer.type,
          sdp: completeOffer.sdp,
        },
      }),
    });

    if (!response.ok) {
      throw new Error(`Server error: ${response.status}`);
    }

    const data = await response.json();
    this.sessionId = data.session_id;
    this.updateStatus("connecting", "Waiting for CLI to respond...");
  }

  startPollingForAnswer() {
    this.pollTimer = setInterval(async () => {
      try {
        const response = await fetch(`/api/webrtc/sessions/${this.sessionId}`, {
          headers: { "X-CSRF-Token": this.csrfTokenValue },
        });

        if (!response.ok) {
          if (response.status === 410) {
            this.updateStatus("error", "Session expired - CLI did not respond");
            this.cleanup();
          }
          return;
        }

        const data = await response.json();

        if (data.status === "answered" && data.answer) {
          clearInterval(this.pollTimer);
          this.pollTimer = null;

          this.updateStatus("connecting", "Establishing P2P connection...");

          const answer = new RTCSessionDescription({
            type: data.answer.type,
            sdp: data.answer.sdp,
          });
          await this.peerConnection.setRemoteDescription(answer);
        }
      } catch (error) {
        console.error("Polling error:", error);
      }
    }, this.pollIntervalValue);

    setTimeout(() => {
      if (this.pollTimer) {
        this.updateStatus(
          "error",
          "Timeout - CLI did not respond. Is botster-hub running?",
        );
        this.cleanup();
      }
    }, 30000);
  }

  handleMessage(message) {
    switch (message.type) {
      case "screen":
        // Full TUI screen update (TUI mode)
        if (this.modeValue === "tui" && this.terminal) {
          const binaryString = atob(message.data);
          const bytes = Uint8Array.from(binaryString, (c) => c.charCodeAt(0));
          const data = new TextDecoder().decode(bytes);
          this.terminal.write(data);
        }
        break;

      case "agents":
        // Agent list update (GUI mode)
        console.log("Received agent list:", message.agents);
        this.agents = message.agents;
        this.renderAgentList();
        // Update selected agent label in case we have a selection
        if (this.selectedAgentId) {
          this.updateSelectedAgentLabel();
        }
        break;

      case "agent_output":
        // Individual agent terminal output (GUI mode)
        if (
          this.modeValue === "gui" &&
          this.terminal &&
          message.id === this.selectedAgentId
        ) {
          const binaryString = atob(message.data);
          const bytes = Uint8Array.from(binaryString, (c) => c.charCodeAt(0));
          const data = new TextDecoder().decode(bytes);
          this.terminal.write(data);
        }
        break;

      case "agent_selected":
        console.log("Agent selected:", message.id);
        this.selectedAgentId = message.id;
        this.renderAgentList();
        this.updateSelectedAgentLabel();
        break;

      case "agent_created":
        console.log("Agent created:", message.id);
        // Auto-select the newly created agent and request fresh list
        this.selectedAgentId = message.id;
        this.requestAgentList();
        // Clear terminal for new agent
        if (this.terminal) {
          this.terminal.clear();
        }
        // Send resize for the newly created agent after a short delay
        setTimeout(() => {
          if (this.fitAddon && this.terminal) {
            this.fitAddon.fit();
            this.sendResize();
          }
        }, 100);
        break;

      case "agent_deleted":
        console.log("Agent deleted:", message.id);
        if (this.selectedAgentId === message.id) {
          this.selectedAgentId = null;
          if (this.terminal) {
            this.terminal.clear();
          }
        }
        this.requestAgentList();
        break;

      case "worktrees":
        console.log("Received worktrees:", message.worktrees);
        this.worktrees = message.worktrees || [];
        this.currentRepo = message.repo;
        this.renderWorktreeList();
        break;

      case "error":
        console.error("CLI error:", message.message);
        this.showError(message.message);
        break;

      default:
        console.log("Unknown message type:", message.type, message);
    }
  }

  // Render the agent list in GUI mode
  renderAgentList() {
    if (!this.hasAgentListTarget) return;

    if (this.agents.length === 0) {
      this.agentListTarget.innerHTML = `
        <div class="text-gray-500 text-center py-8">
          <p>No agents running</p>
          <p class="text-sm mt-2">Use the TUI to create agents</p>
        </div>
      `;
      return;
    }

    const html = this.agents
      .map((agent) => {
        const isSelected = agent.id === this.selectedAgentId;
        const issueLabel = agent.issue_number
          ? `#${agent.issue_number}`
          : agent.branch_name;
        const statusColor =
          agent.status === "Running" ? "text-green-600" : "text-gray-500";

        // Build preview/server status indicator
        let serverBadge = "";
        if (agent.tunnel_port) {
          if (agent.server_running) {
            // Server running - show clickable preview link
            serverBadge = `<a href="/preview/${agent.hub_identifier}/${agent.id}" target="_blank"
               class="inline-flex items-center gap-1 px-2 py-0.5 text-xs font-medium rounded-full bg-green-100 text-green-800 hover:bg-green-200"
               onclick="event.stopPropagation()">
               <span class="w-1.5 h-1.5 rounded-full bg-green-500"></span>
               :${agent.tunnel_port}
             </a>`;
          } else {
            // Server not running - show gray port indicator
            serverBadge = `<span class="inline-flex items-center gap-1 px-2 py-0.5 text-xs font-medium rounded-full bg-gray-100 text-gray-600">
               <span class="w-1.5 h-1.5 rounded-full bg-gray-400"></span>
               :${agent.tunnel_port}
             </span>`;
          }
        }

        // PTY view indicator (only show if agent has server PTY)
        let ptyViewBadge = "";
        if (agent.has_server_pty) {
          const viewLabel = agent.active_pty_view === "server" ? "SRV" : "CLI";
          const viewColor = agent.active_pty_view === "server" ? "bg-purple-100 text-purple-800" : "bg-blue-100 text-blue-800";
          ptyViewBadge = `<span class="inline-flex items-center px-1.5 py-0.5 text-xs font-medium rounded ${viewColor}">${viewLabel}</span>`;
        }

        // Scroll indicator
        let scrollBadge = "";
        if (agent.scroll_offset > 0) {
          scrollBadge = `<span class="inline-flex items-center px-1.5 py-0.5 text-xs font-medium rounded bg-yellow-100 text-yellow-800">↑${agent.scroll_offset}</span>`;
        }

        return `
        <button
          type="button"
          data-action="webrtc#selectAgent"
          data-agent-id="${agent.id}"
          class="w-full text-left px-4 py-3 border-b border-gray-200 hover:bg-gray-50 transition-colors ${isSelected ? "bg-blue-50 border-l-4 border-l-blue-500" : ""}"
        >
          <div class="flex items-center justify-between">
            <div>
              <span class="font-medium text-gray-900">${agent.repo}</span>
              <span class="text-gray-600 ml-2">${issueLabel}</span>
            </div>
            <div class="flex items-center gap-2">
              ${scrollBadge}
              ${ptyViewBadge}
              ${serverBadge}
              <span class="${statusColor} text-sm">${agent.status}</span>
            </div>
          </div>
        </button>
      `;
      })
      .join("");

    this.agentListTarget.innerHTML = html;
  }

  // Select an agent to view its terminal
  selectAgent(event) {
    const agentId = event.currentTarget.dataset.agentId;
    console.log("Selecting agent:", agentId);

    if (this.terminal) {
      this.terminal.clear();
    }

    this.sendMessage({ type: "select_agent", id: agentId });

    // Send resize for the newly selected agent after a short delay
    setTimeout(() => {
      if (this.fitAddon && this.terminal) {
        this.fitAddon.fit();
        this.sendResize();
      }
    }, 100);
  }

  // Request the agent list from CLI
  requestAgentList() {
    this.sendMessage({ type: "list_agents" });
  }

  // Create a new agent with issue number or branch name
  createAgent(issueOrBranch, prompt = null) {
    const message = {
      type: "create_agent",
      issue_or_branch: issueOrBranch,
    };
    if (prompt) {
      message.prompt = prompt;
    }
    this.sendMessage(message);
  }

  // Reopen an existing worktree as an agent
  reopenWorktree(path, branch, prompt = null) {
    const message = {
      type: "reopen_worktree",
      path: path,
      branch: branch,
    };
    if (prompt) {
      message.prompt = prompt;
    }
    this.sendMessage(message);
  }

  // Delete an agent
  deleteAgent(agentId, deleteWorktree = false) {
    this.sendMessage({
      type: "delete_agent",
      id: agentId,
      delete_worktree: deleteWorktree,
    });
  }

  // Send raw input to selected agent
  sendInput(data) {
    this.sendMessage({
      type: "send_input",
      data: data,
    });
  }

  showError(message) {
    // Could show a toast notification here
    console.error("Error from CLI:", message);
  }

  // Touch-friendly control methods for mobile devices
  sendCtrlC() {
    this.sendMessage({
      type: "key_press",
      key: "c",
      ctrl: true,
      alt: false,
      shift: false,
    });
  }

  sendEnter() {
    this.sendMessage({
      type: "key_press",
      key: "Enter",
      ctrl: false,
      alt: false,
      shift: false,
    });
  }

  sendEscape() {
    this.sendMessage({
      type: "key_press",
      key: "Escape",
      ctrl: false,
      alt: false,
      shift: false,
    });
  }

  sendArrowUp() {
    this.sendMessage({
      type: "key_press",
      key: "ArrowUp",
      ctrl: false,
      alt: false,
      shift: false,
    });
  }

  sendArrowDown() {
    this.sendMessage({
      type: "key_press",
      key: "ArrowDown",
      ctrl: false,
      alt: false,
      shift: false,
    });
  }

  sendArrowLeft() {
    this.sendMessage({
      type: "key_press",
      key: "ArrowLeft",
      ctrl: false,
      alt: false,
      shift: false,
    });
  }

  sendArrowRight() {
    this.sendMessage({
      type: "key_press",
      key: "ArrowRight",
      ctrl: false,
      alt: false,
      shift: false,
    });
  }

  sendTab() {
    this.sendMessage({
      type: "key_press",
      key: "Tab",
      ctrl: false,
      alt: false,
      shift: false,
    });
  }

  // Scroll controls - scroll the terminal view via WebRTC
  scrollUp(lines = 3) {
    if (!this.dataChannel || this.dataChannel.readyState !== "open") {
      console.warn("Cannot scroll - data channel not open");
      return;
    }
    console.log(`Sending scroll up ${lines} lines`);
    this.sendMessage({
      type: "scroll",
      direction: "up",
      lines: lines,
    });
  }

  scrollDown(lines = 3) {
    if (!this.dataChannel || this.dataChannel.readyState !== "open") {
      console.warn("Cannot scroll - data channel not open");
      return;
    }
    console.log(`Sending scroll down ${lines} lines`);
    this.sendMessage({
      type: "scroll",
      direction: "down",
      lines: lines,
    });
  }

  scrollToTop() {
    console.log("Sending scroll to top");
    this.sendMessage({ type: "scroll_to_top" });
  }

  scrollToBottom() {
    console.log("Sending scroll to bottom");
    this.sendMessage({ type: "scroll_to_bottom" });
  }

  // Handle wheel events on terminal for scrollback
  handleTerminalWheel(event) {
    // Only handle if we have a connection and are in GUI mode
    if (!this.dataChannel || this.dataChannel.readyState !== "open") return;
    if (this.modeValue !== "gui") return;

    // Prevent default scrolling behavior
    event.preventDefault();

    // Calculate lines to scroll based on wheel delta
    // deltaY is positive for scroll down, negative for scroll up
    const lines = Math.max(1, Math.abs(Math.round(event.deltaY / 30)));

    if (event.deltaY < 0) {
      this.scrollUp(lines);
    } else if (event.deltaY > 0) {
      this.scrollDown(lines);
    }
  }

  // PTY view toggle - switch between CLI and Server terminal views
  togglePtyView() {
    if (!this.dataChannel || this.dataChannel.readyState !== "open") {
      console.warn("Cannot toggle PTY view - data channel not open");
      return;
    }
    console.log("Sending toggle PTY view");
    this.sendMessage({ type: "toggle_pty_view" });
    // Clear terminal when switching views
    if (this.terminal) {
      this.terminal.clear();
    }
  }

  // New Agent Modal
  showNewAgentModal() {
    if (!this.hasNewAgentModalTarget) return;

    // Dispatch event to modal controller to show
    this.newAgentModalTarget.dispatchEvent(new CustomEvent("modal:show"));

    // Request available worktrees from CLI
    this.sendMessage({ type: "list_worktrees" });

    // Focus the issue/branch input
    if (this.hasIssueOrBranchTarget) {
      setTimeout(() => this.issueOrBranchTarget.focus(), 100);
    }
  }

  // Cleanup when new agent modal closes (called via modal:closed event)
  cleanupNewAgentModal() {
    if (this.hasIssueOrBranchTarget) this.issueOrBranchTarget.value = "";
    if (this.hasAgentPromptTarget) this.agentPromptTarget.value = "";
    this.worktrees = [];
    if (this.hasWorktreeListTarget) {
      this.worktreeListTarget.innerHTML = "";
    }
  }

  // Render available worktrees in the modal
  renderWorktreeList() {
    if (!this.hasWorktreeListTarget) return;

    if (this.worktrees.length === 0) {
      this.worktreeListTarget.innerHTML = `
        <p class="text-sm text-gray-500 py-2">No existing worktrees available</p>
      `;
      return;
    }

    const html = this.worktrees
      .map((wt) => {
        const issueLabel = wt.issue_number ? `#${wt.issue_number}` : wt.branch;
        return `
        <button
          type="button"
          data-action="webrtc#selectWorktree"
          data-worktree-path="${wt.path}"
          data-worktree-branch="${wt.branch}"
          class="w-full text-left px-3 py-2 border border-gray-300 rounded-md text-sm hover:bg-gray-50 mb-2"
        >
          <span class="font-medium">${issueLabel}</span>
          <span class="text-gray-500 ml-2">${wt.branch}</span>
        </button>
      `;
      })
      .join("");

    this.worktreeListTarget.innerHTML = html;
  }

  // Select an existing worktree to reopen
  selectWorktree(event) {
    const path = event.currentTarget.dataset.worktreePath;
    const branch = event.currentTarget.dataset.worktreeBranch;
    const prompt = this.hasAgentPromptTarget
      ? this.agentPromptTarget.value.trim()
      : null;

    this.reopenWorktree(path, branch, prompt || null);
    // Close modal via event
    this.newAgentModalTarget.dispatchEvent(new CustomEvent("modal:hide"));
  }

  submitNewAgent() {
    if (!this.hasIssueOrBranchTarget) return;

    // Use native HTML5 validation (pattern attribute on input)
    if (!this.issueOrBranchTarget.reportValidity()) {
      return;
    }

    const issueOrBranch = this.issueOrBranchTarget.value.trim();
    const prompt = this.hasAgentPromptTarget
      ? this.agentPromptTarget.value.trim()
      : null;

    this.createAgent(issueOrBranch, prompt || null);
    // Close modal via event
    this.newAgentModalTarget.dispatchEvent(new CustomEvent("modal:hide"));
  }

  // Close Agent Modal
  showCloseAgentModal() {
    if (!this.selectedAgentId) {
      alert("Please select an agent first");
      return;
    }
    if (!this.hasCloseAgentModalTarget) return;

    // Find agent info
    const agent = this.agents.find((a) => a.id === this.selectedAgentId);
    if (agent && this.hasCloseAgentNameTarget) {
      const issueLabel = agent.issue_number
        ? `#${agent.issue_number}`
        : agent.branch_name;
      this.closeAgentNameTarget.textContent = `${agent.repo} ${issueLabel}`;
    }

    // Dispatch event to modal controller to show
    this.closeAgentModalTarget.dispatchEvent(new CustomEvent("modal:show"));
  }

  closeAgentKeepWorktree() {
    if (!this.selectedAgentId) return;
    this.deleteAgent(this.selectedAgentId, false);
    // Close modal via event
    this.closeAgentModalTarget.dispatchEvent(new CustomEvent("modal:hide"));
  }

  closeAgentDeleteWorktree() {
    if (!this.selectedAgentId) return;
    this.deleteAgent(this.selectedAgentId, true);
    // Close modal via event
    this.closeAgentModalTarget.dispatchEvent(new CustomEvent("modal:hide"));
  }

  // Update selected agent label
  updateSelectedAgentLabel() {
    if (!this.hasSelectedAgentLabelTarget) return;

    if (!this.selectedAgentId) {
      this.selectedAgentLabelTarget.textContent = "No agent selected";
      return;
    }

    const agent = this.agents.find((a) => a.id === this.selectedAgentId);
    if (agent) {
      const issueLabel = agent.issue_number
        ? `#${agent.issue_number}`
        : agent.branch_name;

      // Build label with PTY view indicator if server PTY exists
      let label = `${agent.repo} ${issueLabel}`;
      if (agent.has_server_pty) {
        const viewLabel = agent.active_pty_view === "server" ? "[Server]" : "[CLI]";
        label += ` ${viewLabel}`;
      }

      // Add scroll indicator if scrolled
      if (agent.scroll_offset > 0) {
        label += ` [↑${agent.scroll_offset}]`;
      }

      this.selectedAgentLabelTarget.textContent = label;
    }
  }

  // Get the currently selected agent
  getSelectedAgent() {
    return this.agents.find((a) => a.id === this.selectedAgentId);
  }

  // Check if the selected agent has a server PTY
  selectedAgentHasServerPty() {
    const agent = this.getSelectedAgent();
    return agent?.has_server_pty || false;
  }

  // Clear tunnel service worker and cookie for selected agent (debug helper)
  async clearTunnelCache() {
    const agent = this.getSelectedAgent();
    if (!agent) {
      alert("No agent selected");
      return;
    }

    if (!agent.hub_identifier) {
      alert("Agent has no hub identifier");
      return;
    }

    const scope = `/preview/${agent.hub_identifier}/${agent.id}/`;
    const cookiePath = `/preview/${agent.hub_identifier}/${agent.id}`;

    try {
      // Unregister service worker for this scope
      if ("serviceWorker" in navigator) {
        const registrations = await navigator.serviceWorker.getRegistrations();
        for (const registration of registrations) {
          if (registration.scope.includes(scope) || registration.scope.includes(cookiePath)) {
            await registration.unregister();
            console.log(`Unregistered SW for scope: ${registration.scope}`);
          }
        }
      }

      // Clear the tunnel_sw cookie by setting it to expire
      document.cookie = `tunnel_sw=; path=${cookiePath}; expires=Thu, 01 Jan 1970 00:00:00 GMT; SameSite=Strict`;
      document.cookie = `tunnel_sw=; path=${scope}; expires=Thu, 01 Jan 1970 00:00:00 GMT; SameSite=Strict`;

      alert(`Cleared tunnel cache for ${agent.id}\n\nScope: ${scope}\n\nRefresh the preview page to re-initialize.`);
    } catch (error) {
      console.error("Failed to clear tunnel cache:", error);
      alert(`Error clearing tunnel cache: ${error.message}`);
    }
  }

  initializeTerminal() {
    if (!Terminal) {
      console.error("Terminal not loaded yet, retrying in 100ms...");
      setTimeout(() => this.initializeTerminal(), 100);
      return;
    }

    if (this.terminal) {
      return;
    }

    console.log("Initializing terminal");

    // Use smaller font on mobile for better fit
    const isMobile = window.innerWidth < 768;
    const fontSize = isMobile ? 12 : 14;

    this.terminal = new Terminal({
      cursorBlink: true,
      disableStdin: false,
      fontSize: fontSize,
      fontFamily: "Menlo, Monaco, 'Courier New', monospace",
      theme: {
        background: "#1e1e1e",
        foreground: "#d4d4d4",
      },
      // Mobile-friendly settings
      scrollback: 1000,
      convertEol: true,
    });

    this.fitAddon = new FitAddon();
    this.terminal.loadAddon(this.fitAddon);
    this.terminal.open(this.terminalTarget);

    // Multiple fit attempts to ensure it works correctly
    // The container might not be fully laid out on first attempt
    const fitTerminal = () => {
      if (
        this.fitAddon &&
        this.terminal &&
        this.terminalTarget.offsetWidth > 0
      ) {
        this.fitAddon.fit();
        console.log(
          `Terminal fitted to: ${this.terminal.cols}x${this.terminal.rows}, container width: ${this.terminalTarget.offsetWidth}`,
        );
        this.sendResize();
      }
    };

    // Initial fit after layout
    requestAnimationFrame(() => {
      fitTerminal();
      // Second fit after a short delay
      setTimeout(fitTerminal, 100);
      // Third fit after longer delay (for slow layouts)
      setTimeout(fitTerminal, 300);
    });

    // Capture keyboard input and send to CLI
    this.terminal.onKey(({ key, domEvent }) => {
      this.sendKeyPress(domEvent);
    });

    // Capture special keys
    this.terminalTarget.addEventListener("keydown", (e) => {
      if (this.shouldCaptureKey(e)) {
        e.preventDefault();
        this.sendKeyPress(e);
      }
    });

    // Capture wheel events for scrollback (GUI mode only)
    this.wheelHandler = this.handleTerminalWheel.bind(this);
    this.terminalTarget.addEventListener("wheel", this.wheelHandler, {
      passive: false,
    });

    // Debounced resize handler for window resize and orientation change
    this.resizeTimeout = null;
    this.resizeHandler = () => {
      if (this.resizeTimeout) clearTimeout(this.resizeTimeout);
      this.resizeTimeout = setTimeout(() => {
        if (this.fitAddon && this.terminal) {
          this.fitAddon.fit();
          this.sendResize();
        }
      }, 150);
    };
    window.addEventListener("resize", this.resizeHandler);
    window.addEventListener("orientationchange", this.resizeHandler);

    // Focus terminal for keyboard input
    this.terminal.focus();
  }

  sendResize() {
    if (
      !this.terminal ||
      !this.dataChannel ||
      this.dataChannel.readyState !== "open"
    )
      return;

    // Sanity check - don't send tiny dimensions
    if (this.terminal.cols < 20 || this.terminal.rows < 5) {
      console.warn(
        `Skipping resize - dimensions too small: ${this.terminal.cols}x${this.terminal.rows}`,
      );
      return;
    }

    console.log(`Sending resize: ${this.terminal.cols}x${this.terminal.rows}`);
    this.sendMessage({
      type: "resize",
      rows: this.terminal.rows,
      cols: this.terminal.cols,
    });
  }

  shouldCaptureKey(e) {
    if (e.ctrlKey || e.altKey || e.metaKey) return true;
    if (
      [
        "Escape",
        "Tab",
        "ArrowUp",
        "ArrowDown",
        "ArrowLeft",
        "ArrowRight",
        "Home",
        "End",
        "PageUp",
        "PageDown",
        "Insert",
        "Delete",
        "F1",
        "F2",
        "F3",
        "F4",
        "F5",
        "F6",
        "F7",
        "F8",
        "F9",
        "F10",
        "F11",
        "F12",
      ].includes(e.key)
    )
      return true;
    return false;
  }

  sendKeyPress(domEvent) {
    if (!this.dataChannel || this.dataChannel.readyState !== "open") return;

    this.sendMessage({
      type: "key_press",
      key: domEvent.key,
      ctrl: domEvent.ctrlKey,
      alt: domEvent.altKey,
      shift: domEvent.shiftKey,
    });
  }

  sendMessage(message) {
    if (this.dataChannel && this.dataChannel.readyState === "open") {
      this.dataChannel.send(JSON.stringify(message));
    }
  }

  updateStatus(state, message) {
    if (!this.hasStatusTarget) return;

    const colors = {
      disconnected: "text-gray-500",
      connecting: "text-yellow-600",
      connected: "text-green-600",
      error: "text-red-600",
    };

    const icons = {
      disconnected: "○",
      connecting: "◐",
      connected: "●",
      error: "✕",
    };

    this.statusTarget.className = `text-sm ${colors[state]}`;
    this.statusTarget.textContent = `${icons[state]} ${message}`;
  }

  cleanup() {
    // Clear reconnection timer to prevent auto-reconnect after manual disconnect
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    this.reconnectAttempts = 0;
    this.isReconnecting = false;

    if (this.pollTimer) {
      clearInterval(this.pollTimer);
      this.pollTimer = null;
    }

    if (this.resizeTimeout) {
      clearTimeout(this.resizeTimeout);
      this.resizeTimeout = null;
    }

    if (this.dataChannel) {
      this.dataChannel.close();
      this.dataChannel = null;
    }

    if (this.peerConnection) {
      this.peerConnection.close();
      this.peerConnection = null;
    }

    if (this.resizeHandler) {
      window.removeEventListener("resize", this.resizeHandler);
      window.removeEventListener("orientationchange", this.resizeHandler);
      this.resizeHandler = null;
    }

    if (this.wheelHandler && this.hasTerminalTarget) {
      this.terminalTarget.removeEventListener("wheel", this.wheelHandler);
      this.wheelHandler = null;
    }

    if (this.terminal) {
      this.terminal.dispose();
      this.terminal = null;
      this.fitAddon = null;
    }

    this.sessionId = null;
    this.agents = [];
    this.selectedAgentId = null;

    if (this.hasConnectButtonTarget) {
      this.connectButtonTarget.disabled = false;
      this.connectButtonTarget.textContent = "Connect to Hub";
    }

    if (this.hasAgentListTarget) {
      this.agentListTarget.innerHTML = "";
    }

    this.updateStatus("disconnected", "Not connected");
  }
}
