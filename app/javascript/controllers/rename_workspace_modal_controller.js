import { Controller } from "@hotwired/stimulus";
import { HubConnectionManager, HubConnection } from "connections";

/**
 * Rename Workspace Modal Controller
 *
 * Opened by agent_list_controller with workspace ID and current name
 * set on the controller element via data attributes.
 */
export default class extends Controller {
  static values = { hubId: String };
  static targets = ["input", "currentName"];

  #hubReady = null;

  connect() {
    if (!this.hubIdValue) return;

    this.#hubReady = HubConnectionManager.acquire(HubConnection, this.hubIdValue, {
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

  // Called by agent_list_controller to populate before showing
  open(workspaceId, currentName) {
    this.element.dataset.workspaceId = workspaceId;
    if (this.hasCurrentNameTarget) {
      this.currentNameTarget.textContent = currentName;
    }
    if (this.hasInputTarget) {
      this.inputTarget.value = currentName;
    }
    this.element.closest("dialog")?.showModal();
    // Select text for easy replacement
    this.inputTarget?.select();
  }

  async confirm() {
    const wsId = this.element.dataset.workspaceId;
    if (!wsId) return;

    const newName = this.inputTarget?.value?.trim();
    if (!newName) return;

    const hub = this.hub ?? (await this.#hubReady);
    if (hub) hub.renameWorkspace(wsId, newName);
    this.#closeDialog();
  }

  #closeDialog() {
    this.element.closest("dialog")?.close();
  }
}
