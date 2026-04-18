import { dispatch as dispatchLegacy } from '../lib/actions'
import type { ActionDispatch } from './context'
import type { UiActionV1 } from './types'

/**
 * Bridge from the ui_contract `UiActionV1` envelope to the existing
 * `lib/actions.js` dispatcher. The two envelopes differ in one key:
 *
 * - `UiActionV1` spec: `{ id, payload?, disabled? }`
 * - Legacy binding:    `{ action: id, payload }`
 *
 * Action ids remain semantic (e.g. `botster.session.select`), so the legacy
 * handler map in `lib/actions.js` continues to own the hub-transport mapping.
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

/**
 * Dispatcher for contexts that already know the hub id at the call site and
 * want to pass through the raw payload. Useful in tests.
 */
export function createRawDispatch(
  handler: (action: UiActionV1) => void,
): ActionDispatch {
  return (action: UiActionV1) => {
    if (action.disabled === true) return
    handler(action)
  }
}
