import { afterEach, describe, expect, it, vi } from 'vitest'

import { HubTransport } from '../lib/connections/hub_connection'
import { HUB_GATE_ERROR_CODES } from '../lib/connections/hub_gate_error'

function transport(sendCommand = () => Promise.resolve(false)) {
  const hub = new HubTransport('hub-1', { hubId: 'hub-1' }, {
    hasActiveConnectionForHub: () => true,
  })
  hub.sendCommand = vi.fn(sendCommand)
  return hub
}

describe('HubTransport request send rejection', () => {
  afterEach(() => {
    vi.useRealTimers()
    vi.restoreAllMocks()
  })

  it('rejects fsRequest with send_rejected and cleans up the response timer', async () => {
    vi.useFakeTimers()
    vi.spyOn(crypto, 'randomUUID').mockReturnValue('fs-req-1')
    const hub = transport()

    await expect(hub.fsRequest('fs:read', { path: 'README.md' }, 50))
      .rejects.toMatchObject({ code: HUB_GATE_ERROR_CODES.SEND_REJECTED })

    expect(hub.subscribers.get('fs:response:fs-req-1')?.size || 0).toBe(0)
    expect(vi.getTimerCount()).toBe(0)

    await vi.advanceTimersByTimeAsync(50)
    expect(vi.getTimerCount()).toBe(0)
  })

  it('rejects templateRequest with send_rejected and cleans up the response timer', async () => {
    vi.useFakeTimers()
    vi.spyOn(crypto, 'randomUUID').mockReturnValue('template-req-1')
    const hub = transport()

    await expect(hub.templateRequest('template:install', { dest: 'x' }, 50))
      .rejects.toMatchObject({ code: HUB_GATE_ERROR_CODES.SEND_REJECTED })

    expect(hub.subscribers.get('template:response:template-req-1')?.size || 0).toBe(0)
    expect(vi.getTimerCount()).toBe(0)

    await vi.advanceTimersByTimeAsync(50)
    expect(vi.getTimerCount()).toBe(0)
  })

  it('rejects fsRequest with send_rejected when sendCommand resolves false after response timeout', async () => {
    vi.useFakeTimers()
    vi.spyOn(crypto, 'randomUUID').mockReturnValue('fs-late-req-1')
    const hub = transport(() => new Promise((resolve) => {
      setTimeout(() => resolve(false), 100)
    }))

    const result = hub.fsRequest('fs:read', { path: 'README.md' }, 25)
    const expectation = expect(result)
      .rejects.toMatchObject({ code: HUB_GATE_ERROR_CODES.SEND_REJECTED })

    await vi.advanceTimersByTimeAsync(25)
    expect(hub.subscribers.get('fs:response:fs-late-req-1')?.size || 0).toBe(0)
    await vi.advanceTimersByTimeAsync(75)

    await expectation
    expect(hub.subscribers.get('fs:response:fs-late-req-1')?.size || 0).toBe(0)
    expect(vi.getTimerCount()).toBe(0)
  })

  it('rejects templateRequest with send_rejected when sendCommand resolves false after response timeout', async () => {
    vi.useFakeTimers()
    vi.spyOn(crypto, 'randomUUID').mockReturnValue('template-late-req-1')
    const hub = transport(() => new Promise((resolve) => {
      setTimeout(() => resolve(false), 100)
    }))

    const result = hub.templateRequest('template:install', { dest: 'x' }, 25)
    const expectation = expect(result)
      .rejects.toMatchObject({ code: HUB_GATE_ERROR_CODES.SEND_REJECTED })

    await vi.advanceTimersByTimeAsync(25)
    expect(hub.subscribers.get('template:response:template-late-req-1')?.size || 0).toBe(0)
    await vi.advanceTimersByTimeAsync(75)

    await expectation
    expect(hub.subscribers.get('template:response:template-late-req-1')?.size || 0).toBe(0)
    expect(vi.getTimerCount()).toBe(0)
  })
})
