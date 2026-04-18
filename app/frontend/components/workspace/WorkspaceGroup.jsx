import React from 'react'
import clsx from 'clsx'
import { useWorkspaceStore } from '../../store/workspace-store'
import { dispatch, ACTION } from '../../lib/actions'
import { UiTree, createHubDispatch } from '../../ui_contract'
import { workspaceHeaderContent } from '../../ui_contract/composites'
import { IconGlyph } from '../../ui_contract/icons'
import SessionRow from './SessionRow'

// WorkspaceGroup header content (chevron + title + count) is built from v1
// primitives via `workspaceHeaderContent`. The click-to-toggle container and
// the hover-revealed rename button remain JSX because neither behavior is
// expressible via v1 primitives (rename is hover-only, which the spec
// intentionally does not put on Button / IconButton in v1).
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

  const headerNode = workspaceHeaderContent({
    title,
    count,
    expanded,
    density,
  })

  return (
    <div>
      <div
        onClick={handleToggle}
        className={clsx(
          'flex items-center cursor-pointer select-none group/ws',
          isSidebar ? 'px-2 pt-2 pb-1' : 'py-2',
          '[&_[data-icon="chevron-down"]]:transition-transform [&_[data-icon="chevron-down"]]:duration-150',
          !expanded && '[&_[data-icon="chevron-down"]]:-rotate-90',
          'uppercase tracking-wider'
        )}
      >
        <div className="flex-1 min-w-0">
          <UiTree
            node={headerNode}
            dispatch={createHubDispatch(hubId ?? '')}
          />
        </div>

        <button
          type="button"
          onClick={handleRename}
          className={clsx(
            'ml-auto shrink-0 opacity-0 group-hover/ws:opacity-100 transition-opacity inline-flex items-center justify-center',
            isSidebar
              ? 'p-1 text-zinc-700 hover:text-zinc-400 size-5'
              : 'p-1 text-zinc-600 hover:text-zinc-300 size-6'
          )}
          title="Rename workspace"
        >
          <IconGlyph name="pencil" className={isSidebar ? 'size-3' : 'size-3.5'} />
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
