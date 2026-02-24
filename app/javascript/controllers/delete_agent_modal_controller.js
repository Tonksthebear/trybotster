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

  #hubReady = null;

  connect() {
    if (!this.hubIdValue) return;

    this.#hubReady = ConnectionManager.acquire(HubConnection, this.hubIdValue, {
      hubId: this.hubIdValue,
    }).then((hub) => {
      this.hub = hub;
      return hub;
    });
  }

  disconnect() {
    this.#hubReady = null;
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
  async confirmKeep() {
    const agentId = this.element.dataset.agentId;
    if (!agentId) return;
    const hub = this.hub ?? (await this.#hubReady);
    if (hub) hub.deleteAgent(agentId, false);
    this.#closeDialog();
  }

  // Action: close agent and delete worktree
  async confirmDelete() {
    const agentId = this.element.dataset.agentId;
    if (!agentId) return;
    const hub = this.hub ?? (await this.#hubReady);
    if (hub) hub.deleteAgent(agentId, true);
    this.#closeDialog();
  }

  #closeDialog() {
    this.element.closest("dialog")?.close();
  }
}
