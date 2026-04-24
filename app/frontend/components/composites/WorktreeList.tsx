// Wire protocol v2 — composite renderer for `ui.worktree_list{ target_id }`.
// Filters worktrees to those belonging to the supplied spawn target.

import React, { type ReactElement } from 'react'
import { useShallow } from 'zustand/react/shallow'

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
  const worktrees = useWorktreeStore(
    useShallow((state) =>
      state.order
        .map((id) => [id, state.byId[id] as WorktreeRecord])
        .filter(
          ([, wt]) => (wt as WorktreeRecord)?.target_id === targetId,
        ),
    ),
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
