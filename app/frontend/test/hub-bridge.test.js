import { afterEach, describe, expect, it, vi } from 'vitest'

const mocks = vi.hoisted(() => ({
  acquire: vi.fn(),
}))

vi.mock('connections', () => ({
  HubManager: {
    acquire: (...args) => mocks.acquire(...args),
  },
}))

function fakeHub() {
  return {
    release: vi.fn(),
    transport: {
      uiRouteRegistry: vi.fn(() => []),
      on: vi.fn(),
    },
  }
}

describe('hub bridge waitForHub', () => {
  afterEach(() => {
    vi.useRealTimers()
    vi.clearAllMocks()
    vi.resetModules()
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

  it('settles durable waiters when the route-owned connection fails', async () => {
    const { connect, waitForHub } = await import('../lib/hub-bridge')
    const error = new Error('auth refused')
    mocks.acquire.mockRejectedValueOnce(error)

    const waiting = waitForHub('bad-hub', null)
    const connecting = connect('bad-hub')

    await expect(connecting).rejects.toThrow('auth refused')
    await expect(waiting).resolves.toBe(null)
  })

  it('matches waiters and connections with equivalent numeric and string hub IDs', async () => {
    const { connect, waitForHub } = await import('../lib/hub-bridge')
    const hub = fakeHub()
    mocks.acquire.mockResolvedValueOnce(hub)

    const waiting = waitForHub('42', null)
    await connect(42)

    await expect(waiting).resolves.toBe(hub)
  })
})
