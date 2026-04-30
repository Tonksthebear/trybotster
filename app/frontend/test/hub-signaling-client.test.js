import { beforeEach, describe, expect, it, vi } from 'vitest'

const actionCable = vi.hoisted(() => ({
  createConsumer: vi.fn(),
  createSubscription: vi.fn(),
}))

vi.mock('@rails/actioncable', () => ({
  createConsumer: (...args) => actionCable.createConsumer(...args),
}))

function fakeConsumer() {
  return {
    connection: {
      webSocket: null,
      isOpen: vi.fn(() => false),
      isActive: vi.fn(() => false),
      open: vi.fn(),
      close: vi.fn(),
      installEventHandlers: vi.fn(),
    },
    subscriptions: {
      create: (...args) => actionCable.createSubscription(...args),
    },
  }
}

async function buildClient() {
  const { HubSignalingClient } = await import('../lib/transport/hub_signaling_client')
  const notify = vi.fn()
  return { client: new HubSignalingClient({ notify }), notify }
}

describe('HubSignalingClient', () => {
  beforeEach(() => {
    vi.resetModules()
    vi.clearAllMocks()
    actionCable.createConsumer.mockReturnValue(fakeConsumer())
    actionCable.createSubscription.mockReset()
  })

  it('unsubscribes an existing same-hub subscription before replacing it', async () => {
    const firstSubscription = { unsubscribe: vi.fn() }
    const secondSubscription = { unsubscribe: vi.fn() }
    actionCable.createSubscription
      .mockReturnValueOnce(firstSubscription)
      .mockReturnValueOnce(secondSubscription)

    const { client } = await buildClient()

    await client.connect('hub-1', 'browser-1', {})
    await client.connect('hub-1', 'browser-1', {})

    expect(firstSubscription.unsubscribe).toHaveBeenCalledTimes(1)
    expect(secondSubscription.unsubscribe).not.toHaveBeenCalled()
    expect(client.getSubscription('hub-1')).toBe(secondSubscription)
  })

  it('only delivers callbacks from the latest same-hub subscription', async () => {
    const firstSubscription = { unsubscribe: vi.fn() }
    const secondSubscription = { unsubscribe: vi.fn() }
    const callbacks = []
    actionCable.createSubscription.mockImplementation((_identifier, subscriptionCallbacks) => {
      callbacks.push(subscriptionCallbacks)
      return callbacks.length === 1 ? firstSubscription : secondSubscription
    })

    const { client, notify } = await buildClient()
    const firstHandlers = { onMessage: vi.fn(), onState: vi.fn() }
    const secondHandlers = { onMessage: vi.fn(), onState: vi.fn() }

    await client.connect('hub-1', 'browser-1', firstHandlers)
    await client.connect('hub-1', 'browser-1', secondHandlers)

    callbacks[0].connected()
    callbacks[0].received({ type: 'old-message' })
    callbacks[0].disconnected()

    expect(firstHandlers.onState).not.toHaveBeenCalled()
    expect(firstHandlers.onMessage).not.toHaveBeenCalled()
    expect(notify).not.toHaveBeenCalled()

    callbacks[1].connected()
    callbacks[1].received({ type: 'new-message' })
    callbacks[1].disconnected()

    expect(secondHandlers.onState).toHaveBeenNthCalledWith(1, 'connected')
    expect(secondHandlers.onMessage).toHaveBeenCalledWith({ type: 'new-message' })
    expect(secondHandlers.onState).toHaveBeenNthCalledWith(2, 'disconnected')
    expect(notify).toHaveBeenNthCalledWith(1, 'signaling:state', {
      hubId: 'hub-1',
      state: 'connected',
    })
    expect(notify).toHaveBeenNthCalledWith(2, 'signaling:state', {
      hubId: 'hub-1',
      state: 'disconnected',
    })
  })

  it('disconnects only the latest same-hub subscription after replacement', async () => {
    const firstSubscription = { unsubscribe: vi.fn() }
    const secondSubscription = { unsubscribe: vi.fn() }
    actionCable.createSubscription
      .mockReturnValueOnce(firstSubscription)
      .mockReturnValueOnce(secondSubscription)

    const { client } = await buildClient()

    await client.connect('hub-1', 'browser-1', {})
    await client.connect('hub-1', 'browser-1', {})
    client.disconnect('hub-1')
    client.disconnect('hub-1')

    expect(firstSubscription.unsubscribe).toHaveBeenCalledTimes(1)
    expect(secondSubscription.unsubscribe).toHaveBeenCalledTimes(1)
    expect(client.getSubscription('hub-1')).toBe(null)
  })
})
