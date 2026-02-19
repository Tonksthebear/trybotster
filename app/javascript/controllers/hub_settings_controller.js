import { Controller } from "@hotwired/stimulus";
import { ConnectionManager, HubConnection } from "connections";

/**
 * Hub Settings Controller
 *
 * Manages .botster/ config tree editing via E2E encrypted DataChannel to CLI.
 * Dynamically scans the .botster/ directory structure and renders a tree
 * navigation with Shared and Profiles sections.
 *
 * State management:
 *   - Tree panel: data-view = loading | tree | empty | disconnected
 *   - Editor panel: data-editor = empty | loading | editing | creating | error
 *   - Dialog: data-mode = prompt | confirm
 *   - File selection: data-selected attribute on tree buttons
 *   - Flash highlights: data-flash attribute (auto-removed after timeout)
 *
 * All visual state changes are driven by data attributes — CSS handles visibility
 * via group-data-[...] selectors. No classList manipulation.
 */
export default class extends Controller {
  #saveTimer = null;

  static targets = [
    "tabPanel",
    "treePanel",
    "treeContainer",
    "treeFeedback",
    "editorPanel",
    "editorTitle",
    "editor",
    "editorErrorMsg",
    "saveBtn",
    "createBtn",
    "deleteBtn",
    "renameBtn",
    "promptPanel",
    "promptTitle",
    "promptMessage",
    "promptInput",
    "promptError",
  ];

  static values = {
    hubId: String,
    configMetadata: Object,
  };

  connect() {
    if (!this.hubIdValue) return;

    this.unsubscribers = [];
    this.currentFilePath = null;
    this.originalContent = null;
    this.tree = null;
    this.configScope = "repo";       // "repo" or "device"
    this.deviceTree = null;           // cached device tree
    this.repoTree = null;             // cached repo tree

    ConnectionManager.acquire(HubConnection, this.hubIdValue, {
      hubId: this.hubIdValue,
    }).then((hub) => {
      this.hub = hub;

      this.unsubscribers.push(
        this.hub.onConnected(() => {
          this.scanTree();
        }),
      );

      this.unsubscribers.push(
        this.hub.onDisconnected(() => {
          this.treeFeedbackTarget.textContent = "Hub disconnected. Reconnecting...";
          this.treePanelTarget.dataset.view = "disconnected";
        }),
      );
    });
  }

  disconnect() {
    if (this.#saveTimer) {
      clearTimeout(this.#saveTimer);
      this.#saveTimer = null;
    }

    this.unsubscribers?.forEach((unsub) => unsub());
    this.unsubscribers = null;

    const hub = this.hub;
    this.hub = null;
    hub?.release();
  }

  // ========== Actions ==========

  switchTab(event) {
    const tabName = event.currentTarget.dataset.tab;
    if (!tabName) return;

    // Toggle tab button active state
    this.element.querySelectorAll("[data-tab]").forEach((btn) => {
      btn.toggleAttribute("data-active", btn.dataset.tab === tabName);
    });

    // Toggle tab panels
    this.tabPanelTargets.forEach((panel) => {
      panel.classList.toggle("hidden", panel.dataset.tabPanel !== tabName);
    });
  }

  switchConfigScope(event) {
    const scope = event.currentTarget.dataset.configScope;
    if (!scope || scope === this.configScope) return;

    this.configScope = scope;

    // Toggle scope button active state
    this.element.querySelectorAll("[data-config-scope]").forEach((btn) => {
      btn.toggleAttribute("data-active", btn.dataset.configScope === scope);
    });

    // Clear editor when switching scopes
    this.currentFilePath = null;
    this.originalContent = null;
    this.editorPanelTarget.dataset.editor = "empty";
    this.editorTitleTarget.textContent = "Select a file";

    // Use cached tree if available, otherwise scan
    const cached = scope === "device" ? this.deviceTree : this.repoTree;
    if (cached) {
      this.tree = cached;
      this.#renderTree();
    } else {
      this.scanTree();
    }
  }

  async selectFile(event) {
    const filePath = event.currentTarget.dataset.filePath;
    if (!filePath || !this.hub) return;

    this.currentFilePath = filePath;
    this.#highlightSelected(filePath);

    const fsScope = this.#fsScope();
    try {
      const stat = await this.hub.statFile(filePath, fsScope);
      if (!this.hub) return;
      if (stat.exists) {
        await this.#loadFileContent(filePath);
      } else {
        this.#showCreateState(filePath);
      }
    } catch {
      if (!this.hub) return;
      this.#showCreateState(filePath);
    }
  }

  async save() {
    if (!this.currentFilePath || !this.hub) return;

    const content = this.editorTarget.value;
    this.saveBtnTarget.disabled = true;
    this.saveBtnTarget.textContent = "Saving...";

    try {
      await this.hub.writeFile(this.currentFilePath, content, this.#fsScope());
      this.originalContent = content;
      this.#updateDirtyState();
      this.saveBtnTarget.textContent = "Saved";
      this.#saveTimer = setTimeout(() => {
        this.#saveTimer = null;
        if (this.hub) this.saveBtnTarget.textContent = "Save";
      }, 1500);
      this.scanTree();
    } catch (error) {
      this.saveBtnTarget.textContent = "Save";
      this.saveBtnTarget.disabled = false;
      this.#showError(`Save failed: ${error.message}`);
    }
  }

  revert() {
    if (this.originalContent !== null) {
      this.editorTarget.value = this.originalContent;
      this.#updateDirtyState();
    }
  }

  async createFile() {
    if (!this.currentFilePath || !this.hub) return;

    const content = this.editorTarget.value || this.#defaultContent(this.currentFilePath);
    const fsScope = this.#fsScope();

    this.createBtnTarget.textContent = "Creating...";

    try {
      const parentDir = this.currentFilePath.replace(/\/[^/]+$/, "");
      await this.hub.mkDir(parentDir, fsScope).catch(() => {});

      await this.hub.writeFile(this.currentFilePath, content, fsScope);
      this.originalContent = content;
      this.editorPanelTarget.dataset.editor = "editing";
      this.scanTree();
    } catch (error) {
      this.#showError(`Create failed: ${error.message}`);
    } finally {
      this.createBtnTarget.textContent = "Create";
    }
  }

  async deleteFile() {
    if (!this.currentFilePath || !this.hub) return;

    const confirmed = await this.#confirmUser(
      "Delete File",
      `Delete ${this.currentFilePath}?`,
    );
    if (!confirmed) return;

    this.deleteBtnTarget.textContent = "Deleting...";

    try {
      await this.hub.deleteFile(this.currentFilePath, this.#fsScope());
      this.#showCreateState(this.currentFilePath);
      this.scanTree();
    } catch (error) {
      this.#showError(`Delete failed: ${error.message}`);
    } finally {
      this.deleteBtnTarget.textContent = "Delete";
    }
  }

  async renameFile() {
    if (!this.currentFilePath || !this.hub) return;

    const newPath = await this.#promptUser(
      "Rename",
      `Enter the new path:`,
      (val) => {
        if (val.startsWith("/") || val.includes("..")) return "Invalid path";
        if (!/^[a-z0-9._/-]+$/.test(val)) return "Only lowercase letters, numbers, dots, hyphens, slashes, or underscores.";
        return null;
      },
      this.currentFilePath,
    );
    if (!newPath || newPath === this.currentFilePath) return;

    this.renameBtnTarget.textContent = "Renaming...";

    try {
      await this.hub.renameFile(this.currentFilePath, newPath, this.#fsScope());
      this.currentFilePath = newPath;
      this.editorTitleTarget.textContent = newPath;
      await this.scanTree();
      this.#selectFileByPath(newPath);
    } catch (error) {
      this.#showError(`Rename failed: ${error.message}`);
    } finally {
      this.renameBtnTarget.textContent = "Rename";
    }
  }

  async togglePortForward(event) {
    const filePath = event.currentTarget.dataset.filePath;
    const checked = event.currentTarget.checked;

    if (!filePath || !this.hub) return;

    const fsScope = this.#fsScope();

    // Toggle hint visibility
    const hint = this.element.querySelector(`[data-pf-hint="${event.currentTarget.id}"]`);
    if (hint) hint.classList.toggle("hidden", !checked);

    try {
      if (checked) {
        const parentDir = filePath.replace(/\/[^/]+$/, "");
        await this.hub.mkDir(parentDir, fsScope).catch(() => {});
        await this.hub.writeFile(filePath, "", fsScope);
      } else {
        await this.hub.deleteFile(filePath, fsScope);
      }
      this.scanTree();
    } catch (error) {
      event.currentTarget.checked = !checked;
      if (hint) hint.classList.toggle("hidden", checked);
    }
  }

  async addSession(event) {
    const basePath = event.currentTarget.dataset.basePath;
    if (!basePath || !this.hub) return;

    const fsScope = this.#fsScope();

    // Get existing session names in this scope for duplicate checking
    const existingSessions = this.#sessionsForBasePath(basePath);

    const name = await this.#promptUser(
      "Add Session",
      "Enter a name for the new session (lowercase, no spaces):",
      (val) => {
        if (existingSessions.includes(val)) return `Session '${val}' already exists here`;
        return null;
      },
    );
    if (!name) return;

    try {
      const sessionDir = `${basePath}/${name}`;
      await this.hub.mkDir(sessionDir, fsScope);
      const defaultInit = this.configMetadataValue?.session_files?.initialization?.default || "#!/bin/bash\n";
      await this.hub.writeFile(`${sessionDir}/initialization`, defaultInit, fsScope);
      await this.scanTree();
      this.#selectFileByPath(`${sessionDir}/initialization`);
    } catch (error) {
      this.#showError(`Failed to create session: ${error.message}`);
    }
  }

  async addProfile(event) {
    if (!this.hub) return;

    const fsScope = this.#fsScope();
    const prefix = this.configScope === "device" ? "profiles" : ".botster/profiles";

    const name = await this.#promptUser(
      "Add Profile",
      "Enter a name for the new profile (lowercase, no spaces):",
    );
    if (!name) return;

    try {
      await this.hub.mkDir(`${prefix}/${name}`, fsScope);
      await this.hub.mkDir(`${prefix}/${name}/sessions`, fsScope);
      await this.scanTree();
      this.#scrollToProfile(name);
    } catch (error) {
      this.#showError(`Failed to create profile: ${error.message}`);
    }
  }

  async removeProfile(event) {
    const profileName = event.currentTarget.dataset.profileName;
    if (!profileName || !this.hub) return;

    const confirmed = await this.#confirmUser(
      "Remove Profile",
      `Delete profile "${profileName}" and all its sessions? This cannot be undone.`,
    );
    if (!confirmed) return;

    const fsScope = this.#fsScope();
    const prefix = this.configScope === "device" ? "profiles" : ".botster/profiles";

    try {
      await this.hub.rmDir(`${prefix}/${profileName}`, fsScope);
      if (this.currentFilePath?.startsWith(`${prefix}/${profileName}/`)) {
        this.currentFilePath = null;
        this.originalContent = null;
        this.editorPanelTarget.dataset.editor = "empty";
        this.editorTitleTarget.textContent = "Select a file";
      }
      await this.scanTree();
    } catch (error) {
      this.#showError(`Failed to remove profile: ${error.message}`);
    }
  }

  async quickSetup(event) {
    const dest = event.currentTarget.dataset.templateDest;
    const content = event.currentTarget.dataset.templateContent;
    if (!dest || !content || !this.hub) return;

    const btn = event.currentTarget;
    const originalHtml = btn.innerHTML;
    btn.innerHTML = `<span class="text-sm text-zinc-400">Installing...</span>`;
    btn.disabled = true;

    try {
      // Initialize directory structure and write template.
      // dest is relative to the scope root (e.g., "shared/sessions/agent/initialization").
      const parentDir = dest.replace(/\/[^/]+$/, "");
      if (this.configScope === "device") {
        await this.hub.mkDir(parentDir, "device");
        await this.hub.mkDir("profiles", "device");
        await this.hub.writeFile(dest, content, "device");
      } else {
        await this.hub.mkDir(`.botster/${parentDir}`);
        await this.hub.mkDir(".botster/profiles");
        await this.hub.writeFile(`.botster/${dest}`, content);
      }

      this.scanTree();
    } catch (error) {
      btn.innerHTML = originalHtml;
      btn.disabled = false;
      this.#showError(`Setup failed: ${error.message}`);
    }
  }

  onEditorInput() {
    this.#updateDirtyState();
  }

  // ========== Tree Scanning ==========

  async scanTree() {
    const isFirstLoad = !this.tree;
    if (isFirstLoad) {
      this.treeFeedbackTarget.textContent = "Loading configuration...";
      this.treePanelTarget.dataset.view = "loading";
    }

    const scope = this.configScope;
    const fsScope = scope === "device" ? "device" : undefined;

    try {
      const tree = { shared: null, profiles: {} };

      if (scope === "device") {
        // Device scope: root is ~/.botster/, check shared/ and profiles/
        const sharedStat = await this.hub.statFile("shared", fsScope).catch(() => ({ exists: false }));
        if (!sharedStat.exists) {
          // Check if profiles exist at least
          const profilesStat = await this.hub.statFile("profiles", fsScope).catch(() => ({ exists: false }));
          if (!profilesStat.exists) {
            this.treePanelTarget.dataset.view = "empty";
            return;
          }
        }

        if (sharedStat.exists) {
          tree.shared = await this.#scanScopeWithFs("shared", fsScope);
        }

        const profileEntries = await this.#listDirs("profiles", fsScope);
        for (const profileName of profileEntries) {
          tree.profiles[profileName] = await this.#scanScopeWithFs(`profiles/${profileName}`, fsScope);
        }
      } else {
        // Repo scope: root is repo root, check .botster/
        const botsterStat = await this.hub.statFile(".botster").catch(() => ({ exists: false }));
        if (!botsterStat.exists) {
          this.treePanelTarget.dataset.view = "empty";
          return;
        }

        tree.shared = await this.#scanScopeWithFs(".botster/shared", fsScope);

        const profileEntries = await this.#listDirs(".botster/profiles", fsScope);
        for (const profileName of profileEntries) {
          tree.profiles[profileName] = await this.#scanScopeWithFs(`.botster/profiles/${profileName}`, fsScope);
        }
      }

      this.tree = tree;
      // Cache for scope switching
      if (scope === "device") {
        this.deviceTree = tree;
      } else {
        this.repoTree = tree;
      }
      this.#renderTree();
      this.dispatch("configChanged");
    } catch (error) {
      if (isFirstLoad) {
        this.treeFeedbackTarget.textContent = `Failed to scan: ${error.message}`;
      }
    }
  }

  async #scanScopeWithFs(basePath, fsScope) {
    const scope = { files: {}, sessions: {}, plugins: {} };

    // Scan workspace files, session dirs, and plugin dirs in parallel
    const [, , sessionNames, pluginNames] = await Promise.all([
      ...["workspace_include", "workspace_teardown"].map(async (fileName) => {
        const stat = await this.hub.statFile(`${basePath}/${fileName}`, fsScope).catch(() => ({ exists: false }));
        scope.files[fileName] = stat.exists;
      }),
      this.#listDirs(`${basePath}/sessions`, fsScope),
      this.#listDirs(`${basePath}/plugins`, fsScope),
    ]);

    // Scan sessions (init + port_forward per session)
    await Promise.all(
      sessionNames.map(async (sessionName) => {
        const sessionPath = `${basePath}/sessions/${sessionName}`;
        const [initStat, pfStat] = await Promise.all([
          this.hub.statFile(`${sessionPath}/initialization`, fsScope).catch(() => ({ exists: false })),
          this.hub.statFile(`${sessionPath}/port_forward`, fsScope).catch(() => ({ exists: false })),
        ]);
        scope.sessions[sessionName] = {
          initialization: initStat.exists,
          port_forward: pfStat.exists,
        };
      }),
    );

    // Scan plugins (only include plugins that have init.lua)
    await Promise.all(
      pluginNames.map(async (pluginName) => {
        const initStat = await this.hub
          .statFile(`${basePath}/plugins/${pluginName}/init.lua`, fsScope)
          .catch(() => ({ exists: false }));
        if (initStat.exists) {
          scope.plugins[pluginName] = { init: true };
        }
      }),
    );

    return scope;
  }

  async #listDirs(path, fsScope) {
    try {
      const result = await this.hub.listDir(path, fsScope);
      return (result.entries || [])
        .filter((e) => e.type === "dir")
        .map((e) => e.name)
        .sort();
    } catch {
      return [];
    }
  }

  // ========== Tree Rendering ==========

  async initBotster() {
    if (!this.hub) return;

    const fsScope = this.#fsScope();

    try {
      if (this.configScope === "device") {
        // Device: create shared/ and profiles/ under ~/.botster/
        await this.hub.mkDir("shared/sessions/agent", fsScope);
        await this.hub.mkDir("profiles", fsScope);
        const defaultInit = this.configMetadataValue?.session_files?.initialization?.default || "#!/bin/bash\n";
        await this.hub.writeFile("shared/sessions/agent/initialization", defaultInit, fsScope);
      } else {
        // Repo: create .botster/ structure
        await this.hub.mkDir(".botster/shared/sessions/agent");
        await this.hub.mkDir(".botster/profiles");
        const defaultInit = this.configMetadataValue?.session_files?.initialization?.default || "#!/bin/bash\n";
        await this.hub.writeFile(".botster/shared/sessions/agent/initialization", defaultInit);
      }
      this.scanTree();
    } catch (error) {
      this.#showError(`Failed to initialize: ${error.message}`);
    }
  }

  #renderTree() {
    const container = this.treeContainerTarget;
    container.innerHTML = "";

    const isDevice = this.configScope === "device";
    const sharedBase = isDevice ? "shared" : ".botster/shared";
    const profilesBase = isDevice ? "profiles" : ".botster/profiles";

    // Shared section
    if (this.tree.shared) {
      container.appendChild(this.#renderSection("Shared", sharedBase, this.tree.shared));
    }

    // Profiles section
    const profileNames = Object.keys(this.tree.profiles).sort();
    if (profileNames.length > 0) {
      const header = document.createElement("div");
      header.className = "mt-2";
      header.innerHTML = `<h2 class="text-sm font-medium text-zinc-400 uppercase tracking-wider mb-3">Profiles</h2>`;
      container.appendChild(header);

      for (const name of profileNames) {
        container.appendChild(
          this.#renderSection(
            this.#capitalize(name),
            `${profilesBase}/${name}`,
            this.tree.profiles[name],
            { sharedScope: this.tree.shared, profileName: name },
          ),
        );
      }
    }

    // Add profile button
    const addBtn = document.createElement("button");
    addBtn.type = "button";
    addBtn.className = "w-full mt-2 px-3 py-2 text-xs font-medium text-zinc-500 hover:text-zinc-300 border border-dashed border-zinc-700 hover:border-zinc-600 rounded-lg transition-colors";
    addBtn.textContent = "+ Add Profile";
    addBtn.dataset.action = "hub-settings#addProfile";
    container.appendChild(addBtn);

    this.treePanelTarget.dataset.view = "tree";

    // Re-apply selection highlight after tree rebuild
    if (this.currentFilePath) {
      this.#highlightSelected(this.currentFilePath);
    }
  }

  #renderSection(title, basePath, scope, options = {}) {
    const { sharedScope, profileName } = options;
    const isProfile = !!profileName;

    const section = document.createElement("div");
    section.className = "mb-3 group/section data-[flash]:ring-1 data-[flash]:ring-primary-500/30 data-[flash]:rounded-lg";

    if (isProfile) {
      section.dataset.profile = profileName;
    }

    // Section header — profiles get a remove button
    const headerDiv = document.createElement("div");
    headerDiv.className = "flex items-center justify-between mb-2";
    headerDiv.innerHTML = `<h3 class="text-xs font-medium text-zinc-500 uppercase tracking-wider">${this.#escapeHtml(title)}</h3>`;

    if (isProfile) {
      const removeBtn = document.createElement("button");
      removeBtn.type = "button";
      removeBtn.className =
        "text-zinc-700 hover:text-red-400 transition-colors opacity-0 group-hover/section:opacity-100";
      removeBtn.title = "Remove profile";
      removeBtn.dataset.action = "hub-settings#removeProfile";
      removeBtn.dataset.profileName = profileName;
      removeBtn.innerHTML = `<svg class="size-3.5" viewBox="0 0 20 20" fill="currentColor">
        <path fill-rule="evenodd" d="M8.75 1A2.75 2.75 0 006 3.75v.443c-.795.077-1.584.176-2.365.298a.75.75 0 10.23 1.482l.149-.022.841 10.518A2.75 2.75 0 007.596 19h4.807a2.75 2.75 0 002.742-2.53l.841-10.52.149.023a.75.75 0 00.23-1.482A41.03 41.03 0 0014 4.193V3.75A2.75 2.75 0 0011.25 1h-2.5zM10 4c.84 0 1.673.025 2.5.075V3.75c0-.69-.56-1.25-1.25-1.25h-2.5c-.69 0-1.25.56-1.25 1.25v.325C8.327 4.025 9.16 4 10 4zM8.58 7.72a.75.75 0 00-1.5.06l.3 7.5a.75.75 0 101.5-.06l-.3-7.5zm4.34.06a.75.75 0 10-1.5-.06l-.3 7.5a.75.75 0 101.5.06l.3-7.5z" clip-rule="evenodd"/>
      </svg>`;
      headerDiv.appendChild(removeBtn);
    }

    section.appendChild(headerDiv);

    const list = document.createElement("div");
    list.className = "space-y-1";

    // Workspace files
    for (const [fileName, exists] of Object.entries(scope.files)) {
      const status = this.#fileStatus(exists, sharedScope?.files?.[fileName]);
      list.appendChild(this.#renderFileEntry(`${basePath}/${fileName}`, fileName, status));
    }

    // Sessions
    const sessionNames = Object.keys(scope.sessions).sort((a, b) => {
      if (a === "agent") return -1;
      if (b === "agent") return 1;
      return a.localeCompare(b);
    });

    if (sessionNames.length > 0) {
      const sessHeader = document.createElement("div");
      sessHeader.className = "mt-2 mb-1";
      sessHeader.innerHTML = `<span class="text-xs text-zinc-600 uppercase tracking-wider">Sessions</span>`;
      list.appendChild(sessHeader);

      for (const sessionName of sessionNames) {
        const session = scope.sessions[sessionName];
        const sessionPath = `${basePath}/sessions/${sessionName}`;
        const sharedSession = sharedScope?.sessions?.[sessionName];

        const initStatus = this.#fileStatus(session.initialization, sharedSession?.initialization);
        list.appendChild(
          this.#renderFileEntry(`${sessionPath}/initialization`, `${sessionName}/initialization`, initStatus),
        );

        list.appendChild(
          this.#renderPortForwardToggle(`${sessionPath}/port_forward`, sessionName, session.port_forward),
        );
      }
    }

    // Plugins
    const pluginNames = Object.keys(scope.plugins || {}).sort();
    if (pluginNames.length > 0) {
      const plugHeader = document.createElement("div");
      plugHeader.className = "mt-2 mb-1";
      plugHeader.innerHTML = `<span class="text-xs text-zinc-600 uppercase tracking-wider">Plugins</span>`;
      list.appendChild(plugHeader);

      for (const pluginName of pluginNames) {
        const plugin = scope.plugins[pluginName];
        const pluginPath = `${basePath}/plugins/${pluginName}/init.lua`;
        const sharedPlugin = sharedScope?.plugins?.[pluginName];
        const status = this.#fileStatus(plugin.init, sharedPlugin?.init);
        list.appendChild(this.#renderFileEntry(pluginPath, `${pluginName}/init.lua`, status));
      }
    }

    // Agent session warning — resolved config must include agent
    const hasAgent = !!scope.sessions?.agent;
    const hasAgentInShared = !!sharedScope?.sessions?.agent;
    if (isProfile && !hasAgent && !hasAgentInShared) {
      const warning = document.createElement("p");
      warning.className = "text-xs text-amber-400 mt-2 px-2.5";
      warning.textContent = "Missing agent session — required for this profile to work";
      list.appendChild(warning);
    } else if (!isProfile && !hasAgent) {
      const warning = document.createElement("p");
      warning.className = "text-xs text-amber-400 mt-2 px-2.5";
      warning.textContent = "No agent session — profiles without their own will not work";
      list.appendChild(warning);
    }

    // Add session button
    const addSessionBtn = document.createElement("button");
    addSessionBtn.type = "button";
    addSessionBtn.className = "w-full mt-1 px-2 py-1.5 text-xs text-zinc-600 hover:text-zinc-400 transition-colors text-left";
    addSessionBtn.textContent = "+ Add session";
    addSessionBtn.dataset.action = "hub-settings#addSession";
    addSessionBtn.dataset.basePath = `${basePath}/sessions`;
    list.appendChild(addSessionBtn);

    section.appendChild(list);
    return section;
  }

  /**
   * Determine the display status for a file entry.
   * @param {boolean} exists - Whether the file exists in this scope
   * @param {boolean} [existsInShared] - Whether the file exists in shared (undefined for shared scope)
   * @returns {"exists"|"override"|"inherited"|"missing"}
   */
  #fileStatus(exists, existsInShared) {
    if (exists) return existsInShared !== undefined ? "override" : "exists";
    if (existsInShared) return "inherited";
    return "missing";
  }

  #renderFileEntry(filePath, label, status) {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className =
      "w-full text-left px-2.5 py-1.5 rounded border border-zinc-700/50 hover:border-zinc-700 hover:bg-zinc-800/50 transition-colors " +
      "data-[selected]:bg-zinc-800/50 data-[selected]:border-primary-500/30 " +
      "data-[flash]:ring-1 data-[flash]:ring-primary-500/50";
    btn.dataset.action = "hub-settings#selectFile";
    btn.dataset.filePath = filePath;

    const styles = {
      exists: "bg-emerald-500/10 text-emerald-400",
      override: "bg-amber-500/10 text-amber-400",
      inherited: "bg-sky-500/10 text-sky-400",
      missing: "bg-zinc-700/50 text-zinc-500",
    };

    btn.innerHTML = `
      <div class="flex items-center justify-between">
        <span class="text-xs font-mono text-zinc-300 truncate">${this.#escapeHtml(label)}</span>
        <span class="shrink-0 ml-2 text-[10px] px-1.5 py-0.5 rounded ${styles[status] || styles.missing}">${status}</span>
      </div>
    `;

    return btn;
  }

  #renderPortForwardToggle(filePath, sessionName, enabled) {
    const div = document.createElement("div");
    div.className = "flex items-center justify-between px-2.5 py-1";

    const id = `pf-${filePath.replace(/[/.]/g, "-")}`;
    div.innerHTML = `
      <div class="flex items-center gap-2">
        <label for="${id}" class="text-xs text-zinc-500 cursor-pointer">${this.#escapeHtml(sessionName)}/port_forward</label>
        <span class="text-[10px] text-emerald-400/70 ${enabled ? "" : "hidden"}" data-pf-hint="${id}">$PORT available</span>
      </div>
      <div class="group relative inline-flex w-9 shrink-0 rounded-full p-0.5
                  bg-white/5 inset-ring inset-ring-white/10
                  has-checked:bg-primary-500
                  transition-colors duration-200 ease-in-out
                  outline-offset-2 outline-primary-500 has-focus-visible:outline-2">
        <span class="size-4 rounded-full bg-white shadow-xs ring-1 ring-gray-900/5
                     transition-transform duration-200 ease-in-out
                     group-has-checked:translate-x-4"></span>
        <input type="checkbox" id="${id}"
               ${enabled ? "checked" : ""}
               data-action="hub-settings#togglePortForward"
               data-file-path="${this.#escapeAttr(filePath)}"
               class="absolute inset-0 appearance-none cursor-pointer focus:outline-hidden">
      </div>
    `;

    return div;
  }

  // ========== Editor State ==========

  async #loadFileContent(filePath) {
    this.editorPanelTarget.dataset.editor = "loading";

    try {
      const result = await this.hub.readFile(filePath, this.#fsScope());
      if (!this.hub) return;
      this.originalContent = result.content;
      this.editorTarget.value = result.content;
      this.editorTitleTarget.textContent = filePath;
      this.editorPanelTarget.dataset.editor = "editing";
      this.#updateDirtyState();
    } catch (error) {
      if (!this.hub) return;
      this.#showError(`Read failed: ${error.message}`);
    }
  }

  #showCreateState(filePath) {
    this.editorTitleTarget.textContent = filePath;
    this.editorTarget.value = this.#defaultContent(filePath);
    this.originalContent = null;
    this.editorPanelTarget.dataset.editor = "creating";
  }

  #showError(message) {
    this.editorErrorMsgTarget.textContent = message;
    this.editorPanelTarget.dataset.editor = "error";
  }

  #updateDirtyState() {
    const isDirty =
      this.originalContent !== null &&
      this.editorTarget.value !== this.originalContent;

    this.saveBtnTarget.disabled = !isDirty;
  }

  #highlightSelected(filePath) {
    this.treeContainerTarget.querySelectorAll("button[data-file-path]").forEach((el) => {
      el.toggleAttribute("data-selected", el.dataset.filePath === filePath);
    });
  }

  // ========== Prompt / Confirm Dialog ==========

  async #promptUser(title, message, validate, defaultValue) {
    return this.#openDialog(title, message, { mode: "prompt", validate, defaultValue });
  }

  async #confirmUser(title, message) {
    const result = await this.#openDialog(title, message, { mode: "confirm" });
    return result !== null;
  }

  async #openDialog(title, message, { mode, validate, defaultValue }) {
    const dialog = document.getElementById("settings-prompt-modal");
    if (!dialog) return null;

    this.promptPanelTarget.dataset.mode = mode;
    this.promptTitleTarget.textContent = title;
    this.promptMessageTarget.textContent = message;
    this.promptInputTarget.value = defaultValue || "";
    this.promptErrorTarget.textContent = "";

    this._promptMode = mode;
    this._promptValidate = validate || null;

    return new Promise((resolve) => {
      this._promptResolve = resolve;
      dialog.showModal();
      if (mode === "prompt") {
        requestAnimationFrame(() => this.promptInputTarget.focus());
      }

      dialog.addEventListener(
        "close",
        () => {
          if (this._promptResolve) {
            this._promptResolve(null);
            this._promptResolve = null;
          }
        },
        { once: true },
      );
    });
  }

  promptConfirm() {
    if (this._promptMode === "confirm") {
      const resolve = this._promptResolve;
      this._promptResolve = null;
      document.getElementById("settings-prompt-modal")?.close();
      if (resolve) resolve(true);
      return;
    }

    const value = this.promptInputTarget.value.trim();

    if (!value) {
      this.promptErrorTarget.textContent = "Value is required.";
      return;
    }

    if (this._promptValidate) {
      const error = this._promptValidate(value);
      if (error) {
        this.promptErrorTarget.textContent = error;
        return;
      }
    } else if (!/^[a-z][a-z0-9_-]*$/.test(value)) {
      // Default strict naming validation (for session/profile names)
      this.promptErrorTarget.textContent =
        "Must start with a letter. Only lowercase letters, numbers, hyphens, or underscores.";
      return;
    }

    const resolve = this._promptResolve;
    this._promptResolve = null;
    this._promptValidate = null;
    document.getElementById("settings-prompt-modal")?.close();
    if (resolve) resolve(value);
  }

  // ========== Feedback Helpers ==========

  #selectFileByPath(filePath) {
    const btn = this.treeContainerTarget.querySelector(
      `button[data-file-path="${CSS.escape(filePath)}"]`,
    );
    if (btn) {
      this.#highlightSelected(filePath);
      btn.scrollIntoView({ behavior: "smooth", block: "nearest" });
      btn.toggleAttribute("data-flash", true);
      setTimeout(() => btn.removeAttribute("data-flash"), 1500);
    }
  }

  #scrollToProfile(name) {
    const section = this.treeContainerTarget.querySelector(
      `[data-profile="${CSS.escape(name)}"]`,
    );
    if (section) {
      section.scrollIntoView({ behavior: "smooth", block: "nearest" });
      section.toggleAttribute("data-flash", true);
      setTimeout(() => section.removeAttribute("data-flash"), 1500);
    }
  }

  // ========== Helpers ==========

  #defaultContent(filePath) {
    const meta = this.configMetadataValue || {};

    if (filePath.endsWith("/workspace_include")) {
      return meta.shared_files?.workspace_include?.default || "";
    }
    if (filePath.endsWith("/workspace_teardown")) {
      return meta.shared_files?.workspace_teardown?.default || "";
    }
    if (filePath.endsWith("/initialization")) {
      return meta.session_files?.initialization?.default || "#!/bin/bash\n";
    }
    return "";
  }

  /**
   * Get existing session names for a sessions/ basePath by looking up the tree data.
   * Repo scope: ".botster/shared/sessions" or ".botster/profiles/standard/sessions"
   * Device scope: "shared/sessions" or "profiles/standard/sessions"
   */
  #sessionsForBasePath(basePath) {
    if (!this.tree) return [];

    // Match shared sessions (repo or device path format)
    if (basePath.match(/(?:^|\.botster\/)shared\//)) {
      return Object.keys(this.tree.shared?.sessions || {});
    }

    // Match profile sessions (repo or device path format)
    const match = basePath.match(/(?:\.botster\/)?profiles\/([^/]+)\//);
    if (match) {
      return Object.keys(this.tree.profiles?.[match[1]]?.sessions || {});
    }
    return [];
  }

  /** Return the fs scope string to pass to hub methods. */
  #fsScope() {
    return this.configScope === "device" ? "device" : undefined;
  }

  #capitalize(str) {
    return str.charAt(0).toUpperCase() + str.slice(1);
  }

  #escapeHtml(text) {
    const div = document.createElement("div");
    div.textContent = text;
    return div.innerHTML;
  }

  #escapeAttr(text) {
    return text.replace(/"/g, "&quot;").replace(/'/g, "&#39;");
  }
}
