import React from 'react'
import { useParams } from 'react-router-dom'
import UiTree from '../UiTree'
import SessionActionsMenu from '../workspace/SessionActionsMenu'
import {
  matchSurfaceForPath,
  useRouteRegistryStore,
  selectRoutesForHub,
  selectHasRouteRegistrySnapshot,
} from '../../store/route-registry-store'

/**
 * Phase 4a/4b: dynamic hub-authored surface route.
 *
 * Matches `useParams()`'s splat (`*`) against the hub's
 * `ui_route_registry` entries using `matchSurfaceForPath`. The match is
 * **prefix-scoped to `base_path`**, not an exact-path compare: the first
 * hub-relative segment identifies the surface, and everything after it is
 * the **subpath** (sub-route) handed to `<UiTree>` so the hub dispatcher
 * can route to the correct sub-page. A plugin that registers
 * `surfaces.register("kanban", { routes = { { path = "/" }, { path =
 * "/board/:id" } } })` becomes reachable at
 *   * `/hubs/:hubId/kanban`            → subpath "/"            → home
 *   * `/hubs/:hubId/kanban/board/42`   → subpath "/board/42"    → board(id=42)
 *
 * Phase 4a's exact-match form (`path === requestedPath`) is superseded by
 * the prefix form, which now covers both the root-of-surface case and any
 * nested sub-path. `base_path` always falls back to `path` for backwards
 * compat with older hubs.
 *
 * Three resolution states for the current URL:
 *   1. Registry hasn't shipped its first snapshot yet for this hub
 *      (cold deep-link, hub still connecting) → loading state.
 *   2. Snapshot received, path matches a registered surface's base_path →
 *      mount `<UiTree>` for that surface with the extracted subpath.
 *   3. Snapshot received, no match → true 404.
 *
 * Distinguishing (1) from (3) avoids the "flash of 404" that would
 * otherwise show whenever a user types / bookmarks a plugin URL and
 * hits it before the hub broadcasts the registry.
 */
export default function DynamicSurfaceRoute() {
  const { hubId, '*': splat } = useParams()
  const routes = useRouteRegistryStore((s) => selectRoutesForHub(s, hubId))
  const hasSnapshot = useRouteRegistryStore((s) =>
    selectHasRouteRegistrySnapshot(s, hubId),
  )

  // Belt-and-suspenders: `/sessions/*` is handled by `AppShell`'s
  // `TerminalCache` branch, NOT by a registered surface. React Router
  // matches specific routes before the wildcard in Router v6, so this
  // check only fires on rare misroutes (e.g. a session-like URL that
  // isn't actually a real session). Defer to the legacy fallback by
  // rendering nothing; AppShell's own detection takes over.
  if (typeof splat === 'string' && splat.startsWith('sessions/')) {
    return null
  }

  // The routes registry is keyed on paths that are hub-scoped (e.g. "/",
  // "/plugins/hello"). React Router's splat gives us the path WITHOUT a
  // leading slash for nested routes. Normalise before comparing.
  const requestedPath = '/' + (splat ?? '')

  // Unresolved: registry frame hasn't arrived yet. Render a loading
  // placeholder (visually distinct from the 404 state) instead of
  // flashing "Not found" while the hub is still connecting.
  if (!hasSnapshot) {
    return (
      <div className="h-full flex items-center justify-center p-4">
        <div className="text-sm text-zinc-500">Loading…</div>
      </div>
    )
  }

  const match = matchSurfaceForPath(routes, requestedPath)

  if (!match) {
    // Unknown path — render a minimal local 404. Phase 4a intentionally
    // skips hub-authored 404 surfaces (the orchestrator flagged that as
    // optional); a plugin can still register one at path "/404" if it
    // wants consistent chrome.
    return (
      <div className="h-full flex items-center justify-center p-4">
        <div className="max-w-md text-center">
          <h1 className="text-lg font-semibold text-zinc-200">
            Not found
          </h1>
          <p className="mt-2 text-sm text-zinc-500">
            No surface is registered for{' '}
            <code className="rounded bg-zinc-800 px-1 py-0.5 font-mono text-xs text-zinc-300">
              {requestedPath}
            </code>
            .
          </p>
        </div>
      </div>
    )
  }

  return (
    <div className="h-full overflow-y-auto p-4 lg:p-6">
      <UiTree
        hubId={hubId}
        targetSurface={match.entry.surface}
        subpath={match.subpath}
      >
        <SessionActionsMenu />
      </UiTree>
    </div>
  )
}
