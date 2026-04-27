import React from 'react'
import { cleanup, render, screen } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'

import ConfigEditor from '../components/settings/ConfigEditor'
import { useSettingsStore } from '../store/settings-store'

describe('ConfigEditor installed configuration browser', () => {
  beforeEach(() => {
    useSettingsStore.setState(useSettingsStore.getInitialState(), true)
  })

  afterEach(() => {
    cleanup()
  })

  it('browses installed configuration units by type, name, and contained files', () => {
    useSettingsStore.setState({
      configScope: 'device',
      treeState: 'tree',
      tree: {
        agents: {
          claude: {
            initialization: true,
            files: ['initialization', 'notes.md'],
          },
        },
        accessories: {},
        workspaces: {},
        plugins: {
          demo: {
            init: false,
            files: ['web_layout.lua'],
          },
        },
      },
    })

    render(<ConfigEditor agentTemplates={[]} />)

    expect(screen.getByText('Agents')).toBeInTheDocument()
    expect(screen.getByText('Claude')).toBeInTheDocument()
    expect(screen.getByText('agent')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /initialization/ })).toHaveAttribute(
      'data-file-path',
      'agents/claude/initialization',
    )
    expect(screen.getByRole('button', { name: /notes\.md/ })).toHaveAttribute(
      'data-file-path',
      'agents/claude/notes.md',
    )

    expect(screen.getByText('Plugins')).toBeInTheDocument()
    expect(screen.getByText('Demo')).toBeInTheDocument()
    expect(screen.getByText('plugin')).toBeInTheDocument()
    expect(screen.getByText('missing init.lua')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /init\.lua/ })).toHaveAttribute(
      'data-file-path',
      'plugins/demo/init.lua',
    )
    expect(screen.getByRole('button', { name: /web_layout\.lua/ })).toHaveAttribute(
      'data-file-path',
      'plugins/demo/web_layout.lua',
    )
  })
})
