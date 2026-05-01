import React from 'react'
import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest'
import { render, screen, waitFor, act, cleanup } from '@testing-library/react'
import { MemoryRouter, useLocation } from 'react-router-dom'
import { AppRoutes } from '../components/AppShell'
import { resetHubListSubscriptionForTest, useHubStore } from '../store/hub-store'
import { useRouteRegistryStore } from '../store/route-registry-store'
import { setHubId } from '../lib/modal-bridge'

vi.mock('../lib/transport/hub_signaling_client', () => ({
  getActionCableConsumer: vi.fn(async () => ({
    subscriptions: {
      create: vi.fn(() => ({ unsubscribe: vi.fn() })),
    },
  })),
}))

vi.mock('../components/pages/Home', () => ({
  default: () => <div>Home Route</div>,
}))

vi.mock('../components/pages/HubDashboard', () => ({
  default: () => <div>Hub Dashboard</div>,
}))

vi.mock('../components/pages/HubShow', () => ({
  default: () => <div>Hub Show</div>,
}))

vi.mock('../components/pages/SettingsRoute', () => ({
  default: () => <div>Hub Settings Route</div>,
}))

vi.mock('../components/pages/PairingRoute', () => ({
  default: () => <div>Hub Pairing Route</div>,
}))

vi.mock('../components/catalyst/sidebar-layout', () => ({
  SidebarLayout: ({ navbar, sidebar, children, flush }) => (
    <div data-testid="sidebar-layout" data-flush={flush ? 'true' : 'false'}>
      <div>{navbar}</div>
      <div>{sidebar}</div>
      <div>{children}</div>
    </div>
  ),
}))

vi.mock('../components/catalyst/sidebar', () => ({
  Sidebar: ({ children }) => <div>{children}</div>,
  SidebarHeader: ({ children }) => <div>{children}</div>,
  SidebarBody: ({ children }) => <div>{children}</div>,
  SidebarFooter: ({ children }) => <div>{children}</div>,
  SidebarSection: ({ children }) => <div>{children}</div>,
  SidebarItem: ({ children, href, onClick, current }) =>
    href ? (
      <a href={href} data-current={current ? 'true' : 'false'}>
        {children}
      </a>
    ) : (
      <button type="button" onClick={onClick}>
        {children}
      </button>
    ),
  SidebarLabel: ({ children, className = '' }) => <span className={className}>{children}</span>,
  SidebarHeading: ({ children }) => <div>{children}</div>,
  SidebarSpacer: () => <div />,
}))

vi.mock('../components/catalyst/navbar', () => ({
  Navbar: ({ children }) => <div>{children}</div>,
  NavbarItem: ({ children, href }) => <a href={href}>{children}</a>,
  NavbarSpacer: () => <div />,
}))

vi.mock('../components/UiTree', () => ({
  default: ({ hubId, targetSurface, subpath, children }) => (
    <div data-testid={`ui-tree-${targetSurface}`}>
      <div>{`UiTree:${hubId}:${targetSurface}:${subpath || '/'}`}</div>
      {children}
    </div>
  ),
}))

vi.mock('../components/workspace/SessionActionsMenu', () => ({
  default: () => <div>SessionActionsMenu</div>,
}))

vi.mock('../components/hub/HubSwitcher', () => ({
  default: () => <div>HubSwitcher</div>,
}))

vi.mock('../components/hub/SidebarConnectionStatus', () => ({
  default: () => <div>SidebarConnectionStatus</div>,
}))

vi.mock('../components/hub/ConnectionOverlay', () => ({
  default: ({ suppress }) => <div>{`ConnectionOverlay:${suppress ? 'suppressed' : 'visible'}`}</div>,
}))

vi.mock('../components/DialogHost', () => ({
  default: ({ hubId }) => <div>{`DialogHost:${hubId}`}</div>,
}))

vi.mock('../components/terminal/TerminalCache', () => ({
  default: ({ hubId }) => <div>{`TerminalCache:${hubId}`}</div>,
}))

vi.mock('../lib/modal-bridge', () => ({
  setHubId: vi.fn(),
}))

function LocationProbe() {
  const location = useLocation()
  return <div data-testid="location">{`${location.pathname}${location.search}`}</div>
}

function renderRoutes(initialEntry) {
  return render(
    <MemoryRouter initialEntries={[initialEntry]}>
      <React.Suspense fallback={<div>Loading...</div>}>
        <AppRoutes />
        <LocationProbe />
      </React.Suspense>
    </MemoryRouter>
  )
}

describe('AppRoutes', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    localStorage.clear()
    resetHubListSubscriptionForTest()

    useHubStore.setState({
      hubList: [],
      hubListLoading: false,
      selectedHubId: null,
      connectionState: 'disconnected',
      connectionDetail: '',
      _connectionRef: null,
      _statusUnsub: null,
      fetchHubList: vi.fn(() => Promise.resolve([])),
      selectHub: vi.fn(() => Promise.resolve()),
      disconnectHub: vi.fn(),
      getLastHubId: vi.fn(() => null),
    })
    useRouteRegistryStore.setState({
      routesByHubId: {},
      snapshotReceivedAtByHubId: {},
    })
  })

  afterEach(() => {
    cleanup()
    resetHubListSubscriptionForTest()
    useRouteRegistryStore.setState({
      routesByHubId: {},
      snapshotReceivedAtByHubId: {},
    })
    vi.useRealTimers()
  })

  it('renders the home route', async () => {
    renderRoutes('/')

    expect(await screen.findByText('Home Route')).toBeInTheDocument()
    expect(screen.getByTestId('location')).toHaveTextContent('/')
  })

  it('auto-selects the last hub when visiting /hubs', async () => {
    const hubs = [{ id: 3, name: 'Hub Three', identifier: 'hub-3', active: true }]
    const selectHub = vi.fn(() => Promise.resolve())

    useHubStore.setState({
      hubList: hubs,
      hubListLoading: false,
      fetchHubList: vi.fn(() => Promise.resolve(hubs)),
      selectHub,
      getLastHubId: vi.fn(() => '3'),
    })

    renderRoutes('/hubs')

    await waitFor(() => {
      expect(selectHub).toHaveBeenCalledWith(3)
      expect(screen.getByTestId('location')).toHaveTextContent('/hubs/3')
    })
  })

  it('redirects a stale deep hub URL to a valid last hub without selecting the stale hub', async () => {
    const hubs = [
      { id: 3, name: 'Hub Three', identifier: 'hub-3', active: true },
      { id: 4, name: 'Hub Four', identifier: 'hub-4', active: true },
    ]
    const selectHub = vi.fn(() => Promise.resolve())

    localStorage.setItem('botster:lastHubId', '3')
    useHubStore.setState({
      hubList: hubs,
      hubListLoading: false,
      fetchHubList: vi.fn(() => Promise.resolve(hubs)),
      selectHub,
      getLastHubId: vi.fn(() => localStorage.getItem('botster:lastHubId')),
    })

    renderRoutes('/hubs/99/settings')

    await waitFor(() => {
      expect(screen.getByTestId('location')).toHaveTextContent('/hubs/3')
      expect(selectHub).toHaveBeenCalledWith(3)
    })
    expect(selectHub).not.toHaveBeenCalledWith('99')
    expect(localStorage.getItem('botster:lastHubId')).toBe('3')
  })

  it('preselects the route hub during a deep reload before the hub list validates it', async () => {
    const selectHub = vi.fn(() => Promise.resolve())

    useHubStore.setState({
      hubList: [],
      hubListLoading: true,
      fetchHubList: vi.fn(() => Promise.resolve([])),
      selectHub,
      getLastHubId: vi.fn(() => null),
    })

    renderRoutes('/hubs/7/settings')

    await waitFor(() => {
      expect(selectHub).toHaveBeenCalledWith('7', { persistLastHub: false })
      expect(screen.getByTestId('location')).toHaveTextContent('/hubs/7/settings')
    })
  })

  it('redirects a stale deep hub URL to the sole hub when no valid last hub exists', async () => {
    const hubs = [{ id: 7, name: 'Only Hub', identifier: 'hub-7', active: true }]
    const selectHub = vi.fn(() => Promise.resolve())

    localStorage.setItem('botster:lastHubId', '99')
    useHubStore.setState({
      hubList: hubs,
      hubListLoading: false,
      fetchHubList: vi.fn(() => Promise.resolve(hubs)),
      selectHub,
      getLastHubId: vi.fn(() => localStorage.getItem('botster:lastHubId')),
    })

    renderRoutes('/hubs/99/settings')

    await waitFor(() => {
      expect(screen.getByTestId('location')).toHaveTextContent('/hubs/7')
      expect(selectHub).toHaveBeenCalledWith(7)
    })
    expect(selectHub).not.toHaveBeenCalledWith('99')
    expect(localStorage.getItem('botster:lastHubId')).toBe('99')
  })

  it('redirects a stale deep hub URL to /hubs when no fallback exists', async () => {
    const hubs = [
      { id: 3, name: 'Hub Three', identifier: 'hub-3', active: true },
      { id: 4, name: 'Hub Four', identifier: 'hub-4', active: true },
    ]
    const selectHub = vi.fn(() => Promise.resolve())

    useHubStore.setState({
      hubList: hubs,
      hubListLoading: false,
      fetchHubList: vi.fn(() => Promise.resolve(hubs)),
      selectHub,
      getLastHubId: vi.fn(() => null),
    })

    renderRoutes('/hubs/99/settings')

    await waitFor(() => {
      expect(screen.getByTestId('location')).toHaveTextContent('/hubs')
    })
    expect(selectHub).not.toHaveBeenCalled()
    expect(localStorage.getItem('botster:lastHubId')).toBe(null)
  })

  it('clears an optimistically selected stale route hub when validation has no fallback', async () => {
    const hubs = [
      { id: 3, name: 'Hub Three', identifier: 'hub-3', active: true },
      { id: 4, name: 'Hub Four', identifier: 'hub-4', active: true },
    ]
    const selectHub = vi.fn(() => Promise.resolve())

    useHubStore.setState({
      hubList: hubs,
      hubListLoading: false,
      selectedHubId: '99',
      fetchHubList: vi.fn(() => Promise.resolve(hubs)),
      selectHub,
      getLastHubId: vi.fn(() => null),
    })

    renderRoutes('/hubs/99/settings')

    await waitFor(() => {
      expect(screen.getByTestId('location')).toHaveTextContent('/hubs')
      expect(selectHub).toHaveBeenCalledWith(null)
    })
  })

  it('keeps an optimistically selected route hub when the hub list is empty', async () => {
    const selectHub = vi.fn(() => Promise.resolve())

    useHubStore.setState({
      hubList: [],
      hubListLoading: false,
      selectedHubId: '7',
      fetchHubList: vi.fn(() => Promise.resolve([])),
      selectHub,
      getLastHubId: vi.fn(() => null),
    })

    renderRoutes('/hubs/7/settings')

    await waitFor(() => {
      expect(screen.getByTestId('location')).toHaveTextContent('/hubs/7/settings')
    })
    expect(selectHub).not.toHaveBeenCalled()
  })

  it('keeps a valid deep hub URL selected and unchanged', async () => {
    const hubs = [{ id: 7, name: 'Fresh Hub', identifier: 'hub-7', active: true }]
    const selectHub = vi.fn(() => Promise.resolve())

    useHubStore.setState({
      hubList: hubs,
      hubListLoading: false,
      fetchHubList: vi.fn(() => Promise.resolve(hubs)),
      selectHub,
      getLastHubId: vi.fn(() => null),
    })

    renderRoutes('/hubs/7/pairing')

    await waitFor(() => {
      expect(selectHub).toHaveBeenCalledWith(7)
      expect(screen.getByTestId('location')).toHaveTextContent('/hubs/7/pairing')
    })
    expect(await screen.findByText('Hub Pairing Route')).toBeInTheDocument()
  })

  it('suppresses normal auto-navigation while the booting handoff is active', async () => {
    const selectHub = vi.fn(() => Promise.resolve())
    const hubs = [{ id: 7, name: 'Fresh Hub', identifier: 'hub-7', active: true }]
    const fetchHubList = vi.fn().mockResolvedValue(hubs)

    useHubStore.setState({
      hubList: hubs,
      hubListLoading: false,
      fetchHubList,
      selectHub,
      getLastHubId: vi.fn(() => '7'),
    })

    renderRoutes('/hubs?booting=1')

    await waitFor(() => {
      expect(fetchHubList).toHaveBeenCalledTimes(2)
    })

    expect(selectHub).not.toHaveBeenCalled()
    expect(screen.getByTestId('location')).toHaveTextContent('/hubs?booting=1')
  })

  it('claims a newly approved hub on the first booting poll when the hub list includes the pending fingerprint', async () => {
    const selectHub = vi.fn(() => Promise.resolve())
    const hubs = [{ id: 7, name: 'Fresh Hub', identifier: 'hub-7', fingerprint: 'aa:bb', active: true }]
    const fetchHubList = vi.fn().mockResolvedValue(hubs)

    useHubStore.setState({
      hubList: [],
      hubListLoading: false,
      fetchHubList,
      selectHub,
      getLastHubId: vi.fn(() => '99'),
    })

    renderRoutes('/hubs?booting=1&pending_fingerprint=aa%3Abb')

    await waitFor(() => {
      expect(selectHub).toHaveBeenCalledWith(7)
      expect(screen.getByTestId('location')).toHaveTextContent('/hubs/7')
    })
  })

  it('syncs the selected hub ID into the modal bridge on hub routes', async () => {
    useHubStore.setState({
      hubList: [{ id: 42, name: 'Hub', identifier: 'hub-42', active: true }],
      hubListLoading: false,
      selectedHubId: '42',
      fetchHubList: vi.fn(() => Promise.resolve([{ id: 42, name: 'Hub', identifier: 'hub-42', active: true }])),
    })

    renderRoutes('/hubs/42')

    expect(await screen.findByText('Hub Show')).toBeInTheDocument()

    await waitFor(() => {
      expect(setHubId).toHaveBeenCalledWith('42')
      expect(screen.getByText('DialogHost:42')).toBeInTheDocument()
    })
  })

  it('renders plugin-owned session routes in the flush shell without TerminalCache', async () => {
    const hubs = [{ id: 'hub-1', name: 'Hub', identifier: 'hub-1', active: true }]
    useHubStore.setState({
      hubList: hubs,
      hubListLoading: false,
      selectedHubId: 'hub-1',
      fetchHubList: vi.fn(() => Promise.resolve(hubs)),
    })
    useRouteRegistryStore.getState().setRoutes('hub-1', [
      {
        path: '/vault',
        base_path: '/vault',
        surface: 'vault',
        label: 'Vault',
        routes: [{ path: '/' }, { path: '/sessions/:session_uuid' }],
      },
    ])

    renderRoutes('/hubs/hub-1/vault/sessions/sess-1')

    expect(await screen.findByTestId('sidebar-layout', {}, { timeout: 3000 })).toHaveAttribute('data-flush', 'true')
    expect(screen.queryByText('TerminalCache:hub-1')).toBeNull()
    expect(screen.getByTestId('ui-tree-vault')).toHaveTextContent('UiTree:hub-1:vault:/sessions/sess-1')
  })

  it('renders fullscreen plugin routes in the flush shell', async () => {
    const hubs = [{ id: 'hub-1', name: 'Hub', identifier: 'hub-1', active: true }]
    useHubStore.setState({
      hubList: hubs,
      hubListLoading: false,
      selectedHubId: 'hub-1',
      fetchHubList: vi.fn(() => Promise.resolve(hubs)),
    })
    useRouteRegistryStore.getState().setRoutes('hub-1', [
      {
        path: '/vault',
        base_path: '/vault',
        surface: 'vault',
        label: 'Vault',
        routes: [{ path: '/' }, { path: '/graph', layout: 'fullscreen' }],
      },
    ])

    renderRoutes('/hubs/hub-1/vault/graph')

    expect(await screen.findByTestId('sidebar-layout')).toHaveAttribute('data-flush', 'true')
    expect(screen.queryByText('TerminalCache:hub-1')).toBeNull()
    expect(screen.getByTestId('ui-tree-vault')).toHaveTextContent('UiTree:hub-1:vault:/graph')
  })

  it('swaps to a plugin sidebar with a back button when the active surface declares one', async () => {
    const hubs = [{ id: 'hub-1', name: 'Hub', identifier: 'hub-1', active: true }]
    useHubStore.setState({
      hubList: hubs,
      hubListLoading: false,
      selectedHubId: 'hub-1',
      fetchHubList: vi.fn(() => Promise.resolve(hubs)),
    })
    useRouteRegistryStore.getState().setRoutes('hub-1', [
      {
        path: '/vault',
        base_path: '/vault',
        surface: 'vault',
        label: 'Vault',
        sidebar: { surface: 'vault_sidebar' },
        routes: [{ path: '/' }],
      },
    ])

    renderRoutes('/hubs/hub-1/vault')

    expect(await screen.findByTestId('plugin-sidebar-title')).toHaveTextContent('Vault')
    expect(screen.getByTestId('ui-tree-vault_sidebar')).toHaveTextContent('UiTree:hub-1:vault_sidebar:/')
    expect(screen.queryByTestId('ui-tree-workspace_sidebar')).toBeNull()
    expect(screen.getByTestId('plugin-sidebar-back')).toBeInTheDocument()
  })
})
