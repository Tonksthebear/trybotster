import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => {
  const listeners = new Map();
  const subscriptionListeners = new Map();
  const sendCalls = [];

  const bridge = {
    hasPairing: vi.fn(() => Promise.resolve({ hasPairing: true })),
    getIdentityKey: vi.fn(() => Promise.resolve({ identityKey: "browser-key" })),
    hasSession: vi.fn(() => Promise.resolve({ hasSession: true })),
    encryptBinary: vi.fn(() => Promise.resolve({ data: new Uint8Array([1, 2, 3]) })),
    send: vi.fn((type, payload) => {
      sendCalls.push([type, payload]);
      if (type === "connectSignaling") {
        queueMicrotask(() => {
          bridge.emit("health", { hubId: payload.hubId, cli: "online" });
        });
        return Promise.resolve({
          state: "connected",
          browserSocketState: "connected",
          mode: "direct",
        });
      }
      if (type === "probePeerHealth") {
        return Promise.resolve({ alive: true, pcState: "connected", dcState: "open" });
      }
      return Promise.resolve({});
    }),
    on: vi.fn((event, callback) => {
      if (!listeners.has(event)) listeners.set(event, new Set());
      listeners.get(event).add(callback);
      return () => listeners.get(event)?.delete(callback);
    }),
    emit(event, payload) {
      for (const callback of listeners.get(event) || []) {
        callback(payload);
      }
    },
    onSubscriptionMessage: vi.fn((subscriptionId, callback) => {
      subscriptionListeners.set(subscriptionId, callback);
      return () => subscriptionListeners.delete(subscriptionId);
    }),
    emitSubscription(subscriptionId, message) {
      subscriptionListeners.get(subscriptionId)?.(message);
    },
    clearSubscriptionListeners: vi.fn((subscriptionId) => {
      subscriptionListeners.delete(subscriptionId);
    }),
    reset() {
      listeners.clear();
      subscriptionListeners.clear();
      sendCalls.length = 0;
      bridge.hasPairing.mockClear();
      bridge.getIdentityKey.mockClear();
      bridge.hasSession.mockClear();
      bridge.encryptBinary.mockClear();
      bridge.send.mockClear();
      bridge.on.mockClear();
      bridge.onSubscriptionMessage.mockClear();
      bridge.clearSubscriptionListeners.mockClear();
    },
    sendCalls,
  };

  return {
    bridge,
    ensureMatrixReady: vi.fn(() => Promise.resolve()),
    observeBrowserSocketState: vi.fn((callback) => {
      callback("connected");
      return Promise.resolve(() => {});
    }),
  };
});

vi.mock("workers/bridge", () => ({
  default: mocks.bridge,
}));

vi.mock("matrix/bundle", () => ({
  ensureMatrixReady: mocks.ensureMatrixReady,
}));

vi.mock("transport/hub_signaling_client", () => ({
  observeBrowserSocketState: mocks.observeBrowserSocketState,
}));

async function flushPromises() {
  await Promise.resolve();
  await Promise.resolve();
}

describe("HubRoute peer health probes", () => {
  let HubRoute;
  let route;
  let manager;

  beforeEach(async () => {
    vi.useFakeTimers();
    vi.spyOn(console, "warn").mockImplementation(() => {});
    vi.spyOn(console, "error").mockImplementation(() => {});
    mocks.bridge.reset();
    document.body.innerHTML = "";
    window.localStorage.clear();

    ({ HubRoute } = await import("../lib/connections/hub_route"));
    class TestHubRoute extends HubRoute {
      channelName() {
        return "hub";
      }

      computeSubscriptionId() {
        return "hub:hub-1";
      }

      channelParams() {
        return { hub_id: this.getHubId() };
      }
    }

    manager = {
      hasActiveConnectionForHub: vi.fn(() => true),
      notifySubscribers: vi.fn(),
      release: vi.fn(),
    };
    route = new TestHubRoute("hub-1", { hubId: "hub-1" }, manager);
    await route.initialize();
    await flushPromises();
  });

  afterEach(() => {
    route?.destroy();
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  function disconnectPeerCalls() {
    return mocks.bridge.sendCalls.filter(([type]) => type === "disconnectPeer");
  }

  async function emitStallAndWaitForProbe() {
    mocks.bridge.emit("connection:stalled", { hubId: "hub-1" });
    await flushPromises();
    await vi.advanceTimersByTimeAsync(1501);
    await flushPromises();
  }

  it("keeps an open local peer after one missed encrypted pong", async () => {
    await emitStallAndWaitForProbe();

    expect(disconnectPeerCalls()).toHaveLength(0);
    expect(route.subscriptionId).toBe("hub:hub-1");
  });

  it("rebuilds after repeated missed encrypted pongs while local peer is open", async () => {
    await emitStallAndWaitForProbe();
    await vi.advanceTimersByTimeAsync(1001);
    await emitStallAndWaitForProbe();
    await vi.advanceTimersByTimeAsync(1001);
    await emitStallAndWaitForProbe();

    expect(disconnectPeerCalls()).toHaveLength(1);
  });

  it("resets missed pong count when a pong arrives", async () => {
    await emitStallAndWaitForProbe();
    route.processMessage({ type: "dc_pong" });
    await vi.advanceTimersByTimeAsync(1001);
    await emitStallAndWaitForProbe();
    await vi.advanceTimersByTimeAsync(1001);
    await emitStallAndWaitForProbe();

    expect(disconnectPeerCalls()).toHaveLength(0);
  });
});
