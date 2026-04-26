import React from 'react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { cleanup, render, fireEvent, screen } from '@testing-library/react'

vi.mock('../../lib/actions', () => {
  const ACTION = {
    WORKSPACE_TOGGLE: 'botster.workspace.toggle',
    WORKSPACE_RENAME: 'botster.workspace.rename.request',
    SESSION_SELECT: 'botster.session.select',
    PREVIEW_TOGGLE: 'botster.session.preview.toggle',
    PREVIEW_OPEN: 'botster.session.preview.open',
    SESSION_MOVE: 'botster.session.move.request',
    SESSION_DELETE: 'botster.session.delete.request',
  }
  return {
    ACTION,
    safeUrl: (u: string | null | undefined) => u ?? null,
    dispatch: vi.fn(),
  }
})

import { dispatch as localDispatch } from '../../lib/actions'
import {
  UiTreeBody,
  createTransportDispatch,
  type UiActionTransport,
} from '..'
import type { UiNodeV1, UiViewportV1 } from '../types'

const REGULAR_FINE: UiViewportV1 = {
  widthClass: 'expanded',
  heightClass: 'regular',
  pointer: 'fine',
}

afterEach(() => {
  cleanup()
  vi.mocked(localDispatch).mockClear()
})

function selectButton(label: string): UiNodeV1 {
  return {
    type: 'button',
    props: {
      label,
      action: {
        id: 'botster.session.select',
        payload: { sessionId: 'sess-1', sessionUuid: 'uuid-1' },
      },
    },
  }
}

describe('createTransportDispatch — Phase 2c default', () => {
  function makeTransport(sendImpl?: (type: string, data: unknown) => Promise<boolean>) {
    const send = vi.fn(sendImpl ?? (async () => true))
    const transport: UiActionTransport = { send: send as UiActionTransport['send'] }
    return { transport, send }
  }

  it('sends ui_action frame on the configured target_surface and skips local dispatch', async () => {
    const { transport, send } = makeTransport()
    const dispatch = createTransportDispatch({
      transport,
      hubId: 'hub-7',
      targetSurface: 'workspace_surface',
    })
    render(
      <UiTreeBody
        node={selectButton('Select')}
        dispatch={dispatch}
        viewport={REGULAR_FINE}
      />,
    )
    fireEvent.click(screen.getByRole('button', { name: 'Select' }))

    // send is async-launched inside dispatch; flush the microtask + macrotask queues
    await Promise.resolve()
    await Promise.resolve()

    expect(send).toHaveBeenCalledOnce()
    expect(send).toHaveBeenCalledWith('ui_action', {
      target_surface: 'workspace_surface',
      envelope: {
        id: 'botster.session.select',
        payload: { sessionId: 'sess-1', sessionUuid: 'uuid-1' },
      },
    })
    expect(localDispatch).not.toHaveBeenCalled()
  })

  it('pushes session URL into history on session.select success (hub handles focus, browser owns route)', () => {
    const { transport, send } = makeTransport()
    const dispatch = createTransportDispatch({
      transport,
      hubId: 'hub-7',
      targetSurface: 'workspace_surface',
    })
    window.history.replaceState({}, '', '/hubs/hub-7')
    const popstateSpy = vi.fn()
    window.addEventListener('popstate', popstateSpy)

    render(
      <UiTreeBody
        node={selectButton('Select')}
        dispatch={dispatch}
        viewport={REGULAR_FINE}
      />,
    )
    fireEvent.click(screen.getByRole('button', { name: 'Select' }))

    expect(send).toHaveBeenCalledOnce()
    // Synchronous side-effect — runs before the async transport.send resolves.
    expect(window.location.pathname).toBe('/hubs/hub-7/sessions/uuid-1')
    expect(popstateSpy).toHaveBeenCalledOnce()
    window.removeEventListener('popstate', popstateSpy)
  })

  it('skips history.pushState when already on the target session path', () => {
    const { transport } = makeTransport()
    const dispatch = createTransportDispatch({
      transport,
      hubId: 'hub-7',
      targetSurface: 'workspace_surface',
    })
    window.history.replaceState({}, '', '/hubs/hub-7/sessions/uuid-1')
    const pushSpy = vi.spyOn(window.history, 'pushState')

    render(
      <UiTreeBody
        node={selectButton('Select')}
        dispatch={dispatch}
        viewport={REGULAR_FINE}
      />,
    )
    fireEvent.click(screen.getByRole('button', { name: 'Select' }))

    expect(pushSpy).not.toHaveBeenCalled()
    pushSpy.mockRestore()
  })

  it('does not locally dispatch hub-authored actions when transport send returns false', async () => {
    const { transport, send } = makeTransport(async () => false)
    const dispatch = createTransportDispatch({
      transport,
      hubId: 'hub-9',
      targetSurface: 'workspace_surface',
    })
    render(
      <UiTreeBody
        node={selectButton('Select')}
        dispatch={dispatch}
        viewport={REGULAR_FINE}
      />,
    )
    fireEvent.click(screen.getByRole('button', { name: 'Select' }))

    await Promise.resolve()
    await Promise.resolve()

    expect(send).toHaveBeenCalledOnce()
    expect(localDispatch).not.toHaveBeenCalled()
  })

  it('does not locally dispatch hub-authored actions when transport send throws', async () => {
    const { transport, send } = makeTransport(async () => {
      throw new Error('boom')
    })
    const dispatch = createTransportDispatch({
      transport,
      hubId: 'hub-9',
      targetSurface: 'workspace_surface',
    })
    // Silence the expected console.error
    const errSpy = vi.spyOn(console, 'error').mockImplementation(() => {})
    render(
      <UiTreeBody
        node={selectButton('Select')}
        dispatch={dispatch}
        viewport={REGULAR_FINE}
      />,
    )
    fireEvent.click(screen.getByRole('button', { name: 'Select' }))

    await Promise.resolve()
    await Promise.resolve()

    expect(send).toHaveBeenCalledOnce()
    expect(localDispatch).not.toHaveBeenCalled()
    errSpy.mockRestore()
  })

  it('drops hub-authored actions when transport is null', () => {
    const dispatch = createTransportDispatch({
      transport: null,
      hubId: 'hub-9',
      targetSurface: 'workspace_surface',
    })
    render(
      <UiTreeBody
        node={selectButton('Select')}
        dispatch={dispatch}
        viewport={REGULAR_FINE}
      />,
    )
    fireEvent.click(screen.getByRole('button', { name: 'Select' }))
    expect(localDispatch).not.toHaveBeenCalled()
  })

  it('does NOT fall back to local dispatch for non-idempotent actions like preview.toggle', async () => {
    const { transport, send } = makeTransport(async () => false)
    const dispatch = createTransportDispatch({
      transport,
      hubId: 'hub-9',
      targetSurface: 'workspace_surface',
    })
    const node: UiNodeV1 = {
      type: 'button',
      props: {
        label: 'Toggle preview',
        action: {
          id: 'botster.session.preview.toggle',
          payload: { sessionUuid: 'uuid-1' },
        },
      },
    }
    render(
      <UiTreeBody node={node} dispatch={dispatch} viewport={REGULAR_FINE} />,
    )
    fireEvent.click(screen.getByRole('button', { name: 'Toggle preview' }))

    await Promise.resolve()
    await Promise.resolve()

    expect(send).toHaveBeenCalledOnce()
    expect(localDispatch).not.toHaveBeenCalled()
  })

  it('skips dispatch entirely when action.disabled is true', () => {
    const { transport, send } = makeTransport()
    const dispatch = createTransportDispatch({
      transport,
      hubId: 'hub-x',
      targetSurface: 'workspace_surface',
    })
    const node: UiNodeV1 = {
      type: 'button',
      props: {
        label: 'Nope',
        action: { id: 'botster.session.select', disabled: true },
      },
    }
    render(<UiTreeBody node={node} dispatch={dispatch} viewport={REGULAR_FINE} />)
    fireEvent.click(screen.getByRole('button', { name: 'Nope' }))
    expect(send).not.toHaveBeenCalled()
    expect(localDispatch).not.toHaveBeenCalled()
  })

  it('dispatches browser-local actions (session.create.request) directly without touching transport', () => {
    const { transport, send } = makeTransport()
    const dispatch = createTransportDispatch({
      transport,
      hubId: 'hub-local',
      targetSurface: 'workspace_sidebar',
    })
    const node: UiNodeV1 = {
      type: 'button',
      props: {
        label: 'New session',
        action: { id: 'botster.session.create.request' },
      },
    }
    render(<UiTreeBody node={node} dispatch={dispatch} viewport={REGULAR_FINE} />)
    fireEvent.click(screen.getByRole('button', { name: 'New session' }))
    expect(send).not.toHaveBeenCalled()
    expect(localDispatch).toHaveBeenCalledOnce()
    expect(localDispatch).toHaveBeenCalledWith({
      action: 'botster.session.create.request',
      payload: { hubId: 'hub-local' },
    })
  })

  it('dispatches browser-local actions locally even when transport is available for other actions', () => {
    const { transport, send } = makeTransport()
    const dispatch = createTransportDispatch({
      transport,
      hubId: 'hub-mixed',
      targetSurface: 'workspace_panel',
    })
    const toggleNode: UiNodeV1 = {
      type: 'button',
      props: {
        label: 'Toggle',
        action: {
          id: 'botster.workspace.toggle',
          payload: { workspaceId: 'ws-1' },
        },
      },
    }
    render(
      <UiTreeBody node={toggleNode} dispatch={dispatch} viewport={REGULAR_FINE} />,
    )
    fireEvent.click(screen.getByRole('button', { name: 'Toggle' }))
    expect(send).not.toHaveBeenCalled()
    expect(localDispatch).toHaveBeenCalledOnce()
    expect(localDispatch).toHaveBeenCalledWith({
      action: 'botster.workspace.toggle',
      payload: { hubId: 'hub-mixed', workspaceId: 'ws-1' },
    })
  })
})
