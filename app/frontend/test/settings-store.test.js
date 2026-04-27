import { beforeEach, describe, expect, it, vi } from 'vitest'
import { useSettingsStore } from '../store/settings-store'

function mockHub(overrides = {}) {
  return {
    listInstalledTemplates: vi.fn(() => Promise.resolve({ installed: [] })),
    installTemplate: vi.fn(() => Promise.resolve()),
    uninstallTemplate: vi.fn(() => Promise.resolve()),
    loadPlugin: vi.fn(() => Promise.resolve()),
    ...overrides,
  }
}

describe('settings store template install state', () => {
  beforeEach(() => {
    useSettingsStore.setState(useSettingsStore.getInitialState(), true)
  })

  it('tracks installed templates by destination file', async () => {
    const hub = mockHub({
      listInstalledTemplates: vi.fn(() => Promise.resolve({
        installed: [
          { dest: 'plugins/demo/init.lua', scope: 'device', name: 'demo' },
          { dest: 'plugins/demo/web_layout.lua', scope: 'device', name: 'demo' },
          { dest: 'agents/claude/notes.md', scope: 'repo', name: 'claude' },
        ],
      })),
    })
    useSettingsStore.setState({ hub, selectedTargetId: 'target-1' })

    await useSettingsStore.getState().checkInstalled()

    expect(useSettingsStore.getState().installedDevice).toEqual(
      new Set(['plugins/demo/init.lua', 'plugins/demo/web_layout.lua']),
    )
    expect(useSettingsStore.getState().installedRepo).toEqual(
      new Set(['agents/claude/notes.md']),
    )
  })

  it('does not add null for non-plugin template installs', async () => {
    const hub = mockHub()
    useSettingsStore.setState({ hub })

    await useSettingsStore
      .getState()
      .installTemplate('agents/claude/notes.md', 'Read me', 'device')

    expect(useSettingsStore.getState().installedDevice).toEqual(
      new Set(['agents/claude/notes.md']),
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
})
