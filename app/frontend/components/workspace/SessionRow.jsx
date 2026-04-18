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
import { UiTree, createHubDispatch } from '../../ui_contract'
import { sessionRowContent } from '../../ui_contract/composites'
import { IconGlyph } from '../../ui_contract/icons'
import SessionActionsMenu from './SessionActionsMenu'
import HostedPreviewIndicator from './HostedPreviewIndicator'
import HostedPreviewError from './HostedPreviewError'

// SessionRow keeps its outer container + `<a href>` as JSX:
// - The anchor preserves right-click/middle-click open-in-new-tab.
// - The outer container owns density-specific hover/selected/notification
//   styling that is Rails-surface-owned (not part of the v1 primitive spec).
// Inner content (activity dot + title + title line + subtext) is built from
// v1 primitives via `sessionRowContent` and rendered through `UiTree`.
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

  const contentNode = sessionRowContent({
    primaryName,
    titleLine,
    subtext,
    selected,
    sessionType,
    activityState,
    density,
  })
  const contentTree = (
    <UiTree node={contentNode} dispatch={createHubDispatch(hubId ?? '')} />
  )

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
            {contentTree}
          </a>

          {hostedPreview && (
            <HostedPreviewIndicator
              sessionId={sessionId}
              sessionUuid={sessionUuid}
              hubId={hubId}
              {...hostedPreview}
              density="sidebar"
            />
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
            sessionUuid={sessionUuid}
            hubId={hubId}
            error={previewError}
            installUrl={hostedPreview?.installUrl}
            density="sidebar"
          />
        )}
      </div>
    )
  }

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
          <div className="flex-1 min-w-0">{contentTree}</div>
          <ChevronRightIcon />
        </a>

        {hostedPreview && (
          <HostedPreviewIndicator
            sessionId={sessionId}
            sessionUuid={sessionUuid}
            hubId={hubId}
            {...hostedPreview}
            density="panel"
          />
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
          sessionUuid={sessionUuid}
          hubId={hubId}
          error={previewError}
          installUrl={hostedPreview?.installUrl}
          density="panel"
        />
      )}
    </div>
  )
}

function CommandLineIcon() {
  return (
    <span className="inline-flex size-5 items-center justify-center">
      <IconGlyph name="command-line" className="h-full w-full" />
    </span>
  )
}

function ChevronRightIcon() {
  return (
    <span className="inline-flex size-5 shrink-0 items-center justify-center text-zinc-600">
      <IconGlyph name="chevron-right" className="h-full w-full" />
    </span>
  )
}
