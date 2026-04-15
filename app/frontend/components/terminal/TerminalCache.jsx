import React, { useState, useEffect, useCallback } from 'react'
import { useLocation } from 'react-router-dom'
import TerminalView from './TerminalView'

const MAX_CACHED = 5

/**
 * Keeps terminal sessions alive across route changes within a hub.
 * LRU eviction: when more than MAX_CACHED terminals have been visited,
 * the least-recently-used one is removed (unmounted, transport torn down).
 */
export default function TerminalCache({ hubId }) {
  const location = useLocation()
  // Ordered array: most-recently-used last
  const [order, setOrder] = useState([])

  const match = location.pathname.match(/\/hubs\/[^/]+\/sessions\/([^/]+)/)
  const activeSessionUuid = match ? match[1] : null

  useEffect(() => {
    if (!activeSessionUuid) return

    setOrder((prev) => {
      // Move to end (most recent)
      const without = prev.filter((id) => id !== activeSessionUuid)
      const next = [...without, activeSessionUuid]
      // Evict LRU if over limit
      if (next.length > MAX_CACHED) {
        return next.slice(next.length - MAX_CACHED)
      }
      return next
    })
  }, [activeSessionUuid])

  if (order.length === 0) return null

  return (
    <>
      {order.map((uuid) => (
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
