import { Controller } from "@hotwired/stimulus";
import { ConnectionManager } from "connections/connection_manager";

/**
 * RequiresConnectionController - Disables elements when a connection isn't available.
 *
 * A minimal, passive controller that observes connection state without holding
 * a reference. Perfect for buttons, links, or form fields that should only be
 * interactive when connected.
 *
 * Usage:
 *   <button data-controller="requires-connection"
 *           data-requires-connection-key-value="<%= @hub.id %>">
 *     New Agent
 *   </button>
 *
 * The element will have `disabled` attribute set when disconnected.
 * For non-button elements, also sets `aria-disabled="true"` and
 * `pointer-events: none` via a data attribute for CSS targeting.
 */
export default class extends Controller {
  static values = {
    key: String, // Connection key to observe
  };

  connect() {
    if (!this.keyValue) {
      console.error("[requires-connection] Missing key value");
      return;
    }

    this.unsubscribe = ConnectionManager.subscribe(
      this.keyValue,
      ({ state }) => {
        this.#updateDisabled(state !== "connected");
      },
    );
  }

  disconnect() {
    this.unsubscribe?.();
  }

  #updateDisabled(disabled) {
    if (disabled) {
      this.element.setAttribute("disabled", "");
      this.element.setAttribute("aria-disabled", "true");
      this.element.dataset.connectionState = "disconnected";
    } else {
      this.element.removeAttribute("disabled");
      this.element.removeAttribute("aria-disabled");
      this.element.dataset.connectionState = "connected";
    }
  }
}
