import React, { useEffect } from 'react'
import {
  BrowserRouter,
  Routes,
  Route,
  Outlet,
  useParams,
  useLocation,
  useNavigate,
} from 'react-router-dom'
import { SidebarLayout } from './catalyst/sidebar-layout'
import {
  Sidebar,
  SidebarHeader,
  SidebarBody,
  SidebarFooter,
  SidebarSection,
  SidebarItem,
  SidebarLabel,
  SidebarHeading,
  SidebarSpacer,
} from './catalyst/sidebar'
import { Navbar, NavbarItem, NavbarSpacer } from './catalyst/navbar'
import WorkspaceList from './workspace/WorkspaceList'
import HubSwitcher from './hub/HubSwitcher'
import SidebarConnectionStatus from './hub/SidebarConnectionStatus'
import ConnectionOverlay from './hub/ConnectionOverlay'
import DialogHost from './DialogHost'
import TerminalCache from './terminal/TerminalCache'
import { setHubId } from '../lib/modal-bridge'
import { useHubStore } from '../store/hub-store'

// Lazy-loaded route components
const Home = React.lazy(() => import('./pages/Home'))
const HubDashboard = React.lazy(() => import('./pages/HubDashboard'))
const HubShow = React.lazy(() => import('./pages/HubShow'))
const SettingsRoute = React.lazy(() => import('./pages/SettingsRoute'))
const PairingRoute = React.lazy(() => import('./pages/PairingRoute'))

function SuspenseFallback() {
  return (
    <div className="h-full flex items-center justify-center">
      <div className="text-sm text-zinc-500">Loading...</div>
    </div>
  )
}

function CogIcon() {
  return (
    <svg data-slot="icon" fill="none" stroke="currentColor" viewBox="0 0 24 24">
      <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M9.594 3.94c.09-.542.56-.94 1.11-.94h2.593c.55 0 1.02.398 1.11.94l.213 1.281c.063.374.313.686.645.87.074.04.147.083.22.127.325.196.72.257 1.075.124l1.217-.456a1.125 1.125 0 011.37.49l1.296 2.247a1.125 1.125 0 01-.26 1.431l-1.003.827c-.293.241-.438.613-.431.992a6.759 6.759 0 010 .255c-.007.378.138.75.43.99l1.005.828c.424.35.534.954.26 1.43l-1.298 2.247a1.125 1.125 0 01-1.369.491l-1.217-.456c-.355-.133-.75-.072-1.076.124a6.57 6.57 0 01-.22.128c-.331.183-.581.495-.644.869l-.213 1.28c-.09.543-.56.941-1.11.941h-2.594c-.55 0-1.02-.398-1.11-.94l-.213-1.281c-.062-.374-.312-.686-.644-.87a6.52 6.52 0 01-.22-.127c-.325-.196-.72-.257-1.076-.124l-1.217.456a1.125 1.125 0 01-1.369-.49l-1.297-2.247a1.125 1.125 0 01.26-1.431l1.004-.827c.292-.24.437-.613.43-.991a6.932 6.932 0 010-.255c.007-.38-.138-.751-.43-.992l-1.004-.827a1.125 1.125 0 01-.26-1.43l1.297-2.247a1.125 1.125 0 011.37-.491l1.216.456c.356.133.751.072 1.076-.124.072-.044.146-.087.22-.128.332-.183.582-.495.644-.869l.214-1.281z" />
      <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M15 12a3 3 0 11-6 0 3 3 0 016 0z" />
    </svg>
  )
}

function BookIcon() {
  return (
    <svg data-slot="icon" fill="none" stroke="currentColor" viewBox="0 0 24 24">
      <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M12 6.042A8.967 8.967 0 006 3.75c-1.052 0-2.062.18-3 .512v14.25A8.987 8.987 0 016 18c2.305 0 4.408.867 6 2.292m0-14.25a8.966 8.966 0 016-2.292c1.052 0 2.062.18 3 .512v14.25A8.987 8.987 0 0018 18a8.967 8.967 0 00-6 2.292m0-14.25v14.25" />
    </svg>
  )
}

function LogoutIcon() {
  return (
    <svg data-slot="icon" fill="none" stroke="currentColor" viewBox="0 0 24 24">
      <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M15.75 9V5.25A2.25 2.25 0 0013.5 3h-6a2.25 2.25 0 00-2.25 2.25v13.5A2.25 2.25 0 007.5 21h6a2.25 2.25 0 002.25-2.25V15m3 0l3-3m0 0l-3-3m3 3H9" />
    </svg>
  )
}

function CommandLineIcon() {
  return (
    <svg data-slot="icon" fill="none" stroke="currentColor" viewBox="0 0 24 24">
      <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M8 9l3 3-3 3m5 0h3M5 20h14a2 2 0 002-2V6a2 2 0 00-2-2H5a2 2 0 00-2 2v12a2 2 0 002 2z" />
    </svg>
  )
}

/**
 * Syncs route :hubId param into the hub store.
 * Renders inside /hubs/:hubId routes.
 */
function HubRouteSync() {
  const { hubId } = useParams()
  const selectedHubId = useHubStore((s) => s.selectedHubId)
  const selectHub = useHubStore((s) => s.selectHub)

  useEffect(() => {
    if (hubId && String(hubId) !== String(selectedHubId)) {
      selectHub(hubId)
    }
  }, [hubId, selectedHubId, selectHub])

  return <Outlet />
}

/**
 * Hub-scoped layout with Catalyst sidebar, hub switcher, and connection overlay.
 * Wraps all /hubs/* routes.
 */
function HubShell() {
  const location = useLocation()
  const navigate = useNavigate()
  const selectedHubId = useHubStore((s) => s.selectedHubId)
  const connectionState = useHubStore((s) => s.connectionState)
  const hubListLoading = useHubStore((s) => s.hubListLoading)
  const fetchHubList = useHubStore((s) => s.fetchHubList)
  const selectHub = useHubStore((s) => s.selectHub)
  const disconnectHub = useHubStore((s) => s.disconnectHub)
  const getLastHubId = useHubStore((s) => s.getLastHubId)

  // Fetch hub list on mount
  useEffect(() => {
    fetchHubList()
  }, [fetchHubList])

  // Auto-select last-used hub when at /hubs (no hub in URL)
  useEffect(() => {
    if (hubListLoading) return

    const hubList = useHubStore.getState().hubList
    const isHubRoute = /^\/hubs\/[^/]/.test(location.pathname)

    // Only auto-select when at /hubs with no hub in URL
    if (isHubRoute) return

    const lastId = getLastHubId()
    const target = lastId && hubList.find((h) => String(h.id) === String(lastId))

    if (target) {
      selectHub(target.id)
      navigate(`/hubs/${target.id}`, { replace: true })
    } else if (hubList.length === 1) {
      selectHub(hubList[0].id)
      navigate(`/hubs/${hubList[0].id}`, { replace: true })
    }
  }, [hubListLoading, location.pathname, navigate, selectHub, getLastHubId])

  // Keep modal-bridge in sync
  useEffect(() => {
    if (selectedHubId) setHubId(selectedHubId)
  }, [selectedHubId])

  // Disconnect when navigating away from /hubs/* routes (preserves lastHubId)
  useEffect(() => {
    return () => disconnectHub()
  }, [disconnectHub])

  const isSessionRoute = /\/sessions\//.test(location.pathname)
  const isSettingsRoute = /\/settings/.test(location.pathname)
  const isPairingRoute = /\/pairing/.test(location.pathname)

  return (
    <>
      <SidebarLayout
        flush={isSessionRoute}
        navbar={
          <Navbar>
            <NavbarItem href={selectedHubId ? `/hubs/${selectedHubId}` : '/hubs'}>
              <CommandLineIcon />
              <span className="font-mono font-bold tracking-tight">botster</span>
            </NavbarItem>
            <NavbarSpacer />
          </Navbar>
        }
        sidebar={
          <Sidebar>
            <SidebarHeader>
              <HubSwitcher />
            </SidebarHeader>
            <SidebarBody>
              <SidebarConnectionStatus />
              <SidebarSection>
                <SidebarHeading>Workspaces</SidebarHeading>
                <WorkspaceList hubId={selectedHubId} surface="sidebar" />
              </SidebarSection>
              <SidebarSpacer />
            </SidebarBody>
            <SidebarFooter>
              <SidebarSection>
                {selectedHubId && (
                  <SidebarItem
                    href={`/hubs/${selectedHubId}/settings`}
                    current={isSettingsRoute}
                  >
                    <CogIcon />
                    <SidebarLabel>Hub Settings</SidebarLabel>
                  </SidebarItem>
                )}
                <SidebarItem href="/docs" target="_blank">
                  <BookIcon />
                  <SidebarLabel>Docs</SidebarLabel>
                </SidebarItem>
                <SidebarItem
                  onClick={async () => {
                    const csrf = document.querySelector('meta[name="csrf-token"]')?.content
                    await fetch('/logout', {
                      method: 'DELETE',
                      headers: { 'X-CSRF-Token': csrf },
                      credentials: 'same-origin',
                    })
                    window.location.href = '/'
                  }}
                >
                  <LogoutIcon />
                  <SidebarLabel>Sign out</SidebarLabel>
                </SidebarItem>
              </SidebarSection>
            </SidebarFooter>
          </Sidebar>
        }
      >
        {isSessionRoute ? (
          <TerminalCache hubId={selectedHubId} />
        ) : (
          <Outlet />
        )}
        <ConnectionOverlay suppress={isPairingRoute} />
      </SidebarLayout>
      {selectedHubId && <DialogHost hubId={selectedHubId} />}
    </>
  )
}

/**
 * Root application shell.
 */
export default function AppShell() {
  return (
    <BrowserRouter>
      <React.Suspense fallback={<SuspenseFallback />}>
        <Routes>
          <Route path="/" element={<Home />} />

          <Route element={<HubShell />}>
            <Route path="/hubs" element={<HubDashboard />} />

            <Route path="/hubs/:hubId" element={<HubRouteSync />}>
              <Route index element={<HubShow />} />
              <Route path="sessions/:sessionUuid" element={null} />
              <Route path="settings" element={<SettingsRoute />} />
              <Route path="pairing" element={<PairingRoute />} />
            </Route>
          </Route>
        </Routes>
      </React.Suspense>
    </BrowserRouter>
  )
}
