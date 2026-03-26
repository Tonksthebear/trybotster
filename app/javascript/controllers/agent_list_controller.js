import { Controller } from "@hotwired/stimulus";
import { HubManager } from "connections";

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
 *   data-href             - Interpolates {hubId} and {sessionUuid} in href
 *   data-agent-id         - Sets to agent.id (for actions)
 *
 * Workspace template placeholders:
 *   data-field="workspace-title"  - Sets textContent to workspace name
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

    HubManager.acquire(this.hubIdValue).then(async (hub) => {
      if (this.#disconnected) {
        hub.release();
        return;
      }
      this.hub = hub;
      this.agentsValue = Array.isArray(hub.agents) ? hub.agents : [];
      this.workspacesValue = Array.isArray(hub.openWorkspaces) ? hub.openWorkspaces : [];

      this.unsubscribers.push(
        this.hub.onAgentList((agents) => {
          this.agentsValue = agents;
        }),
      );

      this.unsubscribers.push(
        this.hub.onOpenWorkspaceList((workspaces) => {
          this.workspacesValue = workspaces;
        }),
      );
    });
  }

  disconnect() {
    this.#disconnected = true;

    document.removeEventListener("turbo:load", this.#turboLoadHandler);

    // Clean up event subscriptions before releasing
    this.unsubscribers?.forEach((unsub) => unsub());
    this.unsubscribers = null;

    // Just release - don't unsubscribe. Hub state is shared and
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

  // Action: rename workspace
  renameWorkspace(event) {
    event.preventDefault();
    event.stopPropagation();
    if (!this.hub) return;

    const wsId = event.currentTarget.dataset.workspaceId;
    if (!wsId) return;

    const workspace = this.workspacesValue.find((ws) => ws?.id === wsId);
    const currentName = workspace?.name || wsId;
    const input = window.prompt("Rename workspace:", currentName);
    if (input === null) return;

    const newName = input.trim();
    if (!newName || newName === currentName) return;

    this.hub.renameWorkspace(wsId, newName);
  }

  // Action: move session to another workspace
  moveAgentWorkspace(event) {
    event.preventDefault();
    event.stopPropagation();
    if (!this.hub) return;

    const agentId = event.currentTarget.dataset.agentId;
    if (!agentId) return;

    const currentWorkspace = this.#workspaceForAgent(agentId);
    const workspaceNames = this.workspacesValue
      .map((ws) => ws?.name || ws?.id)
      .filter(Boolean)
      .join(", ");
    const promptLabel = workspaceNames
      ? `Move session to workspace (name or id).\nExisting: ${workspaceNames}`
      : "Move session to workspace (name or id)";

    const input = window.prompt(promptLabel, currentWorkspace?.name || "");
    if (input === null) return;

    const target = input.trim();
    if (!target) return;

    const existing = this.workspacesValue.find(
      (ws) => ws?.id === target || ws?.name === target,
    );

    this.hub.moveAgentWorkspace(
      agentId,
      existing?.id || null,
      existing?.name || target,
    );
  }

  // Action: delete an agent - opens confirmation modal
  delete(event) {
    event.preventDefault();
    event.stopPropagation();

    const agentId = event.currentTarget.dataset.agentId;
    const agent = this.agentsValue.find((a) => a.id === agentId);
    const name = agent?.label || agent?.display_name || agent?.id || "this agent";
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

    // Build agent lookup by id
    const agentById = new Map();
    agents.forEach((agent) => {
      agentById.set(agent.id, { agent });
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
      for (const { agent } of wsAgents) {
        const el = this.#buildAgentItem(agent, hubId);
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
      for (const { agent } of ungrouped) {
        container.appendChild(this.#buildAgentItem(agent, hubId));
      }
    }
  }

  // Private: render agents in a flat list
  #renderFlat(container, agents, hubId) {
    agents.forEach((agent) => {
      container.appendChild(this.#buildAgentItem(agent, hubId));
    });
  }

  // Private: build a workspace group header element
  #buildWorkspaceHeader(ws) {
    const clone = this.workspaceTemplateTarget.content.cloneNode(true);
    const root = clone.firstElementChild;

    // Stable ID for Idiomorph keying
    root.id = `workspace-${ws.id}`;
    root.dataset.workspaceId = ws.id;
    root
      .querySelectorAll("[data-workspace-id]")
      .forEach((el) => (el.dataset.workspaceId = ws.id));

    // Wire up toggle action
    root.dataset.action = "click->agent-list#toggleWorkspace";

    // Fill workspace fields
    root.querySelectorAll("[data-field]").forEach((el) => {
      const field = el.dataset.field;
      if (field === "workspace-title") {
        el.textContent = ws.name || ws.id;
      } else if (field === "workspace-count") {
        const count = Array.isArray(ws.agents) ? ws.agents.length : 0;
        el.textContent = count;
      }
    });

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
  #buildAgentItem(agent, hubId) {
    const clone = this.templateTarget.content.cloneNode(true);
    const root = clone.firstElementChild;
    const isAccessory = agent.session_type === "accessory";

    // Stable ID for Idiomorph keying
    root.id = `agent-${agent.id}`;

    // Mark session type for CSS targeting
    root.dataset.sessionType = agent.session_type || "agent";

    // Set data attributes
    this.#setDataAttributes(root, agent);

    // Fill fields
    const hasLabel = agent.label && agent.label.trim() !== "";

    root.querySelectorAll("[data-field]").forEach((el) => {
      const field = el.dataset.field;
      if (field === "name") {
        // Label is primary display name when present
        el.textContent = hasLabel
          ? agent.label
          : agent.display_name || agent.id;
        // Accessories get muted styling
        if (isAccessory) {
          el.classList.add("text-zinc-400");
          el.classList.remove("text-zinc-100");
        }
      } else if (field === "subtext") {
        // Bottom line: spawn info (target · branch · config) — always shown
        const parts = [];
        if (agent.target_name) parts.push(agent.target_name);
        if (agent.branch_name) parts.push(agent.branch_name);
        const configName = agent.agent_name || agent.profile_name;
        if (configName) parts.push(configName);
        if (isAccessory && parts.length === 0) parts.push("accessory");
        el.textContent = parts.join(" · ");
      } else if (field === "title-line") {
        // Line 2: title/task when different from primary name (optional)
        const titleParts = [];
        const title = agent.title?.trim();
        const primaryName = hasLabel ? agent.label : (agent.display_name || agent.id);
        if (title && title !== primaryName) {
          titleParts.push(title);
        }
        if (agent.task) titleParts.push(agent.task);
        if (titleParts.length > 0) {
          el.textContent = titleParts.join(" · ");
          el.classList.remove("hidden");
        } else {
          el.textContent = "";
          el.classList.add("hidden");
        }
      } else if (field === "id") {
        el.textContent = agent.id;
      } else if (agent[field] !== undefined) {
        el.textContent = agent[field];
      }
    });

    // Interpolate hrefs — use session_uuid for routing
    const sessionUuid = agent.session_uuid;
    root.querySelectorAll("[data-href]").forEach((el) => {
      el.href = el.dataset.href
        .replace("{hubId}", hubId)
        .replace("{sessionUuid}", sessionUuid);
      delete el.dataset.href;
    });

    // Accessories: hide the move-to-workspace button (they belong to their workspace)
    if (isAccessory) {
      root
        .querySelectorAll("[data-action*='moveAgentWorkspace']")
        .forEach((el) => el.classList.add("hidden"));
    }

    // Selection state
    if (agent.id === this.selectedIdValue) {
      root.dataset.selected = "true";
    }

    // Notification left border (via data attribute)
    if (agent.notification) {
      root.dataset.notification = "true";
    } else {
      delete root.dataset.notification;
    }

    this.#renderActivityIndicator(root, agent, isAccessory);

    return root;
  }

  // Private: render activity state for real agent sessions
  #renderActivityIndicator(root, agent, isAccessory) {
    const indicator = root.querySelector("[data-activity-indicator]");
    if (!indicator) return;

    if (isAccessory) {
      indicator.textContent = "";
      indicator.title = "";
      indicator.setAttribute("aria-hidden", "true");
      indicator.classList.add("hidden");
      indicator.classList.remove("text-emerald-300", "text-sky-500");
      return;
    }

    const isIdle = agent.is_idle !== false;
    indicator.textContent = isIdle ? "◌" : "✺";
    indicator.title = isIdle ? "Idle" : "Active";
    indicator.setAttribute("aria-label", isIdle ? "Idle" : "Active");
    indicator.classList.remove("hidden", "text-emerald-300", "text-sky-500");
    indicator.classList.add(isIdle ? "text-sky-500" : "text-emerald-300");
  }

  // Private: set data attributes on the cloned element
  #setDataAttributes(el, agent) {
    if (!el) return;

    // Set on root element
    el.dataset.agentId = agent.id;

    // Also set on any children that need it
    el.querySelectorAll("[data-agent-id]").forEach((child) => {
      child.dataset.agentId = agent.id;
    });
  }

  // Private: find workspace containing an agent ID
  #workspaceForAgent(agentId) {
    return this.workspacesValue.find((ws) =>
      Array.isArray(ws?.agents) ? ws.agents.includes(agentId) : false,
    );
  }

  // Private: derive selected agent from current URL path
  #syncSelectionFromUrl() {
    const match = window.location.pathname.match(
      /\/hubs\/[^/]+\/sessions\/([^/]+)/,
    );
    if (!match) {
      // Not on a session page — clear selection
      if (this.selectedIdValue) this.selectedIdValue = "";
      return;
    }

    const sessionUuid = match[1];
    const agent = this.agentsValue.find(
      (a) => a.session_uuid === sessionUuid,
    );
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
