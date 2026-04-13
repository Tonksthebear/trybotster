import { useWorkspaceStore } from '../store/workspace-store'
import { getHub } from './hub-bridge'

const ACTION = {
  WORKSPACE_TOGGLE: 'botster.workspace.toggle',
  WORKSPACE_RENAME: 'botster.workspace.rename.request',
  SESSION_SELECT: 'botster.session.select',
  PREVIEW_TOGGLE: 'botster.session.preview.toggle',
  PREVIEW_OPEN: 'botster.session.preview.open',
  SESSION_MOVE: 'botster.session.move.request',
  SESSION_DELETE: 'botster.session.delete.request',
}

// Validate that a URL uses a safe protocol (http/https only)
function safeUrl(url) {
  if (!url || typeof url !== 'string') return null
  try {
    const parsed = new URL(url, window.location.origin)
    return parsed.protocol === 'http:' || parsed.protocol === 'https:'
      ? parsed.href
      : null
  } catch {
    return null
  }
}

function dispatch(actionBinding) {
  const { action, payload = {} } = actionBinding
  const handler = handlers[action]
  if (!handler) {
    console.warn(`[actions] unknown action: ${action}`, payload)
    return
  }
  handler(payload)
}

const handlers = {
  [ACTION.WORKSPACE_TOGGLE](payload) {
    useWorkspaceStore.getState().toggleWorkspaceCollapsed(payload.workspaceId)
  },

  [ACTION.WORKSPACE_RENAME](payload) {
    document.dispatchEvent(
      new CustomEvent('botster:workspace:rename', {
        detail: { workspaceId: payload.workspaceId, title: payload.title },
      })
    )
  },

  [ACTION.SESSION_SELECT](payload) {
    useWorkspaceStore.getState().setSelectedSessionId(payload.sessionId)
    // Tell the hub (focuses the session in CLI)
    const hub = getHub(payload.hubId)
    if (hub && payload.sessionId) {
      hub.selectAgent(payload.sessionId)
    }
    // Navigate via Turbo if we have a session URL
    if (payload.url) {
      window.Turbo?.visit(payload.url)
    }
  },

  [ACTION.PREVIEW_TOGGLE](payload) {
    const hub = getHub(payload.hubId)
    if (!hub) return
    hub.toggleHostedPreview(payload.sessionUuid)
  },

  [ACTION.PREVIEW_OPEN](payload) {
    const url = safeUrl(payload.url)
    if (url) {
      window.open(url, '_blank', 'noopener')
    }
  },

  [ACTION.SESSION_MOVE](payload) {
    document.dispatchEvent(
      new CustomEvent('botster:session:move', {
        detail: {
          sessionId: payload.sessionId,
          sessionUuid: payload.sessionUuid,
        },
      })
    )
  },

  [ACTION.SESSION_DELETE](payload) {
    document.dispatchEvent(
      new CustomEvent('botster:session:delete', {
        detail: {
          sessionId: payload.sessionId,
          sessionUuid: payload.sessionUuid,
        },
      })
    )
  },
}

export { ACTION, safeUrl, dispatch }
