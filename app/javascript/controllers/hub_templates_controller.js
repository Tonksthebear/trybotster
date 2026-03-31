import { Controller } from "@hotwired/stimulus";
import { HubManager } from "connections";

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
    "repoTargetPanel",
    "repoTargetSelect",
    "repoTargetHint",
    "previewPanel",
    "installBtn",
    "scopeBtn",
    "feedback",
  ];

  static values = {
    hubId: String,
  };

  connect() {
    if (!this.hubIdValue) return;

    this.unsubscribers = [];
    this.installedDevice = new Set();  // plugin names installed at device scope
    this.installedRepo = new Set();    // plugin names installed at repo scope
    this.spawnTargets = [];
    this.selectedTargetId = null;
    this.targetsLoaded = false;
    this.installedStateLoaded = false;
    this.installStateToken = 0;

    HubManager.acquire(this.hubIdValue).then((hub) => {
      this.hub = hub;
      this.spawnTargets = hub.spawnTargets.current();
      hub.spawnTargets.load().catch(() => {});
      if (!this.spawnTargets.some((target) => target.id === this.selectedTargetId)) {
        this.selectedTargetId = this.spawnTargets.length === 1 ? this.spawnTargets[0].id : null;
      }
      this.targetsLoaded = this.spawnTargets.length > 0;
      this.#renderRepoTargetOptions();

      this.unsubscribers.push(
        this.hub.spawnTargets.onChange((targets) => {
          this.spawnTargets = Array.isArray(targets) ? targets : [];
          if (!this.spawnTargets.some((target) => target.id === this.selectedTargetId)) {
            this.selectedTargetId = this.spawnTargets.length === 1 ? this.spawnTargets[0].id : null;
          }
          this.targetsLoaded = true;
          this.#renderRepoTargetOptions();
          this.#checkInstalled();
        }),
      );

      this.unsubscribers.push(
        this.hub.onDisconnected(() => {
          this.feedbackTarget.textContent = "Hub disconnected";
        }),
      );
      this.#checkInstalled();
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
    const name = this.#pluginName(dest);
    const scope = this.#installScope(slug);
    const scopeSet = scope === "repo" ? this.installedRepo : this.installedDevice;
    const isInstalled = scopeSet.has(name);
    const targetId = scope === "repo" ? this.selectedTargetId : undefined;

    if (scope === "repo" && !targetId) {
      this.feedbackTarget.textContent = this.spawnTargets.length === 0
        ? "Add a spawn target first to install repo-scoped templates."
        : "Select a spawn target before installing repo-scoped templates.";
      return;
    }

    btn.disabled = true;
    btn.textContent = isInstalled ? "Uninstalling..." : "Installing...";
    this.installStateToken += 1;

    try {
      if (isInstalled) {
        await this.hub.uninstallTemplate(dest, scope, targetId);
        scopeSet.delete(name);
      } else {
        await this.hub.installTemplate(dest, panel.dataset.content, scope, targetId);
        scopeSet.add(name);
        // Auto-load the plugin so it runs without a hub reboot
        await this.hub.loadPlugin(name, targetId).catch(() => {});
      }
      this.#syncState(slug, dest);
      this.dispatch("templateChanged");
    } catch (error) {
      btn.textContent = "Failed";
      setTimeout(() => this.#syncState(slug, dest), 2000);
    } finally {
      btn.disabled = false;
    }
  }

  switchInstallScope(event) {
    const slug = event.currentTarget.dataset.slug;
    const scope = event.currentTarget.dataset.installScope;
    if (!slug || !scope) return;

    // Toggle scope button active state within this template
    this.scopeBtnTargets
      .filter((b) => b.dataset.slug === slug)
      .forEach((b) => b.toggleAttribute("data-active", b.dataset.installScope === scope));

    // Update install button text
    const dest = this.previewPanelTargets.find((p) => p.dataset.slug === slug)?.dataset.dest;
    if (dest) this.#syncState(slug, dest);
  }

  selectRepoTarget(event) {
    this.selectedTargetId = event.currentTarget.value || null;
    this.#renderRepoTargetOptions();
    this.#checkInstalled();
  }

  async reloadPlugin(event) {
    const btn = event.currentTarget;
    const slug = btn.dataset.slug;
    const panel = this.previewPanelTargets.find((p) => p.dataset.slug === slug);
    if (!panel || !this.hub) return;

    const name = this.#pluginName(panel.dataset.dest);
    btn.disabled = true;
    btn.textContent = "Reloading...";

    try {
      await this.hub.reloadPlugin(name, this.selectedTargetId);
      btn.textContent = "Reloaded";
      setTimeout(() => { btn.textContent = "Reload"; btn.disabled = false; }, 1500);
    } catch (error) {
      btn.textContent = "Failed";
      setTimeout(() => { btn.textContent = "Reload"; btn.disabled = false; }, 2000);
    }
  }

  recheckInstalled() {
    if (this.hub) this.#checkInstalled();
  }

  // ========== DataChannel ==========

  async #checkInstalled() {
    const token = ++this.installStateToken;
    this.feedbackTarget.textContent = "Checking installed templates...";

    try {
      const result = await this.hub.listInstalledTemplates(this.selectedTargetId);
      if (!this.hub || token !== this.installStateToken) return;

      this.installedDevice = new Set();
      this.installedRepo = new Set();

      const installed = result.installed || [];
      for (const entry of installed) {
        if (!entry.name) continue;
        if (entry.scope === "device") {
          this.installedDevice.add(entry.name);
        } else if (entry.scope === "repo") {
          this.installedRepo.add(entry.name);
        }
      }

      this.#syncAllStates();
      this.feedbackTarget.textContent = "";
      this.installedStateLoaded = true;
      this.#markReady();
    } catch {
      if (token !== this.installStateToken) return;
      this.feedbackTarget.textContent = "";
      this.installedStateLoaded = true;
      this.#markReady();
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
    const name = this.#pluginName(dest);
    const deviceInstalled = this.installedDevice.has(name);
    const repoInstalled = this.installedRepo.has(name);
    const anyInstalled = deviceInstalled || repoInstalled;

    // Build badge text showing where it's installed
    let badgeText = "available";
    if (anyInstalled) {
      const scopes = [];
      if (deviceInstalled) scopes.push("device");
      if (repoInstalled) scopes.push("repo");
      badgeText = `installed (${scopes.join(", ")})`;
    }

    // Update catalog card badge
    const badge = this.badgeTargets.find((b) => b.dataset.badgeFor === slug);
    if (badge) {
      badge.textContent = badgeText;
      badge.className = anyInstalled
        ? "shrink-0 text-[10px] px-1.5 py-0.5 rounded bg-emerald-500/10 text-emerald-400"
        : "shrink-0 text-[10px] px-1.5 py-0.5 rounded bg-zinc-700/50 text-zinc-500";
    }

    // Update install button for the active scope
    const scope = this.#installScope(slug);
    const scopeInstalled = scope === "repo" ? repoInstalled : deviceInstalled;
    const repoSelectable = scope !== "repo" || Boolean(this.selectedTargetId);

    const btn = this.installBtnTargets.find((b) => b.dataset.slug === slug);
    if (btn) {
      btn.textContent = scope === "repo" && !repoSelectable
        ? "Select Target"
        : (scopeInstalled ? "Uninstall" : "Install");
      btn.disabled = scope === "repo" && !repoSelectable;
      btn.className = scopeInstalled
        ? "shrink-0 px-4 py-2 text-sm font-medium rounded transition-colors disabled:opacity-50 text-red-400 hover:text-red-300 bg-red-500/10 hover:bg-red-500/20 border border-red-500/30"
        : "shrink-0 px-4 py-2 text-sm font-medium rounded transition-colors disabled:opacity-50 text-emerald-400 hover:text-emerald-300 bg-emerald-500/10 hover:bg-emerald-500/20 border border-emerald-500/30";
    }
  }

  /**
   * Get the currently selected install scope for a template.
   * Reads from the active scope button, or falls back to template @scope metadata.
   */
  /** Extract plugin name from a dest path (e.g., "plugins/github/init.lua" → "github"). */
  #pluginName(dest) {
    const match = dest?.match(/plugins\/([^/]+)\//);
    return match ? match[1] : dest;
  }

  #installScope(slug) {
    const activeBtn = this.scopeBtnTargets.find(
      (b) => b.dataset.slug === slug && b.hasAttribute("data-active"),
    );
    if (activeBtn) return activeBtn.dataset.installScope;

    // Fall back to template default scope from data attribute
    const panel = this.previewPanelTargets.find((p) => p.dataset.slug === slug);
    return panel?.dataset.defaultScope || "device";
  }

  #renderRepoTargetOptions() {
    if (!this.hasRepoTargetPanelTarget || !this.hasRepoTargetSelectTarget) return;

    const select = this.repoTargetSelectTarget;
    select.innerHTML = "";

    const placeholder = document.createElement("option");
    placeholder.value = "";
    placeholder.textContent = this.spawnTargets.length === 0
      ? "No admitted spawn targets"
      : "Choose a spawn target";
    select.appendChild(placeholder);

    this.spawnTargets.forEach((target) => {
      const option = document.createElement("option");
      option.value = target.id;
      option.textContent = target.name || target.path || target.id;
      select.appendChild(option);
    });

    select.value = this.selectedTargetId || "";
    select.disabled = this.spawnTargets.length === 0;

    if (this.hasRepoTargetHintTarget) {
      this.repoTargetHintTarget.textContent = this.selectedTargetId
        ? this.spawnTargets.find((target) => target.id === this.selectedTargetId)?.path || ""
        : (this.spawnTargets.length === 0
          ? "Repo installs are unavailable until the device has an admitted spawn target."
          : "Repo installs and repo template status are scoped to the selected spawn target.");
    }
  }

  #markReady() {
    if (this.targetsLoaded && this.installedStateLoaded) {
      this.element.dataset.hubTemplatesReady = "true";
    }
  }
}
