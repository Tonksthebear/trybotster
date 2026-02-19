import { Controller } from "@hotwired/stimulus";
import { Turbo } from "@hotwired/turbo-rails";
import bridge from "workers/bridge";
import { ConnectionManager } from "connections/connection_manager";
import { HubConnection } from "connections/hub_connection";

/**
 * Push Subscription Controller
 *
 * Manages browser push notification subscriptions per device.
 * Mounts on /settings/devices/:id — one instance per device page.
 *
 * Three states per device row:
 * 1. CLI has no VAPID keys → show Enable (generate or copy keys)
 * 2. CLI has VAPID keys, browser not subscribed → show "Enable on this device"
 * 3. CLI has VAPID keys, browser subscribed → show Test / Disable
 *
 * Two flows for enabling:
 * A) First device: generate new VAPID keys on CLI, subscribe browser
 * B) Second device: copy keys from source device, then subscribe
 * C) Same CLI, new browser: get existing VAPID public key, subscribe browser
 */
export default class extends Controller {
  static values = {
    deviceId: Number,
    hubId: String,
    sourceHubId: String,
    swUrl: String,
    enabled: Boolean,
  };

  static targets = [
    "status",
    "subscribedButtons",
    "enableBrowserButtons",
    "enableButtons",
  ];

  #unsubscribers = [];
  #connUnsubscribers = [];  // onConnected/onError callbacks on HubConnection
  #hubConn = null;
  #sourceHubConn = null;

  connect() {
    this.#unsubscribers.push(
      bridge.on("push:vapid_key", (data) => this.#handleVapidKey(data))
    );
    this.#unsubscribers.push(
      bridge.on("push:sub_ack", (data) => this.#handleSubAck(data))
    );
    this.#unsubscribers.push(
      bridge.on("push:vapid_keys", (data) => this.#handleVapidKeys(data))
    );
    this.#unsubscribers.push(
      bridge.on("push:test_ack", (data) => this.#handleTestAck(data))
    );
    this.#unsubscribers.push(
      bridge.on("push:disable_ack", (data) => this.#handleDisableAck(data))
    );

    this.#checkBrowserState();
  }

  disconnect() {
    for (const unsub of this.#unsubscribers) {
      unsub();
    }
    this.#unsubscribers = [];
    this.#releaseConnections();
  }

  // ========== Browser State Check ==========

  async #checkBrowserState() {
    if (!this.enabledValue) {
      // CLI has no VAPID keys — show full enable flow
      this.#showButtons("enable");
      return;
    }

    // CLI has VAPID keys — check if this browser has a valid push subscription.
    // A subscription is only valid if we recorded the VAPID key we used to create it.
    // This catches stale subscriptions from old VAPID keys (e.g. after device reset).
    try {
      const registration = await navigator.serviceWorker.getRegistration("/");
      const subscription = await registration?.pushManager?.getSubscription();

      if (subscription && localStorage.getItem("botster_vapid_key")) {
        this.#showButtons("subscribed");
      } else {
        this.#showButtons("enableBrowser");
      }
    } catch (e) {
      this.#showButtons("enableBrowser");
    }
  }

  #showButtons(state) {
    if (this.hasSubscribedButtonsTarget) {
      this.subscribedButtonsTarget.classList.toggle("hidden", state !== "subscribed");
    }
    if (this.hasEnableBrowserButtonsTarget) {
      this.enableBrowserButtonsTarget.classList.toggle("hidden", state !== "enableBrowser");
    }
    if (this.hasEnableButtonsTarget) {
      this.enableButtonsTarget.classList.toggle("hidden", state !== "enable");
    }
  }

  // ========== Actions ==========

  async enable() {
    if (!this.hubIdValue) return;
    if (!("serviceWorker" in navigator) || !("PushManager" in window)) {
      this.#setStatus("not-supported");
      return;
    }

    const permission = await Notification.requestPermission();
    if (permission !== "granted") {
      this.#setStatus("denied");
      return;
    }

    this.#setStatus("connecting");

    try {
      this.#hubConn = await ConnectionManager.acquire(
        HubConnection, this.hubIdValue, { hubId: this.hubIdValue }
      );

      if (this.hasSourceHubIdValue && this.sourceHubIdValue) {
        this.#sourceHubConn = await ConnectionManager.acquire(
          HubConnection, this.sourceHubIdValue, { hubId: this.sourceHubIdValue }
        );
      }

      this.#connUnsubscribers.push(
        this.#hubConn.onConnected(() => this.#startFlow())
      );
      this.#connUnsubscribers.push(
        this.#hubConn.onError(({ reason }) => {
          if (reason === "unpaired") {
            this.#setStatus("unpaired");
          } else {
            this.#setStatus("error");
          }
        })
      );
    } catch (e) {
      console.error("[PushSubscription] Failed to acquire connection:", e);
      this.#setStatus("error");
      this.#releaseConnections();
    }
  }

  /**
   * Enable on this browser — CLI already has VAPID keys, just need to
   * get the public key and subscribe this browser's push manager.
   */
  async enableBrowser() {
    if (!this.hubIdValue) return;
    if (!("serviceWorker" in navigator) || !("PushManager" in window)) {
      this.#setStatus("not-supported");
      return;
    }

    const permission = await Notification.requestPermission();
    if (permission !== "granted") {
      this.#setStatus("denied");
      return;
    }

    this.#setStatus("connecting");

    try {
      this.#hubConn = await ConnectionManager.acquire(
        HubConnection, this.hubIdValue, { hubId: this.hubIdValue }
      );

      this.#connUnsubscribers.push(
        this.#hubConn.onConnected(async () => {
          this.#setStatus("enabling");
          // Ask CLI for its existing VAPID public key
          await bridge.send("sendControlMessage", {
            hubId: this.hubIdValue,
            message: { type: "vapid_pub_req" },
          });
          // Continues in #handleVapidKey when CLI responds
        })
      );
      this.#connUnsubscribers.push(
        this.#hubConn.onError(({ reason }) => {
          if (reason === "unpaired") {
            this.#setStatus("unpaired");
          } else {
            this.#setStatus("error");
          }
        })
      );
    } catch (e) {
      console.error("[PushSubscription] Failed to acquire connection:", e);
      this.#setStatus("error");
      this.#releaseConnections();
    }
  }

  async disable() {
    if (!this.hubIdValue) return;
    this.#setStatus("disabling");
    try {
      this.#hubConn = await ConnectionManager.acquire(
        HubConnection, this.hubIdValue, { hubId: this.hubIdValue }
      );
      this.#connUnsubscribers.push(
        this.#hubConn.onConnected(async () => {
          await bridge.send("sendControlMessage", {
            hubId: this.hubIdValue,
            message: { type: "push_disable" },
          });
        })
      );
      this.#connUnsubscribers.push(
        this.#hubConn.onError(() => this.#setStatus("error"))
      );
    } catch (e) {
      console.error("[PushSubscription] Failed to disable:", e);
      this.#setStatus("error");
    }
  }

  async test() {
    if (!this.hubIdValue) return;
    this.#setStatus("testing");
    try {
      this.#hubConn = await ConnectionManager.acquire(
        HubConnection, this.hubIdValue, { hubId: this.hubIdValue }
      );
      this.#connUnsubscribers.push(
        this.#hubConn.onConnected(async () => {
          await bridge.send("sendControlMessage", {
            hubId: this.hubIdValue,
            message: { type: "push_test" },
          });
        })
      );
      this.#connUnsubscribers.push(
        this.#hubConn.onError(() => this.#setStatus("error"))
      );
    } catch (e) {
      console.error("[PushSubscription] Test failed:", e);
      this.#setStatus("error");
    }
  }

  // ========== Flows ==========

  async #startFlow() {
    this.#setStatus("enabling");
    try {
      if (this.#sourceHubConn) {
        await this.#copyFlow();
      } else {
        await this.#generateFlow();
      }
    } catch (e) {
      console.error("[PushSubscription] Failed:", e);
      this.#setStatus("error");
    }
  }

  async #generateFlow() {
    await bridge.send("sendControlMessage", {
      hubId: this.hubIdValue,
      message: { type: "vapid_generate" },
    });
    // Continues in #handleVapidKey when CLI responds
  }

  async #copyFlow() {
    await bridge.send("sendControlMessage", {
      hubId: this.sourceHubIdValue,
      message: { type: "vapid_key_req" },
    });
    // Continues in #handleVapidKeys → #handleVapidKey
  }

  // ========== Event Handlers ==========

  async #handleVapidKeys({ hubId, pub: pubKey, priv: privKey }) {
    // Flow B step 2: received keypair from source device, forward to target
    if (hubId !== this.sourceHubIdValue) return;

    await bridge.send("sendControlMessage", {
      hubId: this.hubIdValue,
      message: { type: "vapid_key_set", pub: pubKey, priv: privKey },
    });
    // Continues in #handleVapidKey when target CLI responds
  }

  async #handleVapidKey({ hubId, key }) {
    // All flows converge here: CLI has keys and sent the public key.
    // Permission was already granted in enable/enableBrowser (user gesture required).
    if (hubId !== this.hubIdValue) return;

    try {
      const registration = await navigator.serviceWorker.register(
        this.swUrlValue,
        { scope: "/" }
      );
      await navigator.serviceWorker.ready;

      // Unsubscribe any existing push subscription — the VAPID key may have
      // changed (e.g. device reset), and PushManager rejects subscribe() if
      // the applicationServerKey doesn't match the existing subscription.
      const existing = await registration.pushManager.getSubscription();
      if (existing) {
        await existing.unsubscribe();
      }

      const applicationServerKey = this.#urlBase64ToUint8Array(key);
      const subscription = await registration.pushManager.subscribe({
        userVisibleOnly: true,
        applicationServerKey,
      });

      // Record the VAPID key so #checkBrowserState can verify the subscription
      // is for the current key (detects stale subscriptions after key rotation).
      localStorage.setItem("botster_vapid_key", key);

      await this.#sendSubscriptionToCli(subscription);
      this.#setStatus("subscribing");
    } catch (e) {
      console.error("[PushSubscription] Failed to subscribe:", e);
      this.#setStatus("error");
    }
  }

  #handleSubAck({ hubId }) {
    if (hubId !== this.hubIdValue) return;
    this.#releaseConnections();
    // Reload to re-render server-side button state (Enable → Disable)
    Turbo.visit(window.location.href, { action: "replace" });
  }

  #handleTestAck({ hubId, sent }) {
    if (hubId !== this.hubIdValue) return;
    this.#releaseConnections();
    this.#setStatus(sent > 0 ? "test-sent" : "test-failed");
  }

  async #handleDisableAck({ hubId }) {
    if (hubId !== this.hubIdValue) return;
    this.#releaseConnections();

    // Unsubscribe browser push after CLI confirms it's disabled
    try {
      const registration = await navigator.serviceWorker.getRegistration("/");
      if (registration) {
        const subscription = await registration.pushManager.getSubscription();
        if (subscription) {
          await subscription.unsubscribe();
        }
      }
    } catch (e) {
      console.error("[PushSubscription] Failed to unsubscribe browser push:", e);
    }

    localStorage.removeItem("botster_vapid_key");

    Turbo.visit(window.location.href, { action: "replace" });
  }

  // ========== Helpers ==========

  async #sendSubscriptionToCli(subscription) {
    const json = subscription.toJSON();
    await bridge.send("sendControlMessage", {
      hubId: this.hubIdValue,
      message: {
        type: "push_sub",
        browser_id: this.#getBrowserId(),
        endpoint: json.endpoint,
        p256dh: json.keys.p256dh,
        auth: json.keys.auth,
      },
    });
  }

  #getBrowserId() {
    const key = "botster_browser_id";
    let id = localStorage.getItem(key);
    if (!id) {
      id = crypto.randomUUID();
      localStorage.setItem(key, id);
    }
    return id;
  }

  #setStatus(status) {
    if (this.hasStatusTarget) {
      this.statusTarget.dataset.pushStatus = status;
    }
  }

  #releaseConnections() {
    for (const unsub of this.#connUnsubscribers) {
      unsub();
    }
    this.#connUnsubscribers = [];
    this.#hubConn?.release();
    this.#hubConn = null;
    this.#sourceHubConn?.release();
    this.#sourceHubConn = null;
  }

  #urlBase64ToUint8Array(base64String) {
    const padding = "=".repeat((4 - (base64String.length % 4)) % 4);
    const base64 = (base64String + padding).replace(/-/g, "+").replace(/_/g, "/");
    const rawData = atob(base64);
    const outputArray = new Uint8Array(rawData.length);
    for (let i = 0; i < rawData.length; ++i) {
      outputArray[i] = rawData.charCodeAt(i);
    }
    return outputArray;
  }
}
