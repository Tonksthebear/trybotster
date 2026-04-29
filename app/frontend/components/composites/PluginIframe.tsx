import React, {
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactElement,
} from 'react'

import type { RenderContext } from '../../ui_contract/context'
import type { IframeBridgeProps } from '../../ui_contract/types'

export type PluginIframeProps = {
  src: string
  title?: string
  sandbox?: string
  bridge?: IframeBridgeProps
  ctx: RenderContext
}

const PLUGIN_ASSET_SCHEME = 'botster-plugin-asset://'
const DEFAULT_SANDBOX = 'allow-scripts'

function parsePluginAssetId(src: string): string | null {
  if (!src.startsWith(PLUGIN_ASSET_SCHEME)) return null
  const rest = src.slice(PLUGIN_ASSET_SCHEME.length)
  const assetId = rest.split(/[?#]/, 1)[0] ?? ''
  return assetId.length > 0 ? assetId : null
}

function safeDirectSrc(src: string): string | null {
  if (/^https?:\/\//i.test(src)) return src
  if (src.startsWith(PLUGIN_ASSET_SCHEME)) return null
  return null
}

export function PluginIframe({
  src,
  title,
  sandbox,
  bridge,
  ctx,
}: PluginIframeProps): ReactElement {
  const iframeRef = useRef<HTMLIFrameElement | null>(null)
  const [resolvedSrc, setResolvedSrc] = useState<string | null>(() => safeDirectSrc(src))
  const [error, setError] = useState<string | null>(null)
  const assetId = useMemo(() => parsePluginAssetId(src), [src])
  const allowedActions = useMemo(
    () => new Set((bridge?.actions ?? []).filter((action) => typeof action === 'string')),
    [bridge?.actions],
  )

  useEffect(() => {
    if (!assetId) {
      setResolvedSrc(safeDirectSrc(src))
      setError(safeDirectSrc(src) ? null : 'Unsupported iframe source')
      return undefined
    }
    if (!ctx.transport || !ctx.transport.on) {
      setResolvedSrc(null)
      setError('Hub connection is not ready')
      return undefined
    }

    let cancelled = false
    let objectUrl: string | null = null
    const requestId = crypto.randomUUID()
    setResolvedSrc(null)
    setError(null)

    const unsubscribe = ctx.transport.on?.('message', (message: any) => {
      if (
        !message ||
        message.type !== 'plugin_asset:response' ||
        message.request_id !== requestId
      ) {
        return
      }
      if (cancelled) return
      if (message.ok !== true) {
        setError(message.error || 'Unable to load plugin asset')
        return
      }
      const content = typeof message.content === 'string' ? message.content : ''
      const contentType =
        typeof message.content_type === 'string' && message.content_type.length > 0
          ? message.content_type
          : 'application/octet-stream'
      objectUrl = URL.createObjectURL(new Blob([content], { type: contentType }))
      setResolvedSrc(objectUrl)
    })

    void ctx.transport.send('plugin_asset:read', {
      request_id: requestId,
      asset_id: assetId,
    }).catch((err: unknown) => {
      if (!cancelled) setError(err instanceof Error ? err.message : String(err))
    })

    return () => {
      cancelled = true
      unsubscribe?.()
      if (objectUrl) URL.revokeObjectURL(objectUrl)
    }
  }, [assetId, ctx.transport, src])

  useEffect(() => {
    if (allowedActions.size === 0) return undefined

    const handler = (event: MessageEvent) => {
      if (event.source !== iframeRef.current?.contentWindow) return
      const data = event.data
      if (!data || typeof data !== 'object') return
      if (data.type !== 'botster.plugin_action') return
      const action = typeof data.action === 'string' ? data.action : ''
      if (!allowedActions.has(action)) return

      ctx.dispatch({
        id: 'botster.plugin_asset.message',
        payload: {
          assetId,
          action,
          payload: data.payload ?? {},
        },
      })
    }

    window.addEventListener('message', handler)
    return () => window.removeEventListener('message', handler)
  }, [allowedActions, assetId, ctx.dispatch])

  if (error) {
    return (
      <div className="rounded-md border border-red-500/30 bg-red-500/10 p-3 text-sm text-red-300">
        {error}
      </div>
    )
  }

  if (!resolvedSrc) {
    return (
      <div className="flex h-full min-h-40 items-center justify-center text-sm text-zinc-500">
        Loading…
      </div>
    )
  }

  return (
    <iframe
      ref={iframeRef}
      src={resolvedSrc}
      title={title || 'Plugin view'}
      sandbox={sandbox || DEFAULT_SANDBOX}
      className="h-full min-h-0 w-full border-0 bg-white"
      data-testid="plugin-iframe"
    />
  )
}
