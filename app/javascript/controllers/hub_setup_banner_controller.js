import { Controller } from "@hotwired/stimulus";
import { HubManager } from "connections";

/**
 * Hub Setup Banner Controller
 *
 * Shows a warning banner when a hub has no agent configuration.
 * Listens for agentConfig events from Hub to detect
 * whether any agents are configured.
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
    this.targetConfigState = new Map();
    this.selectedTargetId = null;

    HubManager.acquire(this.hubIdValue).then((hub) => {
      this.hub = hub;

      this.unsubscribers.push(
        this.hub.onSpawnTargetList((targets) => {
          const admittedTargets = Array.isArray(targets) ? targets : [];
          this.selectedTargetId = admittedTargets.length === 1 ? admittedTargets[0].id : null;
          this.targetConfigState = new Map();

          if (admittedTargets.length === 0) {
            this.configured = true;
            this.#updateVisibility();
            return;
          }

          admittedTargets.forEach((target) => {
            if (this.hub.hasAgentConfig(target.id)) {
              const config = this.hub.getAgentConfig(target.id);
              this.targetConfigState.set(target.id, config.agents.length > 0);
            } else {
              this.targetConfigState.set(target.id, null);
              this.hub.ensureAgentConfig(target.id);
            }
          });

          this.#recomputeConfigured();
          this.#updateVisibility();
        }),
      );

      this.unsubscribers.push(
        this.hub.on("agentConfig", ({ targetId, agents }) => {
          if (!targetId) return;
          this.targetConfigState.set(targetId, agents.length > 0);
          this.#recomputeConfigured();
          this.#updateVisibility();
        }),
      );

      this.unsubscribers.push(
        this.hub.onDisconnected(() => {
          // Hide banner when disconnected — can't act on it
          this.bannerTarget.classList.add("hidden");
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
    if (!this.selectedTargetId) {
      this.#showStatus("Select a spawn target in Settings before using Quick Setup.");
      return;
    }

    this.#showStatus("Installing...");

    try {
      // Initialize .botster/ structure — new agents/ layout
      const parentDir = this.templateDestValue.replace(/\/[^/]+$/, "");
      await this.hub.mkDir(`.botster/${parentDir}`, "repo", this.selectedTargetId);

      // Write the template content
      await this.hub.writeFile(
        `.botster/${this.templateDestValue}`,
        this.templateContentValue,
        "repo",
        this.selectedTargetId,
      );

      this.configured = true;
      this.#updateVisibility();

      // Refresh target-scoped config state so other controllers pick up the change
      await this.hub.ensureAgentConfig(this.selectedTargetId, { force: true });
    } catch (error) {
      this.#showStatus(`Failed: ${error.message}`);
    }
  }

  #updateVisibility() {
    this.bannerTarget.classList.toggle("hidden", this.configured !== false);
  }

  #recomputeConfigured() {
    const states = Array.from(this.targetConfigState.values());
    this.configured = states.some(Boolean);
    if (!this.configured && states.every((value) => value !== null)) {
      this.configured = false;
    }
  }

  #showStatus(text) {
    if (this.hasStatusTarget) {
      this.statusTarget.textContent = text;
    }
  }
}
