import { Controller } from "@hotwired/stimulus";
import { ConnectionManager, HubConnection } from "connections";

/**
 * Delete Agent Modal Controller
 *
 * Minimal controller that acquires HubConnection to send delete commands.
 * Agent ID is stored on the modal element via data-agent-id (set by opener).
 * When in_worktree is false, the "delete worktree" option is hidden.
 */
export default class extends Controller {
  static values = { hubId: String, inWorktree: { type: Boolean, default: true } };
  static targets = ["worktreeOption", "worktreePrompt"];

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

  inWorktreeValueChanged() {
    if (this.hasWorktreeOptionTarget) {
      this.worktreeOptionTarget.classList.toggle("hidden", !this.inWorktreeValue);
    }
    if (this.hasWorktreePromptTarget) {
      this.worktreePromptTarget.classList.toggle("hidden", !this.inWorktreeValue);
    }
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
