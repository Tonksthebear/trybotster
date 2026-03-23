import { Controller } from "@hotwired/stimulus";
import { HubManager } from "connections";

/**
 * NewAccessoryModalController - Handles accessory creation.
 *
 * Fetches available accessory configs from CLI, lets user select one
 * and a workspace, then sends create_accessory command.
 */
export default class extends Controller {
  static targets = [
    "targetSection",
    "targetSelect",
    "targetPrompt",
    "configSection",
    "accessoryList",
    "workspaceSelect",
    "workspaceSection",
    "noConfigWarning",
    "submitButton",
  ];

  static values = {
    hubId: String,
  };

  connect() {
    if (!this.hubIdValue) return;

    this.accessories = [];
    this.workspaces = [];
    this.spawnTargets = [];
    this.selectedTargetId = null;
    this.selectedAccessory = null;
    this.unsubscribers = [];
    this._onExternalTargetSelection = (event) => {
      this.#applySelectedTarget(event.detail?.targetId || null);
    };

    document.addEventListener(
      "botster:new-session-target",
      this._onExternalTargetSelection,
    );

    HubManager.acquire(this.hubIdValue).then((hub) => {
      this.hub = hub;
      this.spawnTargets = Array.isArray(hub.spawnTargets) ? hub.spawnTargets : [];
      this.workspaces = Array.isArray(hub.openWorkspaces) ? hub.openWorkspaces : [];
      this.#renderTargetSelect();
      this.#renderWorkspaceSelect();
      this.#updateFlowVisibility();

      if (this.selectedTargetId) {
        const config = hub.getAgentConfig(this.selectedTargetId);
        this.accessories = Array.isArray(config.accessories) ? config.accessories : [];
        this.#renderAccessoryList();
        if (!this.hub.hasAgentConfig(this.selectedTargetId)) {
          this.hub.ensureAgentConfig(this.selectedTargetId);
        }
      }

      this.unsubscribers.push(
        this.hub.onSpawnTargetList((targets) => {
          this.spawnTargets = Array.isArray(targets) ? targets : [];
          this.#renderTargetSelect();
          this.#updateFlowVisibility();
        }),
      );

      this.unsubscribers.push(
        this.hub.on("agentConfig", ({ targetId, accessories }) => {
          if (targetId && this.selectedTargetId && targetId !== this.selectedTargetId) return;
          this.accessories = Array.isArray(accessories) ? accessories : [];
          this.#renderAccessoryList();
        }),
      );

      this.unsubscribers.push(
        this.hub.onOpenWorkspaceList((workspaces) => {
          this.workspaces = Array.isArray(workspaces) ? workspaces : [];
          this.#renderWorkspaceSelect();
        }),
      );
    });

    const dialog = this.element.closest("dialog");
    if (dialog) {
      this._onToggle = () => {
        if (dialog.open) this.#onOpen();
      };
      dialog.addEventListener("toggle", this._onToggle);
    }
  }

  disconnect() {
    this.unsubscribers?.forEach((unsub) => unsub());
    this.unsubscribers = null;

    if (this._onExternalTargetSelection) {
      document.removeEventListener(
        "botster:new-session-target",
        this._onExternalTargetSelection,
      );
      this._onExternalTargetSelection = null;
    }

    const dialog = this.element.closest("dialog");
    if (dialog && this._onToggle) {
      dialog.removeEventListener("toggle", this._onToggle);
    }

    const hub = this.hub;
    this.hub = null;
    hub?.release();
  }

  selectAccessory(event) {
    const name = event.currentTarget.dataset.accessoryName;
    if (!name) return;

    this.selectedAccessory = name;

    this.accessoryListTarget
      .querySelectorAll("[data-accessory-name]")
      .forEach((el) => {
        if (el.dataset.accessoryName === name) {
          el.dataset.selected = "true";
        } else {
          delete el.dataset.selected;
        }
      });

    this.#updateSubmitState();
  }

  selectTarget() {
    if (!this.hasTargetSelectTarget) return;
    this.#applySelectedTarget(this.targetSelectTarget.value || null);
  }

  submit() {
    if (!this.selectedAccessory || !this.hub || !this.selectedTargetId) return;

    const workspace = this.#selectedWorkspace();

    this.hub.createAccessory(
      this.selectedAccessory,
      workspace?.id || null,
      workspace?.name || null,
      this.selectedTargetId,
    );

    this.#reset();
    this.element.closest("dialog")?.close();
  }

  #onOpen() {
    if (this.hub) {
      if (this.selectedTargetId) {
        const config = this.hub.getAgentConfig(this.selectedTargetId);
        this.accessories = Array.isArray(config.accessories) ? config.accessories : [];
        this.#renderAccessoryList();
        if (!this.hub.hasAgentConfig(this.selectedTargetId)) {
          this.hub.ensureAgentConfig(this.selectedTargetId);
        }
      }
    }

    this.selectedAccessory = null;
    this.#updateSubmitState();
  }

  #selectedWorkspace() {
    if (!this.hasWorkspaceSelectTarget) return null;
    const workspaceId = this.workspaceSelectTarget.value || null;
    if (!workspaceId) return null;
    return (
      this.workspaces.find((ws) => ws?.id === workspaceId) || {
        id: workspaceId,
        name: null,
      }
    );
  }

  #updateSubmitState() {
    if (!this.hasSubmitButtonTarget) return;
    this.submitButtonTarget.disabled = !this.selectedAccessory || !this.selectedTargetId;
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
    this.#updateFlowVisibility();
  }

  #renderAccessoryList() {
    if (!this.hasAccessoryListTarget) return;

    this.accessoryListTarget.innerHTML = "";

    if (!this.selectedTargetId) {
      this.accessoryListTarget.innerHTML = `
        <p class="text-sm text-zinc-500 text-center py-4">Choose a spawn target first</p>
      `;
      if (this.hasNoConfigWarningTarget) {
        this.noConfigWarningTarget.classList.add("hidden");
      }
      return;
    }

    if (this.accessories.length === 0) {
      if (this.hasNoConfigWarningTarget) {
        this.noConfigWarningTarget.classList.remove("hidden");
      }
      return;
    }

    if (this.hasNoConfigWarningTarget) {
      this.noConfigWarningTarget.classList.add("hidden");
    }

    this.accessories.forEach((name) => {
      const btn = document.createElement("button");
      btn.type = "button";
      btn.dataset.accessoryName = name;
      btn.dataset.action = "new-accessory-modal#selectAccessory";
      btn.className = [
        "w-full text-left px-3 py-2.5 rounded-lg border transition-colors",
        "border-zinc-700 hover:border-primary-500/50 hover:bg-zinc-800/50",
        "data-[selected=true]:border-primary-500 data-[selected=true]:bg-primary-500/10",
      ].join(" ");

      const inner = document.createElement("div");
      inner.className = "flex items-center gap-3";

      const iconWrap = document.createElement("span");
      iconWrap.className =
        "size-8 rounded-md bg-zinc-700/50 text-zinc-400 flex items-center justify-center border border-zinc-600/30 shrink-0 font-mono text-xs";
      iconWrap.textContent = ">";
      inner.appendChild(iconWrap);

      const textWrap = document.createElement("div");
      textWrap.className = "flex-1 min-w-0";

      const nameEl = document.createElement("div");
      nameEl.className = "text-sm font-medium text-zinc-200 font-mono";
      nameEl.textContent = name;
      textWrap.appendChild(nameEl);

      inner.appendChild(textWrap);
      btn.appendChild(inner);
      this.accessoryListTarget.appendChild(btn);
    });
  }

  #renderWorkspaceSelect() {
    if (!this.hasWorkspaceSelectTarget || !this.hasWorkspaceSectionTarget) return;

    const select = this.workspaceSelectTarget;
    select.innerHTML = "";

    const workspaces = this.workspaces.filter(
      (ws) => ws && typeof ws === "object" && ws.id,
    );

    if (workspaces.length === 0) {
      this.workspaceSectionTarget.classList.add("hidden");
      return;
    }

    this.workspaceSectionTarget.classList.remove("hidden");

    const emptyOption = document.createElement("option");
    emptyOption.value = "";
    emptyOption.textContent = "None";
    select.appendChild(emptyOption);

    workspaces.forEach((ws) => {
      const option = document.createElement("option");
      option.value = ws.id;
      option.textContent = ws.name || ws.id;
      select.appendChild(option);
    });
  }

  #reset() {
    this.selectedAccessory = null;
    this.selectedTargetId = null;
    this.accessories = [];
    this.#updateSubmitState();

    if (this.hasTargetSelectTarget) {
      this.targetSelectTarget.value = "";
    }

    this.#renderAccessoryList();
    this.#updateFlowVisibility();

    if (this.hasAccessoryListTarget) {
      this.accessoryListTarget
        .querySelectorAll("[data-selected]")
        .forEach((el) => delete el.dataset.selected);
    }
  }

  #applySelectedTarget(targetId) {
    this.selectedTargetId = targetId;
    this.selectedAccessory = null;
    this.accessories = targetId && this.hub
      ? this.hub.getAgentConfig(targetId).accessories
      : [];

    if (this.hasTargetSelectTarget) {
      this.targetSelectTarget.value = targetId || "";
    }

    this.#renderAccessoryList();
    this.#updateFlowVisibility();
    this.#updateSubmitState();

    if (!targetId || !this.hub) return;
    if (!this.hub.hasAgentConfig(targetId)) {
      this.hub.ensureAgentConfig(targetId);
    }
  }

  #updateFlowVisibility() {
    if (this.hasConfigSectionTarget) {
      this.configSectionTarget.classList.toggle("hidden", !this.selectedTargetId);
    }

    if (!this.hasTargetPromptTarget) return;

    if (this.selectedTargetId) {
      this.targetPromptTarget.textContent =
        "Spawn target selected. Now choose an accessory configuration.";
    } else if (this.spawnTargets.length === 0) {
      this.targetPromptTarget.textContent =
        "Add a spawn target in Device Settings before starting an accessory.";
    } else {
      this.targetPromptTarget.textContent =
        "Choose a spawn target to unlock accessory configuration.";
    }
  }
}
