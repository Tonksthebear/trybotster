import React, { useState, useEffect } from 'react'
import { useLocation, useParams } from 'react-router-dom'
import TerminalView from './TerminalView'

/**
 * Keeps terminal sessions alive across route changes within a hub.
 * Renders inside HubLayout — always present but hidden when not on a session route.
 * Active terminals are kept mounted with display:none when not visible.
 */
export default function TerminalCache({ hubId }) {
  const location = useLocation()
  const [sessions, setSessions] = useState(new Set())

  // Parse the current session UUID from the URL
  const match = location.pathname.match(/\/hubs\/[^/]+\/sessions\/([^/]+)/)
  const activeSessionUuid = match ? match[1] : null

  // Track opened sessions
  useEffect(() => {
    if (activeSessionUuid) {
      setSessions((prev) => {
        if (prev.has(activeSessionUuid)) return prev
        const next = new Set(prev)
        next.add(activeSessionUuid)
        return next
      })
    }
  }, [activeSessionUuid])

  if (sessions.size === 0) return null

  return (
    <>
      {[...sessions].map((uuid) => (
        <div
          key={uuid}
          className="absolute inset-0 z-10"
          style={{ display: uuid === activeSessionUuid ? 'block' : 'none' }}
        >
          <TerminalView hubId={hubId} sessionUuid={uuid} />
        </div>
      ))}
    </>
  )
}
