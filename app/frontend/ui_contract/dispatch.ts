import { dispatch as dispatchLocalAction } from '../lib/actions'
import type { ActionDispatch, ActionDispatchSource } from './context'
import type { UiAction } from './types'
import {
  beginUiActionLifecycle,
  failUiActionLifecycle,
} from './action_lifecycle_store'

/**
 * Minimal transport surface we need to send `ui_action` command frames.
 * Anything with an async `sendCommand(type, data)` method (e.g.
 * `HubTransport.sendCommand`) satisfies this. Kept as a structural type so
 * tests can pass a plain mock.
 */
export type UiActionTransport = {
  sendCommand: (type: string, data: Record<string, unknown>) => Promise<boolean>
  on?: (event: string, handler: (message: unknown) => void) => () => void
}

export type CreateTransportDispatchOptions = {
  /** The hub-level transport object (e.g. `HubTransport`). May be null when
   * the hub hasn't connected yet; dispatch short-circuits in that case. */
  transport: UiActionTransport | null | undefined
  /** Hub id, merged into the browser-local payload so local handlers can
   * resolve hub-scoped state (session routes, workspace selection, etc.). */
  hubId: string
  /** Target surface name the broadcast is associated with (e.g.
   * "workspace_surface"). Echoed back on the outbound frame so the hub can
   * route to the right state bundle. */
  targetSurface: string
}

/**
 * Action ids whose semantics are entirely browser-local — they open modals,
 * manipulate local UI state, or trigger browser navigation. Hub has no
 * handler for them (sending them over transport is a no-op). They MUST be
 * dispatched locally via `lib/actions.js` regardless of transport state.
 *
 * If one of these ever needs server-side observation, register a hub-side
 * handler via `action.on(id, ...)` AND remove it from this set so the
 * transport round-trip runs too.
 */
const LOCAL_ONLY_ACTIONS = new Set<string>([
  'botster.workspace.toggle',
  'botster.workspace.rename.request',
  'botster.session.create.request',
  'botster.url.open',
  'botster.session.move.request',
  'botster.session.delete.request',
  // Router-level nav triggered from a Lua-authored tree (e.g. the sidebar's
  // nav entries for plugin-registered surfaces). Hub has no
  // server-side meaning for this action — it's pure browser navigation.
  'botster.nav.open',
  'botster.presentation.set',
  'botster.presentation.clear',
  'botster.presentation.toggle',
])

function dispatchLocal(
  action: UiAction,
  mergedPayload: Record<string, unknown>,
): void {
  dispatchLocalAction({
    action: action.id,
    payload: mergedPayload,
  })
}

/**
 * Browser-local side-effect that must run for `botster.session.select`
 * regardless of whether the hub round-trip succeeds. The hub handles CLI
 * focus but cannot update the browser URL, so the browser owns this route
 * mutation and keeps it idempotent via the `location.pathname` equality
 * check.
 */
function navigateToSessionLocally(
  action: UiAction,
  mergedPayload: Record<string, unknown>,
): void {
  if (action.id !== 'botster.session.select') return
  const hubId = mergedPayload['hubId']
  const sessionUuid = mergedPayload['sessionUuid']
  if (
    typeof hubId !== 'string' ||
    hubId.length === 0 ||
    typeof sessionUuid !== 'string' ||
    sessionUuid.length === 0
  ) {
    return
  }
  if (typeof window === 'undefined' || !window.history?.pushState) return
  const payloadUrl = mergedPayload['url']
  const url = typeof payloadUrl === 'string' && payloadUrl.length > 0
    ? payloadUrl
    : `/hubs/${hubId}/sessions/${sessionUuid}`
  if (window.location.pathname === url) return
  window.history.pushState({}, '', url)
  window.dispatchEvent(new PopStateEvent('popstate'))
}

/**
 * Build an `ActionDispatch` that routes hub-authored actions through the
 * Phase 2b transport. Serialized wire shape:
 *
 *     { type: "ui_action", target_surface, envelope: UiAction }
 */
export function createTransportDispatch(
  opts: CreateTransportDispatchOptions,
): ActionDispatch {
  const { transport, hubId, targetSurface } = opts
  return (action: UiAction, source?: ActionDispatchSource) => {
    if (action.disabled === true) return
    const mergedPayload = {
      hubId,
      targetSurface,
      ...(action.payload ?? {}),
    } as Record<string, unknown>

    // Browser-local actions (modals, collapse toggles, browser nav) must
    // never go over transport — hub has no handler and the click would be
    // silently swallowed. Dispatch directly through browser-local handlers.
    if (LOCAL_ONLY_ACTIONS.has(action.id)) {
      dispatchLocal(action, mergedPayload)
      return
    }

    if (!transport) {
      return
    }

    const envelope: UiAction = action.payload
      ? { id: action.id, payload: action.payload }
      : { id: action.id }

    // Session select needs to push the browser URL locally even when
    // transport succeeds — the hub handles CLI focus but cannot touch
    // the browser router. Run it before the async send so the URL
    // update is synchronous with the click.
    navigateToSessionLocally(action, mergedPayload)

    const actionRequestId = beginUiActionLifecycle({
      actionId: action.id,
      targetSurface,
      sourceKey: source?.uiActionSourceKey ?? action.id,
    })

    void (async () => {
      let sent = false
      try {
        sent =
          (await transport.sendCommand('ui_action', {
            target_surface: targetSurface,
            action_request_id: actionRequestId,
            envelope,
          })) === true
      } catch (err) {
        console.error('[ui_contract] transport command failed', err)
        failUiActionLifecycle(actionRequestId, 'Action could not be sent.')
        return
      }
      if (!sent) {
        failUiActionLifecycle(actionRequestId, 'Action could not be sent.')
      }
    })()
  }
}

/**
 * Dispatcher for contexts that already know the hub id at the call site and
 * want to pass through the raw payload. Useful in tests.
 */
export function createRawDispatch(
  handler: (action: UiAction, source?: ActionDispatchSource) => void,
): ActionDispatch {
  return (action: UiAction, source?: ActionDispatchSource) => {
    if (action.disabled === true) return
    handler(action, source)
  }
}
