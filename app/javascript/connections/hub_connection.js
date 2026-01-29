/**
 * HubConnection - Typed wrapper for hub control plane.
 *
 * Manages:
 *   - Handshake with CLI (browser identity verification)
 *   - Agent lifecycle (list, create, select, delete)
 *   - Worktree operations
 *   - Invite/share functionality
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
 *   - inviteBundle - { url }
 *
 * Usage:
 *   const hub = await ConnectionManager.acquire(HubConnection, hubId, { hubId });
 *   hub.on("agentList", (agents) => render(agents));
 *   hub.on("connected", () => hub.requestAgents());
 *   hub.requestAgents();
 */

import { Connection, ConnectionState } from "connections/connection";

const HANDSHAKE_TIMEOUT_MS = 8000;

// Hub-specific states (extends base states)
export const HubState = {
  ...ConnectionState,
  HANDSHAKE_SENT: "handshake_sent",
  HANDSHAKE_TIMEOUT: "handshake_timeout",
};

export class HubConnection extends Connection {
  constructor(key, options, manager) {
    super(key, options, manager);
    this.handshakeTimer = null;
    this.handshakeComplete = false;
  }

  // ========== Connection overrides ==========

  channelName() {
    return "HubChannel";
  }

  channelParams() {
    return {
      hub_id: this.getHubId(),
      browser_identity: this.identityKey,
    };
  }

  /**
   * Override initialize to add handshake step.
   */
  async initialize() {
    await super.initialize();

    // If base connection succeeded, send handshake
    if (this.state === ConnectionState.CONNECTED) {
      await this.#sendHandshake();
    }
  }

  destroy() {
    if (this.handshakeTimer) {
      clearTimeout(this.handshakeTimer);
      this.handshakeTimer = null;
    }
    super.destroy();
  }

  /**
   * Override isConnected to require handshake completion.
   */
  isConnected() {
    return super.isConnected() && this.handshakeComplete;
  }

  /**
   * Handle hub-specific messages.
   */
  handleMessage(message) {
    switch (message.type) {
      case "handshake_ack":
        this.#handleHandshakeAck(message);
        break;

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

      case "invite_bundle":
        this.emit("inviteBundle", message);
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
   * Request an invite bundle for sharing hub access.
   */
  requestInviteBundle() {
    return this.send("generate_invite");
  }

  /**
   * Manually trigger reconnect/re-handshake.
   */
  async reconnect() {
    if (this.handshakeTimer) {
      clearTimeout(this.handshakeTimer);
      this.handshakeTimer = null;
    }
    this.handshakeComplete = false;
    await this.#sendHandshake();
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
   */
  onConnected(callback) {
    // If already connected, fire immediately
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

  // ========== Private: Handshake ==========

  async #sendHandshake() {
    const prevState = this.state;
    this.state = HubState.HANDSHAKE_SENT;

    const stateInfo = {
      state: HubState.HANDSHAKE_SENT,
      prevState,
      error: null,
    };
    this.emit("stateChange", stateInfo);
    this.manager.notifySubscribers(this.key, stateInfo);

    const sent = await this.send("connected", {
      device_name: this.#getDeviceName(),
      timestamp: Date.now(),
    });

    if (!sent) {
      this.emit("error", {
        reason: "handshake_failed",
        message: "Failed to send handshake",
      });
      return;
    }

    // Start timeout
    this.handshakeTimer = setTimeout(() => {
      if (!this.handshakeComplete) {
        this.#handleHandshakeTimeout();
      }
    }, HANDSHAKE_TIMEOUT_MS);
  }

  #handleHandshakeAck(message) {
    if (this.handshakeTimer) {
      clearTimeout(this.handshakeTimer);
      this.handshakeTimer = null;
    }

    this.handshakeComplete = true;
    const prevState = this.state;
    this.state = ConnectionState.CONNECTED;

    const stateInfo = {
      state: ConnectionState.CONNECTED,
      prevState,
      error: null,
    };
    this.emit("stateChange", stateInfo);
    this.manager.notifySubscribers(this.key, stateInfo);
    this.emit("connected", this);
  }

  async #handleHandshakeTimeout() {
    console.warn("[HubConnection] Handshake timeout");

    // Check if CLI is online via HTTP
    try {
      const response = await fetch(`/hubs/${this.getHubId()}.json`, {
        credentials: "same-origin",
        headers: { Accept: "application/json" },
      });

      if (response.ok) {
        const status = await response.json();
        const isCliOnline =
          status.seconds_since_heartbeat !== null &&
          status.seconds_since_heartbeat < 30;

        if (isCliOnline) {
          this.emit("error", {
            reason: "session_invalid",
            message: "Session expired. Re-scan QR code from CLI (Ctrl+P).",
          });
        } else {
          this.emit("error", {
            reason: "handshake_timeout",
            message: "CLI not responding. Is botster-hub running?",
          });
        }
      } else {
        this.emit("error", {
          reason: "handshake_timeout",
          message: "CLI did not respond. Is botster-hub running?",
        });
      }
    } catch (error) {
      this.emit("error", {
        reason: "handshake_timeout",
        message: "CLI did not respond. Is botster-hub running?",
      });
    }
  }

  #getDeviceName() {
    const ua = navigator.userAgent;
    if (ua.includes("iPhone")) return "iPhone";
    if (ua.includes("iPad")) return "iPad";
    if (ua.includes("Android")) return "Android";
    if (ua.includes("Mac")) return "Mac Browser";
    if (ua.includes("Windows")) return "Windows Browser";
    if (ua.includes("Linux")) return "Linux Browser";
    return "Browser";
  }
}
