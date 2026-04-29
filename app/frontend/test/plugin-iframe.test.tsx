import React from 'react'
import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'

import { PluginIframe } from '../components/composites/PluginIframe'

function fakeCtx(overrides: Record<string, unknown> = {}) {
  return {
    hubId: 'hub-1',
    viewport: { widthClass: 'regular', heightClass: 'regular', pointer: 'fine' },
    capabilities: {
      hover: true,
      dialog: true,
      tooltip: true,
      externalLinks: true,
      binaryTerminalSnapshots: true,
    },
    dispatch: vi.fn(),
    ...overrides,
  } as any
}

function fakeTransport() {
  const handlers = new Set<(message: unknown) => void>()
  return {
    send: vi.fn(async (_type: string, data: Record<string, unknown>) => {
      queueMicrotask(() => {
        for (const handler of handlers) {
          handler({
            type: 'plugin_asset:response',
            request_id: data.request_id,
            ok: true,
            content: '<html><body>Graph</body></html>',
            content_type: 'text/html',
          })
        }
      })
      return true
    }),
    on: vi.fn((_event: string, handler: (message: unknown) => void) => {
      handlers.add(handler)
      return () => handlers.delete(handler)
    }),
  }
}

describe('<PluginIframe>', () => {
  beforeEach(() => {
    vi.stubGlobal('URL', {
      createObjectURL: vi.fn(() => 'blob:plugin-asset'),
      revokeObjectURL: vi.fn(),
    })
    vi.stubGlobal('crypto', {
      randomUUID: vi.fn(() => 'req-1'),
    })
  })

  afterEach(() => {
    cleanup()
    vi.unstubAllGlobals()
  })

  it('loads botster plugin assets through the hub transport and renders a blob iframe', async () => {
    const transport = fakeTransport()
    render(
      <PluginIframe
        src="botster-plugin-asset://vault:graph?v=1"
        title="Graph"
        ctx={fakeCtx({ transport })}
      />,
    )

    expect(transport.send).toHaveBeenCalledWith('plugin_asset:read', {
      request_id: 'req-1',
      asset_id: 'vault:graph',
    })
    const iframe = await screen.findByTestId('plugin-iframe')
    expect(iframe).toHaveAttribute('src', 'blob:plugin-asset')
    expect(iframe).toHaveAttribute('sandbox', 'allow-scripts')
  })

  it('dispatches declared iframe bridge actions to the hub', async () => {
    const dispatch = vi.fn()
    const transport = fakeTransport()
    render(
      <PluginIframe
        src="botster-plugin-asset://vault:board?v=1"
        bridge={{ actions: ['card.move'] }}
        ctx={fakeCtx({ transport, dispatch })}
      />,
    )

    const iframe = await screen.findByTestId('plugin-iframe') as HTMLIFrameElement
    const iframeWindow = { postMessage: vi.fn() }
    Object.defineProperty(iframe, 'contentWindow', {
      value: iframeWindow,
      configurable: true,
    })

    const event = new MessageEvent('message', {
      data: {
        type: 'botster.plugin_action',
        action: 'card.move',
        payload: { card_id: 'c1', to_column: 'done' },
      },
    })
    Object.defineProperty(event, 'source', {
      value: iframeWindow,
      configurable: true,
    })
    fireEvent(window, event)

    await waitFor(() => {
      expect(dispatch).toHaveBeenCalledWith({
        id: 'botster.plugin_asset.message',
        payload: {
          assetId: 'vault:board',
          action: 'card.move',
          payload: { card_id: 'c1', to_column: 'done' },
        },
      })
    })
  })
})
