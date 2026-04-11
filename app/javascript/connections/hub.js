import { HubConnectionManager } from "connections/hub_connection_manager";
import { HubTransport } from "connections/hub_connection";
import { HubCollection, HubResource, HubScopedResource } from "connections/hub_resource";
import {
  buildHubConnectionStatus,
  DEFAULT_HUB_CONNECTION_STATUS,
} from "connections/hub_connection_status";

const EMPTY_CONFIG = Object.freeze({
  agents: [],
  accessories: [],
  workspaces: [],
});

const DEFAULT_PREFETCH = Object.freeze([
  "agents",
  "openWorkspaces",
  "spawnTargets",
]);

function cloneConfig(value) {
  return {
    targetId: value?.targetId || null,
    agents: Array.isArray(value?.agents) ? value.agents : [],
    accessories: Array.isArray(value?.accessories) ? value.accessories : [],
    workspaces: Array.isArray(value?.workspaces) ? value.workspaces : [],
  };
}

export class HubSession {
  constructor(hubId, options = {}, manager = null) {
    this.hubId = hubId;
    this.options = options;
    this.manager = manager;

    this.transport = null;
    this.subscribers = new Map();
    this.unsubscribers = [];
    this.destroyed = false;

    this.agents = new HubCollection({
      load: () => this.#requestCollection({
        eventName: "agentList",
        request: () => this.transport?.requestAgents(),
        timeoutMs: 8000,
      }),
    });

    this.workspaces = new HubCollection({
      load: () => this.#requestCollection({
        eventName: "hubWorkspaceList",
        request: () => this.transport?.requestWorkspaces(),
      }),
    });

    this.openWorkspaces = new HubCollection({
      load: () => this.#requestCollection({
        eventName: "openWorkspaceList",
        request: () => this.transport?.requestOpenWorkspaces(),
      }),
    });

    this.spawnTargets = new HubCollection({
      load: () => this.#requestCollection({
        eventName: "spawnTargetList",
        request: () => this.transport?.requestSpawnTargets(),
      }),
    });

    this.recoveryState = new HubResource({
      initialValue: null,
    });

    this.connectionStatus = new HubResource({
      initialValue: DEFAULT_HUB_CONNECTION_STATUS,
      normalize: (value) => value || DEFAULT_HUB_CONNECTION_STATUS,
    });

    this.configs = new HubScopedResource({
      createEntry: (targetId) => new HubResource({
        initialValue: cloneConfig(EMPTY_CONFIG),
        normalize: cloneConfig,
        load: () => {
          if (!targetId) return Promise.resolve(cloneConfig(EMPTY_CONFIG));
          return this.#requestCollection({
            eventName: "agentConfig",
            request: () => this.transport?.requestAgentConfig(targetId),
            match: (payload) => (payload?.targetId || null) === targetId,
          });
        },
      }),
    });

    this.worktrees = new HubScopedResource({
      createEntry: (targetId) => new HubCollection({
        load: () => {
          if (!targetId) return Promise.resolve([]);
          return this.#requestCollection({
            eventName: "worktreeList",
            request: () => this.transport?.requestWorktrees(targetId),
            match: (payload) => (payload?.targetId || null) === targetId,
          });
        },
      }),
    });
  }

  async initialize() {
    return this.boot();
  }

  async boot() {
    if (this.transport) return this;

    this.transport = await HubConnectionManager.acquire(HubTransport, this.hubId, {
      hubId: this.hubId,
      ...this.options,
    });

    this.#bindTransport();
    this.#hydrateFromTransport();
    this.#schedulePrefetch();
    return this;
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
        console.error("[HubSession] Event handler error:", error);
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
    return this.agents.onChange(callback);
  }

  onConnectionStatusChange(callback) {
    return this.connectionStatus.onChange(callback);
  }

  onWorkspaceList(callback) {
    return this.workspaces.onChange(callback);
  }

  onOpenWorkspaceList(callback) {
    return this.openWorkspaces.onChange(callback);
  }

  onWorktreeList(callback) {
    return this.on("worktreeList", callback);
  }

  onSpawnTargetList(callback) {
    return this.spawnTargets.onChange(callback);
  }

  async prefetch(resources = DEFAULT_PREFETCH) {
    const loaders = [];

    for (const resource of resources) {
      switch (resource) {
        case "agents":
          loaders.push(this.agents.load().catch(() => {}));
          break;
        case "workspaces":
          loaders.push(this.workspaces.load().catch(() => {}));
          break;
        case "openWorkspaces":
          loaders.push(this.openWorkspaces.load().catch(() => {}));
          break;
        case "spawnTargets":
          loaders.push(this.spawnTargets.load().catch(() => {}));
          break;
      }
    }

    await Promise.all(loaders);
  }

  ensureAgents(options = {}) {
    return this.agents.load(options);
  }

  ensureWorkspaces(options = {}) {
    return this.workspaces.load(options);
  }

  ensureOpenWorkspaces(options = {}) {
    return this.openWorkspaces.load(options);
  }

  ensureSpawnTargets(options = {}) {
    return this.spawnTargets.load(options);
  }

  ensureWorktrees(targetId, options = {}) {
    if (!targetId) return Promise.resolve([]);
    return this.worktrees.load(targetId, options);
  }

  ensureAgentConfig(targetId, options = {}) {
    if (!targetId) return Promise.resolve(cloneConfig(EMPTY_CONFIG));
    return this.configs.load(targetId, options);
  }

  getAgentConfig(targetId) {
    return this.configs.current(targetId);
  }

  hasAgentConfig(targetId) {
    return this.configs.isLoaded(targetId);
  }

  getWorktrees(targetId) {
    return this.worktrees.current(targetId);
  }

  hasWorktrees(targetId) {
    return this.worktrees.isLoaded(targetId);
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
      "connectionModeChange",
    ];

    this.unsubscribers.push(
      this.transport.onAgentList((agents) => {
        const normalized = this.agents.set(agents);
        this.emit("agentList", normalized);
      }),
      this.transport.onOpenWorkspaceList((workspaces) => {
        const normalized = this.openWorkspaces.set(workspaces);
        this.emit("openWorkspaceList", normalized);
      }),
      this.transport.on("hubWorkspaceList", (workspaces) => {
        const normalized = this.workspaces.set(workspaces);
        this.emit("workspaceList", normalized);
      }),
      this.transport.on("spawnTargetList", (targets) => {
        const normalized = this.spawnTargets.set(targets);
        this.#pruneTargetScopedCaches(normalized);
        this.emit("spawnTargetList", normalized);
      }),
      this.transport.on("worktreeList", (payload) => {
        const targetId = payload?.targetId || null;
        const worktrees = Array.isArray(payload?.worktrees) ? payload.worktrees : [];
        this.worktrees.set(targetId, worktrees);
        this.emit("worktreeList", { targetId, worktrees });
      }),
      this.transport.on("agentConfig", (payload) => {
        const normalized = cloneConfig(payload);
        const targetId = normalized.targetId || null;
        this.configs.set(targetId, normalized);
        this.emit("agentConfig", normalized);
      }),
      this.transport.on("hubRecoveryState", (payload) => {
        this.recoveryState.set(payload || null);
        this.emit("hubRecoveryState", this.recoveryState.current());
      }),
      this.transport.onConnected(() => {
        this.#syncConnectionStatus();
        this.emit("connected", this);
      }),
      this.transport.onDisconnected(() => {
        this.#syncConnectionStatus();
        this.emit("disconnected", this);
      }),
      this.transport.onStateChange((stateInfo) => {
        this.#syncConnectionStatus();
        this.emit("stateChange", stateInfo);
      }),
      this.transport.on("browserSocketStateChange", () => {
        this.#syncConnectionStatus();
      }),
      this.transport.on("cliStatusChange", () => {
        this.#syncConnectionStatus();
      }),
      this.transport.on("connectionModeChange", () => {
        this.#syncConnectionStatus();
      }),
      this.transport.on("error", () => {
        this.#syncConnectionStatus();
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
    this.#syncConnectionStatus();

    if (this.transport.hasAgentListSnapshot()) {
      this.agents.set(this.transport.getAgents());
    }

    if (this.transport.hasHubWorkspaceListSnapshot()) {
      this.workspaces.set(this.transport.getHubWorkspaces());
    }

    if (this.transport.hasOpenWorkspaceListSnapshot()) {
      this.openWorkspaces.set(this.transport.getOpenWorkspaces());
    }

    if (this.transport.hasSpawnTargetListSnapshot()) {
      this.spawnTargets.set(this.transport.getSpawnTargets());
    }

    if (this.transport.hasHubRecoveryStateSnapshot()) {
      this.recoveryState.set(this.transport.getHubRecoveryState());
    }
  }

  #schedulePrefetch() {
    const prefetch = this.options.prefetch === false
      ? []
      : Array.isArray(this.options.prefetch)
        ? this.options.prefetch
        : DEFAULT_PREFETCH;

    if (prefetch.length === 0) return;

    queueMicrotask(() => {
      if (this.destroyed) return;
      this.prefetch(prefetch).catch((error) => {
        console.debug("[HubSession] Prefetch failed:", error?.message || error);
      });
    });
  }

  async #requestCollection({
    eventName,
    request,
    match = null,
    timeoutMs = 5000,
  }) {
    let unsubscribe = null;
    let timer = null;

    const resourceEvent = new Promise((resolve, reject) => {
      const cleanup = () => {
        if (timer) {
          clearTimeout(timer);
          timer = null;
        }
        unsubscribe?.();
      };

      timer = setTimeout(() => {
        cleanup();
        reject(new Error(`${eventName} timed out`));
      }, timeoutMs);

      unsubscribe = this.on(eventName, (payload) => {
        if (typeof match === "function" && !match(payload)) return;
        cleanup();
        resolve(payload);
      });
    });

    try {
      const requestResult = await Promise.resolve(request?.());
      if (requestResult === false) {
        throw new Error(`Failed to request ${eventName}`);
      }
    } catch (error) {
      if (timer) {
        clearTimeout(timer);
        timer = null;
      }
      unsubscribe?.();
      throw error;
    }

    return resourceEvent;
  }

  #pruneTargetScopedCaches(targets) {
    const targetIds = (Array.isArray(targets) ? targets : [])
      .map((target) => target?.id)
      .filter(Boolean);

    this.configs.retain(targetIds);
    this.worktrees.retain(targetIds);
  }

  #syncConnectionStatus() {
    this.connectionStatus.set(buildHubConnectionStatus(this.transport));
    this.emit("connectionStatusChange", this.connectionStatus.current());
  }
}

export const Hub = HubSession;

const TRANSPORT_METHODS = [
  "requestAgents",
  "requestWorktrees",
  "requestWorkspaces",
  "requestOpenWorkspaces",
  "selectAgent",
  "deleteAgent",
  "clearNotification",
  "togglePublicPreview",
  "toggleHostedPreview",
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
  HubSession.prototype[methodName] = function(...args) {
    if (!this.transport) {
      throw new Error(`Hub transport is not ready for ${methodName}`);
    }
    return this.transport[methodName](...args);
  };
});
