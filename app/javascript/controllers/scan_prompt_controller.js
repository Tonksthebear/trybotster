import { Controller } from "@hotwired/stimulus";
import { ConnectionManager, HubConnection } from "connections";

/**
 * Shows a prominent QR scan prompt when the connection needs pairing.
 * Acquires the same shared connection as connection_status_controller.
 */
export default class extends Controller {
  static values = { hubId: String };

  connect() {
    this.element.classList.add("hidden");
    this.unsubscribers = [];
    this.#acquireConnection();
  }

  disconnect() {
    this.unsubscribers.forEach(unsub => unsub());
    this.unsubscribers = [];
    this.connection?.release();
    this.connection = null;
  }

  async #acquireConnection() {
    try {
      this.connection = await ConnectionManager.acquire(
        HubConnection, this.hubIdValue, { hubId: this.hubIdValue }
      );

      this.unsubscribers.push(
        this.connection.on("error", ({ reason }) => {
          this.#update(reason === "session_invalid" || reason === "unpaired");
        }),
        this.connection.on("connected", () => this.#update(false)),
        this.connection.on("disconnected", () => this.#update(false))
      );

      // Sync initial state
      const err = this.connection.errorCode;
      if (err === "unpaired" || err === "session_invalid") {
        this.#update(true);
      }
    } catch (e) {
      // Connection failed — don't show scan prompt
    }
  }

  #update(needsScan) {
    this.element.classList.toggle("hidden", !needsScan);
  }
}
