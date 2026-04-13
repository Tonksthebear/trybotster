/**
 * Singleton bridge between React action CustomEvents and existing Rails modals.
 *
 * The action system (actions.js) dispatches document-level CustomEvents for
 * rename, move, and delete. This module registers listeners ONCE at import
 * time to handle them — opening prompts or Rails dialog modals as needed.
 *
 * Imported from application.jsx so it runs once at boot, regardless of how
 * many App component instances mount (sidebar desktop, sidebar mobile, panel).
 */
import { getHub } from './hub-bridge'
import { useWorkspaceStore } from '../store/workspace-store'

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
  const hub = getHub(activeHubId)
  if (!hub) return
  const { workspaceId, title } = e.detail
  const input = window.prompt('Rename workspace:', title)
  if (input === null) return
  const newName = input.trim()
  if (!newName || newName === title) return
  hub.renameWorkspace(workspaceId, newName)
}

function handleMove(e) {
  if (!activeHubId) return
  const hub = getHub(activeHubId)
  if (!hub) return
  const { sessionId } = e.detail
  const store = useWorkspaceStore.getState()
  const workspaces = Object.values(store.workspacesById)
  const currentWs = workspaces.find(
    (ws) => Array.isArray(ws?.agents) && ws.agents.includes(sessionId)
  )
  const names = workspaces
    .map((ws) => ws?.name || ws?.id)
    .filter(Boolean)
    .join(', ')
  const promptLabel = names
    ? `Move session to workspace (name or id).\nExisting: ${names}`
    : 'Move session to workspace (name or id)'
  const input = window.prompt(promptLabel, currentWs?.name || '')
  if (input === null) return
  const target = input.trim()
  if (!target) return
  const existing = workspaces.find(
    (ws) => ws?.id === target || ws?.name === target
  )
  hub.moveAgentWorkspace(
    sessionId,
    existing?.id || null,
    existing?.name || target
  )
}

function handleDelete(e) {
  const { sessionId } = e.detail
  const store = useWorkspaceStore.getState()
  const session = store.sessionsById[sessionId]
  const name =
    session?.label || session?.display_name || session?.id || 'this agent'
  const inWorktree = session?.in_worktree ?? true

  const modal = document.getElementById('delete-agent-modal')
  if (!modal) return
  const controller = modal.querySelector(
    "[data-controller='delete-agent-modal']"
  )
  if (controller) {
    controller.dataset.agentId = sessionId
    controller.dataset.deleteAgentModalInWorktreeValue = inWorktree
  }
  const nameEl = modal.querySelector('[data-agent-name]')
  if (nameEl) nameEl.textContent = name
  modal.showModal()
}

// Register once at module evaluation — never removed.
document.addEventListener('botster:workspace:rename', handleRename)
document.addEventListener('botster:session:move', handleMove)
document.addEventListener('botster:session:delete', handleDelete)
