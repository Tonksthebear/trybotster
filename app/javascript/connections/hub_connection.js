/**
 * HubConnection - Typed wrapper for hub control plane.
 *
 * Manages:
 *   - Agent lifecycle (list, create, select, delete)
 *   - Worktree operations
 *   - Invite/share functionality
 *
 * Handshake is handled by base Connection class.
 *
 * Events:
 *   - connected - Handshake completed, E2E active
 *   - disconnected - Channel closed
 *   - stateChange - { state, prevState, error }
 *   - error - { reason, message }
 *   - agentList - Array of agents
 *   - worktreeList - Array of worktrees
 *   - agentCreated - New agent data
 *   - agentDeleted - { id }
 *   - connectionCode - { url, qr_ascii }
 *   - profileList - Array of profile names
 *
 * Usage:
 *   const hub = await ConnectionManager.acquire(HubConnection, hubId, { hubId });
 *   hub.on("agentList", (agents) => render(agents));
 *   hub.on("connected", () => hub.requestAgents());
 *   hub.requestAgents();
 */

import { Connection } from "connections/connection";

export class HubConnection extends Connection {
  // ========== Connection overrides ==========

  channelName() {
    return "hub";
  }

  /**
   * Compute semantic subscription ID.
   * Hub is singleton per connection, so just "hub".
   */
  computeSubscriptionId() {
    return "hub";
  }

  channelParams() {
    return {
      hub_id: this.getHubId(),
      browser_identity: this.browserIdentity,
    };
  }

  /**
   * Handle hub-specific messages.
   */
  handleMessage(message) {
    // Let base class handle handshake and health messages
    if (this.processMessage(message)) {
      return;
    }

    switch (message.type) {
      case "agents":
      case "agent_list": {
        // Handle Lua's empty table {} serializing as object instead of array
        const agents = Array.isArray(message.agents) ? message.agents : [];
        this.emit("agentList", agents);
        // Sync app badge with notification count from agent list
        this.#updateAppBadge(agents);
        break;
      }

      case "worktrees":
      case "worktree_list":
        // Handle Lua's empty table {} serializing as object instead of array
        this.emit("worktreeList", Array.isArray(message.worktrees) ? message.worktrees : []);
        break;

      case "agent_created":
        this.emit("agentCreated", message);
        break;

      case "agent_deleted":
        this.emit("agentDeleted", message);
        break;

      case "connection_code":
        this.emit("connectionCode", message);
        break;

      case "profiles":
        this.emit("profileList", {
          profiles: Array.isArray(message.profiles) ? message.profiles : [],
          sharedAgent: !!message.shared_agent,
        });
        break;

      default:
        // Route fs:* and template:* responses to one-shot listeners keyed by request_id
        if (message.type?.startsWith("fs:") && message.request_id) {
          this.emit(`fs:response:${message.request_id}`, message);
          return;
        }
        if (message.type === "template:response" && message.request_id) {
          this.emit(`template:response:${message.request_id}`, message);
          return;
        }
        // Emit as generic message for anything unhandled
        this.emit("message", message);
    }
  }

  // ========== Hub Commands ==========

  /**
   * Request list of agents from CLI.
   */
  requestAgents() {
    return this.send("list_agents");
  }

  /**
   * Request list of worktrees from CLI.
   */
  requestWorktrees() {
    return this.send("list_worktrees");
  }

  /**
   * Select an agent (focus in CLI).
   * @param {string} agentId
   */
  selectAgent(agentId) {
    return this.send("select_agent", { id: agentId });
  }

  /**
   * Delete an agent.
   * @param {string} agentId
   * @param {boolean} deleteWorktree - Also delete the git worktree
   */
  deleteAgent(agentId, deleteWorktree = false) {
    return this.send("delete_agent", {
      id: agentId,
      delete_worktree: deleteWorktree,
    });
  }

  /**
   * Create a new agent.
   * @param {Object} options - Agent creation options
   */
  createAgent(options = {}) {
    return this.send("create_agent", options);
  }

  /**
   * Request list of config profiles from CLI.
   */
  requestProfiles() {
    return this.send("list_profiles");
  }

  /**
   * Request connection code for sharing hub access.
   */
  requestConnectionCode() {
    return this.send("get_connection_code");
  }

  // ========== File System API ==========

  /**
   * Send a filesystem request and wait for the correlated response.
   * Uses one-shot event listeners keyed by request_id UUID.
   * @param {string} type - fs:read, fs:write, etc.
   * @param {Object} params - Request parameters
   * @param {number} timeout - Timeout in ms
   * @returns {Promise<Object>}
   */
  fsRequest(type, params = {}, timeout = 10000) {
    const requestId = crypto.randomUUID();
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        unsub();
        reject(new Error(`${type} timed out`));
      }, timeout);
      const unsub = this.on(`fs:response:${requestId}`, (response) => {
        clearTimeout(timer);
        unsub();
        response.ok ? resolve(response) : reject(new Error(response.error));
      });
      this.send(type, { ...params, request_id: requestId });
    });
  }

  readFile(path, scope) {
    return this.fsRequest("fs:read", { path, scope });
  }

  writeFile(path, content, scope) {
    return this.fsRequest("fs:write", { path, content, scope });
  }

  listDir(path = ".", scope) {
    return this.fsRequest("fs:list", { path, scope });
  }

  statFile(path, scope) {
    return this.fsRequest("fs:stat", { path, scope });
  }

  deleteFile(path, scope) {
    return this.fsRequest("fs:delete", { path, scope });
  }

  mkDir(path, scope) {
    return this.fsRequest("fs:mkdir", { path, scope });
  }

  rmDir(path, scope) {
    return this.fsRequest("fs:rmdir", { path, scope });
  }

  renameFile(fromPath, toPath, scope) {
    return this.fsRequest("fs:rename", { from_path: fromPath, to_path: toPath, scope });
  }

  // ========== Template API ==========

  /**
   * Send a template request and wait for the correlated response.
   * Same pattern as fsRequest but for template:* commands.
   */
  templateRequest(type, params = {}, timeout = 15000) {
    const requestId = crypto.randomUUID();
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        unsub();
        reject(new Error(`${type} timed out`));
      }, timeout);
      const unsub = this.on(`template:response:${requestId}`, (response) => {
        clearTimeout(timer);
        unsub();
        response.ok ? resolve(response) : reject(new Error(response.error));
      });
      this.send(type, { ...params, request_id: requestId });
    });
  }

  installTemplate(dest, content, scope) {
    return this.templateRequest("template:install", { dest, content, scope });
  }

  uninstallTemplate(dest, scope) {
    return this.templateRequest("template:uninstall", { dest, scope });
  }

  listInstalledTemplates() {
    return this.templateRequest("template:list");
  }

  // ========== Convenience event helpers ==========

  /**
   * Subscribe to agent list updates.
   */
  onAgentList(callback) {
    return this.on("agentList", callback);
  }

  /**
   * Subscribe to worktree list updates.
   */
  onWorktreeList(callback) {
    return this.on("worktreeList", callback);
  }

  /**
   * Subscribe to connection established (handshake complete).
   * Fires immediately if already connected.
   */
  onConnected(callback) {
    if (this.isConnected()) {
      callback(this);
    }
    return this.on("connected", callback);
  }

  /**
   * Subscribe to disconnection.
   */
  onDisconnected(callback) {
    return this.on("disconnected", callback);
  }

  /**
   * Subscribe to state changes.
   */
  onStateChange(callback) {
    // Fire immediately with current state
    callback({ state: this.state, prevState: null, error: this.errorReason });
    return this.on("stateChange", callback);
  }

  /**
   * Subscribe to errors.
   */
  onError(callback) {
    return this.on("error", callback);
  }

  // ========== Private ==========

  /**
   * Update the PWA app badge to reflect notification count from agent list.
   * Uses the Badging API (navigator.setAppBadge / clearAppBadge).
   */
  #updateAppBadge(agents) {
    if (!navigator.setAppBadge) return;
    const count = agents.filter((a) => a.notification).length;
    if (count > 0) {
      navigator.setAppBadge(count);
    } else if (navigator.clearAppBadge) {
      navigator.clearAppBadge();
    }
  }
}
