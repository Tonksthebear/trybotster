import { Controller } from "@hotwired/stimulus";
import { HubManager } from "connections";
import { observeBrowserSocketState } from "transport/hub_signaling_client";

/**
 * Connection status UI for a hub page.
 *
 * Ownership rules:
 * - Browser badge comes only from the tab's ActionCable socket to Rails.
 * - Hub and WebRTC badges come from HubSession's derived status snapshot.
 *
 * The browser badge must never be rewritten from session/WebRTC state.
 */
export default class extends Controller {
  static values = {
    hubId: String,
    type: { type: String, default: "hub" },
  };

  static targets = ["browserSection", "connectionSection", "hubSection"];

  #disconnected = false;

  connect() {
    if (!this.hubIdValue) return;
    this.#disconnected = false;
    this.unsubscribers = [];
    this.#acquireHub();
  }

  disconnect() {
    this.#disconnected = true;
    this.unsubscribers?.forEach((unsub) => unsub());
    this.unsubscribers = [];
    this.hub?.release();
    this.hub = null;
  }

  async #acquireHub() {
    this.#setBrowserStatus("connecting");
    this.#setConnectionState("disconnected");
    this.#setHubStatus("connecting");

    try {
      const stopObservingBrowser = await observeBrowserSocketState((state) => {
        this.#setBrowserStatus(state === "connected" ? "connected" : state);
      });
      this.unsubscribers.push(stopObservingBrowser);

      this.hub = await HubManager.acquire(this.hubIdValue);

      if (this.#disconnected) {
        this.hub.release();
        this.hub = null;
        return;
      }

      this.unsubscribers.push(
        this.hub.onConnectionStatusChange((status) => {
          this.#renderStatus(status);
        }),
      );

      this.#renderStatus(this.hub.connectionStatus.current());
    } catch (error) {
      console.error("[ConnectionStatus] Failed to acquire hub:", error);
      this.#setConnectionState("disconnected");
    }
  }

  #renderStatus(status) {
    if (!status) return;

    this.#setConnectionState(status.connection || "disconnected");
    this.#setHubStatus(status.hub || "connecting");
  }

  #setBrowserStatus(status) {
    if (this.hasBrowserSectionTarget) {
      this.browserSectionTarget.dataset.status = status;
    }
  }

  #setConnectionState(state) {
    if (this.hasConnectionSectionTarget) {
      this.connectionSectionTarget.dataset.state = state;
    }
  }

  #setHubStatus(status) {
    if (this.hasHubSectionTarget) {
      this.hubSectionTarget.dataset.status = status;
    }
  }
}
