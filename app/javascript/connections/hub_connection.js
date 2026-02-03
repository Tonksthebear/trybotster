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
 *   - connectionCode - { url, qr_png }
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
    return "HubChannel";
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
      case "agent_list":
        this.emit("agentList", message.agents || []);
        break;

      case "worktrees":
      case "worktree_list":
        this.emit("worktreeList", message.worktrees || []);
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

      default:
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
   * Request connection code for sharing hub access.
   */
  requestConnectionCode() {
    return this.send("get_connection_code");
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
}
