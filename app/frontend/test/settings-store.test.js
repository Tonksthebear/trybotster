import { beforeEach, describe, expect, it, vi } from 'vitest'
import { useSettingsStore } from '../store/settings-store'

const mockWaitForHub = vi.hoisted(() => vi.fn())

vi.mock('../lib/hub-bridge', () => ({
  waitForHub: (...args) => mockWaitForHub(...args),
}))

function mockHub(overrides = {}) {
  const disconnectedCallbacks = []
  const connectedCallbacks = []
  const unsubscribeDisconnected = vi.fn()
  const unsubscribeConnected = vi.fn()

  return {
    onDisconnected: vi.fn((callback) => {
      disconnectedCallbacks.push(callback)
      return unsubscribeDisconnected
    }),
    onConnected: vi.fn((callback) => {
      connectedCallbacks.push(callback)
      return unsubscribeConnected
    }),
    listInstalledTemplates: vi.fn(() => Promise.resolve({ installed: [] })),
    installTemplate: vi.fn(() => Promise.resolve()),
    uninstallTemplate: vi.fn(() => Promise.resolve()),
    refreshTemplates: vi.fn(() => Promise.resolve()),
    loadPlugin: vi.fn(() => Promise.resolve()),
    statFile: vi.fn(() => Promise.resolve({ exists: false })),
    listDir: vi.fn(() => Promise.resolve({ entries: [] })),
    emitDisconnected() {
      disconnectedCallbacks.forEach((callback) => callback())
    },
    emitConnected() {
      connectedCallbacks.forEach((callback) => callback())
    },
    unsubscribeDisconnected,
    unsubscribeConnected,
    ...overrides,
  }
}

describe('settings store template install state', () => {
  beforeEach(() => {
    mockWaitForHub.mockReset()
    useSettingsStore.setState(useSettingsStore.getInitialState(), true)
  })

  it('waits durably for the route-owned hub connection', async () => {
    const hub = mockHub()
    mockWaitForHub.mockResolvedValueOnce(hub)

    await useSettingsStore.getState().connectHub('hub-1')

    expect(mockWaitForHub).toHaveBeenCalledWith('hub-1', null)
    expect(useSettingsStore.getState().hub).toBe(hub)
    expect(useSettingsStore.getState().connected).toBe(true)
  })

  it('marks settings disconnected when the hub disconnects', async () => {
    const hub = mockHub()
    mockWaitForHub.mockResolvedValueOnce(hub)

    await useSettingsStore.getState().connectHub('hub-1')
    hub.emitDisconnected()

    expect(useSettingsStore.getState().connected).toBe(false)
    expect(useSettingsStore.getState().treeState).toBe('disconnected')
    expect(useSettingsStore.getState().treeFeedback).toBe(
      'Hub disconnected. Reconnecting...',
    )
  })

  it('recovers settings state and refreshes data when the hub reconnects', async () => {
    const hub = mockHub({
      statFile: vi.fn((path) =>
        Promise.resolve({
          exists: ['agents', 'accessories', 'plugins', 'workspaces'].includes(path),
        }),
      ),
    })
    mockWaitForHub.mockResolvedValueOnce(hub)
    useSettingsStore.setState({ configScope: 'device' })

    await useSettingsStore.getState().connectHub('hub-1')
    hub.emitDisconnected()
    hub.emitConnected()

    expect(useSettingsStore.getState().connected).toBe(true)
    await vi.waitFor(() => {
      expect(hub.statFile).toHaveBeenCalledWith('agents', 'device', undefined)
      expect(hub.listInstalledTemplates).toHaveBeenCalledTimes(1)
      expect(useSettingsStore.getState().treeState).toBe('tree')
      expect(useSettingsStore.getState().installedStateLoaded).toBe(true)
    })
  })

  it('does not run duplicate recovery scans on initial hub connect', async () => {
    const hub = mockHub()
    mockWaitForHub.mockResolvedValueOnce(hub)
    useSettingsStore.setState({ configScope: 'device' })

    await useSettingsStore.getState().connectHub('hub-1')
    hub.emitConnected()

    expect(hub.statFile).not.toHaveBeenCalled()
    expect(hub.listInstalledTemplates).not.toHaveBeenCalled()
  })

  it('does not let an old hub reconnect mutate state after disconnect', async () => {
    const hub = mockHub()
    mockWaitForHub.mockResolvedValueOnce(hub)
    useSettingsStore.setState({ configScope: 'device' })

    await useSettingsStore.getState().connectHub('hub-1')
    hub.emitDisconnected()
    useSettingsStore.getState().disconnectHub()
    hub.emitConnected()

    expect(hub.unsubscribeDisconnected).toHaveBeenCalledTimes(1)
    expect(hub.unsubscribeConnected).toHaveBeenCalledTimes(1)
    expect(useSettingsStore.getState().hub).toBe(null)
    expect(useSettingsStore.getState().connected).toBe(false)
    expect(hub.statFile).not.toHaveBeenCalled()
    expect(hub.listInstalledTemplates).not.toHaveBeenCalled()
  })

  it('does not let an old hub reconnect mutate state after a newer connect', async () => {
    const oldHub = mockHub()
    const newHub = mockHub()
    mockWaitForHub.mockResolvedValueOnce(oldHub).mockResolvedValueOnce(newHub)
    useSettingsStore.setState({ configScope: 'device' })

    await useSettingsStore.getState().connectHub('hub-1')
    oldHub.emitDisconnected()
    await useSettingsStore.getState().connectHub('hub-1')
    oldHub.emitConnected()

    expect(useSettingsStore.getState().hub).toBe(newHub)
    expect(useSettingsStore.getState().connected).toBe(true)
    expect(oldHub.statFile).not.toHaveBeenCalled()
    expect(oldHub.listInstalledTemplates).not.toHaveBeenCalled()
  })

  it('tracks installed templates by destination file', async () => {
    const hub = mockHub({
      listInstalledTemplates: vi.fn(() => Promise.resolve({
        installed: [
          { dest: 'plugins/demo/init.lua', scope: 'device', name: 'demo' },
          { dest: 'plugins/demo/web_layout.lua', scope: 'device', name: 'demo' },
          { dest: 'agents/codex/notes.md', scope: 'repo', name: 'codex' },
        ],
      })),
    })
    useSettingsStore.setState({ hub, selectedTargetId: 'target-1' })

    await useSettingsStore.getState().checkInstalled()

    expect(useSettingsStore.getState().installedDevice).toEqual(
      new Set(['plugins/demo/init.lua', 'plugins/demo/web_layout.lua']),
    )
    expect(useSettingsStore.getState().installedRepo).toEqual(
      new Set(['agents/codex/notes.md']),
    )
  })

  it('does not add null for non-plugin template installs', async () => {
    const hub = mockHub()
    useSettingsStore.setState({ hub })

    await useSettingsStore
      .getState()
      .installTemplate('agents/codex/notes.md', 'Read me', 'device')

    expect(useSettingsStore.getState().installedDevice).toEqual(
      new Set(['agents/codex/notes.md']),
    )
    expect(hub.loadPlugin).not.toHaveBeenCalled()
  })

  it('refreshes the visible config tree after repairing a template file', async () => {
    const installedFiles = new Set(['plugins/demo/init.lua'])
    const hub = mockHub({
      installTemplate: vi.fn((dest) => {
        installedFiles.add(dest)
        return Promise.resolve()
      }),
      statFile: vi.fn((path) =>
        Promise.resolve({
          exists:
            ['agents', 'accessories', 'plugins', 'workspaces', 'plugins/demo'].includes(path) ||
            installedFiles.has(path),
        }),
      ),
      listDir: vi.fn((path) => {
        if (path === 'agents' || path === 'accessories' || path === 'workspaces') {
          return Promise.resolve({ entries: [] })
        }
        if (path === 'plugins') {
          return Promise.resolve({ entries: [{ name: 'demo', type: 'dir' }] })
        }
        if (path === 'plugins/demo') {
          return Promise.resolve({
            entries: [...installedFiles]
              .filter((file) => file.startsWith('plugins/demo/'))
              .map((file) => ({
                name: file.replace('plugins/demo/', ''),
                type: 'file',
              })),
          })
        }
        return Promise.resolve({ entries: [] })
      }),
    })
    useSettingsStore.setState({
      hub,
      configScope: 'device',
      treeState: 'tree',
      tree: {
        agents: {},
        accessories: {},
        workspaces: {},
        plugins: { demo: { init: true, files: ['init.lua'] } },
      },
      deviceTree: {
        agents: {},
        accessories: {},
        workspaces: {},
        plugins: { demo: { init: true, files: ['init.lua'] } },
      },
    })

    await useSettingsStore
      .getState()
      .installTemplate('plugins/demo/web_layout.lua', 'return {}', 'device')

    expect(useSettingsStore.getState().tree.plugins.demo.files).toEqual([
      'init.lua',
      'web_layout.lua',
    ])
    expect(useSettingsStore.getState().deviceTree.plugins.demo.files).toEqual([
      'init.lua',
      'web_layout.lua',
    ])
  })

  it('requests a fresh template catalog from the hub', async () => {
    const hub = mockHub()
    useSettingsStore.setState({ hub })

    await useSettingsStore.getState().refreshTemplates()

    expect(hub.refreshTemplates).toHaveBeenCalled()
    expect(useSettingsStore.getState().templateFeedback).toBe('Template refresh started.')
  })
})
