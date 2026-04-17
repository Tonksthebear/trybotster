import React, { useState, useEffect } from 'react'
import { getHub } from '../../lib/hub-bridge'

const STATUS_COLORS = {
  connected: 'text-emerald-500',
  connecting: 'text-amber-500',
  disconnected: 'text-zinc-500',
  error: 'text-red-500',
  online: 'text-emerald-500',
  offline: 'text-zinc-500',
}

const DOT_COLORS = {
  connected: 'bg-emerald-500',
  connecting: 'bg-amber-500 animate-pulse',
  disconnected: 'bg-zinc-500',
  error: 'bg-red-500',
  online: 'bg-emerald-500',
  offline: 'bg-zinc-500',
}

const CONNECTION_COLORS = {
  direct: 'text-emerald-500',
  relay: 'text-sky-500',
  connecting: 'text-amber-500',
  disconnected: 'text-zinc-500',
  expired: 'text-red-500',
  unpaired: 'text-amber-500',
}

const CONNECTION_LABELS = {
  direct: 'Direct',
  relay: 'Relay',
  connecting: 'Connecting',
  disconnected: 'Offline',
  expired: 'Expired',
  unpaired: 'Scan Code',
}

const BROWSER_LABELS = {
  connected: 'Online',
  connecting: 'Connecting',
  disconnected: 'Offline',
  error: 'Error',
}

export default function ConnectionStatus({ hubId }) {
  const [browser, setBrowser] = useState('connecting')
  const [connection, setConnection] = useState('disconnected')
  const [hub, setHub] = useState(null) // null = connecting, 'online', 'offline'

  useEffect(() => {
    if (!hubId) return

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
      const hubObj = getHub(hubId)
      if (!hubObj) {
        if (!cancelled) pollTimer = setTimeout(trySubscribe, 500)
        return
      }

      const current = hubObj.connectionStatus?.current()
      if (current) {
        setConnection(current.connection || 'disconnected')
        setHub(current.hub || null)
      }

      const unsub = hubObj.onConnectionStatusChange?.((status) => {
        if (!status) return
        setConnection(status.connection || 'disconnected')
        setHub(status.hub || null)
      })
      if (unsub) teardowns.push(unsub)
    }

    trySubscribe()

    return () => {
      cancelled = true
      clearTimeout(pollTimer)
      teardowns.forEach((fn) => fn())
    }
  }, [hubId])

  return (
    <div className="flex items-center gap-1 text-xs font-medium">
      {/* Browser */}
      <div className="flex items-center gap-1 px-2 py-1 rounded-l-md border-r border-zinc-700/50">
        <svg className={`size-3.5 ${STATUS_COLORS[browser] || STATUS_COLORS.disconnected}`} fill="none" stroke="currentColor" viewBox="0 0 24 24">
          <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M12 21a9.004 9.004 0 008.716-6.747M12 21a9.004 9.004 0 01-8.716-6.747M12 21c2.485 0 4.5-4.03 4.5-9S14.485 3 12 3m0 18c-2.485 0-4.5-4.03-4.5-9S9.515 3 12 3m0 0a8.997 8.997 0 017.843 4.582M12 3a8.997 8.997 0 00-7.843 4.582m15.686 0A11.953 11.953 0 0112 10.5c-2.998 0-5.74-1.1-7.843-2.918m15.686 0A8.959 8.959 0 0121 12c0 .778-.099 1.533-.284 2.253m0 0A17.919 17.919 0 0112 16.5c-3.162 0-6.133-.815-8.716-2.247m0 0A9.015 9.015 0 013 12c0-1.605.42-3.113 1.157-4.418" />
        </svg>
        <span className={`size-1.5 rounded-full ${DOT_COLORS[browser] || DOT_COLORS.disconnected}`} />
        <span className={`hidden sm:inline min-w-[4rem] ${STATUS_COLORS[browser] || 'text-zinc-400'}`}>
          {BROWSER_LABELS[browser] || 'Offline'}
        </span>
      </div>

      {/* Connection (WebRTC) */}
      <div className="flex items-center gap-1 px-2 py-1 border-r border-zinc-700/50">
        <span className={`${CONNECTION_COLORS[connection] || 'text-zinc-600'}`}>
          <svg className="size-3" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
            <path d="M5 12h14" />
          </svg>
        </span>
        <ConnectionIcon state={connection} />
        <span className={`${CONNECTION_COLORS[connection] || 'text-zinc-600'}`}>
          <svg className="size-3" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
            <path d="M5 12h14" />
          </svg>
        </span>
        <span className={`hidden sm:inline min-w-[4rem] ${CONNECTION_COLORS[connection] || 'text-zinc-400'}`}>
          {CONNECTION_LABELS[connection] || 'Offline'}
        </span>
      </div>

      {/* Hub */}
      <div className="flex items-center gap-1 px-2 py-1 rounded-r-md">
        <span className={`hidden sm:inline ${STATUS_COLORS[hub || 'connecting'] || 'text-zinc-500'}`}>
          Hub
        </span>
        <span className={`size-1.5 rounded-full ${hub ? DOT_COLORS[hub] || DOT_COLORS.offline : DOT_COLORS.connecting}`} />
        <svg className={`size-3.5 ${STATUS_COLORS[hub || 'connecting'] || 'text-zinc-500'}`} fill="none" stroke="currentColor" viewBox="0 0 24 24">
          <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M21.75 17.25v-.228a4.5 4.5 0 00-.12-1.03l-2.268-9.64a3.375 3.375 0 00-3.285-2.602H7.923a3.375 3.375 0 00-3.285 2.602l-2.268 9.64a4.5 4.5 0 00-.12 1.03v.228m19.5 0a3 3 0 01-3 3H5.25a3 3 0 01-3-3m19.5 0a3 3 0 00-3-3H5.25a3 3 0 00-3 3m16.5 0h.008v.008h-.008v-.008zm-3 0h.008v.008h-.008v-.008z" />
        </svg>
      </div>
    </div>
  )
}

function ConnectionIcon({ state }) {
  if (state === 'disconnected') {
    return (
      <svg className="size-4 text-zinc-500" fill="none" stroke="currentColor" viewBox="0 0 24 24">
        <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="m9.75 9.75 4.5 4.5m0-4.5-4.5 4.5M21 12a9 9 0 1 1-18 0 9 9 0 0 1 18 0Z" />
      </svg>
    )
  }
  if (state === 'connecting') {
    return (
      <svg className="size-4 text-amber-500 animate-spin" fill="none" stroke="currentColor" viewBox="0 0 24 24">
        <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M16.023 9.348h4.992v-.001M2.985 19.644v-4.992m0 0h4.992m-4.993 0 3.181 3.183a8.25 8.25 0 0 0 13.803-3.7M4.031 9.865a8.25 8.25 0 0 1 13.803-3.7l3.181 3.182" />
      </svg>
    )
  }
  if (state === 'direct') {
    return (
      <span className="relative flex items-center text-emerald-500">
        <svg className="size-4" viewBox="0 0 24 24" fill="currentColor">
          <path fillRule="evenodd" d="M14.615 1.595a.75.75 0 01.359.852L12.982 9.75h7.268a.75.75 0 01.548 1.262l-10.5 11.25a.75.75 0 01-1.272-.71l1.992-7.302H3.75a.75.75 0 01-.548-1.262l10.5-11.25a.75.75 0 01.913-.143z" clipRule="evenodd" />
        </svg>
        <span className="absolute -bottom-0.5 -right-0.5 bg-zinc-900 rounded-full p-px">
          <svg className="size-2 text-emerald-400" viewBox="0 0 16 16" fill="currentColor">
            <path fillRule="evenodd" d="M8 1a3.5 3.5 0 00-3.5 3.5V7A1.5 1.5 0 003 8.5v5A1.5 1.5 0 004.5 15h7a1.5 1.5 0 001.5-1.5v-5A1.5 1.5 0 0011.5 7V4.5A3.5 3.5 0 008 1z" clipRule="evenodd" />
          </svg>
        </span>
      </span>
    )
  }
  if (state === 'relay') {
    return (
      <span className="relative flex items-center text-sky-500">
        <svg className="size-4" viewBox="0 0 24 24" fill="currentColor">
          <path fillRule="evenodd" d="M4.5 9.75a6 6 0 0111.573-2.226 3.75 3.75 0 014.133 4.303A4.5 4.5 0 0118 20.25H6.75a5.25 5.25 0 01-2.23-10.004 6.072 6.072 0 01-.02-.496z" clipRule="evenodd" />
        </svg>
        <span className="absolute -bottom-0.5 -right-0.5 bg-zinc-900 rounded-full p-px">
          <svg className="size-2 text-emerald-400" viewBox="0 0 16 16" fill="currentColor">
            <path fillRule="evenodd" d="M8 1a3.5 3.5 0 00-3.5 3.5V7A1.5 1.5 0 003 8.5v5A1.5 1.5 0 004.5 15h7a1.5 1.5 0 001.5-1.5v-5A1.5 1.5 0 0011.5 7V4.5A3.5 3.5 0 008 1z" clipRule="evenodd" />
          </svg>
        </span>
      </span>
    )
  }
  if (state === 'expired') {
    return (
      <svg className="size-4 text-red-500" fill="none" stroke="currentColor" viewBox="0 0 24 24">
        <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M13.5 10.5V6.75a4.5 4.5 0 119 0v3.75M3.75 21.75h10.5a2.25 2.25 0 002.25-2.25v-6.75a2.25 2.25 0 00-2.25-2.25H3.75a2.25 2.25 0 00-2.25 2.25v6.75a2.25 2.25 0 002.25 2.25z" />
      </svg>
    )
  }
  if (state === 'unpaired') {
    return (
      <svg className="size-4 text-amber-500" fill="none" stroke="currentColor" viewBox="0 0 24 24">
        <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M3.75 4.875c0-.621.504-1.125 1.125-1.125h4.5c.621 0 1.125.504 1.125 1.125v4.5c0 .621-.504 1.125-1.125 1.125h-4.5A1.125 1.125 0 013.75 9.375v-4.5zM3.75 14.625c0-.621.504-1.125 1.125-1.125h4.5c.621 0 1.125.504 1.125 1.125v4.5c0 .621-.504 1.125-1.125 1.125h-4.5a1.125 1.125 0 01-1.125-1.125v-4.5zM13.5 4.875c0-.621.504-1.125 1.125-1.125h4.5c.621 0 1.125.504 1.125 1.125v4.5c0 .621-.504 1.125-1.125 1.125h-4.5A1.125 1.125 0 0113.5 9.375v-4.5z" />
      </svg>
    )
  }
  return null
}
