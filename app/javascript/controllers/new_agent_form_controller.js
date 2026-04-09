import { Controller } from "@hotwired/stimulus";
import { HubManager } from "connections";

/**
 * NewAgentFormController - Handles the two-step new agent form.
 *
 * Step 1: Select target and choose main/existing/new branch
 * Step 2: Select agent, optional workspace, optional initial prompt, submit
 */
export default class extends Controller {
  static targets = [
    "targetSection",
    "targetSelect",
    "targetPrompt",
    "worktreeOptions",
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
    this.spawnTargets = [];
    this.selectedTargetId = null;
    this.pendingSelection = null;
    this.unsubscribers = [];
    this._onExternalTargetSelection = (event) => {
      this.#applySelectedTarget(event.detail?.targetId || null);
    };

    document.addEventListener(
      "botster:new-session-target",
      this._onExternalTargetSelection,
    );

    HubManager.acquire(this.hubIdValue).then((hub) => {
      this.hub = hub;
      this.spawnTargets = hub.spawnTargets.current();
      this.workspaces = hub.openWorkspaces.current();
      hub.spawnTargets.load().catch(() => {});
      hub.openWorkspaces.load().catch(() => {});
      this.#renderTargetSelect();
      this.#renderWorkspaceSelect();
      this.#updateStep1Visibility();

      if (this.selectedTargetId) {
        this.worktrees = Array.isArray(hub.getWorktrees(this.selectedTargetId))
          ? hub.getWorktrees(this.selectedTargetId)
          : [];
        const config = hub.getAgentConfig(this.selectedTargetId);
        this.agents = Array.isArray(config.agents) ? config.agents : [];
        this.#renderWorktreeList();
        this.#renderAgentSelect();
        if (!this.hub.hasWorktrees(this.selectedTargetId)) {
          this.hub.ensureWorktrees(this.selectedTargetId);
        }
        this.hub.ensureAgentConfig(this.selectedTargetId, { force: true }).catch(() => {});
      }

      this.unsubscribers.push(
        this.hub.spawnTargets.onChange((targets) => {
          this.spawnTargets = Array.isArray(targets) ? targets : [];
          this.#renderTargetSelect();
          this.#updateStep1Visibility();
        }),
      );

      this.unsubscribers.push(
        this.hub.on("worktreeList", ({ targetId, worktrees }) => {
          if (targetId && this.selectedTargetId && targetId !== this.selectedTargetId) return;
          this.worktrees = Array.isArray(worktrees) ? worktrees : [];
          this.#renderWorktreeList();
        }),
      );

      this.unsubscribers.push(
        this.hub.on("agentConfig", ({ targetId, agents }) => {
          if (targetId && this.selectedTargetId && targetId !== this.selectedTargetId) return;
          this.agents = Array.isArray(agents) ? agents : [];
          this.#renderAgentSelect();
        }),
      );

      this.unsubscribers.push(
        this.hub.openWorkspaces.onChange((workspaces) => {
          this.workspaces = Array.isArray(workspaces) ? workspaces : [];
          this.#renderWorkspaceSelect();
        }),
      );
    });
  }

  disconnect() {
    this.unsubscribers?.forEach((unsub) => unsub());
    this.unsubscribers = null;

    if (this._onExternalTargetSelection) {
      document.removeEventListener(
        "botster:new-session-target",
        this._onExternalTargetSelection,
      );
      this._onExternalTargetSelection = null;
    }

    const hub = this.hub;
    this.hub = null;
    hub?.release();
  }

  selectTarget() {
    if (!this.hasTargetSelectTarget) return;
    this.#applySelectedTarget(this.targetSelectTarget.value || null);
  }

  selectWorktree(event) {
    if (!this.selectedTargetId) return;
    const path = event.currentTarget.dataset.path;
    const branch = event.currentTarget.dataset.branch;

    if (!path || !branch) return;

    this.pendingSelection = { type: "existing", path, branch };
    this.#goToStep2(branch);
  }

  selectMainBranch() {
    if (!this.selectedTargetId) return;
    this.pendingSelection = { type: "main" };
    this.#goToStep2("main branch");
  }

  selectNewBranch(event) {
    if (!this.selectedTargetId) return;
    if (event.type === "keydown") {
      event.preventDefault();
    }

    if (!this.hasNewBranchInputTarget) return;

    const value = this.newBranchInputTarget.value?.trim();
    if (!value) return;

    this.pendingSelection = { type: "new", issueOrBranch: value };
    this.#goToStep2(value);
  }

  goBackToStep1() {
    if (this.hasStep1Target && this.hasStep2Target) {
      this.step2Target.classList.add("hidden");
      this.step1Target.classList.remove("hidden");
    }

    if (this.hasPromptInputTarget) {
      this.promptInputTarget.value = "";
    }
  }

  submit() {
    if (!this.pendingSelection || !this.hub || !this.selectedTargetId) {
      console.warn("[new-agent-form] Cannot submit - no selection, target, or connection");
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
        target_id: this.selectedTargetId,
        workspace_id: workspace?.id || null,
        workspace_name: workspace?.name || null,
      });
    } else if (this.pendingSelection.type === "main") {
      this.hub.send("create_agent", {
        prompt: prompt || null,
        agent_name: agentName,
        target_id: this.selectedTargetId,
        workspace_id: workspace?.id || null,
        workspace_name: workspace?.name || null,
      });
    } else {
      this.hub.send("create_agent", {
        issue_or_branch: this.pendingSelection.issueOrBranch,
        prompt: prompt || null,
        agent_name: agentName,
        target_id: this.selectedTargetId,
        workspace_id: workspace?.id || null,
        workspace_name: workspace?.name || null,
      });
    }

    this.#resetForm();
  }

  refresh() {
    if (this.selectedTargetId) {
      this.hub?.ensureWorktrees(this.selectedTargetId, { force: true });
      this.hub?.ensureAgentConfig(this.selectedTargetId, { force: true }).catch(() => {});
    }
  }

  #selectedAgent() {
    if (!this.hasAgentSelectTarget) return null;
    return this.agentSelectTarget.value || null;
  }

  #selectedWorkspace() {
    if (!this.hasWorkspaceSelectTarget) return null;
    const workspaceId = this.workspaceSelectTarget.value || null;
    if (!workspaceId) return null;
    return (
      this.workspaces.find((workspace) => workspace?.id === workspaceId) || {
        id: workspaceId,
        name: null,
      }
    );
  }

  #goToStep2(label) {
    if (!this.selectedTargetId) return;

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

    this.selectedTargetId = null;
    if (this.hasTargetSelectTarget) {
      this.targetSelectTarget.value = "";
    }
    this.#updateStep1Visibility();

    if (this.hasPromptInputTarget) {
      this.promptInputTarget.value = "";
    }

    if (this.hasStep1Target && this.hasStep2Target) {
      this.step2Target.classList.add("hidden");
      this.step1Target.classList.remove("hidden");
    }
  }

  #applySelectedTarget(targetId) {
    this.selectedTargetId = targetId;
    this.pendingSelection = null;
    this.worktrees = targetId && this.hub
      ? this.hub.getWorktrees(targetId)
      : [];
    this.agents = targetId && this.hub
      ? this.hub.getAgentConfig(targetId).agents
      : [];

    if (this.hasTargetSelectTarget) {
      this.targetSelectTarget.value = targetId || "";
    }

    this.#renderWorktreeList();
    this.#renderAgentSelect();
    this.#updateStep1Visibility();

    if (!targetId || !this.hub) return;
    if (!this.hub.hasWorktrees(targetId)) {
      this.hub.ensureWorktrees(targetId);
    }
    this.hub.ensureAgentConfig(targetId, { force: true }).catch(() => {});
  }

  #renderTargetSelect() {
    if (!this.hasTargetSelectTarget || !this.hasTargetSectionTarget) return;

    const select = this.targetSelectTarget;
    select.innerHTML = "";

    const emptyOption = document.createElement("option");
    emptyOption.value = "";
    emptyOption.textContent = this.spawnTargets.length
      ? "Select a spawn target"
      : "No admitted spawn targets";
    select.appendChild(emptyOption);

    this.spawnTargets.forEach((target) => {
      const option = document.createElement("option");
      option.value = target.id;
      const branchSuffix = target.current_branch ? ` (${target.current_branch})` : "";
      option.textContent = `${target.name || target.path}${branchSuffix}`;
      select.appendChild(option);
    });

    if (
      this.selectedTargetId &&
      !this.spawnTargets.some((target) => target.id === this.selectedTargetId)
    ) {
      this.selectedTargetId = null;
    }

    this.targetSectionTarget.classList.remove("hidden");
    select.value = this.selectedTargetId || "";
    this.#updateTargetPrompt();
  }

  #updateStep1Visibility() {
    if (this.hasWorktreeOptionsTarget) {
      this.worktreeOptionsTarget.classList.toggle("hidden", !this.selectedTargetId);
    }
    this.#updateTargetPrompt();
  }

  #updateTargetPrompt() {
    if (!this.hasTargetPromptTarget) return;

    if (this.selectedTargetId) {
      this.targetPromptTarget.textContent =
        "Spawn target selected. Now choose main, an existing worktree, or a new branch.";
    } else if (this.spawnTargets.length === 0) {
      this.targetPromptTarget.textContent =
        "Add a spawn target in Device Settings before creating an agent.";
    } else {
      this.targetPromptTarget.textContent =
        "Choose a spawn target to unlock worktree and branch selection.";
    }
  }

  #renderAgentSelect() {
    if (!this.hasAgentSelectTarget) return;

    const select = this.agentSelectTarget;
    select.innerHTML = "";

    if (this.agents.length === 0) {
      if (this.hasAgentSectionTarget) {
        this.agentSectionTarget.classList.add("hidden");
      }
      if (this.hasNoConfigWarningTarget) {
        this.noConfigWarningTarget.classList.remove("hidden");
      }
      return;
    }

    if (this.hasNoConfigWarningTarget) {
      this.noConfigWarningTarget.classList.add("hidden");
    }

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
    const previousValue = select.value;
    select.innerHTML = "";

    const workspaces = this.workspaces.filter(
      (workspace) => workspace && typeof workspace === "object" && workspace.id,
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

    workspaces.forEach((workspace) => {
      const option = document.createElement("option");
      option.value = workspace.id;
      option.textContent = workspace.name || workspace.id;
      select.appendChild(option);
    });

    if (previousValue) select.value = previousValue;
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
