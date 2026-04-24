// Wire protocol v2 — composite renderer for `ui.session_row{ session_uuid }`.
// Single-row variant of SessionList; reads one session from the store and
// renders the same two-line content (primary name + titleLine + subtext)
// plus the activity dot and an actions-menu trigger.

import React, { type MouseEvent, type ReactElement } from 'react'
import clsx from 'clsx'

import { useSessionStore } from '../../store/entities'
import {
  activityState,
  displayName,
  subtext,
  titleLine,
} from '../../store/selectors/session-row'
import type {
  SessionRowPropsV1,
  UiActionV1,
  UiSurfaceDensityV1,
  UiValueV1,
} from '../../ui_contract/types'
import type { RenderContext } from '../../ui_contract/context'
import { resolveValue } from '../../ui_contract/viewport'

type SessionRecord = {
  session_uuid?: string
  id?: string
  label?: string
  title?: string
  display_name?: string
  task?: string
  target_name?: string
  branch_name?: string
  agent_name?: string
  profile_name?: string
  session_type?: string
  is_idle?: boolean
  notification?: boolean
}

export type SessionRowProps = SessionRowPropsV1 & { ctx: RenderContext }

export function SessionRow({
  sessionUuid,
  density,
  ctx,
}: SessionRowProps): ReactElement {
  const session = useSessionStore(
    (state) => state.byId[sessionUuid] as SessionRecord | undefined,
  )
  const resolvedDensity =
    resolveValue<UiSurfaceDensityV1>(
      density as UiValueV1<UiSurfaceDensityV1> | undefined,
      ctx.viewport,
    ) ?? 'panel'
  if (!session) {
    return (
      <div
        className="text-sm text-zinc-500"
        data-session-uuid={sessionUuid}
        data-testid="session-row-unavailable"
      >
        (session unavailable)
      </div>
    )
  }

  const sessionId = session.id ?? sessionUuid
  const primaryName = displayName(session)
  const subtitle = titleLine(session)
  const tail = subtext(session)
  const activity = activityState(session)
  const isAccessory = session.session_type === 'accessory'
  const titleSize = resolvedDensity === 'sidebar' ? 'text-xs' : 'text-sm'

  const handleMenuOpen = (event: MouseEvent) => {
    event.preventDefault()
    event.stopPropagation()
    ctx.dispatch(
      {
        id: 'botster.session.menu.open',
        payload: { sessionId, sessionUuid },
      } as UiActionV1,
      { element: event.currentTarget as Element },
    )
  }

  return (
    <div
      className={clsx(
        'flex items-start gap-2 px-2 py-1.5 text-zinc-200',
        resolvedDensity === 'sidebar' && 'py-0.5',
      )}
      data-session-uuid={sessionUuid}
      data-session-id={sessionId}
    >
      {activity === 'active' && (
        <span
          aria-label="Active"
          className="mt-1 size-2 shrink-0 rounded-full bg-emerald-400"
        />
      )}
      <div className="min-w-0 flex-1">
        <div className="flex min-w-0 items-center gap-2">
          {session.notification && (
            <span
              aria-hidden="true"
              data-testid="notification-dot"
              className="size-2 shrink-0 rounded-full bg-amber-400"
            />
          )}
          <span
            className={clsx(
              titleSize,
              'min-w-0 truncate font-mono',
              isAccessory && 'text-zinc-400',
            )}
            data-testid="session-row-primary"
          >
            {primaryName}
          </span>
        </div>
        {(subtitle || tail) && (
          <div className="flex min-w-0 flex-wrap items-center gap-x-2 text-xs text-zinc-500">
            {subtitle && (
              <span
                data-testid="session-row-title-line"
                className="min-w-0 truncate italic"
              >
                {subtitle}
              </span>
            )}
            {tail && (
              <span
                data-testid="session-row-subtext"
                className="min-w-0 truncate"
              >
                {tail}
              </span>
            )}
          </div>
        )}
      </div>
      <button
        type="button"
        onClick={handleMenuOpen}
        aria-label="Session actions"
        data-testid="session-actions-trigger"
        className={clsx(
          'inline-flex size-6 shrink-0 items-center justify-center rounded text-zinc-400',
          'hover:bg-zinc-800/50 hover:text-zinc-200',
        )}
      >
        ⋮
      </button>
    </div>
  )
}
