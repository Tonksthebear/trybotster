/**
 * Bridge between React action CustomEvents and Catalyst dialog components.
 *
 * The action system (actions.js) dispatches document-level CustomEvents for
 * rename, move, and delete. This module registers listeners ONCE at import
 * time and opens the corresponding dialog via the zustand dialog store.
 *
 * Imported from application.jsx so it runs once at boot.
 */
import { useDialogStore } from '../store/dialog-store'

let activeHubId = null

/**
 * Set the active hub ID. Called by each App component on mount.
 * All instances on the same page share the same hub, so last-write-wins is fine.
 */
export function setHubId(hubId) {
  activeHubId = hubId
}

function handleRename(e) {
  if (!activeHubId) return
  const { workspaceId, title } = e.detail
  useDialogStore.getState().openRename({ workspaceId, title })
}

function handleMove(e) {
  if (!activeHubId) return
  const { sessionId, sessionUuid } = e.detail
  useDialogStore.getState().openMove({ sessionId, sessionUuid })
}

function handleDelete(e) {
  if (!activeHubId) return
  const { sessionId, sessionUuid } = e.detail
  useDialogStore.getState().openDelete({ sessionId, sessionUuid })
}

// Register once at module evaluation — never removed.
document.addEventListener('botster:workspace:rename', handleRename)
document.addEventListener('botster:session:move', handleMove)
document.addEventListener('botster:session:delete', handleDelete)
