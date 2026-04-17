import React from 'react'
import { useLocation } from 'react-router-dom'
import TerminalView from './TerminalView'

// Mounts exactly the TerminalView for the current route's session uuid.
// Switching sessions unmounts the previous view, which tears down its
// Restty instance and closes its multiplexed stream subscription —
// trading fast switch-back for much lower memory footprint.
export default function TerminalCache({ hubId }) {
  const location = useLocation()
  const match = location.pathname.match(/\/hubs\/[^/]+\/sessions\/([^/]+)/)
  const sessionUuid = match ? match[1] : null

  if (!sessionUuid) return null

  return (
    <div className="absolute inset-0 z-10">
      <TerminalView key={sessionUuid} hubId={hubId} sessionUuid={sessionUuid} />
    </div>
  )
}
