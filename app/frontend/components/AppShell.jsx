import React, { useEffect, useRef } from 'react'
import {
  BrowserRouter,
  Routes,
  Route,
  Outlet,
  useParams,
  useLocation,
  useNavigate,
} from 'react-router-dom'
import { connect, disconnect } from '../lib/hub-bridge'
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
import ConnectionStatus from './hub/ConnectionStatus'
import DialogHost from './DialogHost'
import TerminalCache from './terminal/TerminalCache'
import { setHubId } from '../lib/modal-bridge'

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

function CommandLineIcon() {
  return (
    <svg data-slot="icon" fill="none" stroke="currentColor" viewBox="0 0 24 24">
      <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M8 9l3 3-3 3m5 0h3M5 20h14a2 2 0 002-2V6a2 2 0 00-2-2H5a2 2 0 00-2 2v12a2 2 0 002 2z" />
    </svg>
  )
}

/**
 * Hub-scoped layout with Catalyst sidebar.
 */
function HubLayout() {
  const { hubId } = useParams()
  const location = useLocation()
  const connectionRef = useRef(null)

  useEffect(() => {
    if (!hubId) return

    setHubId(hubId)
    const state = { unmounted: false }

    connect(hubId, { surface: 'panel' }).then(({ connectionId }) => {
      if (state.unmounted) {
        disconnect(connectionId)
      } else {
        connectionRef.current = connectionId
      }
    })

    return () => {
      state.unmounted = true
      if (connectionRef.current != null) {
        disconnect(connectionRef.current)
        connectionRef.current = null
      }
    }
  }, [hubId])

  const isSessionRoute = /\/sessions\//.test(location.pathname)
  const isSettingsRoute = /\/settings/.test(location.pathname)

  return (
    <>
      <SidebarLayout
        flush={isSessionRoute}
        navbar={
          <Navbar>
            <NavbarItem href={`/hubs/${hubId}`}>
              <CommandLineIcon />
              <span className="font-mono font-bold tracking-tight">botster</span>
            </NavbarItem>
            <NavbarSpacer />
            <ConnectionStatus hubId={hubId} />
          </Navbar>
        }
        sidebar={
          <Sidebar>
            <SidebarHeader>
              <SidebarItem href={`/hubs/${hubId}`}>
                <CommandLineIcon />
                <SidebarLabel className="font-mono font-bold tracking-tight">
                  botster
                </SidebarLabel>
              </SidebarItem>
            </SidebarHeader>
            <SidebarBody>
              <SidebarSection>
                <SidebarHeading>Workspaces</SidebarHeading>
                <WorkspaceList hubId={hubId} surface="sidebar" />
              </SidebarSection>
              <SidebarSpacer />
            </SidebarBody>
            <SidebarFooter>
              <SidebarSection>
                <SidebarItem
                  href={`/hubs/${hubId}/settings`}
                  current={isSettingsRoute}
                >
                  <CogIcon />
                  <SidebarLabel>Hub Settings</SidebarLabel>
                </SidebarItem>
                <SidebarItem href="/docs" target="_blank">
                  <BookIcon />
                  <SidebarLabel>Docs</SidebarLabel>
                </SidebarItem>
              </SidebarSection>
            </SidebarFooter>
          </Sidebar>
        }
      >
        {isSessionRoute ? (
          <TerminalCache hubId={hubId} />
        ) : (
          <Outlet />
        )}
      </SidebarLayout>
      <DialogHost hubId={hubId} />
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
          <Route path="/hubs" element={<HubDashboard />} />

          <Route path="/hubs/:hubId" element={<HubLayout />}>
            <Route index element={<HubShow />} />
            <Route path="sessions/:sessionUuid" element={null} />
            <Route path="settings" element={<SettingsRoute />} />
            <Route path="pairing" element={<PairingRoute />} />
          </Route>
        </Routes>
      </React.Suspense>
    </BrowserRouter>
  )
}
