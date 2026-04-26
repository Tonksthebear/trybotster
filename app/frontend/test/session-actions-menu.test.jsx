import React from 'react'
import {
  afterEach,
  beforeEach,
  describe,
  expect,
  it,
  vi,
} from 'vitest'
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react'

vi.mock('../lib/actions', () => ({
  ACTION: {
    SESSION_DELETE: 'botster.session.delete.request',
    SESSION_MOVE: 'botster.session.move.request',
    PREVIEW_TOGGLE: 'botster.session.preview.toggle',
    PREVIEW_OPEN: 'botster.session.preview.open',
  },
  safeUrl: (u) => u ?? null,
  dispatch: vi.fn(),
}))

import * as hubBridge from '../lib/hub-bridge'
import { useSessionStore } from '../store/entities'
import UiTree, { useUiTreeDispatch } from '../components/UiTree'
import SessionActionsMenu from '../components/workspace/SessionActionsMenu'

class FakeTransport {
  constructor() {
    this._listeners = new Map()
    this.send = vi.fn(async () => true)
  }
  on(event, callback) {
    if (!this._listeners.has(event)) this._listeners.set(event, new Set())
    this._listeners.get(event).add(callback)
    return () => this._listeners.get(event)?.delete(callback)
  }
  emit(event, payload) {
    const callbacks = this._listeners.get(event)
    if (!callbacks) return
    for (const cb of callbacks) cb(payload)
  }
}

let fakeTransport

function immediateHub(transport) {
  return {
    then(resolve) {
      resolve({ transport })
      return Promise.resolve({ transport })
    },
  }
}

const MENU_TRIGGER_TREE = {
  type: 'icon_button',
  props: {
    icon: 'ellipsis-vertical',
    label: 'Session actions',
    action: {
      id: 'botster.session.menu.open',
      payload: { sessionId: 's-1', sessionUuid: 'u-1' },
    },
  },
}

beforeEach(() => {
  fakeTransport = new FakeTransport()
  vi.spyOn(hubBridge, 'waitForHub').mockImplementation(() => immediateHub(fakeTransport))
  // Wire protocol: seed the session entity store directly.
  useSessionStore.setState({
    byId: {
      's-1': {
        id: 's-1',
        session_uuid: 'u-1',
        session_type: 'agent',
        port: 8080,
        hosted_preview: { status: 'inactive' },
      },
    },
    order: ['s-1'],
    snapshotSeq: 1,
  })
})

afterEach(() => {
  cleanup()
  vi.restoreAllMocks()
  useSessionStore.getState()._reset()
})

describe('<SessionActionsMenu> interceptor', () => {
  it('intercepts botster.session.menu.open and prevents transport send', async () => {
    render(
      <UiTree hubId="hub-1" targetSurface="workspace_panel">
        <SessionActionsMenu />
      </UiTree>,
    )

    await act(async () => {
      fakeTransport.emit('message', {
        type: 'ui_tree_snapshot',
        target_surface: 'workspace_panel',
        tree: MENU_TRIGGER_TREE,
      })
    })

    const trigger = await screen.findByRole('button', {
      name: 'Session actions',
    })
    // Drop the initial surface.subpath send — this test only cares that
    // the clicked menu-open action is CONSUMED by the interceptor and
    // never forwarded to transport.
    fakeTransport.send.mockClear()
    fireEvent.click(trigger)
    await Promise.resolve()
    await Promise.resolve()

    // Interceptor consumed the action — nothing should be sent over transport.
    expect(fakeTransport.send).not.toHaveBeenCalled()

    // The interceptor stages the dropdown by mounting an invisible
    // MenuButton anchored to the trigger and programmatically clicking it.
    // The portaled dropdown root appears on the document body once the
    // setTimeout fires.
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 5))
    })

    const anchor = document.querySelector(
      '[data-testid="session-actions-menu-trigger"]',
    )
    expect(anchor).not.toBeNull()
  })

  it('does nothing without an anchor element (returns false from interceptor)', async () => {
    let captured = null
    function ProbeDispatcher() {
      const dispatch = useUiTreeDispatch()
      React.useEffect(() => {
        captured = dispatch
      }, [dispatch])
      return null
    }
    render(
      <UiTree hubId="hub-1" targetSurface="workspace_panel">
        <SessionActionsMenu />
        <ProbeDispatcher />
      </UiTree>,
    )

    // Dispatch the action programmatically without a source.element. The
    // interceptor returns false, so the transport send should fire.
    captured?.({
      id: 'botster.session.menu.open',
      payload: { sessionId: 's-1', sessionUuid: 'u-1' },
    })
    await Promise.resolve()
    await Promise.resolve()
    expect(fakeTransport.send).toHaveBeenCalled()
  })
})
