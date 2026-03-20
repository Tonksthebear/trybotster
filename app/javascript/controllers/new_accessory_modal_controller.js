import { Controller } from "@hotwired/stimulus";
import { HubConnectionManager } from "connections/hub_connection_manager";
import { HubConnection } from "connections/hub_connection";

/**
 * NewAccessoryModalController - Handles accessory creation.
 *
 * Fetches available accessory configs from CLI, lets user select one
 * and a workspace, then sends create_accessory command.
 *
 * No prompt step needed — accessories are plain terminals.
 */
export default class extends Controller {
  static targets = [
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
    this.selectedAccessory = null;
    this.unsubscribers = [];

    HubConnectionManager.acquire(HubConnection, this.hubIdValue, {
      hubId: this.hubIdValue,
    }).then(async (hub) => {
      this.hub = hub;

      this.unsubscribers.push(
        this.hub.on("agentConfig", ({ accessories }) => {
          this.accessories = Array.isArray(accessories) ? accessories : [];
          this.#renderAccessoryList();
        }),
      );

      this.unsubscribers.push(
        this.hub.on("workspaceList", (workspaces) => {
          this.workspaces = Array.isArray(workspaces) ? workspaces : [];
          this.#renderWorkspaceSelect();
        }),
      );

      this.unsubscribers.push(
        this.hub.onConnected(() => {
          this.hub.requestAgentConfig();
          this.hub.requestWorkspaces();
        }),
      );
    });

    // Refresh data when modal opens
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

    const dialog = this.element.closest("dialog");
    if (dialog && this._onToggle) {
      dialog.removeEventListener("toggle", this._onToggle);
    }

    const hub = this.hub;
    this.hub = null;
    hub?.release();
  }

  // Action: select an accessory config
  selectAccessory(event) {
    const name = event.currentTarget.dataset.accessoryName;
    if (!name) return;

    this.selectedAccessory = name;

    // Update visual selection
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

  // Action: submit and create accessory
  submit() {
    if (!this.selectedAccessory || !this.hub) return;

    const workspace = this.#selectedWorkspace();

    this.hub.createAccessory(
      this.selectedAccessory,
      workspace?.id || null,
      workspace?.name || null,
    );

    this.#reset();

    const dialog = this.element.closest("dialog");
    dialog?.close();
  }

  #onOpen() {
    // Re-fetch fresh data
    if (this.hub) {
      this.hub.requestAgentConfig();
      this.hub.requestWorkspaces();
    }
    // Reset selection state
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
    this.submitButtonTarget.disabled = !this.selectedAccessory;
  }

  #renderAccessoryList() {
    if (!this.hasAccessoryListTarget) return;

    this.accessoryListTarget.innerHTML = "";

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
    if (!this.hasWorkspaceSelectTarget || !this.hasWorkspaceSectionTarget)
      return;

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
    this.#updateSubmitState();
    // Clear visual selection
    if (this.hasAccessoryListTarget) {
      this.accessoryListTarget
        .querySelectorAll("[data-selected]")
        .forEach((el) => delete el.dataset.selected);
    }
  }
}
