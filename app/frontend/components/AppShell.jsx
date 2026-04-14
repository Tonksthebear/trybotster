import React, { useEffect, useRef } from 'react'
import {
  BrowserRouter,
  Routes,
  Route,
  Outlet,
  useParams,
  useLocation,
} from 'react-router-dom'
import { connect, disconnect } from '../lib/hub-bridge'
import DialogHost from './DialogHost'
import TerminalCache from './terminal/TerminalCache'
import { setHubId } from '../lib/modal-bridge'

// Lazy-loaded route components
const Home = React.lazy(() => import('./pages/Home'))
const HubDashboard = React.lazy(() => import('./pages/HubDashboard'))
const HubShow = React.lazy(() => import('./pages/HubShow'))
const SettingsRoute = React.lazy(() => import('./pages/SettingsRoute'))
const PairingPage = React.lazy(() => import('./pairing/PairingPage'))

function SuspenseFallback() {
  return (
    <div className="h-full flex items-center justify-center">
      <div className="text-sm text-zinc-500">Loading...</div>
    </div>
  )
}

/**
 * Hub-scoped layout. Persists the hub bridge connection and terminal
 * sessions across route changes within a hub.
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

  // Detect if we're on a session route
  const isSessionRoute = /\/sessions\//.test(location.pathname)

  return (
    <div className="relative h-full">
      {/* Terminal cache — always mounted, visible only on session routes */}
      <TerminalCache hubId={hubId} />

      {/* Non-terminal content — hidden when viewing a terminal */}
      {!isSessionRoute && (
        <div className="h-full">
          <Outlet />
        </div>
      )}

      <DialogHost hubId={hubId} />
    </div>
  )
}

/**
 * Root application shell. React Router drives all navigation.
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
            <Route path="pairing" element={<PairingPage />} />
          </Route>
        </Routes>
      </React.Suspense>
    </BrowserRouter>
  )
}
