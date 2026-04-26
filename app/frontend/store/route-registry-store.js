import { create } from 'zustand'

// Per-hub registry of routable browser surfaces. Populated from
// the `ui_route_registry` broadcast (see `lib/connections/hub_connection.js`
// and the bridge in `lib/hub-bridge.js`). Components subscribe via the
// `useRouteRegistryStore` hook; `DynamicSurfaceRoute` matches the current
// URL's first hub-scoped segment against an entry's `base_path` to decide
// which hub-authored surface to mount.
//
// Shape:
//   routesByHubId: {
//     [hubId]: Array<{
//       path,              // canonical URL path for the surface root
//       base_path,         // matches `path`; explicit for sub-route slicing
//       surface,           // wire target_surface identifier
//       label,
//       icon,
//       hide_from_nav?,
//       routes?,           // sub-patterns: [{ path }]
//     }>,
//   }
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
    const next = Array.isArray(routes) ? routes.map(normaliseEntry) : []
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

/**
 * Normalise an entry so consumers can rely on `base_path`. The current wire
 * format emits `base_path`; `path` remains the canonical surface root and is
 * used as the derived `base_path` when the field is absent.
 */
function normaliseEntry(entry) {
  if (!entry || typeof entry !== 'object') return entry
  const basePath = typeof entry.base_path === 'string' && entry.base_path !== ''
    ? entry.base_path
    : typeof entry.path === 'string' ? entry.path : null
  return {
    ...entry,
    base_path: basePath,
  }
}

/**
 * Given a set of normalised registry entries and a hub-relative URL (e.g.
 * "/kanban/board/42"), return the matching entry AND the remaining subpath
 * (e.g. "/board/42"). Preference order:
 *   1. Exact base_path match with no additional segments ("/kanban" vs
 *      entry.base_path "/kanban") → subpath "/".
 *   2. Prefix match where the character after the base is "/" (so
 *      "/kanban/board/42" matches base "/kanban" but "/kanbana" doesn't
 *      match base "/kanban").
 *   3. Root base_path ("/") matches only the literal root.
 *
 * When multiple entries' base_paths are prefixes of one another (e.g.
 * "/admin" vs "/admin/users"), the longer base wins.
 */
export function matchSurfaceForPath(entries, pathname) {
  if (!Array.isArray(entries) || typeof pathname !== 'string') {
    return null
  }
  const normalised = pathname.length === 0 ? '/' : pathname

  let best = null
  let bestLen = -1
  for (const entry of entries) {
    const base = entry?.base_path
    if (typeof base !== 'string' || base.length === 0) continue

    let subpath = null
    if (base === '/') {
      if (normalised === '/' || normalised === '') {
        subpath = '/'
      }
    } else if (normalised === base) {
      subpath = '/'
    } else if (normalised.startsWith(base + '/')) {
      subpath = normalised.slice(base.length)
    }

    if (subpath !== null && base.length > bestLen) {
      best = { entry, subpath }
      bestLen = base.length
    }
  }
  return best
}

/** Selector: return the routes for a given hub id, or an empty array. */
export function selectRoutesForHub(state, hubId) {
  if (!hubId) return EMPTY_ROUTES
  return state.routesByHubId[String(hubId)] ?? EMPTY_ROUTES
}

/** Selector: has this hub shipped its first `ui_route_registry` frame?
 *  Consumers should render a loading state while this is false and fall
 *  back to the 404 / no-match branch only after it turns true. */
export function selectHasRouteRegistrySnapshot(state, hubId) {
  if (!hubId) return false
  return state.snapshotReceivedAtByHubId[String(hubId)] !== undefined
}
