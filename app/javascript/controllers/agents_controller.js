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
    "selectedLabel", // Label showing selected agent (desktop terminal header)
    "emptyState", // Message when no agents
    "creatingState", // Loading indicator for agent creation
    "worktreeList", // Container for worktree list in modal
    "newBranchInput", // Input for new branch/issue number
    "step1", // Step 1: worktree selection
    "step2", // Step 2: prompt input
    "selectedWorktreeLabel", // Label showing selected worktree in step 2
    "promptInput", // Textarea for initial prompt
    "mobileAgentLabel", // Mobile header agent/hub label
    "mobileAgentInfo", // Mobile dropdown agent info section
    "mobileAgentName", // Mobile dropdown agent name
    "mobileDeleteBtn", // Mobile dropdown delete button
    "landingAgentList", // Agent list on hub landing page
    "noAgentsMessage", // No agents empty state on landing page
  ];

  static outlets = ["hub-connection", "terminal-connection"];

  static values = {
    sidebarListClass: { type: String, default: "sidebar-agents-list" },
    hubName: { type: String, default: "" },
    hubId: { type: String, default: "" },
  };

  // Private field for cached sidebar list elements (there can be multiple - mobile + desktop)
  #sidebarListElements = null;

  // Track pending selection to avoid race condition with CLI confirmation
  #pendingSelection = null;

  connect() {
    this.agents = [];
    this.worktrees = [];
    this.selectedAgentId = null;
    this.connection = null; // Set when connection is ready

    // Two-step modal state
    this.pendingSelection = null; // { type: 'existing' | 'new', path?, branch?, issueOrBranch? }

    // Set up event delegation for sidebar agent buttons (outside this controller's scope)
    this.#setupSidebarClickDelegation();
  }

  // Bound click handler stored as instance property for cleanup
  #boundSidebarClickHandler = null;

  // Private: Set up click delegation for sidebar agent buttons
  // Uses document-level delegation since sidebar is outside controller scope
  // and may be rendered before or after this controller connects
  #setupSidebarClickDelegation() {
    this.#boundSidebarClickHandler = (e) => {
      // Check if click is within a sidebar-agents-list
      const sidebar = e.target.closest(`.${this.sidebarListClassValue}`);
      if (!sidebar) return;

      const agentBtn = e.target.closest("[data-agent-button]");
      if (agentBtn && agentBtn.dataset.agentId) {
        e.preventDefault();
        e.stopPropagation();
        this.#handleAgentClick(agentBtn.dataset.agentId);
      }
    };

    document.addEventListener("click", this.#boundSidebarClickHandler);
  }

  disconnect() {
    // Remove document-level click delegation listener
    if (this.#boundSidebarClickHandler) {
      document.removeEventListener("click", this.#boundSidebarClickHandler);
      this.#boundSidebarClickHandler = null;
    }

    // Clear reference to external elements
    this.#sidebarListElements = null;
  }

  // Getter for sidebar list elements (outside this controller's scope)
  // Returns array of elements - there can be multiple (mobile + desktop sidebars)
  // Note: Always query fresh to avoid stale DOM references after re-renders
  get sidebarListElements() {
    return Array.from(
      document.querySelectorAll(`.${this.sidebarListClassValue}`),
    );
  }

  // Called by Stimulus when hub-connection outlet becomes available
  hubConnectionOutletConnected(outlet) {
    outlet.registerListener(this, {
      onConnected: (outlet) => this.handleConnected(outlet),
      onDisconnected: () => this.handleDisconnected(),
      onMessage: (message) => this.handleMessage(message),
      onError: (error) => this.handleError(error),
    });
  }

  // Called by Stimulus when hub-connection outlet is removed
  hubConnectionOutletDisconnected(outlet) {
    outlet.unregisterListener(this);
    this.connection = null;
  }

  // Handle connection established
  handleConnected(outlet) {
    this.connection = outlet;
    this.requestAgentList();
    this.requestWorktrees();

    // Check if we should auto-select an agent from URL
    const agentIndex = this.#getAgentIndexFromUrl();
    if (agentIndex !== null) {
      const ptyIndex = this.#getPtyIndexFromUrl();
      this.pendingAutoSelect = { agentIndex, ptyIndex: ptyIndex || 0 };
    }
  }

  // Private: Extract agent index from URL path
  #getAgentIndexFromUrl() {
    const match = window.location.pathname.match(/\/hubs\/\d+\/agents\/(\d+)/);
    return match ? parseInt(match[1], 10) : null;
  }

  // Private: Extract PTY index from URL query params
  #getPtyIndexFromUrl() {
    const params = new URLSearchParams(window.location.search);
    const pty = params.get("pty");
    return pty ? parseInt(pty, 10) : 0;
  }

  // Handle connection lost
  handleDisconnected() {
    this.connection = null;
    this.agents = [];
    this.worktrees = [];
    this.selectedAgentId = null;
    this.updateAgentList([]);
    this.updateSelectedLabel(null);
    this.updateMobileAgentUI(null);
    this.#pushAgentStateToTerminalView(null);
  }

  // Handle decrypted messages from CLI
  handleMessage(message) {
    switch (message.type) {
      case "agents":
      case "agent_list":
        this.hideCreatingState();
        this.updateAgentList(message.agents || []);

        // Auto-select agent from URL if pending
        if (
          this.pendingAutoSelect !== undefined &&
          this.pendingAutoSelect !== null
        ) {
          const agents = message.agents || [];
          if (this.pendingAutoSelect.agentIndex < agents.length) {
            const agent = agents[this.pendingAutoSelect.agentIndex];
            const ptyIndex = this.pendingAutoSelect.ptyIndex;
            this.selectedAgentId = agent.id;
            this.updateSelectedLabel(agent.name || agent.id);
            this.updateMobileAgentUI(agent.name || agent.id);
            this.updateAgentList(agents);
            this.#pendingSelection = agent.id;
            this.connection.selectAgent(agent.id);
            if (this.hasTerminalConnectionOutlet) {
              this.terminalConnectionOutlet.connectToPty(
                this.pendingAutoSelect.agentIndex,
                ptyIndex,
              );
            }
            this.#pushAgentStateToTerminalView(agent);
          }
          this.pendingAutoSelect = null;
        }
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

    // Update landing page agent list to show error
    if (this.hasLandingAgentListTarget) {
      const isQrScanNeeded =
        error.reason === "no_bundle" || error.reason === "session_invalid";
      const isCliNotResponding = error.reason === "handshake_timeout";

      if (isQrScanNeeded) {
        // Not paired - need to scan QR code
        this.landingAgentListTarget.innerHTML = `
          <div class="py-8 text-center">
            <svg class="size-10 text-amber-500 mx-auto mb-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
              <path stroke-linecap="round" stroke-linejoin="round" stroke-width="1.5" d="M12 9v3.75m-9.303 3.376c-.866 1.5.217 3.374 1.948 3.374h14.71c1.73 0 2.813-1.874 1.948-3.374L13.949 3.378c-.866-1.5-3.032-1.5-3.898 0L2.697 16.126zM12 15.75h.007v.008H12v-.008z" />
            </svg>
            <h3 class="text-base font-medium text-zinc-200 mb-2">Not Paired Yet</h3>
            <p class="text-sm text-zinc-400 max-w-xs mx-auto">
              ${
                error.reason === "session_invalid"
                  ? "Session expired. Press Ctrl+P in CLI and select 'Show Connection Code' to scan QR code."
                  : "Press Ctrl+P in CLI and select 'Show Connection Code' to scan QR code."
              }
            </p>
          </div>
        `;
      } else if (isCliNotResponding) {
        // Paired but CLI not responding - probably not running
        this.landingAgentListTarget.innerHTML = `
          <div class="py-8 text-center">
            <svg class="size-10 text-amber-500 mx-auto mb-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
              <path stroke-linecap="round" stroke-linejoin="round" stroke-width="1.5" d="M8.288 15.038a5.25 5.25 0 017.424 0M5.106 11.856c3.807-3.808 9.98-3.808 13.788 0M1.924 8.674c5.565-5.565 14.587-5.565 20.152 0M12.53 18.22l-.53.53-.53-.53a.75.75 0 011.06 0z" />
            </svg>
            <h3 class="text-base font-medium text-zinc-200 mb-2">CLI Not Responding</h3>
            <p class="text-sm text-zinc-400 max-w-xs mx-auto">
              Is botster-hub running? Start it with <code class="text-zinc-300 bg-zinc-800 px-1 rounded">botster-hub</code> in your terminal.
            </p>
          </div>
        `;
      } else {
        // Generic error
        this.landingAgentListTarget.innerHTML = `
          <div class="py-8 text-center">
            <svg class="size-10 text-red-500 mx-auto mb-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
              <path stroke-linecap="round" stroke-linejoin="round" stroke-width="1.5" d="M12 9v3.75m9-.75a9 9 0 11-18 0 9 9 0 0118 0zm-9 3.75h.008v.008H12v-.008z" />
            </svg>
            <h3 class="text-base font-medium text-zinc-200 mb-2">Connection Failed</h3>
            <p class="text-sm text-zinc-400 max-w-xs mx-auto">${this.escapeHtml(error.message || "Unable to connect to CLI")}</p>
          </div>
        `;
      }
    }
  }

  // Show creating state (loading indicator)
  showCreatingState(identifier, stage = null) {
    this.creatingIdentifier = identifier;
    this.creatingStage = stage;

    const stageInfo = this.getStageInfo(stage);

    if (this.hasCreatingStateTarget) {
      // Update text if element exists
      const label = this.creatingStateTarget.querySelector(
        "[data-creating-label]",
      );
      if (label) {
        label.textContent = stageInfo.message;
      }
      this.creatingStateTarget.classList.remove("hidden");
    } else {
      // Inject creating state into all sidebar lists
      this.sidebarListElements.forEach((listElement) => {
        const existingCreating = listElement.querySelector(
          "[data-creating-indicator]",
        );
        if (existingCreating) {
          // Update existing indicator
          const statusText = existingCreating.querySelector(
            "[data-creating-status]",
          );
          const progressBar = existingCreating.querySelector(
            "[data-progress-bar]",
          );
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
          creating.className =
            "px-2 py-2 bg-cyan-500/10 border border-cyan-500/20 rounded";
          creating.innerHTML = `
            <div class="flex items-center gap-2">
              <svg class="size-3 text-cyan-400 animate-spin shrink-0" fill="none" viewBox="0 0 24 24">
                <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
                <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"></path>
              </svg>
              <div class="flex-1 min-w-0">
                <div class="text-xs text-cyan-400 font-medium truncate" data-creating-status>${this.escapeHtml(stageInfo.message)}</div>
              </div>
            </div>
            <div class="mt-1.5 bg-zinc-800 rounded-full h-1 overflow-hidden">
              <div class="h-full bg-cyan-400 transition-all duration-500" data-progress-bar style="width: ${stageInfo.progress}%"></div>
            </div>
          `;
          listElement.prepend(creating);
        }
      });
    }

    // Also close dialog if open
    const dialog = document.getElementById("new-agent-modal");
    if (dialog?.open) {
      dialog.close();
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

    const lists = this.sidebarListElements;
    if (lists.length === 0) return;

    let foundIndicator = false;
    lists.forEach((listElement) => {
      const indicator = listElement.querySelector("[data-creating-indicator]");
      if (indicator) {
        foundIndicator = true;
        const statusText = indicator.querySelector("[data-creating-status]");
        const progressBar = indicator.querySelector("[data-progress-bar]");
        if (statusText) {
          statusText.textContent = message.message || stageInfo.message;
        }
        if (progressBar) {
          progressBar.style.width = `${stageInfo.progress}%`;
        }
      }
    });

    // Create indicator if it doesn't exist in any list
    if (!foundIndicator) {
      this.showCreatingState(message.identifier, message.stage);
    }
  }

  // Get stage display info
  getStageInfo(stage) {
    const stages = {
      creating_worktree: { message: "Creating git worktree...", progress: 25 },
      copying_config: {
        message: "Copying configuration files...",
        progress: 50,
      },
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

    // Also remove any injected indicator from all sidebar lists
    this.sidebarListElements.forEach((listElement) => {
      const indicator = listElement.querySelector("[data-creating-indicator]");
      if (indicator) {
        indicator.remove();
      }
    });
  }

  // Update the agent list UI (renders to all sidebar elements and landing page)
  updateAgentList(agents) {
    this.agents = agents;

    const lists = this.sidebarListElements;

    // Get hub ID for building links
    const hubId = this.hubIdValue || this.#getHubIdFromUrl();

    // Update each sidebar list (mobile + desktop)
    lists.forEach((listElement) => {
      // Clear existing list
      listElement.innerHTML = "";

      if (agents.length === 0) {
        if (this.hasEmptyStateTarget) {
          this.emptyStateTarget.classList.remove("hidden");
        }
        listElement.innerHTML = `
          <p class="px-2 py-4 text-center text-xs text-zinc-600">No agents running</p>
        `;
        return;
      }

      if (this.hasEmptyStateTarget) {
        this.emptyStateTarget.classList.add("hidden");
      }

      // Render agent list as links (for URL-based navigation)
      agents.forEach((agent, index) => {
        const isSelected = agent.id === this.selectedAgentId;
        const agentUrl = `/hubs/${hubId}/agents/${index}`;

        // Container with flex layout for agent link and delete button
        const item = document.createElement("div");
        item.className = `group flex items-center gap-1 rounded transition-colors ${
          isSelected ? "bg-primary-500/20" : "hover:bg-zinc-800/50"
        }`;

        // Agent link (main clickable area) - navigates to agent URL
        const agentLink = document.createElement("a");
        agentLink.href = agentUrl;
        agentLink.className = `flex-1 text-left px-2 py-1.5 min-w-0 ${
          isSelected
            ? "text-primary-400 font-medium"
            : "text-zinc-400 hover:text-zinc-200"
        }`;
        agentLink.innerHTML = `<span class="truncate font-mono text-xs block">${this.escapeHtml(agent.name || agent.id)}</span>`;
        // Store agent ID for reference
        agentLink.dataset.agentId = agent.id;
        agentLink.dataset.agentIndex = index;
        agentLink.dataset.agentButton = "";

        // Delete button (visible on hover)
        const deleteBtn = document.createElement("button");
        deleteBtn.type = "button";
        deleteBtn.className =
          "shrink-0 p-1.5 text-zinc-600 hover:text-red-400 opacity-0 group-hover:opacity-100 transition-opacity";
        deleteBtn.title = "Delete agent";
        deleteBtn.innerHTML = `<svg class="size-3.5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
          <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M19 7l-.867 12.142A2 2 0 0116.138 21H7.862a2 2 0 01-1.995-1.858L5 7m5 4v6m4-6v6m1-10V4a1 1 0 00-1-1h-4a1 1 0 00-1 1v3M4 7h16" />
        </svg>`;
        deleteBtn.dataset.agentId = agent.id;
        deleteBtn.addEventListener("click", (e) => this.deleteAgent(e));

        item.appendChild(agentLink);
        item.appendChild(deleteBtn);
        listElement.appendChild(item);
      });
    });

    // Also update landing page agent list if present
    this.#updateLandingAgentList(agents, hubId);
  }

  // Private: Update the landing page agent list
  #updateLandingAgentList(agents, hubId) {
    if (!this.hasLandingAgentListTarget) return;

    this.landingAgentListTarget.innerHTML = "";

    if (agents.length === 0) {
      // Show empty state message
      if (this.hasNoAgentsMessageTarget) {
        this.noAgentsMessageTarget.classList.remove("hidden");
      }
      this.landingAgentListTarget.innerHTML = `
        <div class="py-8 text-center">
          <svg class="size-8 text-zinc-700 mx-auto mb-3" fill="none" stroke="currentColor" viewBox="0 0 24 24">
            <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M9 3v2m6-2v2M9 19v2m6-2v2M5 9H3m2 6H3m18-6h-2m2 6h-2M7 19h10a2 2 0 002-2V7a2 2 0 00-2-2H7a2 2 0 00-2 2v10a2 2 0 002 2zM9 9h6v6H9V9z" />
          </svg>
          <p class="text-sm text-zinc-500">No agents running</p>
          <p class="text-xs text-zinc-600 mt-1">Create a new agent to get started</p>
        </div>
      `;
      return;
    }

    // Hide empty state message
    if (this.hasNoAgentsMessageTarget) {
      this.noAgentsMessageTarget.classList.add("hidden");
    }

    // Render agent cards as links
    agents.forEach((agent, index) => {
      const agentUrl = `/hubs/${hubId}/agents/${index}`;

      const item = document.createElement("a");
      item.href = agentUrl;
      item.className =
        "flex items-center gap-3 px-4 py-3 bg-zinc-800/50 hover:bg-zinc-800 border border-zinc-700/50 hover:border-zinc-700 rounded-lg transition-colors";
      item.innerHTML = `
        <div class="size-10 rounded-lg bg-zinc-700/50 flex items-center justify-center text-zinc-400">
          <svg class="size-5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
            <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M8 9l3 3-3 3m5 0h3M5 20h14a2 2 0 002-2V6a2 2 0 00-2-2H5a2 2 0 00-2 2v12a2 2 0 002 2z" />
          </svg>
        </div>
        <div class="flex-1 min-w-0">
          <div class="text-sm font-medium text-zinc-200 truncate font-mono">${this.escapeHtml(agent.name || agent.id)}</div>
          <div class="text-xs text-zinc-500">Agent ${index + 1}</div>
        </div>
        <svg class="size-5 text-zinc-600" fill="none" stroke="currentColor" viewBox="0 0 24 24">
          <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M9 5l7 7-7 7" />
        </svg>
      `;

      this.landingAgentListTarget.appendChild(item);
    });
  }

  // Private: Extract hub ID from URL
  #getHubIdFromUrl() {
    const match = window.location.pathname.match(/\/hubs\/(\d+)/);
    return match ? match[1] : "";
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
      item.className =
        "w-full text-left px-3 py-2 rounded-lg hover:bg-zinc-700 text-zinc-300 transition-colors";
      item.dataset.action = "agents#selectWorktree";
      item.dataset.worktreePath = worktree.path;
      item.dataset.worktreeBranch = worktree.branch;

      const issueLabel = worktree.issue_number
        ? `Issue #${worktree.issue_number}`
        : worktree.branch;

      item.innerHTML = `
        <div class="flex items-center gap-2">
          <svg class="w-4 h-4 text-emerald-400" fill="none" stroke="currentColor" viewBox="0 0 24 24">
            <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M3 7v10a2 2 0 002 2h14a2 2 0 002-2V9a2 2 0 00-2-2h-6l-2-2H5a2 2 0 00-2 2z"></path>
          </svg>
          <span class="font-mono text-sm">${this.escapeHtml(issueLabel)}</span>
        </div>
        <div class="text-xs text-zinc-500 mt-1 truncate">${this.escapeHtml(worktree.path)}</div>
      `;

      this.worktreeListTarget.appendChild(item);
    });
  }

  // Handle agent selection from CLI (e.g., auto-selection after agent creation)
  async handleAgentSelected(message) {
    this.selectedAgentId = message.id;
    const displayName = message.name || message.id;
    this.updateSelectedLabel(displayName);
    this.updateMobileAgentUI(displayName);
    this.updateAgentList(this.agents); // Re-render to show selection

    // Push agent state to the terminal-view element
    this.#pushAgentStateToTerminalView(message);

    // Only connect to PTY if this selection wasn't initiated by us
    // (avoids race condition when user clicks quickly between agents)
    if (this.#pendingSelection === message.id) {
      // This is a confirmation of our click - we already connected
      this.#pendingSelection = null;
    } else {
      // This is a selection from CLI (e.g., on connect, auto-select, or another client)
      // We need to connect to the correct PTY
      if (
        this.connection &&
        this.hasTerminalConnectionOutlet &&
        this.agents.length > 0
      ) {
        const agentIndex = this.agents.findIndex((a) => a.id === message.id);
        if (agentIndex >= 0) {
          await this.terminalConnectionOutlet.connectToPty(agentIndex, 0);
        }
      }
    }
  }

  // Update mobile header agent UI
  updateMobileAgentUI(agentName) {
    // Update the dropdown trigger label
    if (this.hasMobileAgentLabelTarget) {
      this.mobileAgentLabelTarget.textContent =
        agentName || this.hubNameValue || "No agent";
    }

    // Show/hide agent info section
    if (this.hasMobileAgentInfoTarget) {
      if (agentName) {
        this.mobileAgentInfoTarget.classList.remove("hidden");
      } else {
        this.mobileAgentInfoTarget.classList.add("hidden");
      }
    }

    // Update agent name in dropdown
    if (this.hasMobileAgentNameTarget) {
      this.mobileAgentNameTarget.textContent = agentName || "";
    }

    // Show/hide delete button
    if (this.hasMobileDeleteBtnTarget) {
      if (agentName && this.selectedAgentId) {
        this.mobileDeleteBtnTarget.classList.remove("hidden");
        this.mobileDeleteBtnTarget.classList.add("flex");
      } else {
        this.mobileDeleteBtnTarget.classList.add("hidden");
        this.mobileDeleteBtnTarget.classList.remove("flex");
      }
    }
  }

  // Handle agent closed
  handleAgentClosed(message) {
    if (this.selectedAgentId === message.agent_id) {
      this.selectedAgentId = null;
      this.updateSelectedLabel(null);
      this.#pushAgentStateToTerminalView(null);
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

  // Action: Select an agent (for elements within controller scope)
  selectAgent(event) {
    const agentId = event.currentTarget.dataset.agentId;
    this.#handleAgentClick(agentId);
  }

  // Private: Handle agent selection (used by both action and direct click)
  async #handleAgentClick(agentId) {
    if (!agentId || !this.connection) return;

    // Find agent index for channel switching
    const agentIndex = this.agents.findIndex((a) => a.id === agentId);
    if (agentIndex === -1) return;

    // Track that we initiated this selection (to avoid race condition in handleAgentSelected)
    this.#pendingSelection = agentId;

    // Send select_agent to CLI (for state tracking)
    this.connection.selectAgent(agentId);

    // Connect to the agent's PTY (sends connect_to_pty to CLI + subscribes browser-side)
    if (this.hasTerminalConnectionOutlet) {
      await this.terminalConnectionOutlet.connectToPty(agentIndex, 0);
    }

    // Navigate to agent page if not already there
    const targetUrl = `/hubs/${this.hubIdValue}/agents/${agentIndex}`;
    if (window.location.pathname !== targetUrl) {
      const { visit } = await import("@hotwired/turbo");
      visit(targetUrl);
    }
  }

  // Action: Select existing worktree - go to step 2 for prompt
  selectWorktree(event) {
    const path = event.currentTarget.dataset.worktreePath;
    const branch = event.currentTarget.dataset.worktreeBranch;

    if (!path || !branch) return;

    // Store selection and go to step 2
    this.pendingSelection = {
      type: "existing",
      path: path,
      branch: branch,
    };

    this.goToStep2(branch);
  }

  // Action: Select new branch/issue - go to step 2 for prompt
  selectNewBranch(event) {
    // Prevent form submission if triggered by Enter key
    if (event.type === "keydown") {
      event.preventDefault();
    }

    if (!this.hasNewBranchInputTarget) return;

    const value = this.newBranchInputTarget.value?.trim();
    if (!value) return;

    // Store selection and go to step 2
    this.pendingSelection = {
      type: "new",
      issueOrBranch: value,
    };

    this.goToStep2(value);
  }

  // Navigate to step 2 (prompt input)
  goToStep2(label) {
    if (this.hasStep1Target && this.hasStep2Target) {
      this.step1Target.classList.add("hidden");
      this.step2Target.classList.remove("hidden");
    }

    if (this.hasSelectedWorktreeLabelTarget) {
      this.selectedWorktreeLabelTarget.textContent = label;
    }

    // Focus the prompt input
    if (this.hasPromptInputTarget) {
      this.promptInputTarget.focus();
    }
  }

  // Action: Go back to step 1
  goBackToStep1() {
    if (this.hasStep1Target && this.hasStep2Target) {
      this.step2Target.classList.add("hidden");
      this.step1Target.classList.remove("hidden");
    }

    // Clear prompt but keep selection
    if (this.hasPromptInputTarget) {
      this.promptInputTarget.value = "";
    }
  }

  // Action: Submit agent with prompt
  submitAgent() {
    if (!this.pendingSelection) {
      console.warn("[Agents] No pending selection");
      return;
    }
    if (!this.connection) {
      console.warn("[Agents] No connection");
      return;
    }

    const prompt = this.hasPromptInputTarget
      ? this.promptInputTarget.value?.trim()
      : "";

    if (this.pendingSelection.type === "existing") {
      // Reopen existing worktree with optional prompt
      this.connection.send("reopen_worktree", {
        path: this.pendingSelection.path,
        branch: this.pendingSelection.branch,
        prompt: prompt || null,
      });
    } else {
      // Create new agent with optional prompt
      this.connection.send("create_agent", {
        issue_or_branch: this.pendingSelection.issueOrBranch,
        prompt: prompt || null,
      });
    }

    // Reset state - dialog closes via native command attribute on button
    this.resetModalState();
  }

  // Reset modal state
  resetModalState() {
    this.pendingSelection = null;

    if (this.hasNewBranchInputTarget) {
      this.newBranchInputTarget.value = "";
    }
    if (this.hasPromptInputTarget) {
      this.promptInputTarget.value = "";
    }

    // Reset to step 1
    if (this.hasStep1Target && this.hasStep2Target) {
      this.step2Target.classList.add("hidden");
      this.step1Target.classList.remove("hidden");
    }
  }

  // Action: Request agent list refresh
  requestAgentList() {
    this.connection?.requestAgents();
  }

  // Action: Request worktree list refresh
  requestWorktrees() {
    this.connection?.requestWorktrees();
  }

  // Action: Prepare for new agent creation (called alongside command="show-modal")
  createAgent() {
    // Reset modal state first
    this.resetModalState();

    // Refresh worktree list when opening modal
    this.requestWorktrees();
    // Update UI with current worktrees
    this.updateWorktreeList();
    // Dialog opens via native command="show-modal" attribute on button
  }

  // Action: Delete an agent with confirmation
  deleteAgent(event) {
    event.stopPropagation(); // Don't trigger agent selection
    const agentId = event.currentTarget.dataset.agentId;
    if (!agentId) return;

    const agent = this.agents.find((a) => a.id === agentId);
    const agentName = agent?.name || agent?.id || "this agent";

    // Show confirmation
    if (confirm(`Delete ${agentName}?\n\nThis will stop the agent process.`)) {
      this.#performDelete(agentId, false);
    }
  }

  // Action: Delete selected agent (for mobile dropdown)
  deleteSelectedAgent() {
    if (!this.selectedAgentId) return;

    const agent = this.agents.find((a) => a.id === this.selectedAgentId);
    const agentName = agent?.name || agent?.id || "this agent";

    if (confirm(`Delete ${agentName}?\n\nThis will stop the agent process.`)) {
      this.#performDelete(this.selectedAgentId, false);
    }
  }

  // Private: Perform the delete
  #performDelete(agentId, deleteWorktree = false) {
    if (!this.connection) return;
    this.connection.deleteAgent(agentId, deleteWorktree);

    // Clear selection if deleting selected agent
    if (this.selectedAgentId === agentId) {
      this.selectedAgentId = null;
      this.updateSelectedLabel(null);
    }
  }

  // Private: Push agent PTY/tunnel state to the terminal-view element.
  // Setting dataset values automatically triggers Stimulus valueChanged callbacks.
  #pushAgentStateToTerminalView(agentData) {
    const terminalView = document.querySelector(
      '[data-controller~="terminal-view"]',
    );
    if (!terminalView) return;

    terminalView.dataset.terminalViewHasServerPtyValue =
      agentData?.has_server_pty || false;
    terminalView.dataset.terminalViewTunnelConnectedValue =
      agentData?.tunnel_connected ||
      agentData?.tunnel_status === "connected" ||
      false;
    terminalView.dataset.terminalViewTunnelPortValue =
      agentData?.tunnel_port || 0;
  }

  // Helper: Escape HTML
  escapeHtml(text) {
    const div = document.createElement("div");
    div.textContent = text;
    return div.innerHTML;
  }
}
