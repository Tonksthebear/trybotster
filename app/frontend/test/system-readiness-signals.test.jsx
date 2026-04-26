import React from 'react'
import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest'
import { act, cleanup, render } from '@testing-library/react'
import { MemoryRouter } from 'react-router-dom'

import {
  _mapConnectionStateToCliStatusForTests as mapCliStatus,
  _RootReadinessSignalsForTests as RootReadinessSignals,
} from '../components/AppShell'

import { useHubStore } from '../store/hub-store'
import { useRouteRegistryStore } from '../store/route-registry-store'
import {
  useSurfaceReadinessStore,
  resetSurfaceReadinessStoreForTest,
} from '../store/surface-readiness-store'

import * as hubBridge from '../lib/hub-bridge'
import UiTree from '../components/UiTree'

// The ui_contract interpreter is imported transitively by UiTree. Mock the
// actions module (UiTree's only external dependency from ../lib/actions)
// the same way ui-tree.test.jsx does so we can focus on the readiness
// wrapper's DOM attributes.
vi.mock('../lib/actions', () => ({
  ACTION: { SESSION_SELECT: 'botster.session.select' },
  safeUrl: (u) => u ?? null,
  dispatch: vi.fn(),
}))

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

const HELLO_TREE = {
  type: 'stack',
  props: { direction: 'vertical' },
  children: [{ type: 'text', props: { text: 'hello' } }],
}

// ────────────────────────────────────────────────────────────────────────
// mapConnectionStateToCliStatus — pure function, no DOM needed
// ────────────────────────────────────────────────────────────────────────

describe('mapConnectionStateToCliStatus', () => {
  it('returns unknown when no hub is selected', () => {
    expect(mapCliStatus('connected', null)).toBe('unknown')
    expect(mapCliStatus('disconnected', null)).toBe('unknown')
    expect(mapCliStatus(undefined, null)).toBe('unknown')
  })

  it('maps connected → connected', () => {
    expect(mapCliStatus('connected', '42')).toBe('connected')
  })

  it('maps connecting and pairing_needed → handshaking', () => {
    expect(mapCliStatus('connecting', '42')).toBe('handshaking')
    expect(mapCliStatus('pairing_needed', '42')).toBe('handshaking')
  })

  it('maps disconnected and error → offline', () => {
    expect(mapCliStatus('disconnected', '42')).toBe('offline')
    expect(mapCliStatus('error', '42')).toBe('offline')
  })

  it('falls back to unknown for unexpected states', () => {
    expect(mapCliStatus('weird_state', '42')).toBe('unknown')
  })
})

// ────────────────────────────────────────────────────────────────────────
// RootReadinessSignals — writes to <html data-cli-status> / data-hub-snapshot
//
// We mount the full AppRoutes-less shell: just <RootReadinessSignals /> is
// enough since it's a rendering null component that only has side effects.
// Importing AppRoutes pulls lazy-loaded pages we don't need.
// ────────────────────────────────────────────────────────────────────────

describe('html data-cli-status', () => {
  beforeEach(() => {
    // Reset the html dataset between tests
    delete document.documentElement.dataset.cliStatus
    delete document.documentElement.dataset.hubSnapshot

    useHubStore.setState({
      hubList: [],
      hubListLoading: false,
      selectedHubId: null,
      connectionState: 'disconnected',
      connectionDetail: '',
      _connectionRef: null,
      _statusUnsub: null,
      fetchHubList: vi.fn(async () => []),
      selectHub: vi.fn(async () => {}),
      disconnectHub: vi.fn(),
      getLastHubId: vi.fn(() => null),
    })
  })

  afterEach(() => {
    cleanup()
    vi.restoreAllMocks()
  })

  // Rather than mount the whole AppShell (which has lazy routes and wants
  // a real hub connection), we mount just the RootReadinessSignals island
  // by hand. Using React's lazy machinery in unit tests is fragile, and
  // the island's only input is two zustand stores + one document side-
  // effect that we can exercise directly.
  function mountOnlyRootReadinessSignals() {
    return render(
      <MemoryRouter>
        <RootReadinessSignals />
      </MemoryRouter>,
    )
  }

  it('starts unknown when no hub is selected', () => {
    mountOnlyRootReadinessSignals()
    expect(document.documentElement.dataset.cliStatus).toBe('unknown')
  })

  it('transitions through handshaking to connected as state changes', async () => {
    mountOnlyRootReadinessSignals()

    act(() => {
      useHubStore.setState({
        selectedHubId: '42',
        connectionState: 'connecting',
      })
    })
    expect(document.documentElement.dataset.cliStatus).toBe('handshaking')

    act(() => {
      useHubStore.setState({ connectionState: 'connected' })
    })
    expect(document.documentElement.dataset.cliStatus).toBe('connected')

    act(() => {
      useHubStore.setState({ connectionState: 'disconnected' })
    })
    expect(document.documentElement.dataset.cliStatus).toBe('offline')
  })
})

describe('html data-hub-snapshot', () => {
  beforeEach(() => {
    delete document.documentElement.dataset.cliStatus
    delete document.documentElement.dataset.hubSnapshot
    resetSurfaceReadinessStoreForTest()
    useRouteRegistryStore.setState({
      routesByHubId: {},
      snapshotReceivedAtByHubId: {},
    })
    useHubStore.setState({
      selectedHubId: null,
      connectionState: 'disconnected',
    })
  })

  afterEach(() => {
    cleanup()
    vi.restoreAllMocks()
  })

  function mountOnlyRootReadinessSignals() {
    return render(
      <MemoryRouter>
        <RootReadinessSignals />
      </MemoryRouter>,
    )
  }

  it('stays pending until both preconditions land', () => {
    mountOnlyRootReadinessSignals()
    act(() => {
      useHubStore.setState({ selectedHubId: '42' })
    })
    expect(document.documentElement.dataset.hubSnapshot).toBe('pending')

    // Route registry only → still pending.
    act(() => {
      useRouteRegistryStore.getState().setRoutes('42', [])
    })
    expect(document.documentElement.dataset.hubSnapshot).toBe('pending')

    // Surface readiness only (reset registry to prove the AND) → still pending.
    act(() => {
      useRouteRegistryStore.setState({
        routesByHubId: {},
        snapshotReceivedAtByHubId: {},
      })
      useSurfaceReadinessStore
        .getState()
        .recordFirstTree('42', 'workspace_panel')
    })
    expect(document.documentElement.dataset.hubSnapshot).toBe('pending')

    // BOTH land → flips to received.
    act(() => {
      useRouteRegistryStore.getState().setRoutes('42', [])
    })
    expect(document.documentElement.dataset.hubSnapshot).toBe('received')
  })

  it('reverts to pending on hub switch and re-arms for the new hub', () => {
    mountOnlyRootReadinessSignals()

    act(() => {
      useHubStore.setState({ selectedHubId: '42' })
      useRouteRegistryStore.getState().setRoutes('42', [])
      useSurfaceReadinessStore
        .getState()
        .recordFirstTree('42', 'workspace_panel')
    })
    expect(document.documentElement.dataset.hubSnapshot).toBe('received')

    act(() => {
      useHubStore.setState({ selectedHubId: '99' })
    })
    expect(document.documentElement.dataset.hubSnapshot).toBe('pending')

    act(() => {
      useRouteRegistryStore.getState().setRoutes('99', [])
      useSurfaceReadinessStore
        .getState()
        .recordFirstTree('99', 'workspace_panel')
    })
    expect(document.documentElement.dataset.hubSnapshot).toBe('received')
  })
})

// ────────────────────────────────────────────────────────────────────────
// UiTree data-surface-ready wrapper
// ────────────────────────────────────────────────────────────────────────

describe('UiTree [data-surface-ready]', () => {
  let fakeTransport

  beforeEach(() => {
    resetSurfaceReadinessStoreForTest()
    fakeTransport = new FakeTransport()
    vi.spyOn(hubBridge, 'waitForHub').mockImplementation(() => ({
      then(resolve) {
        resolve({ transport: fakeTransport })
        return Promise.resolve({ transport: fakeTransport })
      },
    }))
  })

  afterEach(() => {
    cleanup()
    vi.restoreAllMocks()
  })

  it('starts loading and flips to ready when the first frame lands', async () => {
    const { container } = render(
      <UiTree hubId="42" targetSurface="workspace_panel" />,
    )

    const wrapper = container.querySelector(
      '[data-surface-ready="workspace_panel"]',
    )
    expect(wrapper).not.toBeNull()
    expect(wrapper.dataset.surfaceReadyState).toBe('loading')

    await act(async () => {
      fakeTransport.emit('message', {
        type: 'ui_tree_snapshot',
        target_surface: 'workspace_panel',
        tree: HELLO_TREE,
      })
      // Let the transport-acquisition useEffect poll complete.
      await Promise.resolve()
    })

    const wrapperAfter = container.querySelector(
      '[data-surface-ready="workspace_panel"]',
    )
    expect(wrapperAfter.dataset.surfaceReadyState).toBe('ready')
    // Also: recordFirstTree was called.
    expect(
      useSurfaceReadinessStore
        .getState()
        .surfacesByHubId['42']?.has('workspace_panel'),
    ).toBe(true)
  })

  it('reverts to loading when targetSurface changes', async () => {
    const { container, rerender } = render(
      <UiTree hubId="42" targetSurface="workspace_panel" />,
    )

    await act(async () => {
      fakeTransport.emit('message', {
        type: 'ui_tree_snapshot',
        target_surface: 'workspace_panel',
        tree: HELLO_TREE,
      })
      await Promise.resolve()
    })
    expect(
      container
        .querySelector('[data-surface-ready="workspace_panel"]')
        .dataset.surfaceReadyState,
    ).toBe('ready')

    rerender(<UiTree hubId="42" targetSurface="kanban" />)
    expect(
      container.querySelector('[data-surface-ready="kanban"]').dataset
        .surfaceReadyState,
    ).toBe('loading')
  })
})
