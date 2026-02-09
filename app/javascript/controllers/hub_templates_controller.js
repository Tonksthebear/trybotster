import { Controller } from "@hotwired/stimulus";
import { ConnectionManager, HubConnection } from "connections";

/**
 * Hub Templates Controller
 *
 * Manages install/uninstall of templates via E2E encrypted DataChannel.
 * The catalog is server-rendered HTML — this controller only handles:
 *   - Checking installed status via DataChannel on connect
 *   - Toggling between catalog and preview views
 *   - Install/uninstall actions via DataChannel
 *
 * State management:
 *   - Catalog/preview visibility: toggled via hidden class
 *   - Installed state: data-installed attribute on cards + badge text/style
 *   - Install button: text + classes swapped based on installed state
 */
export default class extends Controller {
  static targets = [
    "catalog",
    "card",
    "badge",
    "previewPanel",
    "installBtn",
    "feedback",
  ];

  static values = {
    hubId: String,
  };

  connect() {
    if (!this.hubIdValue) return;

    this.unsubscribers = [];
    this.installed = new Set();

    ConnectionManager.acquire(HubConnection, this.hubIdValue, {
      hubId: this.hubIdValue,
      fromFragment: true,
    }).then((hub) => {
      this.hub = hub;

      this.unsubscribers.push(
        this.hub.onConnected(() => this.#checkInstalled()),
      );

      this.unsubscribers.push(
        this.hub.onDisconnected(() => {
          this.feedbackTarget.textContent = "Hub disconnected";
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

  // ========== Actions ==========

  preview(event) {
    const slug = event.currentTarget.dataset.slug;
    if (!slug) return;

    this.catalogTarget.classList.add("hidden");

    this.previewPanelTargets.forEach((panel) => {
      panel.classList.toggle("hidden", panel.dataset.slug !== slug);
    });
  }

  backToCatalog() {
    this.previewPanelTargets.forEach((panel) => panel.classList.add("hidden"));
    this.catalogTarget.classList.remove("hidden");
  }

  async toggleInstall(event) {
    const btn = event.currentTarget;
    const slug = btn.dataset.slug;
    const panel = this.previewPanelTargets.find((p) => p.dataset.slug === slug);
    if (!panel || !this.hub) return;

    const dest = panel.dataset.dest;
    const isInstalled = this.installed.has(dest);

    btn.disabled = true;
    btn.textContent = isInstalled ? "Uninstalling..." : "Installing...";

    try {
      if (isInstalled) {
        await this.hub.uninstallTemplate(dest);
        this.installed.delete(dest);
      } else {
        await this.hub.installTemplate(dest, panel.dataset.content);
        this.installed.add(dest);
      }
      this.#syncState(slug, dest);
    } catch (error) {
      btn.textContent = "Failed";
      setTimeout(() => this.#syncState(slug, dest), 2000);
    } finally {
      btn.disabled = false;
    }
  }

  // ========== DataChannel ==========

  async #checkInstalled() {
    this.feedbackTarget.textContent = "Checking installed templates...";

    try {
      const result = await this.hub.listInstalledTemplates();
      if (!this.hub) return;

      this.installed = new Set(result.installed || []);
      this.#syncAllStates();
      this.feedbackTarget.textContent = "";
    } catch {
      this.feedbackTarget.textContent = "";
    }
  }

  // ========== State Sync ==========

  /** Update all cards and buttons to reflect installed state. */
  #syncAllStates() {
    this.cardTargets.forEach((card) => {
      this.#syncState(card.dataset.slug, card.dataset.dest);
    });
  }

  /** Update a single template's card badge and install button. */
  #syncState(slug, dest) {
    const isInstalled = this.installed.has(dest);

    // Update catalog card badge
    const badge = this.badgeTargets.find((b) => b.dataset.badgeFor === slug);
    if (badge) {
      badge.textContent = isInstalled ? "installed" : "available";
      badge.className = isInstalled
        ? "shrink-0 text-[10px] px-1.5 py-0.5 rounded bg-emerald-500/10 text-emerald-400"
        : "shrink-0 text-[10px] px-1.5 py-0.5 rounded bg-zinc-700/50 text-zinc-500";
    }

    // Update install button
    const btn = this.installBtnTargets.find((b) => b.dataset.slug === slug);
    if (btn) {
      btn.textContent = isInstalled ? "Uninstall" : "Install";
      btn.className = isInstalled
        ? "shrink-0 px-4 py-2 text-sm font-medium rounded transition-colors disabled:opacity-50 text-red-400 hover:text-red-300 bg-red-500/10 hover:bg-red-500/20 border border-red-500/30"
        : "shrink-0 px-4 py-2 text-sm font-medium rounded transition-colors disabled:opacity-50 text-emerald-400 hover:text-emerald-300 bg-emerald-500/10 hover:bg-emerald-500/20 border border-emerald-500/30";
    }
  }
}
