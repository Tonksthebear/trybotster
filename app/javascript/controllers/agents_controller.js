import { Controller } from "@hotwired/stimulus";

/**
 * Agents Controller - Agent list and management
 *
 * This controller handles agent-related UI:
 * - Displays list of agents from CLI
 * - Handles agent selection
 * - Requests agent creation/closure
 *
 * Uses connection controller's registerListener API for reliable event handling.
 */
export default class extends Controller {
  static targets = [
    "list",           // Container for agent list
    "selectedLabel",  // Label showing selected agent
    "emptyState",     // Message when no agents
  ];

  static outlets = ["connection", "modal"];

  connect() {
    this.agents = [];
    this.selectedAgentId = null;
    this.connection = null; // Set when connection is ready
  }

  disconnect() {
    // Cleanup if needed
  }

  // Called by Stimulus when connection outlet becomes available
  connectionOutletConnected(outlet) {
    outlet.registerListener(this, {
      onConnected: (outlet) => this.handleConnected(outlet),
      onDisconnected: () => this.handleDisconnected(),
      onMessage: (message) => this.handleMessage(message),
      onError: (error) => this.handleError(error),
    });
  }

  // Called by Stimulus when connection outlet is removed
  connectionOutletDisconnected(outlet) {
    outlet.unregisterListener(this);
    this.connection = null;
  }

  // Handle connection established
  handleConnected(outlet) {
    // Request agent list when connected
    this.connection = outlet;
    this.requestAgentList();
  }

  // Handle connection lost
  handleDisconnected() {
    this.connection = null;
    this.agents = [];
    this.selectedAgentId = null;
    this.updateAgentList([]);
    this.updateSelectedLabel(null);
  }

  // Handle decrypted messages from CLI
  handleMessage(message) {
    switch (message.type) {
      case "agents":
      case "agent_list":
        this.updateAgentList(message.agents || []);
        break;

      case "agent_selected":
        this.handleAgentSelected(message);
        break;

      case "agent_created":
        this.requestAgentList();
        break;

      case "agent_closed":
        this.handleAgentClosed(message);
        break;
    }
  }

  // Handle connection errors
  handleError(error) {
    console.error("[Agents] Connection error:", error);
  }

  // Update the agent list UI
  updateAgentList(agents) {
    this.agents = agents;

    if (!this.hasListTarget) return;

    // Clear existing list
    this.listTarget.innerHTML = "";

    if (agents.length === 0) {
      if (this.hasEmptyStateTarget) {
        this.emptyStateTarget.classList.remove("hidden");
      }
      this.listTarget.innerHTML = `
        <div class="text-center py-8 text-zinc-500">
          <p>No agents running</p>
          <p class="text-sm mt-1">Create one to get started</p>
        </div>
      `;
      return;
    }

    if (this.hasEmptyStateTarget) {
      this.emptyStateTarget.classList.add("hidden");
    }

    // Render agent list
    agents.forEach((agent) => {
      const isSelected = agent.id === this.selectedAgentId;
      const item = document.createElement("button");
      item.className = `w-full text-left px-3 py-2 rounded-lg transition-colors ${
        isSelected
          ? "bg-emerald-500/20 text-emerald-400 border border-emerald-500/30"
          : "hover:bg-zinc-800 text-zinc-300"
      }`;
      item.dataset.action = "agents#selectAgent";
      item.dataset.agentId = agent.id;

      item.innerHTML = `
        <div class="flex items-center justify-between">
          <div class="flex items-center gap-2">
            <span class="w-2 h-2 rounded-full ${this.getStatusColor(agent.status)}"></span>
            <span class="font-mono text-sm truncate">${this.escapeHtml(agent.name || agent.id)}</span>
          </div>
          <span class="text-xs text-zinc-500">${agent.status || "unknown"}</span>
        </div>
        ${agent.issue ? `<div class="text-xs text-zinc-500 mt-1 truncate">${this.escapeHtml(agent.issue)}</div>` : ""}
      `;

      this.listTarget.appendChild(item);
    });
  }

  // Handle agent selection from CLI
  handleAgentSelected(message) {
    this.selectedAgentId = message.id;
    this.updateSelectedLabel(message.name || message.id);
    this.updateAgentList(this.agents); // Re-render to show selection
  }

  // Handle agent closed
  handleAgentClosed(message) {
    if (this.selectedAgentId === message.agent_id) {
      this.selectedAgentId = null;
      this.updateSelectedLabel(null);
    }
    this.requestAgentList();
  }

  // Update selected agent label
  updateSelectedLabel(name) {
    if (this.hasSelectedLabelTarget) {
      if (name) {
        this.selectedLabelTarget.textContent = name;
        this.selectedLabelTarget.classList.remove("text-zinc-500");
        this.selectedLabelTarget.classList.add("text-emerald-400");
      } else {
        this.selectedLabelTarget.textContent = "None selected";
        this.selectedLabelTarget.classList.remove("text-emerald-400");
        this.selectedLabelTarget.classList.add("text-zinc-500");
      }
    }
  }

  // Action: Select an agent
  selectAgent(event) {
    const agentId = event.currentTarget.dataset.agentId;
    if (!agentId || !this.connection) return;

    this.connection.selectAgent(agentId);
  }

  // Action: Request agent list refresh
  requestAgentList() {
    if (this.connection) {
      this.connection.requestAgents();
    }
  }

  // Action: Open the new agent modal
  createAgent() {
    if (this.hasModalOutlet) {
      this.modalOutlet.show();
    }
  }

  // Action: Submit new agent form
  submitNewAgent(event) {
    event.preventDefault();
    const form = event.currentTarget;
    const input = form.querySelector("input[name='issue_or_branch']");
    const value = input?.value?.trim();

    if (value && this.connection) {
      this.connection.send("create_agent", { issue_or_branch: value });
      input.value = "";
      if (this.hasModalOutlet) {
        this.modalOutlet.hide();
      }
    }
  }

  // Action: Close selected agent
  closeAgent() {
    if (!this.selectedAgentId) return;
    this.dispatch("close-requested", {
      detail: { agentId: this.selectedAgentId }
    });
  }

  // Helper: Get status indicator color
  getStatusColor(status) {
    switch (status) {
      case "running":
      case "active":
        return "bg-emerald-500";
      case "idle":
      case "waiting":
        return "bg-yellow-500";
      case "error":
      case "failed":
        return "bg-red-500";
      default:
        return "bg-zinc-500";
    }
  }

  // Helper: Escape HTML
  escapeHtml(text) {
    const div = document.createElement("div");
    div.textContent = text;
    return div.innerHTML;
  }
}
