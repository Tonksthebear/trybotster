import React, { useEffect } from 'react'
import { connect, disconnect } from '../../lib/hub-bridge'
import { useSettingsStore } from '../../store/settings-store'
import SettingsPage from './SettingsPage'

/**
 * Entry component for the React settings page.
 * Manages hub lifecycle (acquire/release) and passes server-rendered
 * data down as props. Mirrors the pattern from App.jsx.
 */
export default function SettingsApp({
  hubId,
  configMetadata,
  templates,
  agentTemplates,
  hubName,
  hubIdentifier,
  hubSettingsPath,
  hubPath,
}) {
  useEffect(() => {
    if (!hubId) return

    const store = useSettingsStore.getState()
    store.setConfigMetadata(configMetadata)

    // Hub bridge (for ConnectionStatus)
    const state = { unmounted: false, bridgeConnectionId: null }
    connect(hubId, { surface: 'settings' }).then(({ connectionId }) => {
      if (state.unmounted) {
        disconnect(connectionId)
      } else {
        state.bridgeConnectionId = connectionId
      }
    })

    store.connectHub(hubId).then(() => {
      useSettingsStore.getState().scanTree()
      useSettingsStore.getState().checkInstalled()
    })

    return () => {
      state.unmounted = true
      useSettingsStore.getState().disconnectHub()
      if (state.bridgeConnectionId != null) disconnect(state.bridgeConnectionId)
    }
  }, [hubId])

  if (!hubId) return null

  return (
    <SettingsPage
      hubId={hubId}
      templates={templates}
      agentTemplates={agentTemplates}
      hubName={hubName}
      hubIdentifier={hubIdentifier}
      hubSettingsPath={hubSettingsPath}
      hubPath={hubPath}
    />
  )
}
