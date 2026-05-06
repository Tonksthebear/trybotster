import { describe, it, expect } from 'vitest'

import { HubTransport } from '../lib/connections/hub_connection'

describe('HubTransport.channelParams', () => {
  it('identifies the hub/browser without requesting initial data', () => {
    const transport = Object.create(HubTransport.prototype)
    transport.getHubId = () => 'hub-1'
    transport.browserIdentity = 'browser-ident'

    expect(transport.channelParams()).toEqual({
      hub_id: 'hub-1',
      browser_identity: 'browser-ident',
    })
  })
})
