// Wire protocol — composite renderer for `ui.session_row{ session_uuid }`.
// Single-row variant of SessionList; reads one session from the store and
// renders the same two-line content (primary name + titleLine + subtext)
// plus the activity dot and an actions-menu trigger.

import React, { type MouseEvent, type ReactElement } from 'react'
import clsx from 'clsx'

import {
  useSessionActionStore,
  useSessionStore,
} from '../../store/entities'
import {
  activityState,
  displayName,
  subtext,
  titleLine,
} from '../../store/selectors/session-row'
import {
  SessionActionErrorPanel,
  SessionActionIndicators,
  type SessionActionRecord,
  visibleSessionActions,
} from './SessionActionAffordances'
import type {
  SessionRowProps as UiSessionRowProps,
  UiAction,
  UiSurfaceDensity,
  UiValue,
} from '../../ui_contract/types'
import type { RenderContext } from '../../ui_contract/context'
import { resolveValue } from '../../ui_contract/viewport'
import { IconGlyph } from '../../ui_contract/icons'

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
  session_type?: string
  output_activity?: 'active' | 'idle'
  notification?: boolean
}

export type SessionRowProps = UiSessionRowProps & { ctx: RenderContext }

export function SessionRow({
  sessionUuid,
  density,
  ctx,
}: SessionRowProps): ReactElement {
  const session = useSessionStore(
    (state: { byId: Record<string, SessionRecord> }) =>
      state.byId[sessionUuid] as SessionRecord | undefined,
  )
  const resolvedDensity =
    resolveValue<UiSurfaceDensity>(
      density as UiValue<UiSurfaceDensity> | undefined,
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
  const sessionActionOrder = useSessionActionStore(
    (state: { order: string[] }) => state.order,
  ) as string[]
  const sessionActionsById = useSessionActionStore(
    (state: { byId: Record<string, SessionActionRecord> }) => state.byId,
  ) as Record<string, SessionActionRecord>
  const sessionActions = visibleSessionActions(
    sessionActionOrder,
    sessionActionsById,
    sessionUuid,
  )
  const primaryName = displayName(session)
  const subtitle = titleLine(session)
  const tail = subtext(session)
  const activity = activityState(session)
  const isAccessory = session.session_type === 'accessory'
  const titleSize = resolvedDensity === 'sidebar' ? 'text-xs' : 'text-sm'

  // Row state → left-border color. Priority: notification > active > idle.
  // One color at a time; idle rows still get a gray border for column-edge
  // consistency.
  const rowState = session.notification
    ? 'notification'
    : activity === 'active'
      ? 'active'
      : 'idle'
  const rowStateBorder =
    rowState === 'notification' ? 'border-amber-400'
    : rowState === 'active' ? 'border-emerald-500'
    : 'border-zinc-700'

  const handleMenuOpen = (event: MouseEvent) => {
    event.preventDefault()
    event.stopPropagation()
    ctx.dispatch(
      {
        id: 'botster.session.menu.open',
        payload: { sessionId, sessionUuid },
      } as UiAction,
      { element: event.currentTarget as Element },
    )
  }

  return (
    <div>
      <div
        className={clsx(
          'flex items-start gap-2 rounded-md border-l-4 px-2 py-1.5 text-zinc-200',
          rowStateBorder,
          resolvedDensity === 'sidebar' && 'py-0.5',
        )}
        data-session-uuid={sessionUuid}
        data-session-id={sessionId}
        data-row-state={rowState}
      >
        <div className="min-w-0 flex-1">
          <div className="flex min-w-0 items-center gap-2">
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
        <div className="flex shrink-0 items-center gap-1 pt-0.5">
          <SessionActionIndicators
            sessionUuid={sessionUuid}
            actions={sessionActions}
            ctx={ctx}
          />
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
            <IconGlyph name="ellipsis-vertical" className="size-4" />
          </button>
        </div>
      </div>
      <SessionActionErrorPanel
        sessionUuid={sessionUuid}
        actions={sessionActions}
        ctx={ctx}
      />
    </div>
  )
}
