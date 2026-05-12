// Wire protocol — client-only UI presentation state.
//
// Anything the user controls in the browser session (selection, collapse
// state, scroll positions, modal-open flags) lives here, NOT on the wire
// and NOT in the entity stores. The hub never reads this store; it survives
// hub reconnects but resets on browser reload.
//
// Row/display selectors compose the wire entity stores with this
// presentation store so local browser state stays out of hub snapshots.

import { create } from 'zustand'

export const useUiPresentationStore = create((set, get) => ({
  /** Currently focused session_uuid in the local browser. Browser-local —
   *  a click in client A does NOT flip the row in client B. */
  selectedSessionId: null,
  /** Set of workspace ids currently collapsed in the session_list view. */
  collapsedWorkspaceIds: new Set(),
  /** Surface-density override (when an author wants to flip from sidebar
   *  to panel without a layout reload). Undefined means "use the surface's
   *  declared density". */
  densityOverride: undefined,
  /** Browser-local values keyed by hub/surface/key. Used for ephemeral
   * plugin-surface state such as dialog open flags. */
  localValues: {},

  setSelectedSessionId(id) {
    set({ selectedSessionId: id })
  },

  toggleWorkspaceCollapsed(workspaceId) {
    const next = new Set(get().collapsedWorkspaceIds)
    if (next.has(workspaceId)) {
      next.delete(workspaceId)
    } else {
      next.add(workspaceId)
    }
    set({ collapsedWorkspaceIds: next })
  },

  setWorkspaceCollapsed(workspaceId, collapsed) {
    const current = get().collapsedWorkspaceIds
    const isCollapsed = current.has(workspaceId)
    if (isCollapsed === collapsed) return
    const next = new Set(current)
    if (collapsed) {
      next.add(workspaceId)
    } else {
      next.delete(workspaceId)
    }
    set({ collapsedWorkspaceIds: next })
  },

  setDensityOverride(value) {
    set({ densityOverride: value })
  },

  localKey(hubId, targetSurface, key) {
    return `${hubId || ''}:${targetSurface || ''}:${key || ''}`
  },

  localValue(hubId, targetSurface, key, fallback = null) {
    const scopedKey = get().localKey(hubId, targetSurface, key)
    return Object.prototype.hasOwnProperty.call(get().localValues, scopedKey)
      ? get().localValues[scopedKey]
      : fallback
  },

  setLocalValue(hubId, targetSurface, key, value) {
    if (!key) return
    const scopedKey = get().localKey(hubId, targetSurface, key)
    set((state) => ({
      localValues: {
        ...state.localValues,
        [scopedKey]: value,
      },
    }))
  },

  clearLocalValue(hubId, targetSurface, key) {
    if (!key) return
    const scopedKey = get().localKey(hubId, targetSurface, key)
    if (!Object.prototype.hasOwnProperty.call(get().localValues, scopedKey)) return
    set((state) => {
      const next = { ...state.localValues }
      delete next[scopedKey]
      return { localValues: next }
    })
  },

  toggleLocalValue(hubId, targetSurface, key, fallback = false) {
    if (!key) return
    const current = get().localValue(hubId, targetSurface, key, fallback)
    get().setLocalValue(hubId, targetSurface, key, !current)
  },

  /** Test-only — reset to defaults. */
  _reset() {
    set({
      selectedSessionId: null,
      collapsedWorkspaceIds: new Set(),
      densityOverride: undefined,
      localValues: {},
    })
  },
}))
