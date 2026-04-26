import { dispatch as dispatchLocalAction } from '../lib/actions'
import type { ActionDispatch, ActionDispatchSource } from './context'
import type { UiActionV1 } from './types'

/**
 * Minimal transport surface we need to send `ui_action` frames. Anything
 * with an async `send(type, data)` method (e.g. `HubTransport.send`) satisfies
 * this. Kept as a structural type so tests can pass a plain mock.
 */
export type UiActionTransport = {
  send: (type: string, data: Record<string, unknown>) => Promise<boolean>
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
  'botster.session.preview.open',
  'botster.session.move.request',
  'botster.session.delete.request',
  // Router-level nav triggered from a Lua-authored tree (e.g. the sidebar's
  // nav entries for plugin-registered surfaces). Hub has no
  // server-side meaning for this action — it's pure browser navigation.
  'botster.nav.open',
])

function dispatchLocal(
  action: UiActionV1,
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
  action: UiActionV1,
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
  const url = `/hubs/${hubId}/sessions/${sessionUuid}`
  if (window.location.pathname === url) return
  window.history.pushState({}, '', url)
  window.dispatchEvent(new PopStateEvent('popstate'))
}

/**
 * Build an `ActionDispatch` that routes hub-authored actions through the
 * Phase 2b transport. Serialized wire shape:
 *
 *     { type: "ui_action", target_surface, envelope: UiActionV1 }
 */
export function createTransportDispatch(
  opts: CreateTransportDispatchOptions,
): ActionDispatch {
  const { transport, hubId, targetSurface } = opts
  return (action: UiActionV1, _source?: ActionDispatchSource) => {
    if (action.disabled === true) return
    const mergedPayload = {
      hubId,
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

    const envelope: UiActionV1 = action.payload
      ? { id: action.id, payload: action.payload }
      : { id: action.id }

    // Session select needs to push the browser URL locally even when
    // transport succeeds — the hub handles CLI focus but cannot touch
    // the browser router. Run it before the async send so the URL
    // update is synchronous with the click.
    navigateToSessionLocally(action, mergedPayload)

    void (async () => {
      let sent = false
      try {
        sent =
          (await transport.send('ui_action', {
            target_surface: targetSurface,
            envelope,
          })) === true
      } catch (err) {
        console.error('[ui_contract] transport send failed', err)
      }
      if (!sent) return
    })()
  }
}

/**
 * Dispatcher for contexts that already know the hub id at the call site and
 * want to pass through the raw payload. Useful in tests.
 */
export function createRawDispatch(
  handler: (action: UiActionV1, source?: ActionDispatchSource) => void,
): ActionDispatch {
  return (action: UiActionV1, source?: ActionDispatchSource) => {
    if (action.disabled === true) return
    handler(action, source)
  }
}
