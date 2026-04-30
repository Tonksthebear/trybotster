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
})
