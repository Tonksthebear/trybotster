import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

const mocks = vi.hoisted(() => ({
  acquire: vi.fn(),
}))

vi.mock('connections', () => ({
  HubManager: {
    acquire: (...args) => mocks.acquire(...args),
  },
}))

function fakeHub() {
  const hub = {
    release: vi.fn(),
    isConnected: vi.fn(() => true),
    perform: vi.fn((operation, options) => operation(hub, options)),
    transport: {
      uiRouteRegistry: vi.fn(() => []),
      on: vi.fn(),
    },
  }
  return hub
}

describe('hub bridge waitForHub', () => {
  beforeEach(() => {
    vi.resetModules()
  })

  afterEach(() => {
    vi.useRealTimers()
    vi.clearAllMocks()
  })

  it('can wait durably for the route-owned hub connection', async () => {
    vi.useFakeTimers()
    const { connect, waitForHub } = await import('../lib/hub-bridge')
    const hub = fakeHub()
    mocks.acquire.mockResolvedValueOnce(hub)

    let settled = false
    const waiting = waitForHub('durable-hub', null).then((value) => {
      settled = true
      return value
    })

    await vi.advanceTimersByTimeAsync(60_000)
    expect(settled).toBe(false)

    await connect('durable-hub')

    await expect(waiting).resolves.toBe(hub)
  })

  it('still supports bounded optional waits', async () => {
    vi.useFakeTimers()
    const { waitForHub } = await import('../lib/hub-bridge')

    const waiting = waitForHub('missing-hub', 25)
    await vi.advanceTimersByTimeAsync(25)

    await expect(waiting).resolves.toBe(null)
  })
})

describe('withHub', () => {
  beforeEach(() => {
    vi.resetModules()
  })

  afterEach(() => {
    vi.useRealTimers()
    vi.clearAllMocks()
  })

  it('runs an operation after waiting for the route-owned hub', async () => {
    const { connect, withHub } = await import('../lib/hub-bridge')
    const hub = fakeHub()
    hub.perform = vi.fn((operation, options) => operation(hub, options))
    mocks.acquire.mockResolvedValueOnce(hub)

    const waiting = withHub('gate-hub', (resolved) => resolved.hubId)
    hub.hubId = 'gate-hub'

    await connect('gate-hub')

    await expect(waiting).resolves.toBe('gate-hub')
    expect(hub.perform).toHaveBeenCalledWith(expect.any(Function), {})
  })

  it('propagates operation results and errors through HubSession.perform', async () => {
    const { connect, withHub } = await import('../lib/hub-bridge')
    const hub = fakeHub()
    hub.perform = vi
      .fn()
      .mockImplementationOnce((operation) => operation(hub))
      .mockImplementationOnce((operation) => operation(hub))
    mocks.acquire.mockResolvedValueOnce(hub)
    await connect('result-hub')

    await expect(withHub('result-hub', () => 'ok')).resolves.toBe('ok')

    const error = new Error('boom')
    await expect(withHub('result-hub', () => {
      throw error
    })).rejects.toBe(error)
  })

  it('rejects an already-aborted signal immediately', async () => {
    const { withHub } = await import('../lib/hub-bridge')
    const { HUB_GATE_ERROR_CODES } = await import('../lib/connections/hub_gate_error')
    const controller = new AbortController()
    controller.abort()

    await expect(withHub('aborted-hub', () => {}, { signal: controller.signal }))
      .rejects.toMatchObject({ code: HUB_GATE_ERROR_CODES.ABORTED })
    expect(mocks.acquire).not.toHaveBeenCalled()
  })

  it('rejects and cleans up when aborted while waiting', async () => {
    const { connect, withHub } = await import('../lib/hub-bridge')
    const { HUB_GATE_ERROR_CODES } = await import('../lib/connections/hub_gate_error')
    const controller = new AbortController()
    const operation = vi.fn()

    const waiting = withHub('abort-wait-hub', operation, { signal: controller.signal })
    controller.abort()

    await expect(waiting).rejects.toMatchObject({ code: HUB_GATE_ERROR_CODES.ABORTED })

    const hub = fakeHub()
    mocks.acquire.mockResolvedValueOnce(hub)
    await connect('abort-wait-hub')
    expect(operation).not.toHaveBeenCalled()
  })

  it('rejects when the bridge disconnects while waiting', async () => {
    const { connect, disconnect, withHub } = await import('../lib/hub-bridge')
    const { HUB_GATE_ERROR_CODES } = await import('../lib/connections/hub_gate_error')
    const hub = fakeHub()
    hub.isConnected.mockReturnValue(false)
    hub.perform = vi.fn(() => new Promise(() => {}))
    mocks.acquire.mockResolvedValueOnce(hub)
    const { connectionId } = await connect('bridge-disconnect-hub')

    const waiting = withHub('bridge-disconnect-hub', () => {})
    await disconnect(connectionId)

    await expect(waiting).rejects.toMatchObject({
      code: HUB_GATE_ERROR_CODES.UNAVAILABLE,
    })
  })

  it('aborts the underlying perform wait after bridge disconnect', async () => {
    const { connect, disconnect, withHub } = await import('../lib/hub-bridge')
    const { HUB_GATE_ERROR_CODES } = await import('../lib/connections/hub_gate_error')
    const hub = fakeHub()
    const operation = vi.fn(() => 'late result')
    hub.isConnected.mockReturnValue(false)
    hub.perform = vi.fn((performOperation, options) => new Promise((resolve, reject) => {
      options.signal.addEventListener('abort', () => {
        reject(new Error('aborted by bridge'))
      }, { once: true })
      hub.finishOperation = () => {
        if (!options.signal.aborted) resolve(performOperation(hub))
      }
    }))
    mocks.acquire.mockResolvedValueOnce(hub)
    const { connectionId } = await connect('bridge-abort-hub')

    const waiting = withHub('bridge-abort-hub', operation)
    const expectation = expect(waiting).rejects.toMatchObject({
      code: HUB_GATE_ERROR_CODES.UNAVAILABLE,
    })
    await disconnect(connectionId)
    await expectation

    hub.finishOperation()
    await Promise.resolve()
    expect(operation).not.toHaveBeenCalled()
  })

  it('rejects when waiting for an unavailable hub times out', async () => {
    vi.useFakeTimers()
    const { withHub } = await import('../lib/hub-bridge')
    const { HUB_GATE_ERROR_CODES } = await import('../lib/connections/hub_gate_error')

    const waiting = withHub('missing-gate-hub', () => {}, { timeoutMs: 25 })
    const expectation = expect(waiting)
      .rejects.toMatchObject({ code: HUB_GATE_ERROR_CODES.TIMEOUT })
    await vi.advanceTimersByTimeAsync(25)

    await expectation
  })
})
