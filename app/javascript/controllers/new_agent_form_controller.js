import { Controller } from "@hotwired/stimulus";
import { HubConnectionManager } from "connections/hub_connection_manager";
import { HubConnection } from "connections/hub_connection";

/**
 * NewAgentFormController - Handles the two-step new agent form.
 *
 * Step 1: Select existing worktree or enter new branch/issue
 * Step 2: Select agent, optional workspace, optional initial prompt, submit
 *
 * Uses HubConnectionManager to acquire connection for sending commands
 * and receiving worktree list and agent config updates.
 */
export default class extends Controller {
  static targets = [
    "worktreeList",
    "newBranchInput",
    "step1",
    "step2",
    "selectedWorktreeLabel",
    "promptInput",
    "agentSelect",
    "agentSection",
    "workspaceSelect",
    "workspaceSection",
    "noConfigWarning",
  ];

  static values = {
    hubId: String,
  };

  connect() {
    if (!this.hubIdValue) {
      console.error("[new-agent-form] Missing hubId value");
      return;
    }

    this.worktrees = [];
    this.agents = [];
    this.workspaces = [];
    this.pendingSelection = null;
    this.unsubscribers = [];

    // Acquire connection to get worktree list and send commands
    HubConnectionManager.acquire(HubConnection, this.hubIdValue, {
      hubId: this.hubIdValue,
    }).then(async (hub) => {
      this.hub = hub;

      this.unsubscribers.push(
        this.hub.on("worktreeList", (worktrees) => {
          this.worktrees = worktrees;
          this.#renderWorktreeList();
        }),
      );

      this.unsubscribers.push(
        this.hub.on("agentConfig", ({ agents, workspaces }) => {
          this.agents = agents;
          this.workspaces = workspaces;
          this.#renderAgentSelect();
          this.#renderWorkspaceSelect();
        }),
      );

      // Handle connection ready (initial or reconnection)
      // Use onConnected which fires immediately if already connected
      this.unsubscribers.push(
        this.hub.onConnected(() => {
          this.hub.requestWorktrees();
          this.hub.requestAgentConfig();
        }),
      );

      // No explicit subscribe() — health events drive the WebRTC lifecycle.
      // onConnected above fires when handshake completes.
    });
  }

  disconnect() {
    // Clean up event subscriptions
    this.unsubscribers?.forEach((unsub) => unsub());
    this.unsubscribers = null;

    const hub = this.hub;
    this.hub = null;
    hub?.release();
  }

  // Action: Select an existing worktree
  selectWorktree(event) {
    const path = event.currentTarget.dataset.path;
    const branch = event.currentTarget.dataset.branch;

    if (!path || !branch) return;

    this.pendingSelection = { type: "existing", path, branch };
    this.#goToStep2(branch);
  }

  // Action: Spawn agent on main branch (no worktree)
  selectMainBranch() {
    this.pendingSelection = { type: "main" };
    this.#goToStep2("main branch");
  }

  // Action: Create new branch/issue
  selectNewBranch(event) {
    if (event.type === "keydown") {
      event.preventDefault();
    }

    if (!this.hasNewBranchInputTarget) return;

    const value = this.newBranchInputTarget.value?.trim();
    if (!value) return;

    this.pendingSelection = { type: "new", issueOrBranch: value };
    this.#goToStep2(value);
  }

  // Action: Go back to step 1
  goBackToStep1() {
    if (this.hasStep1Target && this.hasStep2Target) {
      this.step2Target.classList.add("hidden");
      this.step1Target.classList.remove("hidden");
    }

    if (this.hasPromptInputTarget) {
      this.promptInputTarget.value = "";
    }
  }

  // Action: Submit the form
  submit() {
    if (!this.pendingSelection || !this.hub) {
      console.warn("[new-agent-form] Cannot submit - no selection or connection");
      return;
    }

    const prompt = this.hasPromptInputTarget
      ? this.promptInputTarget.value?.trim()
      : "";

    const agentName = this.#selectedAgent();
    const workspace = this.#selectedWorkspace();

    if (this.pendingSelection.type === "existing") {
      this.hub.send("reopen_worktree", {
        path: this.pendingSelection.path,
        branch: this.pendingSelection.branch,
        prompt: prompt || null,
        agent_name: agentName,
        workspace_config: workspace || null,
      });
    } else if (this.pendingSelection.type === "main") {
      this.hub.send("create_agent", {
        prompt: prompt || null,
        agent_name: agentName,
        workspace_config: workspace || null,
      });
    } else {
      this.hub.send("create_agent", {
        issue_or_branch: this.pendingSelection.issueOrBranch,
        prompt: prompt || null,
        agent_name: agentName,
        workspace_config: workspace || null,
      });
    }

    this.#resetForm();
  }

  // Action: Refresh worktree list
  refresh() {
    this.hub?.requestWorktrees();
  }

  #selectedAgent() {
    if (!this.hasAgentSelectTarget) return null;
    return this.agentSelectTarget.value || null;
  }

  #selectedWorkspace() {
    if (!this.hasWorkspaceSelectTarget) return null;
    return this.workspaceSelectTarget.value || null;
  }

  #goToStep2(label) {
    if (this.hasStep1Target && this.hasStep2Target) {
      this.step1Target.classList.add("hidden");
      this.step2Target.classList.remove("hidden");
    }

    if (this.hasSelectedWorktreeLabelTarget) {
      this.selectedWorktreeLabelTarget.textContent = label;
    }

    if (this.hasPromptInputTarget) {
      this.promptInputTarget.focus();
    }
  }

  #resetForm() {
    this.pendingSelection = null;

    if (this.hasNewBranchInputTarget) {
      this.newBranchInputTarget.value = "";
    }

    if (this.hasPromptInputTarget) {
      this.promptInputTarget.value = "";
    }

    if (this.hasStep1Target && this.hasStep2Target) {
      this.step2Target.classList.add("hidden");
      this.step1Target.classList.remove("hidden");
    }
  }

  #renderAgentSelect() {
    if (!this.hasAgentSelectTarget) return;

    const select = this.agentSelectTarget;
    select.innerHTML = "";

    if (this.agents.length === 0) {
      // No agents configured — hide the section
      if (this.hasAgentSectionTarget) {
        this.agentSectionTarget.classList.add("hidden");
      }
      // Show warning that no agent config exists
      if (this.hasNoConfigWarningTarget) {
        this.noConfigWarningTarget.classList.remove("hidden");
      }
      return;
    }

    // Has agents — hide warning
    if (this.hasNoConfigWarningTarget) {
      this.noConfigWarningTarget.classList.add("hidden");
    }

    // Show the section
    if (this.hasAgentSectionTarget) {
      this.agentSectionTarget.classList.remove("hidden");
    }

    this.agents.forEach((name) => {
      const option = document.createElement("option");
      option.value = name;
      option.textContent = name.charAt(0).toUpperCase() + name.slice(1);
      select.appendChild(option);
    });
  }

  #renderWorkspaceSelect() {
    if (!this.hasWorkspaceSelectTarget || !this.hasWorkspaceSectionTarget) return;

    const select = this.workspaceSelectTarget;
    select.innerHTML = "";

    if (this.workspaces.length === 0) {
      this.workspaceSectionTarget.classList.add("hidden");
      return;
    }

    this.workspaceSectionTarget.classList.remove("hidden");

    // Add empty option for "no workspace"
    const emptyOption = document.createElement("option");
    emptyOption.value = "";
    emptyOption.textContent = "None";
    select.appendChild(emptyOption);

    this.workspaces.forEach((name) => {
      const option = document.createElement("option");
      option.value = name;
      option.textContent = name.charAt(0).toUpperCase() + name.slice(1);
      select.appendChild(option);
    });
  }

  #renderWorktreeList() {
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

    this.worktrees.forEach((worktree) => {
      const item = document.createElement("button");
      item.type = "button";
      item.className =
        "w-full text-left px-3 py-2 rounded-lg hover:bg-zinc-700 text-zinc-300 transition-colors";
      item.dataset.action = "new-agent-form#selectWorktree";
      item.dataset.path = worktree.path;
      item.dataset.branch = worktree.branch;

      const label = worktree.issue_number
        ? `Issue #${worktree.issue_number}`
        : worktree.branch;

      item.innerHTML = `
        <div class="flex items-center gap-2">
          <svg class="w-4 h-4 text-emerald-400" fill="none" stroke="currentColor" viewBox="0 0 24 24">
            <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M3 7v10a2 2 0 002 2h14a2 2 0 002-2V9a2 2 0 00-2-2h-6l-2-2H5a2 2 0 00-2 2z"></path>
          </svg>
          <span class="font-mono text-sm">${this.#escapeHtml(label)}</span>
        </div>
        <div class="text-xs text-zinc-500 mt-1 truncate">${this.#escapeHtml(worktree.path)}</div>
      `;

      this.worktreeListTarget.appendChild(item);
    });
  }

  #escapeHtml(text) {
    const div = document.createElement("div");
    div.textContent = text;
    return div.innerHTML;
  }
}
