import { Controller } from "@hotwired/stimulus";
import { ConnectionManager, HubConnection } from "connections";

/**
 * Delete Agent Modal Controller
 *
 * Minimal controller that acquires HubConnection to send delete commands.
 * Agent ID is stored on the modal element via data-agent-id (set by opener).
 */
export default class extends Controller {
  static values = { hubId: String };

  connect() {
    if (!this.hubIdValue) return;

    ConnectionManager.acquire(HubConnection, this.hubIdValue, {
      hubId: this.hubIdValue,
    }).then((hub) => {
      this.hub = hub;
    });
  }

  disconnect() {
    const hub = this.hub;
    this.hub = null;
    hub?.release();
  }

  // Action: close agent, keep worktree
  confirmKeep() {
    const agentId = this.element.dataset.agentId;
    if (agentId && this.hub) {
      this.hub.deleteAgent(agentId, false);
    }
  }

  // Action: close agent and delete worktree
  confirmDelete() {
    const agentId = this.element.dataset.agentId;
    if (agentId && this.hub) {
      this.hub.deleteAgent(agentId, true);
    }
  }
}
