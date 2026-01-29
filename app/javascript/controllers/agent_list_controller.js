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
 *   data-field="name"     - Sets textContent to agent.name
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
  static targets = ["template", "list", "empty", "loading"];

  static values = {
    hubId: String,
    agents: { type: Array, default: [] },
    selectedId: String,
  };

  connect() {
    console.debug("[agent-list] connect(), hubIdValue:", this.hubIdValue);
    if (!this.hubIdValue) return;

    // Track unsubscribe functions for cleanup
    this.unsubscribers = [];

    // agentsValueChanged fires automatically on connect if value differs from default,
    // so persisted data from turbo-permanent renders without explicit call

    console.debug("[agent-list] calling acquire...");
    ConnectionManager.acquire(HubConnection, this.hubIdValue, {
      hubId: this.hubIdValue,
      fromFragment: true,
    }).then((hub) => {
      console.debug(
        "[agent-list] acquire resolved, hub:",
        hub,
        "isConnected:",
        hub?.isConnected?.(),
      );
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

      // Request agents when connected - onConnected fires immediately if already connected
      this.unsubscribers.push(
        this.hub.onConnected(() => {
          console.debug(
            "[agent-list] onConnected callback fired, requesting agents",
          );
          const result = this.hub.requestAgents();
          console.debug("[agent-list] requestAgents returned:", result);
        }),
      );
    });
  }

  disconnect() {
    // Clean up event subscriptions before releasing
    this.unsubscribers?.forEach((unsub) => unsub());
    this.unsubscribers = null;

    this.hub?.release();
    this.hub = null;
  }

  // Stimulus: called when agentsValue changes
  agentsValueChanged() {
    console.debug(
      "[agent-list] agentsValueChanged, count:",
      this.agentsValue?.length,
    );
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

  // Action: delete an agent
  delete(event) {
    event.preventDefault();
    event.stopPropagation();

    const agentId = event.currentTarget.dataset.agentId;
    const agent = this.agentsValue.find((a) => a.id === agentId);
    const name = agent?.name || agent?.id || "this agent";

    if (confirm(`Delete ${name}?`)) {
      this.hub?.deleteAgent(agentId, false);
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
          el.textContent = agent.name || agent.id;
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
