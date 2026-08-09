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
        message: 'Terminal session is not ready for input yet.',
      },
    ])
    expect(terminal.isSessionClosed()).toBe(false)
    expect(terminal.shouldResubscribeOnPeerConnect()).toBe(true)
  })

  it('treats process_exited without exit_code field as soft close', () => {
    const terminal = terminalConnection()
    const exits = []
    terminal.onProcessExited((event) => exits.push(event))
    terminal.notifyIdle = vi.fn()

    // Wire shape when exit_code is null JSON (or missing before wire fix).
    terminal.handleMessage({
      type: 'process_exited',
      session_uuid: 'session-1',
    })

    expect(exits[0]).toEqual(
      expect.objectContaining({
        session_uuid: 'session-1',
        permanent: false,
      }),
    )
    expect(terminal.isSessionClosed()).toBe(false)
  })

  it('surfaces mode_changed as an ordinary control message', () => {
    const terminal = terminalConnection()
    const genericMessages = []
    const message = {
      type: 'mode_changed',
      session_uuid: 'session-1',
      mode: { mouse_mode: 12 },
    }

    terminal.on('message', (event) => genericMessages.push(event))
    terminal.handleMessage(message)

    expect(genericMessages).toEqual([message])
  })

  it('soft-closes on process_exited with null exit_code so re-acquire can recover', async () => {
    const terminal = terminalConnection()
    const exits = []
    terminal.onProcessExited((event) => exits.push(event))
    terminal.subscriptionId = 'terminal_session-1'
    terminal.notifyIdle = vi.fn()
    terminal.sendCommand = vi.fn(async () => true)
    terminal.sendBinaryPty = vi.fn(async () => true)

    terminal.handleMessage({
      type: 'process_exited',
      session_uuid: 'session-1',
      exit_code: null,
    })

    expect(exits).toEqual([
      expect.objectContaining({
        session_uuid: 'session-1',
        exit_code: null,
        permanent: false,
      }),
    ])
    // Soft close: subscription dropped, but route remains re-attachable.
    expect(terminal.isSessionClosed()).toBe(false)
    expect(terminal.shouldResubscribeOnPeerConnect()).toBe(true)
    expect(terminal.notifyIdle).toHaveBeenCalled()
  })

  it('permanently closes on process_exited with an exit code', async () => {
    const terminal = terminalConnection()
    const exits = []
    terminal.onProcessExited((event) => exits.push(event))
    terminal.subscriptionId = 'terminal_session-1'
    terminal.sendCommand = vi.fn(async () => true)
    terminal.sendBinaryPty = vi.fn(async () => true)

    terminal.handleMessage({
      type: 'process_exited',
      session_uuid: 'session-1',
      exit_code: 0,
    })

    expect(exits[0]).toEqual(
      expect.objectContaining({
        session_uuid: 'session-1',
        exit_code: 0,
        permanent: true,
      }),
    )
    expect(terminal.isSessionClosed()).toBe(true)
    expect(terminal.shouldResubscribeOnPeerConnect()).toBe(false)
    expect(terminal.hasSubscription()).toBe(false)

    await terminal.sendInput('x')
    await terminal.sendResize(80, 24)
    expect(terminal.sendBinaryPty).not.toHaveBeenCalled()
    expect(terminal.sendCommand).not.toHaveBeenCalled()
  })

  it('marks the session closed on terminal_attach not_found', () => {
    const terminal = terminalConnection()
    const exits = []
    terminal.onProcessExited((event) => exits.push(event))

    terminal.handleMessage({
      type: 'terminal_attach',
      session_uuid: 'session-1',
      state: 'not_found',
    })

    expect(terminal.isSessionClosed()).toBe(true)
    expect(exits[0]).toEqual(
      expect.objectContaining({
        session_uuid: 'session-1',
        reason: 'not_found',
        permanent: true,
      }),
    )
  })

  it('treats terminal_attach reconnecting as recoverable', () => {
    const terminal = terminalConnection()
    const errors = []
    terminal.onError((event) => errors.push(event))

    terminal.handleMessage({
      type: 'terminal_attach',
      session_uuid: 'session-1',
      state: 'reconnecting',
    })

    expect(terminal.isSessionClosed()).toBe(false)
    expect(errors).toEqual([
      {
        reason: 'terminal_reconnecting',
        message: 'Terminal session is reconnecting.',
      },
    ])
  })
})
