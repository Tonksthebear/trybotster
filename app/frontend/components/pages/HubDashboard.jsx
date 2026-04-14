import React, { useState, useEffect } from 'react'
import { Link } from 'react-router-dom'

export default function HubDashboard() {
  const [hubs, setHubs] = useState([])
  const [loading, setLoading] = useState(true)

  useEffect(() => {
    fetch('/hubs.json', {
      headers: { Accept: 'application/json' },
      credentials: 'same-origin',
    })
      .then((res) => {
        if (res.status === 401 || res.redirected) {
          window.location.href = '/github/authorization/new'
          return null
        }
        if (!res.ok) throw new Error(`${res.status}`)
        return res.json()
      })
      .then((data) => {
        if (!data) return
        setHubs(Array.isArray(data) ? data : data.hubs || [])
        setLoading(false)
      })
      .catch(() => setLoading(false))
  }, [])

  return (
    <div className="min-h-full">
      <div className="max-w-3xl mx-auto px-4 py-8 lg:py-12">
        <div className="flex items-center justify-between mb-8">
          <h1 className="text-2xl font-bold text-zinc-100 font-mono">Hubs</h1>
          <a
            href="/users/hubs/new"
            className="inline-flex items-center gap-2 px-4 py-2 bg-primary-600 hover:bg-primary-500 text-white rounded-lg text-sm font-medium transition-colors"
          >
            <svg className="size-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
              <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M12 4v16m8-8H4" />
            </svg>
            Connect Hub
          </a>
        </div>

        {loading && (
          <div className="py-12 text-center text-zinc-500">Loading hubs...</div>
        )}

        {!loading && hubs.length === 0 && (
          <div className="py-12 text-center">
            <svg className="size-12 text-zinc-700 mx-auto mb-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
              <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={1.5} d="M21.75 17.25v-.228a4.5 4.5 0 00-.12-1.03l-2.268-9.64a3.375 3.375 0 00-3.285-2.602H7.923a3.375 3.375 0 00-3.285 2.602l-2.268 9.64a4.5 4.5 0 00-.12 1.03v.228m19.5 0a3 3 0 01-3 3H5.25a3 3 0 01-3-3m19.5 0a3 3 0 00-3-3H5.25a3 3 0 00-3 3" />
            </svg>
            <h3 className="text-lg font-medium text-zinc-300 mb-2">No hubs connected</h3>
            <p className="text-sm text-zinc-500 mb-4">Connect your first hub to get started</p>
          </div>
        )}

        {!loading && hubs.length > 0 && (
          <div className="space-y-3">
            {hubs.map((hub) => (
              <Link
                key={hub.id}
                to={`/hubs/${hub.id}`}
                className="block bg-zinc-900/50 border border-zinc-800 hover:border-zinc-700 rounded-lg p-4 transition-colors"
              >
                <div className="flex items-center justify-between">
                  <div className="min-w-0">
                    <div className="text-sm font-medium text-zinc-100 font-mono truncate">
                      {hub.name || hub.identifier}
                    </div>
                    <div className="text-xs text-zinc-500 mt-1 truncate">
                      {hub.identifier}
                    </div>
                  </div>
                  <div className="flex items-center gap-2">
                    <span className={`size-2 rounded-full ${hub.active ? 'bg-emerald-500' : 'bg-zinc-600'}`} />
                    <span className="text-xs text-zinc-400">
                      {hub.active ? 'Online' : 'Offline'}
                    </span>
                  </div>
                </div>
              </Link>
            ))}
          </div>
        )}
      </div>
    </div>
  )
}
