import { Controller } from "@hotwired/stimulus";

/**
 * Terminal View Controller
 *
 * Manages PTY tab switching (CLI view at index 0, Server view at index 1)
 * and shows a preview link when a tunnel is connected.
 *
 * Agent state (hasServerPty, tunnelConnected, tunnelPort) is pushed via
 * Stimulus values from agents_controller — this controller does NOT listen
 * for agent messages itself.
 *
 * Listens for `pty_channel_switched` from the terminal-connection outlet to
 * stay in sync when the connection controller switches PTY channels.
 */
export default class extends Controller {
  static targets = [
    "agentTab",
    "serverTab",
    "previewLink",
    "tabContainer",
    "fallbackLabel",
  ];

  static outlets = ["terminal-connection"];

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
  terminalConnectionOutletConnected(outlet) {
    outlet.registerListener(this, {
      onMessage: (msg) => this.#handleMessage(msg),
    });
  }

  terminalConnectionOutletDisconnected(outlet) {
    outlet.unregisterListener(this);
  }

  // Handle messages from terminal-connection outlet
  #handleMessage(message) {
    switch (message.type) {
      case "pty_channel_switched":
        // Connection controller switched PTY - update our state
        this.ptyIndexValue = message.pty_index || 0;
        this.updateUI();
        break;
    }
  }

  // Stimulus value-changed callbacks — agents_controller pushes state via data attributes
  hasServerPtyValueChanged() {
    this.updateUI();
  }

  tunnelConnectedValueChanged() {
    this.updateUI();
  }

  tunnelPortValueChanged() {
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
    if (!this.hasTerminalConnectionOutlet) return;

    const agentIndex = this.terminalConnectionOutlet.getCurrentAgentIndex();
    const success = await this.terminalConnectionOutlet.connectToPty(
      agentIndex,
      ptyIndex,
    );

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
        this.agentTabTarget.classList.remove(
          "text-zinc-500",
          "hover:text-zinc-300",
        );
      } else {
        this.agentTabTarget.classList.remove("bg-zinc-700", "text-zinc-100");
        this.agentTabTarget.classList.add(
          "text-zinc-500",
          "hover:text-zinc-300",
        );
      }
    }

    if (this.hasServerTabTarget) {
      if (this.ptyIndexValue === 1) {
        this.serverTabTarget.classList.add("bg-zinc-700", "text-zinc-100");
        this.serverTabTarget.classList.remove(
          "text-zinc-500",
          "hover:text-zinc-300",
        );
      } else {
        this.serverTabTarget.classList.remove("bg-zinc-700", "text-zinc-100");
        this.serverTabTarget.classList.add(
          "text-zinc-500",
          "hover:text-zinc-300",
        );
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
      const agentIndex = this.hasTerminalConnectionOutlet
        ? this.terminalConnectionOutlet.getCurrentAgentIndex()
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
