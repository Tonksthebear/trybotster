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
 *   - workspaceList - Array of workspace groups (from agent_list message)
 *   - worktreeList - Array of worktrees
 *   - agentCreated - New agent data
 *   - agentDeleted - { id }
 *   - connectionCode - { url, qr_ascii }
 *   - hubRecoveryState - { state, ... }
 *   - hubReady - { state: "ready", ... }
 *   - agentConfig - { agents, accessories, workspaces }
 *
 * Usage:
 *   const hub = await HubConnectionManager.acquire(HubConnection, hubId, { hubId });
 *   hub.on("agentList", (agents) => render(agents));
 *   hub.on("connected", () => hub.requestAgents());
 *   hub.requestAgents();
 */

import { HubRoute } from "connections/hub_route";

export class HubConnection extends HubRoute {
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
        const workspaces = Array.isArray(message.workspaces) ? message.workspaces : [];
        this.emit("agentList", agents);
        this.emit("workspaceList", workspaces);
        // Sync app badge with notification count from agent list
        this.#updateAppBadge(agents);
        break;
      }

      case "worktrees":
      case "worktree_list":
        // Handle Lua's empty table {} serializing as object instead of array
        this.emit("worktreeList", Array.isArray(message.worktrees) ? message.worktrees : []);
        break;

      case "workspace_list":
        this.emit("workspaceList", Array.isArray(message.workspaces) ? message.workspaces : []);
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

      case "hub_recovery_state":
        this.emit("hubRecoveryState", message);
        if (message.state === "ready") this.emit("hubReady", message);
        break;

      case "hub_ready":
        this.emit("hubReady", message);
        break;

      case "agent_config":
        this.emit("agentConfig", {
          agents: Array.isArray(message.agents) ? message.agents : [],
          accessories: Array.isArray(message.accessories) ? message.accessories : [],
          workspaces: Array.isArray(message.workspaces) ? message.workspaces : [],
        });
        break;

      case "session_types":
        this.emit("sessionTypes", {
          agentId: message.agent_id,
          sessionTypes: Array.isArray(message.session_types)
            ? message.session_types
            : [],
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
   * Request workspace list from CLI.
   */
  requestWorkspaces() {
    return this.send("list_workspaces");
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
   * Clear the notification flag on a session.
   * @param {string} sessionUuid - Session UUID
   */
  clearNotification(sessionUuid) {
    return this.send("clear_notification", { session_uuid: sessionUuid });
  }

  /**
   * Create a new agent.
   * @param {Object} options - Agent creation options
   */
  createAgent(options = {}) {
    return this.send("create_agent", options);
  }

  /**
   * Rename a workspace.
   * @param {string} workspaceId
   * @param {string} newName
   */
  renameWorkspace(workspaceId, newName) {
    return this.send("rename_workspace", {
      workspace_id: workspaceId,
      new_name: newName,
    });
  }

  /**
   * Move a live session to another workspace.
   * @param {string} agentId
   * @param {string|null} workspaceId
   * @param {string|null} workspaceName
   */
  moveAgentWorkspace(agentId, workspaceId = null, workspaceName = null) {
    return this.send("move_agent_workspace", {
      agent_id: agentId,
      workspace_id: workspaceId,
      workspace_name: workspaceName,
    });
  }

  /**
   * Request agent/accessory/workspace config from CLI.
   */
  requestAgentConfig() {
    return this.send("list_configs");
  }

  /**
   * Add a PTY session to a running agent.
   * @param {string} agentId - Agent key
   * @param {string} sessionType - Session type name (e.g., "shell", "server")
   */
  addSession(agentId, sessionType) {
    return this.send("add_session", {
      agent_id: agentId,
      session_type: sessionType,
    });
  }

  /**
   * Remove a session.
   * @param {string} sessionUuid - Session UUID to remove
   */
  removeSession(sessionUuid) {
    return this.send("remove_session", {
      session_uuid: sessionUuid,
    });
  }

  /**
   * Request available session types for an agent.
   * @param {string} agentId - Agent key
   */
  requestSessionTypes(agentId) {
    return this.send("list_session_types", { agent_id: agentId });
  }

  /**
   * Request connection code for sharing hub access.
   */
  requestConnectionCode() {
    return this.send("get_connection_code");
  }

  /**
   * Request a graceful Hub restart.
   *
   * The broker keeps PTY file descriptors alive for the reconnect window
   * (~120 s) so running agents survive the restart. After calling this the
   * Hub will disconnect; reconnect by relaunching botster within that window.
   */
  restartHub() {
    return this.send("restart_hub");
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

  reloadPlugin(pluginName) {
    return this.templateRequest("plugin:reload", { plugin_name: pluginName });
  }

  loadPlugin(pluginName) {
    return this.templateRequest("plugin:load", { plugin_name: pluginName });
  }

  // ========== Convenience event helpers ==========

  /**
   * Subscribe to agent list updates.
   */
  onAgentList(callback) {
    return this.on("agentList", callback);
  }

  /**
   * Subscribe to workspace list updates.
   * Workspaces arrive alongside agent_list messages.
   */
  onWorkspaceList(callback) {
    return this.on("workspaceList", callback);
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
