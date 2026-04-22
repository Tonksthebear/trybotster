import { describe, it, expect, beforeEach } from 'vitest'
import {
  matchSurfaceForPath,
  useRouteRegistryStore,
  selectRoutesForHub,
} from '../store/route-registry-store'

describe('matchSurfaceForPath', () => {
  const entries = [
    {
      surface: 'kanban',
      path: '/kanban',
      base_path: '/kanban',
      routes: [{ path: '/' }, { path: '/board/:id' }, { path: '/settings' }],
    },
    {
      surface: 'workspace_panel',
      path: '/',
      base_path: '/',
      label: 'Hub',
    },
    {
      surface: 'admin',
      path: '/admin',
      base_path: '/admin',
    },
    {
      surface: 'admin_users',
      path: '/admin/users',
      base_path: '/admin/users',
    },
  ]

  it('returns null for non-matching paths', () => {
    expect(matchSurfaceForPath(entries, '/nope')).toBeNull()
    expect(matchSurfaceForPath(entries, '/kanbana')).toBeNull()
  })

  it('surface root returns subpath "/"', () => {
    const m = matchSurfaceForPath(entries, '/kanban')
    expect(m?.entry.surface).toBe('kanban')
    expect(m?.subpath).toBe('/')
  })

  it('nested path yields the trailing subpath', () => {
    const m = matchSurfaceForPath(entries, '/kanban/board/42')
    expect(m?.entry.surface).toBe('kanban')
    expect(m?.subpath).toBe('/board/42')
  })

  it('longest base_path wins when multiple match', () => {
    const m = matchSurfaceForPath(entries, '/admin/users/bob')
    expect(m?.entry.surface).toBe('admin_users')
    expect(m?.subpath).toBe('/bob')
  })

  it('root `/` entry matches only "/" literally', () => {
    const m = matchSurfaceForPath(entries, '/')
    expect(m?.entry.surface).toBe('workspace_panel')
    expect(m?.subpath).toBe('/')
    // "/kanban" must NOT be absorbed by the root entry.
    const m2 = matchSurfaceForPath(entries, '/kanban/whatever')
    expect(m2?.entry.surface).toBe('kanban')
  })

  it('handles non-string / non-array inputs gracefully', () => {
    expect(matchSurfaceForPath(null, '/foo')).toBeNull()
    expect(matchSurfaceForPath(entries, null)).toBeNull()
  })
})

describe('route-registry-store normalisation', () => {
  beforeEach(() => {
    useRouteRegistryStore.setState({
      routesByHubId: {},
      snapshotReceivedAtByHubId: {},
    })
  })

  it('assigns base_path from legacy `path` when base_path is absent', () => {
    // Old hub still shipping only `path`. Store should fill in base_path so
    // consumers don't have to branch on schema version.
    useRouteRegistryStore.getState().setRoutes('h1', [
      { path: '/plugins/hello', surface: 'hello' },
    ])
    const routes = selectRoutesForHub(useRouteRegistryStore.getState(), 'h1')
    expect(routes[0].base_path).toBe('/plugins/hello')
  })

  it('preserves explicit base_path when supplied', () => {
    useRouteRegistryStore.getState().setRoutes('h1', [
      { path: '/kanban', base_path: '/kanban', surface: 'kanban' },
    ])
    const routes = selectRoutesForHub(useRouteRegistryStore.getState(), 'h1')
    expect(routes[0].base_path).toBe('/kanban')
  })
})
