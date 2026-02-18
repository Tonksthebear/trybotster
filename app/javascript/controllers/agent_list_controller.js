import { Controller } from "@hotwired/stimulus";
import { ConnectionManager, HubConnection } from "connections";

/**
 * Agent List Controller
 *
 * Renders a list of agents using a <template> element for markup.
 * Stores agents as a JSON value so the list persists across Turbo navigations
 * when used with data-turbo-permanent.
 *
 * Each instance can have its own template, allowing different visual
 * representations (sidebar compact vs. hub page cards).
 *
 * All values come from Rails via data attributes — no URL parsing.
 *
 * Template placeholders (data attributes):
 *   data-field="name"     - Sets textContent to agent.display_name
 *   data-field="subtext"  - Sets textContent to "profile · branch"
 *   data-field="id"       - Sets textContent to agent.id
 *   data-field="index"    - Sets textContent to agent index
 *   data-href             - Interpolates {hubId} and {index} in href
 *   data-agent-id         - Sets to agent.id (for actions)
 *   data-agent-index      - Sets to agent index (for actions)
 *
 * Usage:
 *   <div data-controller="agent-list"
 *        data-agent-list-hub-id-value="<%= Current.hub.id %>"
 *        data-agent-list-selected-id-value="<%= Current.agent&.id %>"
 *        data-agent-list-agents-value="[]"
 *        data-turbo-permanent
 *        id="sidebar-agent-list-desktop">
 *     <template data-agent-list-target="template">
 *       <a data-href="/hubs/{hubId}/agents/{index}" data-field="name" class="..."></a>
 *     </template>
 *     <div data-agent-list-target="list"></div>
 *     <div data-agent-list-target="empty" class="hidden">No agents</div>
 *     <div data-agent-list-target="loading">Connecting...</div>
 *   </div>
 */
export default class extends Controller {
  static targets = ["template", "list", "empty", "loading", "header"];

  static values = {
    hubId: String,
    agents: { type: Array, default: [] },
    selectedId: String,
  };

  #disconnected = false;

  connect() {
    if (!this.hubIdValue) return;
    this.#disconnected = false;

    // Track unsubscribe functions for cleanup
    this.unsubscribers = [];

    // agentsValueChanged fires automatically on connect if value differs from default,
    // so persisted data from turbo-permanent renders without explicit call

    ConnectionManager.acquire(HubConnection, this.hubIdValue, {
      hubId: this.hubIdValue,
    }).then(async (hub) => {
      if (this.#disconnected) {
        hub.release();
        return;
      }
      this.hub = hub;

      this.unsubscribers.push(
        this.hub.onAgentList((agents) => {
          this.agentsValue = agents;
        }),
      );

      // Refresh list when agents are created or deleted
      this.unsubscribers.push(
        this.hub.on("agentCreated", () => {
          this.hub.requestAgents();
        }),
      );

      this.unsubscribers.push(
        this.hub.on("agentDeleted", () => {
          this.hub.requestAgents();
        }),
      );

      // Handle connection ready (initial or reconnection)
      // Use onConnected which fires immediately if already connected
      this.unsubscribers.push(
        this.hub.onConnected(() => {
          this.hub.requestAgents();
        }),
      );

      // No explicit subscribe() — health events drive the WebRTC lifecycle.
      // onConnected above fires when handshake completes (or immediately if already connected).
    });
  }

  disconnect() {
    this.#disconnected = true;

    // Clean up event subscriptions before releasing
    this.unsubscribers?.forEach((unsub) => unsub());
    this.unsubscribers = null;

    // Just release - don't unsubscribe. HubConnection is shared and
    // the subscription can be reused by other controllers after navigation.
    const hub = this.hub;
    this.hub = null;
    hub?.release();
  }

  // Stimulus: called when agentsValue changes
  agentsValueChanged() {
    this.#render();
  }

  // Stimulus: called when selectedIdValue changes
  selectedIdValueChanged() {
    this.#updateSelection();
  }

  // Action: select an agent
  select(event) {
    const agentId = event.currentTarget.dataset.agentId;
    if (agentId && this.hub) {
      this.selectedIdValue = agentId;
      this.hub.selectAgent(agentId);
    }
  }

  // Action: delete an agent - opens confirmation modal
  delete(event) {
    event.preventDefault();
    event.stopPropagation();

    const agentId = event.currentTarget.dataset.agentId;
    const agent = this.agentsValue.find((a) => a.id === agentId);
    const name = agent?.display_name || agent?.id || "this agent";
    const inWorktree = agent?.in_worktree ?? true; // default to showing option

    // Set pending info on modal controller element and open dialog
    const modal = document.getElementById("delete-agent-modal");
    if (modal) {
      const controller = modal.querySelector("[data-controller='delete-agent-modal']");
      if (controller) {
        controller.dataset.agentId = agentId;
        controller.dataset.deleteAgentModalInWorktreeValue = inWorktree;
      }
      const nameEl = modal.querySelector("[data-agent-name]");
      if (nameEl) nameEl.textContent = name;
      modal.showModal();
    }
  }

  // Private: render the agent list from template
  #render() {
    if (!this.hasTemplateTarget || !this.hasListTarget) return;

    const agents = this.agentsValue;
    const hubId = this.hubIdValue;

    // Toggle empty/loading/list visibility
    if (this.hasLoadingTarget) {
      this.loadingTarget.classList.add("hidden");
    }
    if (this.hasHeaderTarget) {
      this.headerTarget.classList.remove("hidden");
    }

    if (agents.length === 0) {
      this.listTarget.innerHTML = "";
      if (this.hasEmptyTarget) {
        this.emptyTarget.classList.remove("hidden");
      }
      return;
    }

    if (this.hasEmptyTarget) {
      this.emptyTarget.classList.add("hidden");
    }

    // Build new list
    const fragment = document.createDocumentFragment();

    agents.forEach((agent, index) => {
      const clone = this.templateTarget.content.cloneNode(true);
      const root = clone.firstElementChild;

      // Set data attributes for actions
      this.#setDataAttributes(root, agent, index);

      // Fill in field values
      clone.querySelectorAll("[data-field]").forEach((el) => {
        const field = el.dataset.field;
        if (field === "name") {
          el.textContent = agent.display_name || agent.id;
        } else if (field === "subtext") {
          const parts = [];
          if (agent.profile_name) parts.push(agent.profile_name);
          if (agent.branch_name) parts.push(agent.branch_name);
          el.textContent = parts.join(" · ");
        } else if (field === "id") {
          el.textContent = agent.id;
        } else if (field === "index") {
          el.textContent = index + 1;
        } else if (agent[field] !== undefined) {
          el.textContent = agent[field];
        }
      });

      // Interpolate href templates
      clone.querySelectorAll("[data-href]").forEach((el) => {
        const template = el.dataset.href;
        el.href = template.replace("{hubId}", hubId).replace("{index}", index);
        delete el.dataset.href;
      });

      // Mark selected
      if (agent.id === this.selectedIdValue) {
        root.dataset.selected = "true";
      }

      fragment.appendChild(clone);
    });

    this.listTarget.innerHTML = "";
    this.listTarget.appendChild(fragment);
  }

  // Private: set data attributes on the cloned element
  #setDataAttributes(el, agent, index) {
    if (!el) return;

    // Set on root element
    el.dataset.agentId = agent.id;
    el.dataset.agentIndex = index;

    // Also set on any children that need it
    el.querySelectorAll("[data-agent-id]").forEach((child) => {
      child.dataset.agentId = agent.id;
    });
    el.querySelectorAll("[data-agent-index]").forEach((child) => {
      child.dataset.agentIndex = index;
    });
  }

  // Private: update selection state without full re-render
  #updateSelection() {
    if (!this.hasListTarget) return;

    this.listTarget.querySelectorAll("[data-agent-id]").forEach((el) => {
      if (el.dataset.agentId === this.selectedIdValue) {
        el.dataset.selected = "true";
      } else {
        delete el.dataset.selected;
      }
    });
  }
}
