import React from 'react'
import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest'
import { render, screen, cleanup } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router-dom'
import { useRouteRegistryStore } from '../store/route-registry-store'

// UiTree is the hub-subscribing mount. For this unit test we don't want to
// exercise real subscription / transport code — we just want to assert that
// DynamicSurfaceRoute passes the right `targetSurface` through.
vi.mock('../components/UiTree', () => ({
  default: ({ hubId, targetSurface, children }) => (
    <div data-testid="ui-tree">
      <span>{`hubId=${hubId}`}</span>
      <span>{`targetSurface=${targetSurface}`}</span>
      {children}
    </div>
  ),
}))

vi.mock('../components/workspace/SessionActionsMenu', () => ({
  default: () => <div data-testid="session-actions-menu" />,
}))

// eslint-disable-next-line import/first
import DynamicSurfaceRoute from '../components/pages/DynamicSurface'

function renderDynamic(path) {
  return render(
    <MemoryRouter initialEntries={[path]}>
      <Routes>
        <Route path="/hubs/:hubId/*" element={<DynamicSurfaceRoute />} />
      </Routes>
    </MemoryRouter>
  )
}

describe('DynamicSurfaceRoute', () => {
  beforeEach(() => {
    // Reset registry store between tests so each test owns its routes.
    useRouteRegistryStore.setState({
      routesByHubId: {},
      snapshotReceivedAtByHubId: {},
    })
  })

  afterEach(() => {
    cleanup()
    useRouteRegistryStore.setState({
      routesByHubId: {},
      snapshotReceivedAtByHubId: {},
    })
  })

  it('mounts UiTree with the surface matching the requested path', () => {
    useRouteRegistryStore.getState().setRoutes('h1', [
      { path: '/plugins/hello', surface: 'hello', label: 'Hello' },
    ])
    renderDynamic('/hubs/h1/plugins/hello')

    const tree = screen.getByTestId('ui-tree')
    expect(tree).toHaveTextContent('hubId=h1')
    expect(tree).toHaveTextContent('targetSurface=hello')
  })

  it('renders a 404 fallback when the path is not in the registry', () => {
    useRouteRegistryStore.getState().setRoutes('h1', [
      { path: '/plugins/hello', surface: 'hello', label: 'Hello' },
    ])
    renderDynamic('/hubs/h1/plugins/missing')

    expect(screen.getByText(/Not found/i)).toBeInTheDocument()
    expect(screen.queryByTestId('ui-tree')).toBeNull()
  })

  it('defers to the legacy session route by rendering nothing for sessions/*', () => {
    useRouteRegistryStore.getState().setRoutes('h1', [
      { path: '/', surface: 'workspace_panel', label: 'Hub' },
    ])
    const { container } = renderDynamic('/hubs/h1/sessions/some-session-uuid')

    // The component returns null so the splat branch leaves no UiTree and
    // no 404 in its subtree. The MemoryRouter wrapper still exists — the
    // component's *own* output should be empty.
    expect(screen.queryByTestId('ui-tree')).toBeNull()
    expect(screen.queryByText(/Not found/i)).toBeNull()
    expect(container.textContent?.trim() ?? '').toBe('')
  })

  it('matches the root path for the workspace_panel surface', () => {
    useRouteRegistryStore.getState().setRoutes('h1', [
      { path: '/', surface: 'workspace_panel', label: 'Hub' },
    ])
    renderDynamic('/hubs/h1/')

    expect(screen.getByTestId('ui-tree')).toHaveTextContent(
      'targetSurface=workspace_panel'
    )
  })

  // F4: distinguish "registry hasn't arrived yet" from "true 404".
  it('renders the loading state while the registry snapshot is unresolved', () => {
    // Deliberately do NOT call setRoutes — the hub is still connecting
    // and the first ui_route_registry_v1 frame hasn't arrived.
    renderDynamic('/hubs/brand-new-hub/plugins/hello')

    // Loading placeholder, NOT 404.
    expect(screen.getByText(/Loading/i)).toBeInTheDocument()
    expect(screen.queryByText(/Not found/i)).toBeNull()
    expect(screen.queryByTestId('ui-tree')).toBeNull()
  })

  it('transitions from loading to matched surface when the snapshot arrives', () => {
    const { rerender } = renderDynamic('/hubs/h1/plugins/hello')
    expect(screen.getByText(/Loading/i)).toBeInTheDocument()

    // First frame arrives.
    useRouteRegistryStore.getState().setRoutes('h1', [
      { path: '/plugins/hello', surface: 'hello', label: 'Hello' },
    ])
    rerender(
      <MemoryRouter initialEntries={['/hubs/h1/plugins/hello']}>
        <Routes>
          <Route path="/hubs/:hubId/*" element={<DynamicSurfaceRoute />} />
        </Routes>
      </MemoryRouter>
    )
    expect(screen.getByTestId('ui-tree')).toHaveTextContent('targetSurface=hello')
  })

  it('transitions from loading to 404 when the snapshot confirms no such path', () => {
    const { rerender } = renderDynamic('/hubs/h1/plugins/nope')
    expect(screen.getByText(/Loading/i)).toBeInTheDocument()

    // First frame arrives — but `/plugins/nope` isn't in it.
    useRouteRegistryStore.getState().setRoutes('h1', [
      { path: '/plugins/hello', surface: 'hello', label: 'Hello' },
    ])
    rerender(
      <MemoryRouter initialEntries={['/hubs/h1/plugins/nope']}>
        <Routes>
          <Route path="/hubs/:hubId/*" element={<DynamicSurfaceRoute />} />
        </Routes>
      </MemoryRouter>
    )
    expect(screen.getByText(/Not found/i)).toBeInTheDocument()
  })

  it('treats an explicit empty-array snapshot as resolved (true 404, not loading)', () => {
    // The hub may ship an empty routes array (no routable surfaces yet);
    // that's still a "snapshot received" event and should 404, not loop
    // forever on "loading".
    useRouteRegistryStore.getState().setRoutes('h1', [])
    renderDynamic('/hubs/h1/anywhere')

    expect(screen.getByText(/Not found/i)).toBeInTheDocument()
    expect(screen.queryByText(/Loading/i)).toBeNull()
  })
})
