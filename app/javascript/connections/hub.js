import { HubConnectionManager } from "connections/hub_connection_manager";
import { HubTransport } from "connections/hub_connection";

const EMPTY_CONFIG = Object.freeze({
  agents: [],
  accessories: [],
  workspaces: [],
});

export class Hub {
  constructor(hubId, options = {}, manager = null) {
    this.hubId = hubId;
    this.options = options;
    this.manager = manager;

    this.transport = null;
    this.subscribers = new Map();
    this.unsubscribers = [];

    this.agents = [];
    this.workspaces = [];
    this.openWorkspaces = [];
    this.spawnTargets = [];
    this.hubRecoveryState = null;
    this.agentConfigByTargetId = new Map();
    this.worktreesByTargetId = new Map();

    this.loaded = {
      agents: false,
      workspaces: false,
      openWorkspaces: false,
      spawnTargets: false,
      hubRecoveryState: false,
    };

    this.pending = new Map();
    this.destroyed = false;
  }

  async initialize() {
    if (this.transport) return;

    this.transport = await HubConnectionManager.acquire(HubTransport, this.hubId, {
      hubId: this.hubId,
      ...this.options,
    });

    this.#bindTransport();
    this.#hydrateFromTransport();
  }

  release() {
    this.manager?.release(this.hubId);
  }

  destroy() {
    if (this.destroyed) return;
    this.destroyed = true;

    this.unsubscribers.forEach((unsub) => unsub());
    this.unsubscribers = [];
    this.subscribers.clear();

    const transport = this.transport;
    this.transport = null;
    transport?.release();
  }

  on(event, callback) {
    if (!this.subscribers.has(event)) {
      this.subscribers.set(event, new Set());
    }
    this.subscribers.get(event).add(callback);
    return () => this.off(event, callback);
  }

  off(event, callback) {
    this.subscribers.get(event)?.delete(callback);
  }

  emit(event, data) {
    const callbacks = this.subscribers.get(event);
    if (!callbacks) return;

    for (const callback of callbacks) {
      try {
        callback(data);
      } catch (error) {
        console.error("[Hub] Event handler error:", error);
      }
    }
  }

  isConnected() {
    return this.transport?.isConnected() ?? false;
  }

  onConnected(callback) {
    if (this.isConnected()) callback(this);
    return this.on("connected", callback);
  }

  onDisconnected(callback) {
    return this.on("disconnected", callback);
  }

  onStateChange(callback) {
    if (this.transport) {
      callback({
        state: this.transport.state,
        prevState: null,
        error: this.transport.getError(),
      });
    }
    return this.on("stateChange", callback);
  }

  onAgentList(callback) {
    if (this.loaded.agents) callback(this.agents);
    return this.on("agentList", callback);
  }

  onWorkspaceList(callback) {
    if (this.loaded.workspaces) callback(this.workspaces);
    return this.on("workspaceList", callback);
  }

  onOpenWorkspaceList(callback) {
    if (this.loaded.openWorkspaces) callback(this.openWorkspaces);
    return this.on("openWorkspaceList", callback);
  }

  onWorktreeList(callback) {
    return this.on("worktreeList", callback);
  }

  onSpawnTargetList(callback) {
    if (this.loaded.spawnTargets) callback(this.spawnTargets);
    return this.on("spawnTargetList", callback);
  }

  getAgentConfig(targetId) {
    return this.agentConfigByTargetId.get(targetId || "__default__") || EMPTY_CONFIG;
  }

  hasAgentConfig(targetId) {
    return this.agentConfigByTargetId.has(targetId || "__default__");
  }

  getWorktrees(targetId) {
    return this.worktreesByTargetId.get(targetId || "__default__") || [];
  }

  hasWorktrees(targetId) {
    return this.worktreesByTargetId.has(targetId || "__default__");
  }

  ensureAgents({ force = false } = {}) {
    return this.#load("agents", force, () => this.transport?.requestAgents() ?? Promise.resolve());
  }

  ensureWorkspaces({ force = false } = {}) {
    return this.#load("workspaces", force, () => this.transport?.requestWorkspaces() ?? Promise.resolve());
  }

  ensureOpenWorkspaces({ force = false } = {}) {
    return this.#load(
      "openWorkspaces",
      force,
      () => this.transport?.requestOpenWorkspaces() ?? Promise.resolve(),
    );
  }

  ensureSpawnTargets({ force = false } = {}) {
    return this.#load("spawnTargets", force, () => this.transport?.requestSpawnTargets() ?? Promise.resolve());
  }

  ensureWorktrees(targetId, { force = false } = {}) {
    const key = `worktrees:${targetId || "__default__"}`;
    if (!targetId) return Promise.resolve([]);
    return this.#load(key, force, () => this.transport?.requestWorktrees(targetId) ?? Promise.resolve());
  }

  ensureAgentConfig(targetId, { force = false } = {}) {
    const key = `agentConfig:${targetId || "__default__"}`;
    if (!targetId) return Promise.resolve(EMPTY_CONFIG);
    return this.#load(key, force, () => this.transport?.requestAgentConfig(targetId) ?? Promise.resolve());
  }

  #load(key, force, loader) {
    const loaded = this.#isLoaded(key);
    if (!force && loaded) {
      return Promise.resolve();
    }

    if (!force && this.pending.has(key)) {
      return this.pending.get(key);
    }

    const request = Promise.resolve(loader()).finally(() => {
      this.pending.delete(key);
    });

    this.pending.set(key, request);
    return request;
  }

  #isLoaded(key) {
    if (key === "agents") return this.loaded.agents;
    if (key === "workspaces") return this.loaded.workspaces;
    if (key === "openWorkspaces") return this.loaded.openWorkspaces;
    if (key === "spawnTargets") return this.loaded.spawnTargets;
    if (key.startsWith("worktrees:")) {
      const targetId = key.slice("worktrees:".length);
      return this.worktreesByTargetId.has(targetId);
    }
    if (key.startsWith("agentConfig:")) {
      const targetId = key.slice("agentConfig:".length);
      return this.agentConfigByTargetId.has(targetId);
    }
    return false;
  }

  #bindTransport() {
    const passthroughEvents = [
      "agentCreated",
      "agentDeleted",
      "spawnTargetFeedback",
      "connectionCode",
      "hubReady",
      "sessionTypes",
      "message",
      "error",
      "browserStatusChange",
      "connectionModeChange",
    ];

    this.unsubscribers.push(
      this.transport.onAgentList((agents) => {
        this.agents = Array.isArray(agents) ? agents : [];
        this.loaded.agents = true;
        this.emit("agentList", this.agents);
      }),
      this.transport.onOpenWorkspaceList((workspaces) => {
        this.openWorkspaces = Array.isArray(workspaces) ? workspaces : [];
        this.loaded.openWorkspaces = true;
        this.emit("openWorkspaceList", this.openWorkspaces);
      }),
      this.transport.on("hubWorkspaceList", (workspaces) => {
        this.workspaces = Array.isArray(workspaces) ? workspaces : [];
        this.loaded.workspaces = true;
        this.emit("workspaceList", this.workspaces);
      }),
      this.transport.on("spawnTargetList", (targets) => {
        this.spawnTargets = Array.isArray(targets) ? targets : [];
        this.loaded.spawnTargets = true;
        this.#pruneTargetScopedCaches(this.spawnTargets);
        this.emit("spawnTargetList", this.spawnTargets);
      }),
      this.transport.on("worktreeList", (payload) => {
        const targetId = payload?.targetId || "__default__";
        const worktrees = Array.isArray(payload?.worktrees) ? payload.worktrees : [];
        this.worktreesByTargetId.set(targetId, worktrees);
        this.emit("worktreeList", {
          targetId: payload?.targetId || null,
          worktrees,
        });
      }),
      this.transport.on("agentConfig", (payload) => {
        const targetId = payload?.targetId || "__default__";
        const normalized = {
          targetId: payload?.targetId || null,
          agents: Array.isArray(payload?.agents) ? payload.agents : [],
          accessories: Array.isArray(payload?.accessories) ? payload.accessories : [],
          workspaces: Array.isArray(payload?.workspaces) ? payload.workspaces : [],
        };
        this.agentConfigByTargetId.set(targetId, normalized);
        this.emit("agentConfig", normalized);
      }),
      this.transport.on("hubRecoveryState", (payload) => {
        this.hubRecoveryState = payload || null;
        this.loaded.hubRecoveryState = true;
        this.emit("hubRecoveryState", this.hubRecoveryState);
      }),
      this.transport.onConnected(() => {
        this.emit("connected", this);
      }),
      this.transport.onDisconnected(() => {
        this.emit("disconnected", this);
      }),
      this.transport.onStateChange((stateInfo) => {
        this.emit("stateChange", stateInfo);
      }),
    );

    passthroughEvents.forEach((eventName) => {
      this.unsubscribers.push(
        this.transport.on(eventName, (payload) => {
          this.emit(eventName, payload);
        }),
      );
    });
  }

  #hydrateFromTransport() {
    if (this.transport.hasAgentListSnapshot()) {
      this.agents = this.transport.getAgents();
      this.loaded.agents = true;
    }

    if (this.transport.hasHubWorkspaceListSnapshot()) {
      this.workspaces = this.transport.getHubWorkspaces();
      this.loaded.workspaces = true;
    }

    if (this.transport.hasOpenWorkspaceListSnapshot()) {
      this.openWorkspaces = this.transport.getOpenWorkspaces();
      this.loaded.openWorkspaces = true;
    }

    if (this.transport.hasSpawnTargetListSnapshot()) {
      this.spawnTargets = this.transport.getSpawnTargets();
      this.loaded.spawnTargets = true;
    }

    if (this.transport.hasHubRecoveryStateSnapshot()) {
      this.hubRecoveryState = this.transport.getHubRecoveryState();
      this.loaded.hubRecoveryState = true;
    }
  }

  #pruneTargetScopedCaches(targets) {
    const targetIds = new Set(
      (Array.isArray(targets) ? targets : [])
        .map((target) => target?.id)
        .filter(Boolean),
    );

    for (const key of this.agentConfigByTargetId.keys()) {
      if (key !== "__default__" && !targetIds.has(key)) {
        this.agentConfigByTargetId.delete(key);
      }
    }

    for (const key of this.worktreesByTargetId.keys()) {
      if (key !== "__default__" && !targetIds.has(key)) {
        this.worktreesByTargetId.delete(key);
      }
    }
  }
}

const TRANSPORT_METHODS = [
  "requestAgents",
  "requestWorktrees",
  "requestWorkspaces",
  "requestOpenWorkspaces",
  "selectAgent",
  "deleteAgent",
  "clearNotification",
  "createAgent",
  "renameWorkspace",
  "moveAgentWorkspace",
  "createAccessory",
  "requestAgentConfig",
  "requestSpawnTargets",
  "addSpawnTarget",
  "removeSpawnTarget",
  "renameSpawnTarget",
  "addSession",
  "removeSession",
  "requestSessionTypes",
  "requestConnectionCode",
  "restartHub",
  "fsRequest",
  "readFile",
  "writeFile",
  "listDir",
  "browseHostDir",
  "statFile",
  "deleteFile",
  "mkDir",
  "rmDir",
  "renameFile",
  "templateRequest",
  "installTemplate",
  "uninstallTemplate",
  "listInstalledTemplates",
  "reloadPlugin",
  "loadPlugin",
  "send",
];

TRANSPORT_METHODS.forEach((methodName) => {
  Hub.prototype[methodName] = function(...args) {
    if (!this.transport) {
      throw new Error(`Hub transport is not ready for ${methodName}`);
    }
    return this.transport[methodName](...args);
  };
});
