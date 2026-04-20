import { dispatch as dispatchLegacy } from '../lib/actions'
import type { ActionDispatch, ActionDispatchSource } from './context'
import type { UiActionV1 } from './types'

/**
 * Minimal transport surface we need to send `ui_action_v1` frames. Anything
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
  /** Hub id, merged into the local/fallback payload so legacy handlers can
   * resolve hub-scoped state (session routes, workspace selection, etc.). */
  hubId: string
  /** Target surface name the broadcast is associated with (e.g.
   * "workspace_surface"). Echoed back on the outbound frame so the hub can
   * route to the right state bundle. */
  targetSurface: string
  /** Optional override called on transport-send failure. Defaults to routing
   * to the legacy dispatcher for well-known action ids. Override only in
   * tests or if the caller owns a different fallback path. */
  fallback?: (action: UiActionV1, mergedPayload: Record<string, unknown>) => void
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
  // Phase 4a: router-level nav triggered from a Lua-authored tree (e.g.
  // the sidebar's nav entries for plugin-registered surfaces). Hub has no
  // server-side meaning for this action — it's pure browser navigation.
  'botster.nav.open',
])

/**
 * Action ids that retain a defensive local fallback via `lib/actions.js` when
 * the encrypted transport is unavailable (no subscription, hub not connected,
 * etc.). These actions are hub-authoritative — transport is the primary path
 * and local dispatch is only a fallback while the hub is unreachable.
 *
 * Excludes non-idempotent actions (notably `botster.session.preview.toggle`)
 * where running legacy after a failed transport attempt could diverge from
 * hub state once reconnected. For those we simply drop the click — the user
 * can retry once the connection recovers.
 */
const LEGACY_FALLBACK_ACTIONS = new Set<string>([
  'botster.session.select',
])

/**
 * Merge additional fields into the fallback payload that legacy handlers
 * need to behave correctly when the hub round-trip is skipped. Phase 1
 * `SessionRow.jsx` always passed `url: /hubs/{hubId}/sessions/{sessionUuid}`
 * alongside the select envelope so the browser could `history.pushState`
 * after `event.preventDefault()` — Phase 2a's Lua-authored tree only emits
 * `{ sessionId, sessionUuid }` in the action payload, so we synthesize the
 * URL here to preserve disconnected-state route navigation.
 */
function enrichFallbackPayload(
  action: UiActionV1,
  mergedPayload: Record<string, unknown>,
): Record<string, unknown> {
  if (action.id === 'botster.session.select') {
    const hubId = mergedPayload['hubId']
    const sessionUuid = mergedPayload['sessionUuid']
    if (
      typeof hubId === 'string' &&
      hubId.length > 0 &&
      typeof sessionUuid === 'string' &&
      sessionUuid.length > 0 &&
      mergedPayload['url'] === undefined
    ) {
      return { ...mergedPayload, url: `/hubs/${hubId}/sessions/${sessionUuid}` }
    }
  }
  return mergedPayload
}

function defaultFallback(
  action: UiActionV1,
  mergedPayload: Record<string, unknown>,
): void {
  if (!LEGACY_FALLBACK_ACTIONS.has(action.id)) return
  dispatchLegacy({
    action: action.id,
    payload: enrichFallbackPayload(action, mergedPayload),
  })
}

function dispatchLocal(
  action: UiActionV1,
  mergedPayload: Record<string, unknown>,
): void {
  dispatchLegacy({
    action: action.id,
    payload: enrichFallbackPayload(action, mergedPayload),
  })
}

/**
 * Browser-local side-effect that must run for `botster.session.select`
 * regardless of whether the hub round-trip succeeds. Phase 1's
 * `SessionRow.jsx` always called `lib/actions.js` which pushed the session
 * URL into `window.history`; `hub-bridge.js` listens for `popstate` to
 * derive `selectedSessionId`. On the transport-success path the hub
 * handles CLI focus but cannot update the browser URL, so we mirror the
 * Phase 1 navigation side-effect here. Fallback paths still go through
 * the legacy handler (which already pushes the url via
 * `enrichFallbackPayload`), so this function is a no-op for them —
 * kept idempotent via the `location.pathname` equality check.
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
 * Build an `ActionDispatch` that routes through the Phase 2b transport as the
 * default path. Serialized wire shape (confirmed with Phase 2b):
 *
 *     { type: "ui_action_v1", target_surface, envelope: UiActionV1 }
 *
 * When transport send returns falsy (no subscription, send failed), the
 * dispatcher falls back to the legacy `lib/actions.js` handler for
 * well-known action ids — a defensive bridge that keeps the UI responsive
 * during the Phase 2b wire-up window. Once hub handlers are confirmed the
 * fallback can be removed.
 */
export function createTransportDispatch(
  opts: CreateTransportDispatchOptions,
): ActionDispatch {
  const { transport, hubId, targetSurface } = opts
  const fallback = opts.fallback ?? defaultFallback
  return (action: UiActionV1, _source?: ActionDispatchSource) => {
    if (action.disabled === true) return
    const mergedPayload = {
      hubId,
      ...(action.payload ?? {}),
    } as Record<string, unknown>

    // Browser-local actions (modals, collapse toggles, browser nav) must
    // never go over transport — hub has no handler and the click would be
    // silently swallowed. Dispatch directly through the legacy handlers.
    if (LOCAL_ONLY_ACTIONS.has(action.id)) {
      dispatchLocal(action, mergedPayload)
      return
    }

    if (!transport) {
      fallback(action, mergedPayload)
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
          (await transport.send('ui_action_v1', {
            target_surface: targetSurface,
            envelope,
          })) === true
      } catch (err) {
        console.error('[ui_contract] transport send failed', err)
      }
      if (!sent) fallback(action, mergedPayload)
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
