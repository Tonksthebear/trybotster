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
