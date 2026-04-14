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

    store.connectHub(hubId).then(() => {
      useSettingsStore.getState().scanTree()
      useSettingsStore.getState().checkInstalled()
    })

    return () => {
      useSettingsStore.getState().disconnectHub()
    }
  }, [hubId])

  if (!hubId) return null

  return (
    <SettingsPage
      templates={templates}
      agentTemplates={agentTemplates}
      hubName={hubName}
      hubIdentifier={hubIdentifier}
      hubSettingsPath={hubSettingsPath}
      hubPath={hubPath}
    />
  )
}
