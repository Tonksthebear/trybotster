let cableConsumer = null;
let browserSocketState = "disconnected";
const browserSocketObservers = new Set();

function notifyBrowserSocketObservers(state) {
  browserSocketState = state;
  for (const callback of browserSocketObservers) {
    try {
      callback(state);
    } catch (error) {
      console.error("[HubSignalingClient] Browser socket observer error:", error);
    }
  }
}

function currentBrowserSocketState(connection) {
  if (connection?.isOpen()) return "connected";
  if (connection?.isActive()) return "connecting";
  return "disconnected";
}

function wrapBrowserSocketHandlers(connection) {
  const socket = connection.webSocket;
  if (!socket || socket.__botsterBrowserSocketWrapped) return;

  socket.__botsterBrowserSocketWrapped = true;

  const previousOpen = socket.onopen;
  socket.onopen = (event) => {
    previousOpen?.(event);
    setBrowserSocketState("connected");
  };

  const previousClose = socket.onclose;
  socket.onclose = (event) => {
    previousClose?.(event);
    setBrowserSocketState("disconnected");
  };
}

function setBrowserSocketState(state) {
  if (!state || state === browserSocketState) return;
  notifyBrowserSocketObservers(state);
}

function installBrowserSocketObserver(consumer) {
  if (consumer.__botsterBrowserSocketObserverInstalled) {
    setBrowserSocketState(currentBrowserSocketState(consumer.connection));
    return;
  }

  consumer.__botsterBrowserSocketObserverInstalled = true;

  const { connection } = consumer;
  const originalOpen = connection.open.bind(connection);
  connection.open = (...args) => {
    const result = originalOpen(...args);
    wrapBrowserSocketHandlers(connection);
    setBrowserSocketState(currentBrowserSocketState(connection));
    return result;
  };

  const originalClose = connection.close.bind(connection);
  connection.close = (...args) => {
    const result = originalClose(...args);
    setBrowserSocketState(currentBrowserSocketState(connection));
    return result;
  };

  const originalInstall = connection.installEventHandlers.bind(connection);
  connection.installEventHandlers = () => {
    originalInstall();
    wrapBrowserSocketHandlers(connection);
  };

  wrapBrowserSocketHandlers(connection);
  setBrowserSocketState(currentBrowserSocketState(connection));
}

export async function getActionCableConsumer() {
  if (!cableConsumer) {
    const { createConsumer } = await import("@rails/actioncable");
    cableConsumer = createConsumer();
    installBrowserSocketObserver(cableConsumer);
  }
  return cableConsumer;
}

export function getBrowserSocketState() {
  return browserSocketState;
}

export async function observeBrowserSocketState(callback) {
  browserSocketObservers.add(callback);
  const consumer = await getActionCableConsumer();
  const state = currentBrowserSocketState(consumer.connection);
  setBrowserSocketState(state);
  callback(browserSocketState);

  return () => {
    browserSocketObservers.delete(callback);
  };
}

export class HubSignalingClient {
  #subscriptions = new Map();
  #notify;

  constructor({ notify }) {
    this.#notify = notify;
  }

  get browserSocketState() {
    return browserSocketState;
  }

  getSubscription(hubId) {
    return this.#subscriptions.get(hubId) || null;
  }

  disconnect(hubId) {
    const subscription = this.#subscriptions.get(hubId);
    if (!subscription) return;

    subscription.unsubscribe();
    this.#subscriptions.delete(hubId);
  }

  emitBrowserSocketStateForHub(hubId) {
    queueMicrotask(() => {
      if (!this.#subscriptions.has(hubId)) return;
      this.#notify("browser:state", { hubId, state: browserSocketState });
    });
  }

  async connect(hubId, browserIdentity, { onMessage, onState }) {
    const consumer = await getActionCableConsumer();

    const subscription = consumer.subscriptions.create(
      { channel: "HubSignalingChannel", hub_id: hubId, browser_identity: browserIdentity },
      {
        received: (data) => {
          onMessage?.(data);
        },
        connected: () => {
          onState?.("connected");
          this.#notify("signaling:state", { hubId, state: "connected" });
        },
        disconnected: () => {
          onState?.("disconnected");
          this.#notify("signaling:state", { hubId, state: "disconnected" });
        },
        rejected: () => {
          console.error(`[HubSignalingClient] Signaling channel rejected for hub ${hubId}`);
        },
      },
    );

    this.#subscriptions.set(hubId, subscription);
    return subscription;
  }
}
