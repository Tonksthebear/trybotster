import React from 'react'
import { cleanup, render, screen } from '@testing-library/react'
import { MemoryRouter } from 'react-router-dom'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'

import SettingsPage from '../components/settings/SettingsPage'
import { useSettingsStore } from '../store/settings-store'
import { useTemplateStore } from '../store/entities'

describe('SettingsPage', () => {
  beforeEach(() => {
    useSettingsStore.setState(useSettingsStore.getInitialState(), true)
    useTemplateStore.setState(useTemplateStore.getInitialState(), true)
  })

  afterEach(() => {
    cleanup()
  })

  it('renders hub-authored template entities without an unstable selector loop', () => {
    useSettingsStore.setState({ activeTab: 'templates', installedStateLoaded: true })
    useTemplateStore.getState().applySnapshot(
      [
        {
          id: 'plugins-demo-init',
          slug: 'plugins-demo-init',
          category: 'plugins',
          name: 'Demo Plugin',
          description: 'Demo plugin',
          dest: 'plugins/demo/init.lua',
          content: 'return {}',
        },
      ],
      1,
    )

    render(
      <MemoryRouter>
        <SettingsPage
          hubId="hub-1"
          hubName="Hub One"
          hubIdentifier="hub-one"
          hubSettingsPath="/hubs/hub-1/settings"
          hubPath="/hubs/hub-1"
        />
      </MemoryRouter>,
    )

    expect(screen.getByText('Settings')).toBeInTheDocument()
    expect(screen.getByText('Demo Plugin')).toBeInTheDocument()
  })
})
