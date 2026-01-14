import { Controller } from "@hotwired/stimulus";

/**
 * Agents Controller - Agent list and management
 *
 * This controller handles agent-related UI:
 * - Displays list of agents from CLI
 * - Handles agent selection
 * - Shows available worktrees for fast agent creation
 * - Requests agent creation/closure
 *
 * Uses connection controller's registerListener API for reliable event handling.
 */
export default class extends Controller {
  static targets = [
    "list",           // Container for agent list
    "selectedLabel",  // Label showing selected agent
    "emptyState",     // Message when no agents
    "creatingState",  // Loading indicator for agent creation
    "worktreeList",   // Container for worktree list in modal
    "newBranchInput", // Input for new branch/issue number
  ];

  static outlets = ["connection", "modal"];

  connect() {
    this.agents = [];
    this.worktrees = [];
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
    this.worktrees = [];
    this.selectedAgentId = null;
    this.updateAgentList([]);
    this.updateSelectedLabel(null);
  }

  // Handle decrypted messages from CLI
  handleMessage(message) {
    switch (message.type) {
      case "agents":
      case "agent_list":
        this.hideCreatingState();
        this.updateAgentList(message.agents || []);
        break;

      case "worktrees":
        this.worktrees = message.worktrees || [];
        this.updateWorktreeList();
        break;

      case "agent_selected":
        this.handleAgentSelected(message);
        break;

      case "agent_creating":
        this.showCreatingState(message.identifier);
        break;

      case "agent_creating_progress":
        this.updateCreatingProgress(message);
        break;

      case "agent_created":
        this.hideCreatingState();
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

  // Show creating state (loading indicator)
  showCreatingState(identifier, stage = null) {
    this.creatingIdentifier = identifier;
    this.creatingStage = stage;

    const stageInfo = this.getStageInfo(stage);

    if (this.hasCreatingStateTarget) {
      // Update text if element exists
      const label = this.creatingStateTarget.querySelector("[data-creating-label]");
      if (label) {
        label.textContent = stageInfo.message;
      }
      this.creatingStateTarget.classList.remove("hidden");
    } else if (this.hasListTarget) {
      // Inject creating state into list if no dedicated target
      const existingCreating = this.listTarget.querySelector("[data-creating-indicator]");
      if (existingCreating) {
        // Update existing indicator
        const statusText = existingCreating.querySelector("[data-creating-status]");
        const progressBar = existingCreating.querySelector("[data-progress-bar]");
        if (statusText) {
          statusText.textContent = stageInfo.message;
        }
        if (progressBar) {
          progressBar.style.width = `${stageInfo.progress}%`;
        }
      } else {
        // Create new indicator
        const creating = document.createElement("div");
        creating.dataset.creatingIndicator = "true";
        creating.className = "px-3 py-3 bg-cyan-500/10 border border-cyan-500/20 rounded-lg";
        creating.innerHTML = `
          <div class="flex items-center gap-3">
            <svg class="w-4 h-4 text-cyan-400 animate-spin flex-shrink-0" fill="none" viewBox="0 0 24 24">
              <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
              <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"></path>
            </svg>
            <div class="flex-1 min-w-0">
              <div class="text-sm text-cyan-400 font-medium" data-creating-status>${this.escapeHtml(stageInfo.message)}</div>
              <div class="text-xs text-cyan-400/70 font-mono truncate">${this.escapeHtml(identifier)}</div>
            </div>
          </div>
          <div class="mt-2 bg-zinc-800 rounded-full h-1.5 overflow-hidden">
            <div class="h-full bg-cyan-400 transition-all duration-500" data-progress-bar style="width: ${stageInfo.progress}%"></div>
          </div>
        `;
        this.listTarget.prepend(creating);
      }
    }

    // Also hide modal if open
    if (this.hasModalOutlet) {
      this.modalOutlet.hide();
    }
  }

  // Update creating progress state
  updateCreatingProgress(message) {
    // Only update if same identifier
    if (message.identifier !== this.creatingIdentifier) {
      this.creatingIdentifier = message.identifier;
    }

    this.creatingStage = message.stage;

    const stageInfo = this.getStageInfo(message.stage);

    if (this.hasListTarget) {
      const indicator = this.listTarget.querySelector("[data-creating-indicator]");
      if (indicator) {
        const statusText = indicator.querySelector("[data-creating-status]");
        const progressBar = indicator.querySelector("[data-progress-bar]");
        if (statusText) {
          statusText.textContent = message.message || stageInfo.message;
        }
        if (progressBar) {
          progressBar.style.width = `${stageInfo.progress}%`;
        }
      } else {
        // Create indicator if it doesn't exist
        this.showCreatingState(message.identifier, message.stage);
      }
    }
  }

  // Get stage display info
  getStageInfo(stage) {
    const stages = {
      creating_worktree: { message: "Creating git worktree...", progress: 25 },
      copying_config: { message: "Copying configuration files...", progress: 50 },
      spawning_agent: { message: "Starting agent...", progress: 75 },
      ready: { message: "Agent ready", progress: 100 },
    };
    return stages[stage] || { message: "Creating agent...", progress: 10 };
  }

  // Hide creating state
  hideCreatingState() {
    this.creatingIdentifier = null;

    if (this.hasCreatingStateTarget) {
      this.creatingStateTarget.classList.add("hidden");
    }

    // Also remove any injected indicator
    if (this.hasListTarget) {
      const indicator = this.listTarget.querySelector("[data-creating-indicator]");
      if (indicator) {
        indicator.remove();
      }
    }
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

  // Update worktree list in modal
  updateWorktreeList() {
    if (!this.hasWorktreeListTarget) return;

    this.worktreeListTarget.innerHTML = "";

    if (this.worktrees.length === 0) {
      this.worktreeListTarget.innerHTML = `
        <div class="text-center py-4 text-zinc-500 text-sm">
          No existing worktrees
        </div>
      `;
      return;
    }

    // Render worktree list
    this.worktrees.forEach((worktree) => {
      const item = document.createElement("button");
      item.type = "button";
      item.className = "w-full text-left px-3 py-2 rounded-lg hover:bg-zinc-700 text-zinc-300 transition-colors";
      item.dataset.action = "agents#selectWorktree";
      item.dataset.worktreePath = worktree.path;
      item.dataset.worktreeBranch = worktree.branch;

      const issueLabel = worktree.issue_number
        ? `Issue #${worktree.issue_number}`
        : worktree.branch;

      item.innerHTML = `
        <div class="flex items-center justify-between">
          <div class="flex items-center gap-2">
            <svg class="w-4 h-4 text-emerald-400" fill="none" stroke="currentColor" viewBox="0 0 24 24">
              <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M3 7v10a2 2 0 002 2h14a2 2 0 002-2V9a2 2 0 00-2-2h-6l-2-2H5a2 2 0 00-2 2z"></path>
            </svg>
            <span class="font-mono text-sm">${this.escapeHtml(issueLabel)}</span>
          </div>
          <span class="text-xs text-emerald-400">instant</span>
        </div>
        <div class="text-xs text-zinc-500 mt-1 truncate">${this.escapeHtml(worktree.path)}</div>
      `;

      this.worktreeListTarget.appendChild(item);
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

  // Action: Select existing worktree (fast path - no git clone)
  selectWorktree(event) {
    const path = event.currentTarget.dataset.worktreePath;
    const branch = event.currentTarget.dataset.worktreeBranch;

    if (!path || !branch || !this.connection) return;

    // Use reopen_worktree command for instant agent creation
    this.connection.send("reopen_worktree", {
      path: path,
      branch: branch,
    });

    if (this.hasModalOutlet) {
      this.modalOutlet.hide();
    }
  }

  // Action: Request agent list refresh
  requestAgentList() {
    if (this.connection) {
      this.connection.requestAgents();
    }
  }

  // Action: Open the new agent modal
  createAgent() {
    // Refresh worktree list when opening modal
    if (this.connection) {
      this.connection.send("list_worktrees");
    }
    // Update UI with current worktrees
    this.updateWorktreeList();

    if (this.hasModalOutlet) {
      this.modalOutlet.show();
    }
  }

  // Action: Submit new agent form (creates new worktree - slower)
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
