// Wire protocol v2 — composite renderer for `ui.session_list{}`.
//
// Reads from the session + workspace entity stores and the
// ui-presentation-store (selection / collapse). Renders the workspace-grouped
// tree the v1 web/layout.lua used to ship hub-side. Workspaces do NOT carry
// session lists — membership is derived client-side by filtering sessions
// where session.workspace_id == workspace.id (design brief §12.5).
//
// v1-fidelity restoration: each row carries the activity dot, two-line
// content (primary name + titleLine + subtext), inline hosted-preview
// indicator, and an actions-menu trigger. A `<SessionActionsMenu>` mounted
// outside the tree (App.jsx / HubShow.jsx) intercepts the
// `botster.session.menu.open` action this row dispatches and renders a
// Catalyst dropdown anchored to the trigger button.

import React, { useMemo, type MouseEvent, type ReactElement } from 'react'
import clsx from 'clsx'

import {
  useSessionStore,
  useWorkspaceEntityStore,
} from '../../store/entities'
import { useUiPresentationStore } from '../../store/ui-presentation-store'
import {
  activityState,
  displayName,
  previewState,
  subtext,
  titleLine,
} from '../../store/selectors/session-row'
import type { RenderContext } from '../../ui_contract/context'
import { resolveValue } from '../../ui_contract/viewport'
import type {
  SessionListPropsV1,
  UiActionV1,
  UiSurfaceDensityV1,
  UiValueV1,
} from '../../ui_contract/types'

type SessionRecord = {
  session_uuid?: string
  id?: string
  title?: string
  display_name?: string
  label?: string
  workspace_id?: string
  session_type?: string
  is_idle?: boolean
  notification?: boolean
  port?: number
  task?: string
  target_name?: string
  branch_name?: string
  agent_name?: string
  profile_name?: string
  hosted_preview?: {
    status?: string
    url?: string | null
    error?: string | null
    install_url?: string | null
  } | null
  [key: string]: unknown
}

type WorkspaceRecord = {
  workspace_id?: string
  name?: string
  [key: string]: unknown
}

export type SessionListProps = SessionListPropsV1 & {
  ctx: RenderContext
}

export function SessionList({
  density,
  grouping,
  showNavEntries,
  ctx,
}: SessionListProps): ReactElement {
  const resolvedDensity =
    resolveValue<UiSurfaceDensityV1>(
      density as UiValueV1<UiSurfaceDensityV1> | undefined,
      ctx.viewport,
    ) ?? 'panel'
  const groupingMode = grouping ?? 'workspace'

  const sessionOrder = useSessionStore((state) => state.order)
  const sessionsById = useSessionStore((state) => state.byId)
  const sessions = useMemo(
    () =>
      sessionOrder.map((id) => [
        id,
        sessionsById[id] as SessionRecord,
      ] as const),
    [sessionOrder, sessionsById],
  )
  const workspaceOrder = useWorkspaceEntityStore((state) => state.order)
  const workspacesById = useWorkspaceEntityStore((state) => state.byId)
  const workspaces = useMemo(
    () =>
      workspaceOrder.map((id) => [
        id,
        workspacesById[id] as WorkspaceRecord,
      ] as const),
    [workspaceOrder, workspacesById],
  )

  const selectedSessionId = useUiPresentationStore((s) => s.selectedSessionId)
  const collapsedWorkspaceIds = useUiPresentationStore(
    (s) => s.collapsedWorkspaceIds,
  )
  const setSelected = useUiPresentationStore((s) => s.setSelectedSessionId)
  const toggleCollapsed = useUiPresentationStore(
    (s) => s.toggleWorkspaceCollapsed,
  )

  if (sessions.length === 0) {
    return (
      <div
        className={clsx(
          'flex flex-col items-center justify-center gap-2 py-8 text-center',
          'text-sm text-zinc-500',
        )}
      >
        No sessions running
      </div>
    )
  }

  const handleSelect = (sessionUuid: string | undefined, sessionId: string | undefined) => (
    event: MouseEvent,
  ) => {
    if (!sessionUuid) return
    event.preventDefault()
    setSelected(sessionUuid)
    ctx.dispatch(
      {
        id: 'botster.session.select',
        payload: { sessionUuid, sessionId: sessionId || sessionUuid },
      },
      { element: event.currentTarget as Element },
    )
  }

  const handleMenuOpen = (sessionId: string, sessionUuid: string) => (event: MouseEvent) => {
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

  const renderRow = (
    rowKey: string,
    session: SessionRecord,
    indent = 0,
  ): ReactElement => {
    const sessionUuid = session.session_uuid ?? rowKey
    const sessionId = session.id ?? sessionUuid
    const primaryName = displayName(session)
    const subtitle = titleLine(session)
    const tail = subtext(session)
    const activity = activityState(session)
    const preview = previewState(session)
    const selected = selectedSessionId === sessionUuid
    const sessionHref =
      ctx.hubId && sessionUuid
        ? `/hubs/${ctx.hubId}/sessions/${sessionUuid}`
        : undefined

    // Activity dot: green only when actively running. Idle / accessory /
    // hidden render no dot so quiet rows stay quiet.
    const dot = activity === 'active' ? (
      <span
        aria-label="Active"
        className="size-2 shrink-0 rounded-full bg-emerald-400"
      />
    ) : null

    // Hosted-preview indicator. Running + url → clickable button. Other
    // statuses → small status badge. Inactive / unavailable → null.
    let previewIndicator: ReactElement | null = null
    if (preview.canPreview) {
      if (preview.status === 'running' && preview.url) {
        previewIndicator = (
          <button
            type="button"
            onClick={(event) => {
              event.preventDefault()
              event.stopPropagation()
              ctx.dispatch(
                {
                  id: 'botster.session.preview.open',
                  payload: {
                    sessionId,
                    sessionUuid,
                    url: preview.url ?? undefined,
                  },
                } as UiActionV1,
                { element: event.currentTarget as Element },
              )
            }}
            className="inline-flex items-center gap-1 rounded text-xs text-emerald-300 hover:underline"
            data-testid="hosted-preview-running"
          >
            Running
          </button>
        )
      } else if (preview.status !== 'inactive' && preview.status !== 'unavailable') {
        const label =
          preview.status === 'starting' ? 'Starting…'
          : preview.status === 'error' ? 'Error'
          : preview.status
        const tone =
          preview.status === 'error' ? 'text-red-400'
          : preview.status === 'starting' ? 'text-amber-400'
          : 'text-zinc-400'
        previewIndicator = (
          <span
            data-testid={`hosted-preview-status-${preview.status}`}
            className={clsx('text-xs', tone)}
          >
            {label}
          </span>
        )
      }
    }

    const actionsTrigger = (
      <button
        type="button"
        onClick={handleMenuOpen(sessionId, sessionUuid)}
        aria-label="Session actions"
        data-testid="session-actions-trigger"
        data-session-id={sessionId}
        className={clsx(
          'inline-flex size-6 shrink-0 items-center justify-center rounded text-zinc-400',
          'hover:bg-zinc-800/50 hover:text-zinc-200',
        )}
      >
        ⋮
      </button>
    )

    const containerClass = clsx(
      'group flex min-w-0 items-start gap-2 rounded-md px-2 py-1.5 text-sm',
      indent > 0 && 'ml-4',
      selected
        ? 'bg-sky-500/20 text-sky-300'
        : 'cursor-pointer text-zinc-200 hover:bg-zinc-800/50',
    )

    const titleSize = resolvedDensity === 'sidebar' ? 'text-xs' : 'text-sm'
    const isAccessory = session.session_type === 'accessory'

    const lines = (
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
              selected ? 'font-medium' : 'font-normal',
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
    )

    const innerSlots = (
      <>
        {dot && <div className="pt-1.5">{dot}</div>}
        {lines}
        <div className="flex shrink-0 items-center gap-1 pt-0.5">
          {previewIndicator}
          {actionsTrigger}
        </div>
      </>
    )

    // Wrap the row body (everything except the actions trigger) so the
    // anchor / button surface is the activatable target. The actions
    // trigger lives in `innerSlots` outside the activatable surface so
    // its own click doesn't bubble up to navigation.
    const rowBody = sessionHref ? (
      <a
        href={sessionHref}
        onClick={handleSelect(sessionUuid, sessionId)}
        className={containerClass}
        data-session-id={sessionId}
      >
        {innerSlots}
      </a>
    ) : (
      <div
        role="button"
        tabIndex={0}
        onClick={handleSelect(sessionUuid, sessionId)}
        onKeyDown={(event) => {
          if (event.key === 'Enter' || event.key === ' ') {
            handleSelect(sessionUuid, sessionId)(event as unknown as MouseEvent)
          }
        }}
        className={containerClass}
        data-session-id={sessionId}
      >
        {innerSlots}
      </div>
    )

    // Inline error panel: shown directly below the row when the hosted
    // preview is in error state. Mirrors the v1 web/layout.lua
    // hosted_preview_error_panel composition.
    const errorPanel =
      preview.status === 'error' && preview.error ? (
        <div
          data-testid="hosted-preview-error"
          className={clsx(
            'mx-2 mt-0.5 rounded-md border border-red-500/30 bg-red-500/10 px-2 py-1',
            'text-xs text-red-300',
            indent > 0 && 'ml-6',
          )}
        >
          <div className="flex items-start gap-1">
            <span aria-hidden="true">⚠</span>
            <span className="min-w-0 flex-1">{preview.error}</span>
          </div>
          {preview.installUrl && (
            <button
              type="button"
              onClick={(event) => {
                event.preventDefault()
                event.stopPropagation()
                ctx.dispatch(
                  {
                    id: 'botster.session.preview.open',
                    payload: {
                      sessionUuid,
                      url: preview.installUrl ?? undefined,
                    },
                  } as UiActionV1,
                  { element: event.currentTarget as Element },
                )
              }}
              className="mt-1 inline-flex text-xs text-red-300 hover:underline"
            >
              Install cloudflared
            </button>
          )}
        </div>
      ) : null

    return (
      <li
        key={sessionUuid}
        data-session-id={sessionId}
        aria-selected={selected || undefined}
      >
        {rowBody}
        {errorPanel}
      </li>
    )
  }

  // Build groups. When grouping=flat, render a single bucket of all sessions.
  if (groupingMode === 'flat') {
    return (
      <ul className="flex flex-col gap-0.5">
        {sessions.map(([id, session]) =>
          renderRow(id as string, session as SessionRecord),
        )}
      </ul>
    )
  }

  // grouping = workspace
  const seenSessionIds = new Set<string>()
  const groups: ReactElement[] = []
  for (const [wsId, workspace] of workspaces) {
    const id = (wsId as string) || ''
    const ws = workspace as WorkspaceRecord
    const collapsed = collapsedWorkspaceIds.has(id)
    const childRows: ReactElement[] = []
    for (const [sessId, session] of sessions) {
      const s = session as SessionRecord
      if (s.workspace_id === id) {
        seenSessionIds.add(sessId as string)
        if (!collapsed) {
          childRows.push(renderRow(sessId as string, s, 1))
        }
      }
    }
    groups.push(
      <li key={`ws:${id}`} className="flex flex-col gap-0.5">
        <button
          type="button"
          onClick={() => toggleCollapsed(id)}
          aria-expanded={!collapsed}
          className={clsx(
            'flex items-center gap-1 px-2 py-1 text-xs font-medium uppercase tracking-wider text-zinc-400 hover:text-zinc-300',
          )}
        >
          <span aria-hidden="true">{collapsed ? '▸' : '▾'}</span>
          <span className="min-w-0 truncate">{ws.name || id}</span>
        </button>
        {!collapsed && (
          <ul className="flex flex-col gap-0.5">{childRows}</ul>
        )}
      </li>,
    )
  }

  // Ungrouped bucket for sessions without a known workspace.
  const ungroupedRows: ReactElement[] = []
  for (const [sessId, session] of sessions) {
    if (!seenSessionIds.has(sessId as string)) {
      ungroupedRows.push(
        renderRow(sessId as string, session as SessionRecord),
      )
    }
  }
  if (ungroupedRows.length > 0) {
    groups.push(
      <li key="ungrouped" className="flex flex-col gap-0.5">
        <ul className="flex flex-col gap-0.5">{ungroupedRows}</ul>
      </li>,
    )
  }

  void showNavEntries // sidebar nav entries: future iteration

  return <ul className="flex flex-col gap-1">{groups}</ul>
}
