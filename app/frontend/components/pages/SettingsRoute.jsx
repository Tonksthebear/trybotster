import React from 'react'
import { useParams } from 'react-router-dom'
import SettingsApp from '../settings/SettingsApp'
import { useSettingsBootstrapQuery } from '../../lib/queries'

export default function SettingsRoute() {
  const { hubId } = useParams()
  const { data, isPending, isError } = useSettingsBootstrapQuery(hubId)

  if (isPending) {
    return (
      <div className="h-full flex items-center justify-center">
        <div className="text-sm text-zinc-500">Loading settings...</div>
      </div>
    )
  }

  const settingsData = isError ? {} : (data || {})

  return (
    <SettingsApp
      hubId={hubId}
      configMetadata={settingsData.configMetadata || {}}
      templates={settingsData.templates || {}}
      agentTemplates={settingsData.agentTemplates || []}
      hubName={settingsData.hubName || ''}
      hubIdentifier={settingsData.hubIdentifier || ''}
      hubSettingsPath={`/hubs/${hubId}/settings`}
      hubPath={`/hubs/${hubId}`}
    />
  )
}
