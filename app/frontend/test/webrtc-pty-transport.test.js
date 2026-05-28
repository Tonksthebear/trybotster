import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

const mocks = vi.hoisted(() => ({
  acquire: vi.fn(),
  get: vi.fn(),
}))

vi.mock('connections', () => ({
  HubConnectionManager: { acquire: mocks.acquire, get: mocks.get },
  TerminalConnection: {
    key: vi.fn((hubId, sessionUuid) => `terminal:${hubId}:${sessionUuid}`),
  },
}))

import { WebRtcPtyTransport } from '../lib/transport/webrtc_pty_transport'

function fakeTerminalConnection() {
  return {
    isConnected: vi.fn(() => true),
    hasSubscription: vi.fn(() => false),
    sendResize: vi.fn(() => Promise.resolve(true)),
    requestSnapshot: vi.fn(),
    onSnapshotStart: vi.fn(() => vi.fn()),
    onSnapshotComplete: vi.fn(() => vi.fn()),
    onBinarySnapshot: vi.fn(() => vi.fn()),
    onModeChanged: vi.fn(() => vi.fn()),
    on: vi.fn(() => vi.fn()),
    onOutput: vi.fn(() => vi.fn()),
    onConnected: vi.fn((callback) => {
      callback()
      return vi.fn()
    }),
    onDisconnected: vi.fn(() => vi.fn()),
    onError: vi.fn(() => vi.fn()),
    release: vi.fn(),
  }
}

describe('WebRtcPtyTransport', () => {
  afterEach(() => {
    vi.useRealTimers()
  })

  beforeEach(() => {
    mocks.acquire.mockReset()
    mocks.get.mockReset()
    mocks.get.mockReturnValue(null)
    mocks.acquire.mockResolvedValue(fakeTerminalConnection())
  })

  it('uses the latest pre-connect resize as the terminal subscribe size', async () => {
    const transport = new WebRtcPtyTransport({
      hubId: 'hub-1',
      sessionUuid: 'session-1',
    })

    expect(transport.resize(132, 37)).toBe(true)

    await transport.connect({
      rows: 24,
      cols: 80,
      callbacks: {},
    })

    expect(mocks.acquire).toHaveBeenCalledWith(
      expect.objectContaining({ key: expect.any(Function) }),
      'terminal:hub-1:session-1',
      {
        hubId: 'hub-1',
        sessionUuid: 'session-1',
        rows: 37,
        cols: 132,
      },
    )
    const conn = await mocks.acquire.mock.results[0].value
    expect(conn.sendResize).toHaveBeenCalledWith(132, 37)
  })

  it('resizes an existing subscription before requesting a reconnect snapshot', async () => {
    const existing = fakeTerminalConnection()
    existing.hasSubscription.mockReturnValue(true)
    mocks.get.mockReturnValue(existing)
    mocks.acquire.mockResolvedValue(existing)
    const transport = new WebRtcPtyTransport({
      hubId: 'hub-1',
      sessionUuid: 'session-1',
    })

    await transport.connect({
      rows: 42,
      cols: 120,
      callbacks: {},
    })

    expect(existing.sendResize).toHaveBeenCalledWith(120, 42)
    expect(existing.requestSnapshot).toHaveBeenCalledWith({
      cols: 120,
      rows: 42,
    })
    expect(existing.sendResize.mock.invocationCallOrder[0]).toBeLessThan(
      existing.requestSnapshot.mock.invocationCallOrder[0],
    )
  })

  it('coalesces live resizes to the latest desired size', async () => {
    vi.useFakeTimers()
    const conn = fakeTerminalConnection()
    mocks.acquire.mockResolvedValue(conn)
    const transport = new WebRtcPtyTransport({
      hubId: 'hub-1',
      sessionUuid: 'session-1',
    })

    await transport.connect({
      rows: 24,
      cols: 80,
      callbacks: {},
    })
    conn.sendResize.mockClear()

    transport.resize(100, 30)
    transport.resize(101, 31)
    transport.resize(102, 32)
    vi.advanceTimersByTime(35)

    expect(conn.sendResize).toHaveBeenCalledTimes(1)
    expect(conn.sendResize).toHaveBeenCalledWith(102, 32)
    vi.useRealTimers()
  })

  it('reuses its owned terminal connection across repeated connect calls', async () => {
    const conn = fakeTerminalConnection()
    conn.hasSubscription.mockReturnValue(true)
    mocks.acquire.mockResolvedValue(conn)
    const transport = new WebRtcPtyTransport({
      hubId: 'hub-1',
      sessionUuid: 'session-1',
    })

    await transport.connect({
      rows: 24,
      cols: 80,
      callbacks: {},
    })
    await transport.connect({
      rows: 30,
      cols: 100,
      callbacks: {},
    })

    expect(mocks.acquire).toHaveBeenCalledTimes(1)
    expect(conn.release).not.toHaveBeenCalled()
    expect(conn.sendResize).toHaveBeenCalledWith(80, 24)
    expect(conn.sendResize).toHaveBeenCalledWith(100, 30)
    expect(conn.requestSnapshot).toHaveBeenCalledWith({ cols: 100, rows: 30 })
  })

  it('does not subscribe a stale terminal when destroyed during async connect', async () => {
    let resolveAcquire
    const conn = fakeTerminalConnection()
    mocks.acquire.mockReturnValue(
      new Promise((resolve) => {
        resolveAcquire = resolve
      }),
    )
    const transport = new WebRtcPtyTransport({
      hubId: 'hub-1',
      sessionUuid: 'session-1',
    })

    const connect = transport.connect({
      rows: 24,
      cols: 80,
      callbacks: {},
    })
    transport.destroy()
    resolveAcquire(conn)
    await connect

    expect(conn.release).toHaveBeenCalledTimes(1)
    expect(conn.sendResize).not.toHaveBeenCalled()
    expect(conn.requestSnapshot).not.toHaveBeenCalled()
    expect(conn.onOutput).not.toHaveBeenCalled()
  })

  it('imports a reconnect snapshot before replaying mouse mode changes into Restty', async () => {
    const conn = fakeTerminalConnection()
    const delivered = []
    conn.onModeChanged.mockImplementation((callback) => {
      callback({
        type: 'mode_changed',
        session_uuid: 'session-1',
        mode: { mouse_mode: 12, bracketed_paste: true, focus_reporting: true },
      })
      return vi.fn()
    })
    conn.onBinarySnapshot.mockImplementation((callback) => {
      callback(new Uint8Array([1, 2, 3]))
      return vi.fn()
    })
    mocks.acquire.mockResolvedValue(conn)
    const transport = new WebRtcPtyTransport({
      hubId: 'hub-1',
      sessionUuid: 'session-1',
    })
    transport.onBinarySnapshot = () => delivered.push(['snapshot'])

    await transport.connect({
      rows: 24,
      cols: 80,
      callbacks: {
        onData: (data) => delivered.push(['data', data]),
      },
    })

    expect(delivered).toEqual([
      ['snapshot'],
      [
        'data',
        '\x1b[?1000l\x1b[?1003l\x1b[?1002h\x1b[?1006h\x1b[?2004h\x1b[?1004h',
      ],
    ])
  })
})
