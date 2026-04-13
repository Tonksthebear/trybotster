import React, { useEffect } from 'react'
import { connect, disconnect, getHub, syncSelectionFromUrl } from '../lib/hub-bridge'
import { setHubId } from '../lib/modal-bridge'
import WorkspaceList from './workspace/WorkspaceList'

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

  // Turbo navigation: resync selection from URL when turbo-permanent
  // element persists across page transitions.
  useEffect(() => {
    if (!hubId) return

    function handleTurboLoad() {
      const hub = getHub(hubId)
      if (hub) syncSelectionFromUrl(hub)
    }

    document.addEventListener('turbo:load', handleTurboLoad)
    return () => document.removeEventListener('turbo:load', handleTurboLoad)
  }, [hubId])

  if (!hubId) return null

  return <WorkspaceList hubId={hubId} surface={surface} />
}
