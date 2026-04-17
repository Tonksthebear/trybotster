import React from 'react'
import clsx from 'clsx'
import {
  useWorkspaceStore,
  displayName,
  titleLine as getTitleLine,
  subtext as getSubtext,
  activityState as getActivityState,
  previewState,
} from '../../store/workspace-store'
import { dispatch, ACTION } from '../../lib/actions'
import SessionActionsMenu from './SessionActionsMenu'
import HostedPreviewIndicator from './HostedPreviewIndicator'
import HostedPreviewError from './HostedPreviewError'

export default function SessionRow({ sessionId, hubId, surface }) {
  const session = useWorkspaceStore((s) => s.sessionsById[sessionId])
  const selected = useWorkspaceStore((s) => s.selectedSessionId === sessionId)

  if (!session) return null

  const sessionUuid = session.session_uuid
  const primaryName = displayName(session)
  const titleLine = getTitleLine(session)
  const subtext = getSubtext(session)
  const notification = !!session.notification
  const sessionType = session.session_type || 'agent'
  const activityState = getActivityState(session)
  const preview = previewState(session)
  const hostedPreview = preview.canPreview ? preview : null
  const previewError = preview.status === 'error' ? preview.error : null
  const actionsMenu = {
    canPreview: preview.canPreview,
    previewStatus: preview.status,
    previewUrl: preview.url,
    canMove: true,
    canDelete: true,
  }

  const density = surface === 'sidebar' ? 'sidebar' : 'panel'
  const isSidebar = density === 'sidebar'
  const isAccessory = sessionType === 'accessory'
  const sessionUrl = `/hubs/${hubId}/sessions/${sessionUuid}`

  function handleSelect(e) {
    e.preventDefault()
    dispatch({
      action: ACTION.SESSION_SELECT,
      payload: { sessionId, hubId, url: sessionUrl },
    })
  }

  if (isSidebar) {
    return (
      <div
        className={clsx(
          'group relative rounded transition-colors border-l-2 border-l-transparent',
          selected ? 'bg-primary-500/20' : 'hover:bg-zinc-800/50',
          notification && '!border-l-yellow-400'
        )}
      >
        <div className="flex items-center gap-1">
          <a
            href={sessionUrl}
            onClick={handleSelect}
            className={clsx(
              'flex-1 text-left px-2 py-1.5 min-w-0',
              selected
                ? 'text-primary-300 font-medium'
                : 'text-zinc-300 hover:text-zinc-100'
            )}
          >
            <div className="flex items-center gap-1.5 min-w-0">
              <ActivityIndicator state={activityState} density="sidebar" />
              <span
                className={clsx(
                  'truncate font-mono text-xs block',
                  isAccessory && 'text-zinc-400'
                )}
              >
                {primaryName}
              </span>
            </div>
            {titleLine && (
              <span className="truncate text-[10px] text-zinc-500 italic block">
                {titleLine}
              </span>
            )}
            <span className="truncate text-[10px] text-zinc-500 block">
              {subtext}
            </span>
          </a>

          {hostedPreview && (
            <HostedPreviewIndicator {...hostedPreview} density="sidebar" />
          )}

          <SessionActionsMenu
            sessionId={sessionId}
            sessionUuid={sessionUuid}
            hubId={hubId}
            actionsMenu={actionsMenu}
            isAccessory={isAccessory}
            density="sidebar"
          />
        </div>

        {previewError && (
          <HostedPreviewError
            error={previewError}
            installUrl={hostedPreview?.installUrl}
            density="sidebar"
          />
        )}
      </div>
    )
  }

  // Panel density
  return (
    <div
      className={clsx(
        'group relative bg-zinc-800/50 hover:bg-zinc-800 border border-zinc-700/50 hover:border-zinc-700 rounded-lg transition-colors border-l-2 border-l-transparent',
        selected && '!bg-primary-500/20 !border-primary-500/30',
        notification && '!border-l-yellow-400'
      )}
    >
      <div className="flex items-center gap-0">
        <a
          href={sessionUrl}
          onClick={handleSelect}
          className="flex items-center gap-3 px-4 py-3 flex-1 min-w-0"
        >
          <div className="size-10 rounded-lg bg-zinc-700/50 flex items-center justify-center text-zinc-400 shrink-0">
            <CommandLineIcon />
          </div>
          <div className="flex-1 min-w-0">
            <div className="flex items-center gap-2 min-w-0">
              <ActivityIndicator state={activityState} density="panel" />
              <div
                className={clsx(
                  'text-sm font-medium truncate font-mono',
                  isAccessory ? 'text-zinc-400' : 'text-zinc-100'
                )}
              >
                {primaryName}
              </div>
            </div>
            {titleLine && (
              <div className="text-xs text-zinc-500 italic truncate">
                {titleLine}
              </div>
            )}
            <div className="text-xs text-zinc-400 truncate">{subtext}</div>
          </div>
          <ChevronRightIcon />
        </a>

        {hostedPreview && (
          <HostedPreviewIndicator {...hostedPreview} density="panel" />
        )}

        <SessionActionsMenu
          sessionId={sessionId}
          sessionUuid={sessionUuid}
          hubId={hubId}
          actionsMenu={actionsMenu}
          isAccessory={isAccessory}
          density="panel"
        />
      </div>

      {previewError && (
        <HostedPreviewError
          error={previewError}
          installUrl={hostedPreview?.installUrl}
          density="panel"
        />
      )}
    </div>
  )
}

function ActivityIndicator({ state, density }) {
  if (state === 'accessory') return null

  const isIdle = state === 'idle'
  const sizeClass = density === 'sidebar' ? 'text-xs' : 'text-sm'

  return (
    <span
      className={clsx(
        'shrink-0 leading-none',
        sizeClass,
        isIdle ? 'text-sky-500' : 'text-emerald-300'
      )}
      title={isIdle ? 'Idle' : 'Active'}
      aria-label={isIdle ? 'Idle' : 'Active'}
    >
      {isIdle ? '\u25CC' : '\u273A'}
    </span>
  )
}

function CommandLineIcon() {
  return (
    <svg
      className="size-5"
      fill="none"
      stroke="currentColor"
      viewBox="0 0 24 24"
    >
      <path
        strokeLinecap="round"
        strokeLinejoin="round"
        strokeWidth={2}
        d="M8 9l3 3-3 3m5 0h3M5 20h14a2 2 0 002-2V6a2 2 0 00-2-2H5a2 2 0 00-2 2v12a2 2 0 002 2z"
      />
    </svg>
  )
}

function ChevronRightIcon() {
  return (
    <svg
      className="size-5 text-zinc-600 shrink-0"
      viewBox="0 0 20 20"
      fill="currentColor"
    >
      <path
        fillRule="evenodd"
        d="M8.22 5.22a.75.75 0 011.06 0l4.25 4.25a.75.75 0 010 1.06l-4.25 4.25a.75.75 0 01-1.06-1.06L11.94 10 8.22 6.28a.75.75 0 010-1.06z"
        clipRule="evenodd"
      />
    </svg>
  )
}
