// Wire protocol — composite renderer for `ui.hub_recovery_state{}`.
// Reads the singleton `hub` entity (id = hub_id) and renders the lifecycle
// banner that v1 used to ship as inline panels in the layout tree.

import React, { type ReactElement } from 'react'

import { useHubMetaStore } from '../../store/entities'
import type {
  HubRecoveryStatePropsV1,
} from '../../ui_contract/types'
import type { RenderContext } from '../../ui_contract/context'

type HubRecord = { state?: string; reason?: string; [key: string]: unknown }

export type HubRecoveryStateProps = HubRecoveryStatePropsV1 & {
  ctx: RenderContext
}

export function HubRecoveryState(_props: HubRecoveryStateProps): ReactElement {
  const hub = useHubMetaStore((state) => {
    const ids = state.order
    return ids.length === 0 ? undefined : (state.byId[ids[0]] as HubRecord)
  })
  if (!hub || hub.state === 'ready') return <></>
  return (
    <aside
      role="status"
      data-hub-state={hub.state ?? 'starting'}
      className="flex items-center gap-2 rounded-md bg-amber-500/10 px-3 py-2 text-sm text-amber-300"
    >
      <span aria-hidden="true">⏳</span>
      <span className="font-medium">Hub: {hub.state ?? 'starting'}</span>
      {hub.reason && <span className="text-xs text-amber-200">{hub.reason}</span>}
    </aside>
  )
}
