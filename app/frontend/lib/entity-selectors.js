export function orderedEntities(state) {
  return state.order
    .map((id) => state.byId[id])
    .filter(Boolean)
}

export function entityId(entity, fallback = '') {
  return entity?.id || entity?.target_id || entity?.workspace_id || entity?.worktree_path || fallback
}

export function spawnTargetLabel(target) {
  const branchSuffix = target?.current_branch ? ` (${target.current_branch})` : ''
  return `${target?.name || target?.path || entityId(target, 'target')}${branchSuffix}`
}

export function normalizedWorktree(worktree) {
  const path = worktree?.path || worktree?.worktree_path || ''
  return {
    ...worktree,
    path,
    worktree_path: worktree?.worktree_path || path,
  }
}

export function normalizedWorkspace(workspace) {
  const id = workspace?.id || workspace?.workspace_id
  if (!id) return null
  return {
    ...workspace,
    id,
    workspace_id: workspace?.workspace_id || id,
  }
}

function sessionMatchesFilter(session, filter = {}) {
  if (!session) return false
  const visibility = session.visibility || 'workspace'
  if (filter.visibility && filter.visibility !== 'all' && visibility !== filter.visibility) {
    return false
  }
  if (filter.ownerPlugin && session.owner_plugin !== filter.ownerPlugin) return false
  if (filter.surface && session.surface !== filter.surface) return false
  return true
}

export function isActiveAgentInWorkspace(session, workspaceId, filter = {}) {
  if (!session || session.workspace_id !== workspaceId) return false
  if (session.status === 'closed') return false
  if (!sessionMatchesFilter(session, filter)) return false
  return (session.session_type ?? 'agent') !== 'accessory'
}

export function activeAgentWorkspaces({
  workspaceOrder = [],
  workspacesById = {},
  sessionOrder = [],
  sessionsById = {},
  excludeWorkspaceId = null,
  sessionFilter = {},
} = {}) {
  return workspaceOrder
    .map((id) => normalizedWorkspace(workspacesById[id]))
    .filter((workspace) => {
      if (!workspace || workspace.status === 'closed') return false
      if (excludeWorkspaceId && workspace.id === excludeWorkspaceId) return false
      return sessionOrder.some((sessionId) =>
        isActiveAgentInWorkspace(sessionsById[sessionId], workspace.id, sessionFilter),
      )
    })
}
