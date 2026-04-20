import React from 'react'
import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest'
import { render, screen, waitFor, act, cleanup } from '@testing-library/react'
import { MemoryRouter, useLocation } from 'react-router-dom'
import { AppRoutes } from '../components/AppShell'
import { resetHubListSubscriptionForTest, useHubStore } from '../store/hub-store'
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
  SidebarLayout: ({ navbar, sidebar, children }) => (
    <div>
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
  default: ({ hubId, targetSurface, children }) => (
    <div>
      <div>{`UiTree:${hubId}:${targetSurface}`}</div>
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
  })

  afterEach(() => {
    cleanup()
    resetHubListSubscriptionForTest()
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
})
