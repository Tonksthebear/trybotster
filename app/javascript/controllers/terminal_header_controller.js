import { Controller } from "@hotwired/stimulus";
import { ConnectionManager, HubConnection } from "connections";

/**
 * Terminal Header Controller
 *
 * Handles the terminal view header:
 * - Preview link visibility when tunnel is connected
 *
 * Agent state is pushed via HubConnection messages.
 * PTY tab switching is handled by Rails links with Turbo navigation.
 *
 * Usage:
 *   <div data-controller="terminal-header"
 *        data-terminal-header-hub-id-value="123"
 *        data-terminal-header-agent-index-value="0"
 *        data-terminal-header-tunnel-connected-value="false"
 *        class="group">
 *     <!-- Preview link shown via group-data-[terminal-header-tunnel-connected-value=true] -->
 *   </div>
 */
export default class extends Controller {
  static targets = ["previewLink"];

  static values = {
    hubId: String,
    agentIndex: { type: Number, default: 0 },
    tunnelConnected: { type: Boolean, default: false },
    tunnelPort: { type: Number, default: 0 },
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
          this.tunnelConnectedValue =
            msg.tunnel_connected || msg.tunnel_status === "connected" || false;
          this.tunnelPortValue = msg.tunnel_port || 0;
        }
      }),
    );
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
