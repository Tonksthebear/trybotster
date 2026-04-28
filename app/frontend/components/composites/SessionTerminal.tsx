import React, { type ReactElement } from 'react'

import type {
  SessionTerminalProps as UiSessionTerminalProps,
} from '../../ui_contract/types'
import type { RenderContext } from '../../ui_contract/context'
// @ts-expect-error TerminalView is a JSX runtime component without TS declarations yet.
import TerminalView from '../terminal/TerminalView'

export type SessionTerminalProps = UiSessionTerminalProps & {
  ctx: RenderContext
}

export function SessionTerminal({
  sessionUuid,
  ctx,
}: SessionTerminalProps): ReactElement {
  return (
    <div className="h-full min-h-[70vh] overflow-hidden rounded-md border border-zinc-800 bg-zinc-950">
      <TerminalView hubId={ctx.hubId} sessionUuid={sessionUuid} />
    </div>
  )
}
