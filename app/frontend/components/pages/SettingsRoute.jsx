import React, { useState, useEffect } from 'react'
import { useParams } from 'react-router-dom'
import SettingsApp from '../settings/SettingsApp'

export default function SettingsRoute() {
  const { hubId } = useParams()
  const [data, setData] = useState(null)

  useEffect(() => {
    if (!hubId) return

    fetch(`/hubs/${hubId}/settings.json`, {
      headers: { Accept: 'application/json' },
      credentials: 'same-origin',
    })
      .then((res) => {
        if (!res.ok) throw new Error(`${res.status}`)
        return res.json()
      })
      .then(setData)
      .catch((err) => {
        console.warn('[SettingsRoute] Failed to fetch settings data:', err)
        setData({})
      })
  }, [hubId])

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
