import { Controller } from "@hotwired/stimulus";
import { HubManager } from "connections";

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
    "repoTargetPanel",
    "repoTargetSelect",
    "repoTargetHint",
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
    // Tree component templates (cloned, never shown directly)
    "tplSectionHeader",
    "tplSectionHeaderFirst",
    "tplFileEntry",
    "tplNamedSection",
    "tplAddButton",
    "tplPortForward",
    "tplFileList",
    "tplPluginsEmpty",
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
    this.spawnTargets = [];
    this.selectedTargetId = null;

    HubManager.acquire(this.hubIdValue).then((hub) => {
      this.hub = hub;
      this.spawnTargets = hub.spawnTargets.current();
      hub.spawnTargets.load().catch(() => {});
      if (!this.spawnTargets.some((target) => target.id === this.selectedTargetId)) {
        this.selectedTargetId = this.spawnTargets.length === 1 ? this.spawnTargets[0].id : null;
      }
      this.#renderRepoTargetOptions();

      this.unsubscribers.push(
        this.hub.spawnTargets.onChange((targets) => {
          this.spawnTargets = Array.isArray(targets) ? targets : [];
          if (!this.spawnTargets.some((target) => target.id === this.selectedTargetId)) {
            this.selectedTargetId = this.spawnTargets.length === 1 ? this.spawnTargets[0].id : null;
          }
          this.#renderRepoTargetOptions();
          if (this.configScope === "repo") this.scanTree();
        }),
      );

      this.unsubscribers.push(
        this.hub.onDisconnected(() => {
          this.treeFeedbackTarget.textContent = "Hub disconnected. Reconnecting...";
          this.treePanelTarget.dataset.view = "disconnected";
        }),
      );
      this.scanTree();
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
    this.#renderRepoTargetOptions();

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

  selectRepoTarget(event) {
    this.selectedTargetId = event.currentTarget.value || null;
    this.repoTree = null;
    this.currentFilePath = null;
    this.originalContent = null;
    this.editorPanelTarget.dataset.editor = "empty";
    this.editorTitleTarget.textContent = "Select a file";
    this.#renderRepoTargetOptions();
    if (this.configScope === "repo") this.scanTree();
  }

  async selectFile(event) {
    const filePath = event.currentTarget.dataset.filePath;
    if (!filePath || !this.hub) return;

    this.currentFilePath = filePath;
    this.#highlightSelected(filePath);

    const fsScope = this.#fsScope();
    try {
      const stat = await this.hub.statFile(filePath, fsScope, this.#targetId());
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
      await this.hub.writeFile(this.currentFilePath, content, this.#fsScope(), this.#targetId());
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
      await this.hub.mkDir(parentDir, fsScope, this.#targetId()).catch(() => {});

      await this.hub.writeFile(this.currentFilePath, content, fsScope, this.#targetId());
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
      await this.hub.deleteFile(this.currentFilePath, this.#fsScope(), this.#targetId());
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
      await this.hub.renameFile(this.currentFilePath, newPath, this.#fsScope(), this.#targetId());
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
        await this.hub.mkDir(parentDir, fsScope, this.#targetId()).catch(() => {});
        await this.hub.writeFile(filePath, "", fsScope, this.#targetId());
      } else {
        await this.hub.deleteFile(filePath, fsScope, this.#targetId());
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
      await this.hub.mkDir(`${prefix}agents/${name}`, fsScope, this.#targetId());
      const defaultInit = this.configMetadataValue?.session_files?.initialization?.default || "#!/bin/bash\n";
      await this.hub.writeFile(`${prefix}agents/${name}/initialization`, defaultInit, fsScope, this.#targetId());
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
      await this.hub.rmDir(`${prefix}agents/${agentName}`, fsScope, this.#targetId());
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
      await this.hub.mkDir(`${prefix}accessories/${name}`, fsScope, this.#targetId());
      const defaultInit = this.configMetadataValue?.session_files?.initialization?.default || "#!/bin/bash\n";
      await this.hub.writeFile(`${prefix}accessories/${name}/initialization`, defaultInit, fsScope, this.#targetId());
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
      await this.hub.rmDir(`${prefix}accessories/${accessoryName}`, fsScope, this.#targetId());
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
        await this.hub.mkDir(`.botster/${parentDir}`, "repo", this.#targetId());
        await this.hub.writeFile(`.botster/${dest}`, content, "repo", this.#targetId());
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
    const fsScope = scope === "device" ? "device" : "repo";

    if (scope === "repo" && !this.#targetId()) {
      this.treeFeedbackTarget.textContent = this.spawnTargets.length === 0
        ? "Add a spawn target first to edit target-local configuration."
        : "Select a spawn target to edit target-local configuration.";
      this.treePanelTarget.dataset.view = "loading";
      return;
    }

    try {
      const tree = { agents: {}, accessories: {}, workspaces: {}, plugins: {} };
      const prefix = scope === "device" ? "" : ".botster/";

      // Check if config root exists
      if (scope === "device") {
        // Device scope: check if agents/ or any config dir exists
        const [agentsStat, accessoriesStat, pluginsStat, workspacesStat] = await Promise.all([
          this.hub.statFile("agents", fsScope, this.#targetId()).catch(() => ({ exists: false })),
          this.hub.statFile("accessories", fsScope, this.#targetId()).catch(() => ({ exists: false })),
          this.hub.statFile("plugins", fsScope, this.#targetId()).catch(() => ({ exists: false })),
          this.hub.statFile("workspaces", fsScope, this.#targetId()).catch(() => ({ exists: false })),
        ]);
        if (!agentsStat.exists && !accessoriesStat.exists && !pluginsStat.exists && !workspacesStat.exists) {
          this.treePanelTarget.dataset.view = "empty";
          this.#refreshAgentConfigCache();
          return;
        }
      } else {
        // Repo scope: check .botster/ exists
        const botsterStat = await this.hub.statFile(".botster", fsScope, this.#targetId()).catch(() => ({ exists: false }));
        if (!botsterStat.exists) {
          this.treePanelTarget.dataset.view = "empty";
          this.#refreshAgentConfigCache();
          return;
        }
      }

      // Scan all sections in parallel
      const [agentNames, accessoryNames, workspaceEntries, pluginNames] = await Promise.all([
        this.#listDirs(`${prefix}agents`, fsScope),
        this.#listDirs(`${prefix}accessories`, fsScope),
        this.#listFiles(`${prefix}workspaces`, fsScope, ".json"),
        this.#listDirs(`${prefix}plugins`, fsScope),
      ]);

      // Scan agents (each has initialization file)
      await Promise.all(
        agentNames.map(async (name) => {
          const agentPath = `${prefix}agents/${name}`;
          const initStat = await this.hub.statFile(`${agentPath}/initialization`, fsScope, this.#targetId()).catch(() => ({ exists: false }));
          tree.agents[name] = { initialization: initStat.exists };
        }),
      );

      // Scan accessories (each has initialization + optional port_forward)
      await Promise.all(
        accessoryNames.map(async (name) => {
          const accPath = `${prefix}accessories/${name}`;
          const [initStat, pfStat] = await Promise.all([
            this.hub.statFile(`${accPath}/initialization`, fsScope, this.#targetId()).catch(() => ({ exists: false })),
            this.hub.statFile(`${accPath}/port_forward`, fsScope, this.#targetId()).catch(() => ({ exists: false })),
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
          const initStat = await this.hub.statFile(`${prefix}plugins/${name}/init.lua`, fsScope, this.#targetId()).catch(() => ({ exists: false }));
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
      this.#refreshAgentConfigCache();
    } catch (error) {
      if (isFirstLoad) {
        this.treeFeedbackTarget.textContent = `Failed to scan: ${error.message}`;
      }
    }
  }

  async #listDirs(path, fsScope) {
    try {
      const result = await this.hub.listDir(path, fsScope, this.#targetId());
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
      const result = await this.hub.listDir(path, fsScope, this.#targetId());
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
        await this.hub.mkDir(".botster/agents/claude", "repo", this.#targetId());
        const defaultInit = this.configMetadataValue?.session_files?.initialization?.default || "#!/bin/bash\n";
        await this.hub.writeFile(".botster/agents/claude/initialization", defaultInit, "repo", this.#targetId());
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

    // Agents
    const agentNames = Object.keys(this.tree.agents).sort();
    if (agentNames.length > 0) {
      newContainer.appendChild(this.#cloneSectionHeader("Agents", true));
      for (const name of agentNames) {
        newContainer.appendChild(this.#buildNamedSection(name, "agents", prefix));
      }
    }
    newContainer.appendChild(this.#cloneAddButton("+ Add Agent", "hub-settings#addAgent", "add-agent-btn"));

    // Accessories
    const accessoryNames = Object.keys(this.tree.accessories).sort();
    if (accessoryNames.length > 0) {
      newContainer.appendChild(this.#cloneSectionHeader("Accessories"));
      for (const name of accessoryNames) {
        newContainer.appendChild(this.#buildNamedSection(name, "accessories", prefix));
      }
    }
    newContainer.appendChild(this.#cloneAddButton("+ Add Accessory", "hub-settings#addAccessory", "add-accessory-btn"));

    // Workspaces
    const workspaceNames = Object.keys(this.tree.workspaces).sort();
    if (workspaceNames.length > 0) {
      newContainer.appendChild(this.#cloneSectionHeader("Workspaces"));
      const list = this.#cloneFileList();
      for (const name of workspaceNames) {
        list.appendChild(this.#cloneFileEntry(`${prefix}workspaces/${name}.json`, `${name}.json`, "exists"));
      }
      newContainer.appendChild(list);
    }

    // Plugins (always visible)
    newContainer.appendChild(this.#cloneSectionHeader("Plugins"));
    const pluginNames = Object.keys(this.tree.plugins).sort();
    if (pluginNames.length > 0) {
      const list = this.#cloneFileList();
      for (const name of pluginNames) {
        list.appendChild(this.#cloneFileEntry(`${prefix}plugins/${name}/init.lua`, `${name}/init.lua`, "exists"));
      }
      newContainer.appendChild(list);
    } else {
      newContainer.appendChild(this.tplPluginsEmptyTarget.content.cloneNode(true));
    }

    window.Turbo.morphElements(container, newContainer, { morphStyle: "innerHTML" });
    this.treePanelTarget.dataset.view = "tree";

    if (this.currentFilePath) {
      this.#highlightSelected(this.currentFilePath);
    }
  }

  // ========== Template Cloners ==========

  #cloneSectionHeader(title, first = false) {
    const tpl = first ? this.tplSectionHeaderFirstTarget : this.tplSectionHeaderTarget;
    const frag = tpl.content.cloneNode(true);
    frag.querySelector('[data-slot="title"]').textContent = title;
    return frag;
  }

  #cloneFileEntry(filePath, label, status) {
    const frag = this.tplFileEntryTarget.content.cloneNode(true);
    const btn = frag.querySelector("button");
    btn.id = `file-${filePath.replace(/[^a-zA-Z0-9-]/g, "-")}`;
    btn.dataset.filePath = filePath;
    frag.querySelector('[data-slot="label"]').textContent = label;
    const badge = frag.querySelector('[data-slot="status"]');
    badge.textContent = status;
    badge.dataset.status = status;
    return frag;
  }

  #cloneFileList() {
    const frag = this.tplFileListTarget.content.cloneNode(true);
    return frag.querySelector("div");
  }

  #cloneAddButton(label, action, id) {
    const frag = this.tplAddButtonTarget.content.cloneNode(true);
    const btn = frag.querySelector("button");
    btn.id = id;
    btn.textContent = label;
    btn.dataset.action = action;
    return frag;
  }

  #buildNamedSection(name, type, prefix) {
    const frag = this.tplNamedSectionTarget.content.cloneNode(true);
    const section = frag.querySelector("div");
    section.id = `section-${type}-${name.replace(/[^a-zA-Z0-9-]/g, "-")}`;

    frag.querySelector('[data-slot="name"]').textContent = this.#capitalize(name);

    const removeBtn = frag.querySelector('[data-slot="removeBtn"]');
    removeBtn.dataset.action = type === "agents" ? "hub-settings#removeAgent" : "hub-settings#removeAccessory";
    if (type === "agents") {
      removeBtn.dataset.agentName = name;
      removeBtn.title = "Remove agent";
    } else {
      removeBtn.dataset.accessoryName = name;
      removeBtn.title = "Remove accessory";
    }

    const files = frag.querySelector('[data-slot="files"]');
    const itemPath = `${prefix}${type}/${name}`;
    const item = this.tree[type][name];
    const initStatus = item.initialization ? "exists" : "missing";
    files.appendChild(this.#cloneFileEntry(`${itemPath}/initialization`, "initialization", initStatus));

    if (type === "accessories") {
      files.appendChild(this.#clonePortForward(`${itemPath}/port_forward`, name, item.port_forward));
    }

    return frag;
  }

  #clonePortForward(filePath, name, enabled) {
    const frag = this.tplPortForwardTarget.content.cloneNode(true);
    const wrapper = frag.querySelector("div");
    wrapper.id = `pf-toggle-${filePath.replace(/[^a-zA-Z0-9-]/g, "-")}`;

    const id = `pf-${filePath.replace(/[/.]/g, "-")}`;
    const label = frag.querySelector('[data-slot="label"]');
    label.setAttribute("for", id);

    const hint = frag.querySelector('[data-slot="hint"]');
    if (enabled) hint.dataset.enabled = "";

    const checkbox = frag.querySelector('[data-slot="checkbox"]');
    checkbox.id = id;
    checkbox.dataset.filePath = filePath;
    if (enabled) checkbox.checked = true;

    return frag;
  }

  // ========== Editor State ==========

  async #loadFileContent(filePath) {
    this.editorPanelTarget.dataset.editor = "loading";

    try {
      const result = await this.hub.readFile(filePath, this.#fsScope(), this.#targetId());
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
    return this.configScope === "device" ? "device" : "repo";
  }

  #refreshAgentConfigCache() {
    if (!this.hub) return;

    const targetIds = this.configScope === "repo"
      ? [this.#targetId()]
      : this.spawnTargets.map((target) => target?.id);

    targetIds.filter(Boolean).forEach((targetId) => {
      this.hub.ensureAgentConfig(targetId, { force: true }).catch(() => {});
    });
  }

  #targetId() {
    return this.configScope === "repo" ? this.selectedTargetId : undefined;
  }

  #renderRepoTargetOptions() {
    if (!this.hasRepoTargetPanelTarget || !this.hasRepoTargetSelectTarget) return;

    const repoScope = this.configScope === "repo";
    this.repoTargetPanelTarget.classList.toggle("hidden", !repoScope);

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
          ? "Add a spawn target from the device page before editing target-local .botster files."
          : "Target-local .botster editing is locked to the selected admitted spawn target.");
    }
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

}
