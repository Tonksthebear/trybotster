// Wire protocol — composite renderer for `ui.spawn_target_list{}`.
//
// Renders configured spawn targets. `on_select` / `on_remove` are action
// templates: their `id` (and optional payload base) is merged with the
// per-row `target_id` before dispatch (design brief §5).

import React, { useMemo, type MouseEvent, type ReactElement } from 'react'

import { useSpawnTargetStore } from '../../store/entities'
import type { SpawnTargetListPropsV1, UiActionV1 } from '../../ui_contract/types'
import type { RenderContext } from '../../ui_contract/context'

type SpawnTargetRecord = {
  target_id?: string
  target_name?: string
  target_repo?: string
}

export type SpawnTargetListProps = SpawnTargetListPropsV1 & {
  ctx: RenderContext
}

const DEFAULT_SELECT_ACTION_ID = 'botster.spawn_target.select'
const DEFAULT_REMOVE_ACTION_ID = 'botster.spawn_target.remove'

export function SpawnTargetList({
  onSelect,
  onRemove,
  ctx,
}: SpawnTargetListProps): ReactElement {
  const targetOrder = useSpawnTargetStore((state) => state.order)
  const targetsById = useSpawnTargetStore((state) => state.byId)
  const targets = useMemo(
    () =>
      targetOrder.map((id) => [
        id,
        targetsById[id] as SpawnTargetRecord,
      ] as const),
    [targetOrder, targetsById],
  )
  if (targets.length === 0) {
    return <div className="text-sm text-zinc-500">No spawn targets</div>
  }
  return (
    <ul className="flex flex-col gap-0.5">
      {targets.map(([id, target]) => {
        const tid = id as string
        const t = target as SpawnTargetRecord
        const handleSelect = makeRowAction(
          onSelect,
          DEFAULT_SELECT_ACTION_ID,
          tid,
          ctx,
        )
        const handleRemove = makeRowAction(
          onRemove,
          DEFAULT_REMOVE_ACTION_ID,
          tid,
          ctx,
        )
        return (
          <li
            key={tid}
            data-target-id={tid}
            className="flex items-center justify-between gap-2 rounded-md px-2 py-1.5 text-sm hover:bg-zinc-800/50"
          >
            <button
              type="button"
              onClick={handleSelect}
              className="min-w-0 flex-1 cursor-pointer truncate text-left text-zinc-200"
            >
              <span className="font-medium">{t.target_name || tid}</span>
              {t.target_repo && (
                <span className="ml-2 text-xs text-zinc-500">{t.target_repo}</span>
              )}
            </button>
            <button
              type="button"
              onClick={handleRemove}
              aria-label="Remove spawn target"
              className="rounded text-zinc-500 hover:text-red-400"
            >
              ×
            </button>
          </li>
        )
      })}
    </ul>
  )
}

function makeRowAction(
  template: UiActionV1 | undefined,
  defaultId: string,
  targetId: string,
  ctx: RenderContext,
): (event: MouseEvent) => void {
  return (event: MouseEvent) => {
    event.preventDefault()
    event.stopPropagation()
    const action: UiActionV1 = {
      id: template?.id ?? defaultId,
      payload: { ...(template?.payload ?? {}), targetId },
    }
    ctx.dispatch(action, { element: event.currentTarget as Element })
  }
}
