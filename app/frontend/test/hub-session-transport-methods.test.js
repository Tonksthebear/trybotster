import { describe, expect, it, vi } from 'vitest'

import { HubSession } from '../lib/connections/hub'

describe('HubSession transport forwarding', () => {
  it('forwards template refresh requests to the transport', async () => {
    const hub = new HubSession('hub-1')
    hub.transport = {
      refreshTemplates: vi.fn(() => Promise.resolve({ ok: true })),
    }

    await hub.refreshTemplates()

    expect(hub.transport.refreshTemplates).toHaveBeenCalled()
  })

  it('exposes sendCommand as the client command path', async () => {
    const hub = new HubSession('hub-1')
    hub.transport = {
      sendCommand: vi.fn(() => Promise.resolve(true)),
    }

    await hub.sendCommand('template:refresh', { request_id: 'req-1' })

    expect(hub.transport.sendCommand).toHaveBeenCalledWith(
      'template:refresh',
      { request_id: 'req-1' },
    )
    expect(hub.send).toBeUndefined()
  })
})
