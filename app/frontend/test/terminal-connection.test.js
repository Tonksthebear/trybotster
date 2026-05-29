import { describe, expect, it, vi } from 'vitest'

import { TerminalConnection } from '../lib/connections/terminal_connection'

function terminalConnection() {
  return new TerminalConnection(
    'terminal_hub-1_session-1',
    {
      hubId: 'hub-1',
      sessionUuid: 'session-1',
    },
    {
      hasActiveConnectionForHub: () => true,
      release: vi.fn(),
    },
  )
}

describe('TerminalConnection', () => {
  it('includes session uuid in resize and snapshot commands', async () => {
    const terminal = terminalConnection()
    const commands = []
    terminal.sendCommand = vi.fn(async (type, data) => {
      commands.push([type, data])
      return true
    })

    await terminal.sendResize(132, 37)
    await terminal.requestSnapshot()

    expect(commands).toEqual([
      ['resize', { session_uuid: 'session-1', cols: 132, rows: 37 }],
      ['request_snapshot', { session_uuid: 'session-1', rows: 37, cols: 132 }],
    ])
  })

  it('can request a snapshot with explicit dimensions', async () => {
    const terminal = terminalConnection()
    terminal.sendCommand = vi.fn(async () => true)

    await terminal.requestSnapshot({ cols: 140, rows: 45 })

    expect(terminal.sendCommand).toHaveBeenCalledWith('request_snapshot', {
      session_uuid: 'session-1',
      rows: 45,
      cols: 140,
    })
  })

  it('includes session uuid in focus change telemetry', async () => {
    const terminal = terminalConnection()
    const telemetry = []
    terminal.sendTelemetry = vi.fn(async (type, data) => {
      telemetry.push([type, data])
      return true
    })

    await terminal.sendFocusChanged(true)
    await terminal.sendFocusChanged(false)

    expect(telemetry).toEqual([
      ['focus_changed', { session_uuid: 'session-1', focused: true }],
      ['focus_changed', { session_uuid: 'session-1', focused: false }],
    ])
  })

  it('surfaces terminal_attach not_ready as a user-visible error event', () => {
    const terminal = terminalConnection()
    const attachEvents = []
    const errors = []

    terminal.onTerminalAttach((event) => attachEvents.push(event))
    terminal.onError((event) => errors.push(event))

    terminal.handleMessage({
      type: 'terminal_attach',
      session_uuid: 'session-1',
      state: 'not_ready',
    })

    expect(attachEvents).toEqual([
      {
        type: 'terminal_attach',
        session_uuid: 'session-1',
        state: 'not_ready',
      },
    ])
    expect(errors).toEqual([
      {
        reason: 'terminal_not_ready',
        message: 'Terminal session is not ready for input yet. Reconnecting session I/O...',
      },
    ])
  })

  it('buffers early mode_changed events until a mode listener is attached', () => {
    const terminal = terminalConnection()
    const genericMessages = []
    const modeChanges = []
    const message = {
      type: 'mode_changed',
      session_uuid: 'session-1',
      mode: { mouse_mode: 12 },
    }

    terminal.on('message', (event) => genericMessages.push(event))
    terminal.handleMessage(message)
    terminal.onModeChanged((event) => modeChanges.push(event))

    expect(modeChanges).toEqual([message])
    expect(genericMessages).toEqual([message])
  })
})
