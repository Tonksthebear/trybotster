// Wire protocol — client-only UI presentation state.
//
// Anything the user controls in the browser session (selection, collapse
// state, scroll positions, modal-open flags) lives here, NOT on the wire
// and NOT in the entity stores. The hub never reads this store; it survives
// hub reconnects but resets on browser reload.
//
// When the legacy `workspace-store.js` retires (commit 8), every selector
// it exposed (`displayName`, `selectSessionRowProps`, …) re-points at the
// new entity stores plus this presentation store.

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

  /** Test-only — reset to defaults. */
  _reset() {
    set({
      selectedSessionId: null,
      collapsedWorkspaceIds: new Set(),
      densityOverride: undefined,
    })
  },
}))
