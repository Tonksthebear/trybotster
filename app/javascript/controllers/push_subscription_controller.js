import { Controller } from "@hotwired/stimulus";
import { Turbo } from "@hotwired/turbo-rails";
import bridge from "workers/bridge";
import { ConnectionManager } from "connections/connection_manager";
import { HubConnection } from "connections/hub_connection";

/**
 * Push Subscription Controller
 *
 * Manages browser push notification subscriptions per device.
 * Mounts on hub device settings page — one instance per device.
 *
 * On connect, acquires HubConnection and sends push_status_req to CLI
 * with this browser's stable ID. CLI responds with push:status containing
 * has_keys and browser_subscribed booleans. Three states:
 *
 * 1. CLI has no VAPID keys → show Enable (generate or copy keys)
 * 2. CLI has VAPID keys, browser not subscribed → show "Enable on this browser"
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
      bridge.on("push:status", (data) => this.#handlePushStatus(data))
    );
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

    this.#queryPushStatus();
  }

  disconnect() {
    for (const unsub of this.#unsubscribers) {
      unsub();
    }
    this.#unsubscribers = [];
    this.#releaseConnections();
  }

  // ========== Push Status Query ==========

  async #queryPushStatus() {
    if (!this.hubIdValue) return;

    this.#setStatus("checking");

    try {
      this.#hubConn = await ConnectionManager.acquire(
        HubConnection, this.hubIdValue, { hubId: this.hubIdValue }
      );

      this.#connUnsubscribers.push(
        this.#hubConn.onConnected(async () => {
          await bridge.send("sendControlMessage", {
            hubId: this.hubIdValue,
            message: {
              type: "push_status_req",
              browser_id: this.#getBrowserId(),
            },
          });
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
      console.error("[PushSubscription] Failed to query push status:", e);
      this.#setStatus("error");
    }
  }

  #handlePushStatus({ hubId, hasKeys, browserSubscribed, vapidPub }) {
    if (hubId !== this.hubIdValue) return;

    // Detect stale subscription: CLI has keys and thinks browser is subscribed,
    // but the browser's PushManager subscription was created with a different
    // VAPID key (e.g. after a key rotation or device reset). Transparently
    // resubscribe with the current key.
    if (hasKeys && browserSubscribed && vapidPub) {
      const storedKey = localStorage.getItem("botster_vapid_key");
      if (storedKey && storedKey !== vapidPub) {
        console.warn("[PushSubscription] VAPID key mismatch — resubscribing with current key");
        this.#handleVapidKey({ hubId, key: vapidPub });
        return;
      }
    }

    if (!hasKeys) {
      this.#showButtons("enable");
    } else if (!browserSubscribed) {
      this.#showButtons("enableBrowser");
    } else {
      this.#showButtons("subscribed");
    }

    this.#setStatus("");
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
      // Connection is already acquired from #queryPushStatus — reuse it.
      // If not connected yet, acquire fresh.
      if (!this.#hubConn) {
        this.#hubConn = await ConnectionManager.acquire(
          HubConnection, this.hubIdValue, { hubId: this.hubIdValue }
        );
      }

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
      if (!this.#hubConn) {
        this.#hubConn = await ConnectionManager.acquire(
          HubConnection, this.hubIdValue, { hubId: this.hubIdValue }
        );
      }

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
      if (!this.#hubConn) {
        this.#hubConn = await ConnectionManager.acquire(
          HubConnection, this.hubIdValue, { hubId: this.hubIdValue }
        );
      }
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
      if (!this.#hubConn) {
        this.#hubConn = await ConnectionManager.acquire(
          HubConnection, this.hubIdValue, { hubId: this.hubIdValue }
        );
      }
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

      // Record the VAPID key so we can detect stale subscriptions after key rotation.
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
    // Re-query CLI for fresh state instead of reloading the page
    this.#showButtons("subscribed");
    this.#setStatus("");
  }

  #handleTestAck({ hubId, sent }) {
    if (hubId !== this.hubIdValue) return;
    this.#setStatus(sent > 0 ? "test-sent" : "test-failed");
  }

  async #handleDisableAck({ hubId }) {
    if (hubId !== this.hubIdValue) return;

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

    // Update buttons inline instead of page reload
    this.#showButtons("enable");
    this.#setStatus("");
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
