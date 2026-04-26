import { describe, it, expect, vi } from 'vitest'
import {
  applyQuickSetup,
  createSessionConfig,
  createSettingsFile,
  deleteSettingsFile,
  installSettingsTemplate,
  readSettingsFile,
  removeSessionConfig,
  renameSettingsFile,
  setPortForward,
  uninstallSettingsTemplate,
  writeSettingsFile,
} from '../lib/settings-commands'

function mockHub(overrides = {}) {
  return {
    statFile: vi.fn(() => Promise.resolve({ exists: true })),
    readFile: vi.fn(() => Promise.resolve({ content: 'hello' })),
    writeFile: vi.fn(() => Promise.resolve()),
    mkDir: vi.fn(() => Promise.resolve()),
    deleteFile: vi.fn(() => Promise.resolve()),
    renameFile: vi.fn(() => Promise.resolve()),
    rmDir: vi.fn(() => Promise.resolve()),
    installTemplate: vi.fn(() => Promise.resolve()),
    uninstallTemplate: vi.fn(() => Promise.resolve()),
    loadPlugin: vi.fn(() => Promise.resolve()),
    ...overrides,
  }
}

describe('settings command helpers', () => {
  it('reads an existing repo file through the hub filesystem context', async () => {
    const hub = mockHub()

    const result = await readSettingsFile({
      hub,
      configScope: 'repo',
      selectedTargetId: 'target-1',
      filePath: '.botster/agents/claude/initialization',
    })

    expect(result).toEqual({ exists: true, content: 'hello' })
    expect(hub.statFile).toHaveBeenCalledWith('.botster/agents/claude/initialization', 'repo', 'target-1')
    expect(hub.readFile).toHaveBeenCalledWith('.botster/agents/claude/initialization', 'repo', 'target-1')
  })

  it('creates a missing settings file with default content', async () => {
    const hub = mockHub()

    const content = await createSettingsFile({
      hub,
      configMetadata: { session_files: { initialization: { default: '#!/bin/custom\n' } } },
      configScope: 'device',
      selectedTargetId: null,
      filePath: 'agents/claude/initialization',
      content: '',
    })

    expect(content).toBe('#!/bin/custom\n')
    expect(hub.mkDir).toHaveBeenCalledWith('agents/claude', 'device', undefined)
    expect(hub.writeFile).toHaveBeenCalledWith('agents/claude/initialization', '#!/bin/custom\n', 'device', undefined)
  })

  it('delegates basic file mutations to the hub', async () => {
    const hub = mockHub()

    await writeSettingsFile({ hub, configScope: 'repo', selectedTargetId: 't1', filePath: '.botster/a', content: 'x' })
    await deleteSettingsFile({ hub, configScope: 'repo', selectedTargetId: 't1', filePath: '.botster/a' })
    await renameSettingsFile({ hub, configScope: 'repo', selectedTargetId: 't1', filePath: '.botster/a', newPath: '.botster/b' })
    await setPortForward({ hub, configScope: 'repo', selectedTargetId: 't1', filePath: '.botster/accessories/web/port_forward', enabled: true })

    expect(hub.writeFile).toHaveBeenCalledWith('.botster/a', 'x', 'repo', 't1')
    expect(hub.deleteFile).toHaveBeenCalledWith('.botster/a', 'repo', 't1')
    expect(hub.renameFile).toHaveBeenCalledWith('.botster/a', '.botster/b', 'repo', 't1')
    expect(hub.mkDir).toHaveBeenCalledWith('.botster/accessories/web', 'repo', 't1')
    expect(hub.writeFile).toHaveBeenCalledWith('.botster/accessories/web/port_forward', '', 'repo', 't1')
  })

  it('creates and removes session config directories', async () => {
    const hub = mockHub()

    await createSessionConfig({
      hub,
      configMetadata: {},
      configScope: 'repo',
      selectedTargetId: 'target-1',
      type: 'agents',
      name: 'claude',
    })
    await removeSessionConfig({
      hub,
      configScope: 'repo',
      selectedTargetId: 'target-1',
      type: 'agents',
      name: 'claude',
    })

    expect(hub.mkDir).toHaveBeenCalledWith('.botster/agents/claude', 'repo', 'target-1')
    expect(hub.writeFile).toHaveBeenCalledWith(
      '.botster/agents/claude/initialization',
      '#!/bin/bash\n',
      'repo',
      'target-1',
    )
    expect(hub.rmDir).toHaveBeenCalledWith('.botster/agents/claude', 'repo', 'target-1')
  })

  it('applies quick setup and template commands', async () => {
    const hub = mockHub()

    await applyQuickSetup({
      hub,
      configScope: 'repo',
      selectedTargetId: 'target-1',
      dest: 'plugins/demo/init.lua',
      content: 'return {}',
    })
    const installed = await installSettingsTemplate({
      hub,
      dest: 'plugins/demo/init.lua',
      content: 'return {}',
      scope: 'repo',
      targetId: 'target-1',
    })
    const uninstalled = await uninstallSettingsTemplate({
      hub,
      dest: 'plugins/demo/init.lua',
      scope: 'repo',
      targetId: 'target-1',
    })

    expect(hub.mkDir).toHaveBeenCalledWith('.botster/plugins/demo', 'repo', 'target-1')
    expect(hub.writeFile).toHaveBeenCalledWith('.botster/plugins/demo/init.lua', 'return {}', 'repo', 'target-1')
    expect(installed).toBe('demo')
    expect(uninstalled).toBe('demo')
    expect(hub.installTemplate).toHaveBeenCalledWith('plugins/demo/init.lua', 'return {}', 'repo', 'target-1')
    expect(hub.loadPlugin).toHaveBeenCalledWith('demo', 'target-1')
    expect(hub.uninstallTemplate).toHaveBeenCalledWith('plugins/demo/init.lua', 'repo', 'target-1')
  })
})
