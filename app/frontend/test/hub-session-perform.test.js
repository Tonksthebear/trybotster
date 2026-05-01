import { afterEach, describe, expect, it, vi } from 'vitest'

import { HubSession } from '../lib/connections/hub'
import { HUB_GATE_ERROR_CODES } from '../lib/connections/hub_gate_error'

function hubWithTransport(connected = false) {
  const hub = new HubSession('hub-1')
  let isConnected = connected
  hub.transport = {
    isConnected: vi.fn(() => isConnected),
    release: vi.fn(),
    setConnected(value) {
      isConnected = value
    },
  }
  return hub
}

describe('HubSession.perform', () => {
  afterEach(() => {
    vi.useRealTimers()
    vi.restoreAllMocks()
  })

  it('waits through transient disconnects and runs once connected', async () => {
    const hub = hubWithTransport(false)
    const operation = vi.fn(() => 'ready')
    let settled = false

    const result = hub.perform(operation).then((value) => {
      settled = true
      return value
    })

    hub.emit('disconnected', hub)
    await Promise.resolve()
    expect(settled).toBe(false)
    expect(operation).not.toHaveBeenCalled()

    hub.transport.setConnected(true)
    hub.emit('connected', hub)

    await expect(result).resolves.toBe('ready')
    expect(operation).toHaveBeenCalledWith(hub)
  })

  it('uses the already-connected fast path without waiters or timers', async () => {
    vi.useFakeTimers()
    const timeoutSpy = vi.spyOn(window, 'setTimeout')
    const hub = hubWithTransport(true)

    await expect(hub.perform(() => 'fast')).resolves.toBe('fast')

    expect(timeoutSpy).not.toHaveBeenCalled()
    expect(hub.subscribers.size).toBe(0)
  })

  it('rejects an already-aborted signal immediately', async () => {
    const hub = hubWithTransport(false)
    const controller = new AbortController()
    controller.abort()

    await expect(hub.perform(() => {}, { signal: controller.signal }))
      .rejects.toMatchObject({ code: HUB_GATE_ERROR_CODES.ABORTED })
    expect(hub.subscribers.size).toBe(0)
  })

  it('rejects and cleans up when aborted while waiting', async () => {
    const hub = hubWithTransport(false)
    const controller = new AbortController()
    const operation = vi.fn()

    const result = hub.perform(operation, { signal: controller.signal })
    controller.abort()

    await expect(result).rejects.toMatchObject({ code: HUB_GATE_ERROR_CODES.ABORTED })
    expect(hub.subscribers.get('connected')?.size || 0).toBe(0)
    expect(operation).not.toHaveBeenCalled()

    hub.transport.setConnected(true)
    hub.emit('connected', hub)
    expect(operation).not.toHaveBeenCalled()
  })

  it('propagates operation results and errors', async () => {
    const hub = hubWithTransport(true)
    await expect(hub.perform(() => 42)).resolves.toBe(42)

    const error = new Error('operation failed')
    await expect(hub.perform(() => {
      throw error
    })).rejects.toBe(error)
  })

  it('rejects when destroyed before call', async () => {
    const hub = hubWithTransport(true)
    hub.destroy()

    await expect(hub.perform(() => {}))
      .rejects.toMatchObject({ code: HUB_GATE_ERROR_CODES.DESTROYED })
  })

  it('rejects when destroyed while waiting', async () => {
    const hub = hubWithTransport(false)
    const operation = vi.fn()
    const result = hub.perform(operation)
    const expectation = expect(result)
      .rejects.toMatchObject({ code: HUB_GATE_ERROR_CODES.DESTROYED })

    hub.destroy()

    await expectation
    expect(operation).not.toHaveBeenCalled()
  })

  it('rejects when transport is required but missing', async () => {
    const hub = new HubSession('hub-1')

    await expect(hub.perform(() => {}))
      .rejects.toMatchObject({ code: HUB_GATE_ERROR_CODES.MISSING_TRANSPORT })
  })

  it('allows object-only operations without transport when explicitly requested', async () => {
    const hub = new HubSession('hub-1')

    await expect(hub.perform(
      (resolved) => resolved.hubId,
      { requireTransport: false },
    )).resolves.toBe('hub-1')
  })

  it('rejects and cleans up when the operation times out', async () => {
    vi.useFakeTimers()
    const hub = hubWithTransport(true)
    const result = hub.perform(() => new Promise(() => {}), { timeoutMs: 25 })
    const expectation = expect(result)
      .rejects.toMatchObject({ code: HUB_GATE_ERROR_CODES.TIMEOUT })

    await vi.advanceTimersByTimeAsync(25)

    await expectation
    expect(vi.getTimerCount()).toBe(0)
    expect(hub.subscribers.size).toBe(0)
  })

  it('rejects and cleans up when aborted during the operation', async () => {
    vi.useFakeTimers()
    const hub = hubWithTransport(true)
    const controller = new AbortController()
    const result = hub.perform(
      () => new Promise(() => {}),
      { signal: controller.signal, timeoutMs: 100 },
    )
    const expectation = expect(result)
      .rejects.toMatchObject({ code: HUB_GATE_ERROR_CODES.ABORTED })

    controller.abort()

    await expectation
    expect(vi.getTimerCount()).toBe(0)
    expect(hub.subscribers.size).toBe(0)
  })
})
