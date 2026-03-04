import { Controller } from "@hotwired/stimulus";
import { HubConnectionManager, HubConnection } from "connections";

/**
 * Hub Settings Controller
 *
 * Manages .botster/ config tree editing via E2E encrypted DataChannel to CLI.
 * Dynamically scans the .botster/ directory structure and renders a tree
 * navigation with Agents, Accessories, Workspaces, and Plugins sections.
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

    HubConnectionManager.acquire(HubConnection, this.hubIdValue, {
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

  async addAgent() {
    if (!this.hub) return;

    const fsScope = this.#fsScope();
    const prefix = this.#configPrefix();

    const name = await this.#promptUser(
      "Add Agent",
      "Enter a name for the new agent (lowercase, no spaces):",
    );
    if (!name) return;

    try {
      await this.hub.mkDir(`${prefix}agents/${name}`, fsScope);
      const defaultInit = this.configMetadataValue?.session_files?.initialization?.default || "#!/bin/bash\n";
      await this.hub.writeFile(`${prefix}agents/${name}/initialization`, defaultInit, fsScope);
      await this.scanTree();
      this.#scrollToSection(`agents-${name}`);
    } catch (error) {
      this.#showError(`Failed to create agent: ${error.message}`);
    }
  }

  async removeAgent(event) {
    const agentName = event.currentTarget.dataset.agentName;
    if (!agentName || !this.hub) return;

    const confirmed = await this.#confirmUser(
      "Remove Agent",
      `Delete agent "${agentName}" and its configuration? This cannot be undone.`,
    );
    if (!confirmed) return;

    const fsScope = this.#fsScope();
    const prefix = this.#configPrefix();

    try {
      await this.hub.rmDir(`${prefix}agents/${agentName}`, fsScope);
      if (this.currentFilePath?.includes(`/agents/${agentName}/`)) {
        this.currentFilePath = null;
        this.originalContent = null;
        this.editorPanelTarget.dataset.editor = "empty";
        this.editorTitleTarget.textContent = "Select a file";
      }
      await this.scanTree();
    } catch (error) {
      this.#showError(`Failed to remove agent: ${error.message}`);
    }
  }

  async addAccessory() {
    if (!this.hub) return;

    const fsScope = this.#fsScope();
    const prefix = this.#configPrefix();

    const name = await this.#promptUser(
      "Add Accessory",
      "Enter a name for the new accessory (lowercase, no spaces):",
    );
    if (!name) return;

    try {
      await this.hub.mkDir(`${prefix}accessories/${name}`, fsScope);
      const defaultInit = this.configMetadataValue?.session_files?.initialization?.default || "#!/bin/bash\n";
      await this.hub.writeFile(`${prefix}accessories/${name}/initialization`, defaultInit, fsScope);
      await this.scanTree();
      this.#scrollToSection(`accessories-${name}`);
    } catch (error) {
      this.#showError(`Failed to create accessory: ${error.message}`);
    }
  }

  async removeAccessory(event) {
    const accessoryName = event.currentTarget.dataset.accessoryName;
    if (!accessoryName || !this.hub) return;

    const confirmed = await this.#confirmUser(
      "Remove Accessory",
      `Delete accessory "${accessoryName}" and its configuration? This cannot be undone.`,
    );
    if (!confirmed) return;

    const fsScope = this.#fsScope();
    const prefix = this.#configPrefix();

    try {
      await this.hub.rmDir(`${prefix}accessories/${accessoryName}`, fsScope);
      if (this.currentFilePath?.includes(`/accessories/${accessoryName}/`)) {
        this.currentFilePath = null;
        this.originalContent = null;
        this.editorPanelTarget.dataset.editor = "empty";
        this.editorTitleTarget.textContent = "Select a file";
      }
      await this.scanTree();
    } catch (error) {
      this.#showError(`Failed to remove accessory: ${error.message}`);
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
      // dest is relative to the scope root (e.g., "agents/claude/initialization").
      const parentDir = dest.replace(/\/[^/]+$/, "");
      if (this.configScope === "device") {
        await this.hub.mkDir(parentDir, "device");
        await this.hub.writeFile(dest, content, "device");
      } else {
        await this.hub.mkDir(`.botster/${parentDir}`);
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
      const tree = { agents: {}, accessories: {}, workspaces: {}, plugins: {}, files: {} };
      const prefix = scope === "device" ? "" : ".botster/";

      // Check if config root exists
      if (scope === "device") {
        // Device scope: check if agents/ or any config dir exists
        const [agentsStat, accessoriesStat, pluginsStat, workspacesStat] = await Promise.all([
          this.hub.statFile("agents", fsScope).catch(() => ({ exists: false })),
          this.hub.statFile("accessories", fsScope).catch(() => ({ exists: false })),
          this.hub.statFile("plugins", fsScope).catch(() => ({ exists: false })),
          this.hub.statFile("workspaces", fsScope).catch(() => ({ exists: false })),
        ]);
        if (!agentsStat.exists && !accessoriesStat.exists && !pluginsStat.exists && !workspacesStat.exists) {
          this.treePanelTarget.dataset.view = "empty";
          return;
        }
      } else {
        // Repo scope: check .botster/ exists
        const botsterStat = await this.hub.statFile(".botster").catch(() => ({ exists: false }));
        if (!botsterStat.exists) {
          this.treePanelTarget.dataset.view = "empty";
          return;
        }
      }

      // Scan all sections in parallel
      const [agentNames, accessoryNames, workspaceEntries, pluginNames, wsInclude, wsTeardown] = await Promise.all([
        this.#listDirs(`${prefix}agents`, fsScope),
        this.#listDirs(`${prefix}accessories`, fsScope),
        this.#listFiles(`${prefix}workspaces`, fsScope, ".json"),
        this.#listDirs(`${prefix}plugins`, fsScope),
        this.hub.statFile(`${prefix}workspace_include`, fsScope).catch(() => ({ exists: false })),
        this.hub.statFile(`${prefix}workspace_teardown`, fsScope).catch(() => ({ exists: false })),
      ]);

      tree.files.workspace_include = wsInclude.exists;
      tree.files.workspace_teardown = wsTeardown.exists;

      // Scan agents (each has initialization file)
      await Promise.all(
        agentNames.map(async (name) => {
          const agentPath = `${prefix}agents/${name}`;
          const initStat = await this.hub.statFile(`${agentPath}/initialization`, fsScope).catch(() => ({ exists: false }));
          tree.agents[name] = { initialization: initStat.exists };
        }),
      );

      // Scan accessories (each has initialization + optional port_forward)
      await Promise.all(
        accessoryNames.map(async (name) => {
          const accPath = `${prefix}accessories/${name}`;
          const [initStat, pfStat] = await Promise.all([
            this.hub.statFile(`${accPath}/initialization`, fsScope).catch(() => ({ exists: false })),
            this.hub.statFile(`${accPath}/port_forward`, fsScope).catch(() => ({ exists: false })),
          ]);
          tree.accessories[name] = { initialization: initStat.exists, port_forward: pfStat.exists };
        }),
      );

      // Scan workspaces (.json files)
      for (const fileName of workspaceEntries) {
        tree.workspaces[fileName.replace(/\.json$/, "")] = { file: `${prefix}workspaces/${fileName}` };
      }

      // Scan plugins (only include plugins that have init.lua)
      await Promise.all(
        pluginNames.map(async (name) => {
          const initStat = await this.hub.statFile(`${prefix}plugins/${name}/init.lua`, fsScope).catch(() => ({ exists: false }));
          if (initStat.exists) {
            tree.plugins[name] = { init: true };
          }
        }),
      );

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

  async #listFiles(path, fsScope, ext) {
    try {
      const result = await this.hub.listDir(path, fsScope);
      return (result.entries || [])
        .filter((e) => e.type === "file" && (!ext || e.name.endsWith(ext)))
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
        // Device: create agents/claude/ under ~/.botster/
        await this.hub.mkDir("agents/claude", fsScope);
        const defaultInit = this.configMetadataValue?.session_files?.initialization?.default || "#!/bin/bash\n";
        await this.hub.writeFile("agents/claude/initialization", defaultInit, fsScope);
      } else {
        // Repo: create .botster/agents/claude/
        await this.hub.mkDir(".botster/agents/claude");
        const defaultInit = this.configMetadataValue?.session_files?.initialization?.default || "#!/bin/bash\n";
        await this.hub.writeFile(".botster/agents/claude/initialization", defaultInit);
      }
      this.scanTree();
    } catch (error) {
      this.#showError(`Failed to initialize: ${error.message}`);
    }
  }

  #renderTree() {
    const container = this.treeContainerTarget;
    const newContainer = container.cloneNode(false);

    const prefix = this.#configPrefix();

    // Top-level workspace files
    const hasFiles = this.tree.files.workspace_include || this.tree.files.workspace_teardown;
    if (hasFiles || Object.keys(this.tree.agents).length > 0) {
      const filesSection = document.createElement("div");
      filesSection.className = "mb-3";

      const filesList = document.createElement("div");
      filesList.className = "space-y-1";

      for (const [fileName, exists] of Object.entries(this.tree.files)) {
        const status = exists ? "exists" : "missing";
        filesList.appendChild(this.#renderFileEntry(`${prefix}${fileName}`, fileName, status));
      }

      filesSection.appendChild(filesList);
      newContainer.appendChild(filesSection);
    }

    // Agents section
    const agentNames = Object.keys(this.tree.agents).sort();
    if (agentNames.length > 0) {
      const header = document.createElement("div");
      header.className = "mt-2";
      header.innerHTML = `<h2 class="text-sm font-medium text-zinc-400 uppercase tracking-wider mb-3">Agents</h2>`;
      newContainer.appendChild(header);

      for (const name of agentNames) {
        newContainer.appendChild(this.#renderAgentSection(name, prefix));
      }
    }

    // Add agent button
    const addAgentBtn = document.createElement("button");
    addAgentBtn.type = "button";
    addAgentBtn.id = "add-agent-btn";
    addAgentBtn.className = "w-full mt-2 px-3 py-2 text-xs font-medium text-zinc-500 hover:text-zinc-300 border border-dashed border-zinc-700 hover:border-zinc-600 rounded-lg transition-colors";
    addAgentBtn.textContent = "+ Add Agent";
    addAgentBtn.dataset.action = "hub-settings#addAgent";
    newContainer.appendChild(addAgentBtn);

    // Accessories section
    const accessoryNames = Object.keys(this.tree.accessories).sort();
    if (accessoryNames.length > 0) {
      const header = document.createElement("div");
      header.className = "mt-4";
      header.innerHTML = `<h2 class="text-sm font-medium text-zinc-400 uppercase tracking-wider mb-3">Accessories</h2>`;
      newContainer.appendChild(header);

      for (const name of accessoryNames) {
        newContainer.appendChild(this.#renderAccessorySection(name, prefix));
      }
    }

    // Add accessory button
    const addAccBtn = document.createElement("button");
    addAccBtn.type = "button";
    addAccBtn.id = "add-accessory-btn";
    addAccBtn.className = "w-full mt-2 px-3 py-2 text-xs font-medium text-zinc-500 hover:text-zinc-300 border border-dashed border-zinc-700 hover:border-zinc-600 rounded-lg transition-colors";
    addAccBtn.textContent = "+ Add Accessory";
    addAccBtn.dataset.action = "hub-settings#addAccessory";
    newContainer.appendChild(addAccBtn);

    // Workspaces section
    const workspaceNames = Object.keys(this.tree.workspaces).sort();
    if (workspaceNames.length > 0) {
      const header = document.createElement("div");
      header.className = "mt-4";
      header.innerHTML = `<h2 class="text-sm font-medium text-zinc-400 uppercase tracking-wider mb-3">Workspaces</h2>`;
      newContainer.appendChild(header);

      const list = document.createElement("div");
      list.className = "space-y-1";
      for (const name of workspaceNames) {
        const ws = this.tree.workspaces[name];
        list.appendChild(this.#renderFileEntry(ws.file, `${name}.json`, "exists"));
      }
      newContainer.appendChild(list);
    }

    // Plugins section
    const pluginNames = Object.keys(this.tree.plugins).sort();
    if (pluginNames.length > 0) {
      const header = document.createElement("div");
      header.className = "mt-4";
      header.innerHTML = `<h2 class="text-sm font-medium text-zinc-400 uppercase tracking-wider mb-3">Plugins</h2>`;
      newContainer.appendChild(header);

      const list = document.createElement("div");
      list.className = "space-y-1";
      for (const name of pluginNames) {
        const pluginPath = `${prefix}plugins/${name}/init.lua`;
        list.appendChild(this.#renderFileEntry(pluginPath, `${name}/init.lua`, "exists"));
      }
      newContainer.appendChild(list);
    }

    window.Turbo.morphElements(container, newContainer, {
      morphStyle: "innerHTML",
    });

    this.treePanelTarget.dataset.view = "tree";

    // Re-apply selection highlight after morph
    if (this.currentFilePath) {
      this.#highlightSelected(this.currentFilePath);
    }
  }

  #renderAgentSection(name, prefix) {
    const section = document.createElement("div");
    section.id = `section-agents-${name.replace(/[^a-zA-Z0-9-]/g, "-")}`;
    section.className = "mb-3 group/section data-[flash]:ring-1 data-[flash]:ring-primary-500/30 data-[flash]:rounded-lg";

    // Header with remove button
    const headerDiv = document.createElement("div");
    headerDiv.className = "flex items-center justify-between mb-2";
    headerDiv.innerHTML = `<h3 class="text-xs font-medium text-zinc-500 uppercase tracking-wider">${this.#escapeHtml(this.#capitalize(name))}</h3>`;

    const removeBtn = document.createElement("button");
    removeBtn.type = "button";
    removeBtn.className =
      "text-zinc-700 hover:text-red-400 transition-colors opacity-0 group-hover/section:opacity-100";
    removeBtn.title = "Remove agent";
    removeBtn.dataset.action = "hub-settings#removeAgent";
    removeBtn.dataset.agentName = name;
    removeBtn.innerHTML = `<svg class="size-3.5" viewBox="0 0 20 20" fill="currentColor">
      <path fill-rule="evenodd" d="M8.75 1A2.75 2.75 0 006 3.75v.443c-.795.077-1.584.176-2.365.298a.75.75 0 10.23 1.482l.149-.022.841 10.518A2.75 2.75 0 007.596 19h4.807a2.75 2.75 0 002.742-2.53l.841-10.52.149.023a.75.75 0 00.23-1.482A41.03 41.03 0 0014 4.193V3.75A2.75 2.75 0 0011.25 1h-2.5zM10 4c.84 0 1.673.025 2.5.075V3.75c0-.69-.56-1.25-1.25-1.25h-2.5c-.69 0-1.25.56-1.25 1.25v.325C8.327 4.025 9.16 4 10 4zM8.58 7.72a.75.75 0 00-1.5.06l.3 7.5a.75.75 0 101.5-.06l-.3-7.5zm4.34.06a.75.75 0 10-1.5-.06l-.3 7.5a.75.75 0 101.5.06l.3-7.5z" clip-rule="evenodd"/>
    </svg>`;
    headerDiv.appendChild(removeBtn);

    section.appendChild(headerDiv);

    const list = document.createElement("div");
    list.className = "space-y-1";

    const agentPath = `${prefix}agents/${name}`;
    const agent = this.tree.agents[name];
    const initStatus = agent.initialization ? "exists" : "missing";
    list.appendChild(this.#renderFileEntry(`${agentPath}/initialization`, "initialization", initStatus));

    section.appendChild(list);
    return section;
  }

  #renderAccessorySection(name, prefix) {
    const section = document.createElement("div");
    section.id = `section-accessories-${name.replace(/[^a-zA-Z0-9-]/g, "-")}`;
    section.className = "mb-3 group/section data-[flash]:ring-1 data-[flash]:ring-primary-500/30 data-[flash]:rounded-lg";

    // Header with remove button
    const headerDiv = document.createElement("div");
    headerDiv.className = "flex items-center justify-between mb-2";
    headerDiv.innerHTML = `<h3 class="text-xs font-medium text-zinc-500 uppercase tracking-wider">${this.#escapeHtml(this.#capitalize(name))}</h3>`;

    const removeBtn = document.createElement("button");
    removeBtn.type = "button";
    removeBtn.className =
      "text-zinc-700 hover:text-red-400 transition-colors opacity-0 group-hover/section:opacity-100";
    removeBtn.title = "Remove accessory";
    removeBtn.dataset.action = "hub-settings#removeAccessory";
    removeBtn.dataset.accessoryName = name;
    removeBtn.innerHTML = `<svg class="size-3.5" viewBox="0 0 20 20" fill="currentColor">
      <path fill-rule="evenodd" d="M8.75 1A2.75 2.75 0 006 3.75v.443c-.795.077-1.584.176-2.365.298a.75.75 0 10.23 1.482l.149-.022.841 10.518A2.75 2.75 0 007.596 19h4.807a2.75 2.75 0 002.742-2.53l.841-10.52.149.023a.75.75 0 00.23-1.482A41.03 41.03 0 0014 4.193V3.75A2.75 2.75 0 0011.25 1h-2.5zM10 4c.84 0 1.673.025 2.5.075V3.75c0-.69-.56-1.25-1.25-1.25h-2.5c-.69 0-1.25.56-1.25 1.25v.325C8.327 4.025 9.16 4 10 4zM8.58 7.72a.75.75 0 00-1.5.06l.3 7.5a.75.75 0 101.5-.06l-.3-7.5zm4.34.06a.75.75 0 10-1.5-.06l-.3 7.5a.75.75 0 101.5.06l.3-7.5z" clip-rule="evenodd"/>
    </svg>`;
    headerDiv.appendChild(removeBtn);

    section.appendChild(headerDiv);

    const list = document.createElement("div");
    list.className = "space-y-1";

    const accPath = `${prefix}accessories/${name}`;
    const acc = this.tree.accessories[name];
    const initStatus = acc.initialization ? "exists" : "missing";
    list.appendChild(this.#renderFileEntry(`${accPath}/initialization`, "initialization", initStatus));

    // Port forward toggle
    list.appendChild(this.#renderPortForwardToggle(`${accPath}/port_forward`, name, acc.port_forward));

    section.appendChild(list);
    return section;
  }

  #renderFileEntry(filePath, label, status) {
    const btn = document.createElement("button");
    btn.id = `file-${filePath.replace(/[^a-zA-Z0-9-]/g, "-")}`;
    btn.type = "button";
    btn.className =
      "w-full text-left px-2.5 py-1.5 rounded border border-zinc-700/50 hover:border-zinc-700 hover:bg-zinc-800/50 transition-colors " +
      "data-[selected]:bg-zinc-800/50 data-[selected]:border-primary-500/30 " +
      "data-[flash]:ring-1 data-[flash]:ring-primary-500/50";
    btn.dataset.action = "hub-settings#selectFile";
    btn.dataset.filePath = filePath;

    const styles = {
      exists: "bg-emerald-500/10 text-emerald-400",
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

  #renderPortForwardToggle(filePath, name, enabled) {
    const div = document.createElement("div");
    div.id = `pf-toggle-${filePath.replace(/[^a-zA-Z0-9-]/g, "-")}`;
    div.className = "flex items-center justify-between px-2.5 py-1";

    const id = `pf-${filePath.replace(/[/.]/g, "-")}`;
    div.innerHTML = `
      <div class="flex items-center gap-2">
        <label for="${id}" class="text-xs text-zinc-500 cursor-pointer">port_forward</label>
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
      // Default strict naming validation (for agent/accessory names)
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

  #scrollToSection(key) {
    const section = this.treeContainerTarget.querySelector(
      `#section-${CSS.escape(key)}`,
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
    if (filePath.endsWith(".json")) {
      return '{\n  "agents": [],\n  "accessories": []\n}\n';
    }
    return "";
  }

  /** Return the path prefix for the current config scope. */
  #configPrefix() {
    return this.configScope === "device" ? "" : ".botster/";
  }

  /** Return the fs scope string to pass to hub methods. */
  #fsScope() {
    return this.configScope === "device" ? "device" : undefined;
  }

  #capitalize(str) {
    return str.charAt(0).toUpperCase() + str.slice(1);
  }

  /**
   * Trigger a graceful Hub restart.
   *
   * Sends `restart_hub` to the CLI so it sets the graceful-shutdown flag
   * before disconnecting.  The broker keeps PTYs alive for ~120 s, allowing
   * agents to survive.  The browser will naturally show a disconnected state
   * until the Hub comes back online.
   */
  restartHub(event) {
    if (!this.hub) return;
    const btn = event.currentTarget;
    btn.disabled = true;
    btn.textContent = "Restarting…";
    this.hub.restartHub();
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
