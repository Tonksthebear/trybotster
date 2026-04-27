// Wire protocol — composite renderer for `ui.workspace_list{}`.
// Renders workspaces that currently have at least one active agent session.

import React, { useMemo, type ReactElement } from 'react'

import { useSessionStore, useWorkspaceEntityStore } from '../../store/entities'
import { activeAgentWorkspaces } from '../../lib/entity-selectors'
import type { WorkspaceListProps as UiWorkspaceListProps } from '../../ui_contract/types'
import type { RenderContext } from '../../ui_contract/context'

type WorkspaceRecord = { workspace_id?: string; name?: string; status?: string }

export type WorkspaceListProps = UiWorkspaceListProps & { ctx: RenderContext }

export function WorkspaceList(_props: WorkspaceListProps): ReactElement {
  const workspaceOrder = useWorkspaceEntityStore((state) => state.order)
  const workspacesById = useWorkspaceEntityStore((state) => state.byId)
  const sessionOrder = useSessionStore((state) => state.order)
  const sessionsById = useSessionStore((state) => state.byId)
  const workspaces = useMemo(
    () => activeAgentWorkspaces({
      workspaceOrder,
      workspacesById,
      sessionOrder,
      sessionsById,
    }),
    [workspaceOrder, workspacesById, sessionOrder, sessionsById],
  )
  if (workspaces.length === 0) {
    return <div className="text-sm text-zinc-500">No workspaces</div>
  }
  return (
    <ul className="flex flex-col gap-0.5">
      {workspaces.map((ws) => {
        const id = ws.id
        return (
        <li
          key={id}
          className="px-2 py-1 text-sm text-zinc-200"
          data-workspace-id={id}
        >
          {(ws as WorkspaceRecord).name || id}
        </li>
        )
      })}
    </ul>
  )
}
