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
  /** Hub id, merged into the legacy-fallback payload for parity with
   * pre-Phase-2c `createHubDispatch` callers. */
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
 * Action ids that retain a defensive local fallback via `lib/actions.js` when
 * the encrypted transport is unavailable (no subscription, hub not connected,
 * etc.).
 *
 * Excludes non-idempotent actions (notably `botster.session.preview.toggle`)
 * where running legacy after a failed transport attempt could diverge from
 * hub state once reconnected. For those we simply drop the click — the user
 * can retry once the connection recovers.
 */
const LEGACY_FALLBACK_ACTIONS = new Set<string>([
  'botster.workspace.toggle',
  'botster.workspace.rename.request',
  'botster.session.select',
  'botster.session.preview.open',
  'botster.session.move.request',
  'botster.session.delete.request',
  'botster.session.create.request',
])

function defaultFallback(
  action: UiActionV1,
  mergedPayload: Record<string, unknown>,
): void {
  if (!LEGACY_FALLBACK_ACTIONS.has(action.id)) return
  dispatchLegacy({ action: action.id, payload: mergedPayload })
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

    if (!transport) {
      fallback(action, mergedPayload)
      return
    }

    const envelope: UiActionV1 = action.payload
      ? { id: action.id, payload: action.payload }
      : { id: action.id }

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

/**
 * @deprecated Legacy Phase-1 bridge. Prefer `createTransportDispatch`. Kept
 * while Phase 2b wires in the hub-side `ui_action_v1` handler so any lingering
 * callers continue to function. Remove once Phase 2b + 2c both land.
 *
 * Forwards `UiActionV1` directly to the legacy `lib/actions.js` dispatcher
 * with `hubId` merged into the payload.
 */
export function createHubDispatch(hubId: string): ActionDispatch {
  return (action: UiActionV1) => {
    if (action.disabled === true) return
    const mergedPayload = { hubId, ...(action.payload ?? {}) }
    dispatchLegacy({
      action: action.id,
      payload: mergedPayload,
    })
  }
}
