// Wire protocol — composite renderer for `ui.connection_code{}`.
// Reads the singleton `connection_code` entity (cli/lua/hub/init.lua) and
// renders the in-app pairing card so users can add a second device without
// dropping back to the CLI's stdout / TUI QR pane.

import React, { useState, type ReactElement } from 'react'

import { Subheading } from '../catalyst/heading'
import { Text, TextLink } from '../catalyst/text'
import { Button } from '../catalyst/button'
import { IconGlyph } from '../../ui_contract/icons'
import { useConnectionCodeStore } from '../../store/entities'
import type {
  ConnectionCodePropsV1,
} from '../../ui_contract/types'
import type { RenderContext } from '../../ui_contract/context'

type ConnectionCodeRecord = {
  url?: string
  qr_ascii?: string
  error?: string
  [key: string]: unknown
}

export type ConnectionCodeProps = ConnectionCodePropsV1 & {
  ctx: RenderContext
}

function CardShell({ children }: { children: React.ReactNode }): ReactElement {
  return (
    <section className="rounded-lg border border-zinc-200 bg-white p-4 lg:p-5 dark:border-zinc-800 dark:bg-zinc-900/50">
      <div className="mb-3 flex items-center gap-2">
        <Subheading className="text-zinc-950 dark:text-white">Pair a new device</Subheading>
      </div>
      {children}
    </section>
  )
}

export function ConnectionCode(_props: ConnectionCodeProps): ReactElement {
  const code = useConnectionCodeStore((state) => {
    const ids = state.order
    return ids.length === 0 ? undefined : (state.byId[ids[0]] as ConnectionCodeRecord)
  })
  const [copyState, setCopyState] = useState<'idle' | 'copied' | 'error'>('idle')

  if (!code) {
    return (
      <CardShell>
        <Text>Generating QR code…</Text>
      </CardShell>
    )
  }

  if (code.error) {
    return (
      <CardShell>
        <div className="flex items-start gap-2 text-amber-600 dark:text-amber-400">
          <IconGlyph name="exclamation-triangle" className="mt-0.5 size-4 shrink-0" />
          <Text className="text-amber-700 dark:text-amber-300">{String(code.error)}</Text>
        </div>
      </CardShell>
    )
  }

  async function handleCopy() {
    if (!code?.url) return
    try {
      await navigator.clipboard.writeText(code.url)
      setCopyState('copied')
      setTimeout(() => setCopyState('idle'), 1500)
    } catch {
      setCopyState('error')
      setTimeout(() => setCopyState('idle'), 2000)
    }
  }

  return (
    <CardShell>
      <div className="flex flex-col gap-3">
        {code.qr_ascii && (
          <pre className="overflow-auto rounded-md bg-zinc-100 p-2 font-mono text-[10px] leading-tight text-zinc-900 dark:bg-zinc-950 dark:text-zinc-100">
            {code.qr_ascii}
          </pre>
        )}
        {code.url && (
          <div className="flex flex-col gap-2 sm:flex-row sm:items-center">
            <TextLink
              href={code.url}
              rel="noreferrer"
              target="_blank"
              className="break-all text-sm"
            >
              {code.url}
            </TextLink>
            <Button outline onClick={handleCopy} aria-label="Copy pairing URL">
              {copyState === 'copied' ? 'Copied' : copyState === 'error' ? 'Failed' : 'Copy'}
            </Button>
          </div>
        )}
        <Text className="text-xs">
          Scan or share this link to pair another device. Anyone with the link can connect.
        </Text>
      </div>
    </CardShell>
  )
}
