import React from 'react'
import { useParams, Link } from 'react-router-dom'
import UiTree from '../UiTree'
import SessionActionsMenu from '../workspace/SessionActionsMenu'
import ShareHub from '../hub/ShareHub'

export default function HubShow() {
  const { hubId } = useParams()

  return (
    <div className="h-full flex flex-col overflow-hidden">
      {/* Header */}
      <div className="shrink-0 border-b border-zinc-800 bg-zinc-900/50">
        <div className="px-4 py-4 lg:py-6">
          <div className="flex items-start justify-between gap-4">
            <div className="min-w-0">
              <h1 className="text-xl lg:text-2xl font-bold text-zinc-100 font-mono truncate">
                Hub
              </h1>
            </div>
            <div className="flex items-center gap-2">
              <Link
                to={`/hubs/${hubId}/settings`}
                className="inline-flex items-center gap-1.5 px-3 py-1.5 text-sm font-medium text-zinc-400 hover:text-zinc-200 bg-zinc-800/50 hover:bg-zinc-800 border border-zinc-700/50 rounded-lg transition-colors"
              >
                <svg className="size-3.5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                  <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M9.594 3.94c.09-.542.56-.94 1.11-.94h2.593c.55 0 1.02.398 1.11.94l.213 1.281c.063.374.313.686.645.87.074.04.147.083.22.127.325.196.72.257 1.075.124l1.217-.456a1.125 1.125 0 011.37.49l1.296 2.247a1.125 1.125 0 01-.26 1.431l-1.003.827c-.293.241-.438.613-.431.992a6.759 6.759 0 010 .255c-.007.378.138.75.43.99l1.005.828c.424.35.534.954.26 1.43l-1.298 2.247a1.125 1.125 0 01-1.369.491l-1.217-.456c-.355-.133-.75-.072-1.076.124a6.57 6.57 0 01-.22.128c-.331.183-.581.495-.644.869l-.213 1.28c-.09.543-.56.941-1.11.941h-2.594c-.55 0-1.02-.398-1.11-.94l-.213-1.281c-.062-.374-.312-.686-.644-.87a6.52 6.52 0 01-.22-.127c-.325-.196-.72-.257-1.076-.124l-1.217.456a1.125 1.125 0 01-1.369-.49l-1.297-2.247a1.125 1.125 0 01.26-1.431l1.004-.827c.292-.24.437-.613.43-.991a6.932 6.932 0 010-.255c.007-.38-.138-.751-.43-.992l-1.004-.827a1.125 1.125 0 01-.26-1.43l1.297-2.247a1.125 1.125 0 011.37-.491l1.216.456c.356.133.751.072 1.076-.124.072-.044.146-.087.22-.128.332-.183.582-.495.644-.869l.214-1.281z" />
                  <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M15 12a3 3 0 11-6 0 3 3 0 016 0z" />
                </svg>
                <span>Settings</span>
              </Link>
              <ShareHub hubId={hubId} />
            </div>
          </div>
        </div>
      </div>

      {/* Content */}
      <div className="flex-1 overflow-y-auto">
        <div className="max-w-3xl mx-auto px-4 py-6 lg:py-8">
          {/* Hub Info Card */}
          <div className="bg-zinc-900/50 border border-zinc-800 rounded-lg p-4 lg:p-6 mb-6">
            <h2 className="text-sm font-medium text-zinc-400 uppercase tracking-wider">
              Hub Information
            </h2>
          </div>

          {/* Workspaces */}
          <div className="bg-zinc-900/50 border border-zinc-800 rounded-lg p-4 lg:p-6">
            <UiTree hubId={hubId} targetSurface="workspace_panel">
              <SessionActionsMenu />
            </UiTree>
          </div>
        </div>
      </div>
    </div>
  )
}
