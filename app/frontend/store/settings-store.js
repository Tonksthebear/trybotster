import { create } from 'zustand'
import { waitForHub } from '../lib/hub-bridge'
import {
  defaultSettingsContent,
  invalidateAgentConfigQueries,
  pluginName,
  scanSettingsTree,
  settingsFsContext,
} from '../lib/settings-store-helpers'
import { useSpawnTargetStore } from './entities'
import { orderedEntities } from '../lib/entity-selectors'

export const useSettingsStore = create((set, get) => ({
  // --- Connection ---
  hub: null,
  connected: true,
  _unsubscribers: [],

  // --- Spawn targets ---
  spawnTargets: [],
  selectedTargetId: null,

  // --- Tabs ---
  activeTab: 'config',

  // --- Config tree ---
  configScope: 'repo',
  treeState: 'loading', // loading | tree | empty | disconnected
  treeFeedback: 'Connecting to hub...',
  tree: null,
  deviceTree: null,
  repoTree: null,
  _scanTreeToken: 0,

  // --- Editor ---
  currentFilePath: null,
  originalContent: null,
  editorContent: '',
  editorState: 'empty', // empty | loading | editing | creating | error
  editorError: null,

  // --- Templates ---
  installedDevice: new Set(),
  installedRepo: new Set(),
  installedStateLoaded: false,
  previewSlug: null,
  scopeOverrides: {},
  templateFeedback: '',
  _installStateToken: 0,

  // --- Server data (passed from ERB) ---
  _configMetadata: null,

  // ===================== Actions =====================

  setActiveTab: (tab) => set({ activeTab: tab }),
  setEditorContent: (content) => set({ editorContent: content }),
  setPreviewSlug: (slug) => set({ previewSlug: slug }),
  setTemplateFeedback: (msg) => set({ templateFeedback: msg }),

  setScopeOverride(slug, scope) {
    set((s) => ({ scopeOverrides: { ...s.scopeOverrides, [slug]: scope } }))
  },

  setConfigMetadata(meta) {
    set({ _configMetadata: meta })
  },

  // --- Hub lifecycle ---

  async connectHub(hubId) {
    const hub = await waitForHub(hubId)
    if (!hub) {
      set({
        connected: false,
        treeState: 'disconnected',
        treeFeedback: 'Hub connection is not ready.',
      })
      return null
    }

    const targets = orderedEntities(useSpawnTargetStore.getState())
    hub.requestSpawnTargets?.()
    const selectedTargetId = targets.length === 1 ? targets[0].id || targets[0].target_id : null

    const unsubs = []

    unsubs.push(
      useSpawnTargetStore.subscribe((state) => {
        const arr = orderedEntities(state)
        const { selectedTargetId: currentId } = get()
        const valid = arr.some((t) => (t.id || t.target_id) === currentId)
        set({
          spawnTargets: arr,
          selectedTargetId: valid
            ? currentId
            : arr.length === 1
              ? arr[0].id || arr[0].target_id
              : null,
        })
      })
    )

    unsubs.push(
      hub.onDisconnected(() => {
        set({
          connected: false,
          treeState: 'disconnected',
          treeFeedback: 'Hub disconnected. Reconnecting...',
        })
      })
    )

    set({
      hub,
      connected: true,
      spawnTargets: targets,
      selectedTargetId,
      _unsubscribers: unsubs,
    })

    return hub
  },

  disconnectHub() {
    const { _unsubscribers } = get()
    _unsubscribers.forEach((fn) => fn())
    set({ _unsubscribers: [], hub: null })
  },

  // --- Config scope + target ---

  setConfigScope(scope) {
    const cached = scope === 'device' ? get().deviceTree : get().repoTree
    set({
      configScope: scope,
      currentFilePath: null,
      originalContent: null,
      editorContent: '',
      editorState: 'empty',
      tree: cached,
      treeState: cached ? 'tree' : 'loading',
    })
  },

  setSelectedTargetId(id) {
    set({
      selectedTargetId: id || null,
      repoTree: null,
      currentFilePath: null,
      originalContent: null,
      editorContent: '',
      editorState: 'empty',
    })
  },

  // --- Tree scanning ---

  async scanTree() {
    const { hub, configScope, selectedTargetId, spawnTargets } = get()
    if (!hub) return
    const scope = configScope
    const { tid } = settingsFsContext(scope, selectedTargetId)
    const token = get()._scanTreeToken + 1
    set({ _scanTreeToken: token })
    const isCurrentScan = () =>
      get().hub === hub &&
      get()._scanTreeToken === token &&
      get().configScope === scope &&
      get().selectedTargetId === selectedTargetId

    const isFirstLoad = !get().tree
    if (isFirstLoad) {
      set({ treeState: 'loading', treeFeedback: 'Loading configuration...' })
    }

    if (scope === 'repo' && !tid) {
      if (!isCurrentScan()) return
      set({
        treeState: 'loading',
        treeFeedback:
          spawnTargets.length === 0
            ? 'Add a spawn target first to edit target-local configuration.'
            : 'Select a spawn target to edit target-local configuration.',
      })
      return
    }

    try {
      const result = await scanSettingsTree({ hub, configScope: scope, selectedTargetId })
      if (!isCurrentScan()) return
      if (result.state === 'empty') {
        set({ treeState: 'empty' })
        get()._invalidateAgentConfigQueries()
        return
      }

      const tree = result.tree
      const update = { tree, treeState: 'tree' }
      if (scope === 'device') update.deviceTree = tree
      else update.repoTree = tree
      set(update)
      get()._invalidateAgentConfigQueries()
    } catch (error) {
      if (isFirstLoad && isCurrentScan()) {
        set({ treeFeedback: `Failed to scan: ${error.message}` })
      }
    }
  },

  // --- File operations ---

  async selectFile(filePath) {
    const { hub, configScope, selectedTargetId } = get()
    if (!filePath || !hub) return

    const { fs, tid } = settingsFsContext(configScope, selectedTargetId)

    set({ currentFilePath: filePath })

    try {
      const stat = await hub.statFile(filePath, fs, tid)
      if (!get().hub) return
      if (stat.exists) {
        set({ editorState: 'loading' })
        const result = await hub.readFile(filePath, fs, tid)
        if (!get().hub) return
        set({
          originalContent: result.content,
          editorContent: result.content,
          editorState: 'editing',
        })
      } else {
        set({
          originalContent: null,
          editorContent: get()._defaultContent(filePath),
          editorState: 'creating',
        })
      }
    } catch {
      if (!get().hub) return
      set({
        originalContent: null,
        editorContent: get()._defaultContent(filePath),
        editorState: 'creating',
      })
    }
  },

  async saveFile() {
    const { hub, currentFilePath, editorContent, configScope, selectedTargetId } =
      get()
    if (!currentFilePath || !hub) return false

    const { fs, tid } = settingsFsContext(configScope, selectedTargetId)

    try {
      await hub.writeFile(currentFilePath, editorContent, fs, tid)
      set({ originalContent: editorContent })
      get().scanTree()
      return true
    } catch (error) {
      set({ editorState: 'error', editorError: `Save failed: ${error.message}` })
      return false
    }
  },

  revertFile() {
    const { originalContent } = get()
    if (originalContent !== null) {
      set({ editorContent: originalContent })
    }
  },

  async createFile() {
    const { hub, currentFilePath, editorContent, configScope, selectedTargetId } =
      get()
    if (!currentFilePath || !hub) return false

    const { fs, tid } = settingsFsContext(configScope, selectedTargetId)
    const content = editorContent || get()._defaultContent(currentFilePath)

    try {
      const parentDir = currentFilePath.replace(/\/[^/]+$/, '')
      await hub.mkDir(parentDir, fs, tid).catch(() => {})
      await hub.writeFile(currentFilePath, content, fs, tid)
      set({ originalContent: content, editorState: 'editing' })
      get().scanTree()
      return true
    } catch (error) {
      set({ editorState: 'error', editorError: `Create failed: ${error.message}` })
      return false
    }
  },

  async deleteFile() {
    const { hub, currentFilePath, configScope, selectedTargetId } = get()
    if (!currentFilePath || !hub) return false

    const { fs, tid } = settingsFsContext(configScope, selectedTargetId)

    try {
      await hub.deleteFile(currentFilePath, fs, tid)
      set({
        originalContent: null,
        editorContent: get()._defaultContent(currentFilePath),
        editorState: 'creating',
      })
      get().scanTree()
      return true
    } catch (error) {
      set({ editorState: 'error', editorError: `Delete failed: ${error.message}` })
      return false
    }
  },

  async renameFile(newPath) {
    const { hub, currentFilePath, configScope, selectedTargetId } = get()
    if (!currentFilePath || !hub || newPath === currentFilePath) return false

    const { fs, tid } = settingsFsContext(configScope, selectedTargetId)

    try {
      await hub.renameFile(currentFilePath, newPath, fs, tid)
      set({ currentFilePath: newPath })
      get().scanTree()
      return true
    } catch (error) {
      set({ editorState: 'error', editorError: `Rename failed: ${error.message}` })
      return false
    }
  },

  async togglePortForward(filePath, enabled) {
    const { hub, configScope, selectedTargetId } = get()
    if (!filePath || !hub) return

    const { fs, tid } = settingsFsContext(configScope, selectedTargetId)

    try {
      if (enabled) {
        const parentDir = filePath.replace(/\/[^/]+$/, '')
        await hub.mkDir(parentDir, fs, tid).catch(() => {})
        await hub.writeFile(filePath, '', fs, tid)
      } else {
        await hub.deleteFile(filePath, fs, tid)
      }
      get().scanTree()
    } catch {
      // Will resync on next scan
    }
  },

  // --- Agent / Accessory CRUD ---

  async addAgent(name) {
    const { hub, configScope, selectedTargetId } = get()
    if (!hub) return false

    const { fs, tid, prefix } = settingsFsContext(configScope, selectedTargetId)

    try {
      await hub.mkDir(`${prefix}agents/${name}`, fs, tid)
      const defaultInit =
        get()._configMetadata?.session_files?.initialization?.default ||
        '#!/bin/bash\n'
      await hub.writeFile(
        `${prefix}agents/${name}/initialization`,
        defaultInit,
        fs,
        tid
      )
      get().scanTree()
      return true
    } catch (error) {
      set({
        editorState: 'error',
        editorError: `Failed to create agent: ${error.message}`,
      })
      return false
    }
  },

  async removeAgent(agentName) {
    const { hub, configScope, selectedTargetId, currentFilePath } = get()
    if (!agentName || !hub) return false

    const { fs, tid, prefix } = settingsFsContext(configScope, selectedTargetId)

    try {
      await hub.rmDir(`${prefix}agents/${agentName}`, fs, tid)
      if (currentFilePath?.includes(`/agents/${agentName}/`)) {
        set({
          currentFilePath: null,
          originalContent: null,
          editorContent: '',
          editorState: 'empty',
        })
      }
      get().scanTree()
      return true
    } catch (error) {
      set({
        editorState: 'error',
        editorError: `Failed to remove agent: ${error.message}`,
      })
      return false
    }
  },

  async addAccessory(name) {
    const { hub, configScope, selectedTargetId } = get()
    if (!hub) return false

    const { fs, tid, prefix } = settingsFsContext(configScope, selectedTargetId)

    try {
      await hub.mkDir(`${prefix}accessories/${name}`, fs, tid)
      const defaultInit =
        get()._configMetadata?.session_files?.initialization?.default ||
        '#!/bin/bash\n'
      await hub.writeFile(
        `${prefix}accessories/${name}/initialization`,
        defaultInit,
        fs,
        tid
      )
      get().scanTree()
      return true
    } catch (error) {
      set({
        editorState: 'error',
        editorError: `Failed to create accessory: ${error.message}`,
      })
      return false
    }
  },

  async removeAccessory(accessoryName) {
    const { hub, configScope, selectedTargetId, currentFilePath } = get()
    if (!accessoryName || !hub) return false

    const { fs, tid, prefix } = settingsFsContext(configScope, selectedTargetId)

    try {
      await hub.rmDir(`${prefix}accessories/${accessoryName}`, fs, tid)
      if (currentFilePath?.includes(`/accessories/${accessoryName}/`)) {
        set({
          currentFilePath: null,
          originalContent: null,
          editorContent: '',
          editorState: 'empty',
        })
      }
      get().scanTree()
      return true
    } catch (error) {
      set({
        editorState: 'error',
        editorError: `Failed to remove accessory: ${error.message}`,
      })
      return false
    }
  },

  // --- Quick setup / init ---

  async quickSetup(dest, content) {
    const { hub, configScope, selectedTargetId } = get()
    if (!dest || !content || !hub) return false

    try {
      const parentDir = dest.replace(/\/[^/]+$/, '')
      if (configScope === 'device') {
        await hub.mkDir(parentDir, 'device')
        await hub.writeFile(dest, content, 'device')
      } else {
        await hub.mkDir(`.botster/${parentDir}`, 'repo', selectedTargetId)
        await hub.writeFile(`.botster/${dest}`, content, 'repo', selectedTargetId)
      }
      get().scanTree()
      return true
    } catch (error) {
      set({
        editorState: 'error',
        editorError: `Setup failed: ${error.message}`,
      })
      return false
    }
  },

  async initBotster() {
    const { hub, configScope, selectedTargetId } = get()
    if (!hub) return

    try {
      const defaultInit =
        get()._configMetadata?.session_files?.initialization?.default ||
        '#!/bin/bash\n'
      if (configScope === 'device') {
        await hub.mkDir('agents/claude', 'device')
        await hub.writeFile('agents/claude/initialization', defaultInit, 'device')
      } else {
        await hub.mkDir('.botster/agents/claude', 'repo', selectedTargetId)
        await hub.writeFile(
          '.botster/agents/claude/initialization',
          defaultInit,
          'repo',
          selectedTargetId
        )
      }
      get().scanTree()
    } catch (error) {
      set({
        editorState: 'error',
        editorError: `Failed to initialize: ${error.message}`,
      })
    }
  },

  // --- Template operations ---

  async checkInstalled() {
    const { hub, selectedTargetId } = get()
    if (!hub) return

    const token = get()._installStateToken + 1
    set({ _installStateToken: token, templateFeedback: 'Checking installed templates...' })

    try {
      const result = await hub.listInstalledTemplates(selectedTargetId)
      if (!get().hub || token !== get()._installStateToken) return

      const device = new Set()
      const repo = new Set()
      for (const entry of result.installed || []) {
        if (!entry.name) continue
        if (entry.scope === 'device') device.add(entry.name)
        else if (entry.scope === 'repo') repo.add(entry.name)
      }

      set({
        installedDevice: device,
        installedRepo: repo,
        installedStateLoaded: true,
        templateFeedback: '',
      })
    } catch {
      if (token !== get()._installStateToken) return
      set({ installedStateLoaded: true, templateFeedback: '' })
    }
  },

  async installTemplate(dest, content, scope, targetId) {
    const { hub } = get()
    if (!hub) return false

    try {
      await hub.installTemplate(dest, content, scope, targetId)
      const name = pluginName(dest)
      const key = scope === 'repo' ? 'installedRepo' : 'installedDevice'
      const next = new Set(get()[key])
      next.add(name)
      set({ [key]: next })
      await hub.loadPlugin(name, targetId).catch(() => {})
      return true
    } catch {
      return false
    }
  },

  async uninstallTemplate(dest, scope, targetId) {
    const { hub } = get()
    if (!hub) return false

    try {
      await hub.uninstallTemplate(dest, scope, targetId)
      const name = pluginName(dest)
      const key = scope === 'repo' ? 'installedRepo' : 'installedDevice'
      const next = new Set(get()[key])
      next.delete(name)
      set({ [key]: next })
      return true
    } catch {
      return false
    }
  },

  async reloadPlugin(name, targetId) {
    const { hub } = get()
    if (!hub) throw new Error('Hub not connected')
    await hub.reloadPlugin(name, targetId)
  },

  restartHub() {
    const { hub } = get()
    if (!hub) return
    hub.restartHub()
  },

  // --- Internal helpers ---

  _defaultContent(filePath) {
    return defaultSettingsContent(filePath, get()._configMetadata || {})
  },

  _invalidateAgentConfigQueries() {
    const { configScope, hub, spawnTargets, selectedTargetId } = get()
    invalidateAgentConfigQueries({
      configScope,
      hubId: hub?.hubId,
      spawnTargets,
      selectedTargetId,
    })
  },
}))

// --- Selectors ---

export function isDirty(state) {
  return (
    state.originalContent !== null &&
    state.editorContent !== state.originalContent
  )
}

export function getInstallScope(state, slug, defaultScope) {
  return state.scopeOverrides[slug] || defaultScope || 'device'
}
