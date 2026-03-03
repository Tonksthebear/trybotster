import { Controller } from "@hotwired/stimulus";
import { HubConnectionManager, HubConnection } from "connections";

/**
 * Agent List Controller
 *
 * Renders agents grouped by workspace using <template> elements for markup.
 * Stores agents and workspaces as JSON values so the list persists across
 * Turbo navigations when used with data-turbo-permanent.
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
 * Workspace template placeholders:
 *   data-field="workspace-title"  - Sets textContent to workspace title
 *   data-field="workspace-status" - Sets textContent to workspace status
 *   data-workspace-badge          - Status badge dot (gets status-specific color class)
 *
 * Usage:
 *   <div data-controller="agent-list"
 *        data-agent-list-hub-id-value="<%= Current.hub.id %>"
 *        data-agent-list-selected-id-value="<%= Current.agent&.id %>"
 *        data-agent-list-agents-value="[]"
 *        data-turbo-permanent
 *        id="sidebar-agent-list-desktop">
 *     <template data-agent-list-target="workspaceTemplate">...</template>
 *     <template data-agent-list-target="template">...</template>
 *     <div data-agent-list-target="list"></div>
 *     <div data-agent-list-target="empty" class="hidden">No agents</div>
 *     <div data-agent-list-target="loading">Connecting...</div>
 *   </div>
 */
export default class extends Controller {
  static targets = [
    "template",
    "workspaceTemplate",
    "list",
    "empty",
    "loading",
    "header",
  ];

  static values = {
    hubId: String,
    agents: { type: Array, default: [] },
    workspaces: { type: Array, default: [] },
    selectedId: String,
  };

  #disconnected = false;
  #turboLoadHandler = () => this.#syncSelectionFromUrl();
  #collapsedWorkspaces = new Set();
  #renderScheduled = false;

  connect() {
    if (!this.hubIdValue) return;
    this.#disconnected = false;

    // Track unsubscribe functions for cleanup
    this.unsubscribers = [];

    // Sync selection from URL on connect and turbo navigations
    this.#syncSelectionFromUrl();
    document.addEventListener("turbo:load", this.#turboLoadHandler);

    // agentsValueChanged fires automatically on connect if value differs from default,
    // so persisted data from turbo-permanent renders without explicit call

    HubConnectionManager.acquire(HubConnection, this.hubIdValue, {
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

      this.unsubscribers.push(
        this.hub.onWorkspaceList((workspaces) => {
          this.workspacesValue = workspaces;
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

    document.removeEventListener("turbo:load", this.#turboLoadHandler);

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
    this.#scheduleRender();
    // Agents may arrive after connect — re-sync selection from URL
    this.#syncSelectionFromUrl();
  }

  // Stimulus: called when workspacesValue changes
  workspacesValueChanged() {
    this.#scheduleRender();
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

  // Action: toggle workspace collapse state
  toggleWorkspace(event) {
    const wsId = event.currentTarget.dataset.workspaceId;
    if (!wsId) return;

    if (this.#collapsedWorkspaces.has(wsId)) {
      this.#collapsedWorkspaces.delete(wsId);
    } else {
      this.#collapsedWorkspaces.add(wsId);
    }

    this.#render();
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
      const controller = modal.querySelector(
        "[data-controller='delete-agent-modal']",
      );
      if (controller) {
        controller.dataset.agentId = agentId;
        controller.dataset.deleteAgentModalInWorktreeValue = inWorktree;
      }
      const nameEl = modal.querySelector("[data-agent-name]");
      if (nameEl) nameEl.textContent = name;
      modal.showModal();
    }
  }

  // Private: coalesce rapid value changes (agents + workspaces arrive
  // back-to-back from the same message) into a single render via microtask.
  #scheduleRender() {
    if (this.#renderScheduled) return;
    this.#renderScheduled = true;
    queueMicrotask(() => {
      this.#renderScheduled = false;
      this.#render();
    });
  }

  // Private: render the agent list, morphing via Turbo.morphElements
  #render() {
    if (!this.hasTemplateTarget || !this.hasListTarget) return;

    const agents = this.agentsValue;
    const workspaces = this.workspacesValue;
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

    // Build agent lookup by id -> flat array index (for URL routing)
    const agentById = new Map();
    agents.forEach((agent, index) => {
      agentById.set(agent.id, { agent, index });
    });

    // Build target DOM in a detached container
    const newList = this.listTarget.cloneNode(false);

    if (workspaces.length > 0 && this.hasWorkspaceTemplateTarget) {
      this.#renderGrouped(newList, workspaces, agentById, hubId);
    } else {
      this.#renderFlat(newList, agents, hubId);
    }

    // Morph the existing list into the new state — preserves DOM nodes,
    // hover states, transitions, focus. Idiomorph keys by element id.
    window.Turbo.morphElements(this.listTarget, newList, {
      morphStyle: "innerHTML",
    });
  }

  // Private: render agents grouped under workspace headers
  #renderGrouped(container, workspaces, agentById, hubId) {
    const renderedAgentIds = new Set();

    for (const ws of workspaces) {
      const wsAgentIds = Array.isArray(ws.agents) ? ws.agents : [];
      const wsAgents = wsAgentIds
        .map((id) => agentById.get(id))
        .filter(Boolean);

      if (wsAgents.length === 0) continue;

      // Workspace header
      const header = this.#buildWorkspaceHeader(ws);
      container.appendChild(header);

      // Agent items (hidden if workspace is collapsed)
      const collapsed = this.#collapsedWorkspaces.has(ws.id);
      for (const { agent, index } of wsAgents) {
        const el = this.#buildAgentItem(agent, index, hubId);
        if (collapsed) el.classList.add("hidden");
        el.dataset.workspaceId = ws.id;
        container.appendChild(el);
        renderedAgentIds.add(agent.id);
      }
    }

    // Render any ungrouped agents (no workspace match)
    const ungrouped = [];
    for (const [id, entry] of agentById) {
      if (!renderedAgentIds.has(id)) {
        ungrouped.push(entry);
      }
    }

    if (ungrouped.length > 0) {
      for (const { agent, index } of ungrouped) {
        container.appendChild(this.#buildAgentItem(agent, index, hubId));
      }
    }
  }

  // Private: render agents in a flat list (backward compat)
  #renderFlat(container, agents, hubId) {
    agents.forEach((agent, index) => {
      container.appendChild(this.#buildAgentItem(agent, index, hubId));
    });
  }

  // Private: build a workspace group header element
  #buildWorkspaceHeader(ws) {
    const clone = this.workspaceTemplateTarget.content.cloneNode(true);
    const root = clone.firstElementChild;

    // Stable ID for Idiomorph keying
    root.id = `workspace-${ws.id}`;
    root.dataset.workspaceId = ws.id;

    // Wire up toggle action
    root.dataset.action = "click->agent-list#toggleWorkspace";

    // Fill workspace fields
    root.querySelectorAll("[data-field]").forEach((el) => {
      const field = el.dataset.field;
      if (field === "workspace-title") {
        el.textContent = ws.title || ws.id;
      } else if (field === "workspace-status") {
        el.textContent = ws.status || "inactive";
      } else if (field === "workspace-count") {
        const count = Array.isArray(ws.agents) ? ws.agents.length : 0;
        el.textContent = count;
      }
    });

    // Status badge color
    const badge = root.querySelector("[data-workspace-badge]");
    if (badge) {
      const colorClass = this.#statusBadgeColor(ws.status);
      badge.classList.add(colorClass);
    }

    // Collapse indicator
    const collapsed = this.#collapsedWorkspaces.has(ws.id);
    const chevron = root.querySelector("[data-workspace-chevron]");
    if (chevron) {
      if (collapsed) {
        chevron.classList.add("-rotate-90");
      } else {
        chevron.classList.remove("-rotate-90");
      }
    }

    return root;
  }

  // Private: build a single agent item element
  #buildAgentItem(agent, index, hubId) {
    const clone = this.templateTarget.content.cloneNode(true);
    const root = clone.firstElementChild;

    // Stable ID for Idiomorph keying
    root.id = `agent-${agent.id}`;

    // Set data attributes
    this.#setDataAttributes(root, agent, index);

    // Fill fields
    root.querySelectorAll("[data-field]").forEach((el) => {
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

    // Interpolate hrefs
    root.querySelectorAll("[data-href]").forEach((el) => {
      el.href = el.dataset.href
        .replace("{hubId}", hubId)
        .replace("{index}", index);
      delete el.dataset.href;
    });

    // Selection state
    if (agent.id === this.selectedIdValue) {
      root.dataset.selected = "true";
    }

    // Notification badge
    const badge = root.querySelector("[data-notification-badge]");
    if (badge) {
      badge.classList.toggle("hidden", !agent.notification);
    }

    return root;
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

  // Private: map workspace status to Tailwind badge color class
  #statusBadgeColor(status) {
    switch (status) {
      case "active":
        return "bg-success-500";
      case "suspended":
        return "bg-yellow-500";
      case "orphaned":
        return "bg-orange-500";
      case "closed":
        return "bg-zinc-600";
      default:
        return "bg-zinc-600";
    }
  }

  // Private: derive selected agent from current URL path
  #syncSelectionFromUrl() {
    const match = window.location.pathname.match(
      /\/hubs\/[^/]+\/agents\/(\d+)/,
    );
    if (!match) {
      // Not on an agent page — clear selection
      if (this.selectedIdValue) this.selectedIdValue = "";
      return;
    }

    const index = parseInt(match[1], 10);
    const agent = this.agentsValue[index];
    if (agent && agent.id !== this.selectedIdValue) {
      this.selectedIdValue = agent.id;
    }
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
