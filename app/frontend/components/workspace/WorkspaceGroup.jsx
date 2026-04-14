import React from 'react'
import clsx from 'clsx'
import { useWorkspaceStore } from '../../store/workspace-store'
import { dispatch, ACTION } from '../../lib/actions'
import SessionRow from './SessionRow'

export default function WorkspaceGroup({ workspaceId, hubId, surface }) {
  const ws = useWorkspaceStore((s) => s.workspacesById[workspaceId])
  const expanded = useWorkspaceStore(
    (s) => !s.collapsedWorkspaceIds.has(workspaceId)
  )

  if (!ws || !Array.isArray(ws.agents) || ws.agents.length === 0) return null

  const title = ws.name || ws.id
  const count = ws.agents.length
  const density = surface === 'sidebar' ? 'sidebar' : 'panel'
  const isSidebar = density === 'sidebar'

  function handleToggle() {
    dispatch({
      action: ACTION.WORKSPACE_TOGGLE,
      payload: { workspaceId },
    })
  }

  function handleRename(e) {
    e.stopPropagation()
    dispatch({
      action: ACTION.WORKSPACE_RENAME,
      payload: { workspaceId, title },
    })
  }

  return (
    <div>
      <div
        onClick={handleToggle}
        className={clsx(
          'flex items-center cursor-pointer select-none group/ws',
          isSidebar ? 'gap-1.5 px-2 pt-2 pb-1' : 'gap-2 py-2'
        )}
      >
        <svg
          className={clsx(
            'shrink-0 transition-transform duration-150',
            isSidebar ? 'size-3 text-zinc-600' : 'size-4 text-zinc-500',
            !expanded && '-rotate-90'
          )}
          viewBox="0 0 20 20"
          fill="currentColor"
        >
          <path
            fillRule="evenodd"
            d="M6.293 7.293a1 1 0 011.414 0L10 9.586l2.293-2.293a1 1 0 111.414 1.414l-3 3a1 1 0 01-1.414 0l-3-3a1 1 0 010-1.414z"
            clipRule="evenodd"
          />
        </svg>

        <span
          className={clsx(
            'font-medium truncate uppercase tracking-wider',
            isSidebar
              ? 'text-[10px] text-zinc-500 group-hover/ws:text-zinc-400'
              : 'text-xs text-zinc-400 group-hover/ws:text-zinc-300'
          )}
        >
          {title}
        </span>

        {!isSidebar && (
          <span className="text-[10px] text-zinc-600 shrink-0">{count}</span>
        )}

        <button
          type="button"
          onClick={handleRename}
          className={clsx(
            'ml-auto shrink-0 opacity-0 group-hover/ws:opacity-100 transition-opacity',
            isSidebar
              ? 'p-1 text-zinc-700 hover:text-zinc-400'
              : 'p-1 text-zinc-600 hover:text-zinc-300'
          )}
          title="Rename workspace"
        >
          <svg
            className={isSidebar ? 'size-3' : 'size-3.5'}
            viewBox="0 0 20 20"
            fill="currentColor"
          >
            <path d="M2.695 14.763l-1.262 3.154a.5.5 0 00.65.65l3.155-1.262a4 4 0 001.343-.885L17.5 5.5a2.121 2.121 0 00-3-3L3.58 13.42a4 4 0 00-.885 1.343z" />
          </svg>
        </button>
      </div>

      {expanded && (
        <div className={isSidebar ? 'space-y-0.5' : 'space-y-2'}>
          {ws.agents.map((id) => (
            <SessionRow
              key={id}
              sessionId={id}
              hubId={hubId}
              surface={surface}
            />
          ))}
        </div>
      )}
    </div>
  )
}
