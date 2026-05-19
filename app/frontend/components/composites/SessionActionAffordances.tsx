import React, { type MouseEvent, type ReactElement } from 'react'
import clsx from 'clsx'

import type { RenderContext } from '../../ui_contract/context'
import type { UiAction } from '../../ui_contract/types'
import { Badge, BadgeButton } from '../catalyst/badge'

export type SessionActionRecord = {
  id?: string
  session_uuid?: string
  action_id?: string
  label?: string
  status?: string | null
  icon?: string | null
  visibility?: string | null
  enabled?: boolean
  plugin?: string | null
  url?: string | null
  link_url?: string | null
  install_url?: string | null
  installUrl?: string | null
  error?: string | null
  [key: string]: unknown
}

export function visibleSessionActions(
  actionOrder: string[],
  actionsById: Record<string, SessionActionRecord>,
  sessionUuid: string,
): SessionActionRecord[] {
  return actionOrder
    .map((id) => actionsById[id])
    .filter((action): action is SessionActionRecord =>
      !!action &&
      action.session_uuid === sessionUuid &&
      action.visibility !== 'hidden',
    )
}

function actionUrl(action: SessionActionRecord): string | undefined {
  const value = action.url ?? action.link_url ?? action.install_url ?? action.installUrl
  return typeof value === 'string' && value.length > 0 ? value : undefined
}

function actionStatusLabel(status: string): string {
  return status
    .split(/[\s_-]+/)
    .filter(Boolean)
    .map((part) => part.charAt(0).toUpperCase() + part.slice(1))
    .join(' ')
}

function actionStatusColor(status: string): 'emerald' | 'amber' | 'red' | 'zinc' {
  if (status === 'running' || status === 'ready' || status === 'active') return 'emerald'
  if (status === 'starting' || status === 'pending' || status === 'loading') return 'amber'
  if (status === 'error' || status === 'failed') return 'red'
  return 'zinc'
}

type SessionActionAffordanceProps = {
  sessionUuid: string
  actions: SessionActionRecord[]
  ctx: RenderContext
}

export function SessionActionIndicators({
  sessionUuid,
  actions,
  ctx,
}: SessionActionAffordanceProps): ReactElement {
  return (
    <>
      {actions
        .filter((action) =>
          typeof action.status === 'string' &&
          action.status.length > 0 &&
          action.status !== 'inactive' &&
          action.status !== 'hidden',
        )
        .map((action) => {
          const status = action.status as string
          const url = actionUrl(action)
          const label = actionStatusLabel(status)
          if (url) {
            return (
              <BadgeButton
                key={action.id || action.action_id}
                color={actionStatusColor(status)}
                onClick={(event: MouseEvent) => {
                  event.preventDefault()
                  event.stopPropagation()
                  ctx.dispatch(
                    {
                      id: 'botster.url.open',
                      payload: {
                        sessionUuid,
                        actionId: action.action_id,
                        url,
                      },
                    } as UiAction,
                    { element: event.currentTarget as Element },
                  )
                }}
                data-testid="session-action-link"
              >
                {label}
              </BadgeButton>
            )
          }
          return (
            <Badge
              key={action.id || action.action_id}
              color={actionStatusColor(status)}
              data-testid={`session-action-status-${status}`}
            >
              {label}
            </Badge>
          )
        })}
    </>
  )
}

export function SessionActionErrorPanel({
  sessionUuid,
  actions,
  ctx,
  indent = false,
}: SessionActionAffordanceProps & { indent?: boolean }): ReactElement | null {
  const erroredAction = actions.find((action) => (
    typeof action.error === 'string' && action.error.length > 0
  ))
  const errorUrl = erroredAction ? actionUrl(erroredAction) : undefined
  if (!erroredAction?.error) return null

  return (
    <div
      data-testid="session-action-error"
      className={clsx(
        'mx-2 mt-0.5 rounded-md border border-red-500/30 bg-red-500/10 px-2 py-1',
        'text-xs text-red-300',
        indent && 'ml-6',
      )}
    >
      <div className="flex items-start gap-1">
        <span aria-hidden="true">!</span>
        <span className="min-w-0 flex-1">{erroredAction.error}</span>
      </div>
      {errorUrl && (
        <button
          type="button"
          onClick={(event) => {
            event.preventDefault()
            event.stopPropagation()
            ctx.dispatch(
              {
                id: 'botster.url.open',
                payload: {
                  sessionUuid,
                  actionId: erroredAction.action_id,
                  url: errorUrl,
                },
              } as UiAction,
              { element: event.currentTarget as Element },
            )
          }}
          className="mt-1 inline-flex text-xs text-red-300 hover:underline"
        >
          Open {erroredAction.label ?? 'action link'}
        </button>
      )}
    </div>
  )
}
