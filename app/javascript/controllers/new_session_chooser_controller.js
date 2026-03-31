import { Controller } from "@hotwired/stimulus";
import { HubManager } from "connections";

export default class extends Controller {
  static targets = [
    "targetSection",
    "targetSelect",
    "targetPrompt",
    "agentButton",
    "accessoryButton",
  ];

  static values = {
    hubId: String,
  };

  connect() {
    if (!this.hubIdValue) return;

    this.spawnTargets = [];
    this.selectedTargetId = null;
    this.unsubscribers = [];

    HubManager.acquire(this.hubIdValue).then((hub) => {
      this.hub = hub;
      this.spawnTargets = hub.spawnTargets.current();
      hub.spawnTargets.load().catch(() => {});
      this.#renderTargetSelect();

      this.unsubscribers.push(
        this.hub.spawnTargets.onChange((targets) => {
          this.spawnTargets = Array.isArray(targets) ? targets : [];
          this.#renderTargetSelect();
        }),
      );
    });

    const dialog = this.element.closest("dialog");
    if (dialog) {
      this._onToggle = () => {
        if (dialog.open) this.#renderTargetSelect();
      };
      dialog.addEventListener("toggle", this._onToggle);
    }
  }

  disconnect() {
    this.unsubscribers?.forEach((unsub) => unsub());
    this.unsubscribers = null;

    const dialog = this.element.closest("dialog");
    if (dialog && this._onToggle) {
      dialog.removeEventListener("toggle", this._onToggle);
    }

    const hub = this.hub;
    this.hub = null;
    hub?.release();
  }

  selectTarget() {
    if (!this.hasTargetSelectTarget) return;
    this.selectedTargetId = this.targetSelectTarget.value || null;
    this.#updateState();
  }

  chooseAgent() {
    this.#openSessionModal("new-agent-modal");
  }

  chooseAccessory() {
    this.#openSessionModal("new-accessory-modal");
  }

  #openSessionModal(modalId) {
    if (!this.selectedTargetId) return;

    document.dispatchEvent(
      new CustomEvent("botster:new-session-target", {
        detail: { targetId: this.selectedTargetId },
      }),
    );

    this.element.closest("dialog")?.close();
    document.getElementById(modalId)?.showModal();
  }

  #renderTargetSelect() {
    if (!this.hasTargetSelectTarget || !this.hasTargetSectionTarget) return;

    const select = this.targetSelectTarget;
    select.innerHTML = "";

    const emptyOption = document.createElement("option");
    emptyOption.value = "";
    emptyOption.textContent = this.spawnTargets.length
      ? "Select a spawn target"
      : "No admitted spawn targets";
    select.appendChild(emptyOption);

    this.spawnTargets.forEach((target) => {
      const option = document.createElement("option");
      option.value = target.id;
      const branchSuffix = target.current_branch ? ` (${target.current_branch})` : "";
      option.textContent = `${target.name || target.path}${branchSuffix}`;
      select.appendChild(option);
    });

    if (
      this.selectedTargetId &&
      !this.spawnTargets.some((target) => target.id === this.selectedTargetId)
    ) {
      this.selectedTargetId = null;
    }

    this.targetSectionTarget.classList.remove("hidden");
    select.value = this.selectedTargetId || "";
    this.#updateState();
  }

  #updateState() {
    const disabled = !this.selectedTargetId;

    if (this.hasAgentButtonTarget) {
      this.agentButtonTarget.disabled = disabled;
    }

    if (this.hasAccessoryButtonTarget) {
      this.accessoryButtonTarget.disabled = disabled;
    }

    if (!this.hasTargetPromptTarget) return;

    if (this.selectedTargetId) {
      this.targetPromptTarget.textContent =
        "Spawn target selected. Now choose whether to start an agent or an accessory.";
    } else if (this.spawnTargets.length === 0) {
      this.targetPromptTarget.textContent =
        "Add a spawn target in Device Settings before creating a session.";
    } else {
      this.targetPromptTarget.textContent =
        "Choose a spawn target first. Session type comes after location.";
    }
  }
}
