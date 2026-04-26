import React, { useEffect } from 'react'
import { useParams } from 'react-router-dom'
import SettingsApp from '../settings/SettingsApp'
import { useSettingsBootstrapStore } from '../../store/settings-bootstrap-store'

export default function SettingsRoute() {
  const { hubId } = useParams()
  const data = useSettingsBootstrapStore((s) =>
    s.hubId === String(hubId) ? s.data : null
  )
  const load = useSettingsBootstrapStore((s) => s.load)

  useEffect(() => {
    if (!hubId) return
    load(hubId)
  }, [hubId, load])

  if (!data) {
    return (
      <div className="h-full flex items-center justify-center">
        <div className="text-sm text-zinc-500">Loading settings...</div>
      </div>
    )
  }

  return (
    <SettingsApp
      hubId={hubId}
      configMetadata={data.configMetadata || {}}
      templates={data.templates || {}}
      agentTemplates={data.agentTemplates || []}
      hubName={data.hubName || ''}
      hubIdentifier={data.hubIdentifier || ''}
      hubSettingsPath={`/hubs/${hubId}/settings`}
      hubPath={`/hubs/${hubId}`}
    />
  )
}
