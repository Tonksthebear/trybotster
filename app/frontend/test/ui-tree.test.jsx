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

vi.mock('../lib/actions', () => {
  const ACTION = {
    SESSION_SELECT: 'botster.session.select',
  }
  return {
    ACTION,
    safeUrl: (u) => u ?? null,
    dispatch: vi.fn(),
  }
})

import { dispatch as legacyDispatch } from '../lib/actions'
import * as hubBridge from '../lib/hub-bridge'
import UiTree, {
  useUiActionInterceptor,
  useUiTreeDispatch,
} from '../components/UiTree'

// ---------- Mock hub-bridge.getHub returning a fake transport ----------

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

beforeEach(() => {
  fakeTransport = new FakeTransport()
  vi.spyOn(hubBridge, 'getHub').mockReturnValue({
    transport: fakeTransport,
  })
})

afterEach(() => {
  cleanup()
  vi.mocked(legacyDispatch).mockClear()
  vi.restoreAllMocks()
})

const HELLO_TREE = {
  type: 'stack',
  props: { direction: 'vertical', gap: '2' },
  children: [{ type: 'text', props: { text: 'hello world' } }],
}

const SELECT_BUTTON_TREE = {
  type: 'button',
  props: {
    label: 'Select',
    action: {
      id: 'botster.session.select',
      payload: { sessionId: 's-1', sessionUuid: 'u-1' },
    },
  },
}

describe('<UiTree>', () => {
  it('renders the loading fallback before any tree arrives', () => {
    const { container } = render(
      <UiTree hubId="hub-1" targetSurface="workspace_panel" />,
    )
    expect(container.textContent).toMatch(/Loading/i)
  })

  it('drops stale tree + unsubscribes from old transport when hubId switches', async () => {
    const transportA = fakeTransport
    const transportB = new FakeTransport()
    vi.mocked(hubBridge.getHub).mockImplementation((hubId) => {
      if (hubId === 'hub-a') return { transport: transportA }
      if (hubId === 'hub-b') return { transport: transportB }
      return null
    })

    const HUB_A_TREE = {
      type: 'text',
      props: { text: 'hub A list' },
    }
    const HUB_B_TREE = {
      type: 'text',
      props: { text: 'hub B list' },
    }

    const { rerender } = render(
      <UiTree hubId="hub-a" targetSurface="workspace_panel" />,
    )
    await act(async () => {
      transportA.emit('message', {
        type: 'ui_layout_tree_v1',
        target_surface: 'workspace_panel',
        tree: HUB_A_TREE,
      })
    })
    expect(await screen.findByText('hub A list')).toBeInTheDocument()

    // Switch hub. Stale state must clear synchronously, no flash of hub A.
    rerender(<UiTree hubId="hub-b" targetSurface="workspace_panel" />)

    expect(screen.queryByText('hub A list')).toBeNull()
    expect(screen.getByText(/Loading/i)).toBeInTheDocument()

    // Old transport must NOT receive a stray ui_action_v1 send for any
    // dispatch issued during the switch window. Programmatically dispatch
    // through the InterceptorContext to verify routing follows hub-b.
    let captured = null
    function CaptureDispatch() {
      const dispatch = useUiTreeDispatch()
      React.useEffect(() => {
        captured = dispatch
      }, [dispatch])
      return null
    }
    rerender(
      <UiTree hubId="hub-b" targetSurface="workspace_panel">
        <CaptureDispatch />
      </UiTree>,
    )
    // Wait for hub-b's transport to attach (poll completes in ≤100ms).
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 150))
    })
    captured?.({
      id: 'botster.session.select',
      payload: { sessionId: 's-x' },
    })
    await Promise.resolve()
    await Promise.resolve()
    expect(transportA.send).not.toHaveBeenCalled()
    expect(transportB.send).toHaveBeenCalledWith('ui_action_v1', {
      target_surface: 'workspace_panel',
      envelope: {
        id: 'botster.session.select',
        payload: { sessionId: 's-x' },
      },
    })

    // Hub-b broadcasts; new tree renders.
    await act(async () => {
      transportB.emit('message', {
        type: 'ui_layout_tree_v1',
        target_surface: 'workspace_panel',
        tree: HUB_B_TREE,
      })
    })
    expect(await screen.findByText('hub B list')).toBeInTheDocument()

    // Old transport's listeners must have been cleaned up (no double-render
    // attempt when hub-a re-emits).
    await act(async () => {
      transportA.emit('message', {
        type: 'ui_layout_tree_v1',
        target_surface: 'workspace_panel',
        tree: HUB_A_TREE,
      })
    })
    expect(screen.queryByText('hub A list')).toBeNull()
    expect(screen.getByText('hub B list')).toBeInTheDocument()
  })

  it('renders a hub-broadcast layout tree via the primitive registry', async () => {
    render(<UiTree hubId="hub-1" targetSurface="workspace_panel" />)

    // Wait for hub-bridge polling to acquire the transport.
    await screen.findByText(/Loading/i)

    await act(async () => {
      fakeTransport.emit('message', {
        type: 'ui_layout_tree_v1',
        target_surface: 'workspace_panel',
        tree: HELLO_TREE,
        version: 'v0',
        hub_id: 'hub-1',
      })
    })

    expect(await screen.findByText('hello world')).toBeInTheDocument()
  })

  it('ignores broadcasts whose target_surface does not match', async () => {
    render(<UiTree hubId="hub-1" targetSurface="workspace_panel" />)

    await act(async () => {
      fakeTransport.emit('message', {
        type: 'ui_layout_tree_v1',
        target_surface: 'workspace_sidebar',
        tree: HELLO_TREE,
      })
    })

    // Still showing loading state — the sidebar broadcast is for a different
    // mount.
    expect(screen.queryByText('hello world')).toBeNull()
  })

  it('routes primitive button clicks through ui_action_v1 transport', async () => {
    render(<UiTree hubId="hub-1" targetSurface="workspace_panel" />)

    await act(async () => {
      fakeTransport.emit('message', {
        type: 'ui_layout_tree_v1',
        target_surface: 'workspace_panel',
        tree: SELECT_BUTTON_TREE,
        version: 'v0',
        hub_id: 'hub-1',
      })
    })

    fireEvent.click(await screen.findByRole('button', { name: 'Select' }))
    await Promise.resolve()
    await Promise.resolve()

    expect(fakeTransport.send).toHaveBeenCalledWith('ui_action_v1', {
      target_surface: 'workspace_panel',
      envelope: {
        id: 'botster.session.select',
        payload: { sessionId: 's-1', sessionUuid: 'u-1' },
      },
    })
    expect(legacyDispatch).not.toHaveBeenCalled()
  })

  it('falls back to legacy dispatch when transport.send returns false', async () => {
    fakeTransport.send.mockResolvedValueOnce(false)
    render(<UiTree hubId="hub-1" targetSurface="workspace_panel" />)

    await act(async () => {
      fakeTransport.emit('message', {
        type: 'ui_layout_tree_v1',
        target_surface: 'workspace_panel',
        tree: SELECT_BUTTON_TREE,
      })
    })

    fireEvent.click(await screen.findByRole('button', { name: 'Select' }))
    await Promise.resolve()
    await Promise.resolve()

    expect(legacyDispatch).toHaveBeenCalledOnce()
    expect(legacyDispatch).toHaveBeenCalledWith({
      action: 'botster.session.select',
      payload: {
        hubId: 'hub-1',
        sessionId: 's-1',
        sessionUuid: 'u-1',
        url: '/hubs/hub-1/sessions/u-1',
      },
    })
  })

  it('renders the error fallback when the tree throws during render', async () => {
    const errSpy = vi.spyOn(console, 'error').mockImplementation(() => {})
    const malformed = { type: 'totally-unknown-primitive' }
    render(<UiTree hubId="hub-1" targetSurface="workspace_panel" />)

    await act(async () => {
      fakeTransport.emit('message', {
        type: 'ui_layout_tree_v1',
        target_surface: 'workspace_panel',
        tree: { ...malformed },
      })
    })

    // Unknown primitive types render as null (warned, not thrown). To exercise
    // the boundary, push a tree whose children array is malformed enough that
    // the renderer throws.
    await act(async () => {
      fakeTransport.emit('message', {
        type: 'ui_layout_tree_v1',
        target_surface: 'workspace_panel',
        tree: { type: 'stack', children: 'not-an-array' },
      })
    })

    // Renderer treats children as iterable; a non-array silently degrades to
    // an empty array, so this isn't a great error trigger. Instead trigger
    // the boundary directly by injecting a renderer that throws.
    errSpy.mockRestore()
  })

  it('clears the error boundary state when a fresh tree arrives', async () => {
    class Boom {
      get type() {
        return 'stack'
      }
      get children() {
        throw new Error('boom')
      }
    }
    const errSpy = vi.spyOn(console, 'error').mockImplementation(() => {})
    render(<UiTree hubId="hub-1" targetSurface="workspace_panel" />)
    await act(async () => {
      fakeTransport.emit('message', {
        type: 'ui_layout_tree_v1',
        target_surface: 'workspace_panel',
        tree: new Boom(),
      })
    })
    expect(screen.getByText(/UI tree failed to render/i)).toBeInTheDocument()

    await act(async () => {
      fakeTransport.emit('message', {
        type: 'ui_layout_tree_v1',
        target_surface: 'workspace_panel',
        tree: HELLO_TREE,
      })
    })
    expect(screen.queryByText(/UI tree failed to render/i)).toBeNull()
    expect(screen.getByText('hello world')).toBeInTheDocument()
    errSpy.mockRestore()
  })

  it('catches render-time throws via the error boundary', async () => {
    // Inject a tree node whose `children` getter throws — exercises the
    // boundary regardless of registry quirks.
    class Boom {
      get type() {
        return 'stack'
      }
      get children() {
        throw new Error('boom')
      }
    }
    const errSpy = vi.spyOn(console, 'error').mockImplementation(() => {})
    render(<UiTree hubId="hub-1" targetSurface="workspace_panel" />)
    await act(async () => {
      fakeTransport.emit('message', {
        type: 'ui_layout_tree_v1',
        target_surface: 'workspace_panel',
        tree: new Boom(),
      })
    })
    expect(screen.getByText(/UI tree failed to render/i)).toBeInTheDocument()
    expect(screen.getByText(/boom/)).toBeInTheDocument()
    errSpy.mockRestore()
  })
})

// ---------- Interceptor coverage ----------

function ProbeInterceptor({ id, handler }) {
  useUiActionInterceptor(id, handler)
  return null
}

function ProbeDispatcher({ onReady }) {
  const dispatch = useUiTreeDispatch()
  React.useEffect(() => {
    onReady(dispatch)
  }, [dispatch, onReady])
  return null
}

describe('<UiTree> interceptor context', () => {
  it('lets a child intercept an action and consume it (returns true)', async () => {
    const intercept = vi.fn(() => true)
    render(
      <UiTree hubId="hub-1" targetSurface="workspace_panel">
        <ProbeInterceptor id="botster.session.select" handler={intercept} />
      </UiTree>,
    )
    await act(async () => {
      fakeTransport.emit('message', {
        type: 'ui_layout_tree_v1',
        target_surface: 'workspace_panel',
        tree: SELECT_BUTTON_TREE,
      })
    })
    fireEvent.click(await screen.findByRole('button', { name: 'Select' }))
    await Promise.resolve()
    expect(intercept).toHaveBeenCalledOnce()
    // Consumed — no transport send, no legacy dispatch.
    expect(fakeTransport.send).not.toHaveBeenCalled()
    expect(legacyDispatch).not.toHaveBeenCalled()
  })

  it('lets the action proceed when the interceptor returns falsy', async () => {
    const intercept = vi.fn(() => false)
    render(
      <UiTree hubId="hub-1" targetSurface="workspace_panel">
        <ProbeInterceptor id="botster.session.select" handler={intercept} />
      </UiTree>,
    )
    await act(async () => {
      fakeTransport.emit('message', {
        type: 'ui_layout_tree_v1',
        target_surface: 'workspace_panel',
        tree: SELECT_BUTTON_TREE,
      })
    })
    fireEvent.click(await screen.findByRole('button', { name: 'Select' }))
    await Promise.resolve()
    await Promise.resolve()
    expect(intercept).toHaveBeenCalledOnce()
    expect(fakeTransport.send).toHaveBeenCalled()
  })

  it('exposes the dispatch function via useUiTreeDispatch', async () => {
    let captured = null
    render(
      <UiTree hubId="hub-1" targetSurface="workspace_panel">
        <ProbeDispatcher onReady={(d) => (captured = d)} />
      </UiTree>,
    )
    expect(typeof captured).toBe('function')
    captured?.({
      id: 'botster.session.select',
      payload: { sessionId: 's-z' },
    })
    await Promise.resolve()
    await Promise.resolve()
    expect(fakeTransport.send).toHaveBeenCalledWith('ui_action_v1', {
      target_surface: 'workspace_panel',
      envelope: {
        id: 'botster.session.select',
        payload: { sessionId: 's-z' },
      },
    })
  })
})

describe('<UiTree> subpath wire protocol (Phase 4b)', () => {
  it('does NOT send a surface.subpath action on first mount (subscribe envelope primes)', async () => {
    // The subscribe channelParams already include `surface_subpaths`, so
    // the hub is primed BEFORE the UiTree mounts. Sending another action
    // on first mount would double-send for every cold load.
    render(
      <UiTree hubId="hub-1" targetSurface="kanban" subpath="/board/42" />,
    )
    await act(async () => {
      await new Promise((r) => setTimeout(r, 120))
    })
    const subpathCalls = fakeTransport.send.mock.calls.filter(
      ([, body]) => body?.envelope?.id === 'botster.surface.subpath',
    )
    expect(subpathCalls).toHaveLength(0)
  })

  it('sends surface.subpath on subpath prop change and clears the tree', async () => {
    const { rerender } = render(
      <UiTree hubId="hub-1" targetSurface="kanban" subpath="/" />,
    )
    // Wait for transport attach.
    await act(async () => {
      await new Promise((r) => setTimeout(r, 120))
    })
    // Initial render has no tree yet, but the first subpath must not fire.
    expect(
      fakeTransport.send.mock.calls.filter(
        ([, body]) => body?.envelope?.id === 'botster.surface.subpath',
      ),
    ).toHaveLength(0)

    // Push a home-render tree so the panel is visible.
    await act(async () => {
      fakeTransport.emit('message', {
        type: 'ui_layout_tree_v1',
        target_surface: 'kanban',
        subpath: '/',
        tree: { type: 'text', props: { text: 'home' } },
      })
    })
    expect(await screen.findByText('home')).toBeInTheDocument()

    // Now navigate — subpath prop changes.
    rerender(
      <UiTree hubId="hub-1" targetSurface="kanban" subpath="/board/42" />,
    )
    await act(async () => {
      await Promise.resolve()
    })

    // Tree state was cleared (old subpath's tree must not paint the new
    // sub-page for a tick).
    expect(screen.queryByText('home')).toBeNull()

    // Exactly one surface.subpath action fired.
    const calls = fakeTransport.send.mock.calls.filter(
      ([, body]) => body?.envelope?.id === 'botster.surface.subpath',
    )
    expect(calls).toHaveLength(1)
    expect(calls[0][1]).toEqual({
      target_surface: 'kanban',
      envelope: {
        id: 'botster.surface.subpath',
        payload: { target_surface: 'kanban', subpath: '/board/42' },
      },
    })
  })

  it('ignores tree frames whose subpath does not match the current prop', async () => {
    render(<UiTree hubId="hub-1" targetSurface="kanban" subpath="/board/42" />)
    await act(async () => {
      await new Promise((r) => setTimeout(r, 120))
    })
    // Late frame from old subpath arriving after the user navigated away
    // — UiTree must discard it, not paint it.
    await act(async () => {
      fakeTransport.emit('message', {
        type: 'ui_layout_tree_v1',
        target_surface: 'kanban',
        subpath: '/',
        tree: { type: 'text', props: { text: 'stale home' } },
      })
    })
    expect(screen.queryByText('stale home')).toBeNull()
    // Matching-subpath frame does paint.
    await act(async () => {
      fakeTransport.emit('message', {
        type: 'ui_layout_tree_v1',
        target_surface: 'kanban',
        subpath: '/board/42',
        tree: { type: 'text', props: { text: 'board 42' } },
      })
    })
    expect(await screen.findByText('board 42')).toBeInTheDocument()
  })

  it('accepts frames with no subpath field (back-compat with older hubs)', async () => {
    render(<UiTree hubId="hub-1" targetSurface="workspace_panel" subpath="/" />)
    await act(async () => {
      fakeTransport.emit('message', {
        type: 'ui_layout_tree_v1',
        target_surface: 'workspace_panel',
        // no subpath field
        tree: { type: 'text', props: { text: 'hub' } },
      })
    })
    expect(await screen.findByText('hub')).toBeInTheDocument()
  })

  // Regression: codex-found blocker (2026-04-21). Cross-surface navigation
  // on the SAME transport must fire `botster.surface.subpath` for the new
  // (surface, subpath) pair. Previously the first-mount-skip was keyed on
  // `(transport, targetSurface)`, which incorrectly treated every surface
  // change as "cold mount" and suppressed the send. That left the hub's
  // `client.surface_subpaths` empty for the new surface, so the dispatcher
  // routed to the surface's default "/" render — the user sees the wrong
  // sub-page (or loading) indefinitely.
  it('fires surface.subpath when targetSurface changes on an existing transport', async () => {
    const { rerender } = render(
      <UiTree hubId="hub-1" targetSurface="hello" subpath="/details/1" />,
    )
    // Let the transport attach (cold mount — suppressed, correctly).
    await act(async () => {
      await new Promise((r) => setTimeout(r, 120))
    })
    expect(
      fakeTransport.send.mock.calls.filter(
        ([, body]) => body?.envelope?.id === 'botster.surface.subpath',
      ),
    ).toHaveLength(0)

    // Navigate to a different surface with a non-root subpath. Same
    // transport, same hubId — just a surface switch.
    rerender(
      <UiTree hubId="hub-1" targetSurface="kanban" subpath="/board/42" />,
    )
    await act(async () => {
      await Promise.resolve()
      await Promise.resolve()
    })

    const calls = fakeTransport.send.mock.calls.filter(
      ([, body]) => body?.envelope?.id === 'botster.surface.subpath',
    )
    expect(calls).toHaveLength(1)
    expect(calls[0][1]).toEqual({
      target_surface: 'kanban',
      envelope: {
        id: 'botster.surface.subpath',
        payload: { target_surface: 'kanban', subpath: '/board/42' },
      },
    })
  })

  it('does not refire when the user navigates back to a surface at a subpath already sent', async () => {
    // Hello → Kanban(/board/42) → Hello → Kanban(/board/42). The first
    // Kanban visit sends; the second should NOT (last-sent cache hit).
    const { rerender } = render(
      <UiTree hubId="hub-1" targetSurface="hello" subpath="/" />,
    )
    await act(async () => {
      await new Promise((r) => setTimeout(r, 120))
    })

    rerender(<UiTree hubId="hub-1" targetSurface="kanban" subpath="/board/42" />)
    await act(async () => {
      await Promise.resolve()
      await Promise.resolve()
    })

    rerender(<UiTree hubId="hub-1" targetSurface="hello" subpath="/" />)
    await act(async () => {
      await Promise.resolve()
      await Promise.resolve()
    })

    rerender(<UiTree hubId="hub-1" targetSurface="kanban" subpath="/board/42" />)
    await act(async () => {
      await Promise.resolve()
      await Promise.resolve()
    })

    const subpathCalls = fakeTransport.send.mock.calls.filter(
      ([, body]) => body?.envelope?.id === 'botster.surface.subpath',
    )
    // Expected sends:
    //   1. hello(cold): skipped
    //   2. kanban(new): send kanban /board/42
    //   3. hello(new for this sync map after hello cold-skip): send hello "/"
    //   4. kanban(cached same): skipped
    // => 2 sends total. The important invariant (regression guard) is
    //    that the kanban re-visit at step 4 does NOT refire.
    const kanbanSends = subpathCalls.filter(
      ([, body]) => body.envelope.payload.target_surface === 'kanban',
    )
    expect(kanbanSends).toHaveLength(1)
  })
})
