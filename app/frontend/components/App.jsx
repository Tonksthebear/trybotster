import React, { useEffect } from 'react'
import { connect, disconnect, getHub, syncSelectionFromUrl } from '../lib/hub-bridge'
import { setHubId } from '../lib/modal-bridge'
import UiTree from './UiTree'
import SessionActionsMenu from './workspace/SessionActionsMenu'

export default function App({ hubId, surface = 'panel' }) {
  // Hub bridge lifecycle
  useEffect(() => {
    if (!hubId) return

    // Tell the singleton modal bridge which hub to use.
    // All App instances on the same page share the same hubId.
    setHubId(hubId)

    const state = { unmounted: false, connectionId: null }

    connect(hubId, { surface }).then(({ connectionId }) => {
      if (state.unmounted) {
        disconnect(connectionId)
      } else {
        state.connectionId = connectionId
      }
    })

    return () => {
      state.unmounted = true
      if (state.connectionId != null) {
        disconnect(state.connectionId)
      }
    }
  }, [hubId, surface])

  // Resync selection from URL on popstate (SPA back/forward navigation)
  useEffect(() => {
    if (!hubId) return

    function handlePopState() {
      const hub = getHub(hubId)
      if (hub) syncSelectionFromUrl(hub)
    }

    window.addEventListener('popstate', handlePopState)
    return () => window.removeEventListener('popstate', handlePopState)
  }, [hubId])

  if (!hubId) return null

  const targetSurface = surface === 'sidebar' ? 'workspace_sidebar' : 'workspace_panel'

  return (
    <UiTree hubId={hubId} targetSurface={targetSurface}>
      <SessionActionsMenu />
    </UiTree>
  )
}
