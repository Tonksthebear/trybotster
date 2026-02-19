import { Controller } from "@hotwired/stimulus";
import { ConnectionManager, HubConnection } from "connections";

/**
 * Hub Setup Banner Controller
 *
 * Shows a warning banner when a hub has no session configuration.
 * Listens for profileList events from the hub connection to detect
 * whether shared agent + profiles exist.
 *
 * Offers one-click "Quick Setup" that installs the recommended
 * session template directly from the hub show page.
 */
export default class extends Controller {
  static targets = ["banner", "status"];

  static values = {
    hubId: String,
    templateDest: String,
    templateContent: String,
  };

  connect() {
    if (!this.hubIdValue) return;

    this.unsubscribers = [];
    this.configured = null; // null = unknown, true/false = known

    ConnectionManager.acquire(HubConnection, this.hubIdValue, {
      hubId: this.hubIdValue,
    }).then((hub) => {
      this.hub = hub;

      this.unsubscribers.push(
        this.hub.on("profileList", ({ profiles, sharedAgent }) => {
          this.configured = sharedAgent || profiles.length > 0;
          this.#updateVisibility();
        }),
      );

      this.unsubscribers.push(
        this.hub.onDisconnected(() => {
          // Hide banner when disconnected — can't act on it
          this.bannerTarget.classList.add("hidden");
        }),
      );

      this.unsubscribers.push(
        this.hub.onConnected(() => {
          this.hub.requestProfiles();
        }),
      );
    });
  }

  disconnect() {
    this.unsubscribers?.forEach((unsub) => unsub());
    this.unsubscribers = null;

    const hub = this.hub;
    this.hub = null;
    hub?.release();
  }

  async quickSetup() {
    if (!this.hub || !this.templateDestValue || !this.templateContentValue) return;

    this.#showStatus("Installing...");

    try {
      // Initialize .botster/ structure first
      await this.hub.mkDir(".botster/shared/sessions/agent");
      await this.hub.mkDir(".botster/profiles");

      // Write the template content
      await this.hub.writeFile(
        `.botster/${this.templateDestValue}`,
        this.templateContentValue,
      );

      this.configured = true;
      this.#updateVisibility();

      // Refresh profiles so other controllers pick up the change
      this.hub.requestProfiles();
    } catch (error) {
      this.#showStatus(`Failed: ${error.message}`);
    }
  }

  #updateVisibility() {
    this.bannerTarget.classList.toggle("hidden", this.configured !== false);
  }

  #showStatus(text) {
    if (this.hasStatusTarget) {
      this.statusTarget.textContent = text;
    }
  }
}
