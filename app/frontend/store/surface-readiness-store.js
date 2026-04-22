import { create } from 'zustand'

// Per-hub, per-surface record of "first ui_layout_tree_v1 frame landed".
// Feeds the root-level `<html data-hub-snapshot>` signal (in AppShell) and
// is the second precondition alongside the route-registry snapshot.
//
// Deliberately a separate store from `route-registry-store`: the two
// preconditions have independent sources (route registry is a single
// per-hub broadcast, surface readiness is per-surface, published by each
// `UiTree` mount) and keeping them decoupled avoids coupling the
// registry's normalisation logic to UiTree's frame loop.
//
// Shape:
//   surfacesByHubId: { [hubId]: Set<targetSurface> }
//
// `recordFirstTree` is idempotent: the first call per (hub, surface)
// mutates; subsequent calls with the same pair are no-ops (identity
// preserved so zustand subscribers don't re-render on repeat frames).

export const useSurfaceReadinessStore = create((set, get) => ({
  surfacesByHubId: {},

  recordFirstTree(hubId, surface) {
    if (!hubId || !surface) return
    const key = String(hubId)
    const existing = get().surfacesByHubId[key]
    if (existing && existing.has(surface)) return
    const next = new Set(existing ?? [])
    next.add(surface)
    set((s) => ({
      surfacesByHubId: { ...s.surfacesByHubId, [key]: next },
    }))
  },

  clearForHub(hubId) {
    if (!hubId) return
    const key = String(hubId)
    set((s) => {
      if (!(key in s.surfacesByHubId)) return s
      const copy = { ...s.surfacesByHubId }
      delete copy[key]
      return { surfacesByHubId: copy }
    })
  },
}))

export function selectHasAnySurfaceForHub(state, hubId) {
  if (!hubId) return false
  const entry = state.surfacesByHubId[String(hubId)]
  return !!entry && entry.size > 0
}

export function resetSurfaceReadinessStoreForTest() {
  useSurfaceReadinessStore.setState({ surfacesByHubId: {} })
}
