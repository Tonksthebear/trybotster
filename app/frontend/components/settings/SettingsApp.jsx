import React, { useEffect } from 'react'
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

    const state = { unmounted: false }

    store.connectHub(hubId).then((hub) => {
      if (state.unmounted || !hub) return
      useSettingsStore.getState().scanTree()
      useSettingsStore.getState().checkInstalled()
    })

    return () => {
      state.unmounted = true
      useSettingsStore.getState().disconnectHub()
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
