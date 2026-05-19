// Wire protocol — composite renderer for `ui.session_list{}`.
//
// Reads from the session + workspace entity stores and the
// ui-presentation-store (selection / collapse). Renders the workspace-grouped
// tree the legacy web/layout.lua used to ship hub-side. Workspaces do NOT carry
// session lists — membership is derived client-side by filtering sessions
// where session.workspace_id == workspace.id (design brief §12.5).
//
// Fidelity restoration: each row carries the activity dot, two-line
// content (primary name + titleLine + subtext), inline generic session-action
// indicators, and an actions-menu trigger. A `<SessionActionsMenu>` mounted
// outside the tree (App.jsx / HubShow.jsx) intercepts the
// `botster.session.menu.open` action this row dispatches and renders a
// Catalyst dropdown anchored to the trigger button.

import React, { useMemo, type MouseEvent, type ReactElement } from 'react'
import clsx from 'clsx'

import {
  useSessionActionStore,
  useSessionStore,
  useWorkspaceEntityStore,
} from '../../store/entities'
import {
  selectRoutesForHub,
  useRouteRegistryStore,
} from '../../store/route-registry-store'
import { useUiPresentationStore } from '../../store/ui-presentation-store'
import {
  activityState,
  displayName,
  subtext,
  titleLine,
} from '../../store/selectors/session-row'
import { activeAgentWorkspaces } from '../../lib/entity-selectors'
import {
  SessionActionErrorPanel,
  SessionActionIndicators,
  type SessionActionRecord,
  visibleSessionActions,
} from './SessionActionAffordances'
import type { RenderContext } from '../../ui_contract/context'
import { resolveValue } from '../../ui_contract/viewport'
import type {
  SessionListProps as UiSessionListProps,
  UiAction,
  UiSurfaceDensity,
  UiValue,
} from '../../ui_contract/types'
import { IconGlyph } from '../../ui_contract/icons'

type SessionRecord = {
  session_uuid?: string
  id?: string
  title?: string
  display_name?: string
  label?: string
  workspace_id?: string
  session_type?: string
  owner_plugin?: string
  visibility?: string
  surface?: string
  output_activity?: 'active' | 'idle'
  notification?: boolean
  task?: string
  target_name?: string
  branch_name?: string
  agent_name?: string
  [key: string]: unknown
}

type WorkspaceRecord = {
  workspace_id?: string
  name?: string
  status?: string
  [key: string]: unknown
}

type RouteNav = {
  section?: string
  order?: number
  label?: string
  icon?: string
}

type RouteRegistryEntry = {
  path?: string
  base_path?: string
  surface?: string
  label?: string
  icon?: string
  hide_from_nav?: boolean
  nav?: RouteNav | false
}

type OrderedEntityState<T> = {
  order: string[]
  byId: Record<string, T>
}

type UiPresentationState = {
  selectedSessionId: string | null
  collapsedWorkspaceIds: Set<string>
  setSelectedSessionId: (sessionId: string) => void
  toggleWorkspaceCollapsed: (workspaceId: string) => void
}

export type SessionListProps = UiSessionListProps & {
  ctx: RenderContext
}

export function SessionList({
  density,
  grouping,
  ownerPlugin,
  visibility,
  surface,
  ctx,
}: SessionListProps): ReactElement {
  const resolvedDensity =
    resolveValue<UiSurfaceDensity>(
      density as UiValue<UiSurfaceDensity> | undefined,
      ctx.viewport,
    ) ?? 'panel'
  const groupingMode = grouping ?? 'workspace'

  const sessionOrder = useSessionStore(
    (state: OrderedEntityState<SessionRecord>) => state.order,
  ) as string[]
  const sessionsById = useSessionStore(
    (state: OrderedEntityState<SessionRecord>) => state.byId,
  ) as Record<string, SessionRecord>
  const sessionActionOrder = useSessionActionStore(
    (state: OrderedEntityState<SessionActionRecord>) => state.order,
  ) as string[]
  const sessionActionsById = useSessionActionStore(
    (state: OrderedEntityState<SessionActionRecord>) => state.byId,
  ) as Record<string, SessionActionRecord>
  const requestedVisibility = visibility ?? 'workspace'
  const sessions = useMemo<Array<readonly [string, SessionRecord]>>(
    () =>
      sessionOrder
        .map((id) => [
          id,
          sessionsById[id] as SessionRecord,
        ] as const)
        .filter(([, session]) => {
          if (!session) return false
          const sessionVisibility = session.visibility || 'workspace'
          if (requestedVisibility !== 'all' && sessionVisibility !== requestedVisibility) {
            return false
          }
          if (ownerPlugin && session.owner_plugin !== ownerPlugin) return false
          if (surface && session.surface !== surface) return false
          return true
        }),
    [ownerPlugin, requestedVisibility, sessionOrder, sessionsById, surface],
  )
  const workspaceOrder = useWorkspaceEntityStore(
    (state: OrderedEntityState<WorkspaceRecord>) => state.order,
  ) as string[]
  const workspacesById = useWorkspaceEntityStore(
    (state: OrderedEntityState<WorkspaceRecord>) => state.byId,
  ) as Record<string, WorkspaceRecord>
  const routeEntries = useRouteRegistryStore((state: unknown) =>
    selectRoutesForHub(state, ctx.hubId),
  ) as RouteRegistryEntry[]
  // Filter out closed workspaces and workspaces with no active agent. The hub
  // emits `entity_patch(workspace, status="closed")` when the last session in
  // a workspace closes (handlers/connections.lua workspace_closed hook); the
  // record stays in the store so a future re-open is just an upsert away, but
  // headers + groups should not render until a live agent session exists.
  const workspaces = useMemo(
    () => activeAgentWorkspaces({
      workspaceOrder,
      workspacesById,
      sessionOrder,
      sessionsById,
      sessionFilter: {
        ownerPlugin,
        visibility: requestedVisibility,
        surface,
      },
    }),
    [
      ownerPlugin,
      requestedVisibility,
      surface,
      workspaceOrder,
      workspacesById,
      sessionOrder,
      sessionsById,
    ],
  )

  const selectedSessionId = useUiPresentationStore(
    (s: UiPresentationState) => s.selectedSessionId,
  )
  const collapsedWorkspaceIds = useUiPresentationStore(
    (s: UiPresentationState) => s.collapsedWorkspaceIds,
  )
  const setSelected = useUiPresentationStore(
    (s: UiPresentationState) => s.setSelectedSessionId,
  )
  const toggleCollapsed = useUiPresentationStore(
    (s: UiPresentationState) => s.toggleWorkspaceCollapsed,
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

  const sessionHrefFor = (session: SessionRecord, sessionUuid: string): string | undefined => {
    if (!ctx.hubId || !sessionUuid) return undefined
    const surfaceName = session.surface || session.owner_plugin
    if (surfaceName && session.visibility === 'plugin') {
      const entry = routeEntries.find((candidate) => candidate.surface === surfaceName)
      const basePath = entry?.base_path
      if (basePath && basePath !== '/') {
        return `/hubs/${ctx.hubId}${basePath}/sessions/${sessionUuid}`
      }
    }
    return `/hubs/${ctx.hubId}/sessions/${sessionUuid}`
  }

  const handleSelect = (
    sessionUuid: string | undefined,
    sessionId: string | undefined,
    url: string | undefined,
  ) => (
    event: MouseEvent,
  ) => {
    if (!sessionUuid) return
    event.preventDefault()
    setSelected(sessionUuid)
    ctx.dispatch(
      {
        id: 'botster.session.select',
        payload: { sessionUuid, sessionId: sessionId || sessionUuid, url },
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
      } as UiAction,
      { element: event.currentTarget as Element },
    )
  }

  const handleWorkspaceRename = (workspaceId: string, title: string) => (
    event: MouseEvent,
  ) => {
    event.preventDefault()
    event.stopPropagation()
    ctx.dispatch(
      {
        id: 'botster.workspace.rename.request',
        payload: { workspaceId, title },
      } as UiAction,
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
    const sessionActions = visibleSessionActions(
      sessionActionOrder,
      sessionActionsById as Record<string, SessionActionRecord>,
      sessionUuid,
    )
    const selected = selectedSessionId === sessionUuid
    const sessionHref = sessionHrefFor(session, sessionUuid)

    // Row state → left-border color. Priority: notification beats active
    // beats idle so an alert always wins surface attention. One color at
    // a time. Idle rows still carry a gray border so the column edge stays
    // visually consistent regardless of state.
    const rowState = session.notification
      ? 'notification'
      : activity === 'active'
        ? 'active'
        : 'idle'
    const rowStateBorder =
      rowState === 'notification' ? 'border-amber-400'
      : rowState === 'active' ? 'border-emerald-500'
      : 'border-zinc-700'

    // In-row actions trigger. Catalyst <Button plain> doesn't fit here —
    // its base padding is row-sized, which would visually balloon every
    // session row. We keep a styled <button> sized for the row but use
    // IconGlyph for the ellipsis so it's an actual SVG, not unicode.
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
        <IconGlyph name="ellipsis-vertical" className="size-4" />
      </button>
    )

    const containerClass = clsx(
      'group flex min-w-0 items-start gap-2 rounded-md border-l-4 px-2 py-1.5 text-sm',
      rowStateBorder,
      indent > 0 && 'ml-4',
      selected
        ? 'bg-sky-500/20 text-sky-300'
        : 'cursor-pointer text-zinc-200 hover:bg-zinc-800/50',
    )

    const titleSize = resolvedDensity === 'sidebar' ? 'text-xs' : 'text-sm'
    const isAccessory = session.session_type === 'accessory'

    const lines = (
      <div className="min-w-0 flex-1" data-row-state={rowState}>
        <div className="flex min-w-0 items-center gap-2">
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
        {lines}
        <div className="flex shrink-0 items-center gap-1 pt-0.5">
          <SessionActionIndicators
            sessionUuid={sessionUuid}
            actions={sessionActions}
            ctx={ctx}
          />
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
        onClick={handleSelect(sessionUuid, sessionId, sessionHref)}
        className={containerClass}
        data-session-id={sessionId}
      >
        {innerSlots}
      </a>
    ) : (
      <div
        role="button"
        tabIndex={0}
        onClick={handleSelect(sessionUuid, sessionId, sessionHref)}
        onKeyDown={(event) => {
          if (event.key === 'Enter' || event.key === ' ') {
            handleSelect(sessionUuid, sessionId, sessionHref)(event as unknown as MouseEvent)
          }
        }}
        className={containerClass}
        data-session-id={sessionId}
      >
        {innerSlots}
      </div>
    )

    const errorPanel = (
      <SessionActionErrorPanel
        sessionUuid={sessionUuid}
        actions={sessionActions}
        ctx={ctx}
        indent={indent > 0}
      />
    )

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
  for (const workspace of workspaces) {
    const id = workspace.id || ''
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
        <div
          className={clsx(
            'group flex items-center gap-1 px-2 py-1 text-xs font-medium uppercase tracking-wider text-zinc-400',
          )}
        >
          <button
            type="button"
            onClick={() => toggleCollapsed(id)}
            aria-expanded={!collapsed}
            className="flex min-w-0 flex-1 items-center gap-1 text-left hover:text-zinc-300"
          >
            <IconGlyph
              name={collapsed ? 'chevron-right' : 'chevron-down'}
              className="size-3.5 shrink-0"
            />
            <span className="min-w-0 truncate">{ws.name || id}</span>
          </button>
          <button
            type="button"
            aria-label={`Rename workspace ${ws.name || id}`}
            onClick={handleWorkspaceRename(id, ws.name || id)}
            className="rounded p-0.5 text-zinc-500 opacity-0 hover:bg-zinc-800 hover:text-zinc-200 focus:opacity-100 focus:outline-none focus:ring-1 focus:ring-zinc-500 group-hover:opacity-100"
          >
            <IconGlyph name="pencil" className="size-3.5" />
          </button>
        </div>
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

  return <ul className="flex flex-col gap-1">{groups}</ul>
}
