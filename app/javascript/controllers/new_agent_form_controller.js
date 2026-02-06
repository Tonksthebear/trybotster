import { Controller } from "@hotwired/stimulus";
import { ConnectionManager } from "connections/connection_manager";
import { HubConnection } from "connections/hub_connection";

/**
 * NewAgentFormController - Handles the two-step new agent form.
 *
 * Step 1: Select existing worktree or enter new branch/issue
 * Step 2: Optional initial prompt
 *
 * Uses ConnectionManager to acquire connection for sending commands
 * and receiving worktree list updates.
 */
export default class extends Controller {
  static targets = [
    "worktreeList",
    "newBranchInput",
    "step1",
    "step2",
    "selectedWorktreeLabel",
    "promptInput",
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
    this.pendingSelection = null;
    this.unsubscribers = [];

    // Acquire connection to get worktree list and send commands
    ConnectionManager.acquire(HubConnection, this.hubIdValue, {
      hubId: this.hubIdValue,
    }).then(async (hub) => {
      this.hub = hub;

      this.unsubscribers.push(
        this.hub.on("worktreeList", (worktrees) => {
          this.worktrees = worktrees;
          this.#renderWorktreeList();
        }),
      );

      // Handle connection ready (initial or reconnection)
      // Use onConnected which fires immediately if already connected
      this.unsubscribers.push(
        this.hub.onConnected(() => {
          this.hub.requestWorktrees();
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

    if (this.pendingSelection.type === "existing") {
      this.hub.send("reopen_worktree", {
        path: this.pendingSelection.path,
        branch: this.pendingSelection.branch,
        prompt: prompt || null,
      });
    } else if (this.pendingSelection.type === "main") {
      this.hub.send("create_agent", {
        prompt: prompt || null,
      });
    } else {
      this.hub.send("create_agent", {
        issue_or_branch: this.pendingSelection.issueOrBranch,
        prompt: prompt || null,
      });
    }

    this.#resetForm();
  }

  // Action: Refresh worktree list
  refresh() {
    this.hub?.requestWorktrees();
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
