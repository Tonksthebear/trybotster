import { describe, it, expect, vi } from 'vitest'
import { scanSettingsTree } from '../lib/settings-store-helpers'

function mockHub(entriesByPath) {
  return {
    statFile: vi.fn((path) => Promise.resolve({ exists: path in entriesByPath })),
    listDir: vi.fn((path) => Promise.resolve({ entries: entriesByPath[path] || [] })),
  }
}

describe('settings store helpers', () => {
  it('recursively scans plugin and session definition files', async () => {
    const hub = mockHub({
      agents: [{ name: 'codex', type: 'dir' }],
      'agents/codex': [
        { name: 'initialization', type: 'file' },
        { name: 'notes.md', type: 'file' },
      ],
      'agents/codex/initialization': [],
      accessories: [],
      plugins: [{ name: 'demo', type: 'dir' }],
      'plugins/demo': [
        { name: 'init.lua', type: 'file' },
        { name: 'web_layout.lua', type: 'file' },
        { name: 'tui', type: 'dir' },
      ],
      'plugins/demo/init.lua': [],
      'plugins/demo/tui': [{ name: 'status.lua', type: 'file' }],
      workspaces: [],
    })

    const result = await scanSettingsTree({
      hub,
      configScope: 'device',
      selectedTargetId: null,
    })

    expect(result.state).toBe('tree')
    expect(result.tree.agents.codex.files).toEqual(['initialization', 'notes.md'])
    expect(result.tree.plugins.demo.files).toEqual(['init.lua', 'tui/status.lua', 'web_layout.lua'])
  })

  it('keeps partial plugin directories visible when init.lua is missing', async () => {
    const hub = mockHub({
      agents: [],
      accessories: [],
      plugins: [{ name: 'demo', type: 'dir' }],
      'plugins/demo': [{ name: 'web_layout.lua', type: 'file' }],
      workspaces: [],
    })

    const result = await scanSettingsTree({
      hub,
      configScope: 'device',
      selectedTargetId: null,
    })

    expect(result.state).toBe('tree')
    expect(result.tree.plugins.demo).toEqual({
      init: false,
      files: ['web_layout.lua'],
    })
  })

  it('ignores legacy plugin database directories without init.lua', async () => {
    const hub = mockHub({
      agents: [],
      accessories: [],
      plugins: [
        { name: 'repo__Users_jason_Rails_trybotster_rails-worktree-lifecycle', type: 'dir' },
        { name: 'demo', type: 'dir' },
      ],
      'plugins/repo__Users_jason_Rails_trybotster_rails-worktree-lifecycle': [
        { name: 'db.sqlite', type: 'file' },
        { name: 'db.sqlite-shm', type: 'file' },
        { name: 'db.sqlite-wal', type: 'file' },
      ],
      'plugins/demo': [
        { name: 'init.lua', type: 'file' },
      ],
      'plugins/demo/init.lua': [],
      workspaces: [],
    })

    const result = await scanSettingsTree({
      hub,
      configScope: 'device',
      selectedTargetId: null,
    })

    expect(result.state).toBe('tree')
    expect(result.tree.plugins).toEqual({
      demo: {
        init: true,
        files: ['init.lua'],
      },
    })
  })
})
