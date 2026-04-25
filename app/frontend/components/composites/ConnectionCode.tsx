// Wire protocol — composite renderer for `ui.connection_code{}`.
// Reads the singleton `connection_code` entity and renders the pairing
// QR + URL.

import React, { type ReactElement } from 'react'

import { useConnectionCodeStore } from '../../store/entities'
import type {
  ConnectionCodePropsV1,
} from '../../ui_contract/types'
import type { RenderContext } from '../../ui_contract/context'

type ConnectionCodeRecord = {
  url?: string
  qr_ascii?: string
  [key: string]: unknown
}

export type ConnectionCodeProps = ConnectionCodePropsV1 & {
  ctx: RenderContext
}

export function ConnectionCode(_props: ConnectionCodeProps): ReactElement {
  const code = useConnectionCodeStore((state) => {
    const ids = state.order
    return ids.length === 0 ? undefined : (state.byId[ids[0]] as ConnectionCodeRecord)
  })
  if (!code) {
    return <div className="text-sm text-zinc-500">Connection code unavailable</div>
  }
  return (
    <div className="flex flex-col gap-2 text-sm">
      {code.qr_ascii && (
        <pre className="overflow-auto rounded-md bg-zinc-900/80 p-2 font-mono text-[10px] leading-tight text-zinc-100">
          {code.qr_ascii}
        </pre>
      )}
      {code.url && (
        <a
          href={code.url}
          rel="noreferrer"
          target="_blank"
          className="break-all text-sky-400 hover:underline"
        >
          {code.url}
        </a>
      )}
    </div>
  )
}
