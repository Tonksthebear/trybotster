import React, { useState, useEffect } from 'react'
import { getHub } from '../../lib/hub-bridge'
import { useHubStore } from '../../store/hub-store'
import { useHubMetaStore } from '../../store/entities/hub-meta-store'
import { resolveHubStatus } from '../../lib/connections/hub_connection_status'

const DOT_BG = {
  connected: 'bg-emerald-500',
  connecting: 'bg-amber-500 animate-pulse',
  disconnected: 'bg-zinc-500',
  error: 'bg-red-500',
  online: 'bg-emerald-500',
  offline: 'bg-zinc-500',
}

const CONNECTION_DOT = {
  direct: 'bg-emerald-500',
  relay: 'bg-sky-500',
  connecting: 'bg-amber-500 animate-pulse',
  disconnected: 'bg-zinc-500',
  expired: 'bg-red-500',
  unpaired: 'bg-amber-500',
}

const CONNECTION_LABEL = {
  direct: 'Direct',
  relay: 'Relay',
  connecting: 'Connecting',
  disconnected: 'Offline',
  expired: 'Expired',
  unpaired: 'Unpaired',
}

const BROWSER_LABEL = {
  connected: 'Online',
  connecting: 'Connecting',
  disconnected: 'Offline',
  error: 'Error',
}

const HUB_LABEL = {
  online: 'Online',
  offline: 'Offline',
}

export default function SidebarConnectionStatus() {
  const selectedHubId = useHubStore((s) => s.selectedHubId)
  const hubMetaById = useHubMetaStore((s) => s.byId)
  const [browser, setBrowser] = useState('connecting')
  const [connection, setConnection] = useState('disconnected')
  const [transportHubStatus, setTransportHubStatus] = useState(null)
  const [expanded, setExpanded] = useState(false)

  // The hub entity is keyed by Rust's server_hub_id() — the botster_id
  // returned by `register_hub_with_server` (cli/src/hub/registration.rs:84),
  // which is `String(Rails.Hub.id)`. selectedHubId from useHubStore is the
  // same Rails Hub.id (coerced to string at hub-store.js:44), so the URL we
  // route by IS the entity store key — no mapping through hub.identifier
  // (the local hash) is needed.
  const entityKey = selectedHubId == null ? null : String(selectedHubId)
  const hubEntity = entityKey ? hubMetaById[entityKey] : null
  const entityReady = hubEntity?.state === 'ready'

  const hubStatus = resolveHubStatus(transportHubStatus, entityReady)

  useEffect(() => {
    if (!selectedHubId) return

    const teardowns = []

    // Browser socket state
    import('transport/hub_signaling_client').then(({ observeBrowserSocketState }) => {
      observeBrowserSocketState((state) => {
        setBrowser(state === 'connected' ? 'connected' : state)
      }).then((unsub) => {
        teardowns.push(unsub)
      })
    })

    // Hub connection status — poll until hub is available
    let cancelled = false
    let pollTimer = null

    function trySubscribe() {
      const hubObj = getHub(selectedHubId)
      if (!hubObj) {
        if (!cancelled) pollTimer = setTimeout(trySubscribe, 500)
        return
      }

      const current = hubObj.connectionStatus?.current()
      if (current) {
        setConnection(current.connection || 'disconnected')
        setTransportHubStatus(current.hub || null)
      }

      const unsub = hubObj.onConnectionStatusChange?.((status) => {
        if (!status) return
        setConnection(status.connection || 'disconnected')
        setTransportHubStatus(status.hub || null)
      })
      if (unsub) teardowns.push(unsub)
    }

    trySubscribe()

    return () => {
      cancelled = true
      clearTimeout(pollTimer)
      teardowns.forEach((fn) => fn())
    }
  }, [selectedHubId])

  if (!selectedHubId) return null

  return (
    <div className="px-2 mb-2">
      <button
        type="button"
        onClick={() => setExpanded((v) => !v)}
        data-testid="sidebar-connection-status"
        data-browser-status={browser}
        data-connection-state={connection}
        data-hub-status={hubStatus || 'connecting'}
        className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-xs text-zinc-500 hover:text-zinc-300 hover:bg-zinc-800/50 transition-colors"
      >
        <span className="flex items-center gap-1.5">
          <span
            className={`size-1.5 rounded-full ${DOT_BG[browser] || DOT_BG.disconnected}`}
            title={`Browser: ${BROWSER_LABEL[browser] || 'Offline'}`}
          />
          <span
            className={`size-1.5 rounded-full ${CONNECTION_DOT[connection] || CONNECTION_DOT.disconnected}`}
            title={`Connection: ${CONNECTION_LABEL[connection] || 'Offline'}`}
          />
          <span
            className={`size-1.5 rounded-full ${hubStatus ? DOT_BG[hubStatus] || DOT_BG.offline : DOT_BG.connecting}`}
            title={`Hub: ${hubStatus ? HUB_LABEL[hubStatus] || 'Unknown' : 'Connecting'}`}
          />
        </span>
        <span className="truncate">
          {summaryLabel(browser, connection, hubStatus)}
        </span>
        <svg
          className={`ml-auto size-3 text-zinc-600 transition-transform ${expanded ? 'rotate-180' : ''}`}
          fill="none"
          stroke="currentColor"
          viewBox="0 0 24 24"
        >
          <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M19.5 8.25l-7.5 7.5-7.5-7.5" />
        </svg>
      </button>

      {expanded && (
        <div className="mt-1 space-y-1 px-2 pb-1">
          <StatusRow
            label="Browser"
            dot={DOT_BG[browser] || DOT_BG.disconnected}
            value={BROWSER_LABEL[browser] || 'Offline'}
          />
          <StatusRow
            label="Connection"
            dot={CONNECTION_DOT[connection] || CONNECTION_DOT.disconnected}
            value={CONNECTION_LABEL[connection] || 'Offline'}
          />
          <StatusRow
            label="Hub"
            dot={hubStatus ? DOT_BG[hubStatus] || DOT_BG.offline : DOT_BG.connecting}
            value={hubStatus ? HUB_LABEL[hubStatus] || 'Unknown' : 'Connecting'}
          />
        </div>
      )}
    </div>
  )
}

function StatusRow({ label, dot, value }) {
  return (
    <div className="flex items-center justify-between text-xs">
      <span className="text-zinc-500">{label}</span>
      <span className="flex items-center gap-1.5">
        <span className={`size-1.5 rounded-full ${dot}`} />
        <span className="text-zinc-400">{value}</span>
      </span>
    </div>
  )
}

function summaryLabel(browser, connection, hubStatus) {
  if (connection === 'direct') return 'Direct'
  if (connection === 'relay') return 'Relay'
  if (connection === 'unpaired') return 'Pairing needed'
  if (connection === 'expired') return 'Session expired'
  if (connection === 'connecting') return 'Connecting...'
  if (browser === 'connecting') return 'Connecting...'
  if (hubStatus === 'offline') return 'Hub offline'
  return 'Disconnected'
}
