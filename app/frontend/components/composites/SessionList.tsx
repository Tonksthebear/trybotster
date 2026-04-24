// Wire protocol v2 — composite renderer for `ui.session_list{}`.
//
// Reads from the session + workspace entity stores and the
// ui-presentation-store (selection / collapse). Renders the workspace-grouped
// tree the v1 web/layout.lua used to ship hub-side. Workspaces do NOT carry
// session lists — membership is derived client-side by filtering sessions
// where session.workspace_id == workspace.id (design brief §12.5).

import React, { useMemo, type MouseEvent, type ReactElement } from 'react'
import clsx from 'clsx'

import {
  useSessionStore,
  useWorkspaceEntityStore,
} from '../../store/entities'
import { useUiPresentationStore } from '../../store/ui-presentation-store'
import type { RenderContext } from '../../ui_contract/context'
import { resolveValue } from '../../ui_contract/viewport'
import type {
  SessionListPropsV1,
  UiSurfaceDensityV1,
  UiValueV1,
} from '../../ui_contract/types'

type SessionRecord = {
  session_uuid?: string
  title?: string
  display_name?: string
  workspace_id?: string
  session_type?: string
  is_idle?: boolean
  notification?: boolean
  hosted_preview?: { status?: string; url?: string; error?: string } | null
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

  const handleSelect = (sessionUuid: string | undefined) => (event: MouseEvent) => {
    if (!sessionUuid) return
    event.preventDefault()
    setSelected(sessionUuid)
    ctx.dispatch(
      {
        id: 'botster.session.select',
        payload: { sessionUuid },
      },
      { element: event.currentTarget as Element },
    )
  }

  const renderRow = (
    sessionId: string,
    session: SessionRecord,
    indent = 0,
  ): ReactElement => {
    const sessionUuid = session.session_uuid ?? sessionId
    const title =
      (typeof session.title === 'string' && session.title) ||
      (typeof session.display_name === 'string' && session.display_name) ||
      sessionUuid
    const subtext =
      session.session_type === 'accessory' ? 'accessory' : undefined
    const selected = selectedSessionId === sessionUuid
    const sessionHref =
      ctx.hubId && sessionUuid
        ? `/hubs/${ctx.hubId}/sessions/${sessionUuid}`
        : undefined
    const containerClass = clsx(
      'flex min-w-0 items-center gap-2 rounded-md px-2 py-1.5 text-sm',
      indent > 0 && 'ml-4',
      selected
        ? 'bg-sky-500/20 text-sky-300'
        : 'cursor-pointer text-zinc-200 hover:bg-zinc-800/50',
    )
    const inner = (
      <>
        {session.notification && (
          <span
            aria-hidden="true"
            className="size-2 shrink-0 rounded-full bg-amber-400"
          />
        )}
        <span className="min-w-0 flex-1 truncate">{title}</span>
        {subtext && <span className="text-xs text-zinc-500">{subtext}</span>}
      </>
    )
    return (
      <li
        key={sessionUuid}
        data-session-id={sessionUuid}
        aria-selected={selected || undefined}
      >
        {sessionHref ? (
          <a href={sessionHref} onClick={handleSelect(sessionUuid)} className={containerClass}>
            {inner}
          </a>
        ) : (
          <button
            type="button"
            onClick={handleSelect(sessionUuid)}
            className={clsx(containerClass, 'w-full text-left')}
          >
            {inner}
          </button>
        )}
      </li>
    )
  }

  // Build groups. When grouping=flat, render a single bucket of all sessions.
  if (groupingMode === 'flat') {
    return (
      <ul className="flex flex-col gap-0.5">
        {sessions.map(([id, session]) => renderRow(id as string, session as SessionRecord))}
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
      ungroupedRows.push(renderRow(sessId as string, session as SessionRecord))
    }
  }
  if (ungroupedRows.length > 0) {
    groups.push(
      <li key="ungrouped" className="flex flex-col gap-0.5">
        <ul className="flex flex-col gap-0.5">{ungroupedRows}</ul>
      </li>,
    )
  }

  void resolvedDensity // density currently maps to typography variant; future iteration tweaks it
  void showNavEntries // sidebar nav entries: future iteration

  return <ul className="flex flex-col gap-1">{groups}</ul>
}
