import { Controller } from "@hotwired/stimulus";
import { HubManager } from "connections";

export default class extends Controller {
  static targets = [
    "emptyState",
    "feedback",
    "filterInput",
    "list",
    "nameInput",
    "pathInput",
    "pathSuggestions",
    "selectedHint",
    "selectedName",
    "selectedPath",
  ];

  static values = {
    currentHubContext: Object,
    hubId: String,
    homePath: String,
    selectedTargetId: String,
    targets: Array,
  };

  connect() {
    this.targetsState = (this.targetsValue || []).map((target, index) =>
      this.#normalizeTarget(target, index),
    );

    if (!this.selectedTargetIdValue && this.targetsState[0]) {
      this.selectedTargetIdValue = this.targetsState[0].id;
    }

    this.#render();
    this._pathBrowseToken = 0;
    this._pathBrowseTimer = null;

    if (!this.hubIdValue) return;

    this.unsubscribers = [];
    HubManager.acquire(this.hubIdValue).then((hub) => {
      this.hub = hub;
      this.targetsState = (Array.isArray(hub.spawnTargets) ? hub.spawnTargets : []).map((target, index) =>
        this.#normalizeTarget(target, index),
      );
      if (!this.targetsState.some((target) => target.id === this.selectedTargetIdValue)) {
        this.selectedTargetIdValue = this.targetsState[0]?.id || "";
      }
      this.#render();

      this.unsubscribers.push(
        hub.onSpawnTargetList((targets) => {
          this.targetsState = (Array.isArray(targets) ? targets : []).map((target, index) =>
            this.#normalizeTarget(target, index),
          );
          if (!this.targetsState.some((target) => target.id === this.selectedTargetIdValue)) {
            this.selectedTargetIdValue = this.targetsState[0]?.id || "";
          }
          this.#render();
        }),
      );

      this.unsubscribers.push(
        hub.on("spawnTargetFeedback", ({ tone, message }) => {
          this.#setFeedback(message, tone === "error" ? "error" : tone === "success" ? "success" : "neutral");
        }),
      );
    });
  }

  disconnect() {
    if (this._pathBrowseTimer) {
      clearTimeout(this._pathBrowseTimer);
      this._pathBrowseTimer = null;
    }

    this.unsubscribers?.forEach((unsub) => unsub());
    this.unsubscribers = null;

    const hub = this.hub;
    this.hub = null;
    hub?.release();
  }

  filter() {
    this.#render();
  }

  pathInputChanged() {
    if (this._pathBrowseTimer) {
      clearTimeout(this._pathBrowseTimer);
    }

    this._pathBrowseTimer = setTimeout(() => {
      this.refreshPathSuggestions();
    }, 120);
  }

  async refreshPathSuggestions() {
    if (!this.hub || !this.hasPathInputTarget || !this.hasPathSuggestionsTarget) return;

    const context = this.#browseContext(this.pathInputTarget.value);
    if (!context) {
      this.#clearPathSuggestions();
      return;
    }

    const token = ++this._pathBrowseToken;

    try {
      const result = await this.hub.browseHostDir(context.directory, true);
      if (token !== this._pathBrowseToken) return;

      const entries = Array.isArray(result.entries) ? result.entries : [];
      const suggestions = entries
        .filter((entry) => entry?.type === "directory")
        .filter((entry) => {
          if (!context.fragment) return true;
          return entry.name.toLowerCase().startsWith(context.fragment.toLowerCase());
        })
        .sort((a, b) => a.name.localeCompare(b.name))
        .slice(0, 25)
        .map((entry) => this.#joinBrowsePath(context.directory, entry.name));

      this.#renderPathSuggestions(suggestions);
    } catch (_error) {
      if (token !== this._pathBrowseToken) return;
      this.#clearPathSuggestions();
    }
  }

  admitTarget(event) {
    event?.preventDefault();

    const rawPath = this.hasPathInputTarget ? this.pathInputTarget.value.trim() : "";
    const path = this.#normalizePath(rawPath);

    if (!path || !path.startsWith("/")) {
      this.#setFeedback("Enter an absolute path for the spawn target.", "error");
      return;
    }

    const name = this.hasNameInputTarget && this.nameInputTarget.value.trim()
      ? this.nameInputTarget.value.trim()
      : this.#defaultNameFromPath(path);
    if (!this.hub) {
      this.#setFeedback("Hub is not ready yet.", "error");
      return;
    }

    this.#setFeedback(`Admitting ${path}...`, "neutral");
    this.hub.addSpawnTarget(path, name);

    if (this.hasNameInputTarget) {
      this.nameInputTarget.value = "";
    }
    if (this.hasPathInputTarget) {
      this.pathInputTarget.value = "";
    }
    this.#clearPathSuggestions();
  }

  select(event) {
    const { targetId } = event.currentTarget.dataset;
    if (!targetId) return;

    this.selectedTargetIdValue = targetId;
    this.#render();
  }

  removeTarget(event) {
    event.preventDefault();
    event.stopPropagation();

    const { targetId } = event.currentTarget.dataset;
    if (!targetId) return;

    if (!this.hub) {
      this.#setFeedback("Hub is not ready yet.", "error");
      return;
    }

    this.#setFeedback("Removing spawn target...", "neutral");
    this.hub.removeSpawnTarget(targetId);
  }

  #render() {
    if (!this.hasListTarget) return;

    const selected = this.#selectedTarget();
    const query = this.hasFilterInputTarget ? this.filterInputTarget.value.trim().toLowerCase() : "";
    const visibleTargets = this.targetsState.filter((target) => {
      if (!query) return true;

      return [ target.name, target.path ]
        .filter(Boolean)
        .some((value) => value.toLowerCase().includes(query));
    });

    this.listTarget.innerHTML = "";

    visibleTargets.forEach((target) => {
      this.listTarget.appendChild(this.#buildTargetCard(target, target.id === selected?.id));
    });

    if (this.hasEmptyStateTarget) {
      this.emptyStateTarget.classList.toggle("hidden", visibleTargets.length > 0);
    }

    this.#renderSelectedSummary(selected);
  }

  #buildTargetCard(target, selected) {
    const wrapper = document.createElement("div");
    wrapper.className = [
      "rounded-lg border px-4 py-3 transition-colors",
      selected
        ? "border-primary-500/50 bg-primary-500/10"
        : "border-zinc-800 bg-zinc-950/60 hover:border-zinc-700 hover:bg-zinc-950",
    ].join(" ");

    const button = document.createElement("button");
    button.type = "button";
    button.dataset.action = "spawn-target-browser#select";
    button.dataset.targetId = target.id;
    button.className = "w-full text-left";

    const header = document.createElement("div");
    header.className = "flex items-start justify-between gap-3";

    const title = document.createElement("div");
    title.className = "min-w-0";

    const name = document.createElement("p");
    name.className = "text-sm font-medium text-zinc-100 truncate";
    name.textContent = target.name;
    title.appendChild(name);

    const path = document.createElement("p");
    path.className = "text-xs text-zinc-500 mt-1 font-mono break-all";
    path.textContent = target.path;
    title.appendChild(path);

    header.appendChild(title);
    header.appendChild(this.#buildStatusBadge(target));
    button.appendChild(header);

    const capabilities = document.createElement("div");
    capabilities.className = "mt-3 flex flex-wrap gap-2";
    target.capabilities.forEach((label) => {
      const pill = document.createElement("span");
      pill.className = "inline-flex items-center rounded-full border border-zinc-700 bg-zinc-900 px-2.5 py-1 text-[11px] text-zinc-400";
      pill.textContent = label;
      capabilities.appendChild(pill);
    });
    button.appendChild(capabilities);
    wrapper.appendChild(button);

    const footer = document.createElement("div");
    footer.className = "mt-3 flex items-center justify-between gap-3";

    const note = document.createElement("p");
    note.className = "text-[11px] text-zinc-500";
    note.textContent = target.enabled === false ? "Disabled target" : "Admitted target";
    footer.appendChild(note);

    const remove = document.createElement("button");
    remove.type = "button";
    remove.dataset.action = "spawn-target-browser#removeTarget";
    remove.dataset.targetId = target.id;
    remove.className = "text-xs text-zinc-500 hover:text-red-300 transition-colors";
    remove.textContent = "Remove";
    footer.appendChild(remove);

    wrapper.appendChild(footer);

    return wrapper;
  }

  #buildStatusBadge(target) {
    const badge = document.createElement("span");
    const toneClass = {
      draft: "border-amber-500/20 bg-amber-500/10 text-amber-300",
      live: "border-emerald-500/20 bg-emerald-500/10 text-emerald-300",
      disabled: "border-zinc-700 bg-zinc-800 text-zinc-400",
    }[target.statusTone] || "border-zinc-700 bg-zinc-800 text-zinc-400";

    badge.className = `inline-flex items-center rounded-full border px-2.5 py-1 text-[11px] font-medium ${toneClass}`;
    badge.textContent = target.statusLabel;
    return badge;
  }

  #renderSelectedSummary(target) {
    if (!this.hasSelectedNameTarget || !this.hasSelectedPathTarget || !this.hasSelectedHintTarget) {
      return;
    }

    if (!target) {
      this.selectedNameTarget.textContent = "No target selected";
      this.selectedPathTarget.textContent = "Admitted targets are device-scoped and immediately available to spawn flows.";
      this.selectedHintTarget.textContent = this.currentHubContextValue?.integration_note
        || "Admitted targets are now the required input for spawn, settings, and template flows.";
      return;
    }

    this.selectedNameTarget.textContent = target.name;
    this.selectedPathTarget.textContent = target.path;
    this.selectedHintTarget.textContent = "Admitted target selected. Runtime actions now require explicit target selection.";
  }

  #selectedTarget() {
    return this.targetsState.find((target) => target.id === this.selectedTargetIdValue) || null;
  }

  #normalizeTarget(target, index) {
    const path = this.#normalizePath(target?.path || "");

    return {
      id: target?.id || `target:${index}`,
      name: target?.name || this.#defaultNameFromPath(path),
      path,
      draft: Boolean(target?.draft),
      enabled: target?.enabled !== false,
      statusLabel: target?.statusLabel || (target?.enabled === false ? "Disabled" : "Admitted"),
      statusTone: target?.statusTone || (target?.enabled === false ? "disabled" : "live"),
      capabilities: Array.isArray(target?.capabilities) && target.capabilities.length > 0
        ? target.capabilities
        : this.#buildCapabilities(target),
    };
  }

  #normalizePath(path) {
    if (!path) return "";
    if (path === "/") return path;
    return path.replace(/\/+$/, "");
  }

  #defaultNameFromPath(path) {
    if (!path) return "Untitled target";
    return path.split("/").filter(Boolean).pop() || path;
  }

  #buildCapabilities(target) {
    const capabilities = [];
    capabilities.push(target?.is_git_repo ? "Git: ready" : "Git: not detected");
    capabilities.push(target?.has_botster_dir ? ".botster: present" : ".botster: not detected");
    if (target?.current_branch) {
      capabilities.push(`Branch: ${target.current_branch}`);
    }
    return capabilities;
  }

  #setFeedback(message, tone) {
    if (!this.hasFeedbackTarget) return;

    this.feedbackTarget.textContent = message;
    this.feedbackTarget.className = "text-xs mt-2 min-h-4";

    if (tone === "error") {
      this.feedbackTarget.classList.add("text-red-300");
    } else if (tone === "success") {
      this.feedbackTarget.classList.add("text-emerald-300");
    } else {
      this.feedbackTarget.classList.add("text-zinc-500");
    }
  }

  #browseContext(rawInput) {
    const raw = rawInput?.trim() || "";
    const fallbackDirectory = this.homePathValue || "/";

    if (!raw) {
      return { directory: fallbackDirectory, fragment: "" };
    }

    if (!raw.startsWith("/")) {
      return null;
    }

    if (raw === "/") {
      return { directory: "/", fragment: "" };
    }

    if (raw.endsWith("/")) {
      return { directory: this.#normalizePath(raw), fragment: "" };
    }

    const lastSlash = raw.lastIndexOf("/");
    if (lastSlash < 0) return null;

    const directory = lastSlash === 0 ? "/" : this.#normalizePath(raw.slice(0, lastSlash));
    const fragment = raw.slice(lastSlash + 1);
    return { directory, fragment };
  }

  #joinBrowsePath(directory, name) {
    return directory === "/" ? `/${name}/` : `${directory}/${name}/`;
  }

  #renderPathSuggestions(paths) {
    if (!this.hasPathSuggestionsTarget) return;

    this.pathSuggestionsTarget.innerHTML = "";
    paths.forEach((path) => {
      const option = document.createElement("option");
      option.value = path;
      this.pathSuggestionsTarget.appendChild(option);
    });
  }

  #clearPathSuggestions() {
    if (!this.hasPathSuggestionsTarget) return;
    this.pathSuggestionsTarget.innerHTML = "";
  }
}
