import { Controller } from "@hotwired/stimulus";
import { HubManager } from "connections";

/**
 * Move Agent Modal Controller
 *
 * Opened by agent_list_controller with session UUID, current workspace,
 * and workspace list. Renders existing workspaces as clickable buttons
 * and offers a text input for creating a new workspace.
 */
export default class extends Controller {
  static values = { hubId: String };
  static targets = ["input", "agentName", "workspaceList", "divider"];

  #hubReady = null;

  connect() {
    if (!this.hubIdValue) return;

    this.#hubReady = HubManager.acquire(this.hubIdValue).then((hub) => {
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
  open(sessionUuid, agentName, workspaces, currentWorkspaceId) {
    this.element.dataset.agentId = sessionUuid;
    if (this.hasAgentNameTarget) {
      this.agentNameTarget.textContent = agentName;
    }
    if (this.hasInputTarget) {
      this.inputTarget.value = "";
    }

    // Build workspace buttons (exclude current workspace)
    if (this.hasWorkspaceListTarget) {
      this.workspaceListTarget.innerHTML = "";
      const others = workspaces.filter(
        (ws) => ws?.id !== currentWorkspaceId && ws?.status === "active",
      );

      if (others.length > 0) {
        others.forEach((ws) => {
          const btn = document.createElement("button");
          btn.type = "button";
          btn.className =
            "w-full text-left px-4 py-3 rounded-md border border-zinc-700 bg-zinc-900 hover:bg-zinc-800 hover:border-zinc-600 transition-colors";
          btn.innerHTML = `<div class="text-sm font-medium text-zinc-100">${this.#escapeHtml(ws.name || ws.id)}</div>`;
          btn.addEventListener("click", () => this.#moveToExisting(ws.id, ws.name));
          this.workspaceListTarget.appendChild(btn);
        });
      }

      // Hide divider and list if no other workspaces
      if (this.hasDividerTarget) {
        this.dividerTarget.classList.toggle("hidden", others.length === 0);
      }
      this.workspaceListTarget.classList.toggle("hidden", others.length === 0);
    }

    this.element.closest("dialog")?.showModal();
    this.inputTarget?.focus();
  }

  async #moveToExisting(workspaceId, workspaceName) {
    const sessionUuid = this.element.dataset.agentId;
    if (!sessionUuid) return;

    const hub = this.hub ?? (await this.#hubReady);
    if (hub) hub.moveAgentWorkspace(sessionUuid, workspaceId, workspaceName);
    this.#closeDialog();
  }

  async confirmNew() {
    const sessionUuid = this.element.dataset.agentId;
    if (!sessionUuid) return;

    const target = this.inputTarget?.value?.trim();
    if (!target) return;

    const hub = this.hub ?? (await this.#hubReady);
    if (hub) hub.moveAgentWorkspace(sessionUuid, null, target);
    this.#closeDialog();
  }

  #closeDialog() {
    this.element.closest("dialog")?.close();
  }

  #escapeHtml(str) {
    const div = document.createElement("div");
    div.textContent = str;
    return div.innerHTML;
  }
}
