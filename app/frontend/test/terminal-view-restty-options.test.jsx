import { act, cleanup, render, waitFor } from '@testing-library/react'
import React from 'react'
import { afterEach, describe, expect, it, vi } from 'vitest'

let lastResttyOptions = null
let lastTransport = null

vi.mock('restty', () => ({
  Restty: class {
    constructor(options) {
      lastResttyOptions = options
      options.__instance = this
    }

    destroy = vi.fn()
    connectPty = vi.fn()
    disconnectPty = vi.fn()
    focus = vi.fn()
    getColorForeground = vi.fn(() => 0x112233)
    getColorBackground = vi.fn(() => 0x445566)
    getColorCursor = vi.fn(() => 0x778899)
    getPalette = vi.fn(() => {
      const palette = new Uint8Array(256 * 3)
      for (let index = 0; index < 256; index += 1) {
        const offset = index * 3
        palette[offset] = index
        palette[offset + 1] = 255 - index
        palette[offset + 2] = index % 17
      }
      return palette
    })
    sendKeyInput = vi.fn()
    setMouseMode = vi.fn()
    updateSize = vi.fn()
  },
}))

vi.mock('../lib/hub-bridge', () => ({
  waitForHub: vi.fn(async () => ({ transport: { clearNotification: vi.fn() } })),
}))

vi.mock('transport/webrtc_pty_transport', () => ({
  WebRtcPtyTransport: class {
    constructor() {
      lastTransport = this
    }

    calls = []
    connectCallback = null
    disconnect = vi.fn()
    destroy = vi.fn()
    resize = vi.fn((cols, rows) => {
      this.calls.push(['resize', cols, rows])
      return true
    })
    sendFocusChanged = vi.fn((focused) => {
      this.calls.push(['focus_changed', focused])
    })
    sendInput = vi.fn((data) => {
      this.calls.push(['input', data])
    })
    sendColorProfile = vi.fn((colors) => {
      this.calls.push(['color_profile', colors])
    })
    set onReconnect(_callback) {}
    set onBinarySnapshot(_callback) {}
    set onFocusReportingChanged(_callback) {}
    set onConnect(callback) {
      this.connectCallback = callback
    }
    set onDisconnect(_callback) {}
    emitConnect() {
      this.connectCallback?.()
    }
  },
}))

import TerminalView from '../components/terminal/TerminalView'

describe('TerminalView Restty options', () => {
  afterEach(() => {
    cleanup()
    vi.useRealTimers()
    lastResttyOptions = null
    lastTransport = null
    delete window._botsterTestTerminal
  })

  it('keeps Restty terminal-generated replies off the raw PTY input path', async () => {
    render(<TerminalView hubId="hub-1" sessionUuid="session-1" />)

    await waitFor(() => expect(lastResttyOptions).not.toBeNull())

    expect(lastResttyOptions.appOptions.readOnly).toBe(true)
  })

  it('publishes browser colors before sending focus-in PTY input', async () => {
    render(<TerminalView hubId="hub-1" sessionUuid="session-1" />)

    await waitFor(() => expect(lastTransport).not.toBeNull())
    await waitFor(() => expect(lastResttyOptions).not.toBeNull())

    lastTransport.emitConnect()

    await waitFor(() => {
      expect(lastTransport.calls.map(([type]) => type)).toEqual([
        'color_profile',
        'focus_changed',
        'input',
      ])
    })

    expect(lastTransport.calls[0][1][257]).toEqual({ r: 0, g: 0, b: 0 })
    expect(lastTransport.calls[0][1][7]).toEqual({ r: 7, g: 248, b: 7 })
    expect(lastTransport.calls[1][1]).toBe(true)
    expect(lastTransport.calls[2][1]).toBe('\x1b[I')
  })

  it('pushes measured size to transport before opening the PTY subscription', async () => {
    render(<TerminalView hubId="hub-1" sessionUuid="session-1" />)

    await waitFor(() => expect(lastResttyOptions).not.toBeNull())
    await waitFor(() => expect(lastTransport).not.toBeNull())
    vi.useFakeTimers()
    lastResttyOptions.appOptions.callbacks.onBackend()
    lastResttyOptions.appOptions.callbacks.onTermSize(120, 33)

    act(() => {
      vi.advanceTimersByTime(35)
    })

    expect(lastResttyOptions.root).not.toBeNull()
    const restty = lastResttyOptions.__instance
    expect(lastTransport.resize).toHaveBeenCalledWith(120, 33)
    expect(restty.updateSize).toHaveBeenCalledWith(true)
    expect(restty.connectPty).toHaveBeenCalled()
    expect(lastTransport.resize.mock.invocationCallOrder[0]).toBeLessThan(
      restty.connectPty.mock.invocationCallOrder[0],
    )
    expect(restty.updateSize.mock.invocationCallOrder[0]).toBeLessThan(
      restty.connectPty.mock.invocationCallOrder[0],
    )
  })
})
