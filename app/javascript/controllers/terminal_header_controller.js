import { Controller } from "@hotwired/stimulus";
import { ConnectionManager, HubConnection } from "connections";

/**
 * Terminal Header Controller
 *
 * Handles the terminal view header:
 * - PTY tab switching (navigates to new URL)
 * - Agent info display
 * - Preview link when tunnel is connected
 *
 * Agent state is pushed via HubConnection messages.
 * All visual states are CSS-driven via group-data-[] variants.
 *
 * Usage:
 *   <div data-controller="terminal-header"
 *        data-terminal-header-hub-id-value="123"
 *        data-terminal-header-agent-index-value="0"
 *        data-terminal-header-pty-index-value="0"
 *        data-terminal-header-has-server-pty-value="false"
 *        data-terminal-header-tunnel-connected-value="false"
 *        class="group">
 *     <!-- Tabs shown via group-data-[terminal-header-has-server-pty-value=true] -->
 *     <!-- Preview link shown via group-data-[terminal-header-tunnel-connected-value=true] -->
 *   </div>
 */
export default class extends Controller {
  static targets = ["previewLink"];

  static values = {
    hubId: String,
    agentIndex: { type: Number, default: 0 },
    ptyIndex: { type: Number, default: 0 },
    hasServerPty: { type: Boolean, default: false },
    tunnelConnected: { type: Boolean, default: false },
    tunnelPort: { type: Number, default: 0 },
    agentName: String,
  };

  #hub = null;
  #unsubscribers = [];

  connect() {
    if (!this.hubIdValue) return;
    this.#initConnection();
  }

  disconnect() {
    this.#unsubscribers.forEach((unsub) => unsub());
    this.#unsubscribers = [];
    this.#hub?.release();
    this.#hub = null;
  }

  async #initConnection() {
    this.#hub = await ConnectionManager.acquire(
      HubConnection,
      this.hubIdValue,
      { hubId: this.hubIdValue },
    );

    this.#unsubscribers.push(
      this.#hub.on("message", (msg) => {
        if (msg.type === "agent_selected") {
          this.#handleAgentSelected(msg);
        }
      }),
    );
  }

  // Actions: PTY tab switching (navigates to new URL)
  showAgentView() {
    if (this.ptyIndexValue === 0) return;
    this.#navigateToPty(0);
  }

  showServerView() {
    if (this.ptyIndexValue === 1) return;
    if (!this.hasServerPtyValue) return;
    this.#navigateToPty(1);
  }

  #navigateToPty(ptyIndex) {
    // Navigate to new URL - Turbo will handle the transition
    const url = `/hubs/${this.hubIdValue}/agents/${this.agentIndexValue}/ptys/${ptyIndex}`;
    window.Turbo?.visit(url);
  }

  #handleAgentSelected(message) {
    this.agentNameValue = message.name || message.id || "";
    this.agentIndexValue = message.index ?? this.agentIndexValue;
    this.hasServerPtyValue = message.has_server_pty || false;
    this.tunnelConnectedValue =
      message.tunnel_connected ||
      message.tunnel_status === "connected" ||
      false;
    this.tunnelPortValue = message.tunnel_port || 0;
  }

  // Value changed callbacks - update preview link URL
  tunnelConnectedValueChanged() {
    this.#updatePreviewLink();
  }

  agentIndexValueChanged() {
    this.#updatePreviewLink();
  }

  #updatePreviewLink() {
    if (!this.hasPreviewLinkTarget) return;

    if (this.tunnelConnectedValue && this.hubIdValue) {
      const url = `/hubs/${this.hubIdValue}/agents/${this.agentIndexValue}/1/preview`;
      this.previewLinkTarget.href = url;
    }
  }
}
