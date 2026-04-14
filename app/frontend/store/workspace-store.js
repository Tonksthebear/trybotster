import { create } from 'zustand'

export const useWorkspaceStore = create((set, get) => ({
  // --- State ---
  sessionsById: {},
  sessionOrder: [],
  workspacesById: {},
  workspaceOrder: [],
  ungroupedSessionIds: [],
  selectedSessionId: null,
  collapsedWorkspaceIds: new Set(),
  connected: false,
  surface: 'agent_list',

  // --- Actions ---

  normalize(agents, workspaces) {
    const byId = {}
    const order = []
    for (const agent of agents) {
      byId[agent.id] = agent
      order.push(agent.id)
    }

    const wsById = {}
    const wsOrder = []
    const grouped = new Set()
    for (const ws of workspaces) {
      wsById[ws.id] = ws
      wsOrder.push(ws.id)
      if (Array.isArray(ws.agents)) {
        for (const id of ws.agents) grouped.add(id)
      }
    }

    set({
      sessionsById: byId,
      sessionOrder: order,
      workspacesById: wsById,
      workspaceOrder: wsOrder,
      ungroupedSessionIds: order.filter((id) => !grouped.has(id)),
      connected: true,
    })
  },

  setConnected(value) {
    set({ connected: value })
  },

  setSurface(value) {
    set({ surface: value })
  },

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
}))

// --- Selectors (pure functions, not store methods) ---

export function displayName(session) {
  if (!session) return ''
  const label = session.label?.trim()
  if (label) return label
  return session.display_name || session.id
}

export function subtext(session) {
  if (!session) return ''
  const parts = []
  if (session.target_name) parts.push(session.target_name)
  if (session.branch_name) parts.push(session.branch_name)
  const configName = session.agent_name || session.profile_name
  if (configName) parts.push(configName)
  if (session.session_type === 'accessory' && parts.length === 0) {
    parts.push('accessory')
  }
  return parts.join(' \u00b7 ')
}

export function titleLine(session) {
  if (!session) return ''
  const parts = []
  const title = session.title?.trim()
  const primary = displayName(session)
  if (title && title !== primary) parts.push(title)
  if (session.task) parts.push(session.task)
  return parts.join(' \u00b7 ')
}

export function activityState(session) {
  if (!session) return 'idle'
  if (session.session_type === 'accessory') return 'accessory'
  return session.is_idle !== false ? 'idle' : 'active'
}

export function previewState(session) {
  if (!session) return { canPreview: false }
  const hp = session.hosted_preview
  return {
    canPreview: !!session.port,
    status: hp?.status || 'inactive',
    url: typeof hp?.url === 'string' ? hp.url : null,
    error: hp?.error || null,
    installUrl: typeof hp?.install_url === 'string' ? hp.install_url : null,
  }
}

// --- Composite derivations (read from store) ---

export function selectWorkspaceSessions(state, workspaceId) {
  const ws = state.workspacesById[workspaceId]
  if (!ws || !Array.isArray(ws.agents)) return []
  return ws.agents.map((id) => state.sessionsById[id]).filter(Boolean)
}

export function selectUngroupedSessions(state) {
  return state.ungroupedSessionIds.map((id) => state.sessionsById[id]).filter(Boolean)
}

export function selectWorkspaceGroupProps(state, workspaceId) {
  const ws = state.workspacesById[workspaceId]
  if (!ws) return null
  const sessions = selectWorkspaceSessions(state, workspaceId)
  return {
    id: workspaceId,
    title: ws.name || ws.id,
    count: sessions.length,
    expanded: !state.collapsedWorkspaceIds.has(workspaceId),
    density: state.surface === 'sidebar' ? 'sidebar' : 'panel',
    canRename: true,
    sessions,
  }
}

export function selectSessionRowProps(state, session) {
  if (!session) return null
  const preview = previewState(session)
  return {
    sessionId: session.id,
    sessionUuid: session.session_uuid,
    density: state.surface === 'sidebar' ? 'sidebar' : 'panel',
    primaryName: displayName(session),
    titleLine: titleLine(session),
    subtext: subtext(session),
    selected: state.selectedSessionId === session.id,
    notification: !!session.notification,
    sessionType: session.session_type || 'agent',
    activityState: activityState(session),
    hostedPreview: preview.canPreview ? preview : null,
    previewError: preview.status === 'error' ? preview.error : null,
    actionsMenu: {
      canPreview: preview.canPreview,
      previewStatus: preview.status,
      previewUrl: preview.url,
      canMove: true,
      canDelete: true,
    },
    canMoveWorkspace: true,
    canDelete: true,
    inWorktree: session.in_worktree ?? true,
  }
}

export function selectHostedPreviewIndicatorProps(session) {
  if (!session) return null
  const preview = previewState(session)
  if (!preview.canPreview) return null
  return {
    status: preview.status,
    url: preview.url,
    error: preview.error,
    installUrl: preview.installUrl,
  }
}
