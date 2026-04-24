// Wire protocol v2 — composite renderer for `ui.session_row{ session_uuid }`.
// Single-row variant of SessionList; reads one session from the store.

import React, { type ReactElement } from 'react'
import clsx from 'clsx'

import { useSessionStore } from '../../store/entities'
import type {
  SessionRowPropsV1,
  UiSurfaceDensityV1,
  UiValueV1,
} from '../../ui_contract/types'
import type { RenderContext } from '../../ui_contract/context'
import { resolveValue } from '../../ui_contract/viewport'

type SessionRecord = {
  session_uuid?: string
  title?: string
  display_name?: string
  session_type?: string
}

export type SessionRowProps = SessionRowPropsV1 & { ctx: RenderContext }

export function SessionRow({
  sessionUuid,
  density,
  ctx,
}: SessionRowProps): ReactElement {
  const session = useSessionStore(
    (state) => state.byId[sessionUuid] as SessionRecord | undefined,
  )
  const resolvedDensity =
    resolveValue<UiSurfaceDensityV1>(
      density as UiValueV1<UiSurfaceDensityV1> | undefined,
      ctx.viewport,
    ) ?? 'panel'
  if (!session) {
    return (
      <div className="text-sm text-zinc-500" data-session-uuid={sessionUuid}>
        (session unavailable)
      </div>
    )
  }
  const title = session.title || session.display_name || sessionUuid
  return (
    <div
      className={clsx(
        'flex items-center gap-2 px-2 text-zinc-200',
        resolvedDensity === 'sidebar' ? 'py-0.5 text-xs' : 'py-1.5 text-sm',
      )}
      data-session-uuid={sessionUuid}
    >
      <span className="min-w-0 truncate">{title}</span>
    </div>
  )
}
