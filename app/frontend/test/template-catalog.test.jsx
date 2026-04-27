import React from 'react'
import { cleanup, fireEvent, render, screen } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'

import {
  displayTemplatesFor,
  splitTemplatesByInstallState,
  templateInstallState,
} from '../components/settings/TemplateCatalog'
import TemplateCatalog from '../components/settings/TemplateCatalog'
import { useSettingsStore } from '../store/settings-store'

beforeEach(() => {
  useSettingsStore.setState(useSettingsStore.getInitialState(), true)
})

afterEach(() => {
  cleanup()
})

describe('displayTemplatesFor', () => {
  it('groups multi-file plugins by plugin name', () => {
    const grouped = displayTemplatesFor([
      {
        slug: 'plugins-demo-web',
        name: 'Demo Surface',
        description: 'Demo',
        dest: 'plugins/demo-surface/web_layout.lua',
        content: 'return {}',
      },
      {
        slug: 'plugins-demo-init',
        name: 'Demo Surface',
        description: 'Demo',
        dest: 'plugins/demo-surface/init.lua',
        content: 'require("web_layout")',
      },
      {
        slug: 'plugins-demo-tui',
        name: 'Demo Surface',
        description: 'Demo',
        dest: 'plugins/demo-surface/tui/status.lua',
        content: 'botster.ui.register_component(...)',
      },
    ])

    expect(grouped).toHaveLength(1)
    expect(grouped[0].dest).toBe('plugins/demo-surface/init.lua')
    expect(grouped[0].files.map((file) => file.dest)).toEqual([
      'plugins/demo-surface/init.lua',
      'plugins/demo-surface/tui/status.lua',
      'plugins/demo-surface/web_layout.lua',
    ])
  })

  it('groups multi-file agent definitions by agent name', () => {
    const grouped = displayTemplatesFor([
      {
        slug: 'agents-claude-notes',
        name: 'Claude',
        description: 'Claude',
        dest: 'agents/claude/notes.md',
        content: 'Read me with botster context file notes.md.',
      },
      {
        slug: 'agents-claude-init',
        name: 'Claude',
        description: 'Claude',
        dest: 'agents/claude/initialization',
        content: '#!/bin/bash\n',
      },
    ])

    expect(grouped).toHaveLength(1)
    expect(grouped[0].dest).toBe('agents/claude/initialization')
    expect(grouped[0].files.map((file) => file.dest)).toEqual([
      'agents/claude/initialization',
      'agents/claude/notes.md',
    ])
  })

  it('reports complete, partial, and missing grouped template install state by file', () => {
    const grouped = displayTemplatesFor([
      {
        slug: 'plugins-demo-init',
        name: 'Demo Surface',
        description: 'Demo',
        dest: 'plugins/demo-surface/init.lua',
        content: 'require("web_layout")',
      },
      {
        slug: 'plugins-demo-web',
        name: 'Demo Surface',
        description: 'Demo',
        dest: 'plugins/demo-surface/web_layout.lua',
        content: 'return {}',
      },
    ])[0]

    expect(templateInstallState(grouped, new Set())).toMatchObject({
      installed: 0,
      total: 2,
      complete: false,
      partial: false,
    })
    expect(templateInstallState(grouped, new Set(['plugins/demo-surface/init.lua']))).toMatchObject({
      installed: 1,
      total: 2,
      complete: false,
      partial: true,
    })
    expect(
      templateInstallState(
        grouped,
        new Set(['plugins/demo-surface/init.lua', 'plugins/demo-surface/web_layout.lua']),
      ),
    ).toMatchObject({
      installed: 2,
      total: 2,
      complete: true,
      partial: false,
    })
  })

  it('splits templates into installed and available for the selected scope', () => {
    const grouped = displayTemplatesFor([
      {
        slug: 'plugins-demo-init',
        name: 'Demo Surface',
        description: 'Demo',
        dest: 'plugins/demo/init.lua',
        content: 'return {}',
      },
      {
        slug: 'plugins-empty-init',
        name: 'Empty Surface',
        description: 'Empty',
        dest: 'plugins/empty/init.lua',
        content: 'return {}',
      },
    ])

    const split = splitTemplatesByInstallState(
      grouped,
      new Set(['plugins/demo/init.lua']),
    )

    expect(split.installed.map((template) => template.slug)).toEqual(['plugins-demo'])
    expect(split.available.map((template) => template.slug)).toEqual(['plugins-empty'])
  })

  it('browses templates within one selected scope before showing repo targets', () => {
    useSettingsStore.setState({
      installedDevice: new Set(['plugins/demo/init.lua']),
      installedRepo: new Set(),
      spawnTargets: [{ id: 'target-1', name: 'Repo One', path: '/repo' }],
    })

    render(
      <TemplateCatalog
        templates={{
          plugins: [
            {
              slug: 'plugins-demo-init',
              name: 'Demo Surface',
              description: 'Demo',
              dest: 'plugins/demo/init.lua',
              content: 'return {}',
            },
            {
              slug: 'plugins-empty-init',
              name: 'Empty Surface',
              description: 'Empty',
              dest: 'plugins/empty/init.lua',
              content: 'return {}',
            },
          ],
        }}
      />,
    )

    expect(screen.getByText('Installed in Device')).toBeInTheDocument()
    expect(screen.getByText('Available for Device')).toBeInTheDocument()
    expect(screen.queryByText('Repo Target')).not.toBeInTheDocument()

    fireEvent.click(screen.getByRole('button', { name: 'Repository' }))

    expect(screen.getByText('Available for Repository')).toBeInTheDocument()
    expect(screen.getByText('Repo Target')).toBeInTheDocument()
  })
})
