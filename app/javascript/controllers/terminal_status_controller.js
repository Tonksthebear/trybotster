import { Controller } from "@hotwired/stimulus";
import { ConnectionManager, TerminalConnection } from "connections";

/**
 * Terminal Status Controller
 *
 * Sets data attribute based on terminal connection state.
 * All visual rendering is handled by CSS using group-data-[] variants.
 *
 * States: disconnected, connecting, connected, error
 *
 * Usage:
 *   <div data-controller="terminal-status"
 *        data-terminal-status-hub-id-value="123"
 *        data-terminal-status-agent-index-value="0"
 *        data-terminal-status-pty-index-value="0"
 *        class="group">
 *     <span class="hidden group-data-[terminal-status-state-value=connected]:flex">E2E Encrypted</span>
 *     <span class="hidden group-data-[terminal-status-state-value=connecting]:flex">Connecting...</span>
 *   </div>
 */
export default class extends Controller {
  static values = {
    hubId: String,
    agentIndex: Number,
    ptyIndex: { type: Number, default: 0 },
    state: { type: String, default: "disconnected" },
  };

  #unsubscribe = null;

  connect() {
    if (!this.hubIdValue) return;

    const key = TerminalConnection.key(
      this.hubIdValue,
      this.agentIndexValue,
      this.ptyIndexValue,
    );

    // Subscribe to connection state without holding a reference
    this.#unsubscribe = ConnectionManager.subscribe(key, ({ state }) => {
      this.stateValue = state;
    });
  }

  disconnect() {
    this.#unsubscribe?.();
    this.#unsubscribe = null;
  }
}
