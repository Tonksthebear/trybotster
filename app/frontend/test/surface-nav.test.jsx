import React from 'react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, screen } from '@testing-library/react'
import { MemoryRouter } from 'react-router-dom'

import { SurfaceNav } from '../components/composites/SurfaceNav'
import { useSessionStore } from '../store/entities'
import { useRouteRegistryStore } from '../store/route-registry-store'

function fakeCtx(overrides = {}) {
  return {
    hubId: 'hub-1',
    viewport: {
      widthClass: 'regular',
      heightClass: 'regular',
      pointer: 'fine',
    },
    capabilities: {
      hover: true,
      dialog: true,
      tooltip: true,
      externalLinks: true,
      binaryTerminalSnapshots: false,
    },
    dispatch: vi.fn(),
    ...overrides,
  }
}

beforeEach(() => {
  useSessionStore.getState()._reset()
  useRouteRegistryStore.setState({
    routesByHubId: {},
    snapshotReceivedAtByHubId: {},
  })
})

afterEach(() => {
  cleanup()
})

describe('<SurfaceNav>', () => {
  it('renders route-registry surfaces as react-router links', () => {
    useRouteRegistryStore.getState().setRoutes('hub-1', [
      { path: '/', base_path: '/', surface: 'workspace_panel', label: 'Hub' },
      {
        path: '/vault',
        base_path: '/vault',
        surface: 'vault',
        label: 'Vault',
        icon: 'book-open',
        nav: { section: 'workspace', order: 25 },
      },
      {
        path: '/hidden',
        base_path: '/hidden',
        surface: 'hidden',
        label: 'Hidden',
        hide_from_nav: true,
      },
      {
        path: '/demo',
        base_path: '/demo',
        surface: 'demo',
        label: 'Demo',
        nav: false,
      },
    ])

    render(
      <MemoryRouter>
        <SurfaceNav density="sidebar" ctx={fakeCtx()} />
      </MemoryRouter>,
    )

    const link = screen.getByTestId('surface-nav-entry')
    expect(link).toHaveTextContent('Vault')
    expect(link).toHaveAttribute('href', '/hubs/hub-1/vault')
    expect(screen.queryByText('Hidden')).toBeNull()
    expect(screen.queryByText('Demo')).toBeNull()
  })

  it('is collapsible and marks surfaces with plugin-session notifications', () => {
    useRouteRegistryStore.getState().setRoutes('hub-1', [
      {
        path: '/vault',
        base_path: '/vault',
        surface: 'vault',
        label: 'Vault',
        nav: { section: 'workspace', order: 25 },
      },
    ])
    useSessionStore.setState({
      byId: {
        'sess-vault': {
          id: 'sess-vault',
          session_uuid: 'uuid-vault',
          owner_plugin: 'vault',
          visibility: 'plugin',
          surface: 'vault',
          notification: true,
        },
      },
      order: ['sess-vault'],
      snapshotSeq: 1,
    })

    render(
      <MemoryRouter>
        <SurfaceNav density="sidebar" ctx={fakeCtx()} />
      </MemoryRouter>,
    )

    const link = screen.getByTestId('surface-nav-entry')
    expect(link).toHaveAttribute('data-notification', 'true')
    expect(link.className).toContain('border-amber-400')

    fireEvent.click(screen.getByRole('button', { name: /plugins/i }))
    expect(screen.queryByTestId('surface-nav-entry')).toBeNull()
  })
})
