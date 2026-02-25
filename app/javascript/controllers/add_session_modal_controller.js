import { Controller } from "@hotwired/stimulus";
import { ConnectionManager, HubConnection } from "connections";

/**
 * Add Session Modal Controller
 *
 * Populates session type options when the modal opens, then sends
 * add_session command when user selects a type.
 *
 * Agent ID is set on the modal element via data-agent-id before opening.
 */
export default class extends Controller {
  static values = { hubId: String };
  static targets = ["list"];

  #hubReady = null;
  #unsubscribers = [];

  connect() {
    if (!this.hubIdValue) return;

    this.#hubReady = ConnectionManager.acquire(HubConnection, this.hubIdValue, {
      hubId: this.hubIdValue,
    }).then((hub) => {
      this.hub = hub;
      return hub;
    });

    // Listen for the dialog's toggle event to fetch types when opened
    const dialog = this.element.closest("dialog");
    if (dialog) {
      this._onToggle = (e) => {
        if (dialog.open) this.#onOpen();
      };
      dialog.addEventListener("toggle", this._onToggle);
    }
  }

  disconnect() {
    this.#unsubscribers.forEach((unsub) => unsub());
    this.#unsubscribers = [];

    const dialog = this.element.closest("dialog");
    if (dialog && this._onToggle) {
      dialog.removeEventListener("toggle", this._onToggle);
    }

    this.#hubReady = null;
    const hub = this.hub;
    this.hub = null;
    hub?.release();
  }

  async #onOpen() {
    const hub = this.hub ?? (await this.#hubReady);
    if (!hub) return;

    const agentId = this.element.dataset.agentId;
    if (!agentId) return;

    // Show loading state
    this.listTarget.innerHTML =
      '<p class="text-sm text-zinc-500 text-center py-4">Loading session types...</p>';

    // Request session types with one-shot listener
    const types = await new Promise((resolve) => {
      const timer = setTimeout(() => {
        unsub();
        resolve([{ name: "shell", label: "Shell", description: "Raw bash shell" }]);
      }, 2000);
      const unsub = hub.on("sessionTypes", ({ agentId: id, sessionTypes }) => {
        if (id !== agentId) return;
        clearTimeout(timer);
        unsub();
        resolve(
          sessionTypes.length > 0
            ? sessionTypes
            : [{ name: "shell", label: "Shell", description: "Raw bash shell" }],
        );
      });
      hub.requestSessionTypes(agentId);
    });

    this.#renderTypes(types, agentId);
  }

  #renderTypes(types, agentId) {
    this.listTarget.innerHTML = "";

    types.forEach((t) => {
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = [
        "w-full text-left px-4 py-3 rounded-lg",
        "bg-zinc-800/50 hover:bg-zinc-700/50 border border-zinc-700/50 hover:border-zinc-600/50",
        "transition-colors",
      ].join(" ");
      btn.addEventListener("click", () => this.#selectType(agentId, t.name));

      const inner = document.createElement("div");
      inner.className = "flex items-center gap-3";

      const iconWrap = document.createElement("span");
      iconWrap.className =
        "size-8 rounded-md bg-zinc-700/50 text-zinc-400 flex items-center justify-center border border-zinc-600/30 shrink-0";
      iconWrap.textContent = ">";
      inner.appendChild(iconWrap);

      const textWrap = document.createElement("div");
      const name = document.createElement("div");
      name.className = "text-sm font-medium text-zinc-200";
      name.textContent = t.label || t.name;
      textWrap.appendChild(name);

      if (t.description) {
        const desc = document.createElement("div");
        desc.className = "text-xs text-zinc-500";
        desc.textContent = t.description;
        textWrap.appendChild(desc);
      }

      inner.appendChild(textWrap);
      btn.appendChild(inner);
      this.listTarget.appendChild(btn);
    });
  }

  async #selectType(agentId, sessionType) {
    const hub = this.hub ?? (await this.#hubReady);
    if (hub) hub.addSession(agentId, sessionType);

    const dialog = this.element.closest("dialog");
    dialog?.close();
  }
}
