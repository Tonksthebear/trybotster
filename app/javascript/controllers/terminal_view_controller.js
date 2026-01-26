import { Controller } from "@hotwired/stimulus";

/**
 * Terminal View Controller
 *
 * Manages switching between Agent PTY (CLI, index 0) and Server PTY (index 1) views.
 * Also handles displaying the preview link when tunnel is connected.
 *
 * Uses connection outlet to switch PTY streams via ActionCable.
 */
export default class extends Controller {
  static targets = [
    "agentTab",
    "serverTab",
    "previewLink",
    "tabContainer",
    "fallbackLabel",
  ];

  static outlets = ["connection"];

  static values = {
    hubId: String,
    ptyIndex: { type: Number, default: 0 }, // Current PTY: 0=CLI, 1=Server
    hasServerPty: { type: Boolean, default: false },
    tunnelConnected: { type: Boolean, default: false },
    tunnelPort: { type: Number, default: 0 },
  };

  connect() {
    this.updateUI();
  }

  // Stimulus outlet callbacks
  connectionOutletConnected(outlet) {
    outlet.registerListener(this, {
      onMessage: (msg) => this.#handleMessage(msg),
    });
  }

  connectionOutletDisconnected(outlet) {
    outlet.unregisterListener(this);
  }

  // Handle messages from CLI
  #handleMessage(message) {
    switch (message.type) {
      case "agent_selected":
      case "agents":
      case "agent_list":
        this.#updateAgentData(message);
        break;
      case "pty_channel_switched":
        // Connection controller switched PTY - update our state
        this.ptyIndexValue = message.pty_index || 0;
        this.updateUI();
        break;
    }
  }

  // Update agent data from message
  #updateAgentData(message) {
    // Find selected agent data
    const agents = message.agents || [];
    const selectedId = message.id;

    let agent = null;
    if (selectedId) {
      agent = agents.find(a => a.id === selectedId);
    } else if (message.has_server_pty !== undefined) {
      // Direct agent_selected message with data
      agent = message;
    }

    if (agent) {
      this.hasServerPtyValue = agent.has_server_pty || false;
      this.tunnelConnectedValue = agent.tunnel_connected || agent.tunnel_status === "connected";
      this.tunnelPortValue = agent.tunnel_port || 0;
      // Don't override ptyIndex from agent data - it's managed by connection
    } else {
      // No agent selected
      this.hasServerPtyValue = false;
      this.tunnelConnectedValue = false;
      this.tunnelPortValue = 0;
      this.ptyIndexValue = 0;
    }

    this.updateUI();
  }

  // Action: Switch to Agent (CLI) PTY - index 0
  showAgentView() {
    if (this.ptyIndexValue === 0) return;
    this.#switchToPty(0);
  }

  // Action: Switch to Server PTY - index 1
  showServerView() {
    if (this.ptyIndexValue === 1) return;
    if (!this.hasServerPtyValue) return;
    this.#switchToPty(1);
  }

  // Switch to a PTY by index
  async #switchToPty(ptyIndex) {
    if (!this.hasConnectionOutlet) return;

    const agentIndex = this.connectionOutlet.getCurrentAgentIndex();
    const success = await this.connectionOutlet.connectToPty(agentIndex, ptyIndex);

    if (success) {
      this.ptyIndexValue = ptyIndex;
      this.updateUI();
    }
  }

  // Update all UI elements based on current state
  updateUI() {
    this.#updateTabs();
    this.#updatePreviewLink();
  }

  // Update tab visibility and active state
  #updateTabs() {
    // Show/hide tab container based on whether server PTY exists
    if (this.hasTabContainerTarget) {
      if (this.hasServerPtyValue) {
        this.tabContainerTarget.classList.remove("hidden");
      } else {
        this.tabContainerTarget.classList.add("hidden");
      }
    }

    // Show/hide fallback label (inverse of tabs)
    if (this.hasFallbackLabelTarget) {
      if (this.hasServerPtyValue) {
        this.fallbackLabelTarget.classList.add("hidden");
      } else {
        this.fallbackLabelTarget.classList.remove("hidden");
      }
    }

    // Update active tab styling - CLI is ptyIndex 0, Server is ptyIndex 1
    if (this.hasAgentTabTarget) {
      if (this.ptyIndexValue === 0) {
        this.agentTabTarget.classList.add("bg-zinc-700", "text-zinc-100");
        this.agentTabTarget.classList.remove("text-zinc-500", "hover:text-zinc-300");
      } else {
        this.agentTabTarget.classList.remove("bg-zinc-700", "text-zinc-100");
        this.agentTabTarget.classList.add("text-zinc-500", "hover:text-zinc-300");
      }
    }

    if (this.hasServerTabTarget) {
      if (this.ptyIndexValue === 1) {
        this.serverTabTarget.classList.add("bg-zinc-700", "text-zinc-100");
        this.serverTabTarget.classList.remove("text-zinc-500", "hover:text-zinc-300");
      } else {
        this.serverTabTarget.classList.remove("bg-zinc-700", "text-zinc-100");
        this.serverTabTarget.classList.add("text-zinc-500", "hover:text-zinc-300");
      }
    }
  }

  // Update preview link visibility and href
  #updatePreviewLink() {
    if (!this.hasPreviewLinkTarget) return;

    // Preview is available when tunnel is connected and we have agent info
    // URL format: /hubs/:hub_id/agents/:agent_index/:pty_index/preview
    // Server PTY with port forwarding is typically pty_index 1
    if (this.tunnelConnectedValue && this.hubIdValue) {
      // Get agent index from connection outlet if available
      const agentIndex = this.hasConnectionOutlet
        ? this.connectionOutlet.getCurrentAgentIndex()
        : 0;
      // Server PTY is index 1 (CLI is index 0)
      const ptyIndex = 1;
      const previewUrl = `/hubs/${this.hubIdValue}/agents/${agentIndex}/${ptyIndex}/preview`;
      this.previewLinkTarget.href = previewUrl;
      this.previewLinkTarget.classList.remove("hidden");
    } else if (this.tunnelPortValue > 0) {
      // Show port but no link (not connected yet)
      this.previewLinkTarget.classList.add("hidden");
    } else {
      this.previewLinkTarget.classList.add("hidden");
    }
  }
}
