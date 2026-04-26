import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

// These tests exercise the REAL `lib/actions.js` SESSION_SELECT handler
// (unmocked) to prove the idempotency guard prevents double-push when
// `ui_contract/dispatch.ts` already navigated synchronously on
// transport-success.
//
// The rest of the suite mocks `lib/actions.js` via `vi.mock`, which
// replaces the real dispatcher with a spy — that's fine for routing
// assertions but hides the pushState behavior. This file keeps the real
// implementation and mocks only the hub-bridge side-effect that needs a
// live hub session.

vi.mock('../lib/hub-bridge', () => ({
  waitForHub: () => Promise.resolve(null),
}))

import { dispatch, ACTION } from '../lib/actions'

describe('lib/actions SESSION_SELECT navigation', () => {
  let pushSpy
  let popstateSpy

  beforeEach(() => {
    window.history.replaceState({}, '', '/hubs/hub-1')
    pushSpy = vi.spyOn(window.history, 'pushState')
    popstateSpy = vi.fn()
    window.addEventListener('popstate', popstateSpy)
  })

  afterEach(() => {
    pushSpy.mockRestore()
    window.removeEventListener('popstate', popstateSpy)
  })

  it('pushes url when not already on the target path', () => {
    dispatch({
      action: ACTION.SESSION_SELECT,
      payload: {
        hubId: 'hub-1',
        sessionId: 's-1',
        sessionUuid: 'u-1',
        url: '/hubs/hub-1/sessions/u-1',
      },
    })
    expect(pushSpy).toHaveBeenCalledOnce()
    expect(pushSpy).toHaveBeenCalledWith(
      {},
      '',
      '/hubs/hub-1/sessions/u-1',
    )
    expect(popstateSpy).toHaveBeenCalledOnce()
    expect(window.location.pathname).toBe('/hubs/hub-1/sessions/u-1')
  })

  it('is idempotent when already on the target path (transport path pushed first)', () => {
    // Simulate the transport-success path having already pushed the URL.
    window.history.replaceState({}, '', '/hubs/hub-1/sessions/u-1')
    pushSpy.mockClear()
    popstateSpy.mockClear()

    dispatch({
      action: ACTION.SESSION_SELECT,
      payload: {
        hubId: 'hub-1',
        sessionId: 's-1',
        sessionUuid: 'u-1',
        url: '/hubs/hub-1/sessions/u-1',
      },
    })
    expect(pushSpy).not.toHaveBeenCalled()
    expect(popstateSpy).not.toHaveBeenCalled()
  })

  it('does not push when payload has no url', () => {
    dispatch({
      action: ACTION.SESSION_SELECT,
      payload: { hubId: 'hub-1', sessionId: 's-1', sessionUuid: 'u-1' },
    })
    expect(pushSpy).not.toHaveBeenCalled()
    expect(popstateSpy).not.toHaveBeenCalled()
  })
})
