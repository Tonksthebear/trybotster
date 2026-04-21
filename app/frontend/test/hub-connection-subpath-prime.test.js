import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest'

// Under test: `computeInitialSurfaceSubpaths` is a private helper in
// hub_connection.js. We test through `channelParams()` — the public shape
// that flows to the subscribe envelope — by instantiating the transport
// with a minimal stub and flipping `window.location.pathname`.
//
// The helper's guarantee: for a URL of the form `/hubs/<hubId>/<segment>[/<rest>]`,
// return `{ [segment]: "/<rest>" }` (or `{ [segment]: "/" }` when no rest).
// Reserved segments (`sessions`, `settings`, `pairing`) must NOT be
// primed — those routes have dedicated React components.

import { HubTransport } from '../lib/connections/hub_connection'

const originalPath = typeof window !== 'undefined' ? window.location.pathname : '/'

function setPath(pathname) {
  // jsdom lets us mutate location.pathname via history.replaceState.
  window.history.replaceState({}, '', pathname)
}

describe('HubTransport.channelParams.surface_subpaths prime (Phase 4b)', () => {
  let transport

  beforeEach(() => {
    // HubTransport is a HubRoute subclass that expects a manager + options
    // bundle. For this unit test we only touch `channelParams()` which
    // reads `this.getHubId()` and `this.browserIdentity`. Use Object.create
    // to sidestep the full constructor's manager wiring.
    transport = Object.create(HubTransport.prototype)
    transport.getHubId = () => 'hub-1'
    transport.browserIdentity = 'browser-ident'
  })

  afterEach(() => {
    setPath(originalPath)
  })

  it('empty prime at the hub root', () => {
    setPath('/hubs/hub-1')
    const params = transport.channelParams()
    expect(params.surface_subpaths).toEqual({})
  })

  it('empty prime at /hubs/hub-1/', () => {
    setPath('/hubs/hub-1/')
    expect(transport.channelParams().surface_subpaths).toEqual({})
  })

  it('surface root primes subpath "/"', () => {
    setPath('/hubs/hub-1/kanban')
    expect(transport.channelParams().surface_subpaths).toEqual({
      kanban: '/',
    })
  })

  it('surface deep link primes the full subpath', () => {
    setPath('/hubs/hub-1/kanban/board/42')
    expect(transport.channelParams().surface_subpaths).toEqual({
      kanban: '/board/42',
    })
  })

  it('reserved first segments (sessions/settings/pairing) do not prime', () => {
    setPath('/hubs/hub-1/sessions/abc')
    expect(transport.channelParams().surface_subpaths).toEqual({})
    setPath('/hubs/hub-1/settings')
    expect(transport.channelParams().surface_subpaths).toEqual({})
    setPath('/hubs/hub-1/pairing')
    expect(transport.channelParams().surface_subpaths).toEqual({})
  })

  it('other hubs URLs do not match', () => {
    setPath('/hubs/hub-2/kanban/board/42')
    expect(transport.channelParams().surface_subpaths).toEqual({})
  })
})
