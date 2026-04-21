import { useWorkspaceStore } from '../store/workspace-store'
import { useDialogStore } from '../store/dialog-store'
import { getHub } from './hub-bridge'

const ACTION = {
  WORKSPACE_TOGGLE: 'botster.workspace.toggle',
  WORKSPACE_RENAME: 'botster.workspace.rename.request',
  SESSION_SELECT: 'botster.session.select',
  SESSION_CREATE: 'botster.session.create.request',
  PREVIEW_TOGGLE: 'botster.session.preview.toggle',
  PREVIEW_OPEN: 'botster.session.preview.open',
  SESSION_MOVE: 'botster.session.move.request',
  SESSION_DELETE: 'botster.session.delete.request',
  // Phase 4a: router-level navigation fired from Lua-authored trees (e.g.
  // sidebar nav entries for plugin-registered surfaces). Payload expected:
  // { path, hubId? }. When `hubId` is present the path is prefixed with
  // `/hubs/<id>`; callers that emit pre-qualified absolute paths can omit.
  NAV_OPEN: 'botster.nav.open',
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

  [ACTION.SESSION_CREATE]() {
    useDialogStore.getState().openNewSession()
  },

  [ACTION.SESSION_SELECT](payload) {
    useWorkspaceStore.getState().setSelectedSessionId(payload.sessionId)
    // Tell the hub (focuses the session in CLI)
    const hub = getHub(payload.hubId)
    if (hub && payload.sessionId) {
      hub.selectAgent(payload.sessionId)
    }
    // Navigate via React Router (pushState). Idempotent — skip when
    // already on the target path. The transport-success path in
    // `ui_contract/dispatch.ts` also pushes synchronously, so on a
    // transport-failure fallback this handler would otherwise double-push.
    if (payload.url && window.location.pathname !== payload.url) {
      window.history.pushState({}, '', payload.url)
      window.dispatchEvent(new PopStateEvent('popstate'))
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

  [ACTION.NAV_OPEN](payload) {
    // Phase 4a: router-level nav. Accept either a pre-qualified absolute
    // path (`/hubs/:id/plugins/hello`) or a hub-relative one (`/plugins/hello`)
    // alongside the hubId enriched by createTransportDispatch. Relative
    // paths are the common case — the sidebar Lua emits the surface's own
    // `path` field directly, which is hub-relative.
    let target = typeof payload.path === 'string' ? payload.path : null
    if (!target || typeof window === 'undefined' || !window.history?.pushState) {
      return
    }
    if (!target.startsWith('/hubs/') && typeof payload.hubId === 'string' && payload.hubId.length > 0) {
      const trimmed = target.startsWith('/') ? target : '/' + target
      target = `/hubs/${payload.hubId}${trimmed === '/' ? '' : trimmed}`
    }
    if (window.location.pathname === target) return
    window.history.pushState({}, '', target)
    window.dispatchEvent(new PopStateEvent('popstate'))
  },
}

export { ACTION, safeUrl, dispatch }
