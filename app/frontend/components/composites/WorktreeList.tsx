// Wire protocol v2 — composite renderer for `ui.worktree_list{ target_id }`.
// Filters worktrees to those belonging to the supplied spawn target.

import React, { useMemo, type ReactElement } from 'react'

import { useWorktreeStore } from '../../store/entities'
import type { WorktreeListPropsV1 } from '../../ui_contract/types'
import type { RenderContext } from '../../ui_contract/context'

type WorktreeRecord = {
  worktree_path?: string
  target_id?: string
  branch?: string
  path?: string
}

export type WorktreeListProps = WorktreeListPropsV1 & { ctx: RenderContext }

export function WorktreeList({ targetId }: WorktreeListProps): ReactElement {
  const worktreeOrder = useWorktreeStore((state) => state.order)
  const worktreesById = useWorktreeStore((state) => state.byId)
  const worktrees = useMemo(
    () =>
      worktreeOrder
        .map((id) => [id, worktreesById[id] as WorktreeRecord] as const)
        .filter(([, wt]) => wt?.target_id === targetId),
    [targetId, worktreeOrder, worktreesById],
  )
  if (worktrees.length === 0) {
    return <div className="text-sm text-zinc-500">No worktrees</div>
  }
  return (
    <ul className="flex flex-col gap-0.5">
      {worktrees.map(([id, wt]) => {
        const w = wt as WorktreeRecord
        const path = w.worktree_path || w.path || (id as string)
        return (
          <li
            key={path as string}
            data-worktree-path={path}
            className="px-2 py-1 text-sm text-zinc-200"
          >
            <span className="font-mono">{path}</span>
            {w.branch && (
              <span className="ml-2 text-xs text-zinc-500">{w.branch}</span>
            )}
          </li>
        )
      })}
    </ul>
  )
}
