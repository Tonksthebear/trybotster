import { Controller } from "@hotwired/stimulus";
import { render } from "preact";

// Thin Stimulus bridge that mounts the Preact workspace island.
// On connect: reads hub ID, connects the bridge, renders the island.
// On disconnect: unmounts cleanly and releases the hub connection.

export default class extends Controller {
  static values = {
    hubId: String,
  };

  #disconnected = false;

  async connect() {
    if (!this.hubIdValue) return;
    this.#disconnected = false;

    // Dynamic imports — only load the island code when this controller mounts
    const [{ html }, { App }, bridge] = await Promise.all([
      import("htm/preact"),
      import("island/app"),
      import("island/hub_bridge"),
    ]);

    if (this.#disconnected) return;

    // Connect the signal store to the hub
    await bridge.connect(this.hubIdValue);

    if (this.#disconnected) {
      bridge.disconnect();
      return;
    }

    this.bridgeDisconnect = bridge.disconnect;

    // Render the Preact island into this element
    render(html`<${App} />`, this.element);

    // Sync selection on Turbo navigations
    this.turboLoadHandler = () => bridge.syncSelectionFromUrl();
    document.addEventListener("turbo:load", this.turboLoadHandler);
  }

  disconnect() {
    this.#disconnected = true;

    if (this.turboLoadHandler) {
      document.removeEventListener("turbo:load", this.turboLoadHandler);
      this.turboLoadHandler = null;
    }

    // Unmount Preact tree
    render(null, this.element);

    // Release hub connection
    this.bridgeDisconnect?.();
    this.bridgeDisconnect = null;
  }
}
