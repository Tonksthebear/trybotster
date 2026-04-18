import React from 'react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
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

import { dispatch as legacyDispatch } from '../../lib/actions'
import { UiTree, createHubDispatch } from '..'
import type { UiNodeV1, UiViewportV1 } from '../types'

const REGULAR_FINE: UiViewportV1 = {
  widthClass: 'expanded',
  heightClass: 'regular',
  pointer: 'fine',
}

afterEach(() => {
  cleanup()
  vi.mocked(legacyDispatch).mockClear()
})

describe('createHubDispatch — legacy bridge', () => {
  it('forwards UiActionV1 to legacy dispatcher with hubId merged into payload', () => {
    const dispatch = createHubDispatch('hub-123')
    const node: UiNodeV1 = {
      type: 'button',
      props: {
        label: 'Select',
        action: {
          id: 'botster.session.select',
          payload: { sessionId: 'sess-1', sessionUuid: 'uuid-1' },
        },
      },
    }
    render(<UiTree node={node} dispatch={dispatch} viewport={REGULAR_FINE} />)
    fireEvent.click(screen.getByRole('button', { name: 'Select' }))

    expect(legacyDispatch).toHaveBeenCalledOnce()
    expect(legacyDispatch).toHaveBeenCalledWith({
      action: 'botster.session.select',
      payload: {
        hubId: 'hub-123',
        sessionId: 'sess-1',
        sessionUuid: 'uuid-1',
      },
    })
  })

  it('skips dispatch when action.disabled is true', () => {
    const dispatch = createHubDispatch('hub-x')
    const node: UiNodeV1 = {
      type: 'button',
      props: {
        label: 'Nope',
        action: { id: 'botster.session.select', disabled: true },
      },
    }
    render(<UiTree node={node} dispatch={dispatch} viewport={REGULAR_FINE} />)
    const btn = screen.getByRole('button', { name: 'Nope' })
    expect(btn).toBeDisabled()
    fireEvent.click(btn)
    expect(legacyDispatch).not.toHaveBeenCalled()
  })

  it('works with empty payload', () => {
    const dispatch = createHubDispatch('hub-q')
    const node: UiNodeV1 = {
      type: 'button',
      props: {
        label: 'Ping',
        action: { id: 'botster.workspace.toggle' },
      },
    }
    render(<UiTree node={node} dispatch={dispatch} viewport={REGULAR_FINE} />)
    fireEvent.click(screen.getByRole('button', { name: 'Ping' }))

    expect(legacyDispatch).toHaveBeenCalledWith({
      action: 'botster.workspace.toggle',
      payload: { hubId: 'hub-q' },
    })
  })
})
