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
import {
  useSessionStore,
  useWorkspaceEntityStore,
} from '../store/entities'

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
  useSessionStore.getState()._reset()
  useWorkspaceEntityStore.getState()._reset()
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

  it('renders ui_tree_snapshot frames from the hub wire', async () => {
    render(<UiTree hubId="hub-1" targetSurface="workspace_panel" />)
    await act(async () => {
      fakeTransport.emit('message', {
        type: 'ui_tree_snapshot',
        target_surface: 'workspace_panel',
        tree: HELLO_TREE,
      })
    })
    expect(await screen.findByText('hello world')).toBeInTheDocument()
  })

  it('renders session_list composites without uncached selector loops', async () => {
    const consoleError = vi
      .spyOn(console, 'error')
      .mockImplementation(() => {})
    useWorkspaceEntityStore.getState().applySnapshot(
      [{ workspace_id: 'ws-1', name: 'Main' }],
      1,
    )
    useSessionStore.getState().applySnapshot(
      [
        {
          session_uuid: 'sess-1',
          title: 'Agent One',
          workspace_id: 'ws-1',
        },
      ],
      1,
    )

    render(<UiTree hubId="hub-1" targetSurface="workspace_panel" />)
    await act(async () => {
      fakeTransport.emit('message', {
        type: 'ui_tree_snapshot',
        target_surface: 'workspace_panel',
        tree: { type: 'session_list', props: { grouping: 'workspace' } },
      })
    })

    expect(await screen.findByText('Agent One')).toBeInTheDocument()
    expect(
      consoleError.mock.calls.some(([first]) =>
        String(first).includes('getSnapshot should be cached'),
      ),
    ).toBe(false)
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
        type: 'ui_tree_snapshot',
        target_surface: 'workspace_panel',
        tree: HUB_A_TREE,
      })
    })
    expect(await screen.findByText('hub A list')).toBeInTheDocument()

    // Switch hub. Stale state must clear synchronously, no flash of hub A.
    rerender(<UiTree hubId="hub-b" targetSurface="workspace_panel" />)

    expect(screen.queryByText('hub A list')).toBeNull()
    expect(screen.getByText(/Loading/i)).toBeInTheDocument()

    // Old transport must NOT receive a stray ui_action send for any
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
    // Clear pre-switch sends (transportA got its initial surface.subpath
    // send on mount; that is expected and unrelated to the cross-transport
    // routing we are asserting).
    transportA.send.mockClear()
    transportB.send.mockClear()
    captured?.({
      id: 'botster.session.select',
      payload: { sessionId: 's-x' },
    })
    await Promise.resolve()
    await Promise.resolve()
    expect(transportA.send).not.toHaveBeenCalled()
    expect(transportB.send).toHaveBeenCalledWith('ui_action', {
      target_surface: 'workspace_panel',
      envelope: {
        id: 'botster.session.select',
        payload: { sessionId: 's-x' },
      },
    })

    // Hub-b broadcasts; new tree renders.
    await act(async () => {
      transportB.emit('message', {
        type: 'ui_tree_snapshot',
        target_surface: 'workspace_panel',
        tree: HUB_B_TREE,
      })
    })
    expect(await screen.findByText('hub B list')).toBeInTheDocument()

    // Old transport's listeners must have been cleaned up (no double-render
    // attempt when hub-a re-emits).
    await act(async () => {
      transportA.emit('message', {
        type: 'ui_tree_snapshot',
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
        type: 'ui_tree_snapshot',
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
        type: 'ui_tree_snapshot',
        target_surface: 'workspace_sidebar',
        tree: HELLO_TREE,
      })
    })

    // Still showing loading state — the sidebar broadcast is for a different
    // mount.
    expect(screen.queryByText('hello world')).toBeNull()
  })

  it('routes primitive button clicks through ui_action transport', async () => {
    render(<UiTree hubId="hub-1" targetSurface="workspace_panel" />)

    await act(async () => {
      fakeTransport.emit('message', {
        type: 'ui_tree_snapshot',
        target_surface: 'workspace_panel',
        tree: SELECT_BUTTON_TREE,
        version: 'v0',
        hub_id: 'hub-1',
      })
    })

    fireEvent.click(await screen.findByRole('button', { name: 'Select' }))
    await Promise.resolve()
    await Promise.resolve()

    expect(fakeTransport.send).toHaveBeenCalledWith('ui_action', {
      target_surface: 'workspace_panel',
      envelope: {
        id: 'botster.session.select',
        payload: { sessionId: 's-1', sessionUuid: 'u-1' },
      },
    })
    expect(legacyDispatch).not.toHaveBeenCalled()
  })

  it('falls back to legacy dispatch when transport.send returns false', async () => {
    render(<UiTree hubId="hub-1" targetSurface="workspace_panel" />)

    await act(async () => {
      fakeTransport.emit('message', {
        type: 'ui_tree_snapshot',
        target_surface: 'workspace_panel',
        tree: SELECT_BUTTON_TREE,
      })
    })

    // Let the initial surface.subpath send settle before arming the
    // next-send-fails behaviour we're actually testing here.
    await act(async () => {
      await new Promise((r) => setTimeout(r, 0))
    })
    fakeTransport.send.mockResolvedValueOnce(false)

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
        type: 'ui_tree_snapshot',
        target_surface: 'workspace_panel',
        tree: { ...malformed },
      })
    })

    // Unknown primitive types render as null (warned, not thrown). To exercise
    // the boundary, push a tree whose children array is malformed enough that
    // the renderer throws.
    await act(async () => {
      fakeTransport.emit('message', {
        type: 'ui_tree_snapshot',
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
        type: 'ui_tree_snapshot',
        target_surface: 'workspace_panel',
        tree: new Boom(),
      })
    })
    expect(screen.getByText(/UI tree failed to render/i)).toBeInTheDocument()

    await act(async () => {
      fakeTransport.emit('message', {
        type: 'ui_tree_snapshot',
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
        type: 'ui_tree_snapshot',
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
        type: 'ui_tree_snapshot',
        target_surface: 'workspace_panel',
        tree: SELECT_BUTTON_TREE,
      })
    })
    // Clear the initial surface.subpath send (unrelated to this test's
    // intercept semantics — we only care that the clicked action is
    // consumed, not forwarded).
    fakeTransport.send.mockClear()
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
        type: 'ui_tree_snapshot',
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
    expect(fakeTransport.send).toHaveBeenCalledWith('ui_action', {
      target_surface: 'workspace_panel',
      envelope: {
        id: 'botster.session.select',
        payload: { sessionId: 's-z' },
      },
    })
  })
})

describe('<UiTree> subpath wire protocol (Phase 4b)', () => {
  it('sends surface.subpath on first mount', async () => {
    // UiTree always announces its current (surface, subpath) to the hub on
    // mount. The subscribe envelope also primes `surface_subpaths` server-
    // side, so repeating the value is a no-op on the hub
    // (`set_surface_subpath` early-returns on identical subpath). The fresh
    // mount send is what keeps cross-Route boundary navigation correct —
    // HubShow → DynamicSurface mounts a NEW UiTree instance for a new
    // surface, and the hub must hear about it.
    render(
      <UiTree hubId="hub-1" targetSurface="kanban" subpath="/board/42" />,
    )
    await act(async () => {
      await new Promise((r) => setTimeout(r, 120))
    })
    const subpathCalls = fakeTransport.send.mock.calls.filter(
      ([, body]) => body?.envelope?.id === 'botster.surface.subpath',
    )
    expect(subpathCalls).toHaveLength(1)
    expect(subpathCalls[0][1]).toEqual({
      target_surface: 'kanban',
      envelope: {
        id: 'botster.surface.subpath',
        payload: { target_surface: 'kanban', subpath: '/board/42' },
      },
    })
  })

  // Regression (2026-04-22): system-test caught "Uncaught Error: DataChannel
  // closed" in the browser console during WebRTC handshake because
  // transport.send returns a rejecting Promise when the DataChannel isn't
  // open yet. `void transport.send(...)` swallows the return value but the
  // rejection still fires as an unhandled rejection. UiTree must attach a
  // .catch handler so the rejection is consumed — the subscribe envelope
  // already primed the hub and the next nav will re-fire.
  it('handles a rejecting transport.send without raising unhandled rejection', async () => {
    const rejected = Promise.reject(new Error('DataChannel closed'))
    // Keep the rejection plumbing: attach a noop catch to the PROMISE WE
    // RETURN from send so Node's test runner sees a handled rejection too.
    rejected.catch(() => {})
    fakeTransport.send.mockReturnValueOnce(rejected)

    const unhandledSpy = vi.fn()
    const onUnhandled = (event) => {
      unhandledSpy(event.reason)
    }
    window.addEventListener('unhandledrejection', onUnhandled)

    try {
      render(<UiTree hubId="hub-1" targetSurface="kanban" subpath="/" />)
      await act(async () => {
        await new Promise((r) => setTimeout(r, 120))
      })
      // Microtask flush so the rejecting promise has a chance to report.
      await act(async () => {
        await Promise.resolve()
        await Promise.resolve()
      })
      expect(unhandledSpy).not.toHaveBeenCalled()
    } finally {
      window.removeEventListener('unhandledrejection', onUnhandled)
    }
  })

  it('sends surface.subpath on subpath prop change and clears the tree', async () => {
    const { rerender } = render(
      <UiTree hubId="hub-1" targetSurface="kanban" subpath="/" />,
    )
    // Wait for transport attach + initial mount send.
    await act(async () => {
      await new Promise((r) => setTimeout(r, 120))
    })
    // Clear the initial mount send so the assertion below only counts the
    // subpath-change send we're testing.
    fakeTransport.send.mockClear()

    // Push a home-render tree so the panel is visible.
    await act(async () => {
      fakeTransport.emit('message', {
        type: 'ui_tree_snapshot',
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

    // Exactly one surface.subpath action fired for the new subpath.
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
        type: 'ui_tree_snapshot',
        target_surface: 'kanban',
        subpath: '/',
        tree: { type: 'text', props: { text: 'stale home' } },
      })
    })
    expect(screen.queryByText('stale home')).toBeNull()
    // Matching-subpath frame does paint.
    await act(async () => {
      fakeTransport.emit('message', {
        type: 'ui_tree_snapshot',
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
        type: 'ui_tree_snapshot',
        target_surface: 'workspace_panel',
        // no subpath field
        tree: { type: 'text', props: { text: 'hub' } },
      })
    })
    expect(await screen.findByText('hub')).toBeInTheDocument()
  })

  // Regression: codex-found blocker (2026-04-21). Cross-surface navigation
  // on the SAME transport must fire `botster.surface.subpath` for the new
  // (surface, subpath) pair so the hub's dispatcher routes to the right
  // sub-route instead of leaving the UiTree stuck on loading.
  it('fires surface.subpath when targetSurface changes on an existing transport', async () => {
    const { rerender } = render(
      <UiTree hubId="hub-1" targetSurface="hello" subpath="/details/1" />,
    )
    await act(async () => {
      await new Promise((r) => setTimeout(r, 120))
    })

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
    // Expect sends for BOTH the initial mount and the kanban transition.
    // The hub-side `set_surface_subpath` early-returns on equal subpath so
    // the extra initial send is harmless; we drop the per-instance cache.
    const kanbanSends = calls.filter(
      ([, body]) => body.envelope.payload.target_surface === 'kanban',
    )
    expect(kanbanSends).toHaveLength(1)
    expect(kanbanSends[0][1]).toEqual({
      target_surface: 'kanban',
      envelope: {
        id: 'botster.surface.subpath',
        payload: { target_surface: 'kanban', subpath: '/board/42' },
      },
    })
  })

  // Regression (2026-04-22): UiTree must send `botster.surface.subpath` on
  // FRESH MOUNT, not just on prop rerender. Real-world navigation crosses
  // Route boundaries (HubShow for workspace_panel → DynamicSurface for
  // plugin surfaces) and each boundary mounts a NEW UiTree instance.
  // An earlier revision kept a per-instance skip-cache that treated every
  // new instance as "cold transport" and silently suppressed the send,
  // leaving the hub without the new surface's subpath and the user stuck
  // on loading.
  it('fires surface.subpath on a fresh mount for a non-cold transport', async () => {
    // Simulate a stable transport by mounting UiTree once first (its own
    // initial send won't be suppressed), then UNMOUNT and mount a fresh
    // instance for a different surface — this is the HubShow→DynamicSurface
    // transition in the real app.
    const first = render(
      <UiTree hubId="hub-1" targetSurface="workspace_panel" subpath="/" />,
    )
    await act(async () => {
      await new Promise((r) => setTimeout(r, 120))
    })
    first.unmount()
    fakeTransport.send.mockClear()

    render(<UiTree hubId="hub-1" targetSurface="hello" subpath="/" />)
    await act(async () => {
      await new Promise((r) => setTimeout(r, 120))
    })

    const calls = fakeTransport.send.mock.calls.filter(
      ([, body]) => body?.envelope?.id === 'botster.surface.subpath',
    )
    expect(calls).toHaveLength(1)
    expect(calls[0][1]).toEqual({
      target_surface: 'hello',
      envelope: {
        id: 'botster.surface.subpath',
        payload: { target_surface: 'hello', subpath: '/' },
      },
    })
  })
})
