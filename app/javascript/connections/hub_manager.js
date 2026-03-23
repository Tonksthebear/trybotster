import { Hub } from "connections/hub";

class HubManagerSingleton {
  constructor() {
    this.IDLE_DESTROY_DELAY_MS = 5000;
    this.hubs = new Map();
    this.pendingCreation = new Map();
  }

  async acquire(hubId, options = {}) {
    let entry = this.hubs.get(hubId);
    if (entry) {
      entry.hub.options = { ...entry.hub.options, ...options };
      if (entry.idleDestroyTimer) {
        clearTimeout(entry.idleDestroyTimer);
        entry.idleDestroyTimer = null;
      }
      entry.refCount += 1;
      return entry.hub;
    }

    const pending = this.pendingCreation.get(hubId);
    if (pending) {
      const hub = await pending;
      entry = this.hubs.get(hubId);
      if (entry) {
        entry.refCount += 1;
        return entry.hub;
      }
      return hub;
    }

    const creationPromise = this.#createHub(hubId, options);
    this.pendingCreation.set(hubId, creationPromise);

    try {
      return await creationPromise;
    } finally {
      this.pendingCreation.delete(hubId);
    }
  }

  release(hubId) {
    const entry = this.hubs.get(hubId);
    if (!entry) return;

    entry.refCount -= 1;
    if (entry.refCount > 0 || entry.idleDestroyTimer) return;

    entry.idleDestroyTimer = setTimeout(() => {
      const current = this.hubs.get(hubId);
      if (!current || current !== entry) return;
      if (current.refCount <= 0) {
        this.destroy(hubId);
      }
    }, this.IDLE_DESTROY_DELAY_MS);
  }

  destroy(hubId) {
    const entry = this.hubs.get(hubId);
    if (!entry) return;

    if (entry.idleDestroyTimer) {
      clearTimeout(entry.idleDestroyTimer);
    }

    this.hubs.delete(hubId);
    entry.hub.destroy();
  }

  get(hubId) {
    return this.hubs.get(hubId)?.hub;
  }

  async #createHub(hubId, options) {
    const hub = new Hub(hubId, options, this);
    await hub.initialize();

    this.hubs.set(hubId, {
      hub,
      refCount: 1,
      idleDestroyTimer: null,
    });

    return hub;
  }
}

export const HubManager = new HubManagerSingleton();
