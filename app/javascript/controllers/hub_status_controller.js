import { Controller } from "@hotwired/stimulus";
import { ConnectionManager, HubConnection } from "connections";

/**
 * Hub Status Controller
 *
 * Minimal controller that sets a data attribute based on connection state.
 * All visual rendering is handled by CSS using Tailwind's group-data-[] variants.
 *
 * All values come from Rails via data attributes — no URL parsing.
 *
 * States: disconnected, loading, connecting, handshake, connected, error
 *
 * Usage:
 *   <div data-controller="hub-status"
 *        data-hub-status-hub-id-value="<%= Current.hub.id %>"
 *        data-hub-status-state-value="disconnected"
 *        class="group">
 *     <span class="hidden group-data-[hub-status-state-value=connected]:flex">Connected</span>
 *     <span class="hidden group-data-[hub-status-state-value=loading]:flex">Loading...</span>
 *   </div>
 */
export default class extends Controller {
  static values = {
    hubId: String,
    state: { type: String, default: "disconnected" },
    error: String,
  };

  connect() {
    if (!this.hubIdValue) {
      this.stateValue = "error";
      this.errorValue = "No hub ID";
      return;
    }

    ConnectionManager.acquire(HubConnection, this.hubIdValue, {
      hubId: this.hubIdValue,
      fromFragment: true,
    }).then(async (hub) => {
      this.hub = hub;

      this.hub.onStateChange(({ state, error }) => {
        this.#updateState(state, error);
      });

      this.hub.onError(({ message }) => {
        this.errorValue = message;
      });

      await this.hub.subscribe();
    });
  }

  disconnect() {
    const hub = this.hub;
    this.hub = null;
    // Just release - don't unsubscribe. HubConnection is shared and
    // the subscription can be reused by other controllers after navigation.
    hub?.release();
  }

  // Action: manual reconnect
  reconnect() {
    this.hub?.reconnect();
  }

  #updateState(state, error) {
    // Map connection states to simplified UI states
    const stateMap = {
      disconnected: "disconnected",
      loading: "loading",
      connecting: "connecting",
      handshake_sent: "handshake",
      connected: "connected",
      error: "error",
    };

    this.stateValue = stateMap[state] || state;
    if (error) {
      this.errorValue = error;
    }
  }
}
