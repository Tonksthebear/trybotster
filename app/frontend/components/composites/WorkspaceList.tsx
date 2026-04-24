// Wire protocol v2 — composite renderer for `ui.workspace_list{}`.
// Renders the bare list of workspaces without the session children join.

import React, { useMemo, type ReactElement } from 'react'

import { useWorkspaceEntityStore } from '../../store/entities'
import type { WorkspaceListPropsV1 } from '../../ui_contract/types'
import type { RenderContext } from '../../ui_contract/context'

type WorkspaceRecord = { workspace_id?: string; name?: string }

export type WorkspaceListProps = WorkspaceListPropsV1 & { ctx: RenderContext }

export function WorkspaceList(_props: WorkspaceListProps): ReactElement {
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
  if (workspaces.length === 0) {
    return <div className="text-sm text-zinc-500">No workspaces</div>
  }
  return (
    <ul className="flex flex-col gap-0.5">
      {workspaces.map(([id, ws]) => (
        <li
          key={id as string}
          className="px-2 py-1 text-sm text-zinc-200"
          data-workspace-id={id}
        >
          {(ws as WorkspaceRecord).name || (id as string)}
        </li>
      ))}
    </ul>
  )
}
