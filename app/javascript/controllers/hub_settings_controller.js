import { Controller } from "@hotwired/stimulus";
import { ConnectionManager, HubConnection } from "connections";

/**
 * Hub Settings Controller
 *
 * Manages config file editing via E2E encrypted DataChannel to CLI.
 * Rails serves the page shell + default templates; all file I/O goes
 * through HubConnection's fs:* API directly to the CLI.
 */
export default class extends Controller {
  static targets = [
    "loading",
    "fileList",
    "fileEntry",
    "fileStatus",
    "editorTitle",
    "editorActions",
    "editorEmpty",
    "editorLoading",
    "editorWrapper",
    "editor",
    "editorError",
    "editorErrorMsg",
    "saveBtn",
    "revertBtn",
    "createBtn",
    "deleteBtn",
  ];

  static values = {
    hubId: String,
    configFiles: Array,
  };

  connect() {
    if (!this.hubIdValue) return;

    this.unsubscribers = [];
    this.currentFile = null; // Currently selected file name
    this.originalContent = null; // Content when file was loaded (for dirty tracking)
    this.fileExists = {}; // Map of filename -> boolean

    ConnectionManager.acquire(HubConnection, this.hubIdValue, {
      hubId: this.hubIdValue,
      fromFragment: true,
    }).then((hub) => {
      this.hub = hub;

      this.unsubscribers.push(
        this.hub.onConnected(() => {
          this.loadFiles();
        }),
      );

      this.unsubscribers.push(
        this.hub.onDisconnected(() => {
          this.#setDisconnected();
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

  async selectFile(event) {
    const fileName =
      event.currentTarget.dataset.fileName || event.params?.fileName;
    if (!fileName || !this.hub) return;

    this.currentFile = fileName;
    this.#highlightSelected(fileName);

    const exists = this.fileExists[fileName];

    if (exists) {
      await this.#loadFileContent(fileName);
    } else {
      this.#showCreateState(fileName);
    }
  }

  async save() {
    if (!this.currentFile || !this.hub) return;

    const content = this.editorTarget.value;
    this.saveBtnTarget.disabled = true;
    this.saveBtnTarget.textContent = "Saving...";

    try {
      await this.hub.writeFile(this.currentFile, content);
      this.originalContent = content;
      this.fileExists[this.currentFile] = true;
      this.#updateDirtyState();
      this.#updateFileStatuses();
      this.#showEditorState(this.currentFile, true);
      this.saveBtnTarget.textContent = "Saved";
      setTimeout(() => {
        this.saveBtnTarget.textContent = "Save";
      }, 1500);
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
    if (!this.currentFile || !this.hub) return;

    const config = this.configFilesValue.find(
      (f) => f.name === this.currentFile,
    );
    const content = this.editorTarget.value || config?.default || "";

    this.createBtnTarget.textContent = "Creating...";

    try {
      await this.hub.writeFile(this.currentFile, content);
      this.originalContent = content;
      this.fileExists[this.currentFile] = true;
      this.#updateFileStatuses();
      this.#showEditorState(this.currentFile, true);
    } catch (error) {
      this.#showError(`Create failed: ${error.message}`);
    } finally {
      this.createBtnTarget.textContent = "Create";
    }
  }

  async deleteFile() {
    if (!this.currentFile || !this.hub) return;
    if (!confirm(`Delete ${this.currentFile}?`)) return;

    this.deleteBtnTarget.textContent = "Deleting...";

    try {
      await this.hub.deleteFile(this.currentFile);
      this.fileExists[this.currentFile] = false;
      this.#updateFileStatuses();
      this.#showCreateState(this.currentFile);
    } catch (error) {
      this.#showError(`Delete failed: ${error.message}`);
    } finally {
      this.deleteBtnTarget.textContent = "Delete";
    }
  }

  onEditorInput() {
    this.#updateDirtyState();
  }

  // ========== Data Loading ==========

  async loadFiles() {
    this.loadingTarget.classList.remove("hidden");
    this.fileListTarget.classList.add("hidden");

    try {
      // Stat each config file to check existence
      const results = await Promise.allSettled(
        this.configFilesValue.map((file) => this.hub.statFile(file.name)),
      );

      results.forEach((result, i) => {
        const fileName = this.configFilesValue[i].name;
        this.fileExists[fileName] =
          result.status === "fulfilled" && result.value.exists;
      });

      this.#updateFileStatuses();
      this.loadingTarget.classList.add("hidden");
      this.fileListTarget.classList.remove("hidden");
    } catch (error) {
      this.loadingTarget.innerHTML = `<p class="text-sm text-red-400">Failed to load: ${error.message}</p>`;
    }
  }

  // ========== Private ==========

  async #loadFileContent(fileName) {
    this.#showEditorLoading();

    try {
      const result = await this.hub.readFile(fileName);
      this.originalContent = result.content;
      this.editorTarget.value = result.content;
      this.#showEditorState(fileName, true);
      this.#updateDirtyState();
    } catch (error) {
      this.#showError(`Read failed: ${error.message}`);
    }
  }

  #showCreateState(fileName) {
    const config = this.configFilesValue.find((f) => f.name === fileName);

    this.editorTitleTarget.textContent = fileName;
    this.editorEmptyTarget.classList.add("hidden");
    this.editorLoadingTarget.classList.add("hidden");
    this.editorErrorTarget.classList.add("hidden");
    this.editorWrapperTarget.classList.remove("hidden");

    this.editorTarget.value = config?.default || "";
    this.originalContent = null;

    this.saveBtnTarget.classList.add("hidden");
    this.revertBtnTarget.classList.add("hidden");
    this.createBtnTarget.classList.remove("hidden");
    this.deleteBtnTarget.classList.add("hidden");
  }

  #showEditorState(fileName, exists) {
    this.editorTitleTarget.textContent = fileName;
    this.editorEmptyTarget.classList.add("hidden");
    this.editorLoadingTarget.classList.add("hidden");
    this.editorErrorTarget.classList.add("hidden");
    this.editorWrapperTarget.classList.remove("hidden");

    this.saveBtnTarget.classList.remove("hidden");
    this.revertBtnTarget.classList.remove("hidden");
    this.createBtnTarget.classList.add("hidden");

    if (exists) {
      this.deleteBtnTarget.classList.remove("hidden");
    } else {
      this.deleteBtnTarget.classList.add("hidden");
    }
  }

  #showEditorLoading() {
    this.editorEmptyTarget.classList.add("hidden");
    this.editorWrapperTarget.classList.add("hidden");
    this.editorErrorTarget.classList.add("hidden");
    this.editorLoadingTarget.classList.remove("hidden");

    this.saveBtnTarget.classList.add("hidden");
    this.revertBtnTarget.classList.add("hidden");
    this.createBtnTarget.classList.add("hidden");
    this.deleteBtnTarget.classList.add("hidden");
  }

  #showError(message) {
    this.editorEmptyTarget.classList.add("hidden");
    this.editorLoadingTarget.classList.add("hidden");
    this.editorWrapperTarget.classList.add("hidden");
    this.editorErrorTarget.classList.remove("hidden");
    this.editorErrorMsgTarget.textContent = message;
  }

  #updateDirtyState() {
    const isDirty =
      this.originalContent !== null &&
      this.editorTarget.value !== this.originalContent;

    this.saveBtnTarget.disabled = !isDirty;
    if (isDirty) {
      this.revertBtnTarget.classList.remove("hidden");
    }
  }

  #updateFileStatuses() {
    this.fileStatusTargets.forEach((el) => {
      const fileName = el.dataset.fileName;
      const exists = this.fileExists[fileName];

      if (exists) {
        el.textContent = "exists";
        el.className =
          "shrink-0 ml-2 text-xs px-1.5 py-0.5 rounded bg-emerald-500/10 text-emerald-400";
      } else {
        el.textContent = "missing";
        el.className =
          "shrink-0 ml-2 text-xs px-1.5 py-0.5 rounded bg-zinc-700/50 text-zinc-500";
      }
    });
  }

  #highlightSelected(fileName) {
    this.fileEntryTargets.forEach((el) => {
      if (el.dataset.fileName === fileName) {
        el.classList.add("bg-zinc-800/50", "border-primary-500/30");
        el.classList.remove("border-zinc-700/50");
      } else {
        el.classList.remove("bg-zinc-800/50", "border-primary-500/30");
        el.classList.add("border-zinc-700/50");
      }
    });
  }

  #setDisconnected() {
    this.loadingTarget.classList.remove("hidden");
    this.loadingTarget.innerHTML =
      '<p class="text-sm text-zinc-500">Hub disconnected. Reconnecting...</p>';
    this.fileListTarget.classList.add("hidden");
  }
}
