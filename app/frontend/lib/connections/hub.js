import { HubConnectionManager } from "connections/hub_connection_manager";
import { HubTransport } from "connections/hub_connection";
import { HubGateError, HUB_GATE_ERROR_CODES } from "connections/hub_gate_error";
import { HubResource, HubScopedResource } from "connections/hub_resource";
import {
  buildHubConnectionStatus,
  DEFAULT_HUB_CONNECTION_STATUS,
} from "connections/hub_connection_status";

const EMPTY_CONFIG = Object.freeze({
  agents: [],
  accessories: [],
  workspaces: [],
});

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
    return this;
  }

  release() {
    this.manager?.release(this.hubId);
  }

  destroy() {
    if (this.destroyed) return;
    this.destroyed = true;

    this.emit("destroyed", this);
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

  onConnectionStatusChange(callback) {
    return this.connectionStatus.onChange(callback);
  }

  ensureAgentConfig(targetId, options = {}) {
    if (!targetId) return Promise.resolve(cloneConfig(EMPTY_CONFIG));
    return this.configs.load(targetId, options);
  }

  /**
   * Run a hub operation after the session reaches the requested readiness.
   *
   * By default this waits for the route-owned transport to be connected.
   * Pass `requireTransport: false` only for rare object-only callers that can
   * safely inspect the HubSession without sending commands.
   */
  async perform(operation, options = {}) {
    const {
      timeoutMs = null,
      signal = null,
      requireTransport = true,
    } = options;

    if (typeof operation !== "function") {
      throw new TypeError("HubSession.perform requires an operation function");
    }

    if (signal?.aborted) {
      throw new HubGateError(
        HUB_GATE_ERROR_CODES.ABORTED,
        "Hub operation was aborted",
      );
    }

    if (this.destroyed) {
      throw new HubGateError(
        HUB_GATE_ERROR_CODES.DESTROYED,
        "Hub session has been destroyed",
      );
    }

    if (requireTransport && !this.transport) {
      throw new HubGateError(
        HUB_GATE_ERROR_CODES.MISSING_TRANSPORT,
        "Hub session transport is not available",
      );
    }

    if (requireTransport && !this.isConnected()) {
      await this.#waitUntilConnected({ timeoutMs, signal });
    }

    return this.#runOperation(operation, { timeoutMs, signal });
  }

  #bindTransport() {
    const passthroughEvents = [
      "spawnTargetFeedback",
      "hubReady",
      "sessionTypes",
      "push:status",
      "push:vapid_key",
      "push:sub_ack",
      "push:vapid_keys",
      "push:test_ack",
      "push:disable_ack",
      "message",
      "error",
      "connectionModeChange",
    ];

    this.unsubscribers.push(
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

    if (this.transport.hasHubRecoveryStateSnapshot()) {
      this.recoveryState.set(this.transport.getHubRecoveryState());
    }
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

  #syncConnectionStatus() {
    this.connectionStatus.set(buildHubConnectionStatus(this.transport));
    this.emit("connectionStatusChange", this.connectionStatus.current());
  }

  #waitUntilConnected({ timeoutMs, signal }) {
    if (this.destroyed) {
      return Promise.reject(new HubGateError(
        HUB_GATE_ERROR_CODES.DESTROYED,
        "Hub session has been destroyed",
      ));
    }

    if (!this.transport) {
      return Promise.reject(new HubGateError(
        HUB_GATE_ERROR_CODES.MISSING_TRANSPORT,
        "Hub session transport is not available",
      ));
    }

    if (this.isConnected()) return Promise.resolve(this);

    return new Promise((resolve, reject) => {
      let timer = null;
      let offConnected = null;
      let offDestroyed = null;

      const cleanup = () => {
        if (timer) {
          window.clearTimeout(timer);
          timer = null;
        }
        signal?.removeEventListener?.("abort", onAbort);
        offConnected?.();
        offDestroyed?.();
      };

      const finish = (callback, value) => {
        cleanup();
        callback(value);
      };

      const onAbort = () => finish(reject, new HubGateError(
        HUB_GATE_ERROR_CODES.ABORTED,
        "Hub operation was aborted",
      ));

      if (signal?.aborted) {
        onAbort();
        return;
      }

      offConnected = this.on("connected", () => {
        finish(resolve, this);
      });
      offDestroyed = this.on("destroyed", () => {
        finish(reject, new HubGateError(
          HUB_GATE_ERROR_CODES.DESTROYED,
          "Hub session was destroyed before the operation could run",
        ));
      });
      signal?.addEventListener?.("abort", onAbort, { once: true });

      if (timeoutMs != null) {
        timer = window.setTimeout(() => {
          finish(reject, new HubGateError(
            HUB_GATE_ERROR_CODES.TIMEOUT,
            "Timed out waiting for the hub transport to connect",
          ));
        }, timeoutMs);
      }
    });
  }

  #runOperation(operation, { timeoutMs, signal }) {
    if (signal?.aborted) {
      return Promise.reject(new HubGateError(
        HUB_GATE_ERROR_CODES.ABORTED,
        "Hub operation was aborted",
      ));
    }

    const operationPromise = Promise.resolve().then(() => operation(this));
    if (timeoutMs == null && !signal) return operationPromise;

    return new Promise((resolve, reject) => {
      let settled = false;
      let timer = null;

      const cleanup = () => {
        if (timer) {
          window.clearTimeout(timer);
          timer = null;
        }
        signal?.removeEventListener?.("abort", onAbort);
      };

      const finish = (callback, value) => {
        if (settled) return;
        settled = true;
        cleanup();
        callback(value);
      };

      const onAbort = () => finish(reject, new HubGateError(
        HUB_GATE_ERROR_CODES.ABORTED,
        "Hub operation was aborted",
      ));

      signal?.addEventListener?.("abort", onAbort, { once: true });

      if (timeoutMs != null) {
        timer = window.setTimeout(() => {
          finish(reject, new HubGateError(
            HUB_GATE_ERROR_CODES.TIMEOUT,
            "Timed out waiting for the hub operation to finish",
          ));
        }, timeoutMs);
      }

      operationPromise.then(
        (value) => finish(resolve, value),
        (error) => finish(reject, error),
      );
    });
  }
}

export const Hub = HubSession;

const TRANSPORT_METHODS = [
  "selectAgent",
  "deleteAgent",
  "clearNotification",
  "executeSessionAction",
  "createAgent",
  "renameWorkspace",
  "moveAgentWorkspace",
  "createAccessory",
  "requestAgentConfig",
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
  "refreshTemplates",
  "reloadPlugin",
  "loadPlugin",
  "sendCommand",
];

TRANSPORT_METHODS.forEach((methodName) => {
  HubSession.prototype[methodName] = function(...args) {
    if (!this.transport) {
      throw new Error(`Hub transport is not ready for ${methodName}`);
    }
    return this.transport[methodName](...args);
  };
});
