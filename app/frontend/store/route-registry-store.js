import { create } from 'zustand'

// Phase 4a: per-hub registry of routable browser surfaces. Populated from
// the `ui_route_registry_v1` broadcast (see `lib/connections/hub_connection.js`
// and the bridge in `lib/hub-bridge.js`). Components subscribe via the
// `useRouteRegistryStore` hook; `DynamicSurfaceRoute` matches the current
// URL splat against an entry's `path` to decide which hub-authored surface
// to mount.
//
// Shape:
//   routesByHubId: { [hubId]: Array<{ path, surface, label, icon, hide_from_nav? }> }
//   snapshotReceivedAtByHubId: { [hubId]: number }
//
// Routes are replaced wholesale per frame so the hub always owns the
// source of truth; local mutation is never correct. `snapshotReceivedAt`
// distinguishes "first broadcast hasn't arrived yet" (undefined) from
// "registry says this path is not routable" (timestamp present, path
// absent from the array). `DynamicSurfaceRoute` renders a loading state
// before the first snapshot and a true 404 after.

export const useRouteRegistryStore = create((set) => ({
  routesByHubId: {},
  snapshotReceivedAtByHubId: {},

  setRoutes(hubId, routes) {
    if (!hubId) return
    const next = Array.isArray(routes) ? routes : []
    set((s) => ({
      routesByHubId: { ...s.routesByHubId, [String(hubId)]: next },
      snapshotReceivedAtByHubId: {
        ...s.snapshotReceivedAtByHubId,
        [String(hubId)]: Date.now(),
      },
    }))
  },

  clearRoutes(hubId) {
    if (!hubId) return
    set((s) => {
      const routesCopy = { ...s.routesByHubId }
      const snapshotCopy = { ...s.snapshotReceivedAtByHubId }
      let changed = false
      if (String(hubId) in routesCopy) {
        delete routesCopy[String(hubId)]
        changed = true
      }
      if (String(hubId) in snapshotCopy) {
        delete snapshotCopy[String(hubId)]
        changed = true
      }
      if (!changed) return s
      return {
        routesByHubId: routesCopy,
        snapshotReceivedAtByHubId: snapshotCopy,
      }
    })
  },
}))

// Stable empty-array singleton. Zustand subscribes components via
// `Object.is` comparison; returning a fresh `[]` per selector call would
// look like a new value every render and trigger React's update-depth
// protection. A frozen module-level constant is safe here because the
// selector never mutates.
const EMPTY_ROUTES = Object.freeze([])

/** Selector: return the routes for a given hub id, or an empty array. */
export function selectRoutesForHub(state, hubId) {
  if (!hubId) return EMPTY_ROUTES
  return state.routesByHubId[String(hubId)] ?? EMPTY_ROUTES
}

/** Selector: has this hub shipped its first `ui_route_registry_v1` frame?
 *  Consumers should render a loading state while this is false and fall
 *  back to the 404 / no-match branch only after it turns true. */
export function selectHasRouteRegistrySnapshot(state, hubId) {
  if (!hubId) return false
  return state.snapshotReceivedAtByHubId[String(hubId)] !== undefined
}
